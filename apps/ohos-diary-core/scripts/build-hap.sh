#!/usr/bin/env bash
# 构建 ArkTS 参考工程的 HAP（可签名）。纯命令行：不需要 DevEco GUI。
#
# 步骤与坑都来自探针 probes/ohos-core-smoke/scripts/build-hap.sh（见
# docs/architecture/m6-鸿蒙可行性探针.md §24）：
#   1. scripts/build-native.sh 编出 libdiary_capi.so + libdiary_napi.so + libc++_shared.so；
#   2. `ohpm install --all`（离线、几十毫秒）把本地 `file:` 依赖
#      `libdiary_napi.so → src/main/cpp/types/libdiary_napi` 做成 oh_modules 软链——
#      少了这步，ArkTS 的 `import diaryNapi from 'libdiary_napi.so'` 会退化成 any，
#      编译报 arkts-no-any-unknown；
#   3. hvigorw assembleHap；ProcessLibs 把 entry/libs/arm64-v8a/*.so 收进 HAP。
#
# 用法：
#   scripts/build-hap.sh                              # 只构建（不签名）
#   OHOS_SIGN_STORE_FILE=... scripts/build-hap.sh      # 构建 + 签名
#
# 环境变量：
#   SKIP_NATIVE=1   跳过原生产物重建（复用 entry/libs/arm64-v8a/ 里已有的 .so）
#   SKIP_OHPM=1     跳过 ohpm install
#   WITH_SEMANTIC=1 构建**含语义**的 HAP：额外要求 libonnxruntime-diary-1.29.1.so
#                   （先跑 stage-ort-1291.sh），包内两份 ORT 共存
#   SKIP_ASR_LIBS=1 构建**不含 ASR** 的基线/冒烟包：sherpa/两份 ORT 都临时移出，
#                   构建后恢复；这种 HAP 不能转写。不能与 WITH_SEMANTIC=1 同用
#   BUILD_MODE      debug（默认）/ release
#   OHOS_BUNDLE_NAME / OHOS_VERSION_CODE / OHOS_VERSION_NAME   临时覆盖（构建后还原）
#
# 构建矩阵（两份文档必须与这里说同一句话）：
#   default（不带旗标）  = 只带 sherpa 的 ORT 1.16.3；关键词检索可用，语义检索不可用
#   WITH_SEMANTIC=1      = 再带自编 ORT 1.29.1；语义检索可用（已真机验通）
#   SKIP_ASR_LIBS=1      = 两份 ORT 与 sherpa 都不带；不能转写、不能语义
set -euo pipefail

HERE="$(cd "$(dirname "$0")/.." && pwd)"
HVIGORW="${HVIGORW:-$HOME/command-line-tools/bin/hvigorw}"
OHPM="${OHPM:-$HOME/command-line-tools/bin/ohpm}"
BUILD_MODE="${BUILD_MODE:-debug}"
PRODUCT="${PRODUCT:-default}"
MODULE="${MODULE:-entry}"

PROFILE="$HERE/build-profile.json5"
APPJSON="$HERE/AppScope/app.json5"
MODULEJSON="$HERE/entry/src/main/module.json5"
BP_BACKUP="$(mktemp)"; APP_BACKUP="$(mktemp)"; MOD_BACKUP="$(mktemp)"
cp "$PROFILE" "$BP_BACKUP"; cp "$APPJSON" "$APP_BACKUP"; cp "$MODULEJSON" "$MOD_BACKUP"

# 构建矩阵用到的库可能已在 entry/libs 里（例如上一轮 WITH_SEMANTIC 构建留下的
# 1.29.1）。hvigor 会把 arm64-v8a/*.so 全打进包，所以「默认不含 1.29.1」不能只靠
# 不检查——要在构建期间把它临时移出、构建后原样移回。
PARK_DIR="$HERE/entry/.build-park-$$"
PARKED=()
park_lib() {
  local file="$1" name
  [[ -f "$file" ]] || return 0
  mkdir -p "$PARK_DIR"
  name="$(basename "$file")"
  mv -f "$file" "$PARK_DIR/$name"
  PARKED+=("$file")
  echo "[build-hap] 临时移出 $name（$MATRIX 构建不打包它）"
}
restore_parked() {
  local file
  for file in ${PARKED[@]+"${PARKED[@]}"}; do
    mv -f "$PARK_DIR/$(basename "$file")" "$file"
  done
  PARKED=()
  rmdir "$PARK_DIR" 2>/dev/null || true
}
restore() {
  restore_parked
  cp "$BP_BACKUP" "$PROFILE"
  cp "$APP_BACKUP" "$APPJSON"
  cp "$MOD_BACKUP" "$MODULEJSON"
  rm -f "$BP_BACKUP" "$APP_BACKUP" "$MOD_BACKUP"
}
trap restore EXIT

# 产品的远端模型服务在 debug/release 都需要联网权限。
# 探针的执行由 EntryAbility 的 BuildProfile.DEBUG 门禁控制，不通过删权限禁用产品能力。
check_internet_permission() {
  python3 - "$MODULEJSON" <<'PY'
import json, sys
manifest = json.load(open(sys.argv[1], encoding="utf-8"))
permissions = {p["name"] for p in manifest["module"]["requestPermissions"]}
if "ohos.permission.INTERNET" not in permissions:
    sys.exit("[build-hap] 产品清单缺少 INTERNET，远端模型不可用；拒绝构建")
print("[build-hap] INTERNET 已在产品清单中（debug/release 共用）")
PY
}

# ── 0. 原生产物
if [[ -z "${SKIP_NATIVE:-}" ]]; then
  "$HERE/scripts/build-native.sh"
fi
for so in libdiary_capi.so libdiary_napi.so libc++_shared.so; do
  [[ -f "$HERE/entry/libs/arm64-v8a/$so" ]] \
    || { echo "缺少 entry/libs/arm64-v8a/$so（先跑 scripts/build-native.sh）" >&2; exit 1; }
done

# ── 0a. 构建矩阵：ASR 引擎库与语义 ORT
#
# 为什么不再是「有就带上、没有也照建」：缺引擎的 HAP 曾经能通过 readiness
# （OHOS 的「按库名加载」被当成正常），转写任务跑起来才在 load_library() 失败，
# 而且按时间语义是**终态** failed。核心侧的 P0 修复（readiness 真的探测一次 +
# load_library 错误码归位）已在本 PR（commit 378316c）；构建这边要把矩阵做成
# **构建期可判定**的，而不是装到设备上才发现。
#
# `stage-sherpa.sh --check` 断言：两个文件在、aarch64、sherpa 的 NEEDED 有
# libonnxruntime.so、符号版本是 OrtGetApiBase@VERS_1.16.3，且 sha256 与钉死值一致。
# `stage-ort-1291.sh --check` 断言：1.29.1 的包内 sha256、SONAME、符号版本。
if [[ -n "${SKIP_ASR_LIBS:-}" && -n "${WITH_SEMANTIC:-}" ]]; then
  echo "[build-hap] SKIP_ASR_LIBS=1 与 WITH_SEMANTIC=1 不能同时用（无 ASR 基线 vs 双 ORT 语义包）" >&2
  exit 2
fi
if [[ -n "${SKIP_ASR_LIBS:-}" ]]; then
  MATRIX="no-asr"
  for so in libsherpa-onnx-c-api.so libonnxruntime.so libonnxruntime-diary-1.29.1.so; do
    park_lib "$HERE/entry/libs/arm64-v8a/$so"
  done
  echo "[build-hap] 矩阵 no-asr：不带转写引擎/两份 ORT（这个 HAP 不能转写、不能语义）"
elif [[ -n "${WITH_SEMANTIC:-}" ]]; then
  MATRIX="asr+semantic"
  bash "$HERE/scripts/stage-ort-1291.sh" --check
  echo "[build-hap] 矩阵 asr+semantic：sherpa ORT 1.16.3 + 自编 ORT 1.29.1（语义检索可用）"
else
  MATRIX="asr-only"
  bash "$HERE/scripts/stage-sherpa.sh" --check
  park_lib "$HERE/entry/libs/arm64-v8a/libonnxruntime-diary-1.29.1.so"
  echo "[build-hap] 矩阵 asr-only：只带 sherpa ORT 1.16.3（关键词检索可用；语义检索不可用；WITH_SEMANTIC=1 才含 1.29.1）"
fi

# ── 0a2. 两种构建都必须保留产品联网权限
check_internet_permission

# ── 0b. ohpm install（依赖是本地 file: 目录，不需要联网）
if [[ -z "${SKIP_OHPM:-}" ]]; then
  echo "[build-hap] ohpm install --all"
  ( cd "$HERE" && "$OHPM" install --all )
fi

# ── bundleName / API 版本可覆盖（构建后由 trap 还原）
if [[ -n "${OHOS_BUNDLE_NAME:-}" ]]; then
  echo "[build-hap] bundleName override → $OHOS_BUNDLE_NAME"
  python3 - "$APPJSON" "$OHOS_BUNDLE_NAME" <<'PY'
import re, sys
p, name = sys.argv[1], sys.argv[2]
s = open(p, encoding="utf-8").read()
s2, n = re.subn(r'"bundleName"\s*:\s*"[^"]*"', '"bundleName": "%s"' % name, s, count=1)
assert n == 1, "bundleName not found in app.json5"
open(p, "w", encoding="utf-8").write(s2)
PY
fi
if [[ -n "${OHOS_VERSION_CODE:-}" || -n "${OHOS_VERSION_NAME:-}" ]]; then
  echo "[build-hap] versionCode/versionName override → ${OHOS_VERSION_CODE:-<keep>}/${OHOS_VERSION_NAME:-<keep>}"
  python3 - "$APPJSON" "${OHOS_VERSION_CODE:-}" "${OHOS_VERSION_NAME:-}" <<'PYAPP'
import re, sys
p, code, name = sys.argv[1], sys.argv[2], sys.argv[3]
s = open(p, encoding="utf-8").read()
if code:
    s, n = re.subn(r'"versionCode"\s*:\s*\d+', '"versionCode": %s' % code, s, count=1)
    assert n == 1, "versionCode not found in app.json5"
if name:
    s, n = re.subn(r'"versionName"\s*:\s*"[^"]*"', '"versionName": "%s"' % name, s, count=1)
    assert n == 1, "versionName not found in app.json5"
open(p, "w", encoding="utf-8").write(s)
PYAPP
fi
if [[ -n "${OHOS_COMPAT_SDK_VERSION:-}" || -n "${OHOS_TARGET_SDK_VERSION:-}" ]]; then
  compat="${OHOS_COMPAT_SDK_VERSION:-}"; target="${OHOS_TARGET_SDK_VERSION:-$compat}"
  echo "[build-hap] compatible=$compat target=$target"
  python3 - "$PROFILE" "$compat" "$target" <<'PY'
import re, sys
p, compat, target = sys.argv[1], sys.argv[2], sys.argv[3]
s = open(p, encoding="utf-8").read()
if compat:
    s, n = re.subn(r'"compatibleSdkVersion"\s*:\s*"[^"]*"', '"compatibleSdkVersion": "%s"' % compat, s, count=1)
    assert n == 1, "compatibleSdkVersion not found"
if target:
    s, n = re.subn(r'"targetSdkVersion"\s*:\s*"[^"]*"', '"targetSdkVersion": "%s"' % target, s, count=1)
    assert n == 1, "targetSdkVersion not found"
open(p, "w", encoding="utf-8").write(s)
PY
fi

# ── 签名配置注入（密码必须是 DevEco 加密串，同探针坑 2）
if [[ -n "${OHOS_SIGN_STORE_FILE:-}" ]]; then
  : "${OHOS_SIGN_CERT_FILE:?set OHOS_SIGN_CERT_FILE}"
  : "${OHOS_SIGN_PROFILE_FILE:?set OHOS_SIGN_PROFILE_FILE}"
  : "${OHOS_SIGN_STORE_PASSWORD_ENC:?set OHOS_SIGN_STORE_PASSWORD_ENC (DevEco-encrypted hex)}"
  : "${OHOS_SIGN_KEY_PASSWORD_ENC:?set OHOS_SIGN_KEY_PASSWORD_ENC (DevEco-encrypted hex)}"
  export KEY_ALIAS="${OHOS_SIGN_KEY_ALIAS:-debugKey}"
  echo "[build-hap] injecting signingConfigs (alias=$KEY_ALIAS)"
  python3 - "$PROFILE" <<'PY'
import json, os, sys
path = sys.argv[1]
cfg = {
    "name": "default",
    "type": "HarmonyOS",
    "material": {
        "certpath": os.environ["OHOS_SIGN_CERT_FILE"],
        "storePassword": os.environ["OHOS_SIGN_STORE_PASSWORD_ENC"],
        "keyAlias": os.environ["KEY_ALIAS"],
        "keyPassword": os.environ["OHOS_SIGN_KEY_PASSWORD_ENC"],
        "profile": os.environ["OHOS_SIGN_PROFILE_FILE"],
        "signAlg": "SHA256withECDSA",
        "storeFile": os.environ["OHOS_SIGN_STORE_FILE"],
    },
}
src = open(path, encoding="utf-8").read()
old = '"signingConfigs": []'
assert old in src, "signingConfigs placeholder not found in build-profile.json5"
src = src.replace(old, '"signingConfigs": ' + json.dumps([cfg], ensure_ascii=False, indent=6), 1)
open(path, "w", encoding="utf-8").write(src)
PY
fi

cd "$HERE"
"$HVIGORW" --mode module \
  -p product="$PRODUCT" -p module="$MODULE@$PRODUCT" -p buildMode="$BUILD_MODE" \
  assembleHap --no-daemon

OUT="$HERE/$MODULE/build/$PRODUCT/outputs/$PRODUCT"
echo "[build-hap] outputs in $OUT"
for f in "$OUT"/*.hap; do
  [[ -e "$f" ]] || continue
  printf '[build-hap] %s  bytes=%s  sha256=%s\n' "$(basename "$f")" "$(stat -c%s "$f")" \
    "$(sha256sum "$f" | cut -d' ' -f1)"
  echo "[build-hap] 包内原生库："
  unzip -l "$f" | grep -E "libs/" | sed 's/^/    /' || true
done

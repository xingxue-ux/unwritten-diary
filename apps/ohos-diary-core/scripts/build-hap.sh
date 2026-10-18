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
#   BUILD_MODE      debug（默认）/ release
#   OHOS_BUNDLE_NAME / OHOS_VERSION_CODE / OHOS_VERSION_NAME   临时覆盖（构建后还原）
set -euo pipefail

HERE="$(cd "$(dirname "$0")/.." && pwd)"
HVIGORW="${HVIGORW:-$HOME/command-line-tools/bin/hvigorw}"
OHPM="${OHPM:-$HOME/command-line-tools/bin/ohpm}"
BUILD_MODE="${BUILD_MODE:-debug}"
PRODUCT="${PRODUCT:-default}"
MODULE="${MODULE:-entry}"

PROFILE="$HERE/build-profile.json5"
APPJSON="$HERE/AppScope/app.json5"
BP_BACKUP="$(mktemp)"; APP_BACKUP="$(mktemp)"
cp "$PROFILE" "$BP_BACKUP"; cp "$APPJSON" "$APP_BACKUP"
restore() { cp "$BP_BACKUP" "$PROFILE"; cp "$APP_BACKUP" "$APPJSON"; rm -f "$BP_BACKUP" "$APP_BACKUP"; }
trap restore EXIT

# ── 0. 原生产物
if [[ -z "${SKIP_NATIVE:-}" ]]; then
  "$HERE/scripts/build-native.sh"
fi
for so in libdiary_capi.so libdiary_napi.so libc++_shared.so; do
  [[ -f "$HERE/entry/libs/arm64-v8a/$so" ]] \
    || { echo "缺少 entry/libs/arm64-v8a/$so（先跑 scripts/build-native.sh）" >&2; exit 1; }
done

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

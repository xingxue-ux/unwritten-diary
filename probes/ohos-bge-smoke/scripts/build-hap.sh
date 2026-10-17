#!/usr/bin/env bash
# 构建「真 bge 模型 + ORT + 核心」的探针 HAP（可签名、可切 bundleName / API 版本）。
#
# 与隔壁 `probes/ohos-core-smoke/scripts/build-hap.sh` 的差别：
#   ① 原生库换成 libdiary_bge_core.so / libdiary_bge.so；
#   ② ★ 构建前把真模型（int8 ONNX + tokenizer.json）从 $BGE_MODEL_DIR 拷进
#      `entry/src/main/resources/rawfile/`——**模型不进仓库**（.gitignore 挡着），
#      仓库里只有 models/manifest 的哈希记录。
#
# 用法：
#   scripts/build-hap.sh                              # 只构建（不签名）
#   OHOS_SIGN_STORE_FILE=...  scripts/build-hap.sh     # 构建 + 签名
#
# 环境变量：
#   BGE_MODEL_DIR   模型目录（默认 $HOME/dev/caches/bge-small-zh-v1.5-ours；
#                   也可指向仓库的 <repo>/models/bge-small-zh-v1.5-ours）
#   SKIP_NATIVE=1   跳过原生产物重建（复用 entry/libs/arm64-v8a/ 里已有的 .so）
#   SKIP_OHPM=1     跳过 ohpm install
set -euo pipefail

HERE="$(cd "$(dirname "$0")/.." && pwd)"
HVIGORW="${HVIGORW:-$HOME/command-line-tools/bin/hvigorw}"
OHPM="${OHPM:-$HOME/command-line-tools/bin/ohpm}"
BUILD_MODE="${BUILD_MODE:-debug}"
PRODUCT="${PRODUCT:-default}"
MODULE="${MODULE:-entry}"
BGE_MODEL_DIR="${BGE_MODEL_DIR:-$HOME/dev/caches/bge-small-zh-v1.5-ours}"

PROFILE="$HERE/build-profile.json5"
APPJSON="$HERE/AppScope/app.json5"
BP_BACKUP="$(mktemp)"; APP_BACKUP="$(mktemp)"
cp "$PROFILE" "$BP_BACKUP"; cp "$APPJSON" "$APP_BACKUP"
restore() { cp "$BP_BACKUP" "$PROFILE"; cp "$APP_BACKUP" "$APPJSON"; rm -f "$BP_BACKUP" "$APP_BACKUP"; }
trap restore EXIT

# ── 0. 真模型进 rawfile（先校验 sha256，防止拷错文件进包） ──────────────────
RAW="$HERE/entry/src/main/resources/rawfile"
mkdir -p "$RAW"
for f in model_quantized.onnx tokenizer.json; do
  src="$BGE_MODEL_DIR/$f"
  [[ -f "$src" ]] || { echo "缺模型文件 $src（BGE_MODEL_DIR=$BGE_MODEL_DIR）" >&2; exit 1; }
  if ! cmp -s "$src" "$RAW/$f"; then
    echo "[build-hap] stage model: $src → rawfile/$f ($(stat -c%s "$src") bytes)"
    cp "$src" "$RAW/$f"
  else
    echo "[build-hap] rawfile/$f 已是同一份（$(stat -c%s "$src") bytes）"
  fi
done
# 与 models/manifest/bge-small-zh-v1.5.json 记录的哈希对一遍（拷错文件要当场发现）
printf '%s  %s\n' \
  d44978e451a0b4e0c561b8c6bb2cfaf5aac734f2729359aae46e1db893499f43 "$RAW/model_quantized.onnx" \
  48cea5d44424912a6fd1ea647bf4fe50b55ab8b1e5879c3275f80e339e8fae26 "$RAW/tokenizer.json" \
  | sha256sum -c -

# ── 0b. 原生产物
if [[ -z "${SKIP_NATIVE:-}" ]]; then
  "$HERE/scripts/build-native.sh"
fi
[[ -f "$HERE/entry/libs/arm64-v8a/libdiary_bge_core.so" ]] \
  || { echo "缺少 entry/libs/arm64-v8a/libdiary_bge_core.so（先跑 scripts/build-native.sh）" >&2; exit 1; }

# ── 0c. ohpm install（本地 file: 依赖，不需要联网）
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

# ── 签名配置注入（密码必须是 DevEco 加密串，同第 1 步坑 2）
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
  echo "[build-hap] 包内原生库 / 模型（压缩方式也要看：Stored 才算原始体积）："
  unzip -v "$f" | grep -E "libs/|rawfile/" | sed 's/^/    /' || true
  echo "[build-hap] 逐项体积（包内 Length 合计）："
  unzip -l "$f" | tail -3 | sed 's/^/    /'
done
#!/usr/bin/env bash
# 构建最小 HAP（可签名、可切 bundleName / API 版本）。这段就是「要进 CI 的命令序列」的第一段。
#
# 用法：
#   scripts/build-hap.sh                        # 只构建（不签名）→ entry-default-unsigned.hap
#   OHOS_SIGN_STORE_FILE=...  scripts/build-hap.sh   # 构建 + 签名 → entry-default-signed.hap
#
# 全部环境变量：
#   HVIGORW                       默认 $HOME/command-line-tools/bin/hvigorw
#   OHOS_BUNDLE_NAME              覆盖 AppScope/app.json5 里的 bundleName（构建后自动还原）
#   OHOS_COMPAT_SDK_VERSION       覆盖 compatibleSdkVersion（默认 6.1.0(23)）
#   OHOS_TARGET_SDK_VERSION       覆盖 targetSdkVersion（默认与 compatible 相同）
#   OHOS_SIGN_STORE_FILE          p12 路径（绝对路径；Linux 侧同样可用 /mnt/c/...）
#   OHOS_SIGN_CERT_FILE           .cer（PEM 证书链，3 段）
#   OHOS_SIGN_PROFILE_FILE        .p7b（调试 Profile）
#   OHOS_SIGN_STORE_PASSWORD_ENC  **DevEco 加密后的十六进制串**（0000001A 开头）
#   OHOS_SIGN_KEY_PASSWORD_ENC    同上
#   OHOS_SIGN_KEY_ALIAS           默认 debugKey
#
# 实测得出的三条硬约束：
#   1) 必须用命令行工具自带的 hvigorw（~/command-line-tools/bin/hvigorw），
#      它会自己把 DEVECO_SDK_HOME 指到 ~/command-line-tools/sdk、DEVECO_NODE_HOME 指到自带 node。
#      签名那一步需要 java 在 PATH 上（本机用 ~/.local/jdk-21.0.2）。
#   2) signingConfigs 里的 storePassword/keyPassword 必须是 **DevEco 加密后的十六进制串**。
#      hvigor 6.26.4 无条件调用 DecipherUtil.decryptPwd，明文密码直接以 INVALID_DATA 退出。
#      解密用的 material 目录取的是 **p12 所在目录下的 material/**，所以 p12 放在
#      ~/.ohos/config 时，Linux 侧用 /mnt/c/Users/<user>/.ohos/config/xxx.p12 绝对路径即可解密。
#   3) hvigor 会先解出 Profile 里的 bundle-name 与 app.json5 的 bundleName 比对，
#      不一致直接 00303074 报错退出（详见 README.md「三个坑」）。
set -euo pipefail

HERE="$(cd "$(dirname "$0")/.." && pwd)"
HVIGORW="${HVIGORW:-$HOME/command-line-tools/bin/hvigorw}"
BUILD_MODE="${BUILD_MODE:-debug}"
PRODUCT="${PRODUCT:-default}"
MODULE="${MODULE:-entry}"

PROFILE="$HERE/build-profile.json5"
APPJSON="$HERE/AppScope/app.json5"
BP_BACKUP="$(mktemp)"; APP_BACKUP="$(mktemp)"
cp "$PROFILE" "$BP_BACKUP"; cp "$APPJSON" "$APP_BACKUP"
restore() { cp "$BP_BACKUP" "$PROFILE"; cp "$APP_BACKUP" "$APPJSON"; rm -f "$BP_BACKUP" "$APP_BACKUP"; }
trap restore EXIT

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

# ── 签名配置注入
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
done
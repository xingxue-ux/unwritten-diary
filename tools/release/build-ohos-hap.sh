#!/usr/bin/env bash
# HarmonyOS HAP release wrapper for the fixed product bundle name.
#
# The product HarmonyOS UI project does not exist yet. This script freezes the
# command chain from docs/architecture/m6-鸿蒙可行性探针.md and builds the
# existing bge probe as the executable carrier:
#
#   tools/release/build-ohos-hap.sh
#
# Defaults:
#   OHOS_PROJECT_DIR=probes/ohos-bge-smoke
#   bundleName=com.lingxing.unwrittendiary   (FROZEN; cannot be overridden)
#
# External prerequisite (not solvable by this script):
#   A debug or release Profile (.p7b) whose bundle-info.bundle-name is exactly
#   com.lingxing.unwrittendiary, plus the matching .p12/.cer and the two
#   DevEco-encrypted password strings. The profile is bound to a Huawei account
#   and (for debug) registered device UDIDs; generate it with DevEco
#   "Automatically generate signature" or the AGC console. There is no official
#   command-line-only path for that issuance step.
#
# Usage:
#   OHOS_SIGN_STORE_FILE=... OHOS_SIGN_CERT_FILE=... OHOS_SIGN_PROFILE_FILE=... \
#   OHOS_SIGN_STORE_PASSWORD_ENC=... OHOS_SIGN_KEY_PASSWORD_ENC=... \
#     tools/release/build-ohos-hap.sh [--dry-run]
#
# Environment:
#   OHOS_PROJECT_DIR   default probes/ohos-bge-smoke
#   BUILD_MODE         debug|release, default release
#   HVIGORW            default $HOME/command-line-tools/bin/hvigorw
#   OHPM               default $HOME/command-line-tools/bin/ohpm
#   NDK                OpenHarmony native SDK (default ~/command-line-tools/sdk/default/openharmony/native)
#   OHOS_SIGN_*        as documented in probes/ohos-*/scripts/build-hap.sh
#   BGE_MODEL_DIR      passed through to the probe (if it stages a model)
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
PROJECT_DIR="${OHOS_PROJECT_DIR:-$ROOT/probes/ohos-bge-smoke}"
BUNDLE_NAME="com.lingxing.unwrittendiary"
BUILD_MODE="${BUILD_MODE:-release}"
HVIGORW="${HVIGORW:-$HOME/command-line-tools/bin/hvigorw}"
OHPM="${OHPM:-$HOME/command-line-tools/bin/ohpm}"
NDK="${NDK:-$HOME/command-line-tools/sdk/default/openharmony/native}"
DRY_RUN=0
WORK_DIR="$ROOT/build/ohos-release"
SIGN_TOOL="$HOME/command-line-tools/sdk/default/openharmony/toolchains/lib/hap-sign-tool.jar"

usage() { sed -n '2,35p' "$0" | sed 's/^# \{0,1\}//'; }
fail() { echo "[build-ohos][失败] $*" >&2; exit 2; }
step() { echo; echo "==> $*"; }
run() {
  if ((DRY_RUN)); then printf '[dry-run]'; printf ' %q' "$@"; printf '\n'; else "$@"; fi
}

while (($#)); do
  case "$1" in
    --dry-run) DRY_RUN=1 ;;
    -h|--help) usage; exit 0 ;;
    *) fail "未知参数：$1（--help 看用法）" ;;
  esac
  shift
done

if [[ -n "${OHOS_BUNDLE_NAME:-}" && "$OHOS_BUNDLE_NAME" != "$BUNDLE_NAME" ]]; then
  fail "bundleName 已定案为 $BUNDLE_NAME；不接受覆盖值 $OHOS_BUNDLE_NAME"
fi
export OHOS_BUNDLE_NAME="$BUNDLE_NAME"

step "1/6 检查鸿蒙工具链"
[[ -x "$HVIGORW" ]] || fail "找不到可执行的 hvigorw：$HVIGORW（或用 HVIGORW 指定）"
[[ -x "$OHPM" ]] || fail "找不到可执行的 ohpm：$OHPM（或用 OHPM 指定）"
command -v java >/dev/null || fail "签名需要 java（JDK 21）在 PATH 上"
[[ -f "$SIGN_TOOL" ]] || fail "找不到 hap-sign-tool.jar：$SIGN_TOOL"
[[ -d "$NDK" ]] || fail "找不到 OpenHarmony NDK：$NDK（或用 NDK 指定）"
[[ -d "$PROJECT_DIR" ]] || fail "找不到鸿蒙工程目录：$PROJECT_DIR"
[[ -f "$PROJECT_DIR/scripts/build-hap.sh" ]] || fail "$PROJECT_DIR 没有 scripts/build-hap.sh；产品鸿蒙工程需要先按 m6 文档搭出同样的脚本入口"
rustup target list --installed 2>/dev/null | grep -qx 'aarch64-unknown-linux-ohos' \
  || fail "缺少 Rust target aarch64-unknown-linux-ohos（rustup target add aarch64-unknown-linux-ohos）"
mkdir -p "$WORK_DIR"
# Do not let probe scripts fall back to /tmp. Their mktemp usage then stays
# inside this repository's ignored build/ directory.
export TMPDIR="$WORK_DIR/tmp"
mkdir -p "$TMPDIR"
echo "project:  $PROJECT_DIR"
echo "bundle:   $BUNDLE_NAME"
echo "mode:     $BUILD_MODE"

step "2/6 检查签名材料（外部前置）"
missing=()
for name in OHOS_SIGN_STORE_FILE OHOS_SIGN_CERT_FILE OHOS_SIGN_PROFILE_FILE \
            OHOS_SIGN_STORE_PASSWORD_ENC OHOS_SIGN_KEY_PASSWORD_ENC; do
  [[ -n "${!name:-}" ]] || missing+=("$name")
done
if ((${#missing[@]})); then
  cat >&2 <<EOF
[build-ohos][失败] 缺少签名环境变量：${missing[*]}

这不是脚本能代办的步骤。先把下面这份 Profile 准备好：
  bundle-info.bundle-name = $BUNDLE_NAME
  type = debug（真机调试）或 release（上架）
  调试 Profile 还要包含目标设备的 UDID
生成方式：DevEco「File → Project Structure → Signing Configs →
Automatically generate signature」，或 AGC 控制台手工创建应用/注册设备/下载
.p7b/.cer/.p12。命令行没有官方签发入口（m6 文档 §13.4 已实测）。

然后设置：
  OHOS_SIGN_STORE_FILE=<.p12 绝对路径>
  OHOS_SIGN_CERT_FILE=<.cer 绝对路径>
  OHOS_SIGN_PROFILE_FILE=<.p7b 绝对路径>
  OHOS_SIGN_STORE_PASSWORD_ENC=<DevEco 加密串，0000001A 开头>
  OHOS_SIGN_KEY_PASSWORD_ENC=<DevEco 加密串>
加密串可用 probes/ohos-bge-smoke/scripts/ohos-sign-pwd.js 现场生成。
EOF
  exit 2
fi
for f in "$OHOS_SIGN_STORE_FILE" "$OHOS_SIGN_CERT_FILE" "$OHOS_SIGN_PROFILE_FILE"; do
  [[ -f "$f" ]] || fail "签名文件不存在：$f"
done
[[ "$OHOS_SIGN_STORE_FILE" == /* ]] || fail "OHOS_SIGN_STORE_FILE 必须是绝对路径（hvigor 在工程目录下解析相对路径）"

step "3/6 核对 Profile 的 bundleName"
VERIFY_JSON="$WORK_DIR/profile-verify.json"
if ((DRY_RUN)); then
  echo "[dry-run] java -jar $SIGN_TOOL verify-profile -inFile $OHOS_SIGN_PROFILE_FILE -outFile $VERIFY_JSON"
else
  java -jar "$SIGN_TOOL" verify-profile -inFile "$OHOS_SIGN_PROFILE_FILE" -outFile "$VERIFY_JSON" >/dev/null
  python3 - "$VERIFY_JSON" "$BUNDLE_NAME" <<'PY'
import json, re, sys
path, expected = sys.argv[1], sys.argv[2]
raw = open(path, encoding="utf-8", errors="replace").read()
found = set(re.findall(r'"bundle-name"\s*:\s*"([^"]+)"', raw))
if not found:
    # Some tool versions wrap the profile JSON inside a string.
    found = set(re.findall(r'bundle-name\\?"\s*:\s*\\?"([^"\\]+)', raw))
if not found:
    print("[build-ohos][失败] 无法从 verify-profile 输出里读出 bundle-name", file=sys.stderr)
    sys.exit(2)
if found != {expected}:
    print(f"[build-ohos][失败] Profile 的 bundleName 是 {sorted(found)}，期望 {expected!r}。", file=sys.stderr)
    print("请用 DevEco/AGC 为产品 bundleName 重新签发 Profile；改 app.json5 或绕过检查都不能安装。", file=sys.stderr)
    sys.exit(2)
print(f"[build-ohos] Profile bundleName 匹配：{expected}")
PY
fi

step "4/6 构建 HAP（原生库 + 模型 + 签名）"
# Probe build-hap.sh accepts OHOS_BUNDLE_NAME and restores the source json5 on
# exit. The fixed value is exported above, so even a product project following
# the same interface gets the right bundle name.
( cd "$PROJECT_DIR" && run env BUILD_MODE="$BUILD_MODE" OHOS_BUNDLE_NAME="$BUNDLE_NAME" \
    bash ./scripts/build-hap.sh )

step "5/6 核对产物"
if ((DRY_RUN)); then
  echo "[dry-run] 跳过 HAP 产物核对"
  exit 0
fi
HAP="$(find "$PROJECT_DIR/entry/build" -type f -name '*signed.hap' -printf '%T@ %p\n' 2>/dev/null | sort -nr | head -1 | cut -d' ' -f2-)"
[[ -n "$HAP" && -f "$HAP" ]] || fail "没有找到签名后的 .hap（看上面的 hvigor 输出）"
mkdir -p "$ROOT/dist/ohos"
cp -f "$HAP" "$ROOT/dist/ohos/"
BASE="$(basename "$HAP")"
echo "HAP：$ROOT/dist/ohos/$BASE"
printf 'bytes=%s sha256=%s\n' "$(stat -c%s "$ROOT/dist/ohos/$BASE")" "$(sha256sum "$ROOT/dist/ohos/$BASE" | cut -d' ' -f1)"

# Verify the bundle name actually written into the package, not just the input.
python3 - "$ROOT/dist/ohos/$BASE" "$BUNDLE_NAME" <<'PY'
import json, sys, zipfile
path, expected = sys.argv[1], sys.argv[2]
with zipfile.ZipFile(path) as z:
    data = json.loads(z.read("module.json").decode("utf-8"))
app = data.get("app") or data.get("module", {}).get("app") or {}
actual = app.get("bundleName") or data.get("module", {}).get("bundleName")
if actual != expected:
    print(f"[build-ohos][失败] HAP 内 bundleName={actual!r}，期望 {expected!r}", file=sys.stderr)
    sys.exit(2)
print(f"[build-ohos] HAP 内 bundleName 校验通过：{actual}")
PY

step "6/6 体积与后续"
unzip -v "$ROOT/dist/ohos/$BASE" | awk 'NR<=2 || /libs\/|rawfile\//' | head -60
cat <<EOF

没有做的事：
  - 没有在本次执行里跑 hvigor / 签名 / 真机安装；
  - 没有构建真正的产品鸿蒙 UI 工程（当前载体是 $PROJECT_DIR 的探针）；
  - release Profile 上架流程（assembleApp / AGC）未验证。
安装/启动（需要 hdc 与已注册设备）：
  hdc install "$ROOT/dist/ohos/$BASE"
  hdc shell "aa force-stop $BUNDLE_NAME"
  hdc shell "aa start -a EntryAbility -b $BUNDLE_NAME -m entry"
EOF

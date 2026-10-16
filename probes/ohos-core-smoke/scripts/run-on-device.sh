#!/usr/bin/env bash
# 装到真机 → 启动 → 抓 hilog（tag=DiaryCore）。这是「要进 CI 的命令序列」的最后一段。
#
# 用法：
#   scripts/run-on-device.sh <bundleName> [hapPath]
#   例：HDC="/mnt/e/huawei/DevEco Studio/sdk/default/openharmony/toolchains/hdc.exe" \
#         scripts/run-on-device.sh com.example.myapplication
#
# 环境变量：
#   HDC                  hdc 可执行文件（真机挂在 Windows 侧，传 Windows 版 hdc.exe 的 WSL 路径）
#   HDC_STAGE_DIR        hdc.exe 能读到的 **Windows 路径**（默认 E:\tmp\ohos-core-smoke）
#   HDC_STAGE_DIR_LINUX  同一个目录的 WSL 路径（默认 /mnt/e/tmp/ohos-core-smoke）
#   ABILITY              默认 EntryAbility
#   MODULE               默认 entry
#   UNINSTALL            1（默认）= 先卸载再装，拿到「全新安装」的干净状态；0 = 直接覆盖装
#   HILOG_TAG            默认 DiaryCore
#
# 注意（第 1 步实测，这里继承）：`hdc install` **失败时退出码仍是 0**，
# 所以显式 grep 输出判定成败，不能信 $?。CI 里同理。
set -euo pipefail

BUNDLE="${1:?usage: run-on-device.sh <bundleName> [hapPath]}"
HERE="$(cd "$(dirname "$0")/.." && pwd)"
HAP="${2:-$HERE/entry/build/default/outputs/default/entry-default-signed.hap}"
HDC="${HDC:-hdc}"
ABILITY="${ABILITY:-EntryAbility}"
MODULE="${MODULE:-entry}"
STAGE_WIN="${HDC_STAGE_DIR:-E:\\tmp\\ohos-core-smoke}"
STAGE_LINUX="${HDC_STAGE_DIR_LINUX:-/mnt/e/tmp/ohos-core-smoke}"
TAG="${HILOG_TAG:-DiaryCore}"

[[ -f "$HAP" ]] || { echo "hap not found: $HAP" >&2; exit 1; }
mkdir -p "$STAGE_LINUX"
cp "$HAP" "$STAGE_LINUX/"
STAGED="$STAGE_WIN\\$(basename "$HAP")"

if [[ "${UNINSTALL:-1}" == "1" ]]; then
  echo "== [0/5] uninstall（失败无所谓：本来就没装）"
  "$HDC" uninstall "$BUNDLE" 2>&1 || true
fi

echo "== [1/5] install $STAGED"
OUT="$("$HDC" install "$STAGED" 2>&1)" || true
echo "$OUT"
grep -q "install bundle successfully" <<<"$OUT" || { echo "!! INSTALL FAILED"; exit 1; }

echo "== [2/5] 清空旧 hilog 缓冲"
"$HDC" shell "hilog -r" 2>&1 || true

echo "== [3/5] launch $BUNDLE/$ABILITY"
"$HDC" shell "aa start -a $ABILITY -b $BUNDLE -m $MODULE"

sleep 6
echo "== [4/5] process"
"$HDC" shell "pidof $BUNDLE" || true

echo "== [5/5] hilog (tag=$TAG)"
"$HDC" shell "hilog -x -T $TAG" 2>&1 | grep -E "CORE_MARKER|$TAG" || true

cat <<EOF

卸载： $HDC uninstall $BUNDLE
EOF
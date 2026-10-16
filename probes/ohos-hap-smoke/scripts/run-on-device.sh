#!/usr/bin/env bash
# 装到真机 → 启动 → 抓 hilog。这是「要进 CI 的命令序列」的第二段。
#
# 用法：
#   scripts/run-on-device.sh <bundleName> [hapPath]
#   例：scripts/run-on-device.sh com.example.myapplication \
#         entry/build/default/outputs/default/entry-default-signed.hap
#
# 环境变量：
#   HDC                  hdc 可执行文件（默认 PATH 上的 hdc；本机真机挂在 Windows 侧，
#                        所以这里传 Windows 版 hdc.exe 的 WSL 路径）
#   HDC_STAGE_DIR        hdc.exe 能读到的**Windows 路径**（默认 E:\tmp\ohos-hap-smoke）
#   HDC_STAGE_DIR_LINUX  同一个目录的 WSL 路径（默认 /mnt/e/tmp/ohos-hap-smoke）
#   ABILITY             默认 EntryAbility
#   MODULE              默认 entry
#
# 注意（实测）：`hdc install` **失败时退出码仍是 0**，所以这里显式 grep 输出判定成败，
# 不能用 $?。CI 里同理。
set -euo pipefail

BUNDLE="${1:?usage: run-on-device.sh <bundleName> [hapPath]}"
HERE="$(cd "$(dirname "$0")/.." && pwd)"
HAP="${2:-$HERE/entry/build/default/outputs/default/entry-default-signed.hap}"
HDC="${HDC:-hdc}"
ABILITY="${ABILITY:-EntryAbility}"
MODULE="${MODULE:-entry}"
STAGE_WIN="${HDC_STAGE_DIR:-E:\\tmp\\ohos-hap-smoke}"
STAGE_LINUX="${HDC_STAGE_DIR_LINUX:-/mnt/e/tmp/ohos-hap-smoke}"

[[ -f "$HAP" ]] || { echo "hap not found: $HAP" >&2; exit 1; }
mkdir -p "$STAGE_LINUX"
cp "$HAP" "$STAGE_LINUX/"
STAGED="$STAGE_WIN\\$(basename "$HAP")"

echo "== [1/4] install $STAGED"
OUT="$("$HDC" install "$STAGED" 2>&1)" || true
echo "$OUT"
grep -q "install bundle successfully" <<<"$OUT" || { echo "!! INSTALL FAILED"; exit 1; }

echo "== [2/4] launch $BUNDLE/$ABILITY"
"$HDC" shell "aa start -a $ABILITY -b $BUNDLE -m $MODULE"

sleep 3
echo "== [3/4] process"
"$HDC" shell "pidof $BUNDLE" || true

echo "== [4/4] hilog (tag=DiarySmoke)"
"$HDC" shell "hilog -x -T DiarySmoke" 2>&1 | grep -E "SMOKE_MARKER|DiarySmoke" || true

cat <<EOF

卸载： $HDC uninstall $BUNDLE
EOF
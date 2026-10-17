#!/usr/bin/env bash
# 装到真机 → 启动 → 抓 hilog（tag=DiaryBge）。这是「要进 CI 的命令序列」的最后一段。
#
# 用法：
#   scripts/run-on-device.sh <bundleName> [hapPath]
#   例：HDC="/mnt/e/huawei/DevEco Studio/sdk/default/openharmony/toolchains/hdc.exe" \
#         scripts/run-on-device.sh com.example.myapplication
#
# 环境变量：
#   HDC                  hdc 可执行文件（真机挂在 Windows 侧，传 Windows 版 hdc.exe 的 WSL 路径）
#   HDC_STAGE_DIR        hdc.exe 能读到的 **Windows 路径**（默认 E:\tmp\ohos-bge-smoke）
#   HDC_STAGE_DIR_LINUX  同一个目录的 WSL 路径（默认 /mnt/e/tmp/ohos-bge-smoke）
#   ABILITY              默认 EntryAbility
#   MODULE               默认 entry
#   UNINSTALL            1（默认）= 先卸载再装；0 = 直接覆盖装
#   FORCE_STOP           1（默认）= 先 force-stop，保证是**冷启动**（否则 aa start 只把
#                        已在跑的进程拉到前台，onWindowStageCreate 不会再跑，日志是空的）
#   WAIT_SECONDS         启动后等几秒再抓日志（默认 45：真模型加载 + 17 段文本嵌入）
#   OUT_FILE             原始日志落点（默认 evidence/device-hilog-bge.txt）
#   HILOG_TAG            默认 DiaryBge
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
STAGE_WIN="${HDC_STAGE_DIR:-E:\\tmp\\ohos-bge-smoke}"
STAGE_LINUX="${HDC_STAGE_DIR_LINUX:-/mnt/e/tmp/ohos-bge-smoke}"
TAG="${HILOG_TAG:-DiaryBge}"
WAIT_SECONDS="${WAIT_SECONDS:-45}"
OUT_FILE="${OUT_FILE:-$HERE/evidence/device-hilog-bge.txt}"

mkdir -p "$(dirname "$OUT_FILE")"

if [[ "${UNINSTALL:-1}" == "1" ]]; then
  [[ -f "$HAP" ]] || { echo "hap not found: $HAP" >&2; exit 1; }
  mkdir -p "$STAGE_LINUX"
  cp "$HAP" "$STAGE_LINUX/"
  STAGED="$STAGE_WIN\\$(basename "$HAP")"

  echo "== [0/7] uninstall（失败无所谓：本来就没装）"
  "$HDC" uninstall "$BUNDLE" 2>&1 || true

  echo "== [1/7] install $STAGED"
  OUT="$("$HDC" install "$STAGED" 2>&1)" || true
  echo "$OUT"
  grep -q "install bundle successfully" <<<"$OUT" || { echo "!! INSTALL FAILED"; exit 1; }
fi

if [[ "${FORCE_STOP:-1}" == "1" ]]; then
  echo "== [2/7] force-stop（保证冷启动：已在跑的进程不会重跑 onWindowStageCreate）"
  "$HDC" shell "aa force-stop $BUNDLE" 2>&1 || true
  sleep 2
fi

echo "== [3/7] 清空旧 hilog 缓冲"
"$HDC" shell "hilog -r" 2>&1 || true

echo "== [4/7] launch $BUNDLE/$ABILITY"
"$HDC" shell "aa start -a $ABILITY -b $BUNDLE -m $MODULE"

echo "== [5/7] 等 $WAIT_SECONDS s（真模型加载 + 全部嵌入）"
sleep "$WAIT_SECONDS"

echo "== [6/7] process"
"$HDC" shell "pidof $BUNDLE" || true

echo "== [7/7] hilog (tag=$TAG) → $OUT_FILE"
"$HDC" shell "hilog -x -T $TAG" > "$OUT_FILE" 2>&1
printf '   行数=%s  分片=%s\n' "$(wc -l < "$OUT_FILE")" "$(grep -c 'BGE_JSON' "$OUT_FILE" || true)"

cat <<EOF

下一步（本地解析）：
  python3 scripts/parse-hilog.py $OUT_FILE
  python3 scripts/compare-host-device.py evidence/device-bge.json evidence/host-control.txt
卸载： $HDC uninstall $BUNDLE
EOF
#!/usr/bin/env bash
# 把**自编的 ONNX Runtime 1.29.1** 以「不与 sherpa 撞名」的方式放进 HAP。
#
# 为什么需要第二份 ORT：同一个 HAP 里两条能力要的 ORT **不是同一版**——
#
#   sherpa-onnx 1.13.8 的 OHOS 预编译库：NEEDED = `libonnxruntime.so`，
#     且按**符号版本**解析 `OrtGetApiBase@VERS_1.16.3`；
#   我们自编的 1.29.1（`ort` crate 走 load-dynamic）：只导出 `@@VERS_1.29.1`。
#
# 两者不能互换（换 1.29.1 顶 `libonnxruntime.so` ⇒ sherpa 的 ldso 报
# `relocating failed: symbol not found ... s=OrtGetApiBase`，见
# apps/ohos-diary-core/README.md 的转写片）。同一个 `libs/arm64/` 又**不能有两个同名
# 文件**，所以：
#
#   libonnxruntime.so                  ← sherpa 自带的 1.16.3（满足它的 NEEDED，按名解析）
#   libonnxruntime-diary-1.29.1.so     ← 我们自编的 1.29.1（`ort` 按 init_from 的
#                                          **绝对路径** dlopen，不看文件名与 SONAME 是否一致）
#
# 本脚本只负责「拷进去 + 自检」，不下载、不构建。来源：
#   $HOME/dev/caches/ort-1291/libonnxruntime.so（构建配方见
#   probes/ohos-core-smoke/evidence/ort-1.29.1-build.md；manifest.json 里有 sha256）
#
# 用法：
#   apps/ohos-diary-core/scripts/stage-ort-1291.sh
#   DIARY_ORT_SRC=/path/to/libonnxruntime.so apps/ohos-diary-core/scripts/stage-ort-1291.sh
set -euo pipefail

HERE="$(cd "$(dirname "$0")/.." && pwd)"
OUT="$HERE/entry/libs/arm64-v8a"
SRC="${DIARY_ORT_SRC:-$HOME/dev/caches/ort-1291/libonnxruntime.so}"
NDK="${NDK:-$HOME/command-line-tools/sdk/default/openharmony/native}"
READELF="$NDK/llvm/bin/llvm-readelf"
TARGET_NAME="libonnxruntime-diary-1.29.1.so"
SHERPA_ORT="libonnxruntime.so"

[[ -f "$SRC" ]] || { echo "缺少 $SRC（自编 ORT 1.29.1）" >&2; exit 1; }
mkdir -p "$OUT"
cp -f "$SRC" "$OUT/$TARGET_NAME"

echo "[stage-ort] 来源 $SRC"
printf '[stage-ort] %-36s bytes=%s sha256=%s\n' "$TARGET_NAME" \
  "$(stat -c%s "$OUT/$TARGET_NAME")" "$(sha256sum "$OUT/$TARGET_NAME" | cut -d' ' -f1)"

[[ -x "$READELF" ]] || { echo "[stage-ort] 没有 llvm-readelf：$READELF" >&2; exit 1; }

echo "[stage-ort] -- $TARGET_NAME 的动态段"
"$READELF" -d "$OUT/$TARGET_NAME" | grep -E 'NEEDED|SONAME' | sed 's/^/[stage-ort] /'
echo "[stage-ort] -- $TARGET_NAME 的 OrtGetApiBase 符号版本"
"$READELF" --dyn-syms "$OUT/$TARGET_NAME" | grep 'OrtGetApiBase' | sed 's/^/[stage-ort] /'

# 自检 1：导出的必须是 1.29.1 的版本串（拿错文件后面会在设备上以「符号找不到」出现）。
"$READELF" --dyn-syms "$OUT/$TARGET_NAME" | grep -q 'OrtGetApiBase@@VERS_1.29.1' \
  || { echo "[stage-ort] 失败：$TARGET_NAME 没导出 OrtGetApiBase@@VERS_1.29.1" >&2; exit 1; }

# 自检 2：SONAME **不能**是 `libonnxruntime.so`。否则一旦哪条路径按名加载，
# ldso 可能把这份 1.29.1 当成 sherpa 要的 1.16.3（版本串对不上就是加载失败）。
# 只在这份文件的 SONAME 恰好是 dirname 时报警；不是硬失败——`ort` 按绝对路径加载，
# 不看 SONAME，但这是一个该被看见的坑。
soname="$("$READELF" -d "$OUT/$TARGET_NAME" | sed -n 's/.*SONAME.*\[\(.*\)\].*/\1/p')"
if [[ "$soname" == "$SHERPA_ORT" ]]; then
  echo "[stage-ort] 警告：$TARGET_NAME 的 SONAME 就是 $SHERPA_ORT；按名加载时可能与 sherpa 的 1.16.3 互串" >&2
fi

# 自检 3：sherpa 那一份必须在，且导出 1.16.3（顺序无关，只验存在与版本）。
[[ -f "$OUT/$SHERPA_ORT" ]] \
  || { echo "缺少 $OUT/$SHERPA_ORT（先跑 scripts/stage-sherpa.sh）" >&2; exit 1; }
"$READELF" --dyn-syms "$OUT/$SHERPA_ORT" | grep -q 'OrtGetApiBase@@VERS_1.16.3' \
  || { echo "[stage-ort] 失败：$SHERPA_ORT 不是 sherpa 那份 1.16.3（没导出 VERS_1.16.3）" >&2; exit 1; }
printf '[stage-ort] %-36s bytes=%s sha256=%s（sherpa 的 1.16.3）\n' "$SHERPA_ORT" \
  "$(stat -c%s "$OUT/$SHERPA_ORT")" "$(sha256sum "$OUT/$SHERPA_ORT" | cut -d' ' -f1)"

echo "[stage-ort] OK → $OUT"

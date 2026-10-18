#!/usr/bin/env bash
# 把 sherpa-onnx 的 **OHOS arm64** C API 库放进 HAP 的原生库目录。
#
# 为什么是「放进去」而不是「运行时从沙箱 dlopen」：
#   OHOS 的应用进程只能从自己的 `libs/<abi>`（设备上 `/data/storage/el1/bundle/libs/arm64`）
#   加载随包原生库；从应用数据目录 dlopen 会被 linker/SELinux 拦住。所以引擎库与
#   libonnxruntime.so 必须和 libdiary_capi.so 一起打进 HAP。
#
# 来源（公开 release，v1.13.8 与核心钉死的 C API 版本一致）：
#   https://github.com/k2-fsa/sherpa-onnx/releases/download/v1.13.8/sherpa-onnx-v1.13.8-ohos-arm64-v8a.tar.bz2
#   包内含 lib/libsherpa-onnx-c-api.so、lib/libonnxruntime.so、include/.../c-api.h
#
# 用法：
#   SHERPA_LIB_DIR=$HOME/dev/caches/asr-ohos/sherpa-onnx-v1.13.8-ohos-arm64-v8a/lib \
#     apps/ohos-diary-core/scripts/stage-sherpa.sh
#
# 默认从 `$HOME/dev/caches/asr-ohos/...` 取；缓存目录不在仓库里（228 MB 模型 + 16 MB
# 库都不入库）。`entry/libs/` 本身也在 .gitignore 里：本脚本是「怎么拿到这些库」的
# 可复现记录，产物由构建时生成。
set -euo pipefail

HERE="$(cd "$(dirname "$0")/.." && pwd)"
SRC="${SHERPA_LIB_DIR:-$HOME/dev/caches/asr-ohos/sherpa-onnx-v1.13.8-ohos-arm64-v8a/lib}"
OUT="$HERE/entry/libs/arm64-v8a"
mkdir -p "$OUT"

for so in libsherpa-onnx-c-api.so libonnxruntime.so; do
  [[ -f "$SRC/$so" ]] || {
    echo "缺少 $SRC/$so（先解压 sherpa-onnx-v1.13.8-ohos-arm64-v8a.tar.bz2）" >&2
    exit 1
  }
  cp -f "$SRC/$so" "$OUT/$so"
done

echo "[stage-sherpa] 来源 $SRC"
for so in libsherpa-onnx-c-api.so libonnxruntime.so; do
  printf '[stage-sherpa] %-28s bytes=%s sha256=%s\n' "$so" \
    "$(stat -c%s "$OUT/$so")" "$(sha256sum "$OUT/$so" | cut -d' ' -f1)"
done

# 自检：它必须是 aarch64 + 依赖 libonnxruntime.so（否则打进 HAP 也加载不起来）。
FILE_BIN="$(command -v file || true)"
if [[ -n "$FILE_BIN" ]]; then
  "$FILE_BIN" -b "$OUT/libsherpa-onnx-c-api.so" | sed 's/^/[stage-sherpa] /'
fi
if command -v readelf >/dev/null 2>&1; then
  if ! readelf -d "$OUT/libsherpa-onnx-c-api.so" | grep -q 'libonnxruntime.so'; then
    echo "[stage-sherpa] 警告：libsherpa-onnx-c-api.so 的 NEEDED 里没有 libonnxruntime.so" >&2
  fi
fi
echo "[stage-sherpa] OK → $OUT"

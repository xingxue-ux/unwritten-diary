#!/usr/bin/env bash
# 编出打进 HAP 的三个原生库（**纯命令行、不需要 DevEco GUI、不需要 CMake**）：
#
#   1. libdiary_bge_core.so  ← Rust cdylib（`rust/` 探针 + 产品 diary_core 全量依赖 + ort crate）
#   2. libdiary_bge.so       ← C++ NAPI shim（dlopen 上面那个，暴露成 ArkTS 能 import 的模块）
#   3. libonnxruntime.so     ← 自编 ORT 1.29.1（长期缓存，见下）
#   + libc++_shared.so       ← 必须随包带（第 2 步实测：应用进程的 linker namespace 看不到系统那份）
#
# 全部落到 `entry/libs/arm64-v8a/`，hvigor 的 ProcessLibs 任务会自动把它们收进 HAP 的
# `libs/arm64-v8a/`（不需要在 hvigor 里配 externalNativeOptions / CMakeLists）。
#
# ★ 路径纪律：**不用 /tmp**（本机 /tmp 会被清空）。缓存与构建产物一律放 $HOME：
#     $HOME/dev/caches/ort-1291/libonnxruntime.so   自编 ORT（19.6 MB，见 README §ORT）
#     $HOME/dt-ohosbge                              交叉编译 target
#
# ORT 不再由本脚本自己找：统一交给 `tools/ohos/build-ort.sh`（缓存优先、幂等、
# 自检都在那里），本脚本只负责把最终产物复制进 entry/libs/。
#
# 环境变量：
#   NDK              默认 $HOME/command-line-tools/sdk/default/openharmony/native
#   OHOS_TARGET      默认 aarch64-unknown-linux-ohos
#   CARGO_TARGET_DIR 默认 $HOME/dt-ohosbge
#   ORT_SO_DIR       放 libonnxruntime.so 的目录（兼容旧名；等同 ORT_CACHE_DIR）
#   ORT_CACHE_DIR    同 tools/ohos/build-ort.sh，默认 $HOME/dev/caches/ort-1291
#   SKIP_RUST=1 / SKIP_SHIM=1 / SKIP_ORT=1   只重建其中一部分
set -euo pipefail

HERE="$(cd "$(dirname "$0")/.." && pwd)"
REPO_ROOT="$(cd "$HERE/../.." && pwd)"
NDK="${NDK:-$HOME/command-line-tools/sdk/default/openharmony/native}"
TARGET="${OHOS_TARGET:-aarch64-unknown-linux-ohos}"
CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$HOME/dt-ohosbge}"
OUT="$HERE/entry/libs/arm64-v8a"
mkdir -p "$OUT"

CC_PREFIX="$NDK/llvm/bin/aarch64-unknown-linux-ohos"
[[ -x "$CC_PREFIX-clang" ]] || { echo "NDK clang not found: $CC_PREFIX-clang" >&2; exit 1; }

# ── 1. Rust cdylib（探针核心 + diary_core + ort crate） ──────────────────────
if [[ -z "${SKIP_RUST:-}" ]]; then
  echo "== [1/4] cargo build --release --target $TARGET (libdiary_bge_core.so)"
  # 交叉编译只需要三样东西：linker + C 编译器（rusqlite bundled 的 SQLite C 代码）+ ar。
  export CARGO_TARGET_DIR
  export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_OHOS_LINKER="$CC_PREFIX-clang"
  export CC_aarch64_unknown_linux_ohos="$CC_PREFIX-clang"
  export AR_aarch64_unknown_linux_ohos="$NDK/llvm/bin/llvm-ar"
  ( cd "$HERE/rust" && cargo build --target "$TARGET" --release --offline )
  cp "$CARGO_TARGET_DIR/$TARGET/release/libdiary_bge_core.so" "$OUT/"
fi

# ── 2. C++ NAPI shim ────────────────────────────────────────────────────────
if [[ -z "${SKIP_SHIM:-}" ]]; then
  echo "== [2/4] clang++ -shared (libdiary_bge.so)"
  "$CC_PREFIX-clang++" \
    -shared -fPIC -O2 -std=c++17 \
    -Wall -Wextra \
    -Wl,-soname,libdiary_bge.so \
    -o "$OUT/libdiary_bge.so" \
    "$HERE/entry/src/main/cpp/napi_shim.cpp" \
    -I"$HERE/entry/src/main/cpp" \
    -lace_napi.z
  # ★ 必须把 libc++_shared.so 一起打进 HAP：设备上 /system/lib64/libc++_shared.so 确实存在，
  #   但应用进程的 linker namespace 看不到它（第 2 步 §24.1 的原始报错）：
  #     MUSL-LDSO: load libc++_shared.so failed, namespace=ndk no inherits, errno=2
  cp "$NDK/llvm/lib/aarch64-linux-ohos/libc++_shared.so" "$OUT/libc++_shared.so"
fi

# ── 3. ONNX Runtime（自编 1.29.1；`ort` crate 要求 API ≥17，见第 2 步 §23.3） ──
# 统一走 tools/ohos/build-ort.sh：先查缓存（manifest sha256 + ELF 自检），命中的话
# 秒级返回；未命中才完整构建（~33 min/16 核）。自检与「从哪来」的说明都在那边。
if [[ -z "${SKIP_ORT:-}" ]]; then
  echo "== [3/4] ORT 1.29.1（tools/ohos/build-ort.sh）"
  ORT_CACHE_DIR="${ORT_SO_DIR:-${ORT_CACHE_DIR:-$HOME/dev/caches/ort-1291}}" \
    "$REPO_ROOT/tools/ohos/build-ort.sh" --dest "$OUT"
fi

# ── 4. 自检：ELF 类型、NEEDED、导出符号 ──────────────────────────────────────
echo "== [4/4] 产物自检"
for so in "$OUT"/libdiary_bge_core.so "$OUT"/libdiary_bge.so "$OUT"/libonnxruntime.so "$OUT"/libc++_shared.so; do
  [[ -f "$so" ]] || continue
  printf '\n-- %s  bytes=%s\n' "$(basename "$so")" "$(stat -c%s "$so")"
  file -b "$so"
  "$NDK/llvm/bin/llvm-readelf" -d "$so" | grep -E "NEEDED|SONAME" || true
done
echo
echo "-- libdiary_bge_core.so 导出（dlopen 要用的三个符号）"
"$NDK/llvm/bin/llvm-readelf" --dyn-syms "$OUT/libdiary_bge_core.so" | grep -E "diary_bge" || true
echo "-- libdiary_bge.so 导出（NAPI 模块注册）"
"$NDK/llvm/bin/llvm-readelf" --dyn-syms "$OUT/libdiary_bge.so" | grep -iE "napi_module_register" || true
echo "-- libonnxruntime.so 版本戳（OrtGetApiBase 的符号版本 = ORT 版本）"
"$NDK/llvm/bin/llvm-readelf" --dyn-syms "$OUT/libonnxruntime.so" | grep -E "OrtGetApiBase" || true
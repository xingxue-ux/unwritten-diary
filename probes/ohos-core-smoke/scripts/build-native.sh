#!/usr/bin/env bash
# 编出打进 HAP 的两个原生库（**纯命令行、不需要 DevEco GUI、不需要 CMake**）：
#
#   1. libdiary_core_smoke.so  ← Rust cdylib（`rust/` 薄 C ABI 层 + 产品 diary_core 全量依赖）
#   2. libdiary_napi.so        ← C++ NAPI shim（dlopen 上面那个，暴露成 ArkTS 能 import 的模块）
#
# 两个都落到 `entry/libs/arm64-v8a/`，hvigor 的 ProcessLibs 任务会自动把它们收进 HAP 的
# `libs/arm64-v8a/`（不需要在 hvigor 里配 externalNativeOptions / CMakeLists）。
#
# 环境变量：
#   NDK            默认 $HOME/command-line-tools/sdk/default/openharmony/native
#   OHOS_TARGET    默认 aarch64-unknown-linux-ohos
#   CARGO_TARGET_DIR 默认 /tmp/ohos-core-smoke/target（**不落进仓库**）
#   ORT_FLAVOR     self-built（默认，自编 1.29.1，走 tools/ohos/build-ort.sh）
#                  | sherpa-1.16.3（历史快路，只用来复现「1.16.3 也能 dlopen」）
#   ORT_SO_DIR     已有 ORT 目录（兼容旧名；等同 ORT_CACHE_DIR）
#   SKIP_RUST=1 / SKIP_SHIM=1 / SKIP_ORT=1   只重建其中一个
set -euo pipefail

HERE="$(cd "$(dirname "$0")/.." && pwd)"
REPO_ROOT="$(cd "$HERE/../.." && pwd)"
NDK="${NDK:-$HOME/command-line-tools/sdk/default/openharmony/native}"
TARGET="${OHOS_TARGET:-aarch64-unknown-linux-ohos}"
CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-/tmp/ohos-core-smoke/target}"
OUT="$HERE/entry/libs/${TARGET%%-*}-v8a"   # aarch64-unknown-linux-ohos → aarch64-v8a
# 上面这行只是为了可读；OHOS 的 ABI 目录名固定是 arm64-v8a。
OUT="$HERE/entry/libs/arm64-v8a"
mkdir -p "$OUT"

CC_PREFIX="$NDK/llvm/bin/aarch64-unknown-linux-ohos"
[[ -x "$CC_PREFIX-clang" ]] || { echo "NDK clang not found: $CC_PREFIX-clang" >&2; exit 1; }

# ── 1. Rust cdylib ───────────────────────────────────────────────────────────
if [[ -z "${SKIP_RUST:-}" ]]; then
  echo "== [1/2] cargo build --release --target $TARGET (libdiary_core_smoke.so)"
  # 交叉编译只需要三样东西：linker + C 编译器（rusqlite bundled 的 SQLite C 代码）+ ar。
  export CARGO_TARGET_DIR
  export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_OHOS_LINKER="$CC_PREFIX-clang"
  export CC_aarch64_unknown_linux_ohos="$CC_PREFIX-clang"
  export AR_aarch64_unknown_linux_ohos="$NDK/llvm/bin/llvm-ar"
  ( cd "$HERE/rust" && cargo build --target "$TARGET" --release --offline )
  cp "$CARGO_TARGET_DIR/$TARGET/release/libdiary_core_smoke.so" "$OUT/"
fi

# ── 2. C++ NAPI shim（+ ORT C API 探针） ────────────────────────────────────
if [[ -z "${SKIP_SHIM:-}" ]]; then
  echo "== [2/3] clang++ -shared (libdiary_napi.so)"
  "$CC_PREFIX-clang++" \
    -shared -fPIC -O2 -std=c++17 \
    -Wall -Wextra \
    -Wl,-soname,libdiary_napi.so \
    -o "$OUT/libdiary_napi.so" \
    "$HERE/entry/src/main/cpp/napi_shim.cpp" \
    "$HERE/entry/src/main/cpp/ort_probe.cpp" \
    -I"$HERE/entry/src/main/cpp" \
    -lace_napi.z
  # 说明：NAPI 头/库来自 NDK sysroot（libace_napi.z.so）。
  #
  # ★ 必须把 libc++_shared.so 一起打进 HAP：设备上 /system/lib64/libc++_shared.so 确实存在，
  #   但应用进程的 linker namespace 看不到它（实测原始报错见 README「坑 2」）：
  #     MUSL-LDSO: load libc++_shared.so failed, namespace=ndk no inherits, errno=2
  #     MUSL-LDSO: load libc++_shared.so failed, namespace=moduleNs_default, errno=2
  #     MUSL-LDSO: Error loading shared library libc++_shared.so:
  #                (needed by /data/storage/el1/bundle/libs/arm64/libdiary_napi.so)
  #   DevEco 的 C++ 模板也是这么做的（产物 HAP 里带一份 libc++_shared.so）。
  #   ORT 自己 NEEDED libc++_shared.so，同理必须随包带。
  cp "$NDK/llvm/lib/aarch64-linux-ohos/libc++_shared.so" "$OUT/libc++_shared.so"
fi

# ── 2b. ONNX Runtime（沙箱 dlopen 目标；file:/// 语义：HAP 里带一份） ────────
# 默认走 **自编 1.29.1**（`tools/ohos/build-ort.sh`，缓存优先 + 自检）：这也是产品
# `ort` crate 要求的 ≥API 17 那条路。
#
# `ORT_FLAVOR=sherpa-1.16.3` 保留的是历史快路：sherpa-onnx 官方 OHOS 预编译包里的
# 1.16.3（API 16）。它只能证明「沙箱能 dlopen ORT」，喂不了产品的 `ort` crate
# （§23.3 的版本断言），仅用于复现旧证据。缓存目录也从 /tmp 挪到了 $HOME。
if [[ -z "${SKIP_ORT:-}" ]]; then
  case "${ORT_FLAVOR:-self-built}" in
    sherpa-1.16.3)
      ORT_SRC="${ORT_SO_DIR:-$HOME/dev/caches/sherpa-ort-1.16.3}"
      if [[ ! -f "$ORT_SRC/libonnxruntime.so" ]]; then
        mkdir -p "$ORT_SRC"
        tb="$ORT_SRC/sherpa-onnx-v1.13.8-ohos-arm64-v8a.tar.bz2"
        [[ -f "$tb" ]] || curl -sSL -o "$tb" \
          "https://github.com/k2-fsa/sherpa-onnx/releases/download/v1.13.8/sherpa-onnx-v1.13.8-ohos-arm64-v8a.tar.bz2"
        tar xjf "$tb" -C "$ORT_SRC" --strip-components=2 \
          sherpa-onnx-v1.13.8-ohos-arm64-v8a/lib/libonnxruntime.so
      fi
      cp "$ORT_SRC/libonnxruntime.so" "$OUT/libonnxruntime.so"
      ;;
    self-built)
      ORT_CACHE_DIR="${ORT_SO_DIR:-${ORT_CACHE_DIR:-$HOME/dev/caches/ort-1291}}" \
        "$REPO_ROOT/tools/ohos/build-ort.sh" --dest "$OUT"
      ;;
    *)
      echo "ORT_FLAVOR 只支持 self-built / sherpa-1.16.3，收到：$ORT_FLAVOR" >&2
      exit 2
      ;;
  esac
fi

# ── 3. 自检：ELF 类型、NEEDED、导出符号 ──────────────────────────────────────
echo "== 产物自检"
for so in "$OUT"/libdiary_core_smoke.so "$OUT"/libdiary_napi.so "$OUT"/libonnxruntime.so; do
  [[ -f "$so" ]] || continue
  printf '\n-- %s  bytes=%s\n' "$(basename "$so")" "$(stat -c%s "$so")"
  file -b "$so"
  "$NDK/llvm/bin/llvm-readelf" -d "$so" | grep -E "NEEDED|SONAME" || true
done
echo
echo "-- libdiary_core_smoke.so 导出（dlopen 要用的三个符号）"
"$NDK/llvm/bin/llvm-readelf" --dyn-syms "$OUT/libdiary_core_smoke.so" | grep -E "diary_smoke" || true
echo "-- libdiary_napi.so 导出（NAPI 模块注册）"
"$NDK/llvm/bin/llvm-readelf" --dyn-syms "$OUT/libdiary_napi.so" | grep -iE "napi_module_register" || true
echo "-- libonnxruntime.so 版本戳（OrtGetApiBase 的符号版本 = ORT 版本）"
"$NDK/llvm/bin/llvm-readelf" --dyn-syms "$OUT/libonnxruntime.so" | grep -E "OrtGetApiBase" || true
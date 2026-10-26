#!/usr/bin/env bash
# 编出打进 HAP 的两个原生库（**纯命令行、不需要 DevEco GUI、不需要 CMake**）：
#
#   1. libdiary_capi.so  ← Rust cdylib（`crates/diary_capi`：句柄 + JSON 进 / JSON 出）
#   2. libdiary_napi.so  ← C++ NAPI shim（dlopen 上面那个，暴露成 ArkTS 能 import 的模块）
#   + libc++_shared.so   ← **必须随包带**（探针 §24.1：应用进程的 linker namespace 看不到
#                          系统的 /system/lib64/libc++_shared.so）
#
# 三个都落到 `entry/libs/arm64-v8a/`，hvigor 的 ProcessLibs 任务会自动把它们收进 HAP 的
# `libs/arm64-v8a/`（不需要在 hvigor 里配 externalNativeOptions / CMakeLists，探针 §24.3）。
#
# 环境变量：
#   NDK             默认 $HOME/command-line-tools/sdk/default/openharmony/native
#   OHOS_TARGET     默认 aarch64-unknown-linux-ohos
#   CARGO_TARGET_DIR 默认 $HOME/.cache/diary-ohos/target（**不落进仓库、不用 /tmp**）
#   CARGO_BUILD_JOBS 默认 4（这台机器内存小，别开满）
#   SKIP_RUST=1 / SKIP_SHIM=1   只重建其中一个
#
# 用法：
#   apps/ohos-diary-core/scripts/build-native.sh
#   SKIP_RUST=1 apps/ohos-diary-core/scripts/build-native.sh   # 只重编 shim（~2 s）
set -euo pipefail

HERE="$(cd "$(dirname "$0")/.." && pwd)"          # apps/ohos-diary-core
REPO="$(cd "$HERE/../.." && pwd)"                 # 仓库根
NDK="${NDK:-$HOME/command-line-tools/sdk/default/openharmony/native}"
TARGET="${OHOS_TARGET:-aarch64-unknown-linux-ohos}"
CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$HOME/.cache/diary-ohos/target}"
CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-4}"
CARGO_OFFLINE="${CARGO_OFFLINE:---offline}"
OUT="$HERE/entry/libs/arm64-v8a"
mkdir -p "$OUT"

CC_PREFIX="$NDK/llvm/bin/aarch64-unknown-linux-ohos"
[[ -x "$CC_PREFIX-clang" ]] || { echo "NDK clang not found: $CC_PREFIX-clang" >&2; exit 1; }
[[ -x "$CC_PREFIX-clang++" ]] || { echo "NDK clang++ not found: $CC_PREFIX-clang++" >&2; exit 1; }

# ── 1. Rust cdylib（crates/diary_capi） ──────────────────────────────────────
if [[ -z "${SKIP_RUST:-}" ]]; then
  echo "== [1/3] cargo build -p diary_capi --release --target $TARGET"
  # 交叉编译只需要三样东西：linker + C 编译器（rusqlite bundled 的 SQLite C 代码）+ ar。
  export CARGO_TARGET_DIR CARGO_BUILD_JOBS
  export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_OHOS_LINKER="$CC_PREFIX-clang"
  export CC_aarch64_unknown_linux_ohos="$CC_PREFIX-clang"
  export AR_aarch64_unknown_linux_ohos="$NDK/llvm/bin/llvm-ar"
  ( cd "$REPO" && cargo build -p diary_capi --target "$TARGET" --release --features offline-only $CARGO_OFFLINE )
  cp "$CARGO_TARGET_DIR/$TARGET/release/libdiary_capi.so" "$OUT/libdiary_capi.so"
fi

# ── 2. C++ NAPI shim ────────────────────────────────────────────────────────
if [[ -z "${SKIP_SHIM:-}" ]]; then
  echo "== [2/3] clang++ -shared (libdiary_napi.so)"
  "$CC_PREFIX-clang++" \
    -shared -fPIC -O2 -std=c++17 \
    -Wall -Wextra -Werror \
    -Wl,--no-undefined \
    -Wl,-soname,libdiary_napi.so \
    -o "$OUT/libdiary_napi.so" \
    "$HERE/entry/src/main/cpp/napi_shim.cpp" \
    -I"$HERE/entry/src/main/cpp" \
    -lace_napi.z
  # 说明：
  #  * NAPI 头/库来自 NDK sysroot（libace_napi.z.so）；
  #  * `-Wl,--no-undefined` 是有意的：探针 §24.2 的坑是「符号没定义，链接时不报，
  #    设备上 dlopen 才炸」。本 shim 不直接链接 libdiary_capi.so（运行时 dlopen），
  #    所以这里的 UND 只应剩 libc / libc++ / libace_napi 的符号；多出来就是 bug。
  #
  # ★ libc++_shared.so 必须随包（探针 §24.1）。设备上 /system/lib64/libc++_shared.so
  #   确实存在，但应用进程的 linker namespace 看不到它：
  #     MUSL-LDSO: load libc++_shared.so failed, namespace=ndk no inherits, errno=2
  #     MUSL-LDSO: Error loading shared library libc++_shared.so:
  #                (needed by /data/storage/el1/bundle/libs/arm64/libdiary_napi.so)
  cp "$NDK/llvm/lib/aarch64-linux-ohos/libc++_shared.so" "$OUT/libc++_shared.so"
fi

# ── 3. 自检：ELF 类型、NEEDED、导出/未定义符号 ──────────────────────────────
if [[ "${SKIP_LLM:-0}" != 1 ]]; then
  bash "$HERE/scripts/build-llm.sh"
fi

echo "== [3/3] 产物自检"
for so in "$OUT/libdiary_capi.so" "$OUT/libdiary_napi.so" "$OUT/libc++_shared.so"; do
  [[ -f "$so" ]] || { echo "缺少产物：$so" >&2; exit 1; }
  printf '\n-- %s  bytes=%s\n' "$(basename "$so")" "$(stat -c%s "$so")"
  file -b "$so"
  "$NDK/llvm/bin/llvm-readelf" -d "$so" | grep -E "NEEDED|SONAME" || true
done

echo
echo "-- libdiary_capi.so 导出（NAPI shim 要 dlsym 的 6 个）"
for sym in diary_capi_version diary_open diary_close diary_call diary_string_free diary_last_error; do
  if "$NDK/llvm/bin/llvm-readelf" --dyn-syms "$OUT/libdiary_capi.so" | grep -qE "[[:space:]]${sym}$"; then
    echo "   OK  $sym"
  else
    echo "   MISSING  $sym" >&2
    exit 1
  fi
done

echo "-- libdiary_napi.so 导出（NAPI 模块注册）"
"$NDK/llvm/bin/llvm-readelf" --dyn-syms "$OUT/libdiary_napi.so" | grep -E "diary_napi|RegisterDiary" || true

echo "-- libdiary_napi.so 里不该出现的未定义符号（只允许 libc/libc++/libace_napi 提供）"
"$NDK/llvm/bin/llvm-readelf" --dyn-syms "$OUT/libdiary_napi.so" \
  | awk '$7 == "UND" && $8 != "" {print $8}' | grep -vE "^(__|_Z|_Unwind|_Zn|_Zd|dl|open|close|read|write|fopen|fclose|fread|fwrite|fseek|ftell|stat|chmod|access|getenv|malloc|free|mem|str|snprintf|abort|pthread|std|napi_|v8|uv)" \
  | sed 's/^/   UNEXPECTED /' || true

echo
echo "OK: $OUT/{libdiary_capi.so,libdiary_napi.so,libc++_shared.so}"

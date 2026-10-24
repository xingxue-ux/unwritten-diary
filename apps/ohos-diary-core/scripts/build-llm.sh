#!/usr/bin/env bash
# Minimal in-process GGUF runtime. Run from Linux/WSL with the OHOS NDK.
set -euo pipefail
HERE="$(cd "$(dirname "$0")/.." && pwd)"
NDK="${NDK:-$HOME/command-line-tools/sdk/default/openharmony/native}"
LLAMA_REV=71255fefc092b0344f2acddb891388814ae961be
SOURCE="${LLAMA_SOURCE_DIR:-$HOME/.cache/diary-llm/llama.cpp}"
BUILD="${LLAMA_BUILD_DIR:-$SOURCE/build-ohos}"
OUT="$HERE/entry/libs/arm64-v8a"
if [[ ! -d "$SOURCE" ]]; then
  mkdir -p "$SOURCE"
  git -C "$SOURCE" init
  git -C "$SOURCE" remote add origin https://github.com/ggml-org/llama.cpp.git
  git -C "$SOURCE" fetch --depth 1 origin "$LLAMA_REV"
  git -C "$SOURCE" checkout --detach FETCH_HEAD
fi
[[ "$(git -C "$SOURCE" rev-parse HEAD)" == "$LLAMA_REV" ]] || {
  echo "llama.cpp revision mismatch; use the pinned revision in a separate checkout" >&2; exit 1;
}
# OHOS musl has sched_setaffinity, not pthread_setaffinity_np. No other upstream
# changes are required. This patch is idempotent and bound to the revision above.
python3 - "$SOURCE" <<'PY'
from pathlib import Path
import sys
p = Path(sys.argv[1]) / 'ggml/src/ggml-cpu/ggml-cpu.c'
a = '#ifdef __ANDROID__\n    err = sched_setaffinity'
b = '#if defined(__ANDROID__) || defined(__OHOS__)\n    err = sched_setaffinity'
s = p.read_text()
if a in s:
    assert s.count(a) == 1
    p.write_text(s.replace(a, b))
else:
    assert s.count(b) == 1, 'unexpected upstream affinity implementation'
PY
build_variant() {
local build_dir="$1" library_file="$2" arm_arch="$3"
cmake -S "$SOURCE" -B "$build_dir" -G Ninja \
  -DCMAKE_SYSTEM_NAME=Linux -DCMAKE_SYSTEM_PROCESSOR=aarch64 \
  -DCMAKE_C_COMPILER="$NDK/llvm/bin/aarch64-unknown-linux-ohos-clang" \
  -DCMAKE_CXX_COMPILER="$NDK/llvm/bin/aarch64-unknown-linux-ohos-clang++" \
  -DCMAKE_AR="$NDK/llvm/bin/llvm-ar" -DCMAKE_RANLIB="$NDK/llvm/bin/llvm-ranlib" \
  -DCMAKE_BUILD_TYPE=Release '-DCMAKE_C_FLAGS_RELEASE=-O3 -DNDEBUG' \
  '-DCMAKE_CXX_FLAGS_RELEASE=-O3 -DNDEBUG' \
  -DCMAKE_BUILD_WITH_INSTALL_RPATH=ON -DCMAKE_POSITION_INDEPENDENT_CODE=ON \
  -DBUILD_SHARED_LIBS=OFF -DGGML_NATIVE=OFF -DGGML_OPENMP=OFF -DGGML_CPU_ARM_ARCH="$arm_arch" \
  -DLLAMA_BUILD_TESTS=OFF -DLLAMA_BUILD_COMMON=OFF \
  -DLLAMA_BUILD_TOOLS=OFF -DLLAMA_BUILD_EXAMPLES=OFF -DLLAMA_BUILD_SERVER=OFF
cmake --build "$build_dir" --target llama -j "${LLAMA_BUILD_JOBS:-4}"
mkdir -p "$OUT"
"$NDK/llvm/bin/aarch64-unknown-linux-ohos-clang++" \
  -std=c++17 -O3 -DNDEBUG -fPIC -shared -Wall -Wextra -Werror \
  -Wl,--no-undefined -Wl,-soname,"$library_file" \
  -I"$SOURCE/include" -I"$SOURCE/ggml/include" \
  "$HERE/entry/src/main/cpp/diary_llm.cpp" \
  -Wl,--start-group "$build_dir/src/libllama.a" "$build_dir/ggml/src/libggml.a" \
  "$build_dir/ggml/src/libggml-cpu.a" "$build_dir/ggml/src/libggml-base.a" \
  -Wl,--end-group -ldl -lpthread -lhilog_ndk.z -o "$OUT/$library_file"
"$NDK/llvm/bin/llvm-strip" --strip-unneeded "$OUT/$library_file"
"$NDK/llvm/bin/llvm-readelf" -d "$OUT/$library_file" | grep NEEDED
echo "OK: $OUT/$library_file (llama.cpp $LLAMA_REV)"
}
build_variant "$BUILD" libdiary_llm.so ""
build_variant "${BUILD}-dotprod" libdiary_llm_dotprod.so armv8.2-a+dotprod+fp16

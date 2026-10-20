#!/usr/bin/env bash
# Build and run the NAPI host harness (C++ exception paths + handle table).
#
# Needs only g++ and libdl — no OHOS SDK, no device. Output goes to the ignored
# `build/napi-host/` directory so the source tree stays clean.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/../../../.." && pwd)"
OUT="$ROOT/build/napi-host"
CXX="${CXX:-g++}"

mkdir -p "$OUT"

"$CXX" -std=c++17 -fPIC -shared "$HERE/fake_capi.cpp" -o "$OUT/libfake_capi.so"
"$CXX" -std=c++17 -I"$HERE" -o "$OUT/harness" "$HERE/harness.cpp" "$HERE/stub.cpp" -ldl

# LoadCore() honors DIARY_CAPI_LIB before looking for a sibling .so.
DIARY_CAPI_LIB="$OUT/libfake_capi.so" "$OUT/harness"

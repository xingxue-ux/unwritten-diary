#!/usr/bin/env bash
# 自编 ONNX Runtime（默认 1.29.1）到 OHOS / arm64-v8a —— **一条命令、缓存优先、幂等、自检**。
#
# 为什么存在这个脚本：生态里鸿蒙预编译的 ORT 只有 1.16.3，而产品用的 `ort`
# crate 要求 C API ≥ 17（`OrtApi` 版本门槛），所以必须自编。第一次编要 ~33 min
# （16 核），之后只应该命中缓存、秒级返回。
#
# 行为：
#   1. 默认**先查缓存** `$HOME/dev/caches/ort-1291/libonnxruntime.so`：
#      manifest 里的 sha256 一致 + ELF 自检（NEEDED / 导出符号 / 版本串）通过
#      ⇒ 直接复用，不编译、不联网；
#   2. 缓存未命中才走完整构建（参数化 ORT 版本、NDK、并行度），且**幂等**：
#      源码包、12 个 FetchContent 依赖、宿主 protoc 都复用，补丁重复应用会被
#      识别成「已打过」而不是报错；
#   3. 产物落到缓存目录（**不提交进仓库**，19.6 MB 的二进制不该进 git），
#      同时写 manifest；`--dest` 可再复制一份给 HAP 打包用。
#
# 用法：
#   tools/ohos/build-ort.sh                    # 缓存优先；未命中就完整构建
#   tools/ohos/build-ort.sh --dest <dir>       # 构建/复用后复制到 <dir>/libonnxruntime.so
#   tools/ohos/build-ort.sh --force-build      # 忽略缓存，强制重建（~33 min/16 核）
#   tools/ohos/build-ort.sh --check-only       # 只做构建前置检查（不下载、不编译）
#   tools/ohos/build-ort.sh --check-cache      # 只验缓存（不下载、不编译；坏缓存非 0 退出）
#   tools/ohos/build-ort.sh --prepare-only     # 取源码+打补丁+拉依赖+编宿主 protoc 后停
#   tools/ohos/build-ort.sh --configure-only   # 再往前走到 CMake configure 后停
#
# 环境变量：
#   ORT_VERSION        默认 1.29.1（内置 URL/sha256；换版本要自己给下面两项）
#   ORT_SOURCE_URL     源码包 URL（默认 GitHub tag v1.29.1 归档）
#   ORT_SOURCE_SHA256  源码包 sha256（默认内置 1.29.1 的实测值）
#   ORT_SOURCE_DIR     复用已有源码树（离线/调试；给了就不下载解压）
#   ORT_CACHE_DIR      产物缓存目录，默认 $HOME/dev/caches/ort-<版本去点>
#   ORT_BUILD_DIR      构建树目录，默认 $HOME/dev/build/ort-<版本>
#   NDK                OHOS NDK native 目录，默认 ~/command-line-tools/sdk/default/openharmony/native
#   JOBS               并行度，默认 nproc
#   ORT_FETCH_DEPS     1=预拉 12 个 FetchContent 依赖到源码树 mirror（默认），0=只靠 FetchContent
#   ORT_GITHUB_PROXY   GitHub 代理前缀，默认 https://gh-proxy.com/
#   HOST_CC/HOST_CXX   编宿主 protoc 用的编译器（默认 cc/c++，没有就用 NDK 里的宿主 clang）
#   ORT_PROTOC         直接指定宿主 protoc（跳过编译）
#   ORT_FORCE_BUILD    1=忽略缓存强制重建（等价 --force-build；本地/脚本调用用）
#
# 退出码：0 成功；2 前置检查/参数错误；3 下载失败；
#         4 configure/build/自检失败（--check-cache 时也表示缓存不存在/不可用）。
#
# ★ 路径纪律：**不用 /tmp**（本机 /tmp 会被清空）。缓存、源码、构建树都在 $HOME。
set -euo pipefail

SCRIPT_PATH="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/$(basename "${BASH_SOURCE[0]}")"
SCRIPT_DIR="$(dirname "$SCRIPT_PATH")"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

ORT_VERSION="${ORT_VERSION:-1.29.1}"
CACHE_DIR="${ORT_CACHE_DIR:-$HOME/dev/caches/ort-${ORT_VERSION//./}}"
BUILD_ROOT="${ORT_BUILD_DIR:-$HOME/dev/build/ort-$ORT_VERSION}"
NDK="${NDK:-$HOME/command-line-tools/sdk/default/openharmony/native}"
JOBS="${JOBS:-$(nproc 2>/dev/null || echo 4)}"
FETCH_DEPS="${ORT_FETCH_DEPS:-1}"
GITHUB_PROXY="${ORT_GITHUB_PROXY:-https://gh-proxy.com/}"

DEST=""
# 环境变量与命令行开关等价（本地/脚本调用方便）；CI 的 force_build 输入传的是 --force-build。
case "${ORT_FORCE_BUILD:-0}" in
  1|true|yes|on) FORCE_BUILD=1 ;;
  *)             FORCE_BUILD=0 ;;
esac
MODE="full"   # full | check | cache | prepare | configure

SO="$CACHE_DIR/libonnxruntime.so"
MANIFEST="$CACHE_DIR/manifest.json"
LLVM_BIN="$NDK/llvm/bin"
CMAKE_BIN=""
CMAKE_VER=""
READELF=""
NM=""
STRIP=""
REAL_PATCH=""
# 缓存文件存在但不可用（版本/sha256/ELF 自检失败）时记下原因：preflight 里要把
# 「缓存损坏」和「没有 NDK」区分开，否则 CI 上会误导人去装 NDK（评审 #126 建议项 4）。
CACHE_UNUSABLE=0
CACHE_UNUSABLE_REASON=""

# 1.29.1 的源码包（与 conandata 里的 sha256 一致：实测 2026-10-16 用同一份源码编出过成品）。
SOURCE_URL_1291="https://github.com/microsoft/onnxruntime/archive/refs/tags/v1.29.1.tar.gz"
SOURCE_URL_1291_FALLBACK="https://codeload.github.com/microsoft/onnxruntime/tar.gz/refs/tags/v1.29.1"
SOURCE_SHA_1291="aba6ff915ffa3689af9bd6a84102a221ad35427e602f22bb18ae8323997d0f0d"

# FetchContent 里真正会被用到的 12 个依赖（与已验证配方一致；其余依赖不参与本构建）。
REQUIRED_DEPS="abseil_cpp protobuf onnx re2 safeint microsoft_gsl date eigen pytorch_cpuinfo mp11 json flatbuffers"

say()  { printf '== %s\n' "$*"; }
note() { printf '   %s\n' "$*"; }
warn() { printf '!! %s\n' "$*" >&2; }
die()  { local code=$1; shift; printf '!! %s\n' "$*" >&2; exit "$code"; }

usage() {
  cat <<'EOF'
自编 ONNX Runtime（默认 1.29.1）到 OHOS / arm64-v8a —— 缓存优先、幂等、自检。

用法：
  tools/ohos/build-ort.sh                    # 缓存优先；未命中就完整构建
  tools/ohos/build-ort.sh --dest <dir>       # 构建/复用后复制到 <dir>/libonnxruntime.so
  tools/ohos/build-ort.sh --force-build      # 忽略缓存，强制重建（~33 min/16 核）
  tools/ohos/build-ort.sh --check-only       # 只做构建前置检查（不下载、不编译）
  tools/ohos/build-ort.sh --check-cache      # 只验缓存（坏缓存非 0 退出，不编译）
  tools/ohos/build-ort.sh --prepare-only     # 源码+补丁+依赖+宿主 protoc 后停
  tools/ohos/build-ort.sh --configure-only   # 再走到 CMake configure 后停

常用环境变量：ORT_VERSION / ORT_CACHE_DIR / ORT_BUILD_DIR / NDK / JOBS /
              ORT_SOURCE_URL / ORT_SOURCE_SHA256 / ORT_SOURCE_DIR /
              ORT_FETCH_DEPS / ORT_GITHUB_PROXY / HOST_CC / HOST_CXX / ORT_PROTOC /
              ORT_FORCE_BUILD
详见脚本文档头与 docs/architecture/m6-ORT-构建与缓存.md。
EOF
}

while (($#)); do
  case "$1" in
    --dest)          DEST="${2:?--dest 需要一个目录}"; shift 2 ;;
    --force-build)   FORCE_BUILD=1; shift ;;
    --check-only)    MODE=check; shift ;;
    --check-cache)   MODE=cache; shift ;;
    --prepare-only)  MODE=prepare; shift ;;
    --configure-only) MODE=configure; shift ;;
    -h|--help)       usage; exit 0 ;;
    *)               usage >&2; die 2 "未知参数：$1" ;;
  esac
done

# ── 工具发现（自检优先用 NDK 的 llvm-*，没有就退回宿主 binutils） ───────────────
detect_tools() {
  if [[ -x "$LLVM_BIN/llvm-readelf" ]]; then READELF="$LLVM_BIN/llvm-readelf"; else READELF="$(command -v readelf || true)"; fi
  if [[ -x "$LLVM_BIN/llvm-nm" ]]; then NM="$LLVM_BIN/llvm-nm"; else NM="$(command -v nm || true)"; fi
  if [[ -x "$LLVM_BIN/llvm-strip" ]]; then STRIP="$LLVM_BIN/llvm-strip"; else STRIP="$(command -v strip || true)"; fi
  [[ -n "$READELF" && -n "$NM" ]] || die 2 "缺少 readelf/nm（装 binutils，或给一个带 llvm-readelf 的 NDK）"
  command -v sha256sum >/dev/null 2>&1 || command -v shasum >/dev/null 2>&1 \
    || die 2 "缺少 sha256sum/shasum"
  command -v python3 >/dev/null 2>&1 || die 2 "缺少 python3（源码编辑与 manifest 都要用）"
}

sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | awk '{print $1}';
  else shasum -a 256 "$1" | awk '{print $1}'; fi
}
sha1_of() {
  if command -v sha1sum >/dev/null 2>&1; then sha1sum "$1" | awk '{print $1}';
  else shasum -a 1 "$1" | awk '{print $1}'; fi
}

# ── 自检：ELF 架构 + NEEDED + SONAME + OrtGetApiBase 版本戳 ────────────────────
# 说明（与任务描述的一处事实差异）：产物 NEEDED 实际是 **libc++_shared.so + libc.so**
# 两条，不是只有 libc.so —— ORT 是 C++ 写的，必须随 HAP 带一份 libc++_shared.so
# （探针 §24.1 已实测：应用进程的 linker namespace 看不到系统那份）。
# 所以自检用的是**白名单**：只允许这两条，且 libc.so 必须在。
needed_list() { "$READELF" -d "$1" 2>/dev/null | sed -n 's/.*(NEEDED).*\[\(.*\)\]/\1/p'; }
soname_of()   { "$READELF" -d "$1" 2>/dev/null | sed -n 's/.*(SONAME).*\[\(.*\)\]/\1/p' | head -1; }

selfcheck_artifact() {
  local so=$1 quiet=${2:-}
  local fail=0 desc="" size=0 syms="" soname="" n
  local -a needed=() unexpected=()

  [[ -f "$so" ]] || { warn "自检：文件不存在 $so"; return 1; }
  size=$(stat -c%s "$so")
  if (( size < 1000000 )); then warn "自检：体积只有 $size B，不像 ORT（期望 ~19.6 MB）"; fail=1; fi

  if command -v file >/dev/null 2>&1; then
    desc=$(file -b "$so")
    case "$desc" in
      *aarch64*|*AArch64*) ;;
      *) warn "自检：不是 aarch64 ELF：$desc"; fail=1 ;;
    esac
  fi

  mapfile -t needed < <(needed_list "$so")
  ((${#needed[@]} > 0)) || { warn "自检：读不到 NEEDED"; fail=1; }
  for n in "${needed[@]}"; do
    case "$n" in
      libc.so|libc++_shared.so) ;;
      *) unexpected+=("$n") ;;
    esac
  done
  ((${#unexpected[@]} == 0)) || { warn "自检：出现白名单外的 NEEDED：${unexpected[*]}"; fail=1; }
  printf '%s\n' "${needed[@]}" | grep -qx 'libc.so' || { warn "自检：NEEDED 里没有 libc.so"; fail=1; }
  soname=$(soname_of "$so")
  # 文档 §3.1 把 SONAME 列为命中条件；这里必须**断言**，不能只打印（评审 #126 建议项 5）。
  [[ "$soname" == "libonnxruntime.so.1" ]] \
    || { warn "自检：SONAME 是 ${soname:-<无>}，期望 libonnxruntime.so.1"; fail=1; }

  syms=$("$NM" -D "$so" 2>/dev/null || "$READELF" --dyn-syms "$so")
  grep -q "OrtGetApiBase@@VERS_${ORT_VERSION}" <<<"$syms" \
    || { warn "自检：找不到 OrtGetApiBase@@VERS_${ORT_VERSION}"; fail=1; }

  if [[ -z "$quiet" ]]; then
    note "size_bytes  $size"
    note "sha256      $(sha256_of "$so")"
    [[ -n "$desc" ]] && note "file        $desc"
    note "NEEDED      ${needed[*]}   （白名单：libc.so libc++_shared.so）"
    note "SONAME      ${soname:-<无>}"
    grep -m1 "OrtGetApiBase@@VERS_" <<<"$syms" | sed 's/^/   symbol      /' || true
  fi
  return "$fail"
}

# ── manifest 与缓存判定 ────────────────────────────────────────────────────────
manifest_field() {
  sed -n "s/.*\"$1\"[[:space:]]*:[[:space:]]*\"\([^\"]*\)\".*/\1/p" "$MANIFEST" 2>/dev/null | head -1
}

write_manifest() {
  local so=$1 origin=$2 sha size soname needed_json="" n build_sha src_sha protoc_ver="unknown"
  if [[ -n "${PROTOC:-}" && -x "${PROTOC:-}" ]]; then
    protoc_ver="$("$PROTOC" --version 2>/dev/null | head -1 || true)"
  fi
  sha=$(sha256_of "$so")
  size=$(stat -c%s "$so")
  soname=$(soname_of "$so")
  while IFS= read -r n; do
    [[ -n "$n" ]] && needed_json+="\"$n\", "
  done < <(needed_list "$so")
  needed_json="[${needed_json%, }]"
  build_sha=$(cat "$SCRIPT_PATH" "$SCRIPT_DIR/ort_source_edits.py" | sha256sum | awk '{print $1}')
  src_sha="${ORT_SOURCE_SHA256_EFFECTIVE:-$SOURCE_SHA_1291}"
  mkdir -p "$CACHE_DIR"
  cat > "$MANIFEST" <<EOF
{
  "ort_version": "$ORT_VERSION",
  "sha256": "$sha",
  "size_bytes": $size,
  "soname": "${soname:-}",
  "needed": $needed_json,
  "origin": "$origin",
  "built_at": "$(date -Iseconds)",
  "build_script_sha256": "$build_sha",
  "source_url": "${ORT_SOURCE_URL_EFFECTIVE:-$SOURCE_URL_1291}",
  "source_sha256": "$src_sha",
  "ndk": "${NDK}",
  "host_protoc_version": "$protoc_ver"
}
EOF
  note "manifest    $MANIFEST"
}

try_cache() {
  [[ -f "$SO" ]] || { say "缓存未命中：$SO 不存在"; return 1; }
  if [[ -f "$MANIFEST" ]]; then
    local want_ver want_sha got
    want_ver=$(manifest_field ort_version)
    want_sha=$(manifest_field sha256)
    [[ "$want_ver" == "$ORT_VERSION" ]] \
      || {
        say "缓存 manifest 的版本是 ${want_ver:-?}，不是 $ORT_VERSION → 视为未命中"
        CACHE_UNUSABLE=1
        CACHE_UNUSABLE_REASON="manifest 的 ORT 版本是 ${want_ver:-?}，不是 $ORT_VERSION"
        return 1
      }
    got=$(sha256_of "$SO")
    [[ "$got" == "$want_sha" ]] \
      || {
        say "缓存 sha256 与 manifest 不一致（$got ≠ $want_sha）→ 视为未命中"
        CACHE_UNUSABLE=1
        CACHE_UNUSABLE_REASON="文件 sha256 与 manifest 不一致（缓存损坏）"
        return 1
      }
  else
    say "缓存没有 manifest（可能是手工放进去的成品）：先自检，通过就收编"
  fi
  selfcheck_artifact "$SO" quiet \
    || {
      say "缓存自检不通过（缓存损坏，不是「没有 NDK」）→ 视为未命中，需要重建"
      CACHE_UNUSABLE=1
      CACHE_UNUSABLE_REASON="ELF 自检失败（缓存损坏）"
      return 1
    }
  [[ -f "$MANIFEST" ]] || write_manifest "$SO" "adopted-existing"
  return 0
}

install_dest() {
  [[ -n "$DEST" ]] || return 0
  mkdir -p "$DEST"
  install -m 0755 "$SO" "$DEST/libonnxruntime.so"
  say "已复制：$DEST/libonnxruntime.so"
}

# ── 构建前置检查（缺什么就明确说怎么补） ──────────────────────────────────────
pick_cmake() {
  local c v major rest minor
  local -a candidates=()
  [[ -x "$NDK/build-tools/cmake/bin/cmake" ]] && candidates+=("$NDK/build-tools/cmake/bin/cmake")
  command -v cmake >/dev/null 2>&1 && candidates+=("$(command -v cmake)")
  for c in "${candidates[@]}"; do
    v=$("$c" --version 2>/dev/null | head -1 | awk '{print $3}')
    [[ -n "$v" ]] || continue
    major=${v%%.*}; rest=${v#*.}; minor=${rest%%.*}
    if [[ "$major" == "3" && "$minor" -ge 28 ]]; then
      CMAKE_BIN="$c"; CMAKE_VER="$v"; return 0
    fi
  done
  return 1
}

pick_host_compiler() {
  if [[ -n "${HOST_CC:-}" || -n "${HOST_CXX:-}" ]]; then
    [[ -n "${HOST_CC:-}" && -n "${HOST_CXX:-}" ]] \
      || die 2 "HOST_CC 与 HOST_CXX 必须成对给出"
    return 0
  fi
  if command -v cc >/dev/null 2>&1 && command -v c++ >/dev/null 2>&1; then
    HOST_CC="${HOST_CC:-$(command -v cc)}"; HOST_CXX="${HOST_CXX:-$(command -v c++)}"; return 0
  fi
  if [[ -x "$LLVM_BIN/clang" && -x "$LLVM_BIN/clang++" ]]; then
    HOST_CC="${HOST_CC:-$LLVM_BIN/clang}"; HOST_CXX="${HOST_CXX:-$LLVM_BIN/clang++}"; return 0
  fi
  return 1
}

preflight() {
  local missing=0

  if [[ ! -d "$NDK" ]]; then
    warn "找不到 OHOS NDK：$NDK"
    warn "  期望目录：$NDK/llvm/bin/aarch64-unknown-linux-ohos-clang"
    if (( CACHE_UNUSABLE == 1 )); then
      # CI 缓存命中但自检失败时，真正的问题是「缓存损坏」，不是「没有 NDK」。
      warn "  注意：缓存目录里的产物**存在但不可用**（$CACHE_UNUSABLE_REASON）。"
      warn "  这不是「第一次构建、缺 NDK」，而是缓存本身坏了：CI 上请 bump vars.ORT_CACHE_REV"
      warn "  （或 gh cache delete <key>）让缓存 key 变掉，不要先去装 NDK。"
      warn "  本地：删掉 $CACHE_DIR 后重跑，或用 --force-build 重建。"
    fi
    warn "  获取方式（任选其一）："
    warn "    1) DevEco Studio → SDK Manager → Native（默认落到 ~/command-line-tools/sdk/default/openharmony/native）"
    warn "    2) 下载 Command Line Tools（linux-x64）解压到 ~/command-line-tools（见文档「CI 里没有 NDK 怎么办」）"
    warn "    3) 显式指定：NDK=/path/to/native tools/ohos/build-ort.sh --force-build"
    warn "  只想用已有缓存（不需要 NDK）：去掉 --force-build 再跑一次"
    exit 2
  fi

  # 注意：工具链二进制必须用 **-x** 而不是 -e。zip 用 `python3 -m zipfile` 解压时
  # 不会恢复可执行位（也不会建符号链接），文件存在但没有 +x，preflight 放行后 CMake
  # 调 clang 才 Permission denied（评审 #126 B1）。在 preflight 就把话说清楚。
  local f
  for f in "llvm/bin/aarch64-unknown-linux-ohos-clang" "llvm/bin/aarch64-unknown-linux-ohos-clang++" \
           "llvm/bin/llvm-ar" "llvm/bin/llvm-ranlib" "llvm/bin/llvm-strip"; do
    if [[ ! -e "$NDK/$f" ]]; then
      warn "NDK 里缺少：$NDK/$f"
      missing=1
    elif [[ ! -x "$NDK/$f" ]]; then
      warn "NDK 里的文件存在但不可执行：$NDK/$f"
      warn "  可执行位丢了？zip 解压请用 unzip / bsdtar，不要用 python3 -m zipfile（它不恢复权限位与符号链接）"
      missing=1
    fi
  done
  [[ -e "$NDK/build/cmake/ohos.toolchain.cmake" ]] \
    || { warn "NDK 里缺少：$NDK/build/cmake/ohos.toolchain.cmake"; missing=1; }
  (( missing == 0 )) || die 2 "NDK 不完整（见上）—— 请重装 Command Line Tools 或换 NDK 路径"

  pick_cmake || die 2 "找不到可用的 CMake 3.28–3.x（不是 4.x：protobuf 21.12 的 cmake_minimum_required 是 3.5，CMake 4 会拒绝）。
   NDK 自带的 build-tools/cmake/bin/cmake 是 3.28.2，本脚本会优先用它"
  [[ "$CMAKE_VER" == 4.* ]] && die 2 "CMake $CMAKE_VER 太新（protobuf 21.12 不接受 4.x）"

  pick_host_compiler || die 2 "找不到宿主 C/C++ 编译器（编宿主 protoc 要用）：装 gcc/g++，或设置 HOST_CC/HOST_CXX"
  command -v make >/dev/null 2>&1 || die 2 "缺少 make（CMake 用 Unix Makefiles 生成构建文件）"
  command -v patch >/dev/null 2>&1 || die 2 "缺少 patch（ORT 的 FetchContent 依赖要打上游补丁）"
  command -v curl >/dev/null 2>&1 || die 2 "缺少 curl（下载源码包与依赖）"
  command -v tar >/dev/null 2>&1 || die 2 "缺少 tar"

  mkdir -p "$(dirname "$BUILD_ROOT")"
  local free_kb
  free_kb=$(df -Pk "$(dirname "$BUILD_ROOT")" | awk 'NR==2 {print $4}')
  if [[ -n "$free_kb" ]] && (( free_kb < 6291456 )); then
    die 2 "磁盘剩余 $((free_kb / 1024 / 1024)) GiB，不够（源码 577 MB + 依赖 + 构建树，建议 ≥ 6 GiB）：$BUILD_ROOT"
  fi

  say "前置检查通过"
  note "NDK        $NDK"
  note "CMake      $CMAKE_VER  ($CMAKE_BIN)"
  note "宿主编译器 $HOST_CC / $HOST_CXX"
  note "并行度     -j$JOBS"
  note "构建树     $BUILD_ROOT"
  note "缓存       $CACHE_DIR"
}

# ── 源码包 ────────────────────────────────────────────────────────────────────
setup_source_pins() {
  case "$ORT_VERSION" in
    1.29.1)
      SOURCE_URL_EFFECTIVE="${ORT_SOURCE_URL:-$SOURCE_URL_1291}"
      SOURCE_SHA_EFFECTIVE="${ORT_SOURCE_SHA256:-$SOURCE_SHA_1291}"
      ;;
    *)
      [[ -n "${ORT_SOURCE_URL:-}" && -n "${ORT_SOURCE_SHA256:-}" ]] \
        || die 2 "没有内置 ORT $ORT_VERSION 的源码 URL/sha256；请同时给 ORT_SOURCE_URL 与 ORT_SOURCE_SHA256"
      SOURCE_URL_EFFECTIVE="$ORT_SOURCE_URL"
      SOURCE_SHA_EFFECTIVE="$ORT_SOURCE_SHA256"
      ;;
  esac
  ORT_SOURCE_URL_EFFECTIVE="$SOURCE_URL_EFFECTIVE"
  ORT_SOURCE_SHA256_EFFECTIVE="$SOURCE_SHA_EFFECTIVE"
}

download() {
  local url=$1 out=$2
  say "下载 $url"
  curl -fL -sS --retry 3 --connect-timeout 20 -o "$out.part" "$url" \
    || { rm -f "$out.part"; return 1; }
  mv -f "$out.part" "$out"
}

prepare_source() {
  SRC_DIR="${ORT_SOURCE_DIR:-$BUILD_ROOT/src}"
  if [[ -n "${ORT_SOURCE_DIR:-}" ]]; then
    [[ -f "$SRC_DIR/cmake/CMakeLists.txt" ]] \
      || die 2 "ORT_SOURCE_DIR=$SRC_DIR 不像 ORT 源码根（缺 cmake/CMakeLists.txt）"
    say "复用外部源码树：$SRC_DIR"
    return 0
  fi

  mkdir -p "$BUILD_ROOT/downloads"
  local archive="$BUILD_ROOT/downloads/onnxruntime-v$ORT_VERSION.tar.gz"
  if [[ -f "$archive" ]] && [[ "$(sha256_of "$archive")" == "$SOURCE_SHA_EFFECTIVE" ]]; then
    say "源码包已存在且 sha256 一致，跳过下载：$archive"
  else
    rm -f "$archive"
    download "$SOURCE_URL_EFFECTIVE" "$archive" || {
      if [[ "$SOURCE_URL_EFFECTIVE" == "$SOURCE_URL_1291" ]]; then
        warn "主 URL 失败，改用 codeload 兜底"
        download "$SOURCE_URL_1291_FALLBACK" "$archive" || die 3 "源码包下载失败"
      else
        die 3 "源码包下载失败：$SOURCE_URL_EFFECTIVE"
      fi
    }
    [[ "$(sha256_of "$archive")" == "$SOURCE_SHA_EFFECTIVE" ]] \
      || die 3 "源码包 sha256 不匹配（期望 $SOURCE_SHA_EFFECTIVE）—— 拒绝使用"
  fi

  if [[ -f "$SRC_DIR/cmake/CMakeLists.txt" ]]; then
    say "源码树已解压，复用：$SRC_DIR"
    return 0
  fi
  say "解压源码到 $SRC_DIR"
  rm -rf "$SRC_DIR.part"
  mkdir -p "$SRC_DIR.part"
  tar -xzf "$archive" -C "$SRC_DIR.part" --strip-components=1
  rm -rf "$SRC_DIR"
  mv "$SRC_DIR.part" "$SRC_DIR"
}

apply_source_edits() {
  say "应用 OHOS 可移植性编辑（幂等，见 tools/ohos/ort_source_edits.py）"
  python3 "$SCRIPT_DIR/ort_source_edits.py" --root "$SRC_DIR" \
    || die 4 "源码编辑失败（源码版本不是 1.29.1？）"
}

# ── FetchContent 依赖预拉（幂等：sha1 一致就跳过） ──────────────────────────────
fetch_deps() {
  [[ "$FETCH_DEPS" == "1" ]] || { say "ORT_FETCH_DEPS=$FETCH_DEPS：跳过依赖预拉，交给 FetchContent"; return 0; }
  local deps_file="$SRC_DIR/cmake/deps.txt"
  [[ -f "$deps_file" ]] || die 4 "找不到 $deps_file"
  local name url sha1 dest got ok=0 fetched=0 reused=0
  local -a tried=()
  while IFS=';' read -r name url sha1 _rest; do
    [[ -z "$name" || "$name" == \#* ]] && continue
    case " $REQUIRED_DEPS " in *" $name "*) ;; *) continue ;; esac
    [[ "$url" == https://* ]] || continue
    dest="$SRC_DIR/mirror/${url#https://}"
    if [[ -f "$dest" ]] && [[ "$(sha1_of "$dest")" == "$sha1" ]]; then
      reused=$((reused + 1)); continue
    fi
    mkdir -p "$(dirname "$dest")"
    rm -f "$dest" "$dest.part"
    tried=("$url")
    if [[ "$url" == https://github.com/* ]]; then
      tried+=("${GITHUB_PROXY}${url}" "https://mirror.ghproxy.com/${url}")
    fi
    ok=0
    for u in "${tried[@]}"; do
      if download "$u" "$dest" 2>/dev/null; then ok=1; break; fi
    done
    (( ok == 1 )) || die 3 "依赖 $name 下载失败（$url）；可设 ORT_GITHUB_PROXY=<代理前缀> 或 ORT_FETCH_DEPS=0"
    got=$(sha1_of "$dest")
    [[ "$got" == "$sha1" ]] || { rm -f "$dest"; die 3 "依赖 $name 的 sha1 不匹配（期望 $sha1，实际 $got）"; }
    fetched=$((fetched + 1))
  done < "$deps_file"
  say "依赖镜像就绪：新下载 $fetched 个，复用 $reused 个（$SRC_DIR/mirror）"
}

# ── 宿主 protoc（交叉编译必须：否则会用刚编出来的 aarch64 protoc 跑在宿主上） ──
build_host_protoc() {
  PROTOC="${ORT_PROTOC:-$BUILD_ROOT/host-protoc-build/protoc}"
  if [[ -x "$PROTOC" ]] && "$PROTOC" --version >/dev/null 2>&1; then
    say "复用宿主 protoc：$PROTOC（$("$PROTOC" --version)）"
    return 0
  fi
  local zip="$SRC_DIR/mirror/github.com/protocolbuffers/protobuf/archive/refs/tags/v21.12.zip"
  [[ -f "$zip" ]] || die 3 "缺少 protobuf 21.12 源码包：$zip（先跑依赖预拉）"

  say "解压 protobuf 21.12 并编宿主 protoc（约 4 min/16 核）"
  rm -rf "$BUILD_ROOT/host-protoc-src" "$BUILD_ROOT/host-protoc-build"
  mkdir -p "$BUILD_ROOT/host-protoc-src"
  python3 - "$zip" "$BUILD_ROOT/host-protoc-src" <<'PY'
import sys, zipfile
with zipfile.ZipFile(sys.argv[1]) as zf:
    zf.extractall(sys.argv[2])
PY

  local src="$BUILD_ROOT/host-protoc-src/protobuf-21.12"
  [[ -d "$src" ]] || die 4 "protobuf 解压后目录不对：$src"
  "$CMAKE_BIN" -S "$src" -B "$BUILD_ROOT/host-protoc-build" \
    -DCMAKE_BUILD_TYPE=Release \
    -DCMAKE_C_COMPILER="$HOST_CC" -DCMAKE_CXX_COMPILER="$HOST_CXX" \
    -Dprotobuf_BUILD_TESTS=OFF -Dprotobuf_BUILD_SHARED_LIBS=OFF \
    > "$BUILD_ROOT/protoc-configure.log" 2>&1 \
    || { tail -30 "$BUILD_ROOT/protoc-configure.log" >&2; die 4 "宿主 protoc configure 失败"; }
  "$CMAKE_BIN" --build "$BUILD_ROOT/host-protoc-build" --target protoc -j "$JOBS" \
    > "$BUILD_ROOT/protoc-build.log" 2>&1 \
    || { tail -30 "$BUILD_ROOT/protoc-build.log" >&2; die 4 "宿主 protoc 编译失败"; }

  # 注意：protobuf 的 protoc 是符号链接（protoc -> protoc-3.21.12.0），不能用 -type f。
  PROTOC="$(find "$BUILD_ROOT/host-protoc-build" -maxdepth 2 -name protoc -perm -u+x -print -quit)"
  [[ -n "$PROTOC" ]] || die 4 "编完找不到 protoc"
  say "宿主 protoc：$PROTOC（$("$PROTOC" --version)）"
}

# ── 幂等补丁 shim：上游 FetchContent 的 patch step 重复进入同一构建目录会失败 ──
setup_patch_shim() {
  REAL_PATCH="$(command -v patch)"
  local hostbin="$BUILD_ROOT/hostbin"
  mkdir -p "$hostbin" "$BUILD_ROOT/tmp"
  {
    printf '#!/usr/bin/env bash\n'
    printf '# 由 tools/ohos/build-ort.sh 生成：已打过的补丁直接返回 0\n'
    printf 'set -uo pipefail\n'
    printf 'real=%q\n' "$REAL_PATCH"
    printf 'spool=%q\n' "$BUILD_ROOT/tmp/patch-stdin.$$"
    cat <<'SHIM'
trap 'rm -f "$spool"' EXIT
cat > "$spool"
if "$real" -t -R --dry-run "$@" < "$spool" >/dev/null 2>&1; then
  echo "[ort-patch-shim] 补丁已应用，跳过：$*" >&2
  exit 0
fi
exec "$real" "$@" < "$spool"
SHIM
  } > "$hostbin/patch"
  chmod +x "$hostbin/patch"

  local t
  for t in llvm-ar llvm-ranlib llvm-strip llvm-nm llvm-objcopy ld.lld; do
    [[ -x "$LLVM_BIN/$t" ]] && ln -sf "$LLVM_BIN/$t" "$hostbin/$t"
  done
  export PATH="$hostbin:$PATH"
  say "补丁 shim：$hostbin/patch（real patch = $REAL_PATCH）"
}

# ── configure / build ─────────────────────────────────────────────────────────
configure_ort() {
  local build="$BUILD_ROOT/build"
  local -a args=(
    -S "$SRC_DIR/cmake" -B "$build"
    -DCMAKE_TOOLCHAIN_FILE="$NDK/build/cmake/ohos.toolchain.cmake"
    -DOHOS_ARCH=arm64-v8a
    -DCMAKE_BUILD_TYPE=Release
    -DCMAKE_CXX_STANDARD=20
    -DCMAKE_AR="$LLVM_BIN/llvm-ar"
    -DCMAKE_RANLIB="$LLVM_BIN/llvm-ranlib"
    -Donnxruntime_BUILD_SHARED_LIB=ON
    -Donnxruntime_BUILD_UNIT_TESTS=OFF
    -Donnxruntime_ENABLE_PYTHON=OFF
    -Donnxruntime_MINIMAL_BUILD=OFF
    -Donnxruntime_DISABLE_EXCEPTIONS=OFF
    -Donnxruntime_DISABLE_RTTI=OFF
    -Donnxruntime_DISABLE_CONTRIB_OPS=OFF
    -Donnxruntime_USE_NNAPI_BUILTIN=OFF
    -Donnxruntime_BUILD_JAVA=OFF
    -Donnxruntime_BUILD_CSHARP=OFF
    -Donnxruntime_BUILD_OBJC=OFF
    -DFLATBUFFERS_BUILD_FLATC=OFF
    -DONNX_CUSTOM_PROTOC_EXECUTABLE="$PROTOC"
    -Donnxruntime_CMAKE_DEPS_MIRROR_DIR="$SRC_DIR/mirror"
  )
  mkdir -p "$build"
  say "CMake configure（OHOS toolchain，全算子集，非 MINIMAL_BUILD）"
  if ! "${CMAKE_BIN}" "${args[@]}" > "$BUILD_ROOT/configure.log" 2>&1; then
    tail -40 "$BUILD_ROOT/configure.log" >&2
    die 4 "configure 失败，完整日志：$BUILD_ROOT/configure.log"
  fi
  say "configure 完成"

  say "给 FetchContent 解出来的 cpuinfo 补 OHOS 平台分支"
  python3 "$SCRIPT_DIR/ort_source_edits.py" --patch-cpuinfo "$build" || true
}

build_ort() {
  local build="$BUILD_ROOT/build"
  say "开始编译（-j$JOBS，首次约 24 min/16 核；日志：$BUILD_ROOT/build.log）"
  if ! "${CMAKE_BIN}" --build "$build" -j "$JOBS" 2>&1 | tee "$BUILD_ROOT/build.log"; then
    die 4 "编译失败，完整日志：$BUILD_ROOT/build.log"
  fi
}

locate_and_install() {
  local build="$BUILD_ROOT/build" candidate tmp
  candidate="$(find "$build" -type f -name "libonnxruntime.so.${ORT_VERSION}" -print -quit)"
  if [[ -z "$candidate" ]]; then
    candidate="$(find "$build" -type f -name 'libonnxruntime.so.1*' ! -name '*.so' -print -quit)"
  fi
  [[ -n "$candidate" ]] || candidate="$(find "$build" -type f -name 'libonnxruntime.so' -print -quit)"
  [[ -n "$candidate" ]] || die 4 "编译完了但找不到 libonnxruntime.so*（看 $build）"
  say "产物：$candidate（$(stat -c%s "$candidate") B，未 strip）"

  mkdir -p "$CACHE_DIR"
  tmp="$CACHE_DIR/.libonnxruntime.so.tmp"
  cp -f "$candidate" "$tmp"
  [[ -n "$STRIP" ]] && "$STRIP" --strip-unneeded "$tmp" || true
  chmod 0755 "$tmp"
  mv -f "$tmp" "$SO"
  say "已 strip 并落到缓存：$SO（$(stat -c%s "$SO") B）"
  write_manifest "$SO" "built"
}

# ── 主流程 ───────────────────────────────────────────────────────────────────
detect_tools
setup_source_pins
t0=$SECONDS

if [[ "$MODE" == "check" ]]; then
  preflight
  elapsed=$(( SECONDS - t0 ))
  say "仅前置检查（--check-only），用时 ${elapsed}s"
  exit 0
fi

# --check-cache：只验缓存，**绝不**进 preflight / 构建。
# CI 的「缓存命中 → 自检」步骤必须用它：普通 full 模式在坏缓存 + 有 NDK 时会直接重建
# 并 exit 0（评审 #126 B2 指出的 no-op），坏缓存就永远触发不了自愈。
if [[ "$MODE" == "cache" ]]; then
  if try_cache; then
    say "缓存检查通过（--check-cache，未编译），用时 $(( SECONDS - t0 ))s"
    exit 0
  fi
  die 4 "缓存不可用（--check-cache）：${CACHE_UNUSABLE_REASON:-$SO 不存在或不可读}；未编译"
fi

# --prepare-only / --configure-only 是显式「走构建路径」的验证模式，不吃缓存。
if [[ "$MODE" == "full" ]] && (( FORCE_BUILD == 0 )) && try_cache; then
  install_dest
  say "自检（缓存命中，未编译）："
  selfcheck_artifact "$SO" || die 4 "缓存产物自检失败"
  elapsed=$(( SECONDS - t0 ))
  say "缓存命中，用时 ${elapsed}s"
  exit 0
fi

preflight

[[ "$MODE" == "full" ]] || say "模式：$MODE（不会走完整编译）"
prepare_source
apply_source_edits
fetch_deps
build_host_protoc

if [[ "$MODE" == "prepare" ]]; then
  elapsed=$(( SECONDS - t0 ))
  say "--prepare-only 完成（源码+补丁+依赖+宿主 protoc），用时 ${elapsed}s"
  exit 0
fi

setup_patch_shim
configure_ort

if [[ "$MODE" == "configure" ]]; then
  elapsed=$(( SECONDS - t0 ))
  say "--configure-only 完成（未编译），用时 ${elapsed}s"
  exit 0
fi

build_ort
locate_and_install
install_dest
say "自检（新构建的产物）："
selfcheck_artifact "$SO" || die 4 "新构建产物自检失败"
say "构建完成，总用时 $(( (SECONDS - t0) / 60 )) min"

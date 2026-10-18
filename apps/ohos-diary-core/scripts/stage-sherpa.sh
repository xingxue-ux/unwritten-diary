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
# ★ 为什么这个脚本现在会「拒绝覆盖」和「断言符号版本」：
#   PR #130 最有价值的发现是——OHOS 的 musl-ldso 按**符号版本哈希**解析，
#   sherpa v1.13.8 预编译库引用 `OrtGetApiBase@VERS_1.16.3`，直接把自编 ORT
#   1.29.1 改名成 `libonnxruntime.so` 会得到 `symbol not found: OrtGetApiBase`。
#   这条事实以前只写在 README 里：换一次库就会**静默复发**。现在：
#     - 来源两个 .so 的 sha256 钉死；
#     - 覆盖前先读目标 `libonnxruntime.so` 的符号版本，与来源不同就拒绝（除非 --force，
#       例如 #134 的双 ORT 方案要把自编 1.29.1 以**另一个文件名**并排放）；
#     - 复制后再用 llvm-readelf 断言 `NEEDED` 与 `OrtGetApiBase@VERS_1.16.3`。
#
# 用法：
#   SHERPA_LIB_DIR=$HOME/dev/caches/asr-ohos/sherpa-onnx-v1.13.8-ohos-arm64-v8a/lib \
#     apps/ohos-diary-core/scripts/stage-sherpa.sh
#
#   apps/ohos-diary-core/scripts/stage-sherpa.sh --check
#       # 不拷贝，只检查 entry/libs 里已有的两个库（build-hap.sh 会调用它）
#
#   apps/ohos-diary-core/scripts/stage-sherpa.sh --force
#       # 跳过「来源 sha256 固定」与「不同版本拒绝覆盖」两道闸；符号断言不跳过
#
# 默认从 `$HOME/dev/caches/asr-ohos/...` 取；缓存目录不在仓库里（228 MB 模型 + 16 MB
# 库都不入库）。`entry/libs/` 本身也在 .gitignore 里：本脚本是「怎么拿到这些库」的
# 可复现记录，产物由构建时生成。
set -euo pipefail

HERE="$(cd "$(dirname "$0")/.." && pwd)"
SRC="${SHERPA_LIB_DIR:-$HOME/dev/caches/asr-ohos/sherpa-onnx-v1.13.8-ohos-arm64-v8a/lib}"
OUT="$HERE/entry/libs/arm64-v8a"

SHERPA_SO="libsherpa-onnx-c-api.so"
ORT_SO="libonnxruntime.so"

# 来源固定 sha256（sherpa-onnx v1.13.8 OHOS arm64 预编译产物；本机实测）。
SHERPA_SHA256="75b38a525e7178bee052ec99a77175a934daa71ab6983deabbe948a96ae84221"
ORT_SHA256="f9fd9080a10f2b93323d597225fbaa63d06f93b384bdfdd7285db9a9115fcc2b"

# sherpa v1.13.8 OHOS 预编译库引用、且随包 ORT 必须导出的唯一符号版本。
EXPECTED_ORT_SYMBOL="OrtGetApiBase@VERS_1.16.3"

MODE="stage"
FORCE=0
for arg in "$@"; do
  case "$arg" in
    --check) MODE="check" ;;
    --force) FORCE=1 ;;
    *)
      echo "[stage-sherpa] 未知参数：$arg（只认 --check / --force）" >&2
      exit 2
      ;;
  esac
done

fail() {
  echo "[stage-sherpa] 失败：$*" >&2
  exit 1
}

# 找 llvm-readelf：优先 OHOS NDK（与构建链同一套），再 PATH 上的 llvm-readelf / readelf。
READELF_BIN="${READELF:-}"
if [[ -z "$READELF_BIN" ]]; then
  NDK="${NDK:-$HOME/command-line-tools/sdk/default/openharmony/native}"
  if [[ -x "$NDK/llvm/bin/llvm-readelf" ]]; then
    READELF_BIN="$NDK/llvm/bin/llvm-readelf"
  elif command -v llvm-readelf >/dev/null 2>&1; then
    READELF_BIN="$(command -v llvm-readelf)"
  elif command -v readelf >/dev/null 2>&1; then
    READELF_BIN="$(command -v readelf)"
  else
    fail "需要 llvm-readelf（或 GNU readelf）做符号版本断言；设 READELF=<路径>"
  fi
fi

sha_of() {
  sha256sum "$1" | cut -d' ' -f1
}

# 来源哈希断言；--force 时降级为警告（例如换用自编库）。
check_pinned() {
  local file="$1" expected="$2" label="$3" actual
  actual="$(sha_of "$file")"
  if [[ "$actual" == "$expected" ]]; then
    printf '[stage-sherpa] %-28s sha256=%s（与钉死值一致）\n' "$label" "$actual"
    return 0
  fi
  if [[ "$FORCE" == "1" ]]; then
    printf '[stage-sherpa] 警告：%s 的 sha256=%s 与钉死值 %s 不同（--force）\n' \
      "$label" "$actual" "$expected" >&2
    return 0
  fi
  fail "$label 的 sha256=$actual 与钉死值 $expected 不同；换来源请用 --force 并更新本脚本"
}

# 从 ELF 的 .dynsym 里取 OrtGetApiBase 的符号版本（`VERS_1.16.3` 这种）。
# 注意：不要 `... | grep -q` / `| head`——脚本开了 pipefail，读端提前退出会让
# 写端吃 SIGPIPE，整条管道变成失败（表现为「AArch64 明明是」却报不是）。
ort_symbol_version() {
  local symbols
  symbols="$("$READELF_BIN" --dyn-syms --wide "$1" 2>/dev/null)" || return 1
  printf '%s\n' "$symbols" \
    | grep -oE 'OrtGetApiBase@{1,2}VERS_[0-9.]+' \
    | sed -n '1p' \
    | sed -E 's/^OrtGetApiBase@+//'
}

assert_aarch64() {
  local header
  header="$("$READELF_BIN" -h "$1" 2>/dev/null)" || fail "读不出 $1 的 ELF 头"
  case "$header" in
    *AArch64*) ;;
    *) fail "$1 不是 AArch64 ELF" ;;
  esac
}

# 断言这一对库是配对的：arch 对、sherpa 的 NEEDED 有 ORT、符号版本一致且是钉死的那一版。
assert_pair() {
  local dir="$1"
  local engine="$dir/$SHERPA_SO" ort="$dir/$ORT_SO"
  [[ -f "$engine" ]] || fail "缺少 $engine（先跑 scripts/stage-sherpa.sh）"
  [[ -f "$ort" ]] || fail "缺少 $ort（sherpa 的 NEEDED 需要它；先跑 scripts/stage-sherpa.sh）"

  assert_aarch64 "$engine"
  assert_aarch64 "$ort"

  local dynamics
  dynamics="$("$READELF_BIN" -d "$engine" 2>/dev/null)" || fail "读不出 $engine 的动态段"
  case "$dynamics" in
    *libonnxruntime.so*) ;;
    *) fail "$engine 的 NEEDED 里没有 libonnxruntime.so（打进去也加载不起来）" ;;
  esac

  local required provided
  required="$(ort_symbol_version "$engine")"
  provided="$(ort_symbol_version "$ort")"
  [[ -n "$required" ]] || fail "$engine 没有引用 OrtGetApiBase（不是我们认识的 sherpa C API 库？）"
  [[ -n "$provided" ]] || fail "$ort 没有导出 OrtGetApiBase"
  [[ "$required" == "$provided" ]] \
    || fail "符号版本不配对：sherpa 要 $required，随包 ORT 提供 $provided"
  [[ "OrtGetApiBase@$provided" == "$EXPECTED_ORT_SYMBOL" ]] \
    || fail "随包 ORT 是 OrtGetApiBase@$provided，本 PR 钉死的是 $EXPECTED_ORT_SYMBOL（换 ORT 必须重核 SHERPA_SHA256/ORT_SHA256 与这条断言）"

  printf '[stage-sherpa] %-28s NEEDED 有 %s；符号 %s；AArch64 OK\n' \
    "$SHERPA_SO" "$ORT_SO" "$EXPECTED_ORT_SYMBOL"
}

if [[ "$MODE" == "check" ]]; then
  assert_pair "$OUT"
  check_pinned "$OUT/$SHERPA_SO" "$SHERPA_SHA256" "$SHERPA_SO"
  check_pinned "$OUT/$ORT_SO" "$ORT_SHA256" "$ORT_SO"
  for so in "$SHERPA_SO" "$ORT_SO"; do
    printf '[stage-sherpa] %-28s bytes=%s sha256=%s\n' "$so" \
      "$(stat -c%s "$OUT/$so")" "$(sha_of "$OUT/$so")"
  done
  echo "[stage-sherpa] CHECK OK → $OUT"
  exit 0
fi

# ---------------------------------------------------------------- 写入模式

mkdir -p "$OUT"
for so in "$SHERPA_SO" "$ORT_SO"; do
  [[ -f "$SRC/$so" ]] || {
    echo "缺少 $SRC/$so（先解压 sherpa-onnx-v1.13.8-ohos-arm64-v8a.tar.bz2）" >&2
    exit 1
  }
done
check_pinned "$SRC/$SHERPA_SO" "$SHERPA_SHA256" "$SHERPA_SO"
check_pinned "$SRC/$ORT_SO" "$ORT_SHA256" "$ORT_SO"

# 覆盖前：目标已有 ORT 且版本与来源不同 ⇒ 拒绝（除非 --force）。
# 这条防的正是「别人放的自编 1.29.1 被静默覆盖」。
SRC_ORT_VERSION="$(ort_symbol_version "$SRC/$ORT_SO")"
[[ -n "$SRC_ORT_VERSION" ]] || fail "$SRC/$ORT_SO 没有导出 OrtGetApiBase，来源不对"
if [[ -f "$OUT/$ORT_SO" ]]; then
  TARGET_ORT_VERSION="$(ort_symbol_version "$OUT/$ORT_SO")"
  if [[ -n "$TARGET_ORT_VERSION" && "$TARGET_ORT_VERSION" != "$SRC_ORT_VERSION" ]]; then
    if [[ "$FORCE" == "1" ]]; then
      echo "[stage-sherpa] 警告：覆盖不同版本的 $OUT/$ORT_SO（目标 $TARGET_ORT_VERSION → 来源 $SRC_ORT_VERSION，--force）" >&2
    else
      fail "拒绝覆盖 $OUT/$ORT_SO：目标已是 $TARGET_ORT_VERSION，来源是 $SRC_ORT_VERSION。\
 两份 ORT 可能都有用（sherpa 要 $SRC_ORT_VERSION）；确需覆盖请加 --force"
    fi
  fi
fi

cp -f "$SRC/$SHERPA_SO" "$OUT/$SHERPA_SO"
cp -f "$SRC/$ORT_SO" "$OUT/$ORT_SO"

echo "[stage-sherpa] 来源 $SRC"
assert_pair "$OUT"
if command -v file >/dev/null 2>&1; then
  file -b "$OUT/$SHERPA_SO" | sed 's/^/[stage-sherpa] /'
fi
for so in "$SHERPA_SO" "$ORT_SO"; do
  printf '[stage-sherpa] %-28s bytes=%s sha256=%s\n' "$so" \
    "$(stat -c%s "$OUT/$so")" "$(sha_of "$OUT/$so")"
done
echo "[stage-sherpa] OK → $OUT"

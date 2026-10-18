#!/usr/bin/env bash
# 把**自编的 ONNX Runtime 1.29.1** 以「不与 sherpa 撞名」的方式放进 HAP（可选语义包）。
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
# ★ 来源固定 sha256（两个都要断言，别把「源文件」和「包内文件」搞混）：
#   - **源文件**（构建产物，未 strip）：$HOME/dev/caches/ort-1291/libonnxruntime.so
#       bytes=19,623,736  sha256=a224007497059f8a2a433b6604a22e5898bac76d7b3332bb64582df6ed9991de
#   - **包内文件**（本脚本 `llvm-strip --strip-all` 后，hvigor 打进 HAP 的就是它）：
#       bytes=19,623,560  sha256=1ece24ca08c392a3f7a2bee50b002f3a3f4f33c44c2df9b5e7a424e60e573660
#   证据：HAP 内 `libs/arm64-v8a/libonnxruntime-diary-1.29.1.so` 与第二条逐字节一致
#   （`E:\ohos\evidence\hapcfg\hap-package.txt`）。
#
# 本脚本只负责「拷进去 + strip + 自检」，不下载、不构建。构建配方见
# probes/ohos-core-smoke/evidence/ort-1.29.1-build.md。
#
# 用法：
#   apps/ohos-diary-core/scripts/stage-ort-1291.sh            # 放进 entry/libs（可选语义包）
#   apps/ohos-diary-core/scripts/stage-ort-1291.sh --check    # 只检查已有文件（WITH_SEMANTIC=1 构建调用）
#   apps/ohos-diary-core/scripts/stage-ort-1291.sh --force    # 跳过两条 sha256（换自编来源时）
set -euo pipefail

HERE="$(cd "$(dirname "$0")/.." && pwd)"
OUT="$HERE/entry/libs/arm64-v8a"
SRC="${DIARY_ORT_SRC:-$HOME/dev/caches/ort-1291/libonnxruntime.so}"
NDK="${NDK:-$HOME/command-line-tools/sdk/default/openharmony/native}"
READELF="${READELF:-$NDK/llvm/bin/llvm-readelf}"
STRIP="${STRIP:-$NDK/llvm/bin/llvm-strip}"

TARGET_NAME="libonnxruntime-diary-1.29.1.so"
SHERPA_ORT="libonnxruntime.so"

# 源文件（未 strip）与包内文件（strip 后）各自的钉死值。
SOURCE_SHA256="a224007497059f8a2a433b6604a22e5898bac76d7b3332bb64582df6ed9991de"
PACKAGED_SHA256="1ece24ca08c392a3f7a2bee50b002f3a3f4f33c44c2df9b5e7a424e60e573660"
PACKAGED_BYTES=19623560
EXPECTED_SONAME="libonnxruntime.so.1"

MODE="stage"
FORCE=0
for arg in "$@"; do
  case "$arg" in
    --check) MODE="check" ;;
    --force) FORCE=1 ;;
    *)
      echo "[stage-ort] 未知参数：$arg（只认 --check / --force）" >&2
      exit 2
      ;;
  esac
done

fail() {
  echo "[stage-ort] 失败：$*" >&2
  exit 1
}

sha_of() {
  sha256sum "$1" | cut -d' ' -f1
}

check_pinned() {
  local file="$1" expected="$2" label="$3" actual
  actual="$(sha_of "$file")"
  if [[ "$actual" == "$expected" ]]; then
    printf '[stage-ort] %-36s sha256=%s（与钉死值一致）\n' "$label" "$actual"
    return 0
  fi
  if [[ "$FORCE" == "1" ]]; then
    printf '[stage-ort] 警告：%s 的 sha256=%s 与钉死值 %s 不同（--force）\n' \
      "$label" "$actual" "$expected" >&2
    return 0
  fi
  fail "$label 的 sha256=$actual 与钉死值 $expected 不同；换来源请用 --force 并更新本脚本"
}

# 符号断言：必须导出 OrtGetApiBase@@VERS_1.29.1，SONAME 必须是 libonnxruntime.so.1。
# 注意不要 `readelf ... | grep -q`（pipefail 下读端提前退出会让写端吃 SIGPIPE）。
assert_semantic_ort() {
  local file="$1"
  [[ -f "$file" ]] || fail "缺少 $file（先跑 scripts/stage-ort-1291.sh）"

  local symbols soname
  symbols="$("$READELF" --dyn-syms --wide "$file" 2>/dev/null)" || fail "读不出 $file 的符号表"
  case "$symbols" in
    *OrtGetApiBase@@VERS_1.29.1*) ;;
    *) fail "$file 没导出 OrtGetApiBase@@VERS_1.29.1" ;;
  esac

  soname="$("$READELF" -d "$file" 2>/dev/null | sed -n 's/.*SONAME.*\[\(.*\)\].*/\1/p')"
  [[ "$soname" == "$EXPECTED_SONAME" ]] \
    || fail "$file 的 SONAME 是「$soname」，应为 $EXPECTED_SONAME（否则可能与 sherpa 的 libonnxruntime.so 互串）"

  printf '[stage-ort] %-36s SONAME=%s；符号 OrtGetApiBase@@VERS_1.29.1 OK\n' \
    "$TARGET_NAME" "$soname"
}

# sherpa 那一对（libonnxruntime.so / libsherpa-onnx-c-api.so）由 stage-sherpa.sh 统一断言。
assert_sherpa_pair() {
  bash "$HERE/scripts/stage-sherpa.sh" --check
}

if [[ "$MODE" == "check" ]]; then
  assert_sherpa_pair
  [[ -f "$OUT/$TARGET_NAME" ]] || fail "缺少 $OUT/$TARGET_NAME（WITH_SEMANTIC=1 构建先跑 scripts/stage-ort-1291.sh）"
  check_pinned "$OUT/$TARGET_NAME" "$PACKAGED_SHA256" "$TARGET_NAME"
  [[ "$(stat -c%s "$OUT/$TARGET_NAME")" == "$PACKAGED_BYTES" ]] \
    || fail "$TARGET_NAME 的字节数不是 $PACKAGED_BYTES（包内口径）"
  assert_semantic_ort "$OUT/$TARGET_NAME"
  echo "[stage-ort] CHECK OK → $OUT"
  exit 0
fi

# ---------------------------------------------------------------- 写入模式

[[ -f "$SRC" ]] || fail "缺少 $SRC（自编 ORT 1.29.1）"
[[ -x "$STRIP" ]] || fail "没有 llvm-strip：$STRIP（strip 后的包内哈希才是钉死的 $PACKAGED_SHA256）"
mkdir -p "$OUT"

echo "[stage-ort] 源文件 $SRC（未 strip）"
check_pinned "$SRC" "$SOURCE_SHA256" "$TARGET_NAME 源文件"

cp -f "$SRC" "$OUT/$TARGET_NAME"
"$STRIP" --strip-all "$OUT/$TARGET_NAME"

echo "[stage-ort] 包内文件 $OUT/$TARGET_NAME（llvm-strip --strip-all 后）"
check_pinned "$OUT/$TARGET_NAME" "$PACKAGED_SHA256" "$TARGET_NAME 包内"
[[ "$(stat -c%s "$OUT/$TARGET_NAME")" == "$PACKAGED_BYTES" ]] \
  || fail "$TARGET_NAME 的字节数不是 $PACKAGED_BYTES（包内口径）"
assert_semantic_ort "$OUT/$TARGET_NAME"
assert_sherpa_pair

echo "[stage-ort] OK → $OUT（WITH_SEMANTIC=1 构建会把它打进 HAP；默认构建不含）"

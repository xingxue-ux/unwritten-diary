#!/usr/bin/env bash
# Android release builder for 「不写日记」.
#
# This wrapper intentionally does only the release-specific orchestration:
#   1. preflight the toolchain and the release keystore;
#   2. cross-compile the Rust bridge for arm64-v8a with cargo-ndk;
#   3. call `flutter build apk` and `flutter build appbundle`;
#   4. copy the artifacts to dist/android and print sizes/hashes.
#
# It does NOT build x86_64/armeabi-v7a: the product target is arm64 only, and
# shipping an ABI without a matching libdiary_bridge.so produces an installable
# app that crashes on first use.
#
# Usage:
#   tools/release/build-android.sh [--dry-run] [--skip-rust] [--skip-aab]
#
# Environment:
#   ANDROID_KEY_PROPERTIES   default apps/diary_app/android/key.properties
#   ANDROID_HOME / ANDROID_SDK_ROOT
#   CARGO_BUILD_JOBS         default 4 (this repo is often built on a 8 GB box)
#   DIST_DIR                 default <repo>/dist/android
#
# Exit codes: 0 ok, 2 usage/preflight failure, 1 build failure.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
APP="$ROOT/apps/diary_app"
KEY_PROPERTIES="${ANDROID_KEY_PROPERTIES:-$APP/android/key.properties}"
DIST_DIR="${DIST_DIR:-$ROOT/dist/android}"
DRY_RUN=0
SKIP_RUST=0
SKIP_AAB=0
CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-4}"

usage() {
  sed -n '2,31p' "$0" | sed 's/^# \{0,1\}//'
}

while (($#)); do
  case "$1" in
    --dry-run) DRY_RUN=1 ;;
    --skip-rust) SKIP_RUST=1 ;;
    --skip-aab) SKIP_AAB=1 ;;
    -h|--help) usage; exit 0 ;;
    *) echo "未知参数：$1（--help 看用法）" >&2; exit 2 ;;
  esac
  shift
done

fail() { echo "[build-android][失败] $*" >&2; exit 2; }
step() { echo; echo "==> $*"; }
run() {
  if ((DRY_RUN)); then printf '[dry-run]'; printf ' %q' "$@"; printf '\n'; else "$@"; fi
}

step "1/6 检查工具链"
command -v flutter >/dev/null || fail "找不到 flutter（把 Flutter SDK 的 bin 加进 PATH）"
command -v cargo >/dev/null || fail "找不到 cargo"
command -v cargo-ndk >/dev/null || fail "找不到 cargo-ndk（cargo install cargo-ndk --locked）"
command -v python3 >/dev/null || fail "找不到 python3（用于解析 key.properties）"
SDK="${ANDROID_HOME:-${ANDROID_SDK_ROOT:-}}"
[[ -n "$SDK" && -d "$SDK" ]] || fail "未设置 ANDROID_HOME 或 ANDROID_SDK_ROOT（需指向 Android SDK 根目录）"
[[ -d "$SDK/ndk" ]] || fail "Android SDK 下没有 ndk/（用 sdkmanager 安装 Flutter 要求的 NDK）"
[[ -f "$APP/pubspec.yaml" ]] || fail "找不到 $APP/pubspec.yaml"
echo "flutter: $(flutter --version 2>/dev/null | head -1)"
echo "Android SDK: $SDK"

step "2/6 检查 release 签名材料"
if [[ ! -f "$KEY_PROPERTIES" ]]; then
  cat >&2 <<EOF
[build-android][失败] 找不到 release 签名配置：$KEY_PROPERTIES

这不是可以静默跳过的步骤：没有它，Gradle 会退回 debug 签名，
产物不能当正式发布包（也不能上架/覆盖安装正式版）。

怎么补：
  1. 生成密钥库（只需一次，务必离线备份，丢失后无法覆盖升级）：
       keytool -genkeypair -v -keystore apps/diary_app/android/release-keystore.jks \\
         -storetype JKS -keyalg RSA -keysize 2048 -validity 10000 -alias release
  2. 复制模板并填真实值：
       cp apps/diary_app/android/key.properties.example apps/diary_app/android/key.properties
       chmod 600 apps/diary_app/android/key.properties
  3. storeFile 相对 apps/diary_app/android/ 解析；也可以写绝对路径。
  4. 把 keystore 和 key.properties 放进密码管理器/CI secret，不要提交。
     参考 docs/development/M5-打包与发布.md 的签名小节。
EOF
  exit 2
fi

# Parse key.properties without sourcing it (values may contain spaces or #).
eval "$(python3 - "$KEY_PROPERTIES" "$APP/android" <<'PY'
import os, shlex, sys
path, base = sys.argv[1], sys.argv[2]
props = {}
for raw in open(path, encoding="utf-8"):
    line = raw.strip()
    if not line or line.startswith("#") or "=" not in line:
        continue
    key, value = line.split("=", 1)
    props[key.strip()] = value.strip()
required = ["storeFile", "storePassword", "keyAlias", "keyPassword"]
missing = [k for k in required if not props.get(k)]
if missing:
    print(f"echo 'key.properties 缺少字段：{','.join(missing)}' >&2; exit 2")
    sys.exit(0)
for key in ("storeFile", "storePassword", "keyAlias", "keyPassword"):
    placeholder = "REPLACE_WITH_" in props[key]
    if placeholder:
        print(f"echo 'key.properties 里 {key} 还是模板占位符' >&2; exit 2")
        sys.exit(0)
store = props["storeFile"]
if not os.path.isabs(store):
    store = os.path.join(base, store)
for key, value in {
    "STORE_FILE": store,
    "STORE_PASSWORD": props["storePassword"],
    "KEY_ALIAS": props["keyAlias"],
    "KEY_PASSWORD": props["keyPassword"],
}.items():
    print(f"{key}={shlex.quote(value)}")
PY
)"
[[ -f "$STORE_FILE" ]] || fail "key.properties 指向的 keystore 不存在：$STORE_FILE"
command -v keytool >/dev/null || fail "找不到 keytool（安装 JDK，并把它加进 PATH）"
if ! keytool -list -keystore "$STORE_FILE" -storepass "$STORE_PASSWORD" -alias "$KEY_ALIAS" >/dev/null 2>&1; then
  fail "keystore 打不开或别名不存在（$STORE_FILE / $KEY_ALIAS）；检查密码和别名"
fi
echo "keystore: $STORE_FILE"
echo "alias:    $KEY_ALIAS"

step "3/6 交叉编译 Rust bridge（arm64-v8a）"
JNI_DIR="$ROOT/target/android-jniLibs"
if ((SKIP_RUST)); then
  echo "跳过 cargo-ndk（--skip-rust）"
else
  run env CARGO_BUILD_JOBS="$CARGO_BUILD_JOBS" \
    cargo ndk -t arm64-v8a -o "$JNI_DIR" build --release -p diary_bridge
fi
BRIDGE="$JNI_DIR/arm64-v8a/libdiary_bridge.so"
if ((!DRY_RUN)); then
  [[ -f "$BRIDGE" ]] || fail "cargo-ndk 没有产出 $BRIDGE"
  file "$BRIDGE"
  printf 'libdiary_bridge.so bytes=%s\n' "$(stat -c%s "$BRIDGE")"
fi

step "4/6 flutter build apk --release（arm64 only，ABI 拆分）"
# --split-per-abi keeps each APK to one ABI. With a single target platform the
# result is the arm64-v8a APK; versionCode gets Flutter's ABI offset.
run bash -c "cd \"$APP\" && flutter build apk --release --target-platform android-arm64 --split-per-abi"

APK=""
for candidate in \
  "$APP/build/app/outputs/flutter-apk/app-arm64-v8a-release.apk" \
  "$APP/build/app/outputs/flutter-apk/app-release.apk"; do
  [[ -f "$candidate" ]] && APK="$candidate" && break
done

step "5/6 flutter build appbundle --release（arm64 only）"
if ((SKIP_AAB)); then
  echo "跳过 AAB（--skip-aab）"
else
  run bash -c "cd \"$APP\" && flutter build appbundle --release --target-platform android-arm64"
fi
AAB="$APP/build/app/outputs/bundle/release/app-release.aab"

step "6/6 收集产物与自检"
if ((!DRY_RUN)); then
  [[ -n "$APK" ]] || fail "找不到 APK（期望 app-arm64-v8a-release.apk 或 app-release.apk）"
  mkdir -p "$DIST_DIR"
  cp -f "$APK" "$DIST_DIR/"
  if ((!SKIP_AAB)); then
    [[ -f "$AAB" ]] || fail "找不到 AAB：$AAB"
    cp -f "$AAB" "$DIST_DIR/"
  fi
  if [[ -d "$APP/assets/legal" ]]; then
    mkdir -p "$DIST_DIR/legal"
    cp -f "$APP/assets/legal/"* "$DIST_DIR/legal/" 2>/dev/null || true
  fi
  ( cd "$DIST_DIR" && sha256sum ./*.apk ./*.aab 2>/dev/null > SHA256SUMS || true )
  echo
  echo "产物目录：$DIST_DIR"
  ls -lh "$DIST_DIR"
  echo
  echo "APK 内体积明细（Stored 是未压缩；Android 对 .so 常按不压缩存放）："
  unzip -v "$APK" | awk 'NR<=2 || /lib\/arm64-v8a|assets\/flutter_assets|AndroidManifest|classes.dex|resources.arsc/' | head -80
  APKSIGNER="$(find "$SDK/build-tools" -type f -name apksigner 2>/dev/null | sort -V | tail -1 || true)"
  if [[ -n "$APKSIGNER" ]]; then
    echo
    "$APKSIGNER" verify --print-certs "$APK" | sed -n '1,12p'
  else
    echo "提示：没找到 apksigner，未自动核验签名；安装 Android SDK build-tools 后重跑。"
  fi
  echo
  echo "体积核算与预算冲突说明见 docs/development/M5-打包与发布.md。"
fi

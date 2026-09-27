# diary_bridge

`diary_api` 与 Rust 核心之间的 Dart 适配层，含 [flutter_rust_bridge](https://cjycode.com/flutter_rust_bridge/) 生成文件。

## 目录

| 路径 | 说明 |
|---|---|
| `lib/src/rust/` | 生成文件（`api.dart`、`frb_generated*.dart`），**禁止手改** |
| `test/bridge_probe_test.dart` | 桥接最小闭环：Dart 真正调用到 Rust cdylib |

对应 Rust 侧在 `crates/diary_bridge/`，生成入口是仓库根目录的 `flutter_rust_bridge.yaml`。

## 重新生成绑定

在仓库根目录执行：

```bash
flutter_rust_bridge_codegen generate
```

三条硬性约定：

1. codegen 版本必须与 `crates/diary_bridge/Cargo.toml` 里的 `flutter_rust_bridge` 依赖一致（当前 `2.13.0`）。不一致时 Rust 侧启动会直接拒绝加载，这是刻意的。
2. 生成文件必须入库；改了 Rust 的公开 API 就要在同一个 PR 里重新生成并提交。
3. 不手改生成代码。要改行为就改 `crates/diary_bridge/src/api.rs` 并重新生成。

`dart` 的路径要在 PATH 里指向本机的 Dart/Flutter（WSL 里用 `~/dev/flutter/bin`；`/mnt/e/dev/flutter/bin` 是 Windows 版，从 WSL 调起来是坏的）。

## 构建 Rust 产物

| 目标 | 命令（在哪里跑） | 产物 |
|---|---|---|
| Linux（开发探针用） | `cargo build --release`（WSL） | `target/release/libdiary_bridge.so` |
| Windows x64 | `cargo build --release --target x86_64-pc-windows-msvc`（Windows 侧） | `target/x86_64-pc-windows-msvc/release/diary_bridge.dll` |
| Android arm64 | `cargo ndk -t arm64-v8a -o target/android-jniLibs build --release`（WSL，需 `ANDROID_HOME`） | `target/android-jniLibs/arm64-v8a/libdiary_bridge.so` |

**WSL 与 Windows 共用同一个 `target/`**（仓库在 `/mnt/e` 上）。两边同时跑 cargo 会争用同一个构建目录：我遇到过一次 `tokio` 编译失败，串行重跑即恢复。要么别同时跑，要么给其中一边设独立的 `CARGO_TARGET_DIR`。

Windows 侧构建要显式带 `--target`，否则产物会写进与 WSL 共用的 `target/release/`。

## 跑桥接探针

```bash
cargo build --release                        # 仓库根目录
cd packages/diary_bridge && dart test        # 5 项调用验证
```

`DIARY_BRIDGE_LIB` 可指定别的产物路径（例如 Windows 上的 `.dll`）。

## 现状与未验证

已验证：同步与异步调用、结构体字段映射（Rust `snake_case` → Dart `camelCase`）、中文与 emoji 往返、Windows x64 与 Android arm64 产物构建。

未验证：事件流（`StreamSink`）、调用取消、后台运行入口、以及在 Android 真机/模拟器与 Windows 上真实加载 `.so` / `.dll`（Loading 只在 Linux 宿主上验证过）。
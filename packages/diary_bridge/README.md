# diary_bridge

Dart 侧访问 Rust 核心的唯一入口，含 [flutter_rust_bridge](https://cjycode.com/flutter_rust_bridge/) 生成文件。

对外只有一个入口 `diary_bridge.dart`，里面是 `BridgeSession`（不透明会话句柄）、核心的数据对象（`Capture` / `Job` / `Coverage` 等）、以及统一错误 `BridgeError`。设计取舍见 [M1 · 桥接接线](../../docs/architecture/m1-桥接接线.md)。

## 目录

| 路径 | 说明 |
|---|---|
| `lib/diary_bridge.dart` | 公开入口（barrel），上层只 import 这一个文件 |
| `lib/src/rust/` | 生成文件（`api.dart`、`lib.dart`、`frb_generated*.dart`），**禁止手改** |
| `lib/src/rust/third_party/diary_core/model.dart` | 核心数据结构的 Dart 镜像，同样由 codegen 生成 |
| `test/bridge_probe_test.dart` | 端到端测试：Dart 真正调用到 Rust cdylib 并读写资料库 |

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

`flutter_rust_bridge.yaml` 的 `rust_input` 里除 `crate::api` 还包含 `diary_core::model`——桥接层不另造一套 DTO，直接复用核心的字段定义。

## 构建 Rust 产物

| 目标 | 命令（在哪里跑） | 产物 |
|---|---|---|
| Linux（开发用） | `cargo build --release`（WSL） | `target/release/libdiary_bridge.so` |
| Windows x64 | `cargo build --release --target x86_64-pc-windows-msvc`（Windows 侧） | `target/x86_64-pc-windows-msvc/release/diary_bridge.dll` |
| Android arm64 | `cargo ndk -t arm64-v8a -o target/android-jniLibs build --release`（WSL，需 `ANDROID_HOME`） | `target/android-jniLibs/arm64-v8a/libdiary_bridge.so` |

**WSL 与 Windows 共用同一个 `target/`**（仓库在 `/mnt/e` 上）。两边同时跑 cargo 会争用同一个构建目录：我遇到过一次 `tokio` 编译失败，串行重跑即恢复。要么别同时跑，要么给其中一边设独立的 `CARGO_TARGET_DIR`。

Windows 侧构建要显式带 `--target`，否则产物会写进与 WSL 共用的 `target/release/`。

## 跑端到端测试

```bash
cargo build --release                        # 仓库根目录
cd packages/diary_bridge
dart pub get && dart test                    # 6 项，跑完整读写链路
```

`DIARY_BRIDGE_LIB` 可指定别的产物路径（例如 Windows 上的 `.dll`）。

测试覆盖的不是「能调通一个函数」，而是：打开资料库 → 建草稿 → 保存 → 提交 → 重读 → 导入文本文件 → 排队并执行提取 → 读派生正文 → 定位原件，外加错误码映射、事件游标补读、中文与 emoji 往返。

## 现状与未验证

已验证：Linux（WSL）宿主 6/6、Windows x64 6/6、Android（API 36 x86_64 模拟器）等价探针 5/5；`BridgeSession` 全链路读写、`CoreError` → 契约第 7 节错误码映射、能力清单、中文与 emoji 往返。

**没接的就是 `LibraryInfo.capabilities` 里没列的**：搜索、索引、日记生成、插件、备份、Provider 配置、录音会话、任务执行。

未验证：event `Stream`（只有 `events_since` 拉取）、调用取消、后台运行入口、Android arm64 真机。
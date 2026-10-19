# diary_bridge

Dart 侧访问 Rust 核心的唯一入口，含 [flutter_rust_bridge](https://cjycode.com/flutter_rust_bridge/) 生成文件。

对外只有一个入口 `diary_bridge.dart`，里面是 `BridgeSession`（不透明会话句柄）、核心的数据对象（`Capture` / `Job` / `Coverage` 等）、以及统一错误 `BridgeError`。设计取舍见 [M1 · 桥接接线](../../docs/architecture/m1-桥接接线.md)。

## 目录

| 路径 | 说明 |
|---|---|
| `lib/diary_bridge.dart` | 公开入口（barrel），上层只 import 这一个文件 |
| `lib/src/rust/` | 生成文件（`api.dart`、`lib.dart`、`frb_generated*.dart`），**禁止手改** |
| `lib/src/rust/third_party/diary_core/model.dart` | 核心数据结构的 Dart 镜像，同样由 codegen 生成 |
| `lib/src/bridge_diary_api.dart` | 契约 `DiaryApi` 的适配层（记录、检索、日记、风格、天列表与阅读导出） |
| `test/bridge_probe_test.dart` | 端到端测试：Dart 真正调用到 Rust cdylib 并读写资料库 |
| `test/bridge_diary_api_test.dart` | 端到端测试：**只经 `DiaryApi`** 跑真核心（前端真正用的那条路径） |

对应 Rust 侧在 `crates/diary_bridge/`，生成入口是仓库根目录的 `flutter_rust_bridge.yaml`。

**`diary_api` 适配层覆盖的范围**：`BridgeDiaryApi` 把 `BridgeSession` 适配成契约的
`DiaryApi`——记录、导入/录音交接、提取、检索、Provider/回应、日记生成与版本（含编辑/
指定当前/合并）、**风格 list/get/save**、**`diary.listDays`** 与 **`exports.create`**
都在能力清单里。其余方法明确抛错且不出现在 `capabilities` 里。前端目前仍注入
`packages/diary_mock`——切到真核心还需要平台层给出库路径与时区（F1）。设计与取舍见
[M1 · DiaryApi 适配层](../../docs/architecture/m1-DiaryApi适配层.md)。

**阅读导出（#113）**：核心同步生成 Markdown/纯文本到资料库的 `exports/` 下，并写一条
`state = succeeded` 的 `export` 任务记录。契约的 `ExportTicket` 只有 `jobId` /
`packageTicket`；`packageTicket` 在本构建里是导出文件的绝对路径。桥接的
`ExportTicket` 还带 `path` / `byteSize` / `restorable = false`。**它不能用于恢复资料库**；
需要恢复请用 `backups.*`。

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
dart pub get && dart test                    # 全套（当前 41 项），跑完整读写链路
```

`DIARY_BRIDGE_LIB` 可指定别的产物路径（例如 Windows 上的 `.dll`）。

测试覆盖的不是「能调通一个函数」，而是：打开资料库 → 建草稿 → 保存 → 提交 → 重读 → 导入文本文件 → 排队并执行提取 → 读派生正文 → 定位原件 → 检索（关键词/语义/混合与升级）→ Provider 配置与连接测试 → 日记生成票据与版本读取 → 日记编辑/指定当前/合并的错误码与能力声明，外加错误码映射、事件游标补读、中文与 emoji 往返。日记版本的**语义**（新版本成为当前、旧版退出默认视图、修订链、冲突零改动、幂等）在核心侧的 `crates/diary_core/tests/diary_versions_test.rs` 里钉住：造一版日记要一次真实模型调用，而 CI 不联网。

## 现状与未验证

已验证：Linux（WSL）宿主 6/6、Windows x64 6/6、Android（API 36 x86_64 模拟器）等价探针 5/5；`BridgeSession` 全链路读写、`CoreError` → 契约第 7 节错误码映射、能力清单、中文与 emoji 往返。

**没接的就是 `LibraryInfo.capabilities` 里没列的**：回收站与清理、设置、备份与恢复的票据化入口、任务的确认提醒。**已经接上**的是记录与导入、提取与来源定位、检索与索引、Provider 配置与回应、日记的生成与版本（含 B4c 的编辑/指定当前/合并）、风格（`styles.*`）、天列表（`diary.listDays`）、阅读导出（`exports.create`）与队列驱动。

未验证：event `Stream`（只有 `events_since` 拉取）、调用取消、后台运行入口、Android arm64 真机。
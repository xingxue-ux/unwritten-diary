# diary_bridge

Dart 侧访问 Rust 核心的唯一入口，含 [flutter_rust_bridge](https://cjycode.com/flutter_rust_bridge/) 生成文件。

对外只有一个入口 `diary_bridge.dart`，里面是 `BridgeSession`（不透明会话句柄）、核心的数据对象（`Capture` / `Job` / `Coverage` 等）、以及统一错误 `BridgeError`。设计取舍见 [M1 · 桥接接线](../../docs/architecture/m1-桥接接线.md)。

## 目录

| 路径 | 说明 |
|---|---|
| `lib/diary_bridge.dart` | 公开入口（barrel），上层只 import 这一个文件 |
| `lib/src/rust/` | 生成文件（`api.dart`、`lib.dart`、`frb_generated*.dart`），**禁止手改** |
| `lib/src/rust/third_party/diary_core/model.dart` | 核心数据结构的 Dart 镜像，同样由 codegen 生成 |
| `lib/src/bridge_diary_api.dart` | 契约 `DiaryApi` 的适配层（62/76，见下） |
| `test/bridge_probe_test.dart` | 端到端测试：Dart 真正调用到 Rust cdylib 并读写资料库 |
| `test/bridge_diary_api_test.dart` | 端到端测试：**只经 `DiaryApi`** 跑真核心（前端真正用的那条路径） |
| `test/bridge_diary_coverage_test.dart` | 本轮新接方法：事件流、资产租约、来源定位、索引、确认提醒、路径专用备份检查 |
| `test/bridge_diary_backup_api_test.dart` | 备份**路径专用入口**：同步 jobId、包内不含密钥、非空目标/坏包拒绝、票据形态拒绝 |

对应 Rust 侧在 `crates/diary_bridge/`，生成入口是仓库根目录的 `flutter_rust_bridge.yaml`。

**`diary_api` 适配层当前覆盖 64/76 个契约方法**：记录与导入、来源修订/提取/定位、资产租约、事件流、检索与索引、Provider、日记生成与版本、**风格 `styles.list` / `styles.get` / `styles.save`（#114 / #166）**、**天列表 `diary.listDays`（#168）**、**阅读导出 `exports.create`（#113 / #167）**、任务读取/取消/重试/到期执行/确认提醒、插件管理。**没接的 12 条**会在 `noSuchMethod` 里被点名，且不出现在 `capabilities` 里——回收站五条（#155 已合进 main，但适配层这一轮还没接）、`settings.*` 两条属于另一条线、`core.setRuntimeState`（核心没有资源调度入口）、`plugins.invoke`（核心同步入口没有可读的任务结果），以及契约收 `PlatformHost` 限域票据而票据解析尚未实现的 `backups.*` 三条。备份本轮只有**路径专用**的 `createBackupAtPath` / `inspectBackupAtPath` / `restoreBackupAtPath`（非契约、不写进 capabilities）：同步完成之后写一条 `succeeded` 的 `backup` / `restore` 任务记录，`jobId` 指向已完成记录，不是排队任务（取舍见 m5 文档）。票据解析就绪后，契约入口应先在宿主侧解析再接到这三条上。设计与取舍见 [M1 · DiaryApi 适配层](../../docs/architecture/m1-DiaryApi适配层.md)。

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
dart pub get && dart test                    # 全套，跑完整读写链路
```

`DIARY_BRIDGE_LIB` 可指定别的产物路径（例如 Windows 上的 `.dll`）。

测试覆盖的不是「能调通一个函数」，而是：打开资料库 → 建草稿 → 保存 → 提交 → 重读 → 导入文本文件 → 排队并执行提取 → 读派生正文 → 定位原件 → 打开/释放资产租约 → 检索（关键词/语义/混合与升级）→ Provider 配置与连接测试 → 日记生成票据与版本读取 → 日记编辑/指定当前/合并的错误码与能力声明 → 索引状态与关键词重建任务 → 确认提醒 → 事件流按序号分页补读与轮询 → 路径专用备份创建（jobId 指向已完成记录）→ 检查 → 恢复到新库（拒绝非空目标/坏包/票据形态），外加错误码映射、中文与 emoji 往返。日记版本的**语义**（新版本成为当前、旧版退出默认视图、修订链、冲突零改动、幂等）在核心侧的 `crates/diary_core/tests/diary_versions_test.rs` 里钉住：造一版日记要一次真实模型调用，而 CI 不联网。

## 现状与未验证

已验证（本机 WSL + Rust 1.94.1）：`cargo clippy --workspace --all-targets -- -D warnings`、`cargo test --workspace`（572 通过 / 0 失败 / 8 忽略）、`dart analyze --fatal-infos`；`dart test` 的端到端链路由 CI 的 `bridge` 任务跑（本机未跑，不在这里报数字）。`BridgeSession` 全链路读写、`CoreError` → 契约第 7 节错误码映射、能力清单、中文与 emoji 往返。

**没接的就是 `LibraryInfo.capabilities` 里没列的**（最近一次核对的差集，12 条）：备份三条（`backups.create` / `inspectBackup` / `restoreBackup`——契约收 `PlatformHost` 限域票据，票据解析协议待另立；本轮只有路径专用入口）、回收站五条（`captures.moveDay/trash/restore/previewPurge/purge`；#155 已在 main，适配层还没接）、`setRuntimeState`（核心没有资源调度入口）、`getSettings` / `updateSettings`（设置线在做）、`invokePlugin`（核心同步返回完整结果且没有结果读取入口，硬套 `jobId` 会造出「有 id 没结果」的假票据）。**已经接上**的是上面的 64 条：记录与导入、提取与来源定位、检索与索引、Provider 配置与回应、日记的生成与版本（含编辑/指定当前/合并）、风格（`styles.*`）、天列表（`diary.listDays`）、阅读导出（`exports.create`）与队列驱动。

未验证：Windows/Android 真机上的新增入口（本机只跑了 Linux WSL）、调用取消、后台运行入口、Android arm64 真机。

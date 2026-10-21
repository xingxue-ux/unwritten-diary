# M1 · DiaryApi 适配层

对应 issue [#38](https://github.com/xingxue-ux/unwritten-diary/issues/38) · 日期 2026-10-09 · 执行人 xingxue-ux

把桥接的 `BridgeSession` 适配成契约的 `DiaryApi`，让前端（F0，已合入）能经**契约接口**跑真核心。代码在 `packages/diary_bridge/lib/src/bridge_diary_api.dart`。桥接本身见 [M1 · 桥接接线](m1-桥接接线.md)。

> **2026-10-21 覆盖率更新（64/76，#166 接上风格三条 + #167 接上天列表与阅读导出）**：59/76 → 62/76 是 `styles.list` / `styles.get` / `styles.save`（核心 `styles.rs`、桥接 `list_styles` / `get_style` / `save_style`、适配层 `listStyles` / `getStyle` / `saveStyle`，#166）；再 → 64/76 是 `diary.listDays`（#168）与 `exports.create`（#113 / #167）。下面的 2026-10-20 记录保留为历史。
>
> **2026-10-20 覆盖率更新（59/76）**：在记录路径之外，这一轮又接上了事件流、来源定位、资产租约、索引状态/重建、任务确认提醒等（详见文末「覆盖率扩展」）。备份只有**路径专用**的 `createBackupAtPath` / `inspectBackupAtPath` / `restoreBackupAtPath`：契约 `backups.*` 收的是 `PlatformHost` 限域票据，票据解析尚未实现，因此**不宣告**契约备份能力。下面「只接记录路径」「3 项测试」等段落是 2026-10-09 首版的现场记录，保留作为历史；当前口径以文末与 `packages/diary_bridge/README.md` 为准。

## 这一片解决了什么

在此之前前端只有 `MockDiaryApi` 一个实现：`BridgeSession` 暴露的是 frb 生成的另一套同名类型，两者之间没有桥。现在 `BridgeDiaryApi` 把**记录路径**接通了，端到端测试只经 `DiaryApi` 调用（不碰任何桥接类型），走真核心 + 真 SQLite。

## 定下的决定

| 决定 | 理由 |
|---|---|
| **只接记录路径**：`open` / `snapshot` / `close` / `createDraft` / `saveDraft` / `commit` / `getCapture` / `listCaptures` | 这是 F0 现在真正调用的全部方法。其余方法按需逐个加，不凭猜测设计（提取/导入/搜索各自单独一片更清楚） |
| **没接的方法用 `noSuchMethod` 明确抛错**，照 `diary_mock` 的做法 | 不能悄悄返回空数据。抛的是 `UnsupportedError` 而不是契约异常——这是「代码没写」，不是「核心报错」 |
| **能力清单取交集**：核心报 17 个方法，适配层只上报自己真的能用的 7 个 | 能力清单是给界面看的「哪些入口可见」。桥接接了但适配层没接的方法如果留在清单里，界面会把点不通的入口显示成可用 |
| **错误码翻译，未知码落 `unknown`** | 桥接抛 `BridgeError`（字符串码），契约是 `DiaryException` + `DiaryErrorCode` 枚举。核心将来加了新码而契约还没跟上时，不能让前端拿到一个未定义枚举或直接崩 |
| **时区与偏移由调用方注入** | 契约把它们放在平台层，核心的 `createDraft` 却必填。平台层（F1）还没建，所以先注入；F1 到位后换成平台实现，适配层签名不变 |
| **`recoveredDraftCount` 恒为 0** | 核心不区分「恢复的草稿」：草稿本来就持久存在（B1a），重启后仍是草稿，没有单独标记成「已恢复」的状态。这是「这个设计里没有这个概念」，不是「读不到」——所以没有把它塞进 `notes` 里当噪声 |
| **`provenance` 留空** | 桥接侧的 `SourceRevision` 没有这个字段（契约里它是可空）。等核心有了再映射，不编 |
| **`PlatformInt64` 用 `.toInt()`** | frb 的 `PlatformInt64` 在原生平台是 `int`、在 web 是 `BigInt`；`.toInt()` 两边都能编译，也顺手把 web 的可能性留着 |

## 核心侧补的两个数字

契约的 `CoreSnapshot` 要 `captureCount` 与 `lastEventSequence`，桥接原来不给。**填 0 对已有记录的资料库是假话**，所以补了两个真查询：

- `Core::count_captures()`：`COUNT(*) FROM captures WHERE state <> 'trashed'`——不含回收站，与默认检索口径一致；
- `Core::last_event_sequence()`：`COALESCE(MAX(sequence), 0)`。**不要**用「拉全部事件再取最后一条」，那会把整张事件表读进内存。

两个数字都进了 `LibraryInfo`（`captureCount`、`lastEventSequence`）并随 codegen 暴露给 Dart。

## 验证到了什么

`packages/diary_bridge/test/bridge_diary_api_test.dart`（3 项，**只经 `DiaryApi`**）：

- 开库 → 建草稿 → 保存（`durable`）→ 提交 → 重读 → 分页，字段逐一对上；提交后 `captureCount == 1`、`lastEventSequence > 0`（证明这两个数字真的从库里读出来，不是默认值）；
- 错误路径：查不存在的记录拿到 `DiaryErrorCode.notFound`（不是 `unknown`）；
- 没接的方法（`startSearch`）明确抛错，且不在 `capabilities` 里。

平台：Linux（WSL2 x86_64）宿主，真 cdylib + 真 SQLite。桥接自己的 7 项端到端测试同时回归（共 10 项）。

复现：

```bash
cargo build --release                 # 仓库根目录
cd packages/diary_bridge && dart test
```

## 没有做到的

- **不是全部契约方法都接了**：记录、导入/录音交接、提取、检索、Provider/回应、日记生成与版本、
  风格（`styles.list/get/save`）、天列表（`diary.listDays`）、阅读导出（`exports.create`）
  与队列驱动已接；回收站/清理、设置、备份票据化入口等仍未接，`capabilities` 里也没有它们。
- **前端还没切到真核心**：`apps/diary_app` 仍注入 `MockDiaryApi`。切换是 `main.dart` 那一处 + `demoMode: false`，但真核心需要**库路径、时区**，Android 还要把 `.so` 放进 app 并加载——这些正是 F1/`platform_host` 的活，还不存在。硬切会让应用起不来，所以这一片**刻意没有动前端目录**。
- **`trashed` 分支仍未端到端验证**：核心 v16 已经有 `captures.trash` / `restore` / `previewPurge` / `purge`（含集成测试），但这一层的适配器还没接这五个方法，所以 Flutter 侧仍然调不到；`state <> 'trashed'` 的证据目前只在核心集成测试里（关键词、语义、AI 材料三条）。
- **`mock` 与核心的初始 revision 不一致**：核心从 **1** 开始，`diary_mock` 从 **0** 开始（契约没规定起点）。前端逻辑不受影响（它用 `expectedRevision: 当前值`），但**照 mock 写的断言切到真核心会红**——我自己就在这条上翻过车。已单独记 issue。
- **Android / Windows 未验证**：只在该 Linux 宿主跑过；Android 上的 `.so` 加载在 B1b 的桥接探针里验过，但适配层本身没有设备证据。

## 覆盖率扩展（2026-10-20，`feat/1019-dart-bridge-coverage`）

契约 `DiaryApi` 现有 **76** 个方法（不是早期文档里的 17 个能力项）；适配层从本轮起点 **51** 条接到 **59 条**，新增 **8** 条。`test/bridge_diary_coverage_test.dart`（10 项，只经 `DiaryApi`）覆盖其中 8 条的端到端路径，`core.close` 由该文件的 `tearDownAll` 与每次重开前的释放路径走到；`test/bridge_diary_backup_api_test.dart`（4 项）覆盖**路径专用**的备份创建/检查/恢复（契约入口不宣告，见下）。

| 契约方法 | 实现路径 | 验证 |
|---|---|---|
| `core.close` | 直接释放桥接会话并清空引用；重复 `close` 幂等 | 每个测试的 `tearDownAll` 走真实 close；`open()` 重开前先释放旧会话 |
| `core.events` | 桥接新增 `events_since_page`（SQL `LIMIT` 分页）；适配层按 `eventBatchSize` 分批轮询，满批立即续读，空批才等轮询间隔 | 补读首条事件、订阅后轮询到新事件、跨多批有序且不重不漏、负游标 `invalid_argument`、未来游标与事件缺口报 `CursorExpiredException` |
| `assets.open` / `assets.release` | 桥接新增 `open_asset` / `release_asset`（codegen 2.13.0），核心校验 `usage` 与 `ready` 状态，租约限域 | 导入真原件 → 打开租约 → 句柄在库目录内 → 释放 → 重复释放 `not_found` |
| `sources.locate` | 桥接已有；适配层补 `SourceLocator` 双向映射（`rect` 四元组） | 导入+提取 → 定位到可打开原件；不存在的来源返回 `available=false` + 原因 |
| `indexes.status` | 桥接已有；`pendingCount` / `failedCount` 映射核心的待重建片段数与解析失败材料数 | 真库读覆盖；空库也能报 reasons |
| `indexes.rebuild` | 桥接已有；返回真实 `JobTicket`，任务可 `getJob` | 排 `all` 任务 → `getJob` 读到 `index`；同 operationId 幂等 |
| `jobs.acknowledgeAttention` | 桥接新增；核心写确认表 | 调用两次不抛错（幂等） |

**备份：契约三条未接，只有路径专用临时入口。** `backups.create` / `backups.inspect` / `backups.restore` 的契约入参是 `PlatformHost` 的**限域文件票据**，不保证是本地路径；`PlatformHost` 目前没有票据→路径的解析协议。适配层因此**不把票据当路径**：

| 入口 | 形态 | 说明 |
|---|---|---|
| `createBackupAtPath(String targetPath, {bool includeAssets = true})` | 路径专用，非契约 | 同步建包 → 写 `succeeded` 的 `backup` 任务行；`targetIds = [包路径]`；拒绝 `includeAssets = false` |
| `inspectBackupAtPath(String packagePath)` | 路径专用，非契约 | 同步检查 schema/hash/字节数；坏包进 `issues` |
| `restoreBackupAtPath({required String packagePath, required String targetDir, RestoreOptions options})` | 路径专用，非契约 | 同步恢复到新库 → 写 `succeeded` 的 `restore` 任务行；`targetIds = [libraryPath, rootDir]`；`restoreAsNewLibrary = false` 拒绝 |

这三条**不在** `_wiredMethods` / `capabilities` 里，契约方法 `createBackup` / `inspectBackup` / `restoreBackup` 会走 `noSuchMethod` 并点名。路径专用入口拒绝 `ticket:` / `file:` 等票据或 URI 形态（`invalidArgument`），不会在桌面端按字符串建文件；票据解析就绪后，契约入口应先在宿主侧解析，再调用这三条。测试见 `bridge_diary_backup_api_test.dart` 第 4 项：三种入口和 `targetDir` 各传 `ticket:...`，断言显式报错、无同名文件、无多余任务。

**同步完成的 `jobId`（不是排队任务）**：`createBackupAtPath` / `restoreBackupAtPath` 的核心入口是同步的，没有队列执行器。这里的做法与 `exports.create` 的先例一致——同步做完之后写一条 `state = succeeded` 的任务记录，返回的 `jobId` 指向这条**已经完成**的记录；`getJob` 立刻读得到，`targetIds` 是真实产物路径。这样返回形状不用改，也没有引入一个空壳执行器。未来做真正的异步备份时，替换的是实现，不是契约。**大库上这两个调用会占住线程**，界面应显示「正在备份/恢复」，不要拿 `jobId` 做进度轮询。

`createBackupAtPath` 的 `includeAssets = false` 明确拒绝：核心只做完整备份，不含原件就不是可恢复的备份。`restoreBackupAtPath` 的 `includeIndexes` 不改变核心的整库快照——`false` 不会剔除索引表，`true` 也不会额外重建；索引始终可以在恢复后重建。目标目录必须不存在或为空，非空/坏包/版本不兼容都在写任务行之前拒绝。

**仍没接的 12 条**（`noSuchMethod` 会实时列出；「核心」「桥接」两列是本次盘点实测）：

| 契约方法 | 核心 | 桥接 | 结论 |
|---|---|---|---|
| `backups.create` | 有（同步） | 有 | 契约收 `PlatformHost` 票据，票据解析协议缺失；见路径专用入口 |
| `backups.inspect` | 有（同步） | 有 | 同上 |
| `backups.restore` | 有（同步） | 有 | 同上 |
| `core.setRuntimeState` | 无 | 无 | **真的缺核心**：契约要上报前后台/压力供调度，核心没有对应入口；不造只记不做的实现，另立项 |
| `captures.moveDay` | 有（#155 已合入 main） | 有 | 回收站入口已在核心落地；适配层这一轮刻意不接，另开 PR 接 |
| `captures.trash` | 有 | 有 | 同上 |
| `captures.restore` | 有 | 有 | 同上 |
| `captures.previewPurge` | 有 | 有 | 同上 |
| `captures.purge` | 有 | 有 | 同上 |
| `settings.get` | 部分（只有 `OrganizeSettings`） | 无 | v18 设置线在做；本分支刻意不碰 |
| `settings.update` | 部分（只有 `OrganizeSettings`） | 无 | 同上 |
| `plugins.invoke` | 有（`run_plugin` 同步返回完整结果） | 有（`run_plugin`） | 契约返回 `jobId`，核心没有可读的插件任务结果；需契约/任务化另立项，不做假 jobId |

**能力清单**：适配层只报 `_wiredMethods` 与桥接 `wired_capabilities` 的交集；这 12 条都不在里面（包括备份三条）。#166 接上了风格三条（`styles.list` / `styles.get` / `styles.save`），#167 又接上了 `diary.listDays` 与 `exports.create`，所以它们从上面的差集里移除了；覆盖数 59/76 → 62/76 → **64/76**。

**验证层级**：本机 Linux（WSL2 x86_64、Rust 1.94.1）真 cdylib + 真 SQLite；`cargo clippy --workspace --all-targets -- -D warnings`、`cargo test --workspace`（572 通过 / 0 失败 / 8 忽略）、`dart analyze --fatal-infos` 都实跑；`dart test`（端到端）由 CI 的 `bridge` 任务跑，本机未跑。Windows/Android 真机未跑。

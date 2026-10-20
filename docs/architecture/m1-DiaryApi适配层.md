# M1 · DiaryApi 适配层

对应 issue [#38](https://github.com/xingxue-ux/unwritten-diary/issues/38) · 日期 2026-10-09 · 执行人 xingxue-ux

把桥接的 `BridgeSession` 适配成契约的 `DiaryApi`，让前端（F0，已合入）能经**契约接口**跑真核心。代码在 `packages/diary_bridge/lib/src/bridge_diary_api.dart`。桥接本身见 [M1 · 桥接接线](m1-桥接接线.md)。

> **2026-10-20 覆盖率更新（62/76）**：在记录路径之外，这一轮又接上了事件流、来源定位、资产租约、索引状态/重建、任务确认提醒、备份包检查等（详见文末「覆盖率扩展」）。下面「只接记录路径」「3 项测试」等段落是 2026-10-09 首版的现场记录，保留作为历史；当前口径以文末与 `packages/diary_bridge/README.md` 为准。

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

- **只覆盖记录路径**：其余契约方法（导入、提取、搜索、任务、录音…）未接，`capabilities` 里也没有它们。
- **前端还没切到真核心**：`apps/diary_app` 仍注入 `MockDiaryApi`。切换是 `main.dart` 那一处 + `demoMode: false`，但真核心需要**库路径、时区**，Android 还要把 `.so` 放进 app 并加载——这些正是 F1/`platform_host` 的活，还不存在。硬切会让应用起不来，所以这一片**刻意没有动前端目录**。
- **`trashed` 分支未验证**：回收站没有对外接口，`state <> 'trashed'` 这条只在代码里，没有端到端证据。
- **`mock` 与核心的初始 revision 不一致**：核心从 **1** 开始，`diary_mock` 从 **0** 开始（契约没规定起点）。前端逻辑不受影响（它用 `expectedRevision: 当前值`），但**照 mock 写的断言切到真核心会红**——我自己就在这条上翻过车。已单独记 issue。
- **Android / Windows 未验证**：只在该 Linux 宿主跑过；Android 上的 `.so` 加载在 B1b 的桥接探针里验过，但适配层本身没有设备证据。

## 覆盖率扩展（2026-10-20，`feat/1019-dart-bridge-coverage`）

契约 `DiaryApi` 现有 **76** 个方法（不是早期文档里的 17 个能力项）；适配层从本轮起点 **51** 条接到 **62 条**，新增 **11** 条。`test/bridge_diary_coverage_test.dart`（10 项，只经 `DiaryApi`）覆盖其中 8 条的端到端路径，`core.close` 由该文件的 `tearDownAll` 与每次重开前的释放路径走到；`test/bridge_diary_backup_api_test.dart`（3 项）覆盖备份创建/恢复。

| 契约方法 | 实现路径 | 验证 |
|---|---|---|
| `core.close` | 直接释放桥接会话并清空引用；重复 `close` 幂等 | 每个测试的 `tearDownAll` 走真实 close；`open()` 重开前先释放旧会话 |
| `core.events` | 桥接新增 `events_since_page`（SQL `LIMIT` 分页）；适配层按 `eventBatchSize` 分批轮询，满批立即续读，空批才等轮询间隔 | 补读首条事件、订阅后轮询到新事件、跨多批有序且不重不漏、负游标 `invalid_argument`、未来游标与事件缺口报 `CursorExpiredException` |
| `assets.open` / `assets.release` | 桥接新增 `open_asset` / `release_asset`（codegen 2.13.0），核心校验 `usage` 与 `ready` 状态，租约限域 | 导入真原件 → 打开租约 → 句柄在库目录内 → 释放 → 重复释放 `not_found` |
| `sources.locate` | 桥接已有；适配层补 `SourceLocator` 双向映射（`rect` 四元组） | 导入+提取 → 定位到可打开原件；不存在的来源返回 `available=false` + 原因 |
| `indexes.status` | 桥接已有；`pendingCount` / `failedCount` 映射核心的待重建片段数与解析失败材料数 | 真库读覆盖；空库也能报 reasons |
| `indexes.rebuild` | 桥接已有；返回真实 `JobTicket`，任务可 `getJob` | 排 `all` 任务 → `getJob` 读到 `index`；同 operationId 幂等 |
| `jobs.acknowledgeAttention` | 桥接新增；核心写确认表 | 调用两次不抛错（幂等） |
| `backups.inspect` | 桥接已有同步入口；`packageTicket` 暂按包路径映射（平台票据层未接） | 真核心生成备份 → 检查 schema/hash/字节数，`issues` 为空 |
| `backups.create` | 桥接新增 `create_backup_job`：同步建包 → 写 `succeeded` 的 `backup` 任务行 → 返回 `jobId`；`targetIds = [包路径]`。适配层拒绝 `includeAssets = false` | 真核心建包 → `getJob` 读到 `succeeded` / `backup` / 包路径 → 包可 inspect；已存在目标不覆盖；包内无密钥（core 测试逐条目解压） |
| `backups.restore` | 桥接新增 `restore_backup_job`：同步恢复到新库 → 写 `succeeded` 的 `restore` 任务行 → 返回 `jobId`；`targetIds = [libraryPath, rootDir]`。适配层默认恢复到当前库同级 `<库名>-restored-<微秒>`，`restoreBackupTo` 给平台/测试注入目标 | 恢复成功 → 新库可开、记录/正文在 → 现库 libraryId/记录数/正文不变；非空目标、坏包、`restoreAsNewLibrary = false` 拒绝且不写假任务、现库哈希不变 |

**同步完成的 `jobId`（不是排队任务）**：`backups.create` / `backups.restore` 的核心入口是同步的，没有队列执行器。这里的做法与 `exports.create` 的先例一致——同步做完之后写一条 `state = succeeded` 的任务记录，返回的 `jobId` 指向这条**已经完成**的记录；`getJob` 立刻读得到，`targetIds` 是真实产物路径。这样契约返回形状不用改，也没有引入一个空壳执行器。未来做真正的异步备份时，替换的是实现，不是契约。**大库上这两个调用会占住线程**，界面应显示「正在备份/恢复」，不要拿 `jobId` 做进度轮询。

`backups.create` 的 `includeAssets = false` 明确拒绝：核心只做完整备份，不含原件就不是可恢复的备份。`backups.restore` 的 `includeIndexes` 不改变核心的整库快照——`false` 不会剔除索引表，`true` 也不会额外重建；索引始终可以在恢复后重建。目标目录必须不存在或为空，非空/坏包/版本不兼容都在写任务行之前拒绝。

**仍没接的 14 条**（`noSuchMethod` 会实时列出；「核心」「桥接」两列是本次盘点实测）：

| 契约方法 | 核心 | 桥接 | 结论 |
|---|---|---|---|
| `core.setRuntimeState` | 无 | 无 | **真的缺核心**：契约要上报前后台/压力供调度，核心没有对应入口；不造只记不做的实现，另立项 |
| `captures.moveDay` | 无 | 无 | 等 PR #155 合入；不在本分支复制那份回收站代码 |
| `captures.trash` | 无 | 无 | 同上 |
| `captures.restore` | 无 | 无 | 同上 |
| `captures.previewPurge` | 无 | 无 | 同上 |
| `captures.purge` | 无 | 无 | 同上 |
| `diary.listDays` | 无 | 无 | 等 PR #159 合入 |
| `styles.list` | 无 | 无 | 等 PR #159 合入 |
| `styles.get` | 无 | 无 | 等 PR #159 合入 |
| `styles.save` | 无 | 无 | 等 PR #159 合入 |
| `exports.create` | 无 | 无 | 等 PR #159 合入 |
| `settings.get` | 部分（只有 `OrganizeSettings`） | 无 | v18 设置线在做；本分支刻意不碰 |
| `settings.update` | 部分（只有 `OrganizeSettings`） | 无 | 同上 |
| `plugins.invoke` | 有（`run_plugin` 同步返回完整结果） | 有（`run_plugin`） | 契约返回 `jobId`，核心没有可读的插件任务结果；需契约/任务化另立项，不做假 jobId |

**能力清单**：适配层只报 `_wiredMethods` 与桥接 `wired_capabilities` 的交集；这 14 条都不在里面。

**验证层级**：本机 Linux（WSL2 x86_64、Rust 1.94.1）真 cdylib + 真 SQLite；`cargo clippy --workspace --all-targets -- -D warnings`、`cargo test --workspace`、`dart analyze --fatal-infos`、`dart test`（66 项）都实跑。Windows/Android 真机未跑。

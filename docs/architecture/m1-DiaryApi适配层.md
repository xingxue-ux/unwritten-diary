# M1 · DiaryApi 适配层

对应 issue [#38](https://github.com/xingxue-ux/unwritten-diary/issues/38) · 日期 2026-10-09 · 执行人 xingxue-ux

把桥接的 `BridgeSession` 适配成契约的 `DiaryApi`，让前端（F0，已合入）能经**契约接口**跑真核心。代码在 `packages/diary_bridge/lib/src/bridge_diary_api.dart`。桥接本身见 [M1 · 桥接接线](m1-桥接接线.md)。

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
- **`trashed` 分支未验证**：回收站没有对外接口，`state <> 'trashed'` 这条只在代码里，没有端到端证据。
- **`mock` 与核心的初始 revision 不一致**：核心从 **1** 开始，`diary_mock` 从 **0** 开始（契约没规定起点）。前端逻辑不受影响（它用 `expectedRevision: 当前值`），但**照 mock 写的断言切到真核心会红**——我自己就在这条上翻过车。已单独记 issue。
- **Android / Windows 未验证**：只在该 Linux 宿主跑过；Android 上的 `.so` 加载在 B1b 的桥接探针里验过，但适配层本身没有设备证据。

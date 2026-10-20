# M6 · 核心的稳定 C ABI（#108 鸿蒙化的第一块）

对应 issue #108 · 分支 `feat/B8a-core-c-abi`（基于 `origin/main` = `e2ebb48`）·
执行人 qianxia · 2026-10-17

新增 `crates/diary_capi`（cdylib + staticlib + rlib）。`diary_core` 与 `diary_bridge`
**一行未改**：这一层只把核心已有的能力包成 C 接口，不放业务规则、不新增错误码。

口径同仓库其它文档：**实测** = 本机跑过并贴出输出；**未验证** = 明确标注；**判断** = 推断。

## 1 摘要（先看这个）

| # | 结论 | 强度 |
|---|---|---|
| 1 | 「核心 ↔ 界面」此前只有 `flutter_rust_bridge` 一条 Flutter 专用通道；现在多了一条**与宿主语言无关**的 C ABI，ArkTS/NAPI、CLI、离线测试都能调 | 实测（本机 `cargo test` 8/8 绿） |
| 2 | 接口面 = **6 个导出符号**（版本 / 打开 / 关闭 / 调用 / 释放字符串 / 读最近错误；句柄是不透明指针）；调用是**句柄 + JSON 进 / JSON 出** | 实测（`llvm-nm -D` 实测 6 个） |
| 3 | 错误码**只可能是核心既有 21 码之一**，桥面不新增码；空指针 / 非法 JSON / 未知方法 / 参数不合法 → `invalid_argument`，句柄已关闭 / 锁中毒 / panic → `invalid_state` | 实测（错误码透传用例） |
| 4 | **panic 绝不穿过 FFI**：每个入口 `catch_unwind`，panic 变成 `ok:false`；把 `catch_unwind` 拿掉做红证据 → 进程 `SIGABRT`（`panic in a function that cannot unwind`） | 实测（红证据见 §9.3） |
| 5 | 句柄是**地址令牌**：桥面从不解引用调用方传进来的指针 ⇒ 关闭后再用、野指针都是明确的错误，而不是 UB | 实测 |
| 6 | 同一路径共享一个核心（与 `diary_bridge` 的 `OPEN_LIBRARIES` 同口径），同一句柄可被多线程调用（核心串行） | 实测（跨线程用例） |
| 7 | 端到端（建草稿 → 保存 → 提交 → 抽取 → 检索命中 → 排索引任务 → `runDue` 跑掉 → 关闭）全走 JSON 面，8 条测试在 CI 可跑、不联网 | 实测（实录见 §9.2） |
| 8 | **鸿蒙可用性冒烟（只验「能编」）**：用 OHOS NDK 的 clang 编到 `aarch64-unknown-linux-ohos` **编过了**（1m25s，`.so` 9.59 MB、`NEEDED` 只有 `libc.so`、6 个符号都在）；**真机没验** | 实测（见 §10） |
| 9 | **本块没有做**：回调/事件流、零拷贝、ArkTS 侧 NAPI 包装、鸿蒙真机验证 | 明确未做（见 §11） |

**一句话**：核心现在有了一个**稳定、可版本化、不 panic、线程安全**的 C 入口；ArkTS 侧要接进来，
缺的只剩「一层 NAPI 包装 + 真机验证」——那是后面的事，不是这一块的事。

## 2 为什么现在做

1. **通道是 Flutter 专用的**：`flutter_rust_bridge` 的产物是 Dart 侧生成物 + FRB 运行时，
   ArkTS 拿不到（`packages/diary_bridge/lib/src/rust/frb_generated.dart` 就是那一整套绑定）。
   鸿蒙界面要调核心，必须有**不用 Dart** 的入口；
2. **早做便宜**：核心的错误码（契约第 7 节）、异步任务口径、幂等键都已经冻结，接口面很小；
   等到鸿蒙界面开工再抽，就要同时动 ArkTS、NAPI shim 和真机联调，返工面大得多；
3. **不动既有通道**：`diary_capi` 只依赖 `diary_core`，Flutter 那条路一行未改，
   两边调的是同一个核心、同一份模型、同一套错误码——不存在第二套业务规则。

**这一块不是「鸿蒙包装」**：它只交 C ABI 这一层。真正让 ArkTS 用起来，还需要
C++ NAPI shim + `index.d.ts` + HAP 打包（探针里已有可复用的做法，见
`docs/architecture/m6-鸿蒙可行性探针.md` §24.3）。

## 3 交付物与构建

| 项 | 值 |
|---|---|
| crate | `crates/diary_capi`（`crates/diary_capi/Cargo.toml`） |
| crate-type | `cdylib`（给 dlopen）、`staticlib`（静态链接的宿主）、`rlib`（本 crate 测试，同 `diary_bridge` 口径） |
| 依赖 | `diary_core`（path）+ `serde` / `serde_json` / `chrono`（**都是 workspace 里已有的**，不引入任何新 crate） |
| `Cargo.lock` | 只多一个 `[[package]] diary_capi`，没有新外部依赖 |
| workspace | 根 `Cargo.toml` 的 `members` 加了 `crates/diary_capi`（`cargo clippy/test/build --workspace` 因此覆盖它） |

```bash
# 本机（CI 同款）
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --release          # 产出 target/release/libdiary_capi.{so,a}
```

## 4 C ABI 面

### 4.1 导出符号（一共 6 个）

| 符号 | 签名 | 说明 |
|---|---|---|
| `diary_capi_version` | `char *diary_capi_version(void)` | 诊断用版本戳 `diary_capi/0.1.0 apiVersion=1.0`；**协议版本在信封的 `apiVersion` 里**，不用它协商 |
| `diary_open` | `handle *diary_open(const char *path)` | 打开/创建资料库；失败返回 `NULL`，原因在 `diary_last_error` |
| `diary_close` | `int diary_close(handle *h)` | `0` 成功；`1` 空指针；`2` 句柄无效或已关闭；`3` 桥面内部错误。重复关闭返回 `2`，不崩 |
| `diary_call` | `char *diary_call(handle *h, const char *method_json)` | 方法调用；**永远返回信封 JSON**（失败也是 `ok:false` 信封） |
| `diary_string_free` | `void diary_string_free(char *text)` | 释放本库返回的字符串；空指针是 no-op |
| `diary_last_error` | `char *diary_last_error(void)` | 本线程最近一条错误（JSON，与信封里的 `error` 同形），没有则 `NULL` |
| — | `handle` 是不透明指针 | C 侧只能原样传回，不要读它指向的内存 |

> `diary_last_error` 是「NULL 返回带不出原因」的唯一补救，也是本块在任务书点名的入口之外
> **唯一**新增的符号。语义与 C 的 `errno` / `GetLastError` 同类：**每个入口进入时清空、
> 失败时写入**，线程内有效。

### 4.2 请求

```json
{"method":"captures.saveDraft",
 "params":{"capture_id":"cap_…","text":"…","expected_revision":1,"operation_id":"op-1"}}
```

- `method` 必填，名字用**契约里的方法名**（`captures.saveDraft` / `sources.extract` /
  `search.start` / `jobs.runDue`…），不自造新名词；
- `params` 可省略（`null` 或 `{}` 等价）；
- **字段名以核心 Rust 参数名为准（snake_case）**，与响应 `data` 是同一套写法；
  契约表里的 camelCase 写法作为**别名**一并接受（`expectedRevision` / `operationId` /
  `sourceRef` / `budgetMs` / `timeZone`…）；
- 参数结构体带 `deny_unknown_fields`：拼错的字段直接报错，并列出可用字段名
  （实测：`unknown field `expected_revisions`, expected one of `captureId`, `capture_id`, …`）；
- 无参方法（`core.info` / `jobs.status`）**不接受任何字段**，给了就报错——不静默忽略。

### 4.3 响应（信封）

```json
{"apiVersion":"1.0","data":{…},"ok":true}
{"apiVersion":"1.0","error":{"code":"invalid_argument","message":"…","retryable":false},"ok":false}
```

- **成功与失败都带 `apiVersion`**（`apiVersion` = `diary_core::API_VERSION`，契约第 1.2 节口径）；
- `data` 是**核心模型 serde 出来的原样**（`Capture` / `DraftSaveResult` / `CommitResult` /
  `ExtractedContent` / `SearchSnapshot` / `Job` / `JobRunSummary` / `JobStatus`），
  桥面不另造 DTO、不改字段名；
- JSON 对象**字段顺序不保证**（`serde_json` 默认按 key 排序），调用方按 key 取值。

### 4.4 所有权

| 方向 | 规则 |
|---|---|
| 返回的 `char *` | **Rust 分配**（`CString::into_raw`），只能用 `diary_string_free` 释放；不能 `free()`、不能重复释放 |
| 传入的 `const char *` | **调用方拥有**，桥面只读、不释放、不修改 |
| 句柄 | 只能由 `diary_open` 产生、由 `diary_close` 释放 |

「字符串释放不泄漏」在测试里用线程内的活字符串计数断言（见 `crates/diary_capi/src/lib.rs`
的 `LIVE_STRINGS`，只在测试构建里存在）：借出去 N 个、释放 N 个之后计数必须回到原处。

### 4.5 生命周期

- **句柄 = 地址令牌**：`diary_open` 返回的是一块 1 字节堆分配的地址，桥面用**指针值**查登记表，
  **从不解引用**调用方传进来的指针。因此：
  - 「关闭后再用」→ `invalid_state`（明确错误），不是 UB；
  - 「传一个从来没开过的指针」→ 同样 `invalid_state`，**不崩**；
  - 「旧句柄误命中新句柄」不会发生：令牌**永不回收**，地址在进程内不复用。
    代价是每次成功打开留一块 1 字节墓碑（进程生命周期内），换的是「关闭后使用是定义良好的行为」；
- **同一路径共享一个核心**：登记表 `path → Weak<Mutex<Core>>`（与
  `diary_bridge::api::OPEN_LIBRARIES` 同口径），两次 `diary_open` 同一路径拿到**同一个核心**、
  两个不同句柄。否则两个 `Core::open` 会各跑一遍 `recover_on_open`，两个 `runDue`
  会各自领任务；
- **`diary_close` 只摘掉自己那一个句柄**：最后一个句柄关闭时核心才析构、SQLite 连接才释放
  （实测：最后一个句柄关闭后 `Weak::upgrade()` 拿不到核心，且能立刻重新打开）；
- 路径 key 会做「绝对化 + 父目录 canonicalize」，相对路径与绝对路径的调用方碰得到同一条目。

## 5 信封与错误码

### 5.1 错误码：就是核心那 21 个，一个不多

| 码 | 什么时候出现（本块可见的） |
|---|---|
| `not_found` | 目标不存在（例：`sources.extract` 给了一个不存在的 ref） |
| `invalid_state` | 当前状态不允许（例：往已提交的记录上 `saveDraft`；游标格式不对） |
| `invalid_argument` | 输入不合法（`scope` 编码、参数缺失…） |
| `revision_conflict` | 乐观锁不匹配（`expected_revision` 不对） |
| `idempotency_conflict` | 同一个 `operationId` 换了内容 |
| `storage_full` / `asset_missing` / `permission_denied` / `integrity_failed` | I/O、资产与完整性（本块未走到，核心原样给） |
| `unsupported_format` | 提取器不支持这个格式 |
| `search_expired` / `cursor_expired` | 检索会话/游标失效 |
| `corrupted_backup` / `incompatible_backup` | 备份包损坏/不兼容（`backups.inspect` 只把问题列进 `issues`；清单都读不出才抛 `corrupted_backup`，恢复时同码） |
| `provider_not_configured` / `authentication_failed` / `network_unavailable` / `rate_limited` / `quota_exceeded` / `provider_timeout` / `model_unavailable` | 远端服务与模型（`providers.test` / 任务执行；桥面透传） |

### 5.2 C ABI 边界条件 → 既有码（**不新增码**）

| 边界条件 | 码 | message 前缀 |
|---|---|---|
| `diary_open` / `diary_call` 收到空指针；`path` 为空；请求不是 UTF-8 | `invalid_argument` | `C ABI：` |
| `method_json` 不是合法 JSON / 缺 `method` / `method` 不是字符串 / 多余顶层字段 | `invalid_argument` | `C ABI：` |
| 未知（未接入）方法 | `invalid_argument` | `C ABI：未接入的方法 …（已接入：…）` |
| `params` 不合法（缺字段、拼错、类型不对） | `invalid_argument` | `C ABI：参数不合法（…）` |
| 无参方法收到参数 | `invalid_argument` | `C ABI：… 不接受参数（…）` |
| 句柄为空 / 无效 / 已关闭 | `invalid_argument`（空） / `invalid_state`（无效、已关闭） | `C ABI：` |
| 登记表锁中毒 | `invalid_state` | `C ABI：` |
| 核心 panic（被 `catch_unwind` 拦住） | `invalid_state` | `C ABI：panic 已被拦截（…）：…` |

**为什么 panic 不单开一个码**：契约第 7 节的码表是**双方共用的**，加一个 `internal_panic`
等于改契约（要同步改 Dart 枚举、适配层与文档）。这一块的口径是「桥面不新增码」，
监控要单独计数可以按 message 里的 `panic 已被拦截` 子串统计。**要不要升级成一个真码，
留给评审拍板（§12）**。

## 6 方法映射表

### 6.1 已接（38 条，`core.info.capabilities` 就是这份清单）

| 契约方法 | 核心调用 | `data` |
|---|---|---|
| `core.info` | `schema_version` + 恢复计数 + `count_captures` + `last_event_sequence` | 与 `diary_bridge::api::LibraryInfo` **同形**的对象 |
| `captures.createDraft` | `Core::create_draft` | `Capture` |
| `captures.saveDraft` | `Core::save_draft` | `DraftSaveResult` |
| `captures.commit` | `Core::commit` | `CommitResult` |
| `captures.get` | `Core::get_capture` | `Capture` |
| `captures.list` | `Core::list_captures`（`day_key` + 不透明游标） | `CapturePage` |
| `recordings.prepare` | `Core::prepare_recording` | `RecordingTicket` |
| `recordings.registerSegment` | `Core::register_segment` | `SegmentReceipt` |
| `recordings.updateState` | `Core::update_recording_state` | `RecordingSession` |
| `recordings.finalize` | `Core::finalize_recording` | `RecordingFinalizeResult` |
| `recordings.recover` | `Core::recover_recording` | `RecordingRecovery` |
| `sources.extract` | `Core::extract_source`（`source_ref` 可以是来源 id 或修订 id） | `ExtractedContent` |
| `sources.extractedContent` | `Core::extracted_content`（没提取过是 `null`） | `ExtractedContent \| null` |
| `sources.previousRevision` | `Core::previous_source_revision`（只有一版是 `null`） | `SourceRevision \| null` |
| `sources.reviseText` | `Core::revise_text`（新修订、不覆盖旧内容；`expected_revision` 乐观锁） | `SourceRevision` |
| `sources.locate` | `Core::locate_source`（`source_ref` + locator；打不开时 `available:false` + `reason`） | `SourceLocation` |
| `search.start` | `Core::start_search`（`{"request":{…},"query_revision":N}`） | `SearchSnapshot` |
| `search.nextPage` | `Core::search_next_page`（不透明游标） | `SearchSnapshot` |
| `search.upgrade` | `Core::upgrade_search_to_hybrid`（显式第二阶段；无证据不假装融合） | `SearchSnapshot` |
| `search.cancel` | `Core::cancel_search` | `SearchSnapshot` |
| `indexes.rebuild` | `Core::schedule_index_rebuild`（只排任务） | `Job` |
| `indexes.rebuildSemantic` | `Core::schedule_semantic_index`（只排一条整库语义任务） | `Job` |
| `indexes.status` | `Core::index_status`（可选 `source_scope`） | `IndexStatus` |
| `jobs.runDue` | `Core::run_due_jobs`（`budget_ms` + `trigger`；`now = Utc::now()`，条数上限 64，与桥上同一个常量口径） | `JobRunSummary` |
| `jobs.status` | `Core::job_status` | `JobStatus` |
| `jobs.list` | `Core::list_jobs_page`（状态筛选 + 不透明游标） | `JobPage` |
| `jobs.get` | `Core::get_job` | `Job` |
| `jobs.retry` | `Core::retry_job`（给了 `operation_id` 时走 `retry_job_idempotent`） | `Job` |
| `jobs.cancel` | `Core::cancel_job` | `Job` |
| `jobs.acknowledgeAttention` | `Core::acknowledge_attention` | `null` |
| `providers.save` | `Core::save_provider` | `Provider` |
| `providers.list` | `Core::list_providers` | `Provider[]` |
| `providers.test` | `Core::test_provider` | `ProviderTestReport` |
| `providers.remove` | `Core::remove_provider` | `null` |
| `backups.create` | `Core::create_backup`（同步入口；目标路径必须是沙箱内，已存在则拒绝覆盖） | `BackupReport` |
| `backups.inspect` | `Core::inspect_backup`（同步入口；逐条目重算哈希，问题进 `issues`、不抛错） | `BackupInspection` |
| `backups.restore` | `Core::restore_backup`（同步入口；只恢复到新库，来源/目标都必须沙箱内） | `RestoreReport` |
| `core.events` | `Core::events_page`（按序号轮询持久事件，一页一次 SQL `LIMIT`，见 §6.1.2） | `{events,next_sequence,truncated}` |

`search.start` 的 `request` 里只有 `query` 必填，`mode`（默认 `keyword`；`semantic` 是
语义那一路）、`filters`（全字段可选）、`page_size`（默认 20，与 `bridge_diary_api.dart`
同口径）都可省。

### 6.1.1 分页方法：游标是**真游标**，不是「传了就报错」

三条分页方法都把核心的**不透明游标**原样透传，没有把「只接第一页」当成已接：

| 方法 | 参数 | `data` | 游标语义 |
|---|---|---|---|
| `captures.list` | `day_key?` / `cursor?` / `limit?`（默认 20，核心夹 1–100） | `CapturePage` | 键集 `(occurred_at DESC, id DESC)`；`next_cursor` 原样回传 |
| `jobs.list` | `states?` / `cursor?` / `limit?`（默认 20，核心夹 1–200） | `JobPage` | 键集 `(priority DESC, created_at, id)`；`next_cursor` 原样回传 |
| `search.nextPage` | `session_id` / `cursor?` | `SearchSnapshot` | 会话内偏移 + 结果代次；省略 `cursor` = 继续用会话记着的下一页 |

`jobs.list` 的游标是这一片给核心加的 `Core::list_jobs_page`：原有 `list_jobs(states, limit)`
只能取「前 N 条」（`limit` 还夹在 200），没法越过第 200 条。新入口与它同排序、同状态筛选，
但游标是**键集**（上一页最后一条的三元组），翻页期间入队的新任务不会让后面的页漏项或重项；
`list_jobs` 原样保留，既有调用方不受影响。

`search.upgrade` 是显式的第二阶段：`start(mode=hybrid)` 只算关键词那一阶段并返回
`keyword_ready`；升级后返回同一会话的 `hybrid_ready`，游标重置为融合后第一页，升级前的
游标一律 `cursor_expired`。语义拿不出可融合的证据时**不**声称 `hybrid_ready`（仍是
`keyword_ready` + `warnings`）。

`indexes.rebuildSemantic` 是契约 4.4 已有、核心与桥接早就接上的方法，C ABI 之前漏了它
（鸿蒙上就没有任何办法建语义索引）。参数只有 `reason` + `operation_id`（没有 `scope`：代次
与向量是全局的），返回 `Job`（只排队，跑还是 `jobs.runDue`）。

`core.info` 在契约字段之外多一格 **`runtime_config`**（加性扩展，`LibraryInfo` 没有它）：
配置文件状态、语义与转写两条能力各自的 `state` / `configSource` / 每个路径字段的来源 /
就绪原因。键是 camelCase（与 `core-config.json` 同形），只 stat、不加载模型。设计与真机
证据见 [M6 · 设备配置与双 ORT](m6-设备配置与双ORT.md)。

**两条超出任务书六条的方法，各有理由（不是凑数）**：

1. `captures.commit`：草稿本身**没有来源**，不 commit 就没有 `source_id` / `revision_id`
   可抽取 —— 任务书要求的端到端链路（建草稿 → 保存 → 抽取）缺了它走不通；
2. `indexes.rebuild`：其余已接方法里**没有别的办法往队列里放任务**，不接它 `jobs.runDue`
   永远只能返回「什么都没跑」的摘要，等于没法证明任务这一路是通的。

两条都只「排任务 / 落数据」，不引入任何新语义。**要不要删，评审说了算（§12）**。

### 6.1.2 `core.events`：请求/响应下的**按序号轮询**

C ABI 没有回调/推送面，所以 `core.events` 不装成 Stream，而是把核心**持久事件表**当游标读：

```json
{"method":"core.events","params":{"from_sequence":0,"limit":200}}
// data：
{"events":[{"event_id":"evt_…","sequence":1,"type":"capture.changed",
            "entity_id":"cap_…","revision":2,"emitted_at":"…"}],
 "next_sequence":1,"truncated":false}
```

- **不需要订阅**：核心在每次写事务里就 `insert_event` 落 `domain_events`（提交、保存、
  任务状态变化、日记版本…），不是「订阅后才产生」。因此桥面**不建订阅、不缓存**，
  读到的就是库里真实事件；进程重启也不丢游标。
- **`from_sequence` 省略** = 从**当前最新**序号开始（只等下一条，不补历史）；
  显式给 `0` 才是从头补。要「先拿游标再等新事件」就省略它。
- **`next_sequence`** 是下一次调用的 `from_sequence`：有事件时是本页最后一条的序号，
  没事件时是本次的起点；**调用方必须原样回传**，不要自己 +1。
- **`limit`** 默认 200、上限 1000。**有界读在核心**：`Core::events_page` 的 SQL 是
  `… WHERE sequence > ?1 ORDER BY sequence LIMIT ?2`，只多读一条用来回答 `truncated`
  （`truncated:true` 就继续按 `next_sequence` 拉，直到 `false`）。桥面不再「先读全部
  再截断」——`from_sequence: 0` 在十万条事件的库上也只有一页进内存。
- **积压上限与保留策略**：`domain_events` 只保留最近 `EVENT_BACKLOG_LIMIT = 10000` 条。
  清理发生在**每次写入的同一个事务**里（删掉窗口外的行），所以表不会无限长大；事件是
  通知流不是账本，重放从当前快照开始。
- **游标超前**（大于当前最大序号）：**夹到当前最大序号**，不报错、不返回未来序号。
  这是真实场景——调用方存了游标、库被恢复成更旧的备份；夹住之后下一次调用就能收到
  新事件，不会永久跳过。
- **游标早于保留窗口**（中间的事件已被清理）：返回 `cursor_expired`，调用方按契约
  第 6 节重读快照。不能假装「没有遗漏」——那会让界面永远缺一段状态。
- **事件类型**按线上字符串给（`capture.changed`，不是 serde 的 `CaptureChanged`）：
  核心 `EventType` 没有 serde rename，桥面显式投影一次，与契约 `DomainEventType` 对齐。

### 6.1.3 备份与恢复：同步入口 + 沙箱路径

`backups.*` 是契约那三行的**同步版本**（任务化 #70 不在本块）：返回真实报告，不是 `jobId`。

- **票据不是路径**：契约的 `targetTicket` / `packageTicket` 是 `PlatformHost` 的限域
  票据，宿主必须先解析成沙箱内路径再传 `target_path` / `package_path`。C ABI 把这两个
  票据字段**当错误处理**（`invalid_argument`，消息点名「票据不是文件路径」），不再把它们
  当路径别名——否则桌面端会按票据字符串真的建文件，OHOS 上则被沙箱判定拒绝并给出误导
  性的错误。`targetPath` / `packagePath` 这类**路径**别名仍然接受。
- `backups.create`：`target_path`（包文件完整路径，父目录会建）；已存在拒绝覆盖；
  `include_assets:false` 明确 `invalid_argument`（核心只做完整备份）。
- `backups.inspect`：`package_path`；逐条目哈希/大小/路径安全/版本兼容，坏在哪进
  `issues`（`corrupted` / `unsafe_path` / `incompatible`），只有清单都读不出才抛
  `corrupted_backup`。
- `backups.restore`：`package_path` + `target_dir`（**新的空目录**）+ 契约的
  `options`。`restore_as_new_library:false` 明确 `invalid_argument`（绝不覆盖当前库）；
  `include_indexes` 只做形状兼容（核心恢复的是快照整体，本来就有索引/向量）。
- **沙箱路径**：OHOS 上 `target_path` / `package_path` / `target_dir` 必须是应用数据目录
  （`/data/storage/el2/…`、`/data/storage/el1/base|database|cache/…`、`/data/app/el2/…`、
  `/data/app/el1/base/…`——与 `OHOS_APP_DATA_PREFIXES` 的六条前缀逐条对应），
  且不能含 `..`；否则 `invalid_argument`。复用 `asrEngineLib` 的同一个前缀判定
  （`is_ohos_app_data_path`），**方向相反**：原生库必须在 HAP 安装目录（沙箱 `.so` 不能
  dlopen），备份包必须在应用数据目录（安装目录只读）。其它平台不限制。
- **`BackupCounts` 的键是 camelCase**（`sourceRevisions` / `importsInFlight` /
  `openRecordings`，核心 `#[serde(rename_all = "camelCase")]`），外层 `BackupReport`
  等仍是 snake_case——ArkTS 的 `Models.ets` 照实声明。


### 6.2 扩展点：再加一个方法要动哪里

1. `crates/diary_capi/src/methods.rs` 的 `dispatch` 里加一条分支，并在同文件顶部的表里加一行；
2. 参数结构体写在同文件「参数」一节：`#[derive(Deserialize)]` + `deny_unknown_fields`
   + camelCase 别名；
3. 方法名加进 `WIRED_METHODS`（它同时是 `core.info.capabilities` 的内容——**诚实的能力清单**：
   没列出来的就是还没接，界面不该显示成可用）；
4. `src/tests.rs` 加用例（至少一条成功路径 + 一条参数错误）。

**还没接的（按契约第 4 节分组，代码里带 `TODO(#108)`）**：`captures.moveDay/trash/
restore/previewPurge/purge`、`styles.*`、`exports.create`——**核心还没有对应入口**
（`trash_capture` / `preview_purge` / `purge_capture` / `move_day` / `get_style` /
`save_style` / `list_styles` / `create_export` 全仓零命中），不是桥面漏接；另开核心线
（回收站状态机 / styles 表 / export）。其余：`imports.*`、`assets.open/release`、
`search.snapshot`、`jobs.nextWakeup`、`core.snapshot/close/setRuntimeState`、`diary.*` /
`responses.*` / `settings.*` / `plugins.*`——要么还没接，要么属于后面的块。
`providers.*` 四条、`sources` 正文 5 条、`backups.*` 三条、`core.events` 都已接（见上表）。

未接入的方法一律 `invalid_argument` + 已接入清单，**不静默成功、不假装支持**。

## 7 线程模型

```
diary_call(h, json)
  └─ 登记表 Mutex<HashMap<指针值, {Arc<Mutex<Core>>, library_id}>>   ← 只锁一瞬，取完就放
       └─ Arc<Mutex<Core>>                                          ← 核心串行在这一把锁后面
```

- **同一句柄可以任意线程调**：桥面不持有「当前线程」之类的状态，除线程内的 last-error 外无
  共享可变状态；
- **登记表的锁不跨核心调用持有**：否则一次整库索引重建会把所有 `diary_open` / `diary_close`
  卡住（唯一一处例外见下）；
- **`diary_open` 例外地跨 `Core::open` 持有登记表锁**：两个线程同时首次打开同一路径时，
  不能各自开一个核心（会跑两遍恢复、两个驱动）。这与 `diary_bridge` 的做法一致；
- **核心串行**：核心本身不是并发安全的（`Connection`、内存态检索会话），所有调用都排在那把
  `Mutex` 后面。桥面**不做**并发优化，也不假装能并行；
- **锁中毒按「取回内部值」处理**：核心如果 panic，那把锁会中毒；桥面 `PoisonError::into_inner()`
  继续用，**不把中毒变成第二次 panic**，也不让句柄变成砖头（写路径的事务在 unwind 时已回滚，
  由 `Core` 自己的事务边界兜着）。实测：panic 注入之后同一个句柄照常 `jobs.status`。

## 8 FFI 安全怎么保证的

| 风险 | 处置 |
|---|---|
| **panic 穿过 FFI**（`extern "C"` 里逃出去的 panic 直接 abort 进程） | 每个入口 `catch_unwind(AssertUnwindSafe(...))`，panic → `ok:false` + `invalid_state`；`diary_close` 用自己的壳返回 `3` |
| **空指针** | 所有裸指针入参都查 `is_null()`（句柄、`method_json`、`path`），报明确错误 |
| **非法 UTF-8** | `CStr::to_str()` 失败 → `invalid_argument`（实测用例覆盖） |
| **悬垂 / 野句柄** | 句柄只当**地址令牌**用，桥面不解引用 ⇒ 查不到就是 `invalid_state`；令牌永不回收，地址不复用 |
| **字符串所有权** | 返回值一律 `CString::into_raw` + 只能 `diary_string_free`；测试用活字符串计数断言不漏 |
| **返回值里出现内嵌 NUL** | `to_c_string` 过滤 `\0` 后再 `CString::new`，不会因为一条消息把返回值变成 NULL |
| **`serde_json` / 核心 panic** | 都在 `catch_unwind` 覆盖范围内（panic 注入用例打的就是「核心内部 panic」这条路径） |
| **未定义行为面** | 只剩「给一个非空但不是 NUL 结尾的 `const char *`」这一条（C 字符串的固有约定），文档里写明；指针本身是野指针也不会崩 |

## 9 测试

### 9.1 FFI 边界与生命周期用例（8 条；`crates/diary_capi/src/tests.rs`，CI 可跑、不联网）

本批新增 2 条用例（`locate available:true`、`backups.inspect` 的损坏包 `issues`），
其余在既有用例里补断言（`previousRevision` / `locate` 的错误路径、`core.events` 的超前
游标、capabilities 与 `WIRED_METHODS` 逐项相等、票据字段拒绝）；当前共 **24 条**。下表
只列最初那 8 条 FFI 边界/生命周期用例。

| 用例 | 覆盖 |
|---|---|
| `version_and_last_error_travel_through_the_c_api` | 版本戳、失败写 last-error、成功清 last-error |
| `bad_input_never_crashes_and_always_reports_an_error` | 空句柄 / 空请求 / 野指针 / 非法 UTF-8 / 非法 JSON / 缺 method / 多余顶层字段 / 未知方法 / 参数拼错 / 必填缺失 / 无参方法收到参数 / 打开失败（NULL + last-error）/ 重复关闭 |
| `core_error_codes_pass_through_unchanged` | `not_found` / `revision_conflict` / `idempotency_conflict` 三条核心码原样透传（含 message） |
| `end_to_end_draft_to_search_to_jobs` | 打开 → `core.info` → 建草稿 → 保存 → 提交 → 抽取 → 检索命中 → `indexes.rebuild` → `jobs.status` → `runDue`（processed=1/succeeded=1）→ `jobs.status` → 关闭；含 `createDraft` 幂等重放 |
| `close_releases_the_core_only_after_the_last_handle` | 同路径共享核心（`Arc::ptr_eq` + 强引用计数）、关闭后再用报错、最后一个句柄关闭后 `Weak` 拿不到、能重新打开 |
| `returned_strings_are_freed_by_diary_string_free` | 成功与失败路径的字符串都能释放干净（活字符串计数回原处）、`free(NULL)` no-op |
| `core_panic_becomes_an_error_and_the_handle_survives` | panic 注入（`__test.panic`，**只在测试构建里存在**）→ `ok:false`、进程不中止、句柄之后仍可用 |
| `two_threads_can_share_one_handle` | 两个线程并发调同一句柄（读检索 + 读状态 + 并发写一次）→ 不崩、结果正确（revision 1 → 2、`durable=true`） |

> `__test.panic` 用 `#[cfg(test)]` 挂在 `dispatch` 的一个分支上：**发布产物里没有这个入口**，
> 不是「留了一个能崩的开关」。

### 9.2 端到端实录

抓法：`DIARY_CAPI_TRANSCRIPT=1 cargo test -p diary_capi -- --nocapture`（下面第一段为省篇幅把长 id /
长数组截成 `…`，字段名与结构原样；第二段只按显示需要换行，消息文本原样）。

```
>>> {"method":"core.info","params":null}
<<< {"apiVersion":"1.0","data":{"api_version":"1.0","build_version":"0.1.0",
    "capabilities":["core.info","captures.createDraft","captures.saveDraft","captures.commit",
    "sources.extract","search.start","indexes.rebuild","jobs.runDue","jobs.status"],
    "capture_count":0,"data_schema_version":8,"last_event_sequence":0,"library_id":"library",
    "recovery":{"notes":[],"open_recordings":0,"pending_jobs":0,"recoverable_imports":0}},"ok":true}

>>> {"method":"captures.saveDraft","params":{"capture_id":"cap_01a14a7f…",
    "expected_revision":1,"operation_id":"op-capi-save","text":"今天在鸿蒙上验证核心的 C ABI，关键词是鹅掌楸与检索。"}}
<<< {"apiVersion":"1.0","data":{"durable":true,"revision":2,"saved_at":"2026-10-17T15:33:57.630Z"},"ok":true}

>>> {"method":"search.start","params":{"query_revision":1,
    "request":{"mode":"keyword","page_size":10,"query":"关键词"}}}
<<< {"apiVersion":"1.0","data":{"cursor":null,"index_coverage":"complete","phase":"done",
    "query_revision":1,"results":[{"hit_id":"seg_01a14a80…","group_id":"src_01a14a80…",
    "matched_by":["keyword"],"snippet":"今天在鸿蒙上验证核心的 C ABI，关键词是鹅掌楸与检索。",
    "highlights":[{"end":21,"start":18}],"source_kind":"text"},{"hit_id":"cap_cap_01a14a7f…",
    "group_id":"cap_01a14a7f…","matched_by":["keyword"],"snippet":"今天在鸿蒙上验证核心的 C ABI…"}],
    "session_id":"search_01a14a8004af72b78f660abe43b1a9c8","warnings":[]},"ok":true}

>>> {"method":"indexes.rebuild","params":{"operation_id":"op-capi-index","reason":"capi e2e","scope":"all"}}
<<< {"apiVersion":"1.0","data":{"attempt_count":0,"id":"job_01a14a80…","kind":"index",
    "state":"queued","target_ids":[],"max_attempts":5,"priority":10,"requires_user_action":false},"ok":true}

>>> {"method":"jobs.runDue","params":{"budget_ms":30000,"trigger":"manual"}}
<<< {"apiVersion":"1.0","data":{"backfilled":0,"dependency_failed":0,"due_backlog":0,
    "failed":0,"next_wakeup":null,"processed":1,"recovered":0,"retrying":0,
    "started_at":"2026-10-17T15:33:58.094915204Z","succeeded":1,"trigger":"manual",
    "waiting_dependency":0},"ok":true}
```

错误面实录（同一开关抓的）：

```
>>> {"method":"captures.purge","params":{"id":"cap_1"}}
<<< {"apiVersion":"1.0","error":{"code":"invalid_argument","message":"C ABI：未接入的方法
    `captures.purge`（已接入：core.info、captures.createDraft、…、jobs.status）","retryable":false},"ok":false}

>>> {"method":"sources.extract","params":{"source_ref":"src_不存在"}}
<<< {"apiVersion":"1.0","error":{"code":"not_found","message":"找不到来源修订：src_不存在","retryable":false},"ok":false}

>>> {"method":"__test.panic","params":null}
<<< {"apiVersion":"1.0","error":{"code":"invalid_state","message":"C ABI：panic 已被拦截
    （diary_call，进程未中止）：diary_capi 测试钩子：注入的 panic","retryable":false},"ok":false}
```

### 9.3 能红证据（真的做过，不是推断）

**把 `guard_text` 里的 `catch_unwind` 换成一个直接调用**（其余一个字不改），跑 panic 注入用例：

```
thread 'tests::core_panic_becomes_an_error_and_the_handle_survives' panicked at crates/diary_capi/src/methods.rs:178:13:
...
thread '...' panicked at library/core/src/panicking.rs:225:5:
panic in a function that cannot unwind
...
thread caused non-unwinding panic. aborting.
error: test failed, to rerun pass `-p diary_capi --lib`
Caused by:
  process didn't exit successfully: `.../diary_capi-5e2302cf4ddae7ad core_panic --exact ...` (signal: 6, SIGABRT: process abort signal)
```

⇒ **`catch_unwind` 不是装饰**：没有它，宿主进程会被一次核心 panic 直接带走（SIGABRT）。
恢复后同一用例绿（8/8）。

其它能红的点（改坏哪一处会红）：

| 改坏什么 | 哪条用例会红 |
|---|---|
| `to_c_string` / `diary_string_free` 的所有权（比如忘了 `fetch_sub`） | `returned_strings_are_freed_by_diary_string_free` |
| 句柄改成「解引用后再查表」 | 「野指针」那一档会从 `invalid_state` 变成崩溃（用例会崩而不是断言失败，正是要防的） |
| 去掉「同一路径共享核心」 | `close_releases_the_core_only_after_the_last_handle` 的 `Arc::ptr_eq` / 强引用计数 |
| 登记表的锁跨核心调用持有 | 跨线程用例里 `core.info` 会排队等索引重建，`hits` 断言与耗时都能看出来（当前不持有） |
| 无参方法改成忽略参数 | `bad_input…` 的「不接受参数」断言 |

## 10 鸿蒙可用性冒烟（只验「能编」）

配方见 `docs/architecture/m6-鸿蒙可行性探针.md` 附录 A 与 §3：只需 `rustup target add
aarch64-unknown-linux-ohos` + 三个环境变量指向 OHOS NDK 的 clang / llvm-ar，**不需要 nightly、
不需要 build-std、不改 `crates/**`**。

```bash
export NDK=~/command-line-tools/sdk/default/openharmony/native
export CARGO_TARGET_DIR="$HOME/dt-capi-ohos"          # 独立 target 目录，不用 /tmp
export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_OHOS_LINKER="$NDK/llvm/bin/aarch64-unknown-linux-ohos-clang"
export CC_aarch64_unknown_linux_ohos="$NDK/llvm/bin/aarch64-unknown-linux-ohos-clang"
export AR_aarch64_unknown_linux_ohos="$NDK/llvm/bin/llvm-ar"
cargo +1.94.1 build -p diary_capi --release --target aarch64-unknown-linux-ohos
```

### 10.1 结论（实测）

| 项 | 结果 |
|---|---|
| 能不能编过 | **能**。`Finished \`release\` profile [optimized] target(s) in 1m 25s`（16 核，`CARGO_BUILD_JOBS=4`） |
| 目标 ABI | `aarch64` / OHOS（tier-2 官方 std，`rustup target add` 即可，不需要 nightly / build-std） |
| 环境变量 | **只要探针附录 A 那三个**（linker / `CC_aarch64_unknown_linux_ohos` / `AR_aarch64_unknown_linux_ohos`）；**没有**额外加 `CXX_*`（`tokenizers` 那条 C++ 依赖在这条路径上没被编到 C++） |
| `libdiary_capi.so` | `ELF 64-bit LSB shared object, ARM aarch64, version 1 (SYSV), dynamically linked, stripped`，**9,593,640 B** |
| `libdiary_capi.a` | `current ar archive`，100,731,982 B（静态链接宿主用；含全部依赖，所以大） |
| 动态依赖 | `llvm-readelf -d`：`NEEDED` **只有 `libc.so`**（没有 `libc++_shared.so`）——与探针 §24.1 的坑不冲突：这一份没有链 C++ 运行时 |
| 导出符号 | `llvm-nm -D --defined-only` 实测 6 个全在：`diary_call` / `diary_capi_version` / `diary_close` / `diary_last_error` / `diary_open` / `diary_string_free` |
| profile | 与产品一致（`opt-level=z` + LTO + `codegen-units=1` + strip），所以体积是**可比的** |
| 复现命令 | 见上面那段（`CARGO_TARGET_DIR="$HOME/dt-capi-ohos"`，不用 `/tmp`） |

> 体积口径提醒：探针里那份「核心」`.so` 是 3.93 MB，这一份是 **9.59 MB**——多出来的是
> 本 crate 走 `diary_core` 全量依赖（`pdf-extract`/`lopdf`、`jieba-rs` 词典、`tokenizers`、
> `zip`）而探针只带了核心的一部分路径。**HAP 体积账要按 9.59 MB 这一档重算**（见
> `m6-鸿蒙可行性探针.md` §29 的体积账）。

### 10.2 这一步**没有**验证的

真机加载（`dlopen` + `dlsym` 拿这 6 个符号）、沙箱里建库/落盘、NAPI 包装、HAP 打包与安装
——**全部未做**。探针验过的是探针自己那份薄 ABI（`diary_smoke_run`），**不是这一份**。

## 11 没有做到的（如实）

1. **回调 / 事件流**：没有推送通道，只有请求-响应。`core.events` 这一批按**序号轮询**
   接了（§6.1.2：读持久事件表，`from_sequence` + `next_sequence`），但**没有**「事件发生时
   主动唤醒宿主」的回调/事件泵；宿主仍要定时轮询或按平台调度唤醒；
2. **零拷贝**：所有进出都是 JSON 字符串，正文/大对象要序列化 + 复制一遍。
   本块**没有量**长正文（比如 10 KB 草稿、检索 100 条命中）的往返开销；
3. **ArkTS 侧 NAPI 包装**：没有 C++ shim、没有 `index.d.ts` / `oh-package.json5`、
   没有 `externalNativeOptions`。探针 §24.3 的三件套可以直接复用，但要在鸿蒙工程里做；
   **已在第二块补上：见 [m6-ArkTS-NAPI.md](m6-ArkTS-NAPI.md)——shim + 类型声明 +
   `DiaryCore` 薄封装 + 参考工程，本机能编、HAP 能打；真机仍未验**；
4. **鸿蒙真机验证**：只验到「能交叉编过」。`dlopen` 能不能拿到符号、沙箱里能不能建库、
   SQLite 能不能落盘——**全部未验证**（探针验过的是探针自己那份薄 ABI，不是这一份）；
5. **方法子集**：只接了 38 条（§6.1；回收站 5 / styles 3 / 导出 1 共 9 条**核心还没有
   实现**，见 §6.2），其余仍是扩展点；
6. **性能**：没测吞吐、没测 JSON 序列化占比、没测大库（十万条记录）下的表现；
7. **并发压力**：跨线程用例是 15 次循环的**正确性**验证，不是压测（没有 P50/P99）；
8. **错误 message 的语言**：桥面判定的是中文，`serde_json` 的解析错误是英文
   （`missing field \`budget_ms\``）——两种混在一条消息里。要统一得自己写解析错误映射；
9. **`invalid_state` 兼表 panic**：见 §5.2，监控只能靠 message 子串区分；
10. **令牌墓碑**：每次成功打开留 1 字节（进程内不回收）。长时间反复开关库的场景会有
    极小的常驻增长（1 万次 ≈ 十几 KB），**未做**压力验证；
11. **`diary_last_error` 是线程内的**：跨线程读不到别人的错误（这是有意的，但文档要说清）；
12. **没有把 OHOS 交叉编译加进 CI**：CI 目前只跑本机 `clippy` / `test` / `build --release`。
    要进 CI 得先决定 runner 上有没有 OHOS NDK（或把它做成一个可选 job）。

## 12 需要拍板的地方

1. **JSON 面够不够用，还是将来要上真正的 NAPI 包装？**
   - 现状：ArkTS 侧要用，需要自己写一层 NAPI shim（`napi_create_string_utf8` →
     `diary_call` → 解析 JSON）。**跨语言调用本身没问题，成本在「JSON 拼装 + 解析」**；
   - JSON 面的好处：**接口面稳定、可版本化、任何语言 10 行就能接**、不需要为每个方法写 NAPI 胶水；
     坏处：正文与检索结果的序列化/复制开销、ArkTS 侧要自己解 JSON（没有类型安全）；
   - 真 NAPI 包装（每个方法一个 `napi_value`，字段级映射）好处是类型安全与零解析，
     代价是**每个方法都要写胶水**、每加一个方法动一次 shim，而且要跟着契约演化维护两套签名；
   - **建议**：先按 JSON 面走（本块），ArkTS 侧只写一层通用 shim + TypeScript 类型声明；
     等量到「大正文往返」或「检索首屏延迟」真的被 JSON 拖住，再为热点方法加专用 NAPI 入口。
     **这个判断需要产品侧给一个量级**：单条记录正文的典型/最大长度是多少。
2. **方法子集要不要收窄**：`captures.commit` 与 `indexes.rebuild` 是本块为了「端到端能跑通」
   加的（§6.1），如果评审认为超范围，删掉各是一行映射表 + 一个用例。
3. **panic 要不要一个真错误码**（`internal_panic`）：现在是 `invalid_state` + message 前缀，
   不新增码（§5.2）。要独立码就得改契约第 7 节（Dart 枚举、适配层同步改）。
4. **字段命名的口径**：现在「snake_case 主名 + 契约 camelCase 别名」都收。若要只留一种，
   说一声即可（别名各一行，删掉不影响主名）。
5. **OHOS 交叉编译要不要进 CI**：见 §11 第 12 条。
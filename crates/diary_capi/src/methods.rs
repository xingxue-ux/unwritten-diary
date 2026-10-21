//! 方法映射表：契约方法名 → 核心调用。
//!
//! 这一层**不放业务规则**，只做三件事：把 JSON 参数转成核心入参、调核心、把核心结果
//! 原样序列化成 `data`（类型就是 `diary_core::model` 里那些，桥面不另造 DTO）。
//!
//! # 已接入（本块的最小可用集）
//!
//! | 方法（契约名） | 核心调用 | `data` |
//! |---|---|---|
//! | `core.info` | `schema_version` / `count_imports_in_flight` / `count_open_recordings` / `count_jobs` / `count_captures` / `last_event_sequence` | 与 `diary_bridge::api::LibraryInfo` 同形的对象 |
//! | `captures.createDraft` | `Core::create_draft` | `Capture` |
//! | `captures.saveDraft` | `Core::save_draft` | `DraftSaveResult` |
//! | `captures.commit` | `Core::commit` | `CommitResult` |
//! | `captures.get` | `Core::get_capture` | `Capture` |
//! | `captures.list` | `Core::list_captures`（`day_key` + 不透明游标） | `CapturePage` |
//! | `recordings.prepare` | `Core::prepare_recording` | `RecordingTicket` |
//! | `recordings.registerSegment` | `Core::register_segment` | `SegmentReceipt` |
//! | `recordings.updateState` | `Core::update_recording_state` | `RecordingSession` |
//! | `recordings.finalize` | `Core::finalize_recording` | `RecordingFinalizeResult` |
//! | `recordings.recover` | `Core::recover_recording` | `RecordingRecovery` |
//! | `sources.extract` | `Core::extract_source` | `ExtractedContent` |
//! | `sources.extractedContent` | `Core::extracted_content` | `ExtractedContent \| null` |
//! | `sources.previousRevision` | `Core::previous_source_revision` | `SourceRevision \| null` |
//! | `sources.reviseText` | `Core::revise_text` | `SourceRevision` |
//! | `sources.locate` | `Core::locate_source` | `SourceLocation` |
//! | `search.start` | `Core::start_search` | `SearchSnapshot` |
//! | `search.nextPage` | `Core::search_next_page`（不透明游标） | `SearchSnapshot` |
//! | `search.upgrade` | `Core::upgrade_search_to_hybrid` | `SearchSnapshot` |
//! | `search.cancel` | `Core::cancel_search` | `SearchSnapshot` |
//! | `indexes.rebuild` | `Core::schedule_index_rebuild` | `Job` |
//! | `indexes.rebuildSemantic` | `Core::schedule_semantic_index`（只排任务） | `Job` |
//! | `indexes.status` | `Core::index_status` | `IndexStatus` |
//! | `jobs.runDue` | `Core::run_due_jobs` | `JobRunSummary` |
//! | `jobs.status` | `Core::job_status` | `JobStatus` |
//! | `jobs.list` | `Core::list_jobs_page`（状态筛选 + 不透明游标） | `JobPage` |
//! | `jobs.get` | `Core::get_job` | `Job` |
//! | `jobs.retry` | `Core::retry_job` / `retry_job_idempotent`（给了 `operation_id` 时） | `Job` |
//! | `jobs.cancel` | `Core::cancel_job` | `Job` |
//! | `jobs.acknowledgeAttention` | `Core::acknowledge_attention` | `null` |
//! | `providers.save` | `Core::save_provider` | `Provider` |
//! | `providers.list` | `Core::list_providers` | `Provider[]` |
//! | `providers.test` | `Core::test_provider` | `ProviderTestReport` |
//! | `providers.remove` | `Core::remove_provider` | `null` |
//! | `backups.create` | `Core::create_backup`（目标路径必须是沙箱内） | `BackupReport` |
//! | `backups.inspect` | `Core::inspect_backup`（来源路径必须是沙箱内） | `BackupInspection` |
//! | `backups.restore` | `Core::restore_backup`（来源与目标都必须是沙箱内） | `RestoreReport` |
//! | `core.events` | `Core::events_page`（按序号轮询，一页一次 LIMIT） | `{events,next_sequence,truncated}` |
//! | `diary.getDay` | `Core::get_diary_day` | `DiaryDay` |
//! | `diary.listVersions` | `Core::list_diary_versions`（不透明游标） | `DiaryVersionPage` |
//! | `diary.getVersion` | `Core::get_diary_version` | `DiaryVersion` |
//! | `diary.saveEdit` | `Core::save_diary_edit` | `DiaryVersion` |
//! | `diary.setCurrent` | `Core::set_diary_current` | `DiaryDay` |
//! | `diary.merge` | `Core::merge_diary_versions` | `DiaryVersion` |
//!
//! `core.events` **不是**推送流：C ABI 只有请求/响应，所以它把核心持久事件表当游标读。
//! 核心在**每次写事务里**就落了 `domain_events`（不是订阅后才产生），因此这一层不需要
//! 建立订阅、也不需要进程内缓存：`from_sequence` 省略时从当前最新序号开始（只等下一条），
//! 给了就从该序号之后补；`next_sequence` 原样回传给下一次调用。`limit` 默认 200、上限
//! 1000，超出部分不返回并如实标 `truncated: true`（下一页从 `next_sequence` 继续）。
//!
//! 六条 `diary.*` 是「日记」一级入口的数据源（契约 §2.2）：没有它们，鸿蒙日记页
//! 无数据可取。它们**只做转接**——候选/当前、revision 闸门、幂等与版本链都在核心
//! （`diary::get_day` / `save_edit` / `set_current` / `merge_versions`），桥面不复制规则。
//! 「没有任何材料、也没有版本」的一天在核心里是 `not_found`，这是**空态**不是错误，
//! 界面据此显示「还没有成篇日记」。
//!
//! 还差的两条不在本题材里：`diary.listDays`（核心还没有按天枚举的入口）与
//! `diary.generate`（核心有 `generate_diary`，但它要排 AI 任务，属于生成那一片）。
//! 界面**不显示**这两条的按钮，只如实说明缺口。
//!
//! 两条「为什么在这里」的说明（本块的任务书只点名了另外六条）：
//!
//! - `captures.commit`：草稿本身**没有来源**，不 commit 就没有 `source_id`/`revision_id`
//!   可抽取——任务书要求的端到端链路（建草稿 → 保存 → 抽取）缺了它走不通；
//! - `indexes.rebuild`：已接入的方法里**没有别的办法往队列里放任务**，不接它，
//!   `jobs.runDue` 永远只能返回一份「什么都没跑」的摘要，等于没法证明任务这一路是通的。
//!
//! 两者都是「只排任务 / 只落数据」，不引入任何新语义。
//!
//! # 扩展点：再加一个方法要动哪里
//!
//! 1. 在 [`dispatch`] 的 `match` 里加一条分支，并在上面的表里加一行（表与代码在同一个文件，
//!    评审时能对着看）；
//! 2. 参数结构体写在下面「参数」一节：`#[derive(Deserialize)]` + `deny_unknown_fields`
//!    （拼错的字段要报错，不能静默忽略）+ 契约风格的 camelCase 别名；
//! 3. 把方法名加进 [`WIRED_METHODS`]——它同时是 `core.info.capabilities` 的内容，
//!    是给宿主看的**诚实能力清单**（没列出来的就是还没接，界面不该把它显示成可用）；
//! 4. 在 `src/tests.rs` 里加用例（至少覆盖一条成功路径与一条参数错误）。
//!
//! ## 还没接的（按契约第 4 节分组，TODO(#108)）
//!
//! - 回收站：`captures.moveDay` / `captures.trash` / `captures.restore` /
//!   `captures.previewPurge` / `captures.purge`——**核心还没有对应入口**（没有
//!   `trash_capture` / `preview_purge` / `purge_capture` / `move_day`），不是桥面漏接；
//! - 文件：`imports.*`、`assets.open` / `assets.release`；
//! - 检索：`search.snapshot`（`search.*` 只差它）；
//! - 任务：`jobs.nextWakeup`；
//! - 风格与导出：`styles.*` / `exports.create`——**核心还没有**风格落库与导出入口；
//! - 日记：`diary.listDays`（核心还没有按天枚举入口）/ `diary.generate`（核心有
//!   `generate_diary`，但要排 AI 任务，属于生成那一片）；
//! - 其余：`core.snapshot` / `core.close` / `core.setRuntimeState`、`responses.*`、
//!   `plugins.*`——要么核心还没有对应实现，要么属于后面的块。
//!   （`settings.get` / `settings.update` 已接上，见 [`WIRED_METHODS`]。）
//!
//! 没接的方法一律报 `invalid_argument`，message 里点名「未接入」并附上已接入清单，
//! **不会**静默成功、也不会假装支持。

use chrono::Utc;
use diary_core::model::{NativeRecordingStatus, RecordingState, SegmentManifest, SourceLocator};
use diary_core::provider::local_engine::is_ohos_app_data_path;
use diary_core::{
    Core, CoreError, CreateDraftInput, JobState, Provider, ProviderCapability, SearchFilters,
    SearchMode, SearchRequest, Settings,
};
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::{Component, Path};

use crate::CallError;

/// 已经接到桥上的契约方法名（`core.info.capabilities` 就是它）。
pub(crate) const WIRED_METHODS: &[&str] = &[
    "core.info",
    "captures.createDraft",
    "captures.saveDraft",
    "captures.commit",
    "captures.get",
    "captures.list",
    "recordings.prepare",
    "recordings.registerSegment",
    "recordings.updateState",
    "recordings.finalize",
    "recordings.recover",
    "sources.extract",
    "sources.extractedContent",
    "sources.previousRevision",
    "sources.reviseText",
    "sources.locate",
    "search.start",
    "search.nextPage",
    "search.upgrade",
    "search.cancel",
    "indexes.rebuild",
    "indexes.rebuildSemantic",
    "indexes.status",
    "jobs.runDue",
    "jobs.status",
    "jobs.list",
    "jobs.get",
    "jobs.retry",
    "jobs.cancel",
    "jobs.acknowledgeAttention",
    "providers.save",
    "providers.list",
    "providers.test",
    "providers.remove",
    "settings.get",
    "settings.update",
    "backups.create",
    "backups.inspect",
    "backups.restore",
    "core.events",
    "diary.getDay",
    "diary.listVersions",
    "diary.getVersion",
    "diary.saveEdit",
    "diary.setCurrent",
    "diary.merge",
];

/// 一轮 `jobs.runDue` 最多跑几条任务（防御性上限）。
///
/// 契约 `jobs.runDue` 只给毫秒预算、不给条数，所以这个上限由桥接层拍——与
/// `diary_bridge::api::RUN_DUE_MAX_JOBS` 同一个值、同一个理由：预算挡长尾，
/// 它挡「队列很长 + 多条短任务」把一轮拖成没完没了。没跑上的任务留在队列里、
/// 如实计入摘要的 `dueBacklog`，下一轮接着跑。
const RUN_DUE_MAX_JOBS: usize = 64;

/// 分页方法的默认页大小。
///
/// 与 `packages/diary_bridge/lib/src/bridge_diary_api.dart` 的 `pageSize` 和
/// `search.start` 的 `page_size` 同一口径（20），避免同一份界面在不同入口拿到
/// 不一样的页长。上限由核心各自夹（`captures.list` 100、`jobs.list` 200）。
const DEFAULT_PAGE_SIZE: usize = 20;

/// `core.events` 一页的默认条数与硬上限。
///
/// 上限由**核心**在 SQL 里执行（`Core::events_page` 的 `LIMIT limit+1`）：它只多读
/// 一条用来如实回答 `truncated`，不会先把整张事件表读进内存。桥面只做参数校验与
/// 投影。默认 200 够界面一次补完常见的积压；1000 是单次响应「别拖太久」的防御线。
/// 要补更多就按返回的 `next_sequence` 继续调。
const DEFAULT_EVENTS_LIMIT: usize = 200;
const MAX_EVENTS_LIMIT: usize = 1000;

/// 方法分派。`params` 为 `null` 表示调用方没给参数。
pub(crate) fn dispatch(
    core: &mut Core,
    library_id: &str,
    method: &str,
    params: &Value,
) -> Result<Value, CallError> {
    match method {
        // ------------------------------------------------------------ 核心
        "core.info" => {
            no_params(method, params)?;
            to_value(core_info(core, library_id).map_err(CallError::from)?)
        }
        // ------------------------------------------------------------ 记录
        "captures.createDraft" => {
            let input: CreateDraftParams = parse(params)?;
            let capture = core.create_draft(CreateDraftInput {
                occurred_at: input.occurred_at,
                time_zone: &input.time_zone,
                utc_offset_minutes: input.utc_offset_minutes,
                operation_id: &input.operation_id,
            })?;
            to_value(capture)
        }
        "captures.saveDraft" => {
            let input: SaveDraftParams = parse(params)?;
            let result = core.save_draft(
                &input.capture_id,
                &input.text,
                input.expected_revision,
                &input.operation_id,
            )?;
            to_value(result)
        }
        "captures.commit" => {
            let input: CommitParams = parse(params)?;
            let result = core.commit(
                &input.capture_id,
                input.expected_revision,
                &input.operation_id,
            )?;
            to_value(result)
        }
        // 「看今天」/历史列表/条目详情的数据源。`list` 的游标原样透传核心的
        // `CapturePage.next_cursor`——界面不该自己解析或拼游标。
        "captures.get" => {
            let input: CaptureIdParams = parse(params)?;
            to_value(core.get_capture(&input.capture_id)?)
        }
        "captures.list" => {
            let input: ListCapturesParams = parse_optional(params)?;
            let page = core.list_captures(
                input.day_key.as_deref(),
                input.cursor.as_deref(),
                input.limit.unwrap_or(DEFAULT_PAGE_SIZE),
            )?;
            to_value(page)
        }
        // ------------------------------------------------------------ 录音
        "recordings.prepare" => {
            let input: PrepareRecordingParams = parse(params)?;
            let ticket = core.prepare_recording(&input.capture_id, &input.operation_id)?;
            to_value(ticket)
        }
        "recordings.registerSegment" => {
            let input: RegisterSegmentParams = parse(params)?;
            let receipt = core.register_segment(
                &input.recording_id,
                input.segment_index,
                input.segment.into(),
            )?;
            to_value(receipt)
        }
        "recordings.updateState" => {
            let input: UpdateRecordingStateParams = parse(params)?;
            let status = NativeRecordingStatus {
                recording_id: input.recording_id,
                state: input.state,
                wall_clock: input.wall_clock,
                elapsed_ms: input.elapsed_ms,
                persisted_through_ms: input.persisted_through_ms,
                input_device_changed: input.input_device_changed,
                message: input.message,
            };
            let session = core.update_recording_state(status)?;
            to_value(session)
        }
        "recordings.finalize" => {
            let input: FinalizeRecordingParams = parse(params)?;
            let result = core.finalize_recording(
                &input.recording_id,
                input.last_segment_index,
                &input.end_reason,
            )?;
            to_value(result)
        }
        "recordings.recover" => {
            let input: RecoverRecordingParams = parse(params)?;
            let recovery = core.recover_recording(input.recording_id.as_deref())?;
            to_value(recovery)
        }
        // ------------------------------------------------------------ 来源
        "sources.extract" => {
            let input: ExtractParams = parse(params)?;
            let content = core.extract_source(&input.source_ref)?;
            to_value(content)
        }
        // 当前修订的派生内容 / 上一版正文：**没提取过、没有上一版都是 null**，不是错误。
        // ArkTS 侧用可空返回（`invokeNullable`）接，不能把 null 当成信封坏了。
        "sources.extractedContent" => {
            let input: SourceIdParams = parse(params)?;
            to_value(core.extracted_content(&input.source_id)?)
        }
        "sources.previousRevision" => {
            let input: SourceIdParams = parse(params)?;
            to_value(core.previous_source_revision(&input.source_id)?)
        }
        // 改正文：核心建**新修订**、不覆盖旧内容；乐观锁以记录 revision 为准。
        "sources.reviseText" => {
            let input: ReviseTextParams = parse(params)?;
            let revision = core.revise_text(
                &input.source_id,
                &input.text,
                input.expected_revision,
                &input.operation_id,
            )?;
            to_value(revision)
        }
        // 定位：`source_ref` 可以是来源 id 或修订 id；locator 原样交给核心。
        // 找不到 / 纯文字没有原件时核心返回 `available:false` + `reason`，**不报错**。
        "sources.locate" => {
            let input: LocateSourceParams = parse(params)?;
            to_value(core.locate_source(&input.source_ref, input.locator)?)
        }
        // ------------------------------------------------------------ 检索
        "search.start" => {
            let input: SearchStartParams = parse(params)?;
            let request = SearchRequest {
                query: input.request.query,
                mode: input.request.mode,
                filters: input.request.filters.into(),
                page_size: input.request.page_size,
            };
            let snapshot = core.start_search(request, input.query_revision)?;
            to_value(snapshot)
        }
        // 翻页/升级/取消：游标与会话都是核心的不透明 token，桥面只透传。
        // `search.nextPage` 不给游标时核心用会话自己记着的下一页游标；
        // `search.upgrade` 语义拿不出证据时如实停在 `keyword_ready`（不假装融合）。
        "search.nextPage" => {
            let input: SearchNextPageParams = parse(params)?;
            let snapshot = core.search_next_page(&input.session_id, input.cursor.as_deref())?;
            to_value(snapshot)
        }
        "search.upgrade" => {
            let input: SearchSessionParams = parse(params)?;
            to_value(core.upgrade_search_to_hybrid(&input.session_id)?)
        }
        "search.cancel" => {
            let input: SearchSessionParams = parse(params)?;
            to_value(core.cancel_search(&input.session_id)?)
        }
        // ------------------------------------------------------------ 索引
        "indexes.rebuild" => {
            let input: RebuildIndexesParams = parse(params)?;
            let job =
                core.schedule_index_rebuild(&input.scope, &input.reason, &input.operation_id)?;
            to_value(job)
        }
        // 语义这一路（契约 4.4 的 `indexes.rebuildSemantic`）。核心与桥接早就有了
        // （`Core::schedule_semantic_index`），C ABI 之前没接——鸿蒙上就没有任何办法
        // 建语义索引，语义检索必然空。这里只排一条 `semantic_index` 任务，不重建。
        "indexes.rebuildSemantic" => {
            let input: RebuildSemanticParams = parse(params)?;
            let job = core.schedule_semantic_index(&input.reason, &input.operation_id)?;
            to_value(job)
        }
        // 就绪状态原样暴露 `IndexStatus`：界面要的是 `semantic_index_ready` / `reasons` /
        // `model_version`，桥面不改写、不补算。`source_scope` 省略 = 整库；给了空数组
        // 与核心同义（「什么都不看」），见契约 4.4。
        "indexes.status" => {
            let input: IndexStatusParams = parse_optional(params)?;
            to_value(core.index_status(input.source_scope.as_deref())?)
        }
        // ------------------------------------------------------------ 任务
        "jobs.runDue" => {
            let input: RunDueParams = parse(params)?;
            let summary = core.run_due_jobs(
                Utc::now(),
                input.budget_ms,
                &input.trigger,
                RUN_DUE_MAX_JOBS,
            )?;
            to_value(summary)
        }
        "jobs.status" => {
            no_params(method, params)?;
            to_value(core.job_status()?)
        }
        // 任务中心逐条：`list` 用核心的键集游标（不是偏移），`get` 带 `progress` /
        // `attention_key`。`states` 省略或空数组都表示「不限状态」（与核心 `list_jobs`
        // 的口径一致：空筛选不是「什么都不看」）。
        "jobs.list" => {
            let input: ListJobsParams = parse_optional(params)?;
            let states = if input.states.is_empty() {
                None
            } else {
                Some(input.states.as_slice())
            };
            let page = core.list_jobs_page(
                states,
                input.cursor.as_deref(),
                input.limit.unwrap_or(DEFAULT_PAGE_SIZE),
            )?;
            to_value(page)
        }
        "jobs.get" => {
            let input: JobIdParams = parse(params)?;
            to_value(core.get_job(&input.job_id)?)
        }
        // 人工重试：契约给了 `operationId` 就走幂等入口（同一个 op 重放返回第一次的
        // 结果）；没给则是一次普通的「清零重跑」。两条路在核心里是同一个实现。
        "jobs.retry" => {
            let input: RetryJobParams = parse(params)?;
            let job = match input.operation_id.as_deref() {
                Some(operation_id) => core.retry_job_idempotent(&input.job_id, operation_id)?,
                None => core.retry_job(&input.job_id)?,
            };
            to_value(job)
        }
        "jobs.cancel" => {
            let input: CancelJobParams = parse(params)?;
            to_value(core.cancel_job(&input.job_id, &input.operation_id)?)
        }
        // 「这个待处理问题已经展示过」：核心只落一条 ack，返回值就是 `null`。
        // 同一个 `attentionKey` 重复调用是幂等的。
        "jobs.acknowledgeAttention" => {
            let input: AcknowledgeAttentionParams = parse(params)?;
            core.acknowledge_attention(&input.attention_key)?;
            Ok(Value::Null)
        }
        // ------------------------------------------------------------ 本机服务配置
        // 本机转写引擎（sherpa-onnx）的配置入口。没有这几条，宿主就没法把
        // 「模型目录」告诉核心：`providers.model` 只能通过 `providers.save` 落库。
        //
        // 这里**不加任何业务规则**：适配器校验、endpoint 拒绝、能力声明、
        // 测试结论作废都在核心 `provider::save` 里（见那条路径的注释）。
        "providers.save" => {
            let input: SaveProviderParams = parse(params)?;
            let saved = core.save_provider(&input.config, input.secret.as_deref())?;
            to_value(saved)
        }
        "providers.list" => {
            no_params(method, params)?;
            to_value(core.list_providers()?)
        }
        "providers.test" => {
            let input: TestProviderParams = parse(params)?;
            let report = core.test_provider(&input.provider_id, &input.scope)?;
            to_value(report)
        }
        "providers.remove" => {
            let input: ProviderIdParams = parse(params)?;
            core.remove_provider(&input.provider_id)?;
            Ok(Value::Null)
        }
        // ------------------------------------------------------------ 设置（契约 4.4）
        // 完整设置对象与整理设置共用核心 `settings` 表的同一行；这里不拼业务规则，
        // 只做「JSON ↔ 核心对象」的搬运与参数形状校验。
        "settings.get" => {
            no_params(method, params)?;
            to_value(core.get_settings()?)
        }
        "settings.update" => {
            let input: UpdateSettingsParams = parse(params)?;
            let settings = input.settings.into_settings();
            to_value(core.update_settings(&settings, input.expected_revision)?)
        }
        // ------------------------------------------------------------ 备份与恢复
        // 核心这三个是**同步**入口（契约那三行返回 jobId 的任务化是 #70，不在本块）。
        // C ABI 只做「路径校验 + 调核心 + 原样序列化」；版本/哈希/路径安全全在核心
        // （`Core::create_backup` / `inspect_backup` / `restore_backup`）。
        //
        // 为什么在桥面拦沙箱外路径：鸿蒙宿主只能写自己的应用数据目录；把 `/sdcard`
        // 或 `/data/storage/el1/bundle`（安装目录，只读）传下去，核心会先做一堆工作
        // 才在文件系统上失败，错误也指不到「你没权限写这里」。与 `asrEngineLib` 的
        // 校验复用同一个路径判定（`is_ohos_app_data_path`），方向按备份的诉求：
        // 原生库必须在安装目录，备份包/恢复目标必须在应用数据目录。
        "backups.create" => {
            let input: CreateBackupParams = parse(params)?;
            ensure_full_backup(&input)?;
            let target_path = path_param(
                "target_path",
                input.target_path,
                "targetTicket",
                input.target_ticket,
            )?;
            validate_sandbox_path("backups.create.target_path", &target_path)?;
            to_value(core.create_backup(&target_path)?)
        }
        "backups.inspect" => {
            let input: InspectBackupParams = parse(params)?;
            let package_path = path_param(
                "package_path",
                input.package_path,
                "packageTicket",
                input.package_ticket,
            )?;
            validate_sandbox_path("backups.inspect.package_path", &package_path)?;
            to_value(core.inspect_backup(&package_path)?)
        }
        "backups.restore" => {
            let input: RestoreBackupParams = parse(params)?;
            ensure_restore_to_new_library(&input)?;
            let package_path = path_param(
                "package_path",
                input.package_path,
                "packageTicket",
                input.package_ticket,
            )?;
            let target_dir = input.target_dir.ok_or_else(|| {
                CallError::invalid_argument("C ABI：backups.restore 缺少 `target_dir`")
            })?;
            validate_sandbox_path("backups.restore.package_path", &package_path)?;
            validate_sandbox_path("backups.restore.target_dir", &target_dir)?;
            to_value(core.restore_backup(&package_path, &target_dir)?)
        }
        // ------------------------------------------------------------ 事件
        // 请求/响应形态的「按序号轮询」：核心在写事务里持久化事件，桥面不退化成推送。
        // 不建订阅、不缓存，`from_sequence` 省略 = 从当前最新序号开始（只等下一条）。
        "core.events" => {
            let input: EventsParams = parse_optional(params)?;
            events_page(core, &input)
        }
        // ------------------------------------------------------------ 日记
        // 契约 §2.2 的「日记」一级入口：按天读、读版本、人工编辑、切当前版本、合并。
        // 这一组只透传核心的 `DiaryDay` / `DiaryVersion` / `DiaryVersionPage`：
        // - 哪一版是当前、哪些是候选由 `DiaryDay.current_version_id` /
        //   `candidate_version_ids` 表达（`DiaryVersion` 本身没有状态字段）；
        // - 写路径的 `expected_revision` 取自 `DiaryDay.revision`，语义与冲突闸门全在核心；
        // - `not_found` 原样透传：空的一天对界面是空态，不是「读取失败」。
        "diary.getDay" => {
            let input: DiaryDayParams = parse(params)?;
            to_value(core.get_diary_day(&input.day_key)?)
        }
        "diary.listVersions" => {
            let input: ListDiaryVersionsParams = parse(params)?;
            let page = core.list_diary_versions(
                &input.day_key,
                input.cursor.as_deref(),
                input.limit.unwrap_or(DEFAULT_PAGE_SIZE),
            )?;
            to_value(page)
        }
        "diary.getVersion" => {
            let input: DiaryVersionParams = parse(params)?;
            to_value(core.get_diary_version(&input.version_id)?)
        }
        "diary.saveEdit" => {
            let input: SaveDiaryEditParams = parse(params)?;
            let version = core.save_diary_edit(
                &input.base_version_id,
                &input.body,
                input.expected_revision,
                &input.operation_id,
            )?;
            to_value(version)
        }
        "diary.setCurrent" => {
            let input: SetDiaryCurrentParams = parse(params)?;
            let day = core.set_diary_current(
                &input.day_key,
                &input.version_id,
                input.expected_revision,
            )?;
            to_value(day)
        }
        "diary.merge" => {
            let input: MergeDiaryParams = parse(params)?;
            let version = core.merge_diary_versions(
                &input.base_version_id,
                &input.candidate_version_ids,
                &input.body,
                input.expected_revision,
                &input.operation_id,
            )?;
            to_value(version)
        }
        // ------------------------------------------------------------ 测试钩子
        // 只在 `cargo test` 的构建里存在：证明「核心 panic 不会穿过 FFI」。
        #[cfg(test)]
        "__test.panic" => {
            no_params(method, params)?;
            panic!("diary_capi 测试钩子：注入的 panic");
        }
        other => Err(unknown_method(other)),
    }
}

/// `core.info` 的数据：与 `diary_bridge::api::LibraryInfo` 同形、同口径。
///
/// 恢复摘要里的数字直接来自库，`pending_jobs` 走精确计数（`list_jobs` 的长度会被它自己的
/// 分页上限夹住，任务多时会少报）。
///
/// `runtime_config` 是**加性扩展**（`diary_bridge::api::LibraryInfo` 没有这一格）：
/// 鸿蒙产品包设不了环境变量，宿主必须能从 `core.info` 里看到两条能力各自「从哪儿读到
/// 什么路径、现在到哪一步」——否则配置失败时只能blind 猜。它只 stat、不加载模型。
fn core_info(core: &Core, library_id: &str) -> Result<Value, CoreError> {
    let recoverable_imports = core.count_imports_in_flight()?;
    let open_recordings = core.count_open_recordings()?;
    let pending_jobs = core.count_jobs(Some(&[JobState::Queued, JobState::RetryWait]))?;

    let mut notes = Vec::new();
    if recoverable_imports > 0 {
        notes.push(format!("有 {recoverable_imports} 个导入上次没有完成"));
    }
    if open_recordings > 0 {
        notes.push(format!("有 {open_recordings} 段录音上次没有收尾"));
    }

    Ok(json!({
        "api_version": crate::API_VERSION,
        "data_schema_version": core.schema_version()?,
        "build_version": env!("CARGO_PKG_VERSION"),
        "library_id": library_id,
        "capabilities": WIRED_METHODS,
        "recovery": {
            "recoverable_imports": recoverable_imports,
            "open_recordings": open_recordings,
            "pending_jobs": pending_jobs,
            "notes": notes,
        },
        "capture_count": core.count_captures()?,
        "last_event_sequence": core.last_event_sequence()?,
        // 两条能力的配置来源与就绪状态（语义/转写）。纯 stat，不建会话。
        "runtime_config": core.runtime_config(),
    }))
}

fn unknown_method(method: &str) -> CallError {
    CallError::invalid_argument(format!(
        "C ABI：未接入的方法 `{method}`（已接入：{}）",
        WIRED_METHODS.join("、")
    ))
}

/// 无参方法不接受任何字段：宿主拼错方法名、或把参数给了无参方法时要报错，
/// 不能静默忽略。
fn no_params(method: &str, params: &Value) -> Result<(), CallError> {
    match params {
        Value::Null => Ok(()),
        Value::Object(map) if map.is_empty() => Ok(()),
        _ => Err(CallError::invalid_argument(format!(
            "C ABI：{method} 不接受参数（收到 {params}）"
        ))),
    }
}

fn parse<T: DeserializeOwned>(params: &Value) -> Result<T, CallError> {
    serde_json::from_value(params.clone())
        .map_err(|error| CallError::invalid_argument(format!("C ABI：参数不合法（{error}）")))
}

/// 只含可选字段的参数：`params` 省略（`null`）与空对象都按「全用默认值」处理。
///
/// 其余方法保持 [`parse`] 的严格口径：必填字段缺失、`null` 或字段拼错都是
/// `invalid_argument`，不静默补默认值。
fn parse_optional<T: DeserializeOwned + Default>(params: &Value) -> Result<T, CallError> {
    match params {
        Value::Null => Ok(T::default()),
        other => parse(other),
    }
}

fn to_value<T: serde::Serialize>(value: T) -> Result<Value, CallError> {
    // 核心模型的字段都是 JSON 表达得了的；真序列化不了说明数据本身有问题，如实报错。
    serde_json::to_value(value)
        .map_err(|error| CallError::invalid_state(format!("C ABI：结果序列化失败（{error}）")))
}

// ---------------------------------------------------------------- 事件轮询

/// `core.events` 的一页。
///
/// `next_sequence` 是下一次调用的 `from_sequence`：有事件时是本页最后一条的序号，
/// 没事件时是本次的起点（**已由核心夹到当前最大序号**，游标超前时不会原样返回一个
/// 未来的序号）。`truncated` 为 true 表示「本页之后还有」，必须继续按 `next_sequence`
/// 拉，不能把一页当成全部。
///
/// 有界读在**核心**（`Core::events_page` 的 SQL `LIMIT limit+1`）：桥面不再「先读全部
/// 再截断」。事件表的积压上限与保留策略也在核心（`EVENT_BACKLOG_LIMIT`）；游标早于
/// 保留窗口时核心报 `cursor_expired`，这里原样透传，调用方按契约第 6 节重读快照。
fn events_page(core: &Core, input: &EventsParams) -> Result<Value, CallError> {
    if let Some(from) = input.from_sequence {
        if from < 0 {
            return Err(CallError::invalid_argument(
                "C ABI：core.events 的 from_sequence 不能为负数",
            ));
        }
    }
    let limit = input.limit.unwrap_or(DEFAULT_EVENTS_LIMIT);
    if limit == 0 || limit > MAX_EVENTS_LIMIT {
        return Err(CallError::invalid_argument(format!(
            "C ABI：core.events 的 limit 必须在 1..={MAX_EVENTS_LIMIT} 之间（收到 {limit}）"
        )));
    }

    // 省略 from_sequence = 从现在开始等：先读当前最大序号，再从它之后取（通常为空）。
    // 这**不是**「从 0 补齐历史」——要历史就显式给 0；两种语义刻意不同。
    let from = match input.from_sequence {
        Some(from) => from,
        None => core.last_event_sequence()?,
    };
    let page = core.events_page(from, limit)?;
    // 事件类型必须按**线上字符串**（`capture.changed`）给，不能把 `EventType` 的
    // serde 默认变体名（`CaptureChanged`）漏出去：契约 `DomainEventType` 与
    // `EventType::wire()` 是同一套写法。核心的这个枚举没有 serde rename，桥面在这里
    // 显式投影一次（只改枚举的写法，不改任何字段/语义）。
    let events: Vec<Value> = page
        .events
        .iter()
        .map(|event| {
            json!({
                "event_id": event.event_id,
                "sequence": event.sequence,
                "type": event.event_type.wire(),
                "entity_id": event.entity_id,
                "revision": event.revision,
                "emitted_at": event.emitted_at,
            })
        })
        .collect();
    Ok(json!({
        "events": events,
        "next_sequence": page.next_sequence,
        "truncated": page.truncated,
    }))
}

// ---------------------------------------------------------------- 备份路径

/// `backups.create` 只支持完整备份：`include_assets: false` 明确报错。
///
/// 核心的 `create_backup` 承诺「含原件的一致性备份」，没有「只备份库、不备份原件」
/// 的入口。静默忽略这个字段会产出一份调用方以为更小的包——不如在参数层说清楚。
fn ensure_full_backup(input: &CreateBackupParams) -> Result<(), CallError> {
    if !input.include_assets {
        return Err(CallError::invalid_argument(
            "C ABI：backups.create 只支持完整备份，include_assets 不能为 false",
        ));
    }
    Ok(())
}

/// `backups.restore` 只恢复到**新库**：`restore_as_new_library: false` 明确报错。
///
/// 核心的 `restore_backup` 绝不覆盖当前资料库（目标必须是新目录）。契约的默认值就是
/// `true`；把 `false` 静默当成 `true` 会让调用方以为「覆盖恢复了」。
///
/// `include_indexes` 不做拦截：核心恢复的是快照整体，索引/向量本来就在快照里，
/// 恢复后同样可以重建；这个开关不改变结果（文档里写明，不假装能删索引）。
fn ensure_restore_to_new_library(input: &RestoreBackupParams) -> Result<(), CallError> {
    if !input.options.restore_as_new_library {
        return Err(CallError::invalid_argument(
            "C ABI：backups.restore 只支持恢复到新库，restore_as_new_library 不能为 false",
        ));
    }
    Ok(())
}

/// 备份路径校验：OHOS 上必须是**应用沙箱内**路径。
///
/// 与 `asrEngineLib` 的校验复用同一个前缀判定（`is_ohos_app_data_path`），但方向相反：
/// 原生库必须在 HAP 安装目录（沙箱里的 `.so` 不能 dlopen），备份包与恢复目标必须在
/// 应用数据目录（安装目录只读）。其它平台不限制（桌面端测试/CLI 想放哪都行）。
fn validate_sandbox_path(field: &str, path: &str) -> Result<(), CallError> {
    validate_sandbox_path_with(field, path, cfg!(target_env = "ohos"))
}

/// 把「路径字段 + 同名票据字段」解析成一个路径：**票据一律拒绝**。
///
/// 契约里 `targetTicket` / `packageTicket` 是 `PlatformHost` 的**限域票据**——宿主拿到
/// 它、解析成自己能访问的真实路径，再交给核心。C ABI 在票据层**之下**，只认路径：
/// 把票据字符串当路径用，桌面端会按它建一个奇怪的文件、OHOS 上会被沙箱前缀拒绝，
/// 两种错误都指不到「你在错误的层传了票据」。所以这里显式报错，并把该做的事写进
/// 消息（先解析成沙箱内路径，再传 `target_path` / `package_path`）。
fn path_param(
    field: &str,
    path: Option<String>,
    ticket_field: &str,
    ticket: Option<String>,
) -> Result<String, CallError> {
    if ticket.is_some() {
        return Err(CallError::invalid_argument(format!(
            "C ABI：`{ticket_field}` 是 PlatformHost 的限域票据，不是文件路径；请先在宿主侧\
             把它解析成沙箱内路径，再传 `{field}`"
        )));
    }
    path.ok_or_else(|| CallError::invalid_argument(format!("C ABI：缺少 `{field}`")))
}

/// [`validate_sandbox_path`] 的可测内核：`enforce` 由调用方给，宿主单测能同时覆盖两半。
pub(crate) fn validate_sandbox_path_with(
    field: &str,
    path: &str,
    enforce: bool,
) -> Result<(), CallError> {
    if !enforce {
        return Ok(());
    }
    if !is_inside_ohos_sandbox(path) {
        return Err(CallError::invalid_argument(format!(
            "C ABI：{field} 必须是应用沙箱内的路径（OHOS 只允许 \
             /data/storage/el2|el1/base|database|cache/… 与 /data/app/el2|el1/base/…），\
             收到 {path}"
        )));
    }
    Ok(())
}

/// 沙箱内判定：命中应用数据前缀，且**不含量级上跳**（`..`）。
///
/// 纯字符串/组件判断，与核心的 `is_ohos_app_data_path` 同一口径；不解析符号链接
/// （备份目标通常还不存在，没法 canonicalize）。`..` 必须拒绝，否则
/// `/data/storage/el2/base/../../../etc/x` 也能命中前缀。
fn is_inside_ohos_sandbox(path: &str) -> bool {
    if !is_ohos_app_data_path(path) {
        return false;
    }
    !Path::new(path)
        .components()
        .any(|component| matches!(component, Component::ParentDir))
}

// ---------------------------------------------------------------- 参数
//
// 约定（见 docs/architecture/m6-核心-C-ABI.md）：
// - 字段名以**核心 Rust 参数名**为准（snake_case），与响应 `data` 是同一套写法；
//   契约表里的 camelCase 写法作为别名一并接受（`#[serde(alias = …)]`）；
// - `deny_unknown_fields`：拼错的字段直接报错；
// - 每个字段的 `data`/核心入参含义见对应核心方法的文档注释，这里不复制业务规则。

/// `captures.createDraft`：契约第 4.1 节。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateDraftParams {
    /// 事件时间（RFC 3339）；不给就用现在。
    #[serde(default, alias = "occurredAt")]
    occurred_at: Option<chrono::DateTime<Utc>>,
    /// 设备当前的 IANA 时区名，例如 `Asia/Shanghai`。
    #[serde(alias = "timeZone")]
    time_zone: String,
    /// 事件发生时该时区的偏移分钟数。
    #[serde(alias = "utcOffsetMinutes")]
    utc_offset_minutes: i32,
    /// 幂等键：同一个 operationId 重复提交返回原结果。
    #[serde(alias = "operationId")]
    operation_id: String,
}

/// `captures.saveDraft`。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SaveDraftParams {
    #[serde(alias = "id", alias = "captureId")]
    capture_id: String,
    text: String,
    #[serde(alias = "expectedRevision")]
    expected_revision: i64,
    #[serde(alias = "operationId")]
    operation_id: String,
}

/// `captures.commit`。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CommitParams {
    #[serde(alias = "id", alias = "captureId")]
    capture_id: String,
    #[serde(alias = "expectedRevision")]
    expected_revision: i64,
    #[serde(alias = "operationId")]
    operation_id: String,
}

/// `captures.get`：详情页的数据源。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CaptureIdParams {
    #[serde(alias = "id", alias = "captureId")]
    capture_id: String,
}

/// `captures.list`：「看今天」/历史列表的数据源。
///
/// 三个字段都可省：省略 `day_key` 是全部日期（历史列表），省略 `cursor` 是第一页，
/// 省略 `limit` 用 [`DEFAULT_PAGE_SIZE`]。`cursor` 必须是上一页返回的
/// `next_cursor` 原样值，桥面不解析它。
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct ListCapturesParams {
    #[serde(alias = "dayKey")]
    day_key: Option<String>,
    cursor: Option<String>,
    limit: Option<usize>,
}

/// `recordings.prepare`：申请录音票据与限域暂存目录。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PrepareRecordingParams {
    #[serde(alias = "id", alias = "captureId")]
    capture_id: String,
    #[serde(alias = "operationId")]
    operation_id: String,
}

/// `recordings.registerSegment`：登记一个已封闭片段。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RegisterSegmentParams {
    #[serde(alias = "recordingId")]
    recording_id: String,
    #[serde(alias = "segmentIndex")]
    segment_index: i64,
    segment: SegmentManifestParams,
}

/// `SegmentManifest` 的 JSON 形态：允许 snake_case（核心字段名）与 camelCase（契约写法）。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SegmentManifestParams {
    #[serde(alias = "segmentId")]
    segment_id: String,
    #[serde(alias = "relativeFileName")]
    relative_file_name: String,
    #[serde(alias = "durationMs")]
    duration_ms: i64,
    sha256: String,
    #[serde(alias = "byteSize")]
    byte_size: i64,
    #[serde(default, alias = "closedAt")]
    closed_at: Option<chrono::DateTime<Utc>>,
}

impl From<SegmentManifestParams> for SegmentManifest {
    fn from(params: SegmentManifestParams) -> Self {
        Self {
            segment_id: params.segment_id,
            relative_file_name: params.relative_file_name,
            duration_ms: params.duration_ms,
            sha256: params.sha256,
            byte_size: params.byte_size,
            closed_at: params.closed_at,
        }
    }
}

/// `recordings.updateState`：原生上报的录音状态（核心不虚构麦克风状态）。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateRecordingStateParams {
    #[serde(alias = "recordingId")]
    recording_id: String,
    /// `preparing` / `recording` / `paused` / `stopping` / `saved` / `interrupted` /
    /// `recoverable` / `error` / `idle`（核心 `RecordingState` 的线上写法）。
    state: RecordingState,
    #[serde(alias = "wallClock")]
    wall_clock: chrono::DateTime<Utc>,
    #[serde(alias = "elapsedMs")]
    elapsed_ms: i64,
    #[serde(alias = "persistedThroughMs")]
    persisted_through_ms: i64,
    #[serde(default, alias = "inputDeviceChanged")]
    input_device_changed: bool,
    #[serde(default)]
    message: Option<String>,
}

/// `recordings.finalize`：把已封闭片段拼成逻辑音频并排转写任务。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FinalizeRecordingParams {
    #[serde(alias = "recordingId")]
    recording_id: String,
    #[serde(alias = "lastSegmentIndex")]
    last_segment_index: i64,
    #[serde(alias = "endReason")]
    end_reason: String,
}

/// `recordings.recover`：`recording_id` 省略时恢复最近一条未收尾的录音。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RecoverRecordingParams {
    #[serde(default, alias = "recordingId")]
    recording_id: Option<String>,
}

/// `sources.extract`：`source_ref` 可以是来源 id，也可以是修订 id。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExtractParams {
    #[serde(alias = "sourceRef")]
    source_ref: String,
}

/// `sources.extractedContent` / `sources.previousRevision`：只要来源 id。
///
/// 两个方法都可能返回 `null`（还没提取过 / 没有上一版），这是正常结果，不是错误。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceIdParams {
    #[serde(alias = "id", alias = "sourceId")]
    source_id: String,
}

/// `sources.reviseText`：改正文会建**新修订**，不动旧内容。
///
/// `expected_revision` 是记录（聚合根）的 revision：不符报 `revision_conflict`，
/// 核心一个字节都不改。`operation_id` 是幂等键，重放返回同一条修订。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReviseTextParams {
    #[serde(alias = "id", alias = "sourceId")]
    source_id: String,
    text: String,
    #[serde(alias = "expectedRevision")]
    expected_revision: i64,
    #[serde(alias = "operationId")]
    operation_id: String,
}

/// `sources.locate`：来源引用 + 定位信息。
///
/// 契约表写 `sourceRef`，Dart 方法用的是 `ref`，核心参数是 `source_ref`；三个都收，
/// 避免宿主为了一个字段名改写请求。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LocateSourceParams {
    #[serde(alias = "ref", alias = "sourceRef")]
    source_ref: String,
    locator: SourceLocator,
}

/// `search.start`：`SearchRequest` + `queryRevision`（契约第 4.4 节）。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchStartParams {
    request: SearchRequestParams,
    #[serde(default, alias = "queryRevision")]
    query_revision: i64,
}

/// `search.nextPage`：会话号 + 可省略的游标。
///
/// 省略 `cursor` 时核心用会话自己记着的下一页游标（等价于「继续往下翻」）；
/// 显式给游标时必须是同一会话、同一结果代次的 `cursor`，否则报 `cursor_expired`。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchNextPageParams {
    #[serde(alias = "sessionId")]
    session_id: String,
    #[serde(default)]
    cursor: Option<String>,
}

/// `search.upgrade` / `search.cancel`：只要会话号。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchSessionParams {
    #[serde(alias = "sessionId")]
    session_id: String,
}

/// `SearchRequest` 的 JSON 形态：只有 `query` 是必填的，其余按核心默认口径。
#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct SearchRequestParams {
    query: String,
    mode: SearchMode,
    filters: SearchFiltersParams,
    #[serde(alias = "pageSize")]
    page_size: i64,
}

impl Default for SearchRequestParams {
    fn default() -> Self {
        Self {
            query: String::new(),
            mode: SearchMode::Keyword,
            filters: SearchFiltersParams::default(),
            // 与 `packages/diary_bridge/lib/src/bridge_diary_api.dart` 的 pageSize 同一口径。
            page_size: 20,
        }
    }
}

/// `SearchFilters` 的 JSON 形态：全字段可选（缺省即「不限」）。
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct SearchFiltersParams {
    #[serde(alias = "fromDayKey")]
    from_day_key: Option<String>,
    #[serde(alias = "toDayKey")]
    to_day_key: Option<String>,
    kinds: Vec<diary_core::SourceKind>,
    #[serde(alias = "sourceScope")]
    source_scope: Option<Vec<String>>,
    #[serde(alias = "includeOldDiaryVersions")]
    include_old_diary_versions: bool,
    #[serde(alias = "includeTrashed")]
    include_trashed: bool,
}

impl From<SearchFiltersParams> for SearchFilters {
    fn from(params: SearchFiltersParams) -> Self {
        Self {
            from_day_key: params.from_day_key,
            to_day_key: params.to_day_key,
            kinds: params.kinds,
            source_scope: params.source_scope,
            include_old_diary_versions: params.include_old_diary_versions,
            include_trashed: params.include_trashed,
        }
    }
}

/// `indexes.rebuild`：`scope` 的编码（`all` / `sources:<id>[,<id>]*`）见契约第 4.4 节。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RebuildIndexesParams {
    scope: String,
    reason: String,
    #[serde(alias = "operationId")]
    operation_id: String,
}

/// `indexes.rebuildSemantic`：只排一条**整库**语义重建任务（契约 4.4）。
///
/// 没有 `scope`：语义的代次与向量是全局的，范围只决定这一轮重算了哪些块
/// （见 `Core::schedule_semantic_index` 的说明）。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RebuildSemanticParams {
    reason: String,
    #[serde(alias = "operationId")]
    operation_id: String,
}

/// `indexes.status`：可选的来源范围。
///
/// 省略 = 整库；给了空数组 = 「什么都不看」（与核心 `index_status` 同义，见契约 4.4）。
/// 两者刻意不同，不能把空数组当成「没给」。
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct IndexStatusParams {
    #[serde(alias = "sourceScope")]
    source_scope: Option<Vec<String>>,
}

/// `jobs.runDue`：毫秒预算 + 唤醒来源（契约第 4.4 节）。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RunDueParams {
    #[serde(alias = "budgetMs")]
    budget_ms: u64,
    trigger: String,
}

/// `jobs.list`：状态筛选 + 不透明游标 + 页大小。
///
/// `states` 省略或空数组都是「不限状态」（任务中心默认要看到全部）。
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct ListJobsParams {
    states: Vec<JobState>,
    cursor: Option<String>,
    limit: Option<usize>,
}

/// `jobs.get`。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct JobIdParams {
    #[serde(alias = "id", alias = "jobId")]
    job_id: String,
}

/// `jobs.retry`：`operation_id` 可省。
///
/// 给了就走核心的幂等入口（同一个 op 重放返回第一次的结果）；省了就是一次普通的
/// 「清零重跑」。契约建议总是给，重试按钮要能安全重放。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RetryJobParams {
    #[serde(alias = "id", alias = "jobId")]
    job_id: String,
    #[serde(default, alias = "operationId")]
    operation_id: Option<String>,
}

/// `jobs.cancel`：核心要求 `operation_id`（取消也要能安全重放）。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CancelJobParams {
    #[serde(alias = "id", alias = "jobId")]
    job_id: String,
    #[serde(alias = "operationId")]
    operation_id: String,
}

/// `jobs.acknowledgeAttention`：`attention_key` 就是 `Job.attention_key` 的原值。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AcknowledgeAttentionParams {
    #[serde(alias = "attentionKey")]
    attention_key: String,
}

/// `providers.save`：`config` 与 `secret` 的字段名就是核心 `Provider` 的字段名。
///
/// `config.test_status` 由核心忽略（只读结论，不能由调用方写成「已验证」）；
/// 本机引擎的 `model` 是一份**目录**，`endpoint` 必须为空。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SaveProviderParams {
    config: Provider,
    /// 明文密钥：只在这一刻经过内存，核心立刻换成凭据引用。本机引擎不需要。
    #[serde(default)]
    secret: Option<String>,
}

/// `providers.remove`：只删配置行，不删任何日记/派生内容。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProviderIdParams {
    #[serde(alias = "providerId")]
    provider_id: String,
}

/// `providers.test`：`scope` 是用户授权的测试范围（契约要求显式列出）。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TestProviderParams {
    #[serde(alias = "providerId")]
    provider_id: String,
    #[serde(default)]
    scope: Vec<ProviderCapability>,
}

/// `settings.update`：完整设置对象 + `expectedRevision`（契约第 4.4 节）。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateSettingsParams {
    settings: SettingsInput,
    #[serde(alias = "expectedRevision")]
    expected_revision: i64,
}

/// `Settings` 的入参形态。
///
/// 字段名与响应一样以核心的 snake_case 为准，同时接受契约里的 camelCase。
/// `revision` / `updated_at` 是**只读结论**：调用方给不给都不影响写入（核心忽略它们，
/// 冲突保护只认外层的 `expected_revision`），所以这里允许省略——首次设置不必先伪造
/// 一个时间戳。其余四个字段是必填的，拼错/漏字段都是 `invalid_argument`。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SettingsInput {
    #[serde(alias = "diaryTimeZone")]
    diary_time_zone: String,
    #[serde(default, alias = "revision")]
    revision: i64,
    #[serde(default, alias = "updatedAt")]
    updated_at: Option<chrono::DateTime<Utc>>,
    #[serde(alias = "autoOrganizeEnabled")]
    auto_organize_enabled: bool,
    #[serde(alias = "autoOrganizeTime")]
    auto_organize_time: String,
    #[serde(default, alias = "defaultStyleId")]
    default_style_id: Option<String>,
}

impl SettingsInput {
    /// 转成核心对象。`revision` / `updated_at` 会被 `update_settings` 忽略，这里给 0 /
    /// 当前时间只是为了满足结构体的形状，不参与任何判断。
    fn into_settings(self) -> Settings {
        Settings {
            diary_time_zone: self.diary_time_zone,
            revision: self.revision,
            updated_at: self.updated_at.unwrap_or_else(Utc::now),
            auto_organize_enabled: self.auto_organize_enabled,
            auto_organize_time: self.auto_organize_time,
            default_style_id: self.default_style_id,
        }
    }
}
// ---------------------------------------------------------------- 备份与恢复

/// `backups.create`：目标路径 + 是否含原件。
///
/// 契约的 Dart 形状是 `BackupRequest { targetTicket, includeAssets }`；C ABI 在票据层
/// 之下，只收**已经解析成沙箱路径**的 `target_path` / `targetPath`。
///
/// `targetTicket` / `target_ticket` **刻意不是** `target_path` 的别名：票据是
/// `PlatformHost` 的限域句柄，不是文件路径。当路径用会在桌面端按票据字符串真的建文件，
/// 在 OHOS 上则被沙箱判定拒绝、错误还指向「路径不在沙箱内」——两种都误导人。收到票据
/// 字段时由 [`path_param`] 显式报错，说清它该在哪一层解析。
///
/// `include_assets` 默认 true；false 会被明确拒绝（核心只做完整备份，见
/// [`ensure_full_backup`]）。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateBackupParams {
    #[serde(alias = "targetPath")]
    target_path: Option<String>,
    #[serde(default = "default_true", alias = "includeAssets")]
    include_assets: bool,
    #[serde(alias = "targetTicket")]
    target_ticket: Option<String>,
}

/// `backups.inspect`：包路径（同样在票据层之下，必须是沙箱内）。
///
/// `packageTicket` 与 `targetTicket` 同一条规则：票据不是路径，见 [`path_param`]。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct InspectBackupParams {
    #[serde(alias = "packagePath")]
    package_path: Option<String>,
    #[serde(alias = "packageTicket")]
    package_ticket: Option<String>,
}

/// `backups.restore`：包路径 + 恢复目标目录 + 契约的 `RestoreOptions`。
///
/// 契约的 Dart 形状只有 `packageTicket + options`，目标库由平台决定；C ABI 必须把
/// 目标目录也说清楚（核心要求一个**新的空目录**），所以多一个 `target_dir`。这是
/// **加性参数**，不是另一套业务规则。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RestoreBackupParams {
    #[serde(alias = "packagePath")]
    package_path: Option<String>,
    #[serde(alias = "targetDir")]
    target_dir: Option<String>,
    #[serde(default)]
    options: RestoreOptionsParams,
    #[serde(alias = "packageTicket")]
    package_ticket: Option<String>,
}

/// `RestoreOptions` 的 JSON 形态：默认「恢复到新库、不动当前库」。
///
/// `include_indexes` 只做形状兼容：核心恢复的是快照整体（索引/向量都在里面），
/// 这个开关不改变结果——不假装能「不恢复索引」。
#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RestoreOptionsParams {
    #[serde(alias = "restoreAsNewLibrary")]
    restore_as_new_library: bool,
    #[serde(alias = "includeIndexes")]
    include_indexes: bool,
}

impl Default for RestoreOptionsParams {
    fn default() -> Self {
        Self {
            // 契约 `RestoreOptions.restoreAsNewLibrary` 的默认值。
            restore_as_new_library: true,
            include_indexes: false,
        }
    }
}

fn default_true() -> bool {
    true
}

// ---------------------------------------------------------------- 事件

/// `core.events`：按序号轮询持久事件。
///
/// - `from_sequence` 省略 = 从**当前最新**序号开始（只等下一条，不补历史）；
///   显式给 0 才是「从头补」；
/// - `limit` 默认 [`DEFAULT_EVENTS_LIMIT`]，上限 [`MAX_EVENTS_LIMIT`]。
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct EventsParams {
    #[serde(alias = "fromSequence")]
    from_sequence: Option<i64>,
    limit: Option<usize>,
}

// ---------------------------------------------------------------- 日记（契约 4.3 / 2.5）

/// `diary.getDay`：`day_key` 必填，形状是 `YYYY-MM-DD`（核心校验，桥面不放宽）。
///
/// 契约的 Dart 形状是位置参数 `getDay(String dayKey)`，C ABI 用对象承载；
/// `dayKey` 别名一并接受。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DiaryDayParams {
    #[serde(alias = "dayKey")]
    day_key: String,
}

/// `diary.listVersions`：`day_key` 必填；`cursor` 原样回传上一页的 `next_cursor`；
/// `limit` 默认 [`DEFAULT_PAGE_SIZE`]，核心夹 1–100。
///
/// 与 `captures.list` / `jobs.list` 同一口径：游标是核心的**不透明 token**，
/// 桥面不解析、不自己拼；格式不对由核心报 `invalid_state`。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ListDiaryVersionsParams {
    #[serde(alias = "dayKey")]
    day_key: String,
    #[serde(default)]
    cursor: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
}

/// `diary.getVersion`：`version_id` 必填。
///
/// 契约的 Dart 形状是 `getVersion(String versionId)`；`id` / `versionId` 也收，
/// 与 `captures.get` / `jobs.get` 的别名口径一致。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DiaryVersionParams {
    #[serde(alias = "versionId", alias = "id")]
    version_id: String,
}

/// `diary.saveEdit`：把人工编辑写成**新版本**（基版一个字节都不动）。
///
/// `expected_revision` 是 `DiaryDay.revision`（不是版本数）：不符报
/// `revision_conflict`，核心零改动。`operation_id` 是幂等键，重放返回原版本。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SaveDiaryEditParams {
    #[serde(alias = "baseVersionId")]
    base_version_id: String,
    body: String,
    #[serde(alias = "expectedRevision")]
    expected_revision: i64,
    #[serde(alias = "operationId")]
    operation_id: String,
}

/// `diary.setCurrent`：在一天的版本之间指定当前版本。
///
/// 契约这一行没有 `operationId`：它**不产生任何东西**（重复指定同一个当前版本是
/// 无操作，不推进 revision），按目标状态幂等。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SetDiaryCurrentParams {
    #[serde(alias = "dayKey")]
    day_key: String,
    #[serde(alias = "versionId")]
    version_id: String,
    #[serde(alias = "expectedRevision")]
    expected_revision: i64,
}

/// `diary.merge`：把两版（或多版）合成一条新的 `merge` 版本。
///
/// `body` 是**用户确认后的正文**（核心不替用户合并、不调模型）；候选列表为空、
/// 有重复、含基版自己、跨天都是 `invalid_argument`。`expected_revision` 与
/// `operation_id` 的语义同 `diary.saveEdit`。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MergeDiaryParams {
    #[serde(alias = "baseVersionId")]
    base_version_id: String,
    #[serde(alias = "candidateVersionIds")]
    candidate_version_ids: Vec<String>,
    body: String,
    #[serde(alias = "expectedRevision")]
    expected_revision: i64,
    #[serde(alias = "operationId")]
    operation_id: String,
}

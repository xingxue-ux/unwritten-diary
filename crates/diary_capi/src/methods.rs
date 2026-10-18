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
//! | `recordings.prepare` | `Core::prepare_recording` | `RecordingTicket` |
//! | `recordings.registerSegment` | `Core::register_segment` | `SegmentReceipt` |
//! | `recordings.updateState` | `Core::update_recording_state` | `RecordingSession` |
//! | `recordings.finalize` | `Core::finalize_recording` | `RecordingFinalizeResult` |
//! | `recordings.recover` | `Core::recover_recording` | `RecordingRecovery` |
//! | `sources.extract` | `Core::extract_source` | `ExtractedContent` |
//! | `search.start` | `Core::start_search` | `SearchSnapshot` |
//! | `indexes.rebuild` | `Core::schedule_index_rebuild` | `Job` |
//! | `jobs.runDue` | `Core::run_due_jobs` | `JobRunSummary` |
//! | `jobs.status` | `Core::job_status` | `JobStatus` |
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
//! - 记录与来源：`captures.get` / `captures.list` / `captures.moveDay` /
//!   `captures.trash` / `captures.restore` / `captures.previewPurge` / `captures.purge`、
//!   `sources.extractedContent` / `sources.previousRevision` / `sources.reviseText` /
//!   `sources.locate`；
//! - 文件：`imports.*`、`assets.open` / `assets.release`；
//! - 检索：`search.nextPage` / `search.snapshot` / `search.cancel` / `search.upgrade`、
//!   `indexes.status`、`indexes.rebuildSemantic`；
//! - 任务与事件：`jobs.list` / `jobs.get` / `jobs.retry` / `jobs.cancel` /
//!   `jobs.nextWakeup` / `jobs.acknowledgeAttention`、`core.events`；
//! - 备份与恢复：`backups.create` / `backups.inspect` / `backups.restore`（`diary_bridge`
//!   已经接了同步入口，映射过来即可）；
//! - 其余：`core.snapshot` / `core.close` / `core.setRuntimeState`、`diary.*`、`styles.*`、
//!   `responses.*`、`providers.*`、`settings.*`、`plugins.*`、`exports.*`——这些要么核心
//!   还没有对应实现，要么属于后面的块。
//!
//! 没接的方法一律报 `invalid_argument`，message 里点名「未接入」并附上已接入清单，
//! **不会**静默成功、也不会假装支持。

use chrono::Utc;
use diary_core::model::{NativeRecordingStatus, RecordingState, SegmentManifest};
use diary_core::{
    Core, CoreError, CreateDraftInput, JobState, SearchFilters, SearchMode, SearchRequest,
};
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::CallError;

/// 已经接到桥上的契约方法名（`core.info.capabilities` 就是它）。
pub(crate) const WIRED_METHODS: &[&str] = &[
    "core.info",
    "captures.createDraft",
    "captures.saveDraft",
    "captures.commit",
    "recordings.prepare",
    "recordings.registerSegment",
    "recordings.updateState",
    "recordings.finalize",
    "recordings.recover",
    "sources.extract",
    "search.start",
    "indexes.rebuild",
    "jobs.runDue",
    "jobs.status",
];

/// 一轮 `jobs.runDue` 最多跑几条任务（防御性上限）。
///
/// 契约 `jobs.runDue` 只给毫秒预算、不给条数，所以这个上限由桥接层拍——与
/// `diary_bridge::api::RUN_DUE_MAX_JOBS` 同一个值、同一个理由：预算挡长尾，
/// 它挡「队列很长 + 多条短任务」把一轮拖成没完没了。没跑上的任务留在队列里、
/// 如实计入摘要的 `dueBacklog`，下一轮接着跑。
const RUN_DUE_MAX_JOBS: usize = 64;

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
        // ------------------------------------------------------------ 索引
        "indexes.rebuild" => {
            let input: RebuildIndexesParams = parse(params)?;
            let job =
                core.schedule_index_rebuild(&input.scope, &input.reason, &input.operation_id)?;
            to_value(job)
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

fn to_value<T: serde::Serialize>(value: T) -> Result<Value, CallError> {
    // 核心模型的字段都是 JSON 表达得了的；真序列化不了说明数据本身有问题，如实报错。
    serde_json::to_value(value)
        .map_err(|error| CallError::invalid_state(format!("C ABI：结果序列化失败（{error}）")))
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

/// `search.start`：`SearchRequest` + `queryRevision`（契约第 4.4 节）。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchStartParams {
    request: SearchRequestParams,
    #[serde(default, alias = "queryRevision")]
    query_revision: i64,
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

/// `jobs.runDue`：毫秒预算 + 唤醒来源（契约第 4.4 节）。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RunDueParams {
    #[serde(alias = "budgetMs")]
    budget_ms: u64,
    trigger: String,
}

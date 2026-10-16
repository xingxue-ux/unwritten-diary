//! 契约第 2 节的数据对象（B1a 用到的部分）与调用结果。

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// 记录状态，契约第 3 节的草稿状态机。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureState {
    Draft,
    Committed,
    Trashed,
}

impl CaptureState {
    pub fn wire(self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Committed => "committed",
            Self::Trashed => "trashed",
        }
    }

    pub fn from_wire(value: &str) -> Option<Self> {
        match value {
            "draft" => Some(Self::Draft),
            "committed" => Some(Self::Committed),
            "trashed" => Some(Self::Trashed),
            _ => None,
        }
    }
}

/// 原始内容的作者，契约第 2.2 节。AI 生成的正文不写成 user。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthorType {
    User,
    Import,
}

impl AuthorType {
    pub fn wire(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Import => "import",
        }
    }

    pub fn from_wire(value: &str) -> Option<Self> {
        match value {
            "user" => Some(Self::User),
            "import" => Some(Self::Import),
            _ => None,
        }
    }
}

/// 未处理 / 处理中 / 可检索 / 需关注的数量。
///
/// B1a 还没有提取与索引，所以这些数字现在恒为 0；等 B1b/B2 接上后再算。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ProcessingSummary {
    pub unprocessed: u32,
    pub processing: u32,
    pub searchable: u32,
    pub needs_attention: u32,
}

/// 一次混合记录，契约第 2.1 节。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capture {
    pub id: String,
    pub revision: i64,
    pub state: CaptureState,
    pub occurred_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub time_zone: String,
    pub utc_offset_minutes: i32,
    pub day_key: String,
    pub ordered_source_ids: Vec<String>,
    pub draft_text: String,
    pub processing_summary: ProcessingSummary,
}

/// 一条记录里的原始内容条目，契约第 2.2 节。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceItem {
    pub source_id: String,
    pub capture_id: String,
    pub kind: String,
    pub position: i64,
    pub current_revision_id: Option<String>,
}

/// 原始文字或来源元数据的某一次修订。旧版本永远保留。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceRevision {
    pub revision_id: String,
    pub source_id: String,
    pub parent_revision_id: Option<String>,
    pub text: Option<String>,
    pub asset_id: Option<String>,
    pub author_type: AuthorType,
    pub occurred_at: DateTime<Utc>,
}

/// 草稿保存结果。只有 `durable` 为真，界面才能显示「已保存」。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DraftSaveResult {
    pub revision: i64,
    pub durable: bool,
    pub saved_at: DateTime<Utc>,
}

/// 提交结果：记录本身与提交时创建的原始文字版本。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitResult {
    pub capture: Capture,
    pub original_text_revision: Option<SourceRevision>,
}

/// 记录分页结果。cursor 对调用方不透明。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapturePage {
    pub captures: Vec<Capture>,
    pub next_cursor: Option<String>,
}

/// 持久业务事件类型，契约第 6 节的完整清单。
///
/// 现在实际会发的是 capture.changed、asset.changed 与 job.changed；
/// 其余几种等对应的切片接上（日记、索引、插件）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EventType {
    CaptureChanged,
    AssetChanged,
    DiaryVersionCreated,
    DiaryCurrentChanged,
    JobChanged,
    IndexCoverageChanged,
    PluginChanged,
}

impl EventType {
    pub fn wire(self) -> &'static str {
        match self {
            Self::CaptureChanged => "capture.changed",
            Self::AssetChanged => "asset.changed",
            Self::DiaryVersionCreated => "diary.versionCreated",
            Self::DiaryCurrentChanged => "diary.currentChanged",
            Self::JobChanged => "job.changed",
            Self::IndexCoverageChanged => "index.coverageChanged",
            Self::PluginChanged => "plugin.changed",
        }
    }

    pub fn from_wire(value: &str) -> Option<Self> {
        match value {
            "capture.changed" => Some(Self::CaptureChanged),
            "asset.changed" => Some(Self::AssetChanged),
            "diary.versionCreated" => Some(Self::DiaryVersionCreated),
            "diary.currentChanged" => Some(Self::DiaryCurrentChanged),
            "job.changed" => Some(Self::JobChanged),
            "index.coverageChanged" => Some(Self::IndexCoverageChanged),
            "plugin.changed" => Some(Self::PluginChanged),
            _ => None,
        }
    }
}

/// 持久业务事件。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DomainEvent {
    pub event_id: String,
    /// 单调递增的游标序号。
    pub sequence: i64,
    #[serde(rename = "type")]
    pub event_type: EventType,
    pub entity_id: String,
    pub revision: i64,
    pub emitted_at: DateTime<Utc>,
}
/// 导入状态，契约第 3 节「导入」行。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportState {
    Prepared,
    Copying,
    Verifying,
    Ready,
    /// 进程中断，暂存文件可能还在，等调用方决定重试或取消。
    Recoverable,
    Error,
}

impl ImportState {
    pub fn wire(self) -> &'static str {
        match self {
            Self::Prepared => "prepared",
            Self::Copying => "copying",
            Self::Verifying => "verifying",
            Self::Ready => "ready",
            Self::Recoverable => "recoverable",
            Self::Error => "error",
        }
    }

    pub fn from_wire(value: &str) -> Option<Self> {
        match value {
            "prepared" => Some(Self::Prepared),
            "copying" => Some(Self::Copying),
            "verifying" => Some(Self::Verifying),
            "ready" => Some(Self::Ready),
            "recoverable" => Some(Self::Recoverable),
            "error" => Some(Self::Error),
            _ => None,
        }
    }

    /// 还没结束的导入（重启后要处理的那批）。
    pub fn is_in_flight(self) -> bool {
        matches!(self, Self::Prepared | Self::Copying | Self::Verifying)
    }
}

/// 资产在文件库中的状态，契约第 2.3 节。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssetStorageState {
    Importing,
    Ready,
    Recoverable,
    Missing,
    Trashed,
}

impl AssetStorageState {
    pub fn wire(self) -> &'static str {
        match self {
            Self::Importing => "importing",
            Self::Ready => "ready",
            Self::Recoverable => "recoverable",
            Self::Missing => "missing",
            Self::Trashed => "trashed",
        }
    }

    pub fn from_wire(value: &str) -> Option<Self> {
        match value {
            "importing" => Some(Self::Importing),
            "ready" => Some(Self::Ready),
            "recoverable" => Some(Self::Recoverable),
            "missing" => Some(Self::Missing),
            "trashed" => Some(Self::Trashed),
            _ => None,
        }
    }
}

/// 资产来源方式，契约第 2.3 节。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportOrigin {
    Picker,
    Camera,
    Paste,
    Drop,
    Share,
    Recording,
    Restore,
}

impl ImportOrigin {
    pub fn wire(self) -> &'static str {
        match self {
            Self::Picker => "picker",
            Self::Camera => "camera",
            Self::Paste => "paste",
            Self::Drop => "drop",
            Self::Share => "share",
            Self::Recording => "recording",
            Self::Restore => "restore",
        }
    }

    pub fn from_wire(value: &str) -> Option<Self> {
        match value {
            "picker" => Some(Self::Picker),
            "camera" => Some(Self::Camera),
            "paste" => Some(Self::Paste),
            "drop" => Some(Self::Drop),
            "share" => Some(Self::Share),
            "recording" => Some(Self::Recording),
            "restore" => Some(Self::Restore),
            _ => None,
        }
    }
}

/// 导入票据。`staging_ticket` 是平台层唯一被允许写入的位置。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportTicket {
    pub import_id: String,
    pub staging_ticket: String,
    pub max_bytes: Option<i64>,
}

/// 复制完成信息：调用方声明它写了多少、算出的哈希是什么。
///
/// 核心**不会**直接相信这些数字，它会自己流式重算一遍再比对。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportManifest {
    pub copied_bytes: i64,
    pub sha256: String,
    pub detected_mime: String,
    pub original_name: String,
}

/// 导入状态。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportStatus {
    pub import_id: String,
    pub state: ImportState,
    pub copied_bytes: i64,
    pub total_bytes: i64,
    pub asset_id: Option<String>,
    pub error_code: Option<String>,
    pub message: Option<String>,
}

/// 文件库资产，契约第 2.3 节。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Asset {
    pub id: String,
    pub sha256: String,
    pub object_ref: String,
    pub original_name: String,
    pub detected_mime: String,
    pub byte_size: i64,
    pub storage_state: AssetStorageState,
    pub import_origin: ImportOrigin,
    pub created_at: DateTime<Utc>,
    pub media_duration_ms: Option<i64>,
    pub width: Option<i64>,
    pub height: Option<i64>,
}

/// 只读资产租约。`handle` 是应用私有目录里的绝对路径，不是外部来源路径。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetLease {
    pub lease_id: String,
    pub asset_id: String,
    pub usage: String,
    pub handle: String,
    pub expires_at: DateTime<Utc>,
    pub byte_size: i64,
}

/// 录音状态，契约第 3 节「录音」行。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordingState {
    Idle,
    Preparing,
    Recording,
    Paused,
    Stopping,
    Saved,
    /// 被来电、系统回收等打断。**不是** paused：界面不能继续显示正在收音。
    Interrupted,
    /// 有已封闭片段可以救回来，但可能缺尾巴。
    Recoverable,
    Error,
}

impl RecordingState {
    pub fn wire(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Preparing => "preparing",
            Self::Recording => "recording",
            Self::Paused => "paused",
            Self::Stopping => "stopping",
            Self::Saved => "saved",
            Self::Interrupted => "interrupted",
            Self::Recoverable => "recoverable",
            Self::Error => "error",
        }
    }

    pub fn from_wire(value: &str) -> Option<Self> {
        match value {
            "idle" => Some(Self::Idle),
            "preparing" => Some(Self::Preparing),
            "recording" => Some(Self::Recording),
            "paused" => Some(Self::Paused),
            "stopping" => Some(Self::Stopping),
            "saved" => Some(Self::Saved),
            "interrupted" => Some(Self::Interrupted),
            "recoverable" => Some(Self::Recoverable),
            "error" => Some(Self::Error),
            _ => None,
        }
    }

    /// 是否还在进行中（重启后需要收拾的那批）。
    pub fn is_open(self) -> bool {
        matches!(
            self,
            Self::Preparing | Self::Recording | Self::Paused | Self::Stopping | Self::Interrupted
        )
    }
}

/// 一个已封闭片段的描述，契约第 4.2 节。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SegmentManifest {
    pub segment_id: String,
    /// 相对暂存目录的文件名，不是绝对路径。
    pub relative_file_name: String,
    pub duration_ms: i64,
    pub sha256: String,
    pub byte_size: i64,
    pub closed_at: Option<DateTime<Utc>>,
}

/// 片段登记回执。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SegmentReceipt {
    pub segment_id: String,
    pub segment_index: i64,
    pub durable: bool,
    pub recorded_at: DateTime<Utc>,
}

/// 录音票据。`staging_ticket` 是平台层唯一被允许写入的目录。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordingTicket {
    pub recording_id: String,
    pub asset_id: String,
    pub staging_ticket: String,
}

/// 原生上报的录音状态。后端不虚构麦克风状态。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeRecordingStatus {
    pub recording_id: String,
    pub state: RecordingState,
    pub wall_clock: DateTime<Utc>,
    pub elapsed_ms: i64,
    /// 已经持久化的位置，恢复时用来判断缺口。
    pub persisted_through_ms: i64,
    pub input_device_changed: bool,
    pub message: Option<String>,
}

/// 录音会话快照。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordingSession {
    pub recording_id: String,
    pub asset_id: String,
    pub state: RecordingState,
    pub elapsed_ms: i64,
    pub durable_through_ms: i64,
    pub segment_count: i64,
    pub last_error: Option<String>,
}

/// 最终化结果。转写不阻塞完成。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordingFinalizeResult {
    pub recording_id: String,
    pub asset_id: String,
    pub state: RecordingState,
    pub segment_count: i64,
    pub transcription_queued: bool,
    pub logical_duration_ms: i64,
}

/// 恢复结果：不假设最后一段完好，明确报告缺口。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordingRecovery {
    pub recording_id: Option<String>,
    pub state: RecordingState,
    pub closed_segment_indexes: Vec<i64>,
    /// 已知缺口的总时长；无法估计时为 0 并在 notes 里说明。
    pub gap_ms: i64,
    pub notes: Vec<String>,
}

/// 任务状态，契约第 3 节「任务」行。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Queued,
    Running,
    Succeeded,
    RetryWait,
    WaitingConfiguration,
    WaitingDependency,
    WaitingNetwork,
    Failed,
    Cancelled,
}

impl JobState {
    pub fn wire(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::RetryWait => "retry_wait",
            Self::WaitingConfiguration => "waiting_configuration",
            Self::WaitingDependency => "waiting_dependency",
            Self::WaitingNetwork => "waiting_network",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn from_wire(value: &str) -> Option<Self> {
        match value {
            "queued" => Some(Self::Queued),
            "running" => Some(Self::Running),
            "succeeded" => Some(Self::Succeeded),
            "retry_wait" => Some(Self::RetryWait),
            "waiting_configuration" => Some(Self::WaitingConfiguration),
            "waiting_dependency" => Some(Self::WaitingDependency),
            "waiting_network" => Some(Self::WaitingNetwork),
            "failed" => Some(Self::Failed),
            "cancelled" => Some(Self::Cancelled),
            _ => None,
        }
    }

    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Cancelled)
    }
}

/// 任务优先级，任务书 7.1 节。数字越大越先做。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobPriority {
    /// 保存与录音片段登记最高。
    SaveAndRecording,
    UserSearch,
    UserOrganize,
    BackgroundExtract,
    AutoOrganize,
    Maintenance,
}

impl JobPriority {
    pub fn value(self) -> i64 {
        match self {
            Self::SaveAndRecording => 60,
            Self::UserSearch => 50,
            Self::UserOrganize => 40,
            Self::BackgroundExtract => 30,
            Self::AutoOrganize => 20,
            Self::Maintenance => 10,
        }
    }
}

/// 任务进度。没有可靠进度时为空，不能虚构百分比。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobProgress {
    pub completed: i64,
    pub total: i64,
}

/// 持久任务，契约第 2.7 节。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Job {
    pub id: String,
    pub kind: String,
    pub state: JobState,
    pub priority: i64,
    pub target_ids: Vec<String>,
    pub input_snapshot_hash: Option<String>,
    pub progress: Option<JobProgress>,
    pub attempt_count: i64,
    pub max_attempts: i64,
    pub next_attempt_at: Option<DateTime<Utc>>,
    pub error_code: Option<String>,
    pub requires_user_action: bool,
    pub attention_key: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// 入队参数。
///
/// 没有 `business_key` 字段：业务键由 `(kind, target_ids, input_snapshot_hash)`
/// 拼出来，队列在同一个事务里按它查重（见 `jobs::enqueue_in_tx`）。
/// 这一片不改 schema，DB 级的唯一索引留给下一次 schema 变更一起做。
#[derive(Debug, Clone)]
pub struct NewJob {
    pub kind: String,
    pub priority: JobPriority,
    pub target_ids: Vec<String>,
    pub input_snapshot_hash: Option<String>,
    pub max_attempts: i64,
    /// 首次到期时刻。为空（默认）表示入队即到期。
    ///
    /// 为什么要有它：E29 的「补做」需要一个真实的应跑时刻。平台在崩溃恢复或
    /// 系统唤醒后重建任务时可能已经知道它本该更早跑，记下来才可能如实回答
    /// 「这次是补做」而不是「准时完成」。
    pub next_attempt_at: Option<DateTime<Utc>>,
}

impl NewJob {
    pub fn new(kind: impl Into<String>, priority: JobPriority) -> Self {
        Self {
            kind: kind.into(),
            priority,
            target_ids: Vec::new(),
            input_snapshot_hash: None,
            max_attempts: 5,
            next_attempt_at: None,
        }
    }

    pub fn targeting(mut self, ids: Vec<String>) -> Self {
        self.target_ids = ids;
        self
    }

    pub fn with_snapshot(mut self, hash: impl Into<String>) -> Self {
        self.input_snapshot_hash = Some(hash.into());
        self
    }

    /// 指定首次到期时刻（例如「本该在半小时前跑」的补做任务）。
    pub fn scheduled_at(mut self, at: DateTime<Utc>) -> Self {
        self.next_attempt_at = Some(at);
        self
    }
}

/// 一次「跑到期任务」的结果摘要。
///
/// 数字都是真实统计：**没有跑的任务绝不会被算成跑过**；被预算或条数上限截住的
/// 到期任务在 `due_backlog` 里如实报出来，在等前置的任务在 `waiting_dependency`
/// 里如实报出来。契约 `runDueJobs` 的返回值就是它（`packages/diary_api` 的
/// `DueRunSummary` 字段与这里一一对应）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobRunSummary {
    /// 本次运行开始的时刻（调用方给的 `now`）。「补做」以它为界。
    pub started_at: DateTime<Utc>,
    /// 唤醒来源，调用方给什么就如实记什么（例如 startup / foreground / wake / manual）。
    /// 核心只用它做观察与排查，不用它改变行为。
    pub trigger: String,
    /// 本次领取并执行的任务数，不超过调用方给的条数上限。
    pub processed: i64,
    /// 其中执行成功、进入 `succeeded` 的数量。
    pub succeeded: i64,
    /// 其中用完额度或没有处理器、进入「已放弃」（`failed`）的数量。
    pub failed: i64,
    /// 其中这次失败但还有额度、已经排了退避重试（`retry_wait`）的数量。
    pub retrying: i64,
    /// 其中属于「补做」的数量：领取时它的 `next_attempt_at` 已经早于 `started_at`。
    ///
    /// 口径与理由见 `jobs::run_due`；我们只说「补做」，不说「准时完成」。
    pub backfilled: i64,
    /// 运行开始时回收的「上次没跑完」（`state=running`）任务数。
    pub recovered: i64,
    /// 本次运行结束后仍然到期、但这次没跑上的任务数（被预算或条数上限截住）。
    ///
    /// `due_backlog > 0` 就说明这一轮是**提前收手**的（不是没有活了）；预算耗尽
    /// 不是一个单独的字段，因为它和这个数字说的是一件事，留两个字段只会互相矛盾。
    pub due_backlog: i64,
    /// 本次运行结束时还在等前置条件的任务数（现在只有 `index` 等 `extract`）。
    ///
    /// 这些任务**没有**被领取、没有消耗重试额度，所以也不在 `processed` 里。
    pub waiting_dependency: i64,
    /// 本次运行里被**闸门**判定「前置不可能再满足」而终态失败的任务数。
    ///
    /// 这些任务同样不在 `processed` 里（一行执行者代码都没跑）；它们是被闸门在
    /// 事务里落成「已放弃」的，`job_status().failed` / `recent_errors` 里看得到。
    /// 单独报出来，是为了让摘要不必把「这一轮什么都没跑」说成「一切正常」。
    pub dependency_failed: i64,
    /// 下一个到期时刻；没有待办时为 None。等前置到期的任务不算「待办」：
    /// 它们要等的是别人跑完，不是时间到（见 `jobs::next_wakeup`）。
    pub next_wakeup: Option<DateTime<Utc>>,
}

/// 任务面板里的一行最近错误。
///
/// **只带错误码与时间，不带任何用户内容**：`job_attempts.message` 里可能有文件
/// 路径或原始报错，那是排查用的，故意不往外给。`job_id` 是不透明 ID，供人工重试
/// 时定位，不是用户内容。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobErrorSummary {
    pub job_id: String,
    pub kind: String,
    pub error_code: String,
    /// 这个错误最后一次出现的时刻（任务的 `updated_at`）。
    pub occurred_at: DateTime<Utc>,
    /// 是否已经放弃自动重试、在等用户处理。
    pub requires_user_action: bool,
}

/// 任务状态摘要，任务面板用。
///
/// `indexes.status` 只报索引覆盖，不报任务数；「排队中/进行中/失败可重试/已放弃」
/// 这些数字由这里给。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobStatus {
    pub queued: i64,
    pub running: i64,
    pub retry_wait: i64,
    pub waiting_configuration: i64,
    pub waiting_dependency: i64,
    pub waiting_network: i64,
    /// 「已放弃」：尝试用完或没有处理器，等用户决定要不要重试。
    pub failed: i64,
    pub cancelled: i64,
    pub succeeded: i64,
    /// 到期积压：现在就该跑、但还没跑的任务数（含退避已经到点的重试）。
    ///
    /// 与 `queued + retry_wait` 的区别：后者包含排在未来、还没到点的任务。
    pub due_backlog: i64,
    /// 建议的下次唤醒时刻；没有待办时为 None。
    pub next_wakeup: Option<DateTime<Utc>>,
    /// 最近错误，最新在前，最多 `jobs::RECENT_ERROR_LIMIT` 条。
    pub recent_errors: Vec<JobErrorSummary>,
}

/// 覆盖程度，契约第 2.4 节。用户可见原因放在 coverage_reason 里，不靠枚举表达。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Coverage {
    Complete,
    Partial,
    MetadataOnly,
    Unavailable,
}

impl Coverage {
    pub fn wire(self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::Partial => "partial",
            Self::MetadataOnly => "metadata_only",
            Self::Unavailable => "unavailable",
        }
    }

    pub fn from_wire(value: &str) -> Option<Self> {
        match value {
            "complete" => Some(Self::Complete),
            "partial" => Some(Self::Partial),
            "metadata_only" => Some(Self::MetadataOnly),
            "unavailable" => Some(Self::Unavailable),
            _ => None,
        }
    }
}

/// 派生内容的处理状态。取值沿用契约第 3 节「索引」行。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessingStatus {
    Pending,
    Processing,
    Ready,
    Partial,
    Failed,
    Stale,
}

impl ProcessingStatus {
    pub fn wire(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Processing => "processing",
            Self::Ready => "ready",
            Self::Partial => "partial",
            Self::Failed => "failed",
            Self::Stale => "stale",
        }
    }

    pub fn from_wire(value: &str) -> Option<Self> {
        match value {
            "pending" => Some(Self::Pending),
            "processing" => Some(Self::Processing),
            "ready" => Some(Self::Ready),
            "partial" => Some(Self::Partial),
            "failed" => Some(Self::Failed),
            "stale" => Some(Self::Stale),
            _ => None,
        }
    }
}

/// 定位类型，契约第 2.4 节。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LocatorType {
    TextRange,
    Audio,
    Video,
    Document,
    Image,
    File,
}

impl LocatorType {
    pub fn wire(self) -> &'static str {
        match self {
            Self::TextRange => "text_range",
            Self::Audio => "audio",
            Self::Video => "video",
            Self::Document => "document",
            Self::Image => "image",
            Self::File => "file",
        }
    }

    pub fn from_wire(value: &str) -> Option<Self> {
        match value {
            "text_range" => Some(Self::TextRange),
            "audio" => Some(Self::Audio),
            "video" => Some(Self::Video),
            "document" => Some(Self::Document),
            "image" => Some(Self::Image),
            "file" => Some(Self::File),
            _ => None,
        }
    }
}

/// 定位信息。每个 locator 都带 sourceRevisionId，保证定位到当时的原文版本。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceLocator {
    pub locator_type: LocatorType,
    pub source_revision_id: String,
    /// 文字区间：Unicode 标量值计数，左闭右开。
    pub text_start: Option<i64>,
    pub text_end: Option<i64>,
    pub start_ms: Option<i64>,
    pub end_ms: Option<i64>,
    /// 页码从 1 开始；无法可靠定位时为 None，不伪造。
    pub page_number: Option<i64>,
    pub block_id: Option<String>,
    /// 图片归一化矩形。
    pub rect: Option<[f64; 4]>,
    pub asset_id: Option<String>,
}

impl SourceLocator {
    pub fn text_range(source_revision_id: &str, start: i64, end: i64) -> Self {
        Self {
            locator_type: LocatorType::TextRange,
            source_revision_id: source_revision_id.to_owned(),
            text_start: Some(start),
            text_end: Some(end),
            start_ms: None,
            end_ms: None,
            page_number: None,
            block_id: None,
            rect: None,
            asset_id: None,
        }
    }

    pub fn document(source_revision_id: &str, page_number: Option<i64>, block_id: Option<String>) -> Self {
        Self {
            locator_type: LocatorType::Document,
            source_revision_id: source_revision_id.to_owned(),
            text_start: None,
            text_end: None,
            start_ms: None,
            end_ms: None,
            page_number,
            block_id,
            rect: None,
            asset_id: None,
        }
    }
}

/// 派生内容中的一个片段。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExtractedSegment {
    pub ordinal: i64,
    pub text: String,
    pub locator: SourceLocator,
}

/// 可重建的派生内容，与原件分开保存，契约第 2.4 节。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExtractedContent {
    pub id: String,
    pub source_id: String,
    pub source_revision_id: String,
    pub extractor_id: String,
    pub extractor_version: String,
    pub text: String,
    pub segments: Vec<ExtractedSegment>,
    pub status: ProcessingStatus,
    pub coverage: Coverage,
    /// 用户可读的覆盖原因，例如「没有 OCR 能力，仅文件信息可搜」。
    pub coverage_reason: Option<String>,
    pub error_code: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// 定位结果：前端能否打开原件，以及不能打开时的真实原因。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceLocation {
    pub source_ref: String,
    pub locator: SourceLocator,
    pub available: bool,
    pub asset_id: Option<String>,
    pub reason: Option<String>,
}

/// 索引覆盖状态，契约第 4.4 节 `indexes.status`。
///
/// 这个状态把两条路分开报：**关键词这一路**（`coverage`、`keyword_index_ready`、
/// 各种 `*_segments` / `*_captures`）与**语义这一路**（`semantic_index_ready`、
/// `model_version`、`total_chunks` / `embedded_chunks`）。语义这一路的每个数字都是
/// **算出来的真值**：没有一代向量生效时 `model_version` 是 `None` 而不是一个假装
/// 的字符串，`semantic_index_ready` 要求生效代次真的覆盖了范围内的每一块。
///
/// 两条路**不共用一个 `coverage`**：`coverage` 仍然只描述关键词索引的覆盖程度。
/// 在检索会话里，`semantic` 模式的 `index_coverage` 才按语义那一路的覆盖算
/// （见 `search_session`）。
///
/// 数字的单位不完全一样：片段（`*_segments`）、记录数（`*_captures`）、材料数
/// （`failed_sources`）、块数（`*_chunks`）各自的名字里写清楚，避免前端把它们相加。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IndexStatus {
    pub coverage: Coverage,
    pub keyword_index_ready: bool,
    pub semantic_index_ready: bool,
    /// 建索引时用的分词器版本；与库里行不一致的片段或记录文字会被算成待重建。
    pub tokenizer_version: String,
    /// 语义索引用的模型版本：**生效代次**里所有向量共有的那个 `chunk_vectors.model_version`
    /// （含模型文件的 sha256 前缀）。
    ///
    /// 三种情况是 `None`：还没有生效代次；生效代次里一行向量都没有；生效代次里混了
    /// **不止一种**模型版本（历史遗留的混合代次）——后一种没有单一版本可报，此时
    /// `semantic_index_ready` 为 `false`，`reasons` 里会点名口径不一致并提示重建。
    /// 正常操作（`build_semantic_index`）不会再产出混合代次：续跑前核对口径，
    /// 不一致就整代废弃按当前模型重算。
    pub model_version: Option<String>,
    /// 切块规则版本（`chunker::CHUNKER_VERSION`）。
    ///
    /// B3c-1 里它还是 `None`（那时没有任何块进索引）；从这一片起块真的进库了，所以
    /// 它有值。库里块的 `chunker_version` 与它不一致，就说明块要按新规则重算。
    pub chunker_version: Option<String>,
    /// 范围内的文本块数（`text_chunks`）。
    ///
    /// 范围语义与关键词那一路一致：一条记录属于范围，当且仅当它拥有范围内的来源；
    /// 空白范围是「什么都不看」，得到 0。单位是块数，不是篇数——一块可能含多篇。
    pub total_chunks: i64,
    /// 其中有**当前生效代次**向量的块数（`chunk_vectors.generation = active_generation`）。
    ///
    /// 它是 `total_chunks` 的下界：两者相等才谈得上「语义索引覆盖完了」。没有模型、
    /// 或还没建过语义索引时是 0——那是算出来的（没有可算的代次），不是写死的。
    pub embedded_chunks: i64,
    /// 已进关键词索引的片段数。
    pub indexed_segments: i64,
    /// 派生内容里的片段总数。
    pub total_segments: i64,
    /// 还没进索引的片段数（未索引 + 分词器版本过期）。
    pub pending_segments: i64,
    /// 分词器版本过期、需要重建的片段数（已包含在 pending 里）。
    pub stale_segments: i64,
    /// 已进关键词索引的记录文字条数（`captures.draft_text`）。
    ///
    /// 只数当前分词器版本的行，所以「索引行还在、版本过期」不算已索引；
    /// 进回收站的记录也不算，它们本来就不参与检索。
    pub indexed_captures: i64,
    /// 有文字、未进回收站的记录数（`trim` 后非空才算有文字）。
    ///
    /// 它是 `indexed_captures` 的上界：两者不等就说明有用户自己写的话还搜不到，
    /// `coverage` 因此不会是 `complete`。单位是记录数，不是片段数。
    pub total_captures: i64,
    /// 正文解析失败的材料数（这些材料没有片段可索引）。
    pub failed_sources: i64,
    /// 已索引文档（片段 + 记录文字）的正文字符总数，用来算索引体积的每字符代价。
    pub indexed_chars: i64,
    /// 索引倒排表的行数（片段与记录文字的词项行都算在内）。
    pub index_rows: i64,
    /// 索引里不同词项的个数（片段与记录文字的并集）。
    pub index_terms: i64,
    /// **整库**索引表实际占用的字节数（来自 dbstat）。
    ///
    /// 它把**两条路的表都算进去**：关键词那几张（`search_*`）+ 块与向量
    /// （`text_chunks` / `chunk_spans` / `chunk_vectors` / `index_meta` 及它们的索引）。
    /// 只算关键词那几张会把占用低估一大块——512 维 f32 向量是每块约 2 KiB。
    ///
    /// 它不随 `source_scope` 变化：dbstat 按表汇总页数，页在来源之间共享，拆不出来。
    /// 范围查询里它仍是整库值，并在 `reasons` 里写明。读不到 dbstat 时是 0，同样在
    /// `reasons` 里说明。
    pub index_bytes: i64,
    /// 用户可读的原因说明。
    pub reasons: Vec<String>,
}

/// 检索模式，契约第 2.6 节。三种都实现了：`Hybrid` 是 RRF 融合（#51，B3c-3），
/// **分两阶段给**（#69）——`start_search` 先只算关键词那一路并报 `keyword_ready`，
/// 显式调用 `upgrade_search_to_hybrid(sessionId)` 才融合并报 `hybrid_ready`。
/// 语义未就绪时**退化为关键词并在 `warnings` 里明说**（不静默，也不假装跑过混合）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchMode {
    Keyword,
    Semantic,
    Hybrid,
}

impl SearchMode {
    pub fn wire(self) -> &'static str {
        match self {
            Self::Keyword => "keyword",
            Self::Semantic => "semantic",
            Self::Hybrid => "hybrid",
        }
    }

    pub fn from_wire(value: &str) -> Option<Self> {
        match value {
            "keyword" => Some(Self::Keyword),
            "semantic" => Some(Self::Semantic),
            "hybrid" => Some(Self::Hybrid),
            _ => None,
        }
    }
}

/// 检索阶段，契约第 2.6 节。
///
/// 混合检索的阶段流程（#69）：`start_search(mode = hybrid)` → `keyword_ready`
/// （只有关键词那一路的结果）→ `upgrade_search_to_hybrid(sessionId)` → `hybrid_ready`
/// （融合 + 重排后的结果）→ 翻完最后一页 `done`。语义拿不出证据时升级**不会**给出
/// `hybrid_ready`，阶段如实停在 `keyword_ready`。取消 → `cancelled`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchPhase {
    Initial,
    KeywordReady,
    HybridReady,
    Done,
    Cancelled,
}

impl SearchPhase {
    pub fn wire(self) -> &'static str {
        match self {
            Self::Initial => "initial",
            Self::KeywordReady => "keyword_ready",
            Self::HybridReady => "hybrid_ready",
            Self::Done => "done",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn from_wire(value: &str) -> Option<Self> {
        match value {
            "initial" => Some(Self::Initial),
            "keyword_ready" => Some(Self::KeywordReady),
            "hybrid_ready" => Some(Self::HybridReady),
            "done" => Some(Self::Done),
            "cancelled" => Some(Self::Cancelled),
            _ => None,
        }
    }
}

/// 一条命中是靠什么匹配上的，契约第 2.6 节。关键词那一路全是 `Keyword`，语义那一路
/// 全是 `Semantic`；混合那一路**如实**标：两路都有就是 `[Keyword, Semantic]`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MatchedBy {
    Keyword,
    Semantic,
    Metadata,
}

impl MatchedBy {
    pub fn wire(self) -> &'static str {
        match self {
            Self::Keyword => "keyword",
            Self::Semantic => "semantic",
            Self::Metadata => "metadata",
        }
    }

    pub fn from_wire(value: &str) -> Option<Self> {
        match value {
            "keyword" => Some(Self::Keyword),
            "semantic" => Some(Self::Semantic),
            "metadata" => Some(Self::Metadata),
            _ => None,
        }
    }
}

/// 材料类型，契约第 2.3 节的 SourceKind。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    Text,
    Image,
    Audio,
    Video,
    File,
    Link,
}

impl SourceKind {
    pub fn wire(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Image => "image",
            Self::Audio => "audio",
            Self::Video => "video",
            Self::File => "file",
            Self::Link => "link",
        }
    }

    /// 认不出的类型落到 `file`——这是「有原件但不知道是什么」最接近的诚实答案，
    /// 而不是丢掉这条命中。
    pub fn from_wire(value: &str) -> Self {
        match value {
            "text" => Self::Text,
            "image" => Self::Image,
            "audio" => Self::Audio,
            "video" => Self::Video,
            "link" => Self::Link,
            _ => Self::File,
        }
    }
}

/// 命中里的高亮区间，**下标相对 `snippet`**，不是相对整段正文。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextRange {
    pub start: i64,
    pub end: i64,
}

/// 检索过滤条件，契约第 2.6 节。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SearchFilters {
    pub from_day_key: Option<String>,
    pub to_day_key: Option<String>,
    pub kinds: Vec<SourceKind>,
    /// 限定来源范围。`None` 才是不限；`Some([])` 是「什么都不看」（与 `indexes.status`
/// 的空范围同义），不是「看全部」。
    pub source_scope: Option<Vec<String>>,
    pub include_old_diary_versions: bool,
    pub include_trashed: bool,
}

/// 一次检索请求，契约第 2.6 节。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchRequest {
    /// 用户原样输入，是普通检索文字，不直接当 FTS 表达式执行。
    pub query: String,
    pub mode: SearchMode,
    pub filters: SearchFilters,
    pub page_size: i64,
}

/// 一条检索命中，契约第 2.6 节。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchHit {
    /// 稳定标识：直接用片段 ID。同一份内容在不同页里出现时是同一个值。
    pub hit_id: String,
    /// 同一来源的命中归到一组，避免一份长材料占满结果。
    pub group_id: String,
    pub source_kind: SourceKind,
    pub matched_by: Vec<MatchedBy>,
    pub coverage: Coverage,
    pub source_id: Option<String>,
    pub revision_id: Option<String>,
    pub day_key: Option<String>,
    /// 原件名（有资产时）；纯文字来源没有名字。
    pub title: Option<String>,
    /// 命中位置附近的摘录。
    pub snippet: Option<String>,
    /// 高亮区间，下标相对 `snippet`。
    pub highlights: Vec<TextRange>,
    pub locator: Option<SourceLocator>,
}

/// 检索会话的完整快照，契约第 2.6 节。
///
/// 每次返回的是**当前页的完整快照**，前端按 `hit_id` 更新，不要把两份快照
/// 当增量拼接。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchSnapshot {
    pub session_id: String,
    pub query_revision: i64,
    pub phase: SearchPhase,
    pub results: Vec<SearchHit>,
    pub cursor: Option<String>,
    pub index_coverage: Coverage,
    pub warnings: Vec<String>,
}

// ------------------------------------------------------------ 远端服务（Provider）

/// 远端服务能力，契约第 2.7 节。
///
/// **诚实声明**：这里只是「这一类能力」的集合，不代表核心内置的适配器都实现了。
/// 一个能力要真的能被排任务，还要适配器有实现、且（测过之后）被连接测试验过——
/// 口径见 `docs/architecture/m3-Provider-配置与能力.md`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderCapability {
    /// 文本生成。
    GenerateText,
    /// 音频转写。
    TranscribeAudio,
    /// 图像理解。内置适配器**还没有**实现它（插件模型能力在 M4）。
    UnderstandImage,
}

impl ProviderCapability {
    pub fn wire(self) -> &'static str {
        match self {
            Self::GenerateText => "generate_text",
            Self::TranscribeAudio => "transcribe_audio",
            Self::UnderstandImage => "understand_image",
        }
    }

    pub fn from_wire(value: &str) -> Option<Self> {
        match value {
            "generate_text" => Some(Self::GenerateText),
            "transcribe_audio" => Some(Self::TranscribeAudio),
            "understand_image" => Some(Self::UnderstandImage),
            _ => None,
        }
    }

    /// 全部能力（用于「列出这个适配器支持什么」的稳定顺序输出）。
    pub const ALL: [Self; 3] = [Self::GenerateText, Self::TranscribeAudio, Self::UnderstandImage];
}

/// 连接测试状态，契约第 2.7 节。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderTestStatus {
    /// 还没测过。
    Untested,
    /// 最近一次测试通过。
    Ok,
    /// 最近一次测试失败（具体错误码在 `providers.last_error_code`，不进这个对象：
    /// 契约的 Provider 对象没有这个字段，编一个会变成没人认识的形状）。
    Failed,
}

impl ProviderTestStatus {
    pub fn wire(self) -> &'static str {
        match self {
            Self::Untested => "untested",
            Self::Ok => "ok",
            Self::Failed => "failed",
        }
    }

    pub fn from_wire(value: &str) -> Option<Self> {
        match value {
            "untested" => Some(Self::Untested),
            "ok" => Some(Self::Ok),
            "failed" => Some(Self::Failed),
            _ => None,
        }
    }
}

/// 脱敏后的远端服务配置，契约第 2.7 节。
///
/// **没有密钥字段**：`credential_ref` 是凭据存储里的引用，不是密钥本身。
/// 这个对象会原样过桥到 Dart，所以任何密文都不该出现在这里。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Provider {
    pub id: String,
    pub label: String,
    /// 协议适配器标识（例如 `openai_compatible`）。业务层不硬编码供应商。
    pub adapter_id: String,
    pub endpoint: Option<String>,
    pub model: Option<String>,
    /// 配置里声明的能力。空集合表示「什么都还没声明」，不是「什么都能做」。
    pub capabilities: std::collections::HashSet<ProviderCapability>,
    /// 凭据存储里的引用；为空表示还没配密钥。**永远不是密钥明文。**
    pub credential_ref: Option<String>,
    pub enabled: bool,
    pub test_status: ProviderTestStatus,
}

/// 连接测试报告，契约第 4.4 节 `providers.test`。
///
/// `actual_call_made` 是**如实**的：只有真的发出了 HTTP 请求才是 true；能力在
/// 适配器/配置层面就不成立、根本没发请求时是 false。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderTestReport {
    pub provider_id: String,
    /// 用户授权的测试范围里，每项能力的结论。
    pub results: std::collections::HashMap<ProviderCapability, ProviderTestStatus>,
    /// 这次测试是否真的发生了模型调用（发起了 HTTP 请求）。
    pub actual_call_made: bool,
    /// 用户可读的说明：可达性 / 认证 / 模型可用 / 实际能力集 / 下一步。
    pub message: Option<String>,
}

/// 一次主动回应的请求回执，契约第 4.4 节 `responses.request`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResponseTicket {
    pub job_id: String,
    /// 回应对象 id。请求受理时就已经存在（`pending`），生成完成后正文才有内容。
    pub response_id: Option<String>,
}

/// 独立保存的 AI 回应，契约第 4.3 节 `responses.get`。
///
/// AI 回应**不自动成为日记的原始事实**：它只带来源（`capture_ids`）与生成信息
/// （`provider_id` / `model`），正文是模型给的东西。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiResponse {
    pub id: String,
    pub request_text: String,
    pub body: String,
    pub created_at: DateTime<Utc>,
    pub capture_ids: Vec<String>,
    pub provider_id: Option<String>,
    pub model: Option<String>,
}

// ---------------------------------------------------------------- 日记与版本（B4b）

/// 生成日记的原因，契约 4.3 节 `diary.generate` 的入参。
///
/// 它进幂等指纹（同一个 operationId 换了原因就是换了内容），也进快照的审计信息。
/// `Regenerate` 是唯一被允许「同一份输入再产一版」的原因——用户明确说了要再来一版，
/// 其余原因对同一快照哈希**不重复产版本**（见 `diary` 模块）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiaryGenerationReason {
    Manual,
    Scheduled,
    Automatic,
    Regenerate,
}

impl DiaryGenerationReason {
    pub fn wire(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::Scheduled => "scheduled",
            Self::Automatic => "automatic",
            Self::Regenerate => "regenerate",
        }
    }

    pub fn from_wire(value: &str) -> Option<Self> {
        match value {
            "manual" => Some(Self::Manual),
            "scheduled" => Some(Self::Scheduled),
            "automatic" => Some(Self::Automatic),
            "regenerate" => Some(Self::Regenerate),
            _ => None,
        }
    }
}

/// 日记版本的来源，契约 2.5 节。
///
/// **AI 生成的正文永远是 `Generated`**，绝不写成 `UserEdit`——`source_revisions`
/// 的 `AuthorType` 只有 user/import，AI 文本也不写进用户原件（见 `diary` 模块）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiaryVersionOrigin {
    Generated,
    UserEdit,
    Merge,
}

impl DiaryVersionOrigin {
    pub fn wire(self) -> &'static str {
        match self {
            Self::Generated => "generated",
            Self::UserEdit => "user_edit",
            Self::Merge => "merge",
        }
    }

    pub fn from_wire(value: &str) -> Option<Self> {
        match value {
            "generated" => Some(Self::Generated),
            "user_edit" => Some(Self::UserEdit),
            "merge" => Some(Self::Merge),
            _ => None,
        }
    }

    /// 是不是用户自己写的（手写或用户确认后的合并）。
    ///
    /// 「首次生成可成为 current」这条例外的边界靠它：只要这一天有用户自己的版本，
    /// 自动生成的版本就**不会**被设成当前版本。
    pub fn is_user_authored(self) -> bool {
        matches!(self, Self::UserEdit | Self::Merge)
    }
}

/// 当天的材料统计，契约 2.5 节。数字都是真值，不是估计。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct MaterialCounts {
    /// 当天的记录数（不含回收站）。
    pub captures: i64,
    /// 已经有可用正文的材料数（用户的文字天然算「已处理」）。
    pub processed: i64,
    /// 还在等提取的材料数。
    pub pending: i64,
    /// 提取失败、暂无可读正文的材料数。
    pub failed: i64,
}

/// 某一天的日记聚合视图，契约 2.5 节 `DiaryDay`。
///
/// **候选与当前只在这里区分**：契约的 `DiaryVersion` 没有状态字段，所以「哪一版是当前、
/// 哪些是候选」由 `current_version_id` 与 `candidate_version_ids` 表达，不给版本另造状态。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DiaryDay {
    pub day_key: String,
    /// 这一天的时区名。没有任何材料也没有生成过时是 `UTC`（缺省，不是推测）。
    pub time_zone: String,
    /// 当前版本；这一天还没有任何版本时为 None。
    pub current_version_id: Option<String>,
    /// 候选版本（这一天的全部版本里除当前版本之外的那些），最新在前。
    pub candidate_version_ids: Vec<String>,
    pub material_counts: MaterialCounts,
    pub coverage: Coverage,
    pub updated_at: DateTime<Utc>,
}

/// 生成过程的审计信息，契约 2.5 节。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GenerationInfo {
    pub prompt_version: String,
    pub provider_id: Option<String>,
    pub model: Option<String>,
    /// 生成选项：固定的协议参数与**如实记录的步数**
    /// （`steps` / `intermediateCalls` / `repairCalls`），不虚构。
    pub generation_options: std::collections::HashMap<String, String>,
    /// 快照里已处理的材料数。
    pub covered_source_count: i64,
    /// 快照里明确记为「暂未处理」的材料数（不假装完整）。
    pub unprocessed_source_count: i64,
}

/// 正文与来源的关联，契约 2.5 节。涉及事实的内容要能追溯；不要求每句修辞都有引用。
///
/// 长材料两步走时，第一步提炼出来的**中间事实**也记在这里（`note` 是那条派生事实，
/// 来源是它真正来自的记录/片段），所以中间结果不脱离来源独立存在。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProvenanceLink {
    pub source_id: String,
    pub source_revision_id: String,
    pub locator: SourceLocator,
    /// 正文中对应的字符区间；整段关联或位置不可靠时为空（不伪造）。
    pub body_range: Option<TextRange>,
    pub note: Option<String>,
}

/// 日记版本，契约 2.5 节。正文是自然段，来源关联放在独立字段。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DiaryVersion {
    pub id: String,
    pub day_key: String,
    pub parent_version_id: Option<String>,
    pub origin: DiaryVersionOrigin,
    pub body: String,
    pub created_at: DateTime<Utc>,
    /// 不可变的输入清单快照（用了哪些材料、哪些修订、覆盖范围的真相）。
    pub source_snapshot_id: Option<String>,
    pub style_snapshot_id: Option<String>,
    pub generation_info: Option<GenerationInfo>,
    pub provenance_links: Vec<ProvenanceLink>,
}

/// 分页的版本列表，契约 4.3 节 `diary.listVersions`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DiaryVersionPage {
    pub versions: Vec<DiaryVersion>,
    pub next_cursor: Option<String>,
}

/// 一次日记生成的票据，契约 4.3 节 `diary.generate`。
///
/// 输入在**返回之前**就已经冻结（快照已落库、哈希已算），真正的调用由队列驱动执行。
/// Provider 还没配好时**照样返回票据**：那条任务会停在 `waiting_configuration` 等配置，
/// 而不是把「还没有配置服务」说成一次调用失败（与 #66 的队列闸门同一口径）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiaryGenerationTicket {
    pub job_id: String,
    pub source_snapshot_id: String,
}

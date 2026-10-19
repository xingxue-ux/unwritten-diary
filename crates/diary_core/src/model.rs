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

/// 任务分页结果。cursor 对调用方不透明。
///
/// 与 [`CapturePage`] 同一个形状（列表 + 不透明游标），给任务中心逐条翻页用：
/// 只有 `jobs.status` 的计数、没有逐条列表时，`attentionKey` 与 `progress` 都没处读。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobPage {
    pub jobs: Vec<Job>,
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
    /// 采集器**实际收到**的 PCM 总时长（只增，不含还没收到的）。
    pub elapsed_ms: i64,
    /// **已成功登记**片段时长之和；两次登记之间还没封闭的尾巴不能算进来。
    ///
    /// 恢复时用 `elapsed_ms - persisted_through_ms` 判断「可能丢失的尾巴」。
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
    /// **已登记**片段时长之和：真正落盘、`finalize` 能拼出来的音频长度。
    ///
    /// 只有「已封闭并登记成功」的片段算数；两次登记之间收到的 PCM 还在内存 /
    /// 未封闭文件里，进程崩溃就没了，绝不能算进来。
    pub persisted_ms: i64,
    /// 采集器**最后一次上报**的「已收到 PCM 总时长」（会话的 `elapsed_ms`）。
    ///
    /// 这是上报时刻的值（平台层通常一秒上报一次、停止时再补一次），所以只是实际
    /// 收到量的**下界**；`received_ms - persisted_ms` 就是「可能丢失的尾巴」。
    pub received_ms: i64,
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
///
/// **分段让出的长任务**（现在只有语义索引）算进 `processed`、但不算
/// `succeeded`/`failed`/`retrying`——它还活着（回到 `queued`，进度在 `Job.progress`
/// 上），下一轮接着跑。所以 `processed` 可以大于后三者之和，差额正是它；摘要**不为它
/// 加字段**（契约的字段一个没多），「还没跑完」由 `due_backlog ≥ 1` 与
/// `next_wakeup` 指向「现在」一起说清楚。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobRunSummary {
    /// 本次运行开始的时刻（调用方给的 `now`）。「补做」以它为界。
    pub started_at: DateTime<Utc>,
    /// 唤醒来源，调用方给什么就如实记什么（例如 startup / foreground / wake / manual）。
    /// 核心只用它做观察与排查，不用它改变行为。
    pub trigger: String,
    /// 本次领取并执行的任务数，不超过调用方给的条数上限。**分段让出的长任务算在内**
    /// （它确实被领取并执行了一段）；它不在下面三个计数里（见结构体说明）。
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
    /// 本次运行结束时还在等前置条件的任务数（`index` / `semantic_index` 等
    /// `extract`）。
    ///
    /// 这些任务**没有**被领取、没有消耗重试额度，所以也不在 `processed` 里。
    /// **等配置**（`waiting_configuration`，例如语义索引等本地模型）不算在这里：
    /// 它等的是配置而不是前置任务，在 `job_status().waiting_configuration` 里计数。
    pub waiting_dependency: i64,
    /// 本次运行里被**闸门**判定「前置不可能再满足」而终态失败的任务数。
    ///
    /// 这些任务同样不在 `processed` 里（一行执行者代码都没跑）；它们是被闸门在
    /// 事务里落成「已放弃」的，`job_status().failed` / `recent_errors` 里看得到。
    /// 单独报出来，是为了让摘要不必把「这一轮什么都没跑」说成「一切正常」。
    pub dependency_failed: i64,
    /// 下一个到期时刻；没有待办时为 None。等前置、等配置的任务都不算「待办」：
    /// 前者要等的是别人跑完，后者要等的是配置变化，都不是「时间到」
    /// （见 `jobs::next_wakeup`）。
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
    /// 等配置：现在只有「语义索引在等本地模型配好」（`error_code = waiting_model`）。
    pub waiting_configuration: i64,
    /// 等前置：`index` / `semantic_index` 在等 `extract` 收尾。
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
/// 的字符串，`semantic_index_ready` 要求生效代次真的覆盖了范围内的每一块，**并且**
/// 整代口径一致——`storage` / `dims` **总是**要与当前口径相同；`model_version` 只在
/// **当前版本可知**时才比（嵌入器已加载，或只配了模型时读模型文件哈希）。
/// `NotConfigured` / `Failed` 时不知道当前版本，这一项**跳过**：既不算不一致、也不
/// 假装一致——状态只报库里的事实与「为什么比不了」，不猜一个版本出来。
/// 尤其 `storage`：存量 f32 库在默认改成 int8 之后，只看 `model_version` 会误报
/// 「已就绪」，而检索路径整代判 `ForeignSpace`；因此状态检查直接读库里的 `storage`
/// 列（不加载模型），不一致时 `semantic_index_ready = false` 并在 `reasons` 里
/// 写明「存储格式从 f32 变为 int8，需要重建」。
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
    ///
    /// **不是**「这一条属于哪篇记录」：片段命中时它是 `source_id`，记录文字命中时
    /// 才是 `capture_id`。要跳到记录详情页请用 [`SearchHit::capture_id`]。
    pub group_id: String,
    /// 这一条命中属于哪篇记录（`captures.id`）。
    ///
    /// **与 `group_id` 不是一回事**：`group_id` 只是折叠/归组用的键，片段命中时
    /// 是 `source_id`；`capture_id` 才是「这一条属于哪篇记录」，两种命中都一定有。
    pub capture_id: String,
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

    /// 是不是**自动整理**这一路（B4e）。
    ///
    /// 这一路与用户主动要一版（`manual` / `regenerate`）有三处不同：
    /// 1. 用设置里选中的**整理模板**（写进冻结快照）拼提示词；
    /// 2. 结果**一律进候选**，绝不成为当前版本（用户确认走 `diary.setCurrent`）；
    /// 3. 受整理设置里的**配额上限**约束（单次/每日调用次数、材料条数与字数、失败率）。
    pub fn is_organize(self) -> bool {
        matches!(self, Self::Scheduled | Self::Automatic)
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
    /// 这一天的 **revision**：持久、单调递增的状态推进计数（`diary_days.revision`）。
    ///
    /// 每次成功写入 +1：新增一版（生成 / 编辑 / 合并）算一次，`diary.setCurrent`
    /// 真的换了当前版本也算一次；没有推进的调用（切到已经是当前的版本）不加。
    /// 调用方把读到的这个值原样回传给 `saveEdit` / `setCurrent` / `merge` 的
    /// `expectedRevision`（契约 2.5 / 4.3）；没有推进过的天是 0。
    pub revision: i64,
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

// ------------------------------------------------------------ 整理与自动化（B4e，#92）

/// 整理模板的一个可选参数：**只允许整数与布尔**，不接受自由文本。
///
/// 为什么参数不能是文本：模板规则属于**产品指令**，只在 system 消息里。如果参数能带
/// 任意文字，调用方（或将来某个插件）就能把「忽略上面的指令」写进指令槽——那正是
/// #64 花力气堵住的那条路（材料是数据，不是指令）。限定成整数/布尔之后，参数**没有
/// 地方**可以承载指令文本。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrganizeTemplateParam {
    /// 参数键（wire 名，例如 `targetChars`）。
    pub key: String,
    /// 中文短名，设置页直接用。
    pub label: String,
    /// 参数类型：`integer` 或 `boolean`。
    pub kind: String,
    /// 整数参数的取值范围（布尔参数两者都为空）。
    pub minimum: Option<i64>,
    pub maximum: Option<i64>,
    /// 默认值（字符串形式，`true` / `false` 或十进制整数）。
    pub default_value: String,
    pub description: String,
}

/// 一个整理模板：核心内置规则，或将来由 #88 的声明式插件提供的规则。
///
/// **这一片只实现内置三类**（`source = "builtin"`）。`source` 字段就是留给 #88 的
/// 对接点：插件模板进来时，`source = "plugin"` 并且 `plugin_id` 有值——解析、校验、
/// 提示词拼装的路径都不变（见 `docs/architecture/m4-整理与自动化.md`「#88 的对接点」）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrganizeTemplate {
    /// 模板 id（不透明字符串，进冻结快照）。
    pub id: String,
    pub label: String,
    pub description: String,
    /// 规则文本：**产品指令**，只在 system 消息里出现，由核心常量拼出。
    pub rule: String,
    pub params: Vec<OrganizeTemplateParam>,
    /// `builtin`（核心内置）或 `plugin`（#88 的声明式插件，尚未实现）。
    pub source: String,
    /// 插件模板所属的插件 id；内置模板为空。
    pub plugin_id: Option<String>,
}

/// 整理设置。默认**全关**：`enabled = false`，核心不会自动调用任何服务。
///
/// 这是「必须由用户显式开启」的落点：`Core::open` 建出来的资料库（含从旧版本升级上来
/// 的）都是关的，只有显式 `update_organize_settings` 才能打开。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OrganizeSettings {
    pub enabled: bool,
    pub template_id: String,
    /// 显式覆盖的参数（生效值 = 覆盖 + 模板默认值）。**只允许整数与布尔**。
    pub template_params: std::collections::HashMap<String, String>,
    /// 单次整理的材料条数上限。
    pub max_materials: i64,
    /// 单次整理的材料字数上限（Unicode 标量值）。
    pub max_chars: i64,
    /// 单次整理的模型调用次数上限（含两步走与格式修复）。
    pub max_calls_per_run: i64,
    /// 最近 24 小时内自动整理的模型调用次数上限。
    pub max_calls_per_day: i64,
    /// 失败率上限（0.0–1.0）：最近窗口内样本 ≥3 且失败比例超过它就停下。
    pub max_failure_rate: f64,
    /// 计划触发时间 `HH:mm`（本地时间，按当天材料的 UTC 偏移折算）。**不是完成承诺**。
    pub auto_organize_time: String,
    /// 当天已有当前版本、之后又来了新材料时，是否自动补做一份候选。关掉就只手动。
    pub supplement_on_new_materials: bool,
    /// 设置版本（乐观锁；每次真的改动 +1）。
    pub revision: i64,
    pub updated_at: DateTime<Utc>,
}

/// 自动整理的配额与用量现状。数字都来自**真实的调用账本**，不是估算。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OrganizeQuota {
    /// 最近 24 小时（滚动窗口）真的发出去的整理调用次数。
    pub calls_last_24h: i64,
    pub max_calls_per_day: i64,
    /// 还能发几次（不会小于 0）。
    pub calls_remaining: i64,
    /// 失败率窗口里的样本数（最近 24 小时的整理调用，最多取最近 20 条）。
    pub failure_samples: i64,
    /// 窗口里的失败比例（0.0–1.0）；没有样本时为 0。
    pub failure_rate: f64,
    pub max_failure_rate: f64,
    pub last_call_at: Option<DateTime<Utc>>,
}

/// 一个「该整理」的日子，以及它为什么该整理。
///
/// 判据全是可判定的（见 `docs/architecture/m4-整理与自动化.md`），没有一条是
/// 「AI 觉得该整理」。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OrganizePendingDay {
    pub day_key: String,
    /// 该整理的理由：`first`（这一天还没有任何版本）/ `supplement`（已有版本，之后又来了新材料）。
    pub reason: String,
    /// 可读正文的材料条数。
    pub materials: i64,
    /// 这些材料的字符总数（Unicode 标量值）。
    pub chars: i64,
    /// 按现有材料估算的模型调用次数（1 = 一步；N+1 = 两步走）。
    pub estimated_calls: i64,
    /// 这一天的计划触发时刻（`autoOrganizeTime` 折成 UTC）。
    pub due_at: DateTime<Utc>,
    /// 这一天的当前版本（没有就是空）。
    pub current_version_id: Option<String>,
}

/// 被跳过的日子与原因（超限、材料有问题……），如实报出来。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrganizeSkippedDay {
    pub day_key: String,
    /// 契约第 7 节的错误码（现在只会用到 `quota_exceeded`）。
    pub code: String,
    /// 可执行的说明：哪一条上限、已经做了什么、下一步怎么办。
    pub message: String,
}

/// 最近一次自动整理的真实结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrganizeRunInfo {
    pub day_key: String,
    /// 生成行的状态：`pending`（还没有结论：在队列里、在等配置、或在退避重试）/
    /// `ready`（产出了版本）/ `failed`（终态失败，**没有**版本）。
    pub state: String,
    pub error_code: Option<String>,
    pub version_id: Option<String>,
    /// 这次真的发出去了几次调用（来自账本，不是估算）。
    pub calls: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// 自动整理的状态：开关、模板、**什么时候会跑、会跑哪些材料**、配额与最近结果。
///
/// 这是「开启后要能说清会跑什么、什么时候跑」的落点。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OrganizeStatus {
    pub settings: OrganizeSettings,
    /// 选中的模板（含规则与参数声明）。
    pub template: OrganizeTemplate,
    /// 生效参数（覆盖 + 默认值），键有序。
    pub effective_params: std::collections::HashMap<String, String>,
    /// 现在就该跑的日子。
    pub due_days: Vec<OrganizePendingDay>,
    /// 已经该整理、但还没到计划触发时刻的日子（`nextRunAt` 就是其中最早的）。
    pub scheduled_days: Vec<OrganizePendingDay>,
    /// 下一次计划触发时刻；没有待办时为空。
    pub next_run_at: Option<DateTime<Utc>>,
    pub quota: OrganizeQuota,
    /// Provider 能力闸门（没配好时给出原因与可执行的下一步）。**只读，不发请求**。
    pub provider_code: Option<String>,
    pub provider_reason: Option<String>,
    pub provider_next_step: Option<String>,
    /// 全局停下时的错误码与说明（例如 `quota_exceeded`：失败率过高）。
    pub blocked_code: Option<String>,
    pub blocked_reason: Option<String>,
    /// 被跳过的日子（单日超限等）。
    pub skipped_days: Vec<OrganizeSkippedDay>,
    pub last_run: Option<OrganizeRunInfo>,
    /// 如实补充说明（观察窗、默认关、哪些不算待处理……）。
    pub notes: Vec<String>,
}

/// 一轮自动整理的排期结果（驱动每轮开始时调用一次）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrganizePlanSummary {
    /// 观察窗内被盘点过的日子数。
    pub considered_days: i64,
    /// 这一轮排出去的日子（已入队 `diary_generate`）。
    pub queued_days: Vec<String>,
    /// 现在就该跑、但因为全局上限被挡住的日子。
    pub blocked_days: Vec<String>,
    /// 被跳过的日子与原因。
    pub skipped_days: Vec<OrganizeSkippedDay>,
    /// 全局停下的错误码与说明；没有全局停下时为空。
    pub blocked_code: Option<String>,
    pub blocked_reason: Option<String>,
    pub notes: Vec<String>,
}


// ---------------------------------------------------------------- 备份与恢复（B7）

/// 备份快照里的关键行数。
///
/// 这三件事都靠它，所以它既是**清单字段**、也是**报告字段**：
/// - 创建备份时按快照现算，写进清单；
/// - 恢复时拿恢复出来的库**逐项对**，对不上就是「清单与内容不一致」；
/// - 报告里如实给用户/上层看「这一份备份里有多少条记录、多少个原件」。
///
/// `Option` 不是「可能读不到」，而是**老 schema 的包**：`blobs` / `assets` /
/// `import_sessions` 从 v2 起、`recording_sessions` 从 v3 起才存在，拿一个 v1 时代的
/// 备份来恢复时这些表本来就不该有。核心自己产出的清单永远是全的。
///
/// 清单里的字段名是 camelCase（与清单其它字段、与契约的口径一致）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupCounts {
    /// 记录数（含草稿与回收站：备份是整库，不按界面口径过滤）。
    pub captures: i64,
    /// 原始文字/原件修订数。
    pub source_revisions: i64,
    /// 资产数（`assets` 行数，同一份内容被导入两次算两条）。
    pub assets: Option<i64>,
    /// 内容寻址的原件数（`blobs` 行数，同一份内容只算一条、只存一份）。
    pub blobs: Option<i64>,
    /// 快照里还没走完的导入数（`prepared` / `copying` / `verifying` / `recoverable`）。
    pub imports_in_flight: Option<i64>,
    /// 快照里还没收尾的录音数。
    pub open_recordings: Option<i64>,
}

/// 创建备份的结果（`backups.create`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BackupReport {
    /// 备份包（归档文件）的最终路径。写完并**自校验通过**之后才有这个文件。
    pub package_path: String,
    /// 备份包的字节数。
    pub byte_size: i64,
    /// 快照的 schema 版本（`PRAGMA user_version`）。
    pub schema_version: i64,
    /// 产出这份备份的核心 API 版本，口径与 `CoreInfo.apiVersion` 一致。
    pub api_version: String,
    /// 产出这份备份的核心构建版本。
    pub core_version: String,
    /// 快照的导出时刻。
    pub created_at: DateTime<Utc>,
    /// 随包的内容寻址原件数（`blobs`）。
    pub blob_count: i64,
    /// 这些原件的原始字节数合计（压缩前的口径）。
    pub blob_bytes: i64,
    /// 随包的**未收尾录音片段**数（不是内容寻址的那一类，见架构文档）。
    pub transient_clip_count: i64,
    /// 快照里的关键行数。
    pub counts: BackupCounts,
    /// 需要调用方知道的事实（未完成的导入/录音、暂存片段没进包等）。
    pub warnings: Vec<String>,
}

/// 备份包检查结果（`backups.inspect`）。
///
/// **不抛错**：包坏了、路径越界、版本不兼容都如实进 `issues`，让界面能先展示
/// 「这个包有什么问题」再决定要不要恢复。只有连清单都读不出来（不是 zip、没有
/// 清单）才报 `corrupted_backup`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BackupInspection {
    pub package_path: String,
    /// 包格式版本（`formatVersion`）。
    pub format_version: i64,
    pub schema_version: i64,
    pub api_version: String,
    pub core_version: String,
    pub created_at: DateTime<Utc>,
    pub byte_size: i64,
    pub blob_count: i64,
    pub blob_bytes: i64,
    pub asset_count: i64,
    pub transient_clip_count: i64,
    pub counts: BackupCounts,
    /// 逐条目重算哈希、比大小、比清单之后**仍然为空**才算没有问题的包。
    ///
    /// 每条 `issues` 的格式是 `种类: 细节`，种类只有三种：
    /// `corrupted`（哈希/结构/清单不一致）、`unsafe_path`（越界路径）、
    /// `incompatible`（版本高于本核心）。机器可读的是种类，给人看的是细节。
    pub issues: Vec<String>,
    /// 本构建能不能恢复这个包（`schemaVersion` / `apiVersion` 都在可处理范围内）。
    pub compatible: bool,
    /// 是否逐条目校验过哈希。检查被中断时为 false，且 `issues` 非空。
    pub hashes_verified: bool,
}

/// 恢复到新库的结果（`backups.restore`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RestoreReport {
    /// 恢复出来的资料库文件路径：调用方拿它直接 `Core::open`。
    pub library_path: String,
    /// 恢复出来的资料库根目录（原件与资料库文件所在目录）。
    pub root_dir: String,
    pub schema_version: i64,
    pub api_version: String,
    pub core_version: String,
    /// 备份包的导出时刻。
    pub created_at: DateTime<Utc>,
    pub byte_size: i64,
    pub blob_count: i64,
    pub blob_bytes: i64,
    pub transient_clip_count: i64,
    pub counts: BackupCounts,
    /// 如实说明恢复出来的库里「还没做完」的东西（不假装完成）。
    pub warnings: Vec<String>,
}

// ------------------------------------------------------------ 插件（#88，B6）
//
// 这些类型是**桥接与缓存清单的公开形状**。插件清单字段与任务书 §8.2 一一对应；
// 缓存进 `plugin_installations` 的 `manifest_json` 后，应用启动只读这一份 JSON，
// 不读、不编译、不执行插件的入口文件（见 `docs/architecture/m4-插件SDK与宿主.md`）。

/// 插件入口形态：声明式（纯数据，宿主解释）或 Wasm（受限沙箱）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginEntryKind {
    /// `main.json`：只有模板，宿主按固定占位符解释，**没有可执行代码**。
    Declarative,
    /// `main.wasm`：在 `wasmi` 解释器里跑，权限与预算由宿主强制。
    Wasm,
}

impl PluginEntryKind {
    pub fn wire(self) -> &'static str {
        match self {
            Self::Declarative => "declarative",
            Self::Wasm => "wasm",
        }
    }

    pub fn from_wire(value: &str) -> Option<Self> {
        match value {
            "declarative" => Some(Self::Declarative),
            "wasm" => Some(Self::Wasm),
            _ => None,
        }
    }
}

/// 插件清单。字段名与任务书 §8.2 的 camelCase 一致（缓存 JSON 也用它）。
///
/// `package_hashes` 覆盖包内**除 `manifest.json` 之外**的每一个文件；清单自身无法
/// 自哈希，由安装时的结构校验与包哈希（`PluginInspection.package_hash`）兜住。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginManifest {
    pub manifest_version: i64,
    pub id: String,
    pub name: String,
    pub version: String,
    pub author: String,
    pub license: String,
    pub api_range: String,
    pub supported_platforms: Vec<String>,
    pub entry_kind: PluginEntryKind,
    pub entry_file: String,
    pub activation_events: Vec<String>,
    pub permissions: Vec<String>,
    /// 设置声明的 JSON 文本（对象）。这一片只做保存与展示，不据此自动授权。
    pub settings_schema_json: Option<String>,
    pub package_hashes: std::collections::HashMap<String, String>,
    pub description: Option<String>,
    pub homepage: Option<String>,
}

/// 一项权限的展示信息。权限的**判定**在核心，界面只负责让用户看清再授权。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginPermission {
    /// wire 名，例如 `read_selected_material` 或 `invoke_provider:<providerId>`。
    pub id: String,
    pub label: String,
    pub description: String,
    /// `low` / `medium` / `high`；只用于界面提示，不参与判定。
    pub risk: String,
}

/// 一次包检查的结论。**不执行包内容**：只读文件、校验清单/哈希/版本/平台/权限。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginInspection {
    pub package_path: String,
    /// 包的来源形态：`directory` 或 `zip`。
    pub package_kind: String,
    /// 对清单与全部文件算出的稳定包哈希（换一个字节就变）。
    pub package_hash: String,
    pub manifest: PluginManifest,
    /// 声明的模板（声明式插件）；Wasm 插件为空。
    pub templates: Vec<PluginTemplate>,
    /// 清单声明的权限，带中文说明。
    pub permissions: Vec<PluginPermission>,
    /// 调用方这次没有授予的权限（安装会因此被拒，并说清缺哪几项）。
    pub missing_permissions: Vec<String>,
    /// 本构建能不能安装：版本范围、平台、清单版本、结构都算。
    pub compatible: bool,
    /// 不能安装/需要用户注意的原因。机器可读前缀：`incompatible:` / `invalid:`。
    pub issues: Vec<String>,
    /// 已安装的版本（没有就是 None）。
    pub installed_version: Option<String>,
    /// 这次安装会不会是一次升级。
    pub is_upgrade: bool,
}

/// 一个已安装插件。**全部来自缓存清单与授权表**，启动时不需要碰插件文件。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginInstallation {
    pub plugin_id: String,
    pub name: String,
    pub version: String,
    pub manifest_version: i64,
    pub entry_kind: PluginEntryKind,
    pub api_range: String,
    pub enabled: bool,
    /// 用户**已经授予**的权限（不是清单声明的全集；两者取交集后存这里）。
    pub permissions: Vec<String>,
    /// 清单声明的权限（授权前展示用；与 `permissions` 对照能看出撤权/升级新增）。
    pub requested_permissions: Vec<String>,
    /// 清单声明的支持平台（缓存，不读入口文件）。
    pub supported_platforms: Vec<String>,
    pub activation_events: Vec<String>,
    /// 缓存下来的模板（声明式插件）；启动时直接从缓存读，不读入口文件。
    pub templates: Vec<PluginTemplate>,
    pub package_hash: String,
    pub installed_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub last_run_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
}

/// 声明式插件里的一个模板。纯数据，宿主只做白名单占位符替换。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginTemplate {
    pub plugin_id: String,
    pub id: String,
    /// `style`（风格模板）或 `export`（导出模板）。
    pub kind: String,
    pub label: String,
    pub description: String,
    /// `style_instruction` / `markdown` / `plain_text`。
    pub format: String,
    /// 模板正文（有大小上限，进缓存清单供预览）。
    pub body: String,
}

/// 风格输入：字段与契约 `StyleProfile` 对齐（#114 的现有形状），但**不落库**。
///
/// 这一片把渲染好的文本交回调用方；`styles.*` 的存储仍是 #114 的未完成部分，
/// 不在这里另造一套风格表。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginStyleProfile {
    pub id: String,
    pub label: String,
    pub revision: i64,
    pub updated_at: DateTime<Utc>,
    pub length_preference: Option<String>,
    pub person: Option<String>,
    pub tone: Option<String>,
    pub emotional_temperature: Option<String>,
    pub rhetoric_level: Option<String>,
    pub examples: Vec<String>,
    pub banned_expressions: Vec<String>,
}

/// `plugins.invoke` 的入参。
///
/// 激活点必须是清单声明的字符串之一：`command:<id>`、`materials_pending:<kind>`、
/// `export.start`、`organize.template.select`。`export.start` 的格式与日期范围
/// 沿用契约 `ExportRequest` 的 `format` 与 dayKey 语义（#113 的现有形状）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginInvocation {
    pub plugin_id: String,
    pub activation: String,
    /// 声明式模板 id；Wasm 插件与命令激活可以为空。
    pub template_id: Option<String>,
    /// 用户选定的日记版本（读当前选中材料权限）。
    pub selected_version_ids: Vec<String>,
    /// 用户选定的材料/记录（读当前选中材料权限）。
    pub selected_capture_ids: Vec<String>,
    /// 明确日期范围（读日期范围权限）；闭区间，最多 366 天。
    pub from_day_key: Option<String>,
    pub to_day_key: Option<String>,
    /// 风格模板的输入（#114 的 `StyleProfile` 形状）。
    pub style_profile: Option<PluginStyleProfile>,
    /// 导出格式：`markdown` / `plain_text`；声明式模板必须与模板的 format 一致。
    pub format: Option<String>,
    /// 命令的附加文字；有 1 MiB 上限，作为**数据**交给插件，不是指令。
    pub input_text: Option<String>,
}

/// 一次插件运行产出的文件。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginArtifact {
    pub file_name: String,
    pub media_type: String,
    pub byte_size: i64,
    pub sha256: String,
    /// 文件库里的绝对路径；内存库没有落盘时为 None。
    pub path: Option<String>,
    /// 文本内容的副本（最多 1 MiB），方便界面直接展示/保存；二进制为 None。
    pub content: Option<String>,
}

/// 插件返回的候选派生内容。**不会自动写进日记**；核心校验后随运行结果交给调用方，
/// 并把摘要记在 `plugin_installations.last_run_json` 供审计。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginDerivedContent {
    pub kind: String,
    pub text: String,
    pub source_refs: Vec<PluginSourceRef>,
}

/// 派生内容的来源定位（必须是本次授权范围内的材料/日记）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginSourceRef {
    pub source_id: String,
    pub source_revision_id: Option<String>,
    /// 可读定位的 JSON 文本（例如 `{"type":"text_range",...}`）；没有就是 None。
    pub locator_json: Option<String>,
}

/// 插件调用 Provider 失败时的审计项。核心在运行结束后据此把服务标记为「测试失败」
/// （只对鉴权/模型不可用这类永久错误，与既有 `mark_provider_verdict_failed` 同口径）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginProviderFailure {
    pub provider_id: String,
    pub error_code: String,
}

/// 一次插件运行的结果。`status` 只有 `succeeded` / `failed` / `denied` / `timeout` /
/// `budget_exceeded` 五种；`error_code` 用契约第 7 节的既有码，不新增 wire 码。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginRunResult {
    pub run_id: String,
    pub plugin_id: String,
    pub activation: String,
    pub status: String,
    pub output: Option<String>,
    pub artifacts: Vec<PluginArtifact>,
    pub derived_contents: Vec<PluginDerivedContent>,
    pub logs: Vec<String>,
    pub warnings: Vec<String>,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    pub duration_ms: i64,
    /// 这一轮实际消耗的 Wasm 指令预算（fuel）；声明式插件为 None。
    pub instructions_used: Option<i64>,
}

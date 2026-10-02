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
#[derive(Debug, Clone)]
pub struct NewJob {
    pub kind: String,
    pub priority: JobPriority,
    pub target_ids: Vec<String>,
    pub input_snapshot_hash: Option<String>,
    pub max_attempts: i64,
}

impl NewJob {
    pub fn new(kind: impl Into<String>, priority: JobPriority) -> Self {
        Self {
            kind: kind.into(),
            priority,
            target_ids: Vec::new(),
            input_snapshot_hash: None,
            max_attempts: 5,
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
/// 核心只建关键词索引，所以 `semantic_index_ready` 恒为 false、
/// `model_version` 恒为空——照实说，而不是留一个看起来「都就绪」的默认值。
/// `pending_segments` 与 `failed_sources` 的单位不同（片段 / 材料），
/// 名字里写清楚，避免前端把两个数字相加。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IndexStatus {
    pub coverage: Coverage,
    pub keyword_index_ready: bool,
    pub semantic_index_ready: bool,
    /// 建索引时用的分词器版本；与库里行不一致的片段会被算成待重建。
    pub tokenizer_version: String,
    pub model_version: Option<String>,
    pub chunker_version: Option<String>,
    /// 已进关键词索引的片段数。
    pub indexed_segments: i64,
    /// 派生内容里的片段总数。
    pub total_segments: i64,
    /// 还没进索引的片段数（未索引 + 分词器版本过期）。
    pub pending_segments: i64,
    /// 分词器版本过期、需要重建的片段数（已包含在 pending 里）。
    pub stale_segments: i64,
    /// 正文解析失败的材料数（这些材料没有片段可索引）。
    pub failed_sources: i64,
    /// 已索引片段的正文字符总数，用来算索引体积的每字符代价。
    pub indexed_chars: i64,
    /// 索引倒排表的行数。
    pub index_rows: i64,
    /// 索引里不同词项的个数。
    pub index_terms: i64,
    /// 索引表实际占用的字节数（来自 dbstat）。读不到时是 0，并在 reasons 里说明。
    pub index_bytes: i64,
    /// 用户可读的原因说明。
    pub reasons: Vec<String>,
}

//! 桥接层：把 `diary_core` 的能力暴露给 Dart。
//!
//! 这一层**不放业务规则**，只做三件事：
//! 1. 把 Dart 传来的参数转成核心调用；
//! 2. 把核心结果原样交出去（类型由 codegen 镜像，不另造一套 DTO）；
//! 3. 把核心错误转成契约第 7 节的错误码。
//!
//! 核心的公开模型（`diary_core::model`）已经写进 `flutter_rust_bridge.yaml` 的
//! `rust_input`，所以这里返回的是真正的字段级镜像，而不是不透明句柄。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, Weak};

use chrono::{DateTime, Utc};
use diary_core::{
    AiResponse, BackupInspection, BackupReport, Capture, CapturePage, CommitResult, Core,
    CoreError, DiaryDay, DiaryGenerationReason, DiaryGenerationTicket, DiaryVersion,
    DiaryVersionPage, DomainEvent, DraftSaveResult, ExtractedContent, ImportManifest, ImportOrigin,
    ImportStatus, ImportTicket, IndexStatus, Job, JobRunSummary, JobState, JobStatus,
    NativeRecordingStatus, OrganizeSettings, OrganizeStatus, OrganizeTemplate, PluginInspection,
    PluginInstallation, PluginInvocation, PluginRunResult, PluginTemplate, Provider,
    ProviderCapability, ProviderTestReport, RecordingFinalizeResult, RecordingRecovery,
    RecordingSession, RecordingTicket, ResponseTicket, RestoreReport, SearchRequest,
    SearchSnapshot, SegmentManifest, SegmentReceipt, SourceLocation, SourceLocator, SourceRevision,
    StyleProfile,
};
use flutter_rust_bridge::frb;

/// 桥接错误：`code` 是契约第 7 节的错误码，`message` 给用户看。
///
/// frb 会把它变成 Dart 侧的异常类型，所以前端可以用 `on BridgeError catch (e)`
/// 直接读 `e.code`。
#[derive(Debug)]
pub struct BridgeError {
    pub code: String,
    pub message: String,
    pub retryable: bool,
}

impl From<CoreError> for BridgeError {
    fn from(error: CoreError) -> Self {
        Self {
            code: error.code().wire().to_owned(),
            message: error.to_string(),
            retryable: error.retryable(),
        }
    }
}

/// 打开资料库后的恢复摘要。数字都是真实统计，不是估计。
pub struct RecoverySummary {
    /// 上次没走完的导入数量。
    pub recoverable_imports: u32,
    /// 上次没走完的录音数量。
    pub open_recordings: u32,
    /// 还没做完的任务数量。
    pub pending_jobs: u32,
    pub notes: Vec<String>,
}

/// 核心信息与恢复摘要。
pub struct LibraryInfo {
    pub api_version: String,
    pub data_schema_version: i64,
    pub build_version: String,
    pub library_id: String,
    /// 已经接到桥上的契约方法名。**没列出来的就是还没接**，界面据此决定哪些入口可见。
    pub capabilities: Vec<String>,
    pub recovery: RecoverySummary,
    /// 记录数量（不含回收站），契约 `CoreSnapshot.captureCount`。
    pub capture_count: i64,
    /// 事件表里最大的序号，契约 `CoreSnapshot.lastEventSequence`；没有事件时为 0。
    pub last_event_sequence: i64,
}

/// 一个打开的资料库会话。Dart 侧持有的就是这个句柄。
#[frb(opaque)]
pub struct BridgeSession {
    inner: Arc<Mutex<Core>>,
    library_id: String,
}

// WorkManager starts a second Dart isolate in this Android process. All bridge
// handles for one library must share the same Core lock: opening a second Core
// would run recover_on_open while a foreground import is still copying, and
// two runDue calls could both claim the same job. Weak entries let the last
// session really close the database. This is an in-process guarantee; a
// separate Android process must not open the library concurrently.
static OPEN_LIBRARIES: OnceLock<Mutex<HashMap<PathBuf, Weak<Mutex<Core>>>>> = OnceLock::new();

fn library_key(path: &Path) -> Result<PathBuf, BridgeError> {
    let file_name = path.file_name().ok_or_else(|| BridgeError {
        code: "invalid_argument".to_owned(),
        message: "资料库路径必须指向一个文件".to_owned(),
        retryable: false,
    })?;
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(CoreError::from)?
            .join(path)
    };
    // The file may not exist on its first open; canonicalize its parent so
    // relative and absolute callers still meet at the same registry entry.
    let parent = absolute.parent().unwrap_or_else(|| Path::new("."));
    let canonical_parent = std::fs::canonicalize(parent).unwrap_or_else(|_| parent.to_path_buf());
    Ok(canonical_parent.join(file_name))
}

/// 一轮驱动最多跑几条任务（防御性上限）。
///
/// 契约 `jobs.runDue` 只给毫秒预算、不给条数，所以这个上限由桥接层拍：预算挡的是
/// 长尾，它挡的是「队列很长 + 多条短任务」把一轮拖成没完没了。没跑上的任务留在
/// 队列里、如实计入摘要的 `dueBacklog`，下一轮接着跑——不会丢。
const RUN_DUE_MAX_JOBS: usize = 64;

/// `events_since` 每次从核心读多少条：只影响**单次 SQL** 的中间列表，不改变返回形状。
///
/// 取 1000 与 C ABI 的 `MAX_EVENTS_LIMIT` 同量级：一次查询的峰值内存由它兜住。
const EVENTS_PAGE: usize = 1000;

impl BridgeSession {
    /// 打开（或创建）资料库。
    pub fn open(library_path: String) -> Result<BridgeSession, BridgeError> {
        let path = std::path::PathBuf::from(&library_path);
        let key = library_key(&path)?;
        let mut registry = OPEN_LIBRARIES
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .map_err(|_| BridgeError {
                code: "invalid_state".to_owned(),
                message: "资料库会话登记表已损坏".to_owned(),
                retryable: false,
            })?;
        let inner = match registry.get(&key).and_then(Weak::upgrade) {
            Some(existing) => existing,
            None => {
                let opened = Arc::new(Mutex::new(Core::open(&key)?));
                registry.insert(key, Arc::downgrade(&opened));
                opened
            }
        };
        let library_id = path
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_else(|| "library".to_owned());
        Ok(Self {
            inner,
            library_id,
        })
    }

    /// 核心信息与恢复摘要。
    pub fn info(&self) -> Result<LibraryInfo, BridgeError> {
        let core = self.lock()?;
        let recovery = recovery_summary(&core)?;
        // 契约的 CoreSnapshot 需要另外两个数字，一起在这里给出去：
        // 不提供的话适配层只能填 0，而 0 对已有记录的资料库是假话。
        let capture_count = core.count_captures()?;
        let last_event_sequence = core.last_event_sequence()?;
        Ok(LibraryInfo {
            api_version: diary_core::API_VERSION.to_owned(),
            data_schema_version: core.schema_version()?,
            build_version: env!("CARGO_PKG_VERSION").to_owned(),
            library_id: self.library_id.clone(),
            capabilities: wired_capabilities(),
            recovery,
            capture_count,
            last_event_sequence,
        })
    }

    // ------------------------------------------------------------ 记录

    /// 创建草稿，契约第 4.1 节 `captures.createDraft`。
    pub fn create_draft(
        &self,
        occurred_at: Option<DateTime<Utc>>,
        time_zone: String,
        utc_offset_minutes: i32,
        operation_id: String,
    ) -> Result<Capture, BridgeError> {
        let mut core = self.lock()?;
        Ok(core.create_draft(diary_core::CreateDraftInput {
            occurred_at,
            time_zone: &time_zone,
            utc_offset_minutes,
            operation_id: &operation_id,
        })?)
    }

    /// 保存草稿。只有核心确认落盘后才返回 `durable = true`。
    pub fn save_draft(
        &self,
        capture_id: String,
        text: String,
        expected_revision: i64,
        operation_id: String,
    ) -> Result<DraftSaveResult, BridgeError> {
        let mut core = self.lock()?;
        Ok(core.save_draft(&capture_id, &text, expected_revision, &operation_id)?)
    }

    /// 提交记录，创建原始文字版本。
    pub fn commit(
        &self,
        capture_id: String,
        expected_revision: i64,
        operation_id: String,
    ) -> Result<CommitResult, BridgeError> {
        let mut core = self.lock()?;
        Ok(core.commit(&capture_id, expected_revision, &operation_id)?)
    }

    pub fn get_capture(&self, capture_id: String) -> Result<Capture, BridgeError> {
        let core = self.lock()?;
        Ok(core.get_capture(&capture_id)?)
    }

    /// 分页读取记录。cursor 对调用方不透明。
    pub fn list_captures(
        &self,
        day_key: Option<String>,
        cursor: Option<String>,
        limit: u32,
    ) -> Result<CapturePage, BridgeError> {
        let core = self.lock()?;
        Ok(core.list_captures(day_key.as_deref(), cursor.as_deref(), limit as usize)?)
    }

    // ------------------------------------------------------------ 原件与导入

    /// 申请导入暂存位置。平台层只能往票据指向的文件里写。
    pub fn prepare_import(
        &self,
        capture_id: String,
        display_name: String,
        mime_hint: Option<String>,
        size_hint: Option<i64>,
        origin: ImportOrigin,
        operation_id: String,
    ) -> Result<ImportTicket, BridgeError> {
        let mut core = self.lock()?;
        Ok(core.prepare_import(diary_core::ImportRequest {
            capture_id: &capture_id,
            display_name: &display_name,
            mime_hint: mime_hint.as_deref(),
            size_hint,
            origin,
            operation_id: &operation_id,
        })?)
    }

    /// 声明复制完成。核心会自己重算哈希并比对。
    pub fn finish_import(
        &self,
        import_id: String,
        staging_ticket: String,
        manifest: ImportManifest,
    ) -> Result<ImportStatus, BridgeError> {
        let mut core = self.lock()?;
        Ok(core.finish_import(&import_id, &staging_ticket, manifest)?)
    }

    pub fn import_status(&self, import_id: String) -> Result<ImportStatus, BridgeError> {
        let core = self.lock()?;
        Ok(core.import_status(&import_id)?)
    }

    // ------------------------------------------------------------ 备份与恢复

    /// 创建备份包（B7 / 验收 E34；契约 `backups.create` 的同步入口）。
    ///
    /// 核心先取 SQLite 的一致性快照（`VACUUM INTO`，**不拷 .sqlite 文件**），再复制被
    /// 快照引用的内容寻址原件，写完之后**自校验**一遍（重开包逐条目重算哈希）——过了才
    /// 把包放到 `target_path` 上。备份期间资料库可以继续写，不加全局写锁。
    ///
    /// `target_path` 是**包文件的完整路径**，父目录会被创建；已存在时报
    /// `invalid_argument`（绝不覆盖旧备份）。任何一步失败：目标路径上不留东西。
    ///
    /// 与契约的差异：契约 `backups.create` 返回 `jobId`（任务化，#70）。这一片先给
    /// **同步**入口并把返回换成真实报告——任务化不属于 #86；Dart 适配层因此**不**把
    /// `backups.create` 列进能力清单（能力是窄化过的，见 `wired_capabilities`）。
    pub fn create_backup(&self, target_path: String) -> Result<BackupReport, BridgeError> {
        let core = self.lock()?;
        Ok(core.create_backup(&target_path)?)
    }

    /// 检查备份包（契约 `backups.inspect` 的同步入口）：结构、版本、路径与逐条目哈希。
    ///
    /// 坏在哪就列在哪（`issues` 每条的格式是 `种类: 细节`，种类只有 `corrupted` /
    /// `unsafe_path` / `incompatible`），**不抛错**；只有连清单都读不出来才报
    /// `corrupted_backup`。界面据此先展示问题，再决定要不要恢复。
    pub fn inspect_backup(&self, package_path: String) -> Result<BackupInspection, BridgeError> {
        let core = self.lock()?;
        Ok(core.inspect_backup(&package_path)?)
    }

    /// 恢复到**新的空目录**（E34/E35；契约 `backups.restore` 的同步入口）。
    ///
    /// **绝不覆盖当前资料库**：目标必须是新的空目录；解包先落在目标同级的暂存目录，
    /// 全部校验通过才一次 rename 落地，中途失败只删暂存目录。拒绝口径：
    /// 损坏 ⇒ `corrupted_backup`；越界路径 ⇒ `corrupted_backup`（绝不按它解包）；
    /// 版本高于本核心 ⇒ `incompatible_backup`。返回的 `library_path` 直接交给
    /// `BridgeSession.open` 就能打开恢复出来的库。
    pub fn restore_backup(
        &self,
        package_path: String,
        target_dir: String,
    ) -> Result<RestoreReport, BridgeError> {
        let core = self.lock()?;
        Ok(core.restore_backup(&package_path, &target_dir)?)
    }

    pub fn cancel_import(&self, import_id: String) -> Result<(), BridgeError> {
        let mut core = self.lock()?;
        Ok(core.cancel_import(&import_id)?)
    }

    // ------------------------------------------------------------ 录音交接

    pub fn prepare_recording(
        &self,
        capture_id: String,
        operation_id: String,
    ) -> Result<RecordingTicket, BridgeError> {
        let mut core = self.lock()?;
        Ok(core.prepare_recording(&capture_id, &operation_id)?)
    }

    pub fn register_segment(
        &self,
        recording_id: String,
        segment_index: i64,
        segment: SegmentManifest,
    ) -> Result<SegmentReceipt, BridgeError> {
        let mut core = self.lock()?;
        Ok(core.register_segment(&recording_id, segment_index, segment)?)
    }

    pub fn update_recording_state(
        &self,
        status: NativeRecordingStatus,
    ) -> Result<RecordingSession, BridgeError> {
        let mut core = self.lock()?;
        Ok(core.update_recording_state(status)?)
    }

    pub fn finalize_recording(
        &self,
        recording_id: String,
        last_segment_index: i64,
        end_reason: String,
    ) -> Result<RecordingFinalizeResult, BridgeError> {
        let mut core = self.lock()?;
        Ok(core.finalize_recording(&recording_id, last_segment_index, &end_reason)?)
    }

    pub fn recover_recording(
        &self,
        recording_id: Option<String>,
    ) -> Result<RecordingRecovery, BridgeError> {
        let core = self.lock()?;
        Ok(core.recover_recording(recording_id.as_deref())?)
    }

    // ------------------------------------------------------------ 提取与定位

    /// 对某个来源（来源 id 或修订 id 都行）跑一次提取。
    pub fn extract_source(&self, source_ref: String) -> Result<ExtractedContent, BridgeError> {
        let mut core = self.lock()?;
        Ok(core.extract_source(&source_ref)?)
    }

    /// 读取某个来源当前版本的派生内容；没提取过时返回空。
    pub fn extracted_content(
        &self,
        source_id: String,
    ) -> Result<Option<ExtractedContent>, BridgeError> {
        let core = self.lock()?;
        Ok(core.extracted_content(&source_id)?)
    }

    /// 修改某一篇来源的原始文字：新建一个修订，旧修订保留。
    ///
    /// 契约第 4.1 节 `sources.reviseText`。**改完必须再跑一次 `extract_source`**，
    /// 新修订才会进索引——这一步不自动做（提取有自己的失败与重试语义，见
    /// `docs/architecture/m2-修订与检索可见性.md`）。
    pub fn revise_text(
        &self,
        source_id: String,
        text: String,
        expected_revision: i64,
        operation_id: String,
    ) -> Result<SourceRevision, BridgeError> {
        let mut core = self.lock()?;
        Ok(core.revise_text(&source_id, &text, expected_revision, &operation_id)?)
    }

    /// 取某个来源**当前修订的父修订**（上一版正文），供界面「看原正文」用。
    ///
    /// 没有上一版时返回空。返回的是修订，不是 diff：差在哪由前端算。
    /// 父修订可能是原件型修订（`text` 为空、`asset_id` 有值），前端要按这个分支
    /// 决定是显示文字还是提供「打开原件」。
    pub fn previous_source_revision(
        &self,
        source_id: String,
    ) -> Result<Option<SourceRevision>, BridgeError> {
        let core = self.lock()?;
        Ok(core.previous_source_revision(&source_id)?)
    }

    /// 把 sourceRef + locator 解析成可打开的原件与可用性。
    pub fn locate_source(
        &self,
        source_ref: String,
        locator: SourceLocator,
    ) -> Result<SourceLocation, BridgeError> {
        let core = self.lock()?;
        Ok(core.locate_source(&source_ref, locator)?)
    }

    // ------------------------------------------------------------ 关键词索引

    /// 索引覆盖状态，契约第 4.4 节 `indexes.status`。
    ///
    /// `source_scope` 为空表示整个资料库；空数组是「什么都不看」而不是「看全部」。
    pub fn index_status(
        &self,
        source_scope: Option<Vec<String>>,
    ) -> Result<IndexStatus, BridgeError> {
        let core = self.lock()?;
        Ok(core.index_status(source_scope.as_deref())?)
    }

    /// 排一条关键词索引重建任务，契约第 4.4 节 `indexes.rebuild`。
    ///
    /// **异步**：只排任务并返回它（`targetIds` 为空表示整库），不做同步重建——同步
    /// 重建会阻塞界面，而队列的退避、补做与依赖闸门已经就绪。`scope` 的编码见契约
    /// 第 4.4 节：`all` 整库，`sources:<id>[,<id>]*` 指定来源（顺序无语义，空列表 /
    /// 非法字符报 `invalid_argument`，不存在的来源报 `not_found`）。
    ///
    /// 幂等：同一个 `(scope, reason, operationId)` 重复调用返回同一条任务；同一个
    /// operationId 换了 scope 或原因报 `idempotency_conflict`。
    pub fn rebuild_indexes(
        &self,
        scope: String,
        reason: String,
        operation_id: String,
    ) -> Result<Job, BridgeError> {
        let mut core = self.lock()?;
        Ok(core.schedule_index_rebuild(&scope, &reason, &operation_id)?)
    }

    /// 排一条**语义索引**重建任务，契约第 4.4 节 `indexes.rebuildSemantic`。
    ///
    /// **异步**：只排任务并返回它，不做同步重建。与 `rebuildIndexes` 的区别（三条）：
    ///
    /// - **整库**：没有 `scope`。任务业务键是 `(semantic_index, 空目标, 空快照)`，所以
    ///   「队列里至多一条活着的语义重建」，重复排队不产生第二份工作；
    /// - **等模型配置**：本地模型没配好时任务落 `waiting_configuration` +
    ///   `errorCode = waiting_model`（不是失败、不退避），并且不进 `nextWakeup`——配置
    ///   变化不是时间问题，下一次 `runDueJobs` 的前置评估会把它拉起；
    /// - **可分段让出**：整库重建会远超一次唤醒的预算，预算到点时它在块与块之间回到
    ///   `queued`（进度在 `Job.progress` 上、`attemptCount` 不增加），下一轮续跑；
    ///   代次切换仍是原子的，让出不算「已完成」。
    ///
    /// 幂等：同一个 `(reason, operationId)` 重复调用返回同一条任务；同一个 operationId
    /// 换了原因报 `idempotency_conflict`；任务跑完后再请求会建新的一条（按现状增量重算）。
    pub fn rebuild_semantic_index(
        &self,
        reason: String,
        operation_id: String,
    ) -> Result<Job, BridgeError> {
        let mut core = self.lock()?;
        Ok(core.schedule_semantic_index(&reason, &operation_id)?)
    }

    // ------------------------------------------------------------ 检索会话

    /// 发起一次检索，契约第 4.4 节 `search.start`。
    pub fn start_search(
        &self,
        request: SearchRequest,
        query_revision: i64,
    ) -> Result<SearchSnapshot, BridgeError> {
        let mut core = self.lock()?;
        Ok(core.start_search(request, query_revision)?)
    }

    /// 翻页；`cursor` 为空表示接着当前进度。游标不属于这个会话报 `cursor_expired`。
    pub fn search_next_page(
        &self,
        session_id: String,
        cursor: Option<String>,
    ) -> Result<SearchSnapshot, BridgeError> {
        let mut core = self.lock()?;
        Ok(core.search_next_page(&session_id, cursor.as_deref())?)
    }

    /// 读当前快照；会话不存在或索引变了报 `search_expired`。
    pub fn search_snapshot(&self, session_id: String) -> Result<SearchSnapshot, BridgeError> {
        let core = self.lock()?;
        Ok(core.search_snapshot(&session_id)?)
    }

    /// 取消后续处理；不删除任何原件。
    pub fn cancel_search(&self, session_id: String) -> Result<SearchSnapshot, BridgeError> {
        let mut core = self.lock()?;
        Ok(core.cancel_search(&session_id)?)
    }

    /// 把混合会话从关键词阶段升级到融合阶段（#69）；幂等，重复调用不重复算。
    pub fn upgrade_search(&self, session_id: String) -> Result<SearchSnapshot, BridgeError> {
        let mut core = self.lock()?;
        Ok(core.upgrade_search_to_hybrid(&session_id)?)
    }

    // ------------------------------------------------------------ 任务与事件

    /// 按状态列出任务；不传状态就列全部。
    pub fn list_jobs(
        &self,
        states: Option<Vec<JobState>>,
        limit: u32,
    ) -> Result<Vec<Job>, BridgeError> {
        let core = self.lock()?;
        Ok(core.list_jobs(states.as_deref(), limit as usize)?)
    }

    pub fn get_job(&self, job_id: String) -> Result<Job, BridgeError> {
        let core = self.lock()?;
        Ok(core.get_job(&job_id)?)
    }

    /// User cancellation keeps captured material and version history intact.
    pub fn cancel_job(&self, job_id: String, operation_id: String) -> Result<Job, BridgeError> {
        let mut core = self.lock()?;
        Ok(core.cancel_job(&job_id, &operation_id)?)
    }

    /// Retry a failed job with an idempotent user action identifier.
    pub fn retry_job(&self, job_id: String, operation_id: String) -> Result<Job, BridgeError> {
        let mut core = self.lock()?;
        Ok(core.retry_job_idempotent(&job_id, &operation_id)?)
    }

    /// 任务面板状态：各状态计数、到期积压、下次唤醒与最近错误。
    ///
    /// 最近错误只有 kind + 错误码 + 时间（外加不透明的任务 ID），**没有用户内容**。
    pub fn job_status(&self) -> Result<JobStatus, BridgeError> {
        let core = self.lock()?;
        Ok(core.job_status()?)
    }

    /// **跑一轮到期任务**（驱动），返回本轮摘要：跑了几条、成功/失败各几条、其中多少条是
    /// **补做**（本轮领取时排期已过期）、还有多少到期未跑（被预算/条数截住）、有多少在等
    /// 依赖、下一个唤醒时刻，以及本次的唤醒来源。
    ///
    /// - `budget_ms` 是平台授予的**时间预算**（毫秒），对齐契约 `jobs.runDue` 的
    ///   `budgetMs`；`0` 表示这一轮一条都不跑。预算**只在任务之间判断**：已经在跑的
    ///   任务不打断（核心不做抢占），所以一条任务本身可以比预算还长；
    /// - `trigger` 是唤醒来源（`startup` / `foreground` / `wake` / `manual` …），
    ///   核心如实记进摘要但不改变行为；什么时候调用由平台层决定（#19）；
    /// - 条数上限由这一层夹一个防御性常量 `RUN_DUE_MAX_JOBS`（契约没给这个参数）：
    ///   没跑上的任务留在队列里、如实计入 `dueBacklog`，下一轮接着跑。
    pub fn run_due_jobs(
        &self,
        budget_ms: u32,
        trigger: String,
    ) -> Result<JobRunSummary, BridgeError> {
        let mut core = self.lock()?;
        Ok(core.run_due_jobs(Utc::now(), u64::from(budget_ms), &trigger, RUN_DUE_MAX_JOBS)?)
    }

    /// 建议的下次唤醒时刻；没有待办时为空。
    pub fn next_wakeup(&self) -> Result<Option<DateTime<Utc>>, BridgeError> {
        let core = self.lock()?;
        Ok(core.next_wakeup()?)
    }

    /// 从某个序号之后读取持久业务事件。
    ///
    /// 核心按**分页**读（`Core::events_page`，SQL 带 `LIMIT`）；这一层为了保持 Dart
    /// 面「一次拿全」的语义循环取页，但每次查询只取 `EVENTS_PAGE` 条，不会把整张
    /// 事件表一次性 materialize 进 SQLite → Rust 的中间列表。真正的有界读取在
    /// **C ABI** 那一路（`core.events` 直接透传 `limit`/`truncated`）；这里仍是
    /// 「全部事件」的返回形状，调用方自己管好游标。
    pub fn events_since(&self, from_sequence: i64) -> Result<Vec<DomainEvent>, BridgeError> {
        let core = self.lock()?;
        let mut events = Vec::new();
        let mut cursor = from_sequence;
        loop {
            let page = core.events_page(cursor, EVENTS_PAGE)?;
            if page.events.is_empty() {
                break;
            }
            cursor = page.next_sequence;
            let done = !page.truncated;
            events.extend(page.events);
            if done {
                break;
            }
        }
        Ok(events)
    }

    // ------------------------------------------------------------ 服务配置与主动回应（B4a）

    /// 列出脱敏后的服务配置，契约 `providers.list`。
    ///
    /// **不含密钥**：`credentialRef` 是凭据存储里的引用，只有「配没配」这个信息。
    pub fn list_providers(&self) -> Result<Vec<Provider>, BridgeError> {
        let core = self.lock()?;
        Ok(core.list_providers()?)
    }

    /// 保存服务配置，契约 `providers.save`。
    ///
    /// `secret` 是明文密钥，**只在这一次调用里经过桥**：核心把它写进密钥托管后只留
    /// 引用（`credentialRef`），数据库、返回值与日志里都没有明文。传 `None` 表示不改
    /// 密钥（沿用配置里的引用；引用留空就是清掉凭据）。
    ///
    /// 协议/端点/模型变了会让上一次的连接测试结论作废——那是对旧配置的验证。
    pub fn save_provider(
        &self,
        config: Provider,
        secret: Option<String>,
    ) -> Result<Provider, BridgeError> {
        let mut core = self.lock()?;
        Ok(core.save_provider(&config, secret.as_deref())?)
    }

    /// 移除服务配置，契约 `providers.remove`。**不删除日记**，也不删除已经生成的回应。
    pub fn remove_provider(&self, provider_id: String) -> Result<(), BridgeError> {
        let mut core = self.lock()?;
        Ok(core.remove_provider(&provider_id)?)
    }

    /// 连接测试，契约 `providers.test`。
    ///
    /// **只有显式调用才发网络请求**；`scope` 是用户授权的测试范围，报告里逐项给结论，
    /// 并如实说明「是否真的发生了模型调用」。失败以可区分的错误码抛出
    /// （`provider_not_configured` / `authentication_failed` / `provider_timeout` …），
    /// 消息里写明本次发起了几次真实请求。
    pub fn test_provider(
        &self,
        provider_id: String,
        scope: Vec<ProviderCapability>,
    ) -> Result<ProviderTestReport, BridgeError> {
        let mut core = self.lock()?;
        Ok(core.test_provider(&provider_id, &scope)?)
    }

    /// 主动请求一次回应，契约 `responses.request`：**只有主动调用才运行**。
    ///
    /// 能力不满足时直接拒绝（`provider_not_configured`，消息里带可执行的下一步），
    /// **不排一条注定失败的任务**；返回的 `jobId` 由驱动在 `run_due_jobs` 里执行。
    pub fn request_response(
        &self,
        capture_ids: Vec<String>,
        request_text: String,
        provider_id: Option<String>,
    ) -> Result<ResponseTicket, BridgeError> {
        let mut core = self.lock()?;
        Ok(core.request_response(&capture_ids, &request_text, provider_id.as_deref())?)
    }

    /// 读取一条回应，契约 `responses.get`。还没生成时正文为空（如实，不编内容）。
    pub fn get_response(&self, response_id: String) -> Result<AiResponse, BridgeError> {
        let core = self.lock()?;
        Ok(core.get_response(&response_id)?)
    }

    // ------------------------------------------------------------ 日记生成与版本（B4b）

    /// 某一天的日记聚合视图，契约 `diary.getDay`。
    ///
    /// **候选与当前只在这里区分**：契约的 `DiaryVersion` 没有状态字段，所以
    /// `currentVersionId` / `candidateVersionIds` 是唯一的口径。空日报 `not_found`。
    pub fn get_diary_day(&self, day_key: String) -> Result<DiaryDay, BridgeError> {
        let core = self.lock()?;
        Ok(core.get_diary_day(&day_key)?)
    }

    /// 生成一篇日记，契约 `diary.generate`。
    ///
    /// **输入在返回之前就冻结**（`sourceSnapshotId` 指的那一行），真正的调用由
    /// `run_due_jobs` 执行。首次生成可成为当前版本；之后一律候选，**当前版本不会被
    /// 生成改写**（`reason = regenerate` 是「用户明确再要一版」的例外，产新候选）。
    ///
    /// 没有配好服务**照样返回票据**：那条任务会停在 `waiting_configuration` 等配置，
    /// 由 `jobs.get` 看得到状态；不是一次调用失败，也不产生任何版本。
    pub fn generate_diary(
        &self,
        day_key: String,
        style_id: Option<String>,
        reason: DiaryGenerationReason,
        operation_id: String,
    ) -> Result<DiaryGenerationTicket, BridgeError> {
        let mut core = self.lock()?;
        Ok(core.generate_diary(&day_key, style_id.as_deref(), reason, &operation_id)?)
    }

    /// 分页读某一天的日记版本，契约 `diary.listVersions`。最新在前。
    pub fn list_diary_versions(
        &self,
        day_key: String,
        cursor: Option<String>,
        limit: u32,
    ) -> Result<DiaryVersionPage, BridgeError> {
        let core = self.lock()?;
        Ok(core.list_diary_versions(&day_key, cursor.as_deref(), limit as usize)?)
    }

    /// 读一条完整的日记版本（含生成信息与来源关联），契约 `diary.getVersion`。
    pub fn get_diary_version(&self, version_id: String) -> Result<DiaryVersion, BridgeError> {
        let core = self.lock()?;
        Ok(core.get_diary_version(&version_id)?)
    }

    /// 保存人工编辑：新写一条 `user_edit` 版本并让它成为当前，契约 `diary.saveEdit`。
    ///
    /// **不原地改基版**（基版一个字节都不动），旧当前版退成候选。`expectedRevision`
    /// 是这一天的版本数（`candidateVersionIds.length + (currentVersionId == null ? 0 : 1)`），
    /// 不符报 `revision_conflict` 且零改动；同一条 `operationId` 重试返回原版本。
    pub fn save_diary_edit(
        &self,
        base_version_id: String,
        body: String,
        expected_revision: i64,
        operation_id: String,
    ) -> Result<DiaryVersion, BridgeError> {
        let mut core = self.lock()?;
        Ok(core.save_diary_edit(
            &base_version_id,
            &body,
            expected_revision,
            &operation_id,
        )?)
    }

    /// 指定某一天的当前版本，契约 `diary.setCurrent`。
    ///
    /// 只能指向这一天自己的版本（不存在 `not_found`、跨天 `invalid_argument`）；
    /// `expectedRevision` 不符 `revision_conflict`；切到已经是当前的版本是**无操作**，
    /// 所以这一行契约没有 `operationId`（不产生任何东西，重复调用收敛到同一状态）。
    pub fn set_diary_current(
        &self,
        day_key: String,
        version_id: String,
        expected_revision: i64,
    ) -> Result<DiaryDay, BridgeError> {
        let mut core = self.lock()?;
        Ok(core.set_diary_current(&day_key, &version_id, expected_revision)?)
    }

    /// 把两版（或多版）合并成一条新版本，契约 `diary.merge`。
    ///
    /// `body` 是用户确认后的正文（核心不调模型）；来源记在
    /// `generationInfo.generationOptions.mergeSourceVersionIds`；参与合并的版本全部保留。
    pub fn merge_diary_versions(
        &self,
        base_version_id: String,
        candidate_version_ids: Vec<String>,
        body: String,
        expected_revision: i64,
        operation_id: String,
    ) -> Result<DiaryVersion, BridgeError> {
        let mut core = self.lock()?;
        Ok(core.merge_diary_versions(
            &base_version_id,
            &candidate_version_ids,
            &body,
            expected_revision,
            &operation_id,
        )?)
    }

    // ------------------------------------------------------------ 风格（#114）

    /// 全部风格的当前版本；默认风格 `sty_default` 也在列表里。
    pub fn list_styles(&self) -> Result<Vec<StyleProfile>, BridgeError> {
        let core = self.lock()?;
        Ok(core.list_styles()?)
    }

    /// 读一条风格的当前版本；不存在报 `not_found`。
    pub fn get_style(&self, style_id: String) -> Result<StyleProfile, BridgeError> {
        let core = self.lock()?;
        Ok(core.get_style(&style_id)?)
    }

    /// 新建/更新风格；带 `expectedRevision` 时是乐观锁（不符报 `revision_conflict`）。
    pub fn save_style(
        &self,
        profile: StyleProfile,
        expected_revision: Option<i64>,
    ) -> Result<StyleProfile, BridgeError> {
        let mut core = self.lock()?;
        Ok(core.save_style(&profile, expected_revision)?)
    }

    // ------------------------------------------------------------ 整理与自动化（B4e）

    /// 全部可用的整理模板（现在只有三个核心内置的；声明式插件模板见 #88）。
    ///
    /// 模板的规则是**产品指令**（只在 system 消息里），参数只接受整数与布尔——
    /// 两者都不接受自由文本，所以「把材料里的文字变成指令」没有入口。
    pub fn organize_templates(&self) -> Result<Vec<OrganizeTemplate>, BridgeError> {
        let core = self.lock()?;
        Ok(core.organize_templates())
    }

    /// 整理设置：开关（**默认关**）、模板与参数、各项上限与计划触发时间。
    ///
    /// 这是「必须由用户显式开启」的落点：新建与升级上来的资料库都是关的，
    /// 只有 `update_organize_settings` 显式传 `enabled = true` 才会打开。
    ///
    /// **能力清单里不出现在 `settings.get` 名下**（那是契约里覆盖全部可编辑设置的
    /// 方法，还包含日记时区等平台层字段，这一片没有实现）：这里是核心真实的整理
    /// 设置对象，界面按需直接调用，不做能力名上的假装。
    pub fn organize_settings(&self) -> Result<OrganizeSettings, BridgeError> {
        let core = self.lock()?;
        Ok(core.organize_settings()?)
    }

    /// 更新整理设置（契约 `settings.update` 的整理那一段）：整份替换 + `expectedRevision`。
    ///
    /// 校验先行（未知模板 `not_found`；参数/上限/时间不合法 `invalid_argument`；
    /// 过期 `revision_conflict`），**被拒的调用零改动**；值没变就是无操作
    /// （`revision` 不 +1）。传入对象里的 `revision` / `updatedAt` 会被忽略
    /// （它们是只读结论，与 `providers.save` 忽略 `testStatus` 同一个口径）。
    pub fn update_organize_settings(
        &self,
        settings: OrganizeSettings,
        expected_revision: i64,
    ) -> Result<OrganizeSettings, BridgeError> {
        let mut core = self.lock()?;
        Ok(core.update_organize_settings(&settings, expected_revision)?)
    }

    /// 整理状态：开关、模板、**什么时候会跑、会跑哪些材料**、配额用量与最近一次结果。
    ///
    /// **只读**：不发请求、不排任务、不改任何东西。没开启时也能看出「开启后会跑什么」。
    /// 配额与失败率都来自真实的调用账本；超限停下时 `blockedCode` / `blockedReason`
    /// 写清哪一条上限、已经做了什么、下一步怎么办。
    pub fn organize_status(&self) -> Result<OrganizeStatus, BridgeError> {
        let core = self.lock()?;
        Ok(core.organize_status()?)
    }

    // ------------------------------------------------------------ 插件（B6，#88）
    //
    // 安装流程与契约 `plugins.inspectPackage` / `install` 对齐：先检查（不执行包内容），
    // 展示权限后把用户勾选的 `grants` 原样传进来；缺一项就拒绝并说清缺什么。
    // 这里**不放业务规则**，只做参数转发与错误码映射。

    /// 检查插件包（目录或 zip）。不执行任何包内容。
    pub fn inspect_plugin_package(
        &self,
        package_path: String,
        platform: String,
    ) -> Result<PluginInspection, BridgeError> {
        let core = self.lock()?;
        let platform = if platform.trim().is_empty() {
            diary_core::plugin::host_platform().to_owned()
        } else {
            platform
        };
        Ok(core.inspect_plugin_package(package_path, &platform)?)
    }

    /// 安装/升级插件。`grants` 是用户在这次安装里显式勾选的权限。
    pub fn install_plugin(
        &self,
        package_path: String,
        platform: String,
        grants: Vec<String>,
    ) -> Result<PluginInstallation, BridgeError> {
        let mut core = self.lock()?;
        let platform = if platform.trim().is_empty() {
            diary_core::plugin::host_platform().to_owned()
        } else {
            platform
        };
        Ok(core.install_plugin(package_path, &platform, &grants)?)
    }

    /// 启用/停用。停用后新的调用被拒，历史日记与导出物保留。
    pub fn set_plugin_enabled(
        &self,
        plugin_id: String,
        enabled: bool,
    ) -> Result<PluginInstallation, BridgeError> {
        let mut core = self.lock()?;
        Ok(core.set_plugin_enabled(&plugin_id, enabled)?)
    }

    /// 卸载。`remove_private_state` 对齐契约的「可选择的私有缓存」；执行文件总会清理。
    pub fn uninstall_plugin(
        &self,
        plugin_id: String,
        remove_private_state: bool,
    ) -> Result<(), BridgeError> {
        let mut core = self.lock()?;
        Ok(core.uninstall_plugin(&plugin_id, remove_private_state)?)
    }

    /// 已安装插件。只读缓存清单，不读、不执行插件入口。
    pub fn list_plugins(&self) -> Result<Vec<PluginInstallation>, BridgeError> {
        let core = self.lock()?;
        Ok(core.list_plugins()?)
    }

    /// 声明式模板缓存（可按插件过滤）。启动/列表路径不会碰插件文件。
    pub fn plugin_templates(&self, plugin_id: Option<String>) -> Result<Vec<PluginTemplate>, BridgeError> {
        let core = self.lock()?;
        Ok(core.plugin_templates(plugin_id.as_deref())?)
    }

    /// 运行插件（`plugins.invoke`）。激活点、权限、预算与隔离都由核心收口。
    pub fn run_plugin(&self, invocation: PluginInvocation) -> Result<PluginRunResult, BridgeError> {
        let mut core = self.lock()?;
        Ok(core.run_plugin(&invocation)?)
    }

    /// 已经接到桥上的契约方法名。
    pub fn capabilities(&self) -> Vec<String> {
        wired_capabilities()
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Core>, BridgeError> {
        self.inner.lock().map_err(|_| BridgeError {
            code: "invalid_state".to_owned(),
            message: "会话状态已损坏（互斥锁中毒）".to_owned(),
            retryable: false,
        })
    }
}

fn recovery_summary(core: &Core) -> Result<RecoverySummary, CoreError> {
    // 数字直接来自库，不猜。
    let recoverable_imports = core.count_imports_in_flight()? as u32;
    let open_recordings = core.count_open_recordings()? as u32;
    // 走精确计数：`list_jobs` 的 limit 会被夹到 200，拿它当计数会在任务多时少报。
    let pending_jobs = core.count_jobs(Some(&[JobState::Queued, JobState::RetryWait]))? as u32;

    let mut notes = Vec::new();
    if recoverable_imports > 0 {
        notes.push(format!("有 {recoverable_imports} 个导入上次没有完成"));
    }
    if open_recordings > 0 {
        notes.push(format!("有 {open_recordings} 段录音上次没有收尾"));
    }
    Ok(RecoverySummary {
        recoverable_imports,
        open_recordings,
        pending_jobs,
        notes,
    })
}

/// 已经接到桥上的契约方法名。
///
/// 这是**诚实的能力声明**：没列出来的就是还没接，界面不该把它显示成可用。
fn wired_capabilities() -> Vec<String> {
    [
        "core.open",
        "core.snapshot",
        "captures.createDraft",
        "captures.saveDraft",
        "captures.commit",
        "captures.get",
        "captures.list",
        "imports.prepare",
        "imports.finish",
        "imports.status",
        "imports.cancel",
        "recordings.prepare",
        "recordings.registerSegment",
        "recordings.updateState",
        "recordings.finalize",
        "recordings.recover",
        "sources.extract",
        "sources.extractedContent",
        "sources.reviseText",
        "sources.previousRevision",
        "sources.locate",
        "indexes.status",
        "indexes.rebuild",
        "indexes.rebuildSemantic",
        "search.start",
        "search.nextPage",
        "search.snapshot",
        "search.cancel",
        "search.upgrade",
        "jobs.list",
        "jobs.get",
        "jobs.cancel",
        "jobs.retry",
        "jobs.status",
        "jobs.runDue",
        "jobs.nextWakeup",
        "providers.list",
        "providers.save",
        "providers.remove",
        "providers.test",
        "responses.request",
        "responses.get",
        "diary.getDay",
        "diary.generate",
        "diary.listVersions",
        "diary.getVersion",
        "diary.saveEdit",
        "diary.setCurrent",
        "diary.merge",
        // #114 风格管理：三种方法都走真实存储；生成时的风格快照已是版本行。
        "styles.list",
        "styles.get",
        "styles.save",
        "core.events",
        // B7：备份与恢复这一轮是**同步**入口（契约那三行返回 jobId，任务化见 #70）。
        // 名字照契约写，界面看到的仍是 Dart 适配层窄化过的清单。
        "backups.create",
        "backups.inspect",
        "backups.restore",
        // B6（#88）：契约里的 plugins.* 六条真的接上了；外加 `plugins.templates`
        // 是列表缓存模板的加性方法（契约的 `plugins.list` 只返回安装状态）。
        "plugins.inspectPackage",
        "plugins.install",
        "plugins.enable",
        "plugins.disable",
        "plugins.uninstall",
        "plugins.list",
        "plugins.invoke",
        "plugins.templates",
    ]
    .iter()
    .map(|name| (*name).to_owned())
    .collect()
}

#[cfg(test)]
mod session_tests {
    use super::*;

    #[test]
    fn same_library_shares_one_core_until_last_session_closes() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("diary-bridge-shared-{unique}"));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("library.sqlite");
        let foreground = BridgeSession::open(path.to_string_lossy().into_owned()).unwrap();
        let worker = BridgeSession::open(path.to_string_lossy().into_owned()).unwrap();
        assert!(Arc::ptr_eq(&foreground.inner, &worker.inner));
        drop(foreground);
        drop(worker);
        let reopened = BridgeSession::open(path.to_string_lossy().into_owned()).unwrap();
        assert_eq!(reopened.info().unwrap().library_id, "library");
        drop(reopened);
        std::fs::remove_dir_all(dir).unwrap();
    }
}

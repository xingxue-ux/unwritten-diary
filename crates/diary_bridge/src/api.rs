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
    Capture, CapturePage, CommitResult, Core, CoreError, DomainEvent, DraftSaveResult,
    ExtractedContent, ImportManifest, ImportOrigin, ImportStatus, ImportTicket, IndexStatus, Job,
    JobRunSummary, JobState, JobStatus, SearchRequest, SearchSnapshot, SourceLocation,
    SourceLocator,
    SourceRevision,
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
            api_version: "1.0".to_owned(),
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
        Ok(core.run_due_jobs(
            Utc::now(),
            u64::from(budget_ms),
            &trigger,
            RUN_DUE_MAX_JOBS,
        )?)
    }

    /// 建议的下次唤醒时刻；没有待办时为空。
    pub fn next_wakeup(&self) -> Result<Option<DateTime<Utc>>, BridgeError> {
        let core = self.lock()?;
        Ok(core.next_wakeup()?)
    }

    /// 从某个序号之后读取持久业务事件。
    pub fn events_since(&self, from_sequence: i64) -> Result<Vec<DomainEvent>, BridgeError> {
        let core = self.lock()?;
        Ok(core.events_since(from_sequence)?)
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
    let pending_jobs = core
        .count_jobs(Some(&[JobState::Queued, JobState::RetryWait]))?
        as u32;

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
        "sources.extract",
        "sources.extractedContent",
        "sources.reviseText",
        "sources.previousRevision",
        "sources.locate",
        "indexes.status",
        "indexes.rebuild",
        "search.start",
        "search.nextPage",
        "search.snapshot",
        "search.cancel",
        "search.upgrade",
        "jobs.list",
        "jobs.get",
        "jobs.status",
        "jobs.runDue",
        "jobs.nextWakeup",
        "core.events",
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

//! 桥接层：把 `diary_core` 的能力暴露给 Dart。
//!
//! 这一层**不放业务规则**，只做三件事：
//! 1. 把 Dart 传来的参数转成核心调用；
//! 2. 把核心结果原样交出去（类型由 codegen 镜像，不另造一套 DTO）；
//! 3. 把核心错误转成契约第 7 节的错误码。
//!
//! 核心的公开模型（`diary_core::model`）已经写进 `flutter_rust_bridge.yaml` 的
//! `rust_input`，所以这里返回的是真正的字段级镜像，而不是不透明句柄。

use std::sync::Mutex;

use chrono::{DateTime, Utc};
use diary_core::{
    Capture, CapturePage, CommitResult, Core, CoreError, DomainEvent, DraftSaveResult,
    ExtractedContent, ImportManifest, ImportOrigin, ImportStatus, ImportTicket, IndexStatus, Job,
    JobState, SourceLocation, SourceLocator,
};
use flutter_rust_bridge::frb;

/// 桥接错误：`code` 是契约第 7 节的错误码，`message` 给用户看。
///
/// frb 会把它变成 Dart 侧的异常类型，所以前端可以用 `on BridgeError catch (e)`
/// 直接读 `e.code`。
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
}

/// 一个打开的资料库会话。Dart 侧持有的就是这个句柄。
#[frb(opaque)]
pub struct BridgeSession {
    inner: Mutex<Core>,
    library_id: String,
}

impl BridgeSession {
    /// 打开（或创建）资料库。
    pub fn open(library_path: String) -> Result<BridgeSession, BridgeError> {
        let path = std::path::PathBuf::from(&library_path);
        let core = Core::open(&path)?;
        let library_id = path
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_else(|| "library".to_owned());
        Ok(Self {
            inner: Mutex::new(core),
            library_id,
        })
    }

    /// 核心信息与恢复摘要。
    pub fn info(&self) -> Result<LibraryInfo, BridgeError> {
        let core = self.lock()?;
        let recovery = recovery_summary(&core)?;
        Ok(LibraryInfo {
            api_version: "1.0".to_owned(),
            data_schema_version: core.schema_version()?,
            build_version: env!("CARGO_PKG_VERSION").to_owned(),
            library_id: self.library_id.clone(),
            capabilities: wired_capabilities(),
            recovery,
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
    let pending_jobs =
        core.list_jobs(Some(&[JobState::Queued, JobState::RetryWait]), 1000)?.len() as u32;

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
        "sources.locate",
        "indexes.status",
        "jobs.list",
        "jobs.get",
        "jobs.nextWakeup",
        "core.events",
    ]
    .iter()
    .map(|name| (*name).to_owned())
    .collect()
}
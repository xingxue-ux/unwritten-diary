//! 「不写日记」设备内本地核心。
//!
//! 已落地：记录路径（B1a）、原件文件库与导入（B1b）、录音与队列（B1c）、
//! 提取与来源定位（B2）、关键词索引与覆盖状态（B3a）、检索会话 `search.*`（B3b）、
//! 用户自己写的文字进检索（B3d）、文本块与真实向量（B3c-2）、
//! 混合检索 RRF（#51，B3c-3）。
//!
//! 三条硬性约束贯穿本 crate：
//! 1. **不丢已确认的记录**：写入落在同一个事务里，事务提交后才返回 durable。
//! 2. **幂等**：可重试写入带 operationId，重复提交返回原结果；
//!    同一个 operationId 换了内容报 `idempotency_conflict`。
//! 3. **乐观锁**：带 expectedRevision 的写入不匹配就报 `revision_conflict`，
//!    绝不静默覆盖用户内容。

mod assets;
mod chunker;
mod chunks;
mod error;
/// 本地向量模型接入（B3c-2）。公开是为了让测试与 `tools/probe` 能显式注入 stub 或
/// 指定模型包——这是给测试/量测的入口，不是让产品代码去直接碰 ORT。
pub mod embedding;
mod extractors;
mod index_rebuild;
mod jobs;
/// 数据对象。公开是为了让桥接层的 codegen 能扫描它们（见 flutter_rust_bridge.yaml）。
pub mod model;
/// Provider 适配与配置（B4a）：配置、密钥托管、能力协商、连接测试与可注入传输层。
///
/// 公开是为了让测试与 `tools/probe` 能注入回放/记账传输层——这是给测试与量测的
/// 入口，产品代码只用默认实现（见 `provider::transport`）。
pub mod provider;
/// AI 质量集的**判定器**（issue #93，B4b-ai-quality）：可判定的特征判定、CER 计算、
/// 回放式引擎与「调用 B4b 核心方法」的薄适配层。
///
/// 它是**量测代码**（与 `tools/probe` 里的 `quality.rs` 同性质），不是产品路径：
/// 默认不开，靠 `ai-quality` feature 打开（集成测试与探针会开）。放在核心而不是探针里，
/// 是因为同一套判定器要同时给探针命令与集成测试（回归）用——复制两份必然各自漂移。
#[cfg(any(test, feature = "ai-quality"))]
pub mod quality;
mod recordings;
mod schema;
mod search;
mod search_session;
mod semantic;
mod support;

pub use assets::ImportRequest;
pub use extractors::{
    builtin_extractors, ExtractionInput, ExtractionOutcome, Extractor, TEXT_DIRECT_EXTRACTOR_ID,
    TEXT_DIRECT_EXTRACTOR_VERSION,
};
pub use error::{CoreError, ErrorCode, Result};
pub use provider::adapter::{
    adapter_for, builtin_adapter_ids, GenerateOutcome, GenerateRequest, ProtocolAdapter,
    CALL_TIMEOUT_MS, OPENAI_COMPATIBLE, PROBE_TIMEOUT_MS,
};
pub use provider::secret::{FileSecretStore, MemorySecretStore, SecretStore};
pub use provider::transport::{
    CountingTransport, HttpRequest, HttpResponse, PanicTransport, ScriptedTransport, Transport,
    TransportError, UreqTransport,
};
pub use model::{
    AiResponse, Asset, AssetLease, AssetStorageState, AuthorType, Capture, CapturePage,
    CaptureState, CommitResult, Coverage, DomainEvent, DraftSaveResult, EventType,
    ExtractedContent, ExtractedSegment, ImportManifest, ImportOrigin, ImportState, ImportStatus,
    ImportTicket, IndexStatus, Job, JobErrorSummary, JobPriority, JobProgress, JobRunSummary,
    JobState, JobStatus, LocatorType, MatchedBy, NativeRecordingStatus, NewJob,
    ProcessingStatus, ProcessingSummary, Provider, ProviderCapability, ProviderTestReport,
    ProviderTestStatus, RecordingFinalizeResult, RecordingRecovery, RecordingSession,
    RecordingState, RecordingTicket, ResponseTicket, SearchFilters, SearchHit, SearchMode,
    SearchPhase, SearchRequest, SearchSnapshot, SegmentManifest, SegmentReceipt, SourceItem,
    SourceKind, SourceLocation, SourceLocator, SourceRevision, TextRange,
};
pub use provider::CapabilityGate;
pub use chunker::{
    chunks_for, pack_pieces, Chunk, ChunkSpan, Piece, CHUNKER_VERSION, CHUNK_OVERLAP_CHARS,
    MAX_CHUNK_CHARS,
};
pub use embedding::{
    bytes_to_vector, config_from_env, load as load_embedder, text_hash_vector, vector_bytes,
    EmbedOptions, Embedder, EmbeddingConfig, EnvEmbedding, OrtEmbedder, Pooling, DEFAULT_POOLING,
    DEFAULT_QUERY_INSTRUCTION, EXPECTED_DIMS, MODEL_ID, QUERY_INSTRUCTION,
};
pub use schema::SCHEMA_VERSION;
pub use search::TOKENIZER_VERSION;
pub use semantic::SemanticBuildReport;
pub use search_session::{HYBRID_FUSION_VERSION, HYBRID_SEMANTIC_MIN_SCORE};

use std::collections::HashMap;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension, Transaction};

/// 创建草稿的入参。
#[derive(Debug, Clone)]
pub struct CreateDraftInput<'a> {
    /// 事件时间；为空时用当前时间。
    pub occurred_at: Option<DateTime<Utc>>,
    /// 设备当前的 IANA 时区名，例如 `Asia/Shanghai`。
    pub time_zone: &'a str,
    /// 事件发生时该时区的偏移分钟数。
    pub utc_offset_minutes: i32,
    pub operation_id: &'a str,
}

/// 资料库句柄。
pub struct Core {
    conn: Connection,
    /// 文件库根目录：资料库文件所在目录。内存库没有根目录，导入相关方法会报错。
    root: Option<PathBuf>,
    /// 只读租约，内存态，不持久化。
    leases: HashMap<String, assets::LeaseRecord>,
    /// 检索会话，内存态：它记录的是「这一次查询的翻页与失效判断」，不是业务数据。
    search_sessions: HashMap<String, search_session::SearchSessionState>,
    /// 语义这一路的运行时状态：模型配置 + 懒加载的嵌入器（进程级单例）。
    ///
    /// `Core::open` 只读环境变量与 stat 文件，**不建 ORT 会话**——启动不做重活。
    semantic: semantic::SemanticRuntime,
    /// 混合检索的语义证据门槛。产品默认是 `HYBRID_SEMANTIC_MIN_SCORE`；量测与测试
    /// 可以临时改它来扫曲线（见 `set_hybrid_min_score_for_test`）。
    hybrid_min_score: f32,
    /// 密钥托管。默认实现是资料库目录下权限受限的独立文件；测试与内存库用内存实现。
    /// **明文只在这一层**，`providers` 表里只有引用（见 `provider::secret`）。
    secrets: Box<dyn provider::secret::SecretStore>,
    /// 出站请求的唯一出口。默认：文件库用真实 HTTP（ureq + rustls），
    /// 内存库用「一旦被调用就 panic」的替身——内存库只用于测试，不该真的出网。
    transport: std::sync::Arc<dyn provider::transport::Transport>,
}

/// 一行的原始形态：先取出来，再按业务语义解析，避免把解析错误塞进 SQL 层。
type RawCapture = (String, i64, String, String, String, String, String, i64, String, String);

const CAPTURE_COLUMNS: &str = "id, revision, state, occurred_at, created_at, updated_at, \
                                time_zone, utc_offset_minutes, day_key, draft_text";

impl Core {
    /// 打开（或创建）指定路径的资料库。文件库根目录就是它所在的目录。
    pub fn open(path: impl AsRef<std::path::Path>) -> Result<Self> {
        let path = path.as_ref();
        let root = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .map_or_else(|| PathBuf::from("."), std::path::Path::to_path_buf);
        // 文件库是产品路径：默认装真实 HTTP 传输层。
        Self::prepare(
            Connection::open(path)?,
            Some(root),
            std::sync::Arc::new(provider::transport::UreqTransport::new()),
        )
    }

    /// 内存库，只用于测试。没有文件系统根目录，导入相关方法会报 `invalid_state`。
    ///
    /// 传输层默认是「一旦被调用就 panic」的替身：内存库只用于测试，**不该真的出网**。
    /// 要测 Provider 交互就显式 `set_transport_for_test` 注入回放替身。
    pub fn open_in_memory() -> Result<Self> {
        Self::prepare(
            Connection::open_in_memory()?,
            None,
            std::sync::Arc::new(provider::transport::PanicTransport {}),
        )
    }

    /// 内存库 + 一个文件系统根目录，用于测试导入而不必落一个真库文件。
    ///
    /// 传输层同 `open_in_memory`：测试默认不联网。
    pub fn open_in_memory_at(root: impl AsRef<std::path::Path>) -> Result<Self> {
        Self::prepare(
            Connection::open_in_memory()?,
            Some(root.as_ref().to_path_buf()),
            std::sync::Arc::new(provider::transport::PanicTransport {}),
        )
    }

    fn prepare(
        mut conn: Connection,
        root: Option<PathBuf>,
        transport: std::sync::Arc<dyn provider::transport::Transport>,
    ) -> Result<Self> {
        // WAL 提高并发读写表现；synchronous=FULL 保证「提交了就是落盘了」，
        // 只用 NORMAL 会让进程崩溃时丢掉最后几个事务，与 durable 承诺不符。
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "FULL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        schema::migrate(&mut conn)?;
        if let Some(dir) = &root {
            std::fs::create_dir_all(dir)?;
        }
        // 收拾上次没走完的导入与录音：都如实标成「没完成」，不假装成功。
        assets::recover_on_open(&conn)?;
        recordings::recover_on_open(&conn)?;
        // 密钥托管：文件库落在资料库目录下的 `secrets/`（与库文件分开），
        // 内存库（只用于测试）只在内存里。传输层由调用方给（见 `prepare` 的三个调用点）。
        let secrets: Box<dyn provider::secret::SecretStore> = match &root {
            Some(dir) => Box::new(provider::secret::FileSecretStore::beside_library(dir)),
            None => Box::new(provider::secret::MemorySecretStore::new()),
        };
        Ok(Self {
            conn,
            root,
            leases: HashMap::new(),
            search_sessions: HashMap::new(),
            semantic: semantic::SemanticRuntime::from_env(),
            hybrid_min_score: search_session::HYBRID_SEMANTIC_MIN_SCORE,
            secrets,
            transport,
        })
    }

    /// 文件库根目录。内存库没有根目录，导入相关方法会因此报错。
    pub(crate) fn root_dir(&self) -> Result<&std::path::Path> {
        self.root.as_deref().ok_or_else(|| CoreError::InvalidState {
            entity: "资料库",
            id: "<内存库>".to_owned(),
            state: "内存库没有文件系统根目录，不能导入原件".to_owned(),
        })
    }

    pub fn schema_version(&self) -> Result<i64> {
        Ok(self
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))?)
    }

    /// 仅供测试：把资料库可用的页数压小，用来触发 SQLite 的 `SQLITE_FULL`。
    ///
    /// 没有 root 权限时无法造一个真的满盘环境，这里用页数上限走同一条错误路径。
    #[doc(hidden)]
    pub fn set_page_limit_for_test(&self, pages: i64) -> Result<()> {
        self.conn.pragma_update(None, "max_page_count", pages)?;
        Ok(())
    }

    /// 仅供量测/测试：改混合检索的语义证据门槛。
    ///
    /// 阈值扫描必须走**同一条产品路径**（`start_search(mode = hybrid)` + 真融合），
    /// 而不是在探针里另写一套融合——否则量到的曲线不是线上跑的那条。产品代码不调它；
    /// 默认值仍是 `HYBRID_SEMANTIC_MIN_SCORE` 那个可审计常量。
    #[doc(hidden)]
    pub fn set_hybrid_min_score_for_test(&mut self, value: f32) {
        self.hybrid_min_score = value;
    }

    // ------------------------------------------------------------ 记录路径

    /// 创建草稿，契约第 4.1 节 `captures.createDraft`。
    pub fn create_draft(&mut self, input: CreateDraftInput<'_>) -> Result<Capture> {
        let occurred_hint = input.occurred_at.map(support::to_iso).unwrap_or_default();
        let fingerprint = support::fingerprint(&[
            "create_draft",
            &occurred_hint,
            input.time_zone,
            &input.utc_offset_minutes.to_string(),
        ]);
        if let Some(json) = self.receipt("create_draft", &fingerprint, input.operation_id)? {
            return Ok(serde_json::from_str(&json)?);
        }

        let now = support::now();
        let occurred_at = input.occurred_at.unwrap_or(now);
        let capture = Capture {
            id: support::new_id("cap"),
            revision: 1,
            state: CaptureState::Draft,
            occurred_at,
            created_at: now,
            updated_at: now,
            time_zone: input.time_zone.to_owned(),
            utc_offset_minutes: input.utc_offset_minutes,
            day_key: support::day_key(occurred_at, input.utc_offset_minutes),
            ordered_source_ids: Vec::new(),
            draft_text: String::new(),
            processing_summary: ProcessingSummary::default(),
        };

        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO captures (id, revision, state, occurred_at, created_at, updated_at, \
             time_zone, utc_offset_minutes, day_key, draft_text) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                capture.id,
                capture.revision,
                capture.state.wire(),
                support::to_iso(capture.occurred_at),
                support::to_iso(capture.created_at),
                support::to_iso(capture.updated_at),
                capture.time_zone,
                capture.utc_offset_minutes,
                capture.day_key,
                capture.draft_text,
            ],
        )?;
        store_receipt(
            &tx,
            input.operation_id,
            "create_draft",
            &fingerprint,
            &serde_json::to_string(&capture)?,
        )?;
        // 新记录的 draft_text 一定是空的：这里刷一次索引是「先清后写」的一致性
        // 保证——同一个 capture_id 被重用或重放时不会留下上一条的旧词项。
        search::index_capture(
            &tx,
            search::CaptureInput {
                capture_id: &capture.id,
                text: &capture.draft_text,
            },
        )?;
        insert_event(&tx, EventType::CaptureChanged, &capture.id, capture.revision)?;
        tx.commit()?;
        Ok(capture)
    }

    /// 保存草稿正文。频繁保存只改草稿，不制造永久版本，契约第 2.1 节。
    pub fn save_draft(
        &mut self,
        capture_id: &str,
        text: &str,
        expected_revision: i64,
        operation_id: &str,
    ) -> Result<DraftSaveResult> {
        let fingerprint = support::fingerprint(&[
            "save_draft",
            capture_id,
            &expected_revision.to_string(),
            text,
        ]);
        if let Some(json) = self.receipt("save_draft", &fingerprint, operation_id)? {
            return Ok(serde_json::from_str(&json)?);
        }

        let capture = load_capture(&self.conn, capture_id)?;
        ensure_revision(&capture, expected_revision)?;
        if capture.state != CaptureState::Draft {
            return Err(CoreError::InvalidState {
                entity: "记录",
                id: capture.id.clone(),
                state: capture.state.wire().to_owned(),
            });
        }

        let now = support::now();
        let new_revision = capture.revision + 1;
        let result = DraftSaveResult {
            revision: new_revision,
            // 事务提交后才返回 true：界面据此显示「已保存」。
            durable: true,
            saved_at: now,
        };

        let tx = self.conn.transaction()?;
        tx.execute(
            "UPDATE captures SET revision = ?1, draft_text = ?2, updated_at = ?3 WHERE id = ?4",
            params![new_revision, text, support::to_iso(now), capture.id],
        )?;
        store_receipt(
            &tx,
            operation_id,
            "save_draft",
            &fingerprint,
            &serde_json::to_string(&result)?,
        )?;
        // 草稿正文是用户自己写的文字，也要能被搜到。索引写入与保存放在同一个事务：
        // 不能出现「界面说已保存、检索却还看不到」这种两个事实的状态。
        search::index_capture(
            &tx,
            search::CaptureInput {
                capture_id: &capture.id,
                text,
            },
        )?;
        insert_event(&tx, EventType::CaptureChanged, &capture.id, new_revision)?;
        tx.commit()?;
        Ok(result)
    }

    /// 提交记录：创建原始文字版本，契约第 4.1 节 `captures.commit`。
    pub fn commit(
        &mut self,
        capture_id: &str,
        expected_revision: i64,
        operation_id: &str,
    ) -> Result<CommitResult> {
        self.commit_with_jobs(capture_id, expected_revision, operation_id, &[])
    }

    /// 提交记录，并**在同一个事务里**入队若干任务。
    ///
    /// 任务书 3.2 节要求「核心提交数据与对应的待处理任务/出站事件记录应在同一数据库
    /// 事务中完成」，否则会出现「资料已保存，索引任务却永远丢了」。
    pub fn commit_with_jobs(
        &mut self,
        capture_id: &str,
        expected_revision: i64,
        operation_id: &str,
        jobs_to_queue: &[NewJob],
    ) -> Result<CommitResult> {
        let mut parts: Vec<String> = vec![
            "commit".to_owned(),
            capture_id.to_owned(),
            expected_revision.to_string(),
        ];
        for job in jobs_to_queue {
            parts.push(job.kind.clone());
            parts.push(job.priority.value().to_string());
            parts.push(job.target_ids.join(","));
            // 快照、额度与首次到期时刻也是任务规格的一部分：同一个 operationId
            // 换了其中任何一个都算「换了内容」，必须报幂等冲突而不是静默返回旧结果。
            parts.push(job.input_snapshot_hash.clone().unwrap_or_default());
            parts.push(job.max_attempts.to_string());
            parts.push(job.next_attempt_at.map(support::to_iso).unwrap_or_default());
        }
        let fingerprint =
            support::fingerprint(&parts.iter().map(String::as_str).collect::<Vec<_>>());
        if let Some(json) = self.receipt("commit", &fingerprint, operation_id)? {
            return Ok(serde_json::from_str(&json)?);
        }

        let capture = load_capture(&self.conn, capture_id)?;
        ensure_revision(&capture, expected_revision)?;
        if capture.state != CaptureState::Draft {
            return Err(CoreError::InvalidState {
                entity: "记录",
                id: capture.id.clone(),
                state: capture.state.wire().to_owned(),
            });
        }

        let now = support::now();
        let new_revision = capture.revision + 1;
        let mut ordered = capture.ordered_source_ids.clone();
        let mut original_text_revision = None;

        let tx = self.conn.transaction()?;
        // 空白草稿不制造空的原始文字版本；有内容才建 source + revision。
        if !capture.draft_text.is_empty() {
            let source_id = support::new_id("src");
            let revision_id = support::new_id("rev");
            let position = ordered.len() as i64;
            tx.execute(
                "INSERT INTO source_items (source_id, capture_id, kind, position, \
                 current_revision_id) VALUES (?1, ?2, 'text', ?3, ?4)",
                params![source_id, capture.id, position, revision_id],
            )?;
            tx.execute(
                "INSERT INTO source_revisions (revision_id, source_id, parent_revision_id, text, \
                 asset_id, author_type, occurred_at) VALUES (?1, ?2, NULL, ?3, NULL, ?4, ?5)",
                params![
                    revision_id,
                    source_id,
                    capture.draft_text,
                    AuthorType::User.wire(),
                    support::to_iso(now),
                ],
            )?;
            ordered.push(source_id.clone());
            original_text_revision = Some(SourceRevision {
                revision_id,
                source_id,
                parent_revision_id: None,
                text: Some(capture.draft_text.clone()),
                asset_id: None,
                author_type: AuthorType::User,
                occurred_at: now,
            });
        }

        let mut committed = capture.clone();
        committed.revision = new_revision;
        committed.state = CaptureState::Committed;
        committed.updated_at = now;
        committed.ordered_source_ids = ordered;

        tx.execute(
            "UPDATE captures SET revision = ?1, state = 'committed', updated_at = ?2 WHERE id = ?3",
            params![new_revision, support::to_iso(now), capture.id],
        )?;

        // 任务与提交同一个事务：要么都成，要么都不成。
        // `job.changed` 由队列自己在入队时发（`jobs::enqueue_in_tx`），这里不重复发。
        for job in jobs_to_queue {
            jobs::enqueue_in_tx(&tx, job.clone())?;
        }

        let result = CommitResult {
            capture: committed,
            original_text_revision,
        };
        store_receipt(
            &tx,
            operation_id,
            "commit",
            &fingerprint,
            &serde_json::to_string(&result)?,
        )?;
        insert_event(&tx, EventType::CaptureChanged, &capture.id, new_revision)?;
        tx.commit()?;
        Ok(result)
    }

    /// 修改原始文字：创建新的 SourceRevision，旧版本保留。
    pub fn revise_text(
        &mut self,
        source_id: &str,
        text: &str,
        expected_revision: i64,
        operation_id: &str,
    ) -> Result<SourceRevision> {
        let fingerprint = support::fingerprint(&[
            "revise_text",
            source_id,
            &expected_revision.to_string(),
            text,
        ]);
        if let Some(json) = self.receipt("revise_text", &fingerprint, operation_id)? {
            let revision: SourceRevision = serde_json::from_str(&json)?;
            // 幂等重试的自愈：修订上次已经写好了，但派生内容可能没写成（提取那一步失败）。
            // 不补的话，这条来源的正文会永远搜不到——而「改完就能搜到新正文」是这一片的
            // 首要保证。
            self.ensure_extracted(&revision.revision_id)?;
            return Ok(revision);
        }

        let (capture_id, parent_revision_id) = load_source_item(&self.conn, source_id)?;
        let capture = load_capture(&self.conn, &capture_id)?;
        // 乐观锁以记录的 revision 为准：记录是聚合根，一次修订同时推进它。
        ensure_revision(&capture, expected_revision)?;
        if capture.state == CaptureState::Draft {
            return Err(CoreError::InvalidState {
                entity: "记录",
                id: capture.id.clone(),
                state: capture.state.wire().to_owned(),
            });
        }

        // 记录自己的文字（`captures.draft_text`）与「这一篇的上一版」可能是**同一段文字
        // 的两种存法**：提交时会把 `draft_text` 复制成一条 text 修订。改正文如果只改
        // 修订、不动 `draft_text`，记录文字那一路（B3d 已索引）就仍然能搜到**旧正文**，
        // 而决策是「用户在搜索里直接看不到旧正文」。
        //
        // 只在两者确实相同时也是同一段文字时才同步：不同就说明这条记录的文字另有出处
        // （比如记录文字后来又编辑过），不能替它下结论。
        let sync_draft = match parent_revision_id.as_deref() {
            Some(parent) => {
                load_revision_text(&self.conn, parent)?.as_deref() == Some(capture.draft_text.as_str())
            }
            None => false,
        };

        let now = support::now();
        let revision = SourceRevision {
            revision_id: support::new_id("rev"),
            source_id: source_id.to_owned(),
            parent_revision_id,
            text: Some(text.to_owned()),
            asset_id: None,
            author_type: AuthorType::User,
            occurred_at: now,
        };
        let new_revision = capture.revision + 1;

        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO source_revisions (revision_id, source_id, parent_revision_id, text, \
             asset_id, author_type, occurred_at) VALUES (?1, ?2, ?3, ?4, NULL, ?5, ?6)",
            params![
                revision.revision_id,
                revision.source_id,
                revision.parent_revision_id,
                revision.text,
                revision.author_type.wire(),
                support::to_iso(now),
            ],
        )?;
        tx.execute(
            "UPDATE source_items SET current_revision_id = ?1 WHERE source_id = ?2",
            params![revision.revision_id, source_id],
        )?;
        tx.execute(
            "UPDATE captures SET revision = ?1, updated_at = ?2 WHERE id = ?3",
            params![new_revision, support::to_iso(now), capture.id],
        )?;
        if sync_draft {
            tx.execute(
                "UPDATE captures SET draft_text = ?1 WHERE id = ?2",
                params![text, capture.id],
            )?;
            // 同事务里刷新记录文字的索引（它会推进索引代次，进行中的会话因此失效）。
            search::index_capture(
                &tx,
                search::CaptureInput {
                    capture_id: &capture.id,
                    text,
                },
            )?;
        }
        store_receipt(
            &tx,
            operation_id,
            "revise_text",
            &fingerprint,
            &serde_json::to_string(&revision)?,
        )?;
        insert_event(&tx, EventType::CaptureChanged, &capture.id, new_revision)?;
        tx.commit()?;

        // 改完立刻产出派生内容。否则在新修订被提取之前，这条来源的正文在默认搜索里会
        // **完全消失**（旧修订已不是当前修订，新修订还没有派生内容）——改正文是高频操作，
        // 留这个窗口不行。纯文本提取不碰任何文件（正文就在库里），代价很小。
        //
        // 空白正文不试提取：那是「这段话被删空了」的正常状态，不是失败——提取器对空正文
        // 仍然报错（那是有意的，见 `blank_text_revision_is_still_an_error`），但那该由
        // 显式调用 `sources.extract` 的人看到，不该让 `revise_text` 变成「存了却报错」。
        //
        // 非空白正文的提取失败不静默吞掉：修订已经保存，错误如实报给调用方；他拿同一条
        // operationId 重试会走上面的自愈路径，或者显式调 `sources.extract`。
        if !text.trim().is_empty() {
            crate::extractors::extract_source_revision(self, &revision.revision_id)?;
        }
        Ok(revision)
    }

    /// 修订已经有派生内容就什么都不做；没有就提取一次（幂等重试的自愈用）。
    fn ensure_extracted(&mut self, source_revision_id: &str) -> Result<()> {
        if crate::extractors::has_content(&self.conn, source_revision_id)? {
            return Ok(());
        }
        crate::extractors::extract_source_revision(self, source_revision_id)?;
        Ok(())
    }

    // ------------------------------------------------------------ 读取

    pub fn get_capture(&self, capture_id: &str) -> Result<Capture> {
        load_capture(&self.conn, capture_id)
    }

    /// 分页读取。按 `occurred_at DESC, id DESC` 确定性排序，cursor 对调用方不透明。
    pub fn list_captures(
        &self,
        day_key: Option<&str>,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<CapturePage> {
        let (cursor_at, cursor_id) = match cursor {
            Some(raw) => {
                let (at, id) = support::split_cursor(raw)?;
                (Some(support::to_iso(at)), Some(id))
            }
            None => (None, None),
        };
        let page_size = limit.clamp(1, 100);

        let sql = format!(
            "SELECT {CAPTURE_COLUMNS} FROM captures \
             WHERE (?1 IS NULL OR day_key = ?1) \
               AND (?2 IS NULL OR occurred_at < ?2 OR (occurred_at = ?2 AND id < ?3)) \
             ORDER BY occurred_at DESC, id DESC LIMIT ?4"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt
            .query_map(
                params![day_key, cursor_at, cursor_id, (page_size + 1) as i64],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, String>(6)?,
                        row.get::<_, i64>(7)?,
                        row.get::<_, String>(8)?,
                        row.get::<_, String>(9)?,
                    ))
                },
            )?
            .collect::<rusqlite::Result<Vec<RawCapture>>>()?;

        let mut captures = Vec::with_capacity(rows.len().min(page_size));
        for raw in rows {
            captures.push(capture_from_raw(&self.conn, raw)?);
        }
        // 多取一条用来判断还有没有下一页。
        let next_cursor = if captures.len() > page_size {
            captures.truncate(page_size);
            captures
                .last()
                .map(|capture| support::join_cursor(capture.occurred_at, &capture.id))
        } else {
            None
        };

        Ok(CapturePage {
            captures,
            next_cursor,
        })
    }

    // ------------------------------------------------------------ 录音

    /// 申请录音票据与限域暂存目录，契约第 4.2 节 `recordings.prepare`。
    pub fn prepare_recording(&mut self, capture_id: &str, operation_id: &str) -> Result<RecordingTicket> {
        recordings::prepare(self, capture_id, operation_id)
    }

    /// 登记一个已封闭片段。重复登记同一序号返回原回执，内容不一致报冲突。
    pub fn register_segment(
        &mut self,
        recording_id: &str,
        segment_index: i64,
        segment: SegmentManifest,
    ) -> Result<SegmentReceipt> {
        recordings::register_segment(self, recording_id, segment_index, segment)
    }

    /// 上报原生录音状态。后端不虚构麦克风状态。
    pub fn update_recording_state(&mut self, status: NativeRecordingStatus) -> Result<RecordingSession> {
        recordings::update_state(self, status)
    }

    /// 最终化：拼出逻辑音频、建资产，并在同一事务里排一个转写任务。
    pub fn finalize_recording(
        &mut self,
        recording_id: &str,
        last_segment_index: i64,
        end_reason: &str,
    ) -> Result<RecordingFinalizeResult> {
        recordings::finalize(self, recording_id, last_segment_index, end_reason)
    }

    /// 恢复：不假设最后一段完好，缺口如实报告。
    pub fn recover_recording(&self, recording_id: Option<&str>) -> Result<RecordingRecovery> {
        recordings::recover(self, recording_id)
    }

    pub fn recording_session(&self, recording_id: &str) -> Result<RecordingSession> {
        recordings::session(self, recording_id)
    }

    // ------------------------------------------------------------ 任务队列

    pub fn enqueue_job(&mut self, job: NewJob) -> Result<Job> {
        jobs::enqueue(self, job)
    }

    pub fn get_job(&self, job_id: &str) -> Result<Job> {
        jobs::get(self, job_id)
    }

    /// 按状态筛选列出任务，按优先级与创建时间排序。
    pub fn list_jobs(&self, states: Option<&[JobState]>, limit: usize) -> Result<Vec<Job>> {
        jobs::list(self, states, limit)
    }

    /// 按状态精确计数。**不要用 `list_jobs` 的长度当计数**：它为分页把
    /// limit 夹在 200 以内，任务多的时候会少报。
    pub fn count_jobs(&self, states: Option<&[JobState]>) -> Result<i64> {
        jobs::count(self, states)
    }

    /// 领取下一个到期任务：优先级高的先跑。执行走 `run_due_jobs`。
    pub fn claim_next_due_job(&mut self, now: DateTime<Utc>) -> Result<Option<Job>> {
        jobs::claim_next_due(self, now)
    }

    /// 跑到期任务：领取 → 按 `kind` 执行 → 落结果（成功 / 退避重试 / 已放弃）。
    ///
    /// - `now` 是本次运行的开始时刻，也是「补做」判定的分界；
    /// - `budget_ms` 是调用方（平台层）给的**时间预算**，毫秒，`0` 表示一个都不跑。
    ///   预算**只在任务之间判断**：已经领取、正在跑的那条任务不打断，也不能打断
    ///   （核心不做抢占）。所以一条任务本身可以比预算还长——预算是「这一轮还能不能
    ///   再开一条」，不是「这一轮最多跑多久」；
    /// - `max_jobs` 是防御性的**条数上限**：预算挡长尾，它挡「队列很长 + 多条短任务」；
    /// - `trigger` 是唤醒来源（startup / foreground / wake / manual …），核心只如实
    ///   记进摘要，不用它改变行为；
    /// - 被预算或条数截住的到期任务留在队列里，数量如实报进
    ///   `JobRunSummary::due_backlog`；
    /// - 每一轮**每次领取前**先重新评估在等的任务（`waiting_dependency`），并在**一轮
    ///   收尾时**再评估一次：前置已满足的回到 `queued` 并在**同一轮**被拉起执行，
    ///   仍未满足的留在等待态、**不消耗重试额度**；前置不会再有执行者满足的进终态
    ///   `failed` + 明确错误码（不挂死），数量报在
    ///   `JobRunSummary::dependency_failed`。所以「导入 → 提取 → 索引」这样的链在
    ///   **一次**调用里就能走完，不需要平台层再叫一次；一轮结束后若还留着等待态，
    ///   摘要给出的 `next_wakeup` 必定非空（等在 `queued`/`retry_wait` 的前置上）；
    /// - 运行开始会先回收上一次没跑完（`running`）的任务（进程被杀/断电时留下的），
    ///   否则它们会被永远跳过；
    /// - 没有执行者的 `kind` 不会静默成功：进「已放弃」+ `no_handler`。
    ///
    /// 口径与不谎报的理由见 `docs/architecture/m4-任务调度与补做.md`。
    pub fn run_due_jobs(
        &mut self,
        now: DateTime<Utc>,
        budget_ms: u64,
        trigger: &str,
        max_jobs: usize,
    ) -> Result<JobRunSummary> {
        jobs::run_due(self, now, budget_ms, trigger, max_jobs)
    }

    pub fn complete_job(&mut self, job_id: &str) -> Result<Job> {
        jobs::complete(self, job_id)
    }

    /// 任务失败：还有额度就退避重试，用完了进 failed（「已放弃」）。
    pub fn fail_job(
        &mut self,
        job_id: &str,
        error_code: &str,
        message: &str,
        now: DateTime<Utc>,
    ) -> Result<Job> {
        jobs::fail(self, job_id, error_code, message, now)
    }

    /// 人工重试：把「已放弃」（failed）的任务清零重来（`attempt_count = 0`、
    /// `next_attempt_at` 清空、错误码清掉），并发出 `job.changed`。
    ///
    /// `job_attempts` 里的历史不删：编号跨人工重试累计，才回答得了
    /// 「为什么重试了五次」。
    pub fn retry_job(&mut self, job_id: &str) -> Result<Job> {
        jobs::retry(self, job_id)
    }

    /// 带 operationId 的幂等重试（契约 `jobs.retry`）。语义与 `retry_job` 相同，
    /// 只是同一个 operationId 重复调用返回第一次的结果。
    pub fn retry_job_idempotent(&mut self, job_id: &str, operation_id: &str) -> Result<Job> {
        jobs::retry_idempotent(self, job_id, operation_id)
    }

    /// 取消任务。取消不删除任何原件。
    pub fn cancel_job(&mut self, job_id: &str, operation_id: &str) -> Result<Job> {
        jobs::cancel(self, job_id, operation_id)
    }

    /// 建议的下次唤醒时刻；没有待办时为 None。
    pub fn next_wakeup(&self) -> Result<Option<DateTime<Utc>>> {
        jobs::next_wakeup(self)
    }

    /// 任务面板状态：各状态计数、到期积压、下次唤醒与最近错误。
    ///
    /// 最近错误**只带 kind + 错误码 + 时间**（外加不透明的任务 ID 供人工重试定位），
    /// 不带任何用户内容。
    pub fn job_status(&self) -> Result<JobStatus> {
        jobs::status(self)
    }

    /// 记录一个需处理的问题已展示过，避免每次重开重复通知。
    pub fn acknowledge_attention(&mut self, attention_key: &str) -> Result<()> {
        jobs::acknowledge_attention(self, attention_key)
    }

    pub fn is_attention_acknowledged(&self, attention_key: &str) -> Result<bool> {
        jobs::is_attention_acknowledged(self, attention_key)
    }

    // ------------------------------------------------------------ 提取与定位

    /// 对某个来源修订跑一次提取，结果作为可重建的派生内容存下来。
    ///
    /// 提取失败也会落一条记录（status=failed、coverage=unavailable、带错误码），
    /// 这样界面上能看到「解析失败」而不是「没有相关内容」。
    pub fn extract_source(&mut self, source_revision_id: &str) -> Result<ExtractedContent> {
        extractors::extract_source_revision(self, source_revision_id)
    }

    /// 读取某个来源当前版本的派生内容；没提取过时返回 None。
    pub fn extracted_content(&self, source_id: &str) -> Result<Option<ExtractedContent>> {
        extractors::extracted_content(self, source_id)
    }

    /// 取某个来源**当前修订的父修订**（上一版正文），供界面「看原正文」用。
    ///
    /// 只给两版正文，**不算 diff**：怎么呈现差异是前端的事（见
    /// `docs/architecture/m2-修订与检索可见性.md`）。
    ///
    /// 三种情况都返回 `None`，它们对调用方是同一件事——「这篇没有上一版」：
    /// - 来源不存在，或它的 `current_revision_id` 是空的；
    /// - 当前修订就是第一版（`parent_revision_id` 为空）；
    /// - 父修订 id 悬空（数据坏了）。这时给不出正文，如实当「没有」，
    ///   比编一个空修订出来好。
    ///
    /// 要看清楚父修订可能是**原件型**修订（导入的文件、录音）：它的 `text` 是空的、
    /// `asset_id` 有值，正文要从它的派生内容里取。契约侧怎么把这一层包给前端，
    /// 留给 #55。
    pub fn previous_source_revision(&self, source_id: &str) -> Result<Option<SourceRevision>> {
        previous_source_revision(&self.conn, source_id)
    }

    /// 把 sourceRef + locator 解析成前端可打开的原件与可用性。
    pub fn locate_source(&self, source_ref: &str, locator: SourceLocator) -> Result<SourceLocation> {
        extractors::locate(self, source_ref, locator)
    }

    // ------------------------------------------------------------ 关键词索引

    /// 索引覆盖状态，契约第 4.4 节 `indexes.status`。
    pub fn index_status(&self, source_scope: Option<&[String]>) -> Result<IndexStatus> {
        search::status(self, source_scope)
    }

    /// 重建关键词索引；`source_scope` 为空表示整库。
    ///
    /// 返回重建的文档数：派生内容片段 + 用户自己写的记录文字（`captures.draft_text`）。
    ///
    /// 这是**同步**入口，给覆盖状态校准与测试用。产品路径走
    /// `schedule_index_rebuild`（契约 `indexes.rebuild`）：它只排任务，不阻塞界面。
    pub fn rebuild_keyword_index(&mut self, source_scope: Option<&[String]>) -> Result<i64> {
        search::rebuild(self, source_scope)
    }

    /// 排一条关键词索引重建任务，契约第 4.4 节 `indexes.rebuild`。
    ///
    /// **异步**：只排任务并返回它（调用方拿 `job.id`），真正重建由 `index` 执行者在
    /// `run_due_jobs` 里跑。`scope` 的编码（v1）是：
    ///
    /// - `all`：整库（与 `rebuild_keyword_index(None)` 同义，含记录文字那一路）；
    /// - `sources:<id>[,<id>]*`：只重建这些来源；id 只允许 `[A-Za-z0-9_]`，`,` 是
    ///   分隔符，不定义转义。
    ///
    /// 顺序对重建没有语义，解析时排序成规范形；空列表、空记号、重复 id、越界字符
    /// 报 `invalid_argument`；指定了不存在的来源报 `not_found`。
    ///
    /// 幂等：同一个 `(scope, 原因, operationId)` 重复调用返回同一条任务（按 id 读
    /// 当前状态）；同一个 operationId 换了 scope 或原因报 `idempotency_conflict`。
    ///
    /// 口径与 `scope` 规范见契约第 4.4 节与 `docs/architecture/m4-任务调度与补做.md`。
    pub fn schedule_index_rebuild(
        &mut self,
        scope: &str,
        reason: &str,
        operation_id: &str,
    ) -> Result<Job> {
        index_rebuild::schedule(self, scope, reason, operation_id)
    }

    // ------------------------------------------------------------ 文本块

    /// 重建文本块；`source_scope` 为空表示整个资料库。
    ///
    /// 块是**语义检索的载体**，不是用户看到的东西：用户看到的仍然以「篇」为单位。
    /// 它跟关键词索引是两条独立的路：调用它不影响 `coverage` / `keyword_index_ready`，
    /// 反过来也一样。真正的向量由 `build_semantic_index` 算。
    ///
    /// 返回写进库的块数（**不是篇数**：一块可能由同一天的几篇拼成）。范围语义与
    /// `rebuild_keyword_index` 一致：`Some([])` 是「什么都不看」，得到 0 块。
    ///
    /// 重打包的单位是日子而不是记录，理由见 `chunks::rebuild` 的说明：分块规则按
    /// `day_key` 合块，只重算范围内的那几篇会让同一份内容有两种块边界。
    ///
    /// 整库重建是重活，产品路径上它应当是可取消的任务；这一片先提供同步入口
    /// （任务化见 B5 / issue #32 的待办）。
    pub fn rebuild_text_chunks(&mut self, source_scope: Option<&[String]>) -> Result<i64> {
        chunks::rebuild(self, source_scope)
    }

    /// 建语义索引：重建块 → 开新代次 → 复用/补算向量 → 原子切换。
    ///
    /// 具体顺序与理由见 `semantic` 模块的说明与
    /// `docs/architecture/m2-向量索引与换代.md`。三个要点：
    ///
    /// - **模型缺失/加载失败时返回诚实的错误，并且不改任何状态**（连块都不重建）。
    ///   想确认「语义这一路现在能不能跑」，看 `index_status().semantic_index_ready`
    ///   与 `reasons`；
    /// - **没变的块不重复算**：块 id 是内容的函数，重建保住 id，于是新代次里口径一致
    ///   的向量从旧代次整批复制，只有真的变了/新来的块才走模型；
    /// - **换代期间关键词检索照旧**：块与向量不碰关键词那几张表。
    ///
    /// `source_scope` 与 `rebuild_text_chunks` 同义；`Some([])` 是「什么都不看」，
    /// 直接返回、什么都不做。**注意**：代次是全局的，所以这个入口总是把**块表里所有
    /// 块**在新代次里补齐（范围只决定重算了哪些块），否则切过去会让范围外的块失去
    /// 生效向量。代价见文档「没有做到的」。
    pub fn build_semantic_index(
        &mut self,
        source_scope: Option<&[String]>,
    ) -> Result<SemanticBuildReport> {
        semantic::build(self, source_scope)
    }

    /// 量测用：语义检索按篇折叠后的 `(篇标识, 余弦分数)`，分数从高到低。
    ///
    /// 产品路径不读它（`SearchHit` 上没有 score 字段）；它给质量集量测讨论
    /// 「语义会不会对没有答案的查询乱报」「要不要设分数阈值」用，与 `search.start`
    /// 的语义路共用同一段候选与折叠逻辑。
    pub fn semantic_ranking(
        &mut self,
        query: &str,
        filters: &SearchFilters,
    ) -> Result<Vec<(String, f32)>> {
        semantic::ranking(self, query, filters)
    }

    /// 关键词检索候选：命中**派生片段**的 ID，按索引写入顺序。
    ///
    /// 记录文字（`captures.draft_text`）不在这里：片段 ID 这个返回值表达不了它。
    /// 要连记录文字一起搜，走 `start_search`。
    pub fn search_candidates(&self, query: &str, limit: usize) -> Result<Vec<String>> {
        search::candidates(self, query, limit)
    }

    /// 命中数量。测试与实测脚本用它核对片段索引的召回是否完整。
    pub fn count_search_matches(&self, query: &str) -> Result<i64> {
        search::count_matches(self, query)
    }

    // ------------------------------------------------------------ 检索会话

    /// 发起一次检索，契约第 4.4 节 `search.start`。
    pub fn start_search(
        &mut self,
        request: SearchRequest,
        query_revision: i64,
    ) -> Result<SearchSnapshot> {
        search_session::start(self, request, query_revision)
    }

    /// 翻页，契约第 4.4 节 `search.nextPage`。游标不属于这个会话时报 `cursor_expired`。
    pub fn search_next_page(&mut self, session_id: &str, cursor: Option<&str>) -> Result<SearchSnapshot> {
        search_session::next_page(self, session_id, cursor)
    }

    /// 读当前快照，契约第 4.4 节 `search.snapshot`。会话不存在或索引变了报 `search_expired`。
    pub fn search_snapshot(&self, session_id: &str) -> Result<SearchSnapshot> {
        search_session::snapshot(self, session_id)
    }

    /// 取消后续处理，契约第 4.4 节 `search.cancel`。不删除任何原件。
    pub fn cancel_search(&mut self, session_id: &str) -> Result<SearchSnapshot> {
        search_session::cancel(self, session_id)
    }

    /// 把混合（`mode = hybrid`）会话从关键词阶段升级到融合阶段（#69）。
    ///
    /// `start_search(mode = hybrid)` 先只算关键词那一路并报 `keyword_ready`（首屏不等
    /// 模型）；这里才真正做 RRF 融合，返回**同一会话**的 `hybrid_ready` 快照。升级后
    /// 翻页给融合结果，翻完报 `done`。**幂等**：重复调用返回当前快照，不重复算。
    ///
    /// 语义拿不出可融合的证据时**不声称** `hybrid_ready`：没配模型 / 加载失败 /
    /// 没有生效代次 / 没有块（`Unavailable`），或语义可用但候选全被证据门槛
    /// `HYBRID_SEMANTIC_MIN_SCORE` 滤掉、查询是空的（`NoEvidence`），都如实返回
    /// **关键词那份快照**、阶段仍是 `keyword_ready`，并在 `warnings` 里说明。已取消的
    /// 会话返回 `cancelled` 快照；索引变了报 `search_expired`；非混合会话报
    /// `invalid_state`。
    pub fn upgrade_search_to_hybrid(&mut self, session_id: &str) -> Result<SearchSnapshot> {
        search_session::upgrade(self, session_id)
    }

    // ------------------------------------------------------------ 原件与导入

    /// 申请导入暂存位置，契约第 4.2 节 `imports.prepare`。
    pub fn prepare_import(&mut self, request: ImportRequest<'_>) -> Result<ImportTicket> {
        assets::prepare(self, request)
    }

    /// 声明复制完成，契约第 4.2 节 `imports.finish`。
    ///
    /// 核心会自己重算哈希；对不上就报 `integrity_failed` 并保留暂存文件。
    pub fn finish_import(
        &mut self,
        import_id: &str,
        staging_ticket: &str,
        manifest: ImportManifest,
    ) -> Result<ImportStatus> {
        assets::finish(self, import_id, staging_ticket, manifest)
    }

    pub fn import_status(&self, import_id: &str) -> Result<ImportStatus> {
        assets::status(self, import_id)
    }

    /// 取消导入，不影响其他已导入的材料。
    pub fn cancel_import(&mut self, import_id: &str) -> Result<()> {
        assets::cancel(self, import_id)
    }

    /// 打开只读租约，契约第 4.2 节 `assets.open`。
    pub fn open_asset(&mut self, asset_id: &str, usage: &str) -> Result<AssetLease> {
        assets::open_asset(self, asset_id, usage)
    }

    pub fn release_asset(&mut self, lease_id: &str) -> Result<()> {
        assets::release_asset(self, lease_id)
    }

    /// 还没走完的导入数量（未完成与可恢复都算）。
    pub fn count_imports_in_flight(&self) -> Result<i64> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM import_sessions WHERE state IN \
             ('prepared', 'copying', 'verifying', 'recoverable')",
            [],
            |row| row.get(0),
        )?)
    }

    /// 还没收尾的录音数量。
    pub fn count_open_recordings(&self) -> Result<i64> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM recording_sessions WHERE state IN \
             ('preparing', 'recording', 'paused', 'stopping', 'interrupted', 'recoverable')",
            [],
            |row| row.get(0),
        )?)
    }

    /// 记录数量（不含回收站）。
    ///
    /// 契约 `CoreSnapshot.captureCount` 用它；草稿也算——用户看到的「记录」里
    /// 草稿本来就在。回收站的不算，和默认检索口径一致。
    pub fn count_captures(&self) -> Result<i64> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM captures WHERE state <> 'trashed'",
            [],
            |row| row.get(0),
        )?)
    }

    /// 事件表里最大的序号；一条事件都没有时是 0。
    ///
    /// 契约 `CoreSnapshot.lastEventSequence` 用它：调用方据此知道从哪个游标续读。
    /// 不要用「拉全部事件再取最后一条」来算——那会把整张事件表读进内存。
    pub fn last_event_sequence(&self) -> Result<i64> {
        Ok(self.conn.query_row(
            "SELECT COALESCE(MAX(sequence), 0) FROM domain_events",
            [],
            |row| row.get(0),
        )?)
    }

    /// 资产数量与内容存储占用的字节数。
    pub fn asset_stats(&self) -> Result<(i64, i64)> {
        assets::asset_stats(self)
    }

    /// 当前未释放的租约数量。租约是内存态，进程重启后归零。
    pub fn active_lease_count(&self) -> usize {
        assets::active_leases(&self.leases)
    }

    /// 当前未释放的租约句柄（租约 id → 文件路径），按 id 排序。
    pub fn lease_handles(&self) -> Vec<(String, String)> {
        assets::lease_handles(self)
    }

    /// 丢弃过期租约，返回丢弃的数量。
    pub fn reap_expired_leases(&mut self) -> usize {
        assets::reap_expired_leases(self)
    }

    /// 从序号之后读取事件，契约第 6 节。
    pub fn events_since(&self, from_sequence: i64) -> Result<Vec<DomainEvent>> {
        let mut stmt = self.conn.prepare(
            "SELECT sequence, event_id, type, entity_id, revision, emitted_at \
             FROM domain_events WHERE sequence > ?1 ORDER BY sequence",
        )?;
        let rows = stmt
            .query_map(params![from_sequence], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, String>(5)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        let mut events = Vec::with_capacity(rows.len());
        for (sequence, event_id, event_type, entity_id, revision, emitted_at) in rows {
            events.push(DomainEvent {
                event_id,
                sequence,
                event_type: EventType::from_wire(&event_type).ok_or_else(|| {
                    CoreError::CorruptedData {
                        message: format!("未知事件类型：{event_type}"),
                    }
                })?,
                entity_id,
                revision,
                emitted_at: support::parse_iso(&emitted_at)?,
            });
        }
        Ok(events)
    }

    // ------------------------------------------------------------ 远端服务（B4a）

    /// 密钥托管（只给测试与受控调用路径用，**不要**把它交给上层）。
    pub fn secrets(&self) -> &dyn provider::secret::SecretStore {
        self.secrets.as_ref()
    }

    /// 出站请求的唯一出口。
    pub fn transport(&self) -> &dyn provider::transport::Transport {
        self.transport.as_ref()
    }

    /// 仅供测试/量测：换一个传输层（回放、记账、panic 替身）。
    ///
    /// 产品代码不调它：文件库默认就是真实 HTTP，内存库默认是「一调用就 panic」。
    #[doc(hidden)]
    pub fn set_transport_for_test(&mut self, transport: std::sync::Arc<dyn provider::transport::Transport>) {
        self.transport = transport;
    }

    /// 仅供测试：换一个密钥存储。
    #[doc(hidden)]
    pub fn set_secret_store_for_test(&mut self, secrets: Box<dyn provider::secret::SecretStore>) {
        self.secrets = secrets;
    }

    /// 列出脱敏后的服务配置，契约第 4.4 节 `providers.list`。按创建顺序，稳定。
    pub fn list_providers(&self) -> Result<Vec<Provider>> {
        provider::list(self)
    }

    /// 保存服务配置，契约第 4.4 节 `providers.save`。
    ///
    /// `secret` 是**明文密钥**，只在这一刻经过内存：存进 `SecretStore` 后立刻换成
    /// 引用，数据库与返回值里都只有引用。传 `None` 表示不改密钥（沿用配置里的引用，
    /// 引用为空就是清掉凭据）。
    ///
    /// 协议/端点/模型变了会让上一次的验证结论作废（那是对旧配置的验证）。
    ///
    /// 传进来的 `test_status` **是只读结论，会被忽略**：它由连接测试写，
    /// 不能让调用方「保存成一个已验证的服务」。
    pub fn save_provider(&mut self, config: &Provider, secret: Option<&str>) -> Result<Provider> {
        provider::save(self, config, secret)
    }

    /// 移除服务配置，契约第 4.4 节 `providers.remove`。**不删除任何日记**，
    /// 也不删除已经生成的回应（它记着当时的来源，是历史事实）。
    pub fn remove_provider(&mut self, provider_id: &str) -> Result<()> {
        provider::remove(self, provider_id)
    }

    /// 连接测试，契约第 4.4 节 `providers.test`。
    ///
    /// **只有显式调用才发网络请求**；`scope` 是用户授权的测试范围，报告里逐项给
    /// 结论并说明是否真的发生了模型调用。详见 `provider::test` 的文档。
    pub fn test_provider(
        &mut self,
        provider_id: &str,
        scope: &[ProviderCapability],
    ) -> Result<ProviderTestReport> {
        provider::test(self, provider_id, scope)
    }

    /// 能力闸门：现在能不能用某个能力、为什么、下一步做什么。
    ///
    /// 这是「不排注定失败的任务」的唯一判据，排任务的入口都应该先问它。
    pub fn provider_capability_gate(
        &self,
        capability: ProviderCapability,
    ) -> Result<CapabilityGate> {
        provider::capability_gate_for(self, capability, None)
    }

    /// 主动请求一次回应，契约第 4.3 节 `responses.request`。
    ///
    /// **只有主动调用才排任务**：这个方法返回后，作业在队列里等驱动；没有调用就
    /// 什么都没有。能力不满足时直接拒绝（不排一条注定失败的任务）。
    pub fn request_response(
        &mut self,
        capture_ids: &[String],
        request_text: &str,
        provider_id: Option<&str>,
    ) -> Result<ResponseTicket> {
        provider::request_response(self, capture_ids, request_text, provider_id)
    }

    /// 读取一条回应，契约第 4.3 节 `responses.get`。
    pub fn get_response(&self, response_id: &str) -> Result<AiResponse> {
        provider::get_response(self, response_id)
    }

    // ------------------------------------------------------------ 幂等回执

    fn receipt(
        &self,
        kind: &str,
        fingerprint: &str,
        operation_id: &str,
    ) -> Result<Option<String>> {
        let row: Option<(String, String, String)> = self
            .conn
            .query_row(
                "SELECT kind, request_fingerprint, result_json FROM operation_receipts \
                 WHERE operation_id = ?1",
                params![operation_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;

        match row {
            None => Ok(None),
            Some((stored_kind, stored_fingerprint, result_json)) => {
                if stored_kind != kind || stored_fingerprint != fingerprint {
                    return Err(CoreError::IdempotencyConflict {
                        operation_id: operation_id.to_owned(),
                    });
                }
                Ok(Some(result_json))
            }
        }
    }
}

// ------------------------------------------------------------------ 内部工具

fn ensure_revision(capture: &Capture, expected_revision: i64) -> Result<()> {
    if capture.revision != expected_revision {
        return Err(CoreError::RevisionConflict {
            expected: expected_revision,
            actual: capture.revision,
        });
    }
    Ok(())
}

fn store_receipt(
    tx: &Transaction<'_>,
    operation_id: &str,
    kind: &str,
    fingerprint: &str,
    result_json: &str,
) -> Result<()> {
    tx.execute(
        "INSERT INTO operation_receipts (operation_id, kind, request_fingerprint, result_json, \
         created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            operation_id,
            kind,
            fingerprint,
            result_json,
            support::to_iso(support::now())
        ],
    )?;
    Ok(())
}

fn insert_event(
    tx: &Transaction<'_>,
    event_type: EventType,
    entity_id: &str,
    revision: i64,
) -> Result<()> {
    tx.execute(
        "INSERT INTO domain_events (event_id, type, entity_id, revision, emitted_at) \
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            support::new_id("evt"),
            event_type.wire(),
            entity_id,
            revision,
            support::to_iso(support::now())
        ],
    )?;
    Ok(())
}

/// `previous_source_revision` 查到的一行，解析前先按位置取出来。
type RawSourceRevision = (
    String,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    String,
    String,
);

/// 读「当前修订的父修订」，一次查询取完。
///
/// 两次 `JOIN`（而不是先查 current、再查 parent 的两条语句）让所有「没有上一版」的
/// 情况自然退化成「查不到行」：来源不在、`current_revision_id` 为空、
/// `parent_revision_id` 为空、父修订悬空——一个 `None` 全包了，没有分支要维护。
fn previous_source_revision(conn: &Connection, source_id: &str) -> Result<Option<SourceRevision>> {
    let row: Option<RawSourceRevision> = conn
        .query_row(
            "SELECT p.revision_id, p.source_id, p.parent_revision_id, p.text, p.asset_id, \
             p.author_type, p.occurred_at \
             FROM source_items i \
             JOIN source_revisions cur ON cur.revision_id = i.current_revision_id \
             JOIN source_revisions p ON p.revision_id = cur.parent_revision_id \
             WHERE i.source_id = ?1",
            params![source_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                ))
            },
        )
        .optional()?;
    let Some(row) = row else {
        return Ok(None);
    };
    Ok(Some(SourceRevision {
        revision_id: row.0,
        source_id: row.1,
        parent_revision_id: row.2,
        text: row.3,
        asset_id: row.4,
        author_type: AuthorType::from_wire(&row.5).ok_or_else(|| CoreError::CorruptedData {
            message: format!("未知作者类型：{}", row.5),
        })?,
        occurred_at: support::parse_iso(&row.6)?,
    }))
}

fn load_source_item(conn: &Connection, source_id: &str) -> Result<(String, Option<String>)> {
    conn.query_row(
        "SELECT capture_id, current_revision_id FROM source_items WHERE source_id = ?1",
        params![source_id],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
    )
    .optional()?
    .ok_or_else(|| CoreError::NotFound {
        entity: "来源",
        id: source_id.to_owned(),
    })
}

/// 读某个修订的正文（没有正文就是 `None`）。
///
/// 只用在「判断上一篇是不是同一段文字」这种比较上，不做对外读取接口。
fn load_revision_text(conn: &Connection, revision_id: &str) -> Result<Option<String>> {
    let text: Option<Option<String>> = conn
        .query_row(
            "SELECT text FROM source_revisions WHERE revision_id = ?1",
            params![revision_id],
            |row| row.get(0),
        )
        .optional()?;
    Ok(text.flatten())
}

fn load_capture(conn: &Connection, capture_id: &str) -> Result<Capture> {
    let sql = format!("SELECT {CAPTURE_COLUMNS} FROM captures WHERE id = ?1");
    let raw: Option<RawCapture> = conn
        .query_row(&sql, params![capture_id], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
                row.get(6)?,
                row.get(7)?,
                row.get(8)?,
                row.get(9)?,
            ))
        })
        .optional()?;

    match raw {
        Some(row) => capture_from_raw(conn, row),
        None => Err(CoreError::NotFound {
            entity: "记录",
            id: capture_id.to_owned(),
        }),
    }
}

fn capture_from_raw(conn: &Connection, raw: RawCapture) -> Result<Capture> {
    let (id, revision, state, occurred_at, created_at, updated_at, time_zone, offset, day_key, draft_text) = raw;
    let mut stmt =
        conn.prepare("SELECT source_id FROM source_items WHERE capture_id = ?1 ORDER BY position")?;
    let ordered_source_ids = stmt
        .query_map(params![id], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<String>>>()?;

    Ok(Capture {
        id,
        revision,
        state: CaptureState::from_wire(&state).ok_or_else(|| CoreError::CorruptedData {
            message: format!("未知记录状态：{state}"),
        })?,
        occurred_at: support::parse_iso(&occurred_at)?,
        created_at: support::parse_iso(&created_at)?,
        updated_at: support::parse_iso(&updated_at)?,
        time_zone,
        utc_offset_minutes: i32::try_from(offset).map_err(|_| CoreError::CorruptedData {
            message: format!("时区偏移超出范围：{offset}"),
        })?,
        day_key,
        ordered_source_ids,
        draft_text,
        // B1b/B2 接上提取与索引后才会出现非零值。
        processing_summary: ProcessingSummary::default(),
    })
}
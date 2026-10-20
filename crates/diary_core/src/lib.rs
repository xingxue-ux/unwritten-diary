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
mod backup;
mod chunker;
mod chunks;
/// 设备配置文件 `core-config.json`（与 `library.sqlite` 同目录）。
///
/// 公开是为了让 C ABI 与集成测试能读同一份类型（`RuntimeConfigReport` /
/// `ConfigOrigin`）；产品代码只通过 `Core::runtime_config()` 看它。
pub mod config_file;
/// 日记生成与版本（B4b）：冻结输入快照、候选/当前语义、防注入、两步走、去重。
///
/// 公开是为了让集成测试能直接断言提示词拼装（`SYSTEM_INSTRUCTIONS` /
/// `DIRECT_INPUT_CHAR_LIMIT`）——产品代码只走 `Core::generate_diary` 与队列。
pub mod diary;
mod error;
/// 本地向量模型接入（B3c-2）。公开是为了让测试与 `tools/probe` 能显式注入 stub 或
/// 指定模型包——这是给测试/量测的入口，不是让产品代码去直接碰 ORT。
pub mod embedding;
mod extractors;
mod index_rebuild;
mod jobs;
/// 数据对象。公开是为了让桥接层的 codegen 能扫描它们（见 flutter_rust_bridge.yaml）。
pub mod model;
/// 整理与自动化（B4e，issue #92）：整理模板、开关与触发边界、待处理判据、配额上限。
///
/// 公开是为了让集成测试能直接断言模板规则与判据（`render_rule` / `LIMITS` /
/// `candidates`）——产品代码只走 `Core::organize_*` 与驱动。
pub mod organize;
/// 插件 SDK 与宿主（B6，#88）：包格式、清单、权限、声明式模板与 Wasm 沙箱。
///
/// 公开是为了让桥接扫描 DTO、让测试直接断言权限目录与 ABI 导入表；产品代码只走
/// `Core::inspect_plugin_package` / `install_plugin` / `run_plugin` 这几个入口。
pub mod plugin;
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
/// 转写链（#91）：本机引擎、分段转写、时间戳对齐与质量集接缝。
///
/// 公开是为了让测试与 `tools/probe` 能注入 stub 引擎、能直接跑一条转写
/// （与 `provider::transport` 同一个理由：真引擎要模型，CI 里没有）。
pub mod transcription;
mod schema;
mod search;
mod search_session;
mod semantic;
mod semantic_index_job;
mod support;

pub use assets::ImportRequest;
pub use backup::{BackupFault, BackupGate};
pub use extractors::{
    builtin_extractors, ExtractionInput, ExtractionOutcome, Extractor, TEXT_DIRECT_EXTRACTOR_ID,
    TEXT_DIRECT_EXTRACTOR_VERSION,
};
pub use error::{CoreError, ErrorCode, Result};
pub use config_file::{
    ConfigFileReport, ConfigOrigin, CoreConfigFile, RuntimeConfigReport, SemanticConfigReport,
    TranscriptionConfigReport, CONFIG_FILE_NAME,
};
pub use provider::adapter::{
    adapter_for, builtin_adapter_ids, GenerateOutcome, GenerateRequest, ProtocolAdapter,
    CALL_TIMEOUT_MS, OPENAI_COMPATIBLE, PROBE_TIMEOUT_MS,
};
pub use provider::local_engine::{
    EngineConfig as AsrEngineConfig, EngineReadiness, LocalEngine, ModelKind as AsrModelKind,
    PcmOutcome, PcmRequest, BUILTIN_LOCAL_ASR_ID, SHERPA_ONNX,
};
pub use provider::secret::{FileSecretStore, MemorySecretStore, SecretStore};
pub use transcription::seam::{
    LocalTranscriptEngine, TranscriptEngine, TranscriptOutcome, TranscriptRequest,
};
pub use provider::transport::{
    CountingTransport, HttpRequest, HttpResponse, PanicTransport, ScriptedTransport, Transport,
    TransportError, UreqTransport,
};
pub use model::{
    AiResponse, Asset, AssetLease, AssetStorageState, AuthorType, BackupCounts, BackupInspection,
    BackupReport, Capture, CapturePage, CaptureState, CommitResult, Coverage, DiaryDay,
    DiaryGenerationReason, DiaryGenerationTicket, DiaryVersion, DiaryVersionOrigin,
    DiaryVersionPage, DomainEvent, DraftSaveResult, EventType, ExtractedContent, ExtractedSegment,
    GenerationInfo, ImportManifest, ImportOrigin, ImportState, ImportStatus, ImportTicket,
    IndexStatus, Job, JobErrorSummary, JobPage, JobPriority, JobProgress, JobRunSummary, JobState,
    JobStatus, LocatorType, MatchedBy, MaterialCounts, NativeRecordingStatus, NewJob,
    OrganizePendingDay, OrganizePlanSummary, OrganizeQuota, OrganizeRunInfo, OrganizeSettings,
    OrganizeSkippedDay, OrganizeStatus, OrganizeTemplate, OrganizeTemplateParam, ProcessingStatus,
    ProcessingSummary, PluginArtifact, PluginDerivedContent, PluginEntryKind, PluginInspection,
    PluginInstallation, PluginInvocation, PluginManifest, PluginPermission, PluginProviderFailure,
    PluginRunResult, PluginSourceRef, PluginStyleProfile, PluginTemplate, ProvenanceLink, Provider,
    ProviderCapability, ProviderTestReport, ProviderTestStatus, RecordingFinalizeResult,
    RecordingRecovery, RecordingSession, RecordingState, RecordingTicket, ResponseTicket,
    RestoreReport, SearchFilters, SearchHit,
    SearchMode, SearchPhase, SearchRequest, SearchSnapshot, SegmentManifest, SegmentReceipt,
    SourceItem, SourceKind, SourceLocation, SourceLocator, SourceRevision, TextRange,
};
pub use provider::CapabilityGate;
pub use diary::{
    DEFAULT_STYLE_SNAPSHOT_ID, DIRECT_INPUT_CHAR_LIMIT, MAX_INTERMEDIATE_CALLS,
    MERGE_PROMPT_VERSION, PROMPT_VERSION, SYSTEM_INSTRUCTIONS,
};
pub use organize::{
    DEFAULT_AUTO_ORGANIZE_TIME, DEFAULT_TEMPLATE_ID, FAILURE_WINDOW_HOURS, LIMITS, LOOKBACK_DAYS,
    MIN_FAILURE_SAMPLES, ORGANIZE_PROMPT_VERSION,
};

/// 仅供测试：某一天的整理计划时刻（本地 `HH:mm` 按当天偏移折成 UTC）。
#[doc(hidden)]
pub fn organize_due_at_for_test(
    day_key: &str,
    time: &str,
    utc_offset_minutes: i64,
) -> Result<DateTime<Utc>> {
    organize::due_at(day_key, time, utc_offset_minutes)
}
pub use chunker::{
    chunks_for, pack_pieces, Chunk, ChunkSpan, Piece, CHUNKER_VERSION, CHUNK_OVERLAP_CHARS,
    MAX_CHUNK_CHARS,
};
pub use embedding::{
    bytes_to_vector, config_from_env, load as load_embedder, resolve as resolve_embedding_config,
    text_hash_vector, vector_bytes, EmbedOptions, Embedder, Embedding, EmbeddingConfig,
    EmbeddingResolution, EnvEmbedding, OrtEmbedder, Pooling, TokenTruncation, VectorStorage,
    DEFAULT_POOLING, DEFAULT_QUERY_INSTRUCTION, DEFAULT_VECTOR_STORAGE, ENV_MODEL_DIR, ENV_ORT_DYLIB,
    ENV_ORT_DYLIB_COMPAT, EXPECTED_DIMS, MAX_BODY_TOKENS, MAX_SEQ_TOKENS, MODEL_ID,
    QUERY_INSTRUCTION,
};
pub use schema::SCHEMA_VERSION;
pub use search::TOKENIZER_VERSION;
pub use semantic::SemanticBuildReport;
#[doc(hidden)]
pub use semantic::{set_query_profiling, take_query_profile, SemanticQueryProfile};
pub use search_session::{HYBRID_FUSION_VERSION, HYBRID_SEMANTIC_MIN_SCORE};

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

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
    /// 资料库文件名（只用于备份清单里的展示字段，绝不参与路径拼接）。
    library_file_name: Option<String>,
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
    /// 新写入向量的存储编码。默认 `DEFAULT_VECTOR_STORAGE`；量测与测试可以临时改它
    /// （见 `set_vector_storage_for_test`）。它是口径三字段之一，换格式 = 整代重算。
    vector_storage: embedding::VectorStorage,
    /// 当前嵌入器（或产品模型）的维度。默认 `EXPECTED_DIMS`；注入/加载嵌入器后跟着它走。
    /// `index_status` 用它判断生效代次的 `dims` 是不是同一个向量空间（不加载模型）。
    current_dims: usize,
    /// 快速路径保留的候选块数上限（#90）。默认 `FAST_SCAN_DEPTH`；量测与测试可以
    /// 临时改它来扫「深度 vs 延迟」曲线（见 `set_semantic_scan_depth_for_test`）。
    semantic_scan_depth: usize,
/// 密钥托管。默认实现是资料库目录下权限受限的独立文件；测试与内存库用内存实现。
    /// **明文只在这一层**，`providers` 表里只有引用（见 `provider::secret`）。
    secrets: Box<dyn provider::secret::SecretStore>,
    /// 出站请求的唯一出口。默认：文件库用真实 HTTP（ureq + rustls），
    /// 内存库用「一旦被调用就 panic」的替身——内存库只用于测试，不该真的出网。
    transport: std::sync::Arc<dyn provider::transport::Transport>,
    /// 本机转写引擎。默认是真实现（运行时动态加载 sherpa-onnx 的 C API），
    /// 测试注入 stub——**与传输层同一个形状、同一个理由**：CI 里没有引擎库、
    /// 没有模型，而「转写链本身」（分段、恢复、幂等、降级）必须能在 CI 里跑完。
    asr_engine: std::sync::Arc<dyn provider::local_engine::LocalEngine>,
    /// 仅供测试/量测：钉死的引擎配置（模型目录、引擎库路径）。
    ///
    /// 为什么要有它：配置解析会读环境变量，而环境变量是**进程级**的——多个测试
    /// 并行跑时会互相踩（一个测试设的目录被另一个删掉）。钉死的配置只影响这个
    /// `Core`，测试因此可以各自用自己的「模型目录」，不需要抢环境变量。
    asr_config_for_test: Option<provider::local_engine::EngineConfig>,
    /// 仅供测试的备份故障注入点，默认不注入（见 `set_backup_fault_for_test`）。
    backup_fault: backup::BackupFault,
    /// 仅供测试的备份会合点，默认不设（见 `set_backup_gate_for_test`）。
    backup_gate: Option<Arc<backup::BackupGate>>,
    /// 资料库旁边的 `core-config.json`（四个可选路径）。`None` = 没有/不可用。
    ///
    /// 只在 `open` 时读一次：与 `DIARY_*` 环境变量同一语义（打开那一刻的快照）。
    /// 读不了/解析失败**不是错误**：`config_report.error` 如实记下原因，配置回落。
    config_file: Option<config_file::CoreConfigFile>,
    /// 配置文件本身的状态（路径、是否存在、解析错误、不认识的字段）。
    config_report: config_file::ConfigFileReport,
}

/// 契约 1.2 节的 API 版本。
///
/// 桥接的 `CoreInfo.apiVersion` 与备份清单里的 `apiVersion` 都用这一个常量：
/// 两处各写一份字面量的话，升版本时必然漏掉一处。口径是「主次版本」：
/// 新增**可选**字段或加性方法不改它；新增**必填**字段、删方法或改既有语义才动它
/// （见契约第 1.3 节与 `docs/architecture/m5-备份与恢复.md` 的兼容口径）。
///
/// 1.0 → 1.1：`SearchHit.captureId` 是必填字段。旧客户端忽略它是安全的，但
/// **新客户端 + 旧核心**读不到这个键（ArkTS 拿到 `undefined`），必须靠 `apiVersion`
/// 才分辨得出来——所以版本要动，不能只写「加性字段，调用方不受影响」。
pub const API_VERSION: &str = "1.1";

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
        let file_name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned());
        // 文件库是产品路径：默认装真实 HTTP 传输层。
        Self::prepare(
            Connection::open(path)?,
            Some(root),
            file_name,
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
            None,
            std::sync::Arc::new(provider::transport::PanicTransport {}),
        )
    }

    fn prepare(
        conn: Connection,
        root: Option<PathBuf>,
        library_file_name: Option<String>,
        transport: std::sync::Arc<dyn provider::transport::Transport>,
    ) -> Result<Self> {
        // 设备配置文件：与库文件同目录的 `core-config.json`。读失败/解析失败都不致命，
        // 只是这一档不生效（原因记在 `config_report` 里，`core.info` 会如实报出）。
        //
        // **先读配置、再建语义运行时**：自动排队的口径核对必须用这份配置解析出来的
        // 当前口径（环境变量逐字段优先、文件补空缺），不能拿一份旧快照去比。
        let loaded_config = config_file::load(root.as_deref());
        let semantic = semantic::SemanticRuntime::from_config_file(loaded_config.file.as_ref());
        Self::prepare_with_semantic(
            conn,
            root,
            library_file_name,
            transport,
            semantic,
            loaded_config,
        )
    }

    /// `prepare` 的实现体：语义运行时与已读到的配置文件都由调用方给——产品路径从
    /// `core-config.json` + 环境变量解析，测试路径显式注入（见两个 `_for_test` 入口）。
    fn prepare_with_semantic(
        mut conn: Connection,
        root: Option<PathBuf>,
        library_file_name: Option<String>,
        transport: std::sync::Arc<dyn provider::transport::Transport>,
        semantic: semantic::SemanticRuntime,
        loaded_config: config_file::LoadedCoreConfig,
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
        // 历史包袱一次性清账：以前因为没有执行者而进「已放弃 + no_handler」的旧任务，
        // 现在有执行者的放回队列（判据自带幂等，见 `jobs::requeue_legacy_no_handler`）。
        jobs::requeue_legacy_no_handler(&mut conn, support::now())?;
        // 已加载的嵌入器可能带着自己的维度（测试注入 384 维的桩）；其余情形是产品默认。
        let current_dims = semantic.current_dims();
        let mut core = Self {
            conn,
            root,
            library_file_name,
            leases: HashMap::new(),
            search_sessions: HashMap::new(),
            semantic,
            hybrid_min_score: search_session::HYBRID_SEMANTIC_MIN_SCORE,
            vector_storage: embedding::DEFAULT_VECTOR_STORAGE,
            current_dims,
            semantic_scan_depth: semantic::FAST_SCAN_DEPTH,
            secrets,
            transport,
            asr_engine: std::sync::Arc::new(transcription::sherpa::SherpaOnnxEngine),
            asr_config_for_test: None,
            backup_fault: backup::BackupFault::None,
            backup_gate: None,
            config_file: loaded_config.file,
            config_report: loaded_config.report,
        };
        // 存量库的口径（storage / dims / 已知时的 model_version）与当前不一致时，
        // **核心自己**把语义重建排上：`reasons` 说「需要重建」，就不能让用户找不到
        // 能重建的入口。判据、去重与「同一口径最多自动尝试两次」见
        // `semantic_index_job::enqueue_for_caliber_mismatch`。
        //
        // **必须在配置读完之后**：`semantic` 就是由这次读到的 `core-config.json`
        // 与环境变量解析出来的当前口径；先排队会拿旧口径去比。
        semantic_index_job::enqueue_for_caliber_mismatch(&mut core)?;
        Ok(core)
    }

    /// 仅供测试：用一份**显式**的模型配置打开资料库，不读进程环境变量。
    ///
    /// 为什么要这个入口：自动排队只在模型 `Configured` / `Ready` 时生效，而测试不能
    /// 靠设环境变量来造这个状态（环境变量是进程级的，并行测试会互相踩）。`None`
    /// 固定成「没配」的合法退化；`Some(config)` 走与 `from_config_file` 同一条
    /// `check_files` 路径（缺文件就是 `Failed`，不是 `Configured`）。
    #[doc(hidden)]
    pub fn open_with_embedding_config_for_test(
        path: impl AsRef<std::path::Path>,
        config: Option<embedding::EmbeddingConfig>,
    ) -> Result<Self> {
        let path = path.as_ref();
        let root = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .map_or_else(|| PathBuf::from("."), std::path::Path::to_path_buf);
        let file_name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned());
        let loaded_config = config_file::load(Some(&root));
        Self::prepare_with_semantic(
            Connection::open(path)?,
            Some(root),
            file_name,
            std::sync::Arc::new(provider::transport::PanicTransport {}),
            semantic::SemanticRuntime::configured(config),
            loaded_config,
        )
    }

    /// 仅供测试：打开资料库时就把一个**已加载**的嵌入器挂上（`Ready`）。
    ///
    /// 自动排队的 `model_version` 一项只在「内存里已有版本」时才比——产品冷启动做
    /// 不到这一点（见 `semantic_index_job` 的说明），测试用这个入口把那一刻提前。
    #[doc(hidden)]
    pub fn open_with_embedder_for_test(
        path: impl AsRef<std::path::Path>,
        embedder: Box<dyn embedding::Embedder>,
    ) -> Result<Self> {
        let path = path.as_ref();
        let root = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .map_or_else(|| PathBuf::from("."), std::path::Path::to_path_buf);
        let file_name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned());
        let loaded_config = config_file::load(Some(&root));
        Self::prepare_with_semantic(
            Connection::open(path)?,
            Some(root),
            file_name,
            std::sync::Arc::new(provider::transport::PanicTransport {}),
            semantic::SemanticRuntime::ready(embedder),
            loaded_config,
        )
    }

    /// 打开时读到的核心配置文件（`core-config.json` 的四个可选字段）。
    pub(crate) fn config_file(&self) -> Option<&config_file::CoreConfigFile> {
        self.config_file.as_ref()
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

    /// 仅供量测：改这条连接的 `synchronous`（产品默认 `FULL`）。
    ///
    /// 为什么量测需要它：`build_semantic_index` **一块一个事务**（中断可续跑的前提），
    /// `synchronous = FULL` 时每块一次 fsync。笔记本/机械盘上 fsync 可能比模型推理还贵，
    /// 于是「构建时间」主要量的是磁盘而不是索引。量测时用 `OFF` 把这一项单独拿掉，
    /// 并在报告里把两种口径分开写（产品默认的那一档单独量、按比例说明）。
    ///
    /// 它只影响这一条连接与之后的提交；产品代码不调它。
    #[doc(hidden)]
    pub fn set_synchronous_for_test(&self, mode: &str) -> Result<()> {
        self.conn.pragma_update(None, "synchronous", mode)?;
        Ok(())
    }

    /// 仅供量测：设 SQLite 的 `mmap_size`（0 = 关，产品默认沿用 SQLite 的默认值）。
    ///
    /// 大库语义查询要顺序读整代向量（10 万块 f32 = 195 MiB）。不开 mmap 时 SQLite 把页
    /// 先拷进自己的 2 MiB 页缓存，等于每次查询多一遍 195 MiB 的内存拷贝；`mmap_size`
    /// 让 BLOB 直接指进映射文件。量测用它回答「这一项值不值得进产品默认」。
    /// 它只影响这一条连接；产品代码不调它。
    #[doc(hidden)]
    pub fn set_mmap_size_for_test(&self, bytes: i64) -> Result<()> {
        self.conn.pragma_update(None, "mmap_size", bytes)?;
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

    /// 仅供量测/测试：改**新写入**向量的存储格式（`f32` / `int8`）。
    ///
    /// 为什么需要它：`storage` 是口径三字段之一，换格式必须走**同一条产品路径**
    /// （`build_semantic_index` 整代重算）来量，不能在探针里另写一套写入逻辑——
    /// 否则量到的召回与体积不是线上会得到的东西。
    ///
    /// 它**只影响之后写入的向量**：已有的生效代次不会就地改写；改完格式再检索，
    /// 整代口径核对会发现不一致（`ForeignSpace`），下一次 `build_semantic_index`
    /// 整代重算。产品代码不调它，默认值是可审计的 `DEFAULT_VECTOR_STORAGE`。
    #[doc(hidden)]
    pub fn set_vector_storage_for_test(&mut self, storage: VectorStorage) {
        self.vector_storage = storage;
    }

    /// 当前配置的向量存储格式（状态查询与量测用）。
    pub fn vector_storage(&self) -> VectorStorage {
        self.vector_storage
    }

    /// 当前已知的向量维度：注入/加载嵌入器后是它的维度，否则是产品模型的
    /// `EXPECTED_DIMS`。状态检查用它比对生效代次的 `dims`（不加载模型）。
    pub(crate) fn current_dims(&self) -> i64 {
        self.current_dims as i64
    }

    /// 当前**已知**的模型版本：已加载嵌入器优先；只配置了模型时读模型文件哈希
    /// （不建 ORT 会话）；都没有就是 `None`（无从比对，不猜）。
    pub(crate) fn current_model_version(&self) -> Option<String> {
        self.semantic.model_version_hint()
    }

    /// 当前**已经加载**的模型版本（内存里的真值）：没加载就是 `None`，不读文件。
    ///
    /// `open` 的自动排队只用这一项——为 discovery 去读 24 MiB 模型文件与「启动不做
    /// 重活」冲突。状态路径（`index_status`）仍用 `current_model_version`。
    pub(crate) fn loaded_model_version(&self) -> Option<String> {
        self.semantic.loaded_model_version()
    }

    /// 仅供量测/测试：改快速路径保留的候选块数上限（#90），用来扫「深度 vs 延迟」。
    ///
    /// 产品默认是可审计的 `FAST_SCAN_DEPTH`（4096）。这个上限**不是 ANN**：保留范围内
    /// 的名次仍是逐条精算的，只影响「最多能翻到多深」。产品代码不调它。
    #[doc(hidden)]
    pub fn set_semantic_scan_depth_for_test(&mut self, depth: usize) {
        self.semantic_scan_depth = depth.max(1);
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

    /// 分页读取任务，契约 `jobs.list` 的逐条翻页用。
    ///
    /// 排序与 `list_jobs` 相同（`priority DESC, created_at, id`），但游标是**键集**：
    /// 把上一页最后一条的位置带回去，下一批从它之后取，不是偏移。翻页期间入队的新任务
    /// 不会让后面的页漏项或重项。`cursor` 对调用方不透明，原样回传上一页的 `next_cursor`。
    pub fn list_jobs_page(
        &self,
        states: Option<&[JobState]>,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<JobPage> {
        jobs::list_page(self, states, cursor, limit)
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

    /// 排一条**语义索引**重建任务（`kind = "semantic_index"`）。
    ///
    /// **异步**：只排任务并返回它，真正的重建由 `semantic_index` 执行者在
    /// `run_due_jobs` 里跑——它**可以分段让出**（预算到点就回到 `queued`、进度写在
    /// `Job::progress` 上，下一轮接着算没算完的块），所以整库重建不会把一次唤醒拖死。
    ///
    /// 与 `schedule_index_rebuild` 的关系：那是关键词这一路（契约 4.4 的
    /// `indexes.rebuild`，只排 `index` 任务），这是语义这一路。**两个 kind**：
    /// 前置（等模型配置）、成败口径、能不能分段都不一样，混在一起会让既有入口的
    /// 语义跟着变。理由与排队口径见 `semantic_index_job` 模块说明与
    /// `docs/architecture/m4-任务调度与补做.md`。
    ///
    /// 任务**总是整库**（`target_ids` 为空）：代次与向量是全局的，范围只决定重算了
    /// 哪些块。因此业务键 `(kind, 空目标, 空快照)` 让「队列里至多有一条活着的语义
    /// 重建」——重复排队（连点、多处请求）不会产生重复工作。同一个 operationId +
    /// 同一个 `reason` 重复调用返回同一条任务（按 id 读当前状态）；换了 `reason` 报
    /// `idempotency_conflict`。
    ///
    /// **契约 4.4 还没有这个方法**，所以它只在核心上：桥接（`BridgeSession`）与能力
    /// 白名单都没有加，Dart 现在调不到——要给应用入口得先改契约。
    pub fn schedule_semantic_index(&mut self, reason: &str, operation_id: &str) -> Result<Job> {
        semantic_index_job::schedule(self, reason, operation_id)
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
    ///
    /// **它请求的是全部结果**：快速路径的深度截断无法证明完整时会整条回退精确 SQL，
    /// 所以这里不会出现「只返回前 4096 块、还声称与精确路径一致」的缺口（#135 复审）。
    pub fn semantic_ranking(
        &mut self,
        query: &str,
        filters: &SearchFilters,
    ) -> Result<Vec<(String, f32)>> {
        semantic::ranking(self, query, filters)
    }

    /// 仅供测试：**全量载入**路径的语义排名（与 `semantic_ranking` 同一段解码、
    /// 排序与折叠，只是先把整代向量收进堆）。用来钉住「流式扫描没有改变结果」。
    #[doc(hidden)]
    pub fn semantic_ranking_load_all_for_test(
        &mut self,
        query: &str,
        filters: &SearchFilters,
    ) -> Result<Vec<(String, f32)>> {
        semantic::ranking_load_all_for_test(self, query, filters)
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

    /// 本机转写引擎。默认实现按配置动态加载 sherpa-onnx 的 C API；
    /// 引擎库/模型不在时它如实报「不可用」，闸门据此把任务停在等配置。
    pub fn asr_engine(&self) -> &dyn provider::local_engine::LocalEngine {
        self.asr_engine.as_ref()
    }

    /// 本机转写引擎的**所有权句柄**（`Arc`）。
    ///
    /// 给转写执行者用：它在一次任务里要一边调用引擎、一边写库（需要 `&mut Core`），
    /// 拿一个借用会与写库的借用打架；`Arc` 让「引擎是谁」与「库怎么改」互不牵制。
    pub(crate) fn asr_engine_handle(&self) -> std::sync::Arc<dyn provider::local_engine::LocalEngine> {
        self.asr_engine.clone()
    }

    /// 仅供测试/量测：钉死本机引擎的配置（模型目录、引擎库路径）。
    ///
    /// 产品代码不调它：产品路径按「配置行 → 环境变量 → 随包默认」解析
    /// （见 `provider::local_engine::EngineConfig::resolve`）。
    #[doc(hidden)]
    pub fn set_asr_engine_config_for_test(
        &mut self,
        config: provider::local_engine::EngineConfig,
    ) {
        self.asr_config_for_test = Some(config);
    }

    /// 钉死的引擎配置（没有就是 None）。给 `provider::local_engine::config_for` 用。
    pub(crate) fn asr_engine_config_for_test(&self) -> Option<&provider::local_engine::EngineConfig> {
        self.asr_config_for_test.as_ref()
    }

    /// 仅供测试/量测：换一个本机转写引擎（stub、记账替身）。
    ///
    /// 产品代码不调它：真实现就是默认值。测试注入 stub 之后，**转写链的每一段**
    /// （分段、恢复、幂等、降级、时间戳）都能在没有任何模型与原生库的 CI 里跑。
    #[doc(hidden)]
    pub fn set_asr_engine_for_test(
        &mut self,
        engine: std::sync::Arc<dyn provider::local_engine::LocalEngine>,
    ) {
        self.asr_engine = engine;
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

    // ------------------------------------------------------------ 日记生成与版本（B4b）

    /// 生成一篇日记，契约第 4.3 节 `diary.generate`。
    ///
    /// **输入在返回之前就冻结**：这一天的材料清单（来源、修订、提取版本、覆盖范围、
    /// 时间范围、风格、提示词版本与生成选项）写进不可变的快照行并算出哈希，返回的
    /// `source_snapshot_id` 指的就是它。真正的调用由队列在 `run_due_jobs` 里执行，
    /// 执行时**只读快照**——事后改了材料也不会改变这一版基于什么。
    ///
    /// 候选/当前：首次（这一天没有当前版本、也没有用户自己的版本）生成的结果会成为
    /// 当前版本；其余一律候选，**当前版本不会被生成改写**。同一份快照哈希与同一条
    /// `operationId` 不重复产版本（`reason = regenerate` 是「用户明确要再来一版」的例外）。
    ///
    /// 没有可用的服务**照样返回票据**：那条任务由队列停在 `waiting_configuration`
    /// 等配置（与 #66 的能力闸门同一口径），不是一次调用失败。
    pub fn generate_diary(
        &mut self,
        day_key: &str,
        style_id: Option<&str>,
        reason: DiaryGenerationReason,
        operation_id: &str,
    ) -> Result<DiaryGenerationTicket> {
        diary::request_generation(self, day_key, style_id, reason, operation_id)
    }

    /// 某一天的当前/候选与材料覆盖，契约第 4.3 节 `diary.getDay`。
    ///
    /// 候选与当前只在这里区分（契约的 `DiaryVersion` 没有状态字段）。空日报 `not_found`。
    pub fn get_diary_day(&self, day_key: &str) -> Result<DiaryDay> {
        diary::get_day(self, day_key)
    }

    /// 分页读某一天的日记版本，契约第 4.3 节 `diary.listVersions`。最新在前。
    pub fn list_diary_versions(
        &self,
        day_key: &str,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<DiaryVersionPage> {
        diary::list_versions(self, day_key, cursor, limit)
    }

    /// 读一条完整的日记版本（含生成信息与来源关联），契约第 4.3 节 `diary.getVersion`。
    pub fn get_diary_version(&self, version_id: &str) -> Result<DiaryVersion> {
        diary::get_version(self, version_id)
    }

    /// 保存人工编辑：写一条新的 `user_edit` 版本，契约第 4.3 节 `diary.saveEdit`。
    ///
    /// **绝不原地改**：基版（`baseVersionId`）一个字节都不动，新版本的 `parentVersionId`
    /// 指向它、`origin = user_edit`；新版本**成为当前**（用户手写编辑就是「我要用这一版」），
    /// 旧当前版退成候选、不删除（`diary.getDay` 的 `candidateVersionIds`）。
    ///
    /// `expectedRevision` 是这一天的版本数（`candidateVersionIds.length +
    /// (currentVersionId == null ? 0 : 1)`）：不符报 `revision_conflict`，**零改动**。
    /// 同一条 `operationId` 重试返回原版本；换了正文/基版/`expectedRevision` 报
    /// `idempotency_conflict`。空正文报 `invalid_argument`；正文**不设长度上限**。
    pub fn save_diary_edit(
        &mut self,
        base_version_id: &str,
        body: &str,
        expected_revision: i64,
        operation_id: &str,
    ) -> Result<DiaryVersion> {
        diary::save_edit(self, base_version_id, body, expected_revision, operation_id)
    }

    /// 指定某一天的当前版本，契约第 4.3 节 `diary.setCurrent`。
    ///
    /// **只允许指向这一天自己的版本**：版本不存在报 `not_found`，属于别的天报
    /// `invalid_argument`。`expectedRevision`（版本数）不符报 `revision_conflict`，零改动。
    /// 旧当前版变候选、仍可取回；按目标状态幂等：切到已经是当前的版本是**无操作**
    /// （不重复发事件、不新增版本），所以这一行契约没有 `operationId`。
    pub fn set_diary_current(
        &mut self,
        day_key: &str,
        version_id: &str,
        expected_revision: i64,
    ) -> Result<DiaryDay> {
        diary::set_current(self, day_key, version_id, expected_revision)
    }

    /// 把两版（或多版）合并成一条新版本，契约第 4.3 节 `diary.merge`。
    ///
    /// `body` 是**用户确认后的正文**（核心不替用户合并、不调模型：`modelCalls = 0`）。
    /// 新版本 `origin = merge`、`parentVersionId = baseVersionId`，参与合并的全部版本 id
    /// 记在 `generationInfo.generationOptions.mergeSourceVersionIds`（来源可追溯）；
    /// **参与合并的版本一行都不改**，随时可以取回原文。新版本成为当前。
    ///
    /// 候选列表为空 / 有重复 / 包含基版自己 / 版本不属于这一天 → `invalid_argument`
    /// （版本不存在 → `not_found`）；`expectedRevision` 与 `operationId` 的语义同 `saveEdit`。
    pub fn merge_diary_versions(
        &mut self,
        base_version_id: &str,
        candidate_version_ids: &[String],
        body: &str,
        expected_revision: i64,
        operation_id: &str,
    ) -> Result<DiaryVersion> {
        diary::merge_versions(
            self,
            base_version_id,
            candidate_version_ids,
            body,
            expected_revision,
            operation_id,
        )
    }

    // ------------------------------------------------------------ 整理与自动化（B4e）

    /// 全部可用整理模板（现在只有三个核心内置的；插件模板见 #88）。
    ///
    /// 模板规则是**产品指令**，参数只接受整数与布尔——两者都不接受自由文本，
    /// 所以调用方（或将来的插件）没有地方能把「指令」塞进提示词的指令槽。
    pub fn organize_templates(&self) -> Vec<OrganizeTemplate> {
        organize::list_templates()
    }

    /// 整理设置：开关（**默认关**）、模板与参数、各项上限、计划触发时间。
    pub fn organize_settings(&self) -> Result<OrganizeSettings> {
        organize::load_settings(self)
    }

    /// 更新整理设置，契约第 4.4 节 `settings.update` 的落地（整理这一段）。
    ///
    /// 口径：
    /// - **必须显式开启**：`enabled` 默认是 `false`，只有调用方明确传 `true` 才会打开；
    /// - **校验先行**：模板不存在 `not_found`；参数类型/范围、各项上限、`HH:mm`
    ///   不合法都是 `invalid_argument`，且**零改动**；
    /// - **过期就拒绝**：`expectedRevision` 不符报 `revision_conflict`，零改动；
    /// - **按目标状态幂等**：值没变就是无操作（`revision` 不 +1）。
    ///
    /// 传入对象里的 `revision` / `updatedAt` 是**只读结论，会被忽略**（与
    /// `providers.save` 忽略 `testStatus` 同一个口径）。
    pub fn update_organize_settings(
        &mut self,
        settings: &OrganizeSettings,
        expected_revision: i64,
    ) -> Result<OrganizeSettings> {
        organize::update_settings(self, settings, expected_revision)
    }

    /// 整理状态：开关、模板、**什么时候会跑、会跑哪些材料**、配额用量与最近一次结果。
    ///
    /// **只读**：不发请求、不排任务、不改任何东西。没开启时也能看出「开启后会跑什么」。
    pub fn organize_status(&self) -> Result<OrganizeStatus> {
        organize::status(self)
    }

    /// 排一轮自动整理：把「该整理」的日子冻结成快照、排成 `diary_generate` 任务。
    ///
    /// 驱动（`run_due_jobs`）每轮开始时会自己调它；单独暴露是为了测试与将来的界面
    /// （例如用户刚打开开关，想立刻看看会排什么）。**默认关时它什么都不做**。
    pub fn plan_organize(&mut self, now: DateTime<Utc>) -> Result<OrganizePlanSummary> {
        organize::plan_due(self, now)
    }

    /// 仅供测试/量测：按一句只读查询取一行文本。
    ///
    /// 产品代码不调它：核心的读取入口都有自己的方法与语义（例如快照 `body` 是内部
    /// 形状，不在契约里）。测试用它核对**落库的原始事实**，不另造一套读取路径。
    #[doc(hidden)]
    pub fn debug_query_text(&self, sql: &str, param: Option<&str>) -> Result<Option<String>> {
        let mut statement = self.conn.prepare(sql)?;
        let value = match param {
            Some(value) => statement
                .query_row(params![value], |row| row.get::<_, String>(0))
                .optional()?,
            None => statement
                .query_row([], |row| row.get::<_, String>(0))
                .optional()?,
        };
        Ok(value)
    }

    /// 仅供测试/量测：按一句只读查询取一个计数。
    #[doc(hidden)]
    pub fn debug_query_count(&self, sql: &str, param: Option<&str>) -> Result<i64> {
        let mut statement = self.conn.prepare(sql)?;
        let value = match param {
            Some(value) => statement.query_row(params![value], |row| row.get::<_, i64>(0))?,
            None => statement.query_row([], |row| row.get::<_, i64>(0))?,
        };
        Ok(value)
    }

    // ------------------------------------------------------------ 备份与恢复

    /// 创建备份包（B7，验收 E34；契约 `backups.create` 的**同步**入口）。
    ///
    /// 顺序是硬要求，也是「快照边界」成立的依据：
    /// 1. `VACUUM INTO` 取**一致性快照**（不拷 `.sqlite` 文件，见 `backup` 模块说明）；
    /// 2. 读**快照**（不是源库）算出这一份备份的内容清单；
    /// 3. 复制被快照引用的内容寻址原件（`blobs/<sha 前两位>/<sha>`），逐条重算 sha256；
    /// 4. 写完**自校验**（重开包、逐条目重算哈希、比大小、比条目集合），过了才把包
    ///    放到 `target_path` 上。
    ///
    /// 备份期间资料库可以继续写：全程**不加全局写锁**，`VACUUM INTO` 只读源库，
    /// 快照之后的新写入不进这份备份、也不会让备份失败。
    ///
    /// `target_path` 是**包文件的完整路径**（父目录会被创建）；已经存在时报
    /// `invalid_argument`，绝不覆盖旧备份。中途任何失败：目标路径上不会留下文件。
    ///
    /// 库里已经有原件不可用（文件被外部删掉）时**报错而不是产出一份缺原件的包**：
    /// 「完整备份」是这一条的承诺；要不要允许「如实标记为不完整」的部分备份由产品定
    /// （见架构文档「没有做到的」）。
    pub fn create_backup(&self, target_path: &str) -> Result<BackupReport> {
        backup::create(self, std::path::Path::new(target_path))
    }

    /// 检查备份包（契约 `backups.inspect` 的**同步**入口）。
    ///
    /// 逐条目重算哈希、比大小、比条目集合，并检查路径安全与版本兼容。坏在哪就
    /// 如实列在哪（`issues` 的每条是 `种类: 细节`，种类只有 `corrupted` /
    /// `unsafe_path` / `incompatible` 三种），**不抛错**——界面要能先展示
    /// 「这个包有什么问题」再决定要不要恢复。
    ///
    /// 只有连清单都读不出来（不是 zip、没有清单、清单解析不了）才报
    /// `corrupted_backup`；格式版本更高报 `incompatible_backup`。
    pub fn inspect_backup(&self, package_path: &str) -> Result<BackupInspection> {
        backup::inspect(std::path::Path::new(package_path))
    }

    /// 恢复到**新的空目录**（B7，验收 E34/E35；契约 `backups.restore` 的同步入口）。
    ///
    /// 三条保证：
    /// - **绝不覆盖当前资料库**：目标必须是新的空目录（不存在，或存在且为空），
    ///   且不能是当前资料库所在目录；恢复过程一个字都不写当前资料库；
    /// - **逐条校验原件哈希**（顺带校验快照哈希与大小），再打开恢复出来的库做
    ///   完整性、外键、关键行数与原件的交叉核对，全部通过才落地；
    /// - **失败不留半成品**：所有解包先在目标同级的暂存目录里做，全部过了才**一次
    ///   rename** 落地；中途失败只删自己的暂存目录，目标目录保持原样。
    ///
    /// 拒绝口径与错误码（E35）：
    /// - 损坏（哈希/大小/结构/清单与内容不一致）⇒ `corrupted_backup`；
    /// - 越界路径（绝对路径、`..`、反斜杠/盘符、清单外的条目）⇒ `corrupted_backup`
    ///   （绝不按越界路径解包）；
    /// - 不兼容（`schemaVersion` / `apiVersion` 高于本构建）⇒ `incompatible_backup`。
    ///
    /// 恢复出来的库与快照**逐字节相同**：不在这一步做迁移，也不做恢复态修正；
    /// 那些留给第一次 `Core::open`（既有 `recover_on_open` 语义，本条不改）。
    /// 未完成的导入/录音因此如实保持「未完成」。
    pub fn restore_backup(&self, package_path: &str, target_dir: &str) -> Result<RestoreReport> {
        backup::restore(
            self,
            std::path::Path::new(package_path),
            std::path::Path::new(target_dir),
        )
    }

    /// 仅供测试：注入备份环节的故障（默认不注入）。
    ///
    /// 自校验这条路径平时不会被触发，而验收计划明确要求对「备份创建」做故障注入；
    /// 产品代码不调它。
    #[doc(hidden)]
    pub fn set_backup_fault_for_test(&mut self, fault: BackupFault) {
        self.backup_fault = fault;
    }

    /// 仅供测试：让备份在打包随行片段之前停在会合点上，等测试放行（默认不设）。
    ///
    /// 用来把「并发的录音最终化」**确定地**安排进备份窗口，而不是靠 sleep 去碰运气
    /// （见 `BackupGate`）。产品代码不调它。
    #[doc(hidden)]
    pub fn set_backup_gate_for_test(&mut self, gate: Arc<BackupGate>) {
        self.backup_gate = Some(gate);
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
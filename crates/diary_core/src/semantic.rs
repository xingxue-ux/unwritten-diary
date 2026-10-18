//! 语义索引与语义检索：真实向量接入（B3c-2 后半）。
//!
//! 这一片把 B3c-1 建好的块与向量存储结构接上真实模型，并严格照
//! `docs/architecture/m2-向量索引与换代.md` 的**消费契约**出结果：
//!
//! 1. **命中按篇上报**：同一篇的多个块命中折叠成一条，取得分最高的块当证据。
//! 2. **`groupId` 与关键词那一路一致**：来源篇 = `chunk_spans.source_id`，
//!    记录自己写的文字 = `capture_id`（`chunk_spans` 里记录文字那一段的
//!    `source_id` 写的就是 `capture_id`，两路于是同一条规则）。
//! 3. **合块按 `chunk_spans` 分别归属**：一块含多篇时，这一块的相似度对各篇共享，
//!    各自成条。
//! 4. **`snippet` / `locator` 取该篇自己那一段**（`start_char` / `end_char` 圈出的
//!    区间），**不是**整块拼接文本——整块里混着别的篇。
//!
//! # 换代流程（照文档「building → active」）
//!
//! `Core::build_semantic_index` 一次跑完；任务化的 `semantic_index` 作业走
//! `build_with_deadline`（同一段实现，多一个预算截止点：到点就在块与块之间让出，
//! 下一轮续跑）：
//!
//! 1. **重建块**（`chunks::rebuild`）：只删消失的块 + UPSERT，内容没变的块保住 id
//!    与既有向量；
//! 2. **开新代次**：`building_generation = COALESCE(building_generation, COALESCE(active_generation, 0) + 1)`。
//!    正在服务的检索继续读旧那一代，界面不会突然空掉；
//! 3. **续跑前核对口径**：`building_generation` 是上一次中断（或取消、或预算让出）留下的。
//!    若那一代里已有的向量有任何一条与**当前嵌入器**的 `model_version` / `dims` / `storage`
//!    不一致（中途换过模型或换过存储格式），就整代清理、按当前模型从零重算。否则旧模型的
//!    半成品会被当成「已完成」，剩下的块由新模型补算，最后切出一个**混合代次**；
//! 4. **复用 + 补算**：新代次里还没有向量的块，先尝试从旧代次**整批复制**口径一致
//!    （`model_version` / `dims` / `storage` 都相同）的向量，剩下的才算。
//!    这正是「没变的块不重复算」。任务路径在这一步**逐块检查预算**：到点就让出
//!    （至少算完一块，保证每次唤醒都前进；见 `build_with_deadline`）；
//! 5. **原子切换**：一个事务里 `active_generation = building_generation;
//!    building_generation = NULL`，并清掉 `generation < active` 的旧向量。
//!    不存在「一半新一半旧」的服务状态，也不会每重建一次就把存储翻倍。
//!    **让出的那一轮不走这一步**：半成品不激活，只有全部块算完才切换。
//!
//! 换代期间**关键词检索照旧**：块与向量都在自己的表里，不碰 `search_docs` /
//! `search_grams`，也不推 `search_index_epoch`。
//!
//! # 语义检索
//!
//! 默认过滤器下走**两段式精确扫描**（#90）：先用一张表扫完生效代次的向量、只留
//! 得分最高的 `FAST_SCAN_DEPTH` 块，再只取这些块的 `chunk_spans` / `captures` 元数据，
//! 按篇折叠。**这不是 ANN**：保留范围内的名次仍然是逐条精算的，只是不保留更深的
//! 结果（上限说明见 `docs/architecture/m5-大库可用性.md`）。日期 / 类型 / 来源范围
//! 过滤会用上单表扫描里没有的列，那种查询走原来的逐行 join 精确 SQL（候选集通常
//! 小得多）。两段式里只要有块没通过后置过滤，就整条回退到精确 SQL——绝不拿
//! 「前 K 条恰好都有效」当完整结果。
//!
//! 向量本身按 `int8`（体积优先）或 `f32` 存，解码按行上的 `storage` 分派；编码格式
//! 是口径三字段之一（#58），换格式整代重算。点积直接从 BLOB 字节读（x86_64 上 AVX2），
//! 三条路径共用同一个打分函数，分数逐位一致。
//!
//! # 口径判据（换模型未重建）
//!
//! 生效代次的口径（`model_version` / `dims` / `storage`）与**当前嵌入器**不一致时
//! （`Single(旧模型)` 或历史遗留的 `Mixed`），查询向量与新库向量来自两个向量空间，
//! 余弦相似度**全是垃圾但看起来正常**——有排序、有分数。所以 `candidates` 在生成查询
//! 向量之前先核对整代口径（`caliber_mismatch`），不一致就返回 `ForeignSpace`：
//! **语义这一路不作数**（不当成证据、不拿它的分排序），由调用方如实说出原因
//! （纯语义空结果 + 警告；混合退化为关键词、阶段停在 `keyword_ready`）。
//! 重建语义索引即可按当前模型整代重算并恢复。判据的完整说明见
//! `docs/architecture/m2-向量索引与换代.md`。

use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap, HashSet};
use std::sync::{Arc, OnceLock};
use std::time::Instant;

use rusqlite::types::ValueRef;
use rusqlite::{params, OptionalExtension};

use crate::chunks;
use crate::embedding::{
    self, Embedder, EmbeddingConfig, EnvEmbedding, TokenTruncation, VectorStorage,
};
use crate::error::{CoreError, Result};
use crate::model::{
    Coverage, MatchedBy, SearchFilters, SearchHit, SourceKind, SourceLocator,
};
use crate::Core;

/// 语义摘录最多这么多字符（超出截断加省略号）。取该篇自己那一段，不是整块正文。
const SEMANTIC_SNIPPET_CHARS: usize = 160;

// ---------------------------------------------------------------- 查询分段计时
//
// 「10 万块一次语义查询 1.5 秒」必须先知道钱花在哪，才谈得上优化。这里在**产品查询
// 路径本身**上打点（不是探针里另写一套），分七段：取行（SQLite 步进 / 列拷贝）、
// 反序列化、余弦、拼候选、排序、折叠、查询嵌入。默认**关闭**：关闭时每行只多一次
// `bool` 判断，没有 `Instant::now()`。只有探针的 `--profile` 会打开它。
//
// 为什么不放进 `#[cfg(test)]`：这段要被 release 探针量真语料，测试构建量不出真数字。

/// 一次语义查询的分段耗时（纳秒）。`rows` 是扫过的向量行数。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SemanticQueryProfile {
    pub rows: u64,
    /// 整个 `candidates_impl` 的墙钟时间（含下面所有段与未归类的残余）。
    pub total_ns: u64,
    /// 查询文本 → 查询向量。
    pub embed_ns: u64,
    /// SQL 准备（编译语句；进程内只发生在第一次）。
    pub prepare_ns: u64,
    /// `rows.next()`：SQLite 虚拟机的步进。
    pub step_ns: u64,
    /// 取出 13 列（含 BLOB 拷贝成 `Vec<u8>`、7 个 `String` 的分配）。
    pub fetch_ns: u64,
    /// `storage.decode`：字节 → `Vec<f32>`。
    pub decode_ns: u64,
    /// 点积 / 余弦。
    pub cosine_ns: u64,
    /// 拼 `SemanticCandidate`（字符串移动 + 结构体入 `Vec`）。
    pub assemble_ns: u64,
    /// 排序。
    pub sort_ns: u64,
    /// 按篇折叠。
    pub fold_ns: u64,
    /// 语义覆盖状态（`chunks::counts`）——与查询文本无关，但每次查询都算一遍。
    pub coverage_ns: u64,
    /// 首屏结果装配（按页取正文、拼 `SearchHit`）。
    pub materialize_ns: u64,
    /// #90 快速路径：单表向量扫描（取 BLOB + 校验 + 算分 + 选 top-K）。
    pub fast_scan_ns: u64,
    /// #90 快速路径：只取 top-K 块的元数据（span + capture）。
    pub fast_meta_ns: u64,
    /// 其中的 SQLite 部分（准备语句 + 逐行步进）。
    pub fast_meta_query_ns: u64,
    /// 其中的 Rust 部分（取列、拼候选、去重核对）。
    pub fast_meta_build_ns: u64,
    /// #90 快速路径是否命中；`false` = 回退到逐行 join 的精确路径。
    pub fast_path: bool,
    /// 语义覆盖计数是否走了快路径；`false` = 因为库里有异常块/过期 span 退回精确 SQL。
    /// 与 `fast_path` 是**两条独立的回退**：覆盖计数只要有一条过期 span 就整库回退，
    /// 与查询命中哪一块无关。
    pub coverage_fast: bool,
}

impl SemanticQueryProfile {
    /// 已归类各段之和。
    pub fn accounted_ns(self) -> u64 {
        self.embed_ns
            + self.prepare_ns
            + self.step_ns
            + self.fetch_ns
            + self.decode_ns
            + self.cosine_ns
            + self.assemble_ns
            + self.sort_ns
            + self.fold_ns
            + self.coverage_ns
            + self.materialize_ns
            + self.fast_scan_ns
            + self.fast_meta_ns
    }
}

mod profile_sink {
    use super::SemanticQueryProfile;
    use std::cell::RefCell;
    use std::sync::atomic::{AtomicBool, Ordering};

    static ENABLED: AtomicBool = AtomicBool::new(false);

    thread_local! {
        static ACC: RefCell<SemanticQueryProfile> = const { RefCell::new(SemanticQueryProfile {
            rows: 0,
            total_ns: 0,
            embed_ns: 0,
            prepare_ns: 0,
            step_ns: 0,
            fetch_ns: 0,
            decode_ns: 0,
            cosine_ns: 0,
            assemble_ns: 0,
            sort_ns: 0,
            fold_ns: 0,
            coverage_ns: 0,
            materialize_ns: 0,
            fast_scan_ns: 0,
            fast_meta_ns: 0,
            fast_meta_query_ns: 0,
            fast_meta_build_ns: 0,
            fast_path: false,
            coverage_fast: false,
        }) };
    }

    pub(super) fn set_enabled(enabled: bool) {
        ENABLED.store(enabled, Ordering::Relaxed);
        reset();
    }

    pub(super) fn enabled() -> bool {
        ENABLED.load(Ordering::Relaxed)
    }

    pub(super) fn reset() {
        ACC.with(|acc| *acc.borrow_mut() = SemanticQueryProfile::default());
    }

    /// 取走这一段计时并清零（探针按查询累积中位数）。
    pub(super) fn take() -> Option<SemanticQueryProfile> {
        if !enabled() {
            return None;
        }
        ACC.with(|acc| {
            let mut slot = acc.borrow_mut();
            let profile = *slot;
            *slot = SemanticQueryProfile::default();
            Some(profile)
        })
    }

    /// 累加一段。调用方先判 `enabled()`，关闭时不会走到这里。
    pub(super) fn add(update: impl FnOnce(&mut SemanticQueryProfile)) {
        ACC.with(|acc| update(&mut acc.borrow_mut()));
    }
}

/// 仅供量测：打开/关闭语义查询的分段计时（默认关；产品代码不调它）。
#[doc(hidden)]
pub fn set_query_profiling(enabled: bool) {
    profile_sink::set_enabled(enabled);
}

/// 仅供量测：取走上一次语义查询的分段计时；没打开计时时返回 `None`。
#[doc(hidden)]
pub fn take_query_profile() -> Option<SemanticQueryProfile> {
    profile_sink::take()
}

pub(crate) fn elapsed_ns(start: Instant) -> u64 {
    u64::try_from(start.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

/// 分段计时是否打开。会话层据此决定要不要取 `Instant`。
pub(crate) fn profile_enabled() -> bool {
    profile_sink::enabled()
}

/// 会话层（覆盖状态、首屏装配）也记进同一份分段账本。关闭计时时零成本。
pub(crate) fn note_profile(update: impl FnOnce(&mut SemanticQueryProfile)) {
    if profile_sink::enabled() {
        profile_sink::add(update);
    }
}

/// 一次 `build_semantic_index` 的结果。**算出来的真值**，测试与探针靠它核对
/// 「没变的块没有重复算」。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticBuildReport {
    /// 这次生效的代次。
    pub generation: i64,
    /// 块表里的块数（全库；`source_scope` 只决定重算了哪些块）。
    pub total_chunks: i64,
    /// 从旧代次整批复制过来的向量数（口径一致，没有重算）。
    pub reused_chunks: i64,
    /// 这次真正算了向量的块数。
    pub embedded_chunks: i64,
    /// 上一次中断留下的、本代次里已经有的**口径一致**向量数（同模型续跑时不为 0；
    /// 换过模型时那一批被整代清掉、这里回到 0）。
    pub resumed_chunks: i64,
    /// **本次调用**里正文超长、被 token 截断后才算的块数（issue #109）。
    ///
    /// 产品切块按 400 字符，实测最多 402 token（数字见
    /// `docs/architecture/m2-质量集与分块.md`），正常永远是 0；不为 0 就说明有别的
    /// 入口把超长正文递给了嵌入器，或者切块规则/分词器换了——这就是「不静默」的信号。
    pub truncated_chunks: i64,
    /// 本次调用里这些块一共被截掉多少 token（`truncated_chunks` 的 token 之和）。
    pub truncated_tokens: i64,
}

/// 嵌入器在 `Core` 上的状态。懒加载：`Core::open` 只读环境变量与 stat 文件，
/// 不建 ORT 会话（启动不做重活）。
pub(crate) enum EmbedderSlot {
    /// 一个环境变量都没设：合法退化。
    NotConfigured,
    /// 配了、文件也在，但还没加载。
    Configured(EmbeddingConfig),
    /// 已经加载好（进程级单例，退出不释放）。
    Ready(Arc<dyn Embedder>),
    /// 配了但不可用，原因是给人看的。**不许**把它盖成「未就绪」。
    Failed(String),
}

/// `Core` 上挂的语义运行时状态。
pub(crate) struct SemanticRuntime {
    pub slot: EmbedderSlot,
    /// 解析结果（每条路径从哪来）。`core.info.runtime_config` 直接用它，不再重算一遍
    /// ——重算就会读第二次环境变量，而环境变量是打开库那一刻的快照。
    pub resolution: embedding::EmbeddingResolution,
    /// 配置了模型但还没加载时，`model_version` 可以从模型文件哈希算出来（不建 ORT
    /// 会话）。第一次 `index_status` 算一次就缓存——状态检查不该每次重读 24 MiB 模型。
    configured_version: OnceLock<Option<String>>,
}

impl SemanticRuntime {
    /// 从「环境变量 + 可选的核心配置文件」初始化。只 stat 文件，不加载模型。
    ///
    /// 顺序是 `embedding::resolve` 里钉死的：环境变量逐字段优先，文件补空缺。
    pub(crate) fn from_config_file(file: Option<&crate::config_file::CoreConfigFile>) -> Self {
        let resolution = embedding::resolve(file);
        let slot = match &resolution.state {
            EnvEmbedding::Unset => EmbedderSlot::NotConfigured,
            EnvEmbedding::Invalid(reason) => EmbedderSlot::Failed(reason.clone()),
            EnvEmbedding::Ready(config) => match config.check_files() {
                Ok(()) => EmbedderSlot::Configured(config.clone()),
                // 缺文件也是**诚实的原因**，不是「未就绪」。
                Err(reason) => EmbedderSlot::Failed(reason),
            },
        };
        Self {
            slot,
            resolution,
            configured_version: OnceLock::new(),
        }
    }

    /// 按一份**显式**配置初始化（测试用；产品路径只走 `from_config_file`）。
    ///
    /// `None` 固定为「一个环境变量都没设」的合法退化——测试不能依赖进程环境变量
    /// （它是进程级的，多个测试并行时会互相踩）。
    pub(crate) fn configured(config: Option<EmbeddingConfig>) -> Self {
        let (slot, state) = match &config {
            None => (EmbedderSlot::NotConfigured, EnvEmbedding::Unset),
            Some(config) => match config.check_files() {
                Ok(()) => (
                    EmbedderSlot::Configured(config.clone()),
                    EnvEmbedding::Ready(config.clone()),
                ),
                Err(reason) => (
                    EmbedderSlot::Failed(reason.clone()),
                    EnvEmbedding::Invalid(reason),
                ),
            },
        };
        Self {
            slot,
            resolution: explicit_resolution(state, config.as_ref()),
            configured_version: OnceLock::new(),
        }
    }

    /// 直接给一个**已加载**的嵌入器（测试用）：`open` 时就能比 `model_version`。
    pub(crate) fn ready(embedder: Box<dyn Embedder>) -> Self {
        Self {
            slot: EmbedderSlot::Ready(Arc::from(embedder)),
            // 注入的嵌入器没有「配置从哪来」这回事：报告如实说 absent，不编来源。
            resolution: explicit_resolution(EnvEmbedding::Unset, None),
            configured_version: OnceLock::new(),
        }
    }

    /// 当前**已知**的模型版本：已加载的嵌入器优先；只配了模型时用文件哈希算
    /// （不建 ORT 会话）；都没配就是 `None`（无从比对，不能猜）。
    pub(crate) fn model_version_hint(&self) -> Option<String> {
        match &self.slot {
            EmbedderSlot::Ready(embedder) => Some(embedder.model_version().to_owned()),
            EmbedderSlot::Configured(config) => self
                .configured_version
                .get_or_init(|| embedding::model_version_for(config).ok())
                .clone(),
            EmbedderSlot::NotConfigured | EmbedderSlot::Failed(_) => None,
        }
    }

    /// 当前**已经加载**的模型版本（内存里的真值，不读文件、不算哈希）。
    ///
    /// `open` 的自动排队只用它：`Configured` 时算哈希要读一遍模型文件（24 MiB），
    /// 与「启动不做重活」冲突。没加载就返回 `None`——那时宁可不比，也不为 discovery
    /// 去读文件；状态路径（`index_status`）另走 `model_version_hint`。
    pub(crate) fn loaded_model_version(&self) -> Option<String> {
        match &self.slot {
            EmbedderSlot::Ready(embedder) => Some(embedder.model_version().to_owned()),
            _ => None,
        }
    }

    /// 当前嵌入器的维度（已加载时以它为准；否则是产品默认）。
    pub(crate) fn current_dims(&self) -> usize {
        match &self.slot {
            EmbedderSlot::Ready(embedder) => embedder.dims(),
            _ => embedding::EXPECTED_DIMS,
        }
    }
}

/// 测试入口的配置解析结果：显式路径按「环境变量」记来源（它确实是调用方给的），
/// `None` 则如实记「没有任何来源」。只服务 `configured` / `ready` 两个测试构造器；
/// 产品路径的来源由 `embedding::resolve` 逐字段给出。
fn explicit_resolution(
    state: EnvEmbedding,
    config: Option<&EmbeddingConfig>,
) -> embedding::EmbeddingResolution {
    use crate::config_file::ConfigOrigin;
    let origin = if config.is_some() {
        ConfigOrigin::Env
    } else {
        ConfigOrigin::Absent
    };
    embedding::EmbeddingResolution {
        state,
        source: origin,
        model_dir: config.map(|config| config.model_dir.clone()),
        model_dir_origin: origin,
        ort_dylib: config.map(|config| config.ort_dylib.clone()),
        ort_dylib_origin: origin,
    }
}

/// 给状态用的一句话：语义这一路为什么没就绪。
pub(crate) enum EmbedderReadiness {
    NotConfigured,
    Configured,
    Ready,
    Failed(String),
}

impl Core {
    /// 显式注入嵌入器（测试与探针用）。注入之后不再读环境变量、不再加载真模型。
    pub fn set_embedder(&mut self, embedder: Box<dyn Embedder>) {
        self.current_dims = embedder.dims();
        self.semantic.configured_version = OnceLock::new();
        self.semantic.slot = EmbedderSlot::Ready(Arc::from(embedder));
    }

    /// 语义这一路现在到哪一步了（不加载模型）。
    pub(crate) fn embedder_readiness(&self) -> EmbedderReadiness {
        match &self.semantic.slot {
            EmbedderSlot::NotConfigured => EmbedderReadiness::NotConfigured,
            EmbedderSlot::Configured(_) => EmbedderReadiness::Configured,
            EmbedderSlot::Ready(_) => EmbedderReadiness::Ready,
            EmbedderSlot::Failed(reason) => EmbedderReadiness::Failed(reason.clone()),
        }
    }

    /// **已经加载好**的嵌入器（不触发懒加载）。
    ///
    /// 给「不进计算、也不加载模型就能做的口径核对」用：slot 是 `Configured` 时这里给
    /// `None`——那时要比口径只能真的加载模型，由检索路径在 `core.embedder()` 之后核对
    /// （见 `search_session::upgrade`）。`Arc` 是克隆，不移动 slot。
    pub(crate) fn loaded_embedder(&self) -> Option<Arc<dyn Embedder>> {
        match &self.semantic.slot {
            EmbedderSlot::Ready(embedder) => Some(Arc::clone(embedder)),
            _ => None,
        }
    }

    /// 拿一个可用的嵌入器。**懒加载**：第一次真正要算向量时才建 ORT 会话。
    ///
    /// 拿不到时返回**诚实的错误**（没有配置 / 加载失败 / 维度不符），调用方据此
    /// 保持状态不变——绝不假装「语义未就绪」把错误盖掉。
    pub(crate) fn embedder(&mut self) -> Result<Arc<dyn Embedder>> {
        let slot = std::mem::replace(&mut self.semantic.slot, EmbedderSlot::NotConfigured);
        match slot {
            EmbedderSlot::Ready(embedder) => {
                let handle = Arc::clone(&embedder);
                self.semantic.slot = EmbedderSlot::Ready(embedder);
                Ok(handle)
            }
            EmbedderSlot::NotConfigured => {
                self.semantic.slot = EmbedderSlot::NotConfigured;
                Err(semantic_unavailable(
                    "没有配置本地模型：设 DIARY_MODEL_DIR（模型目录）与 DIARY_ORT_DYLIB（ORT 动态库），\
                     或在资料库旁边的 core-config.json 里写 modelDir 与 ortDylib，之后才能建语义索引；\
                     现在只有关键词索引可用"
                        .to_owned(),
                ))
            }
            EmbedderSlot::Failed(reason) => {
                self.semantic.slot = EmbedderSlot::Failed(reason.clone());
                Err(semantic_unavailable(reason))
            }
            EmbedderSlot::Configured(config) => match embedding::load(&config, embedding::EmbedOptions::default())
            {
                Ok(loaded) => {
                    let embedder: Arc<dyn Embedder> = Arc::new(loaded);
                    self.current_dims = embedder.dims();
                    self.semantic.slot = EmbedderSlot::Ready(Arc::clone(&embedder));
                    Ok(embedder)
                }
                Err(reason) => {
                    self.semantic.slot = EmbedderSlot::Failed(reason.clone());
                    Err(semantic_unavailable(reason))
                }
            },
        }
    }

}

/// 一次语义重建**能分段跑**的结果：跑完，还是到了截止点主动让出。
///
/// 为什么要有这个二分：整库重建会远超一次唤醒的预算，而同步入口
/// （`Core::build_semantic_index`）必须照旧「跑完才返回」。作业路径
/// （`jobs::run_semantic_index`）走的是带截止点的那条：到点就让出，
/// `building_generation` 留着，下一轮**接着算没算完的块**（见 `build_generation`）。
pub(crate) enum SemanticBuildOutcome {
    /// 跑完了：新代次已经**原子切换**（`active_generation` 指向它）。
    Done(SemanticBuildReport),
    /// 到了截止点、主动让出：这一代还在建，**没有切换**、也没有丢弃。
    Yielded(SemanticBuildCheckpoint),
}

/// 让出时的进度快照（都是库里算出来的真值，不是估算）。
pub(crate) struct SemanticBuildCheckpoint {
    /// 正在建的代次（= `index_meta.building_generation`）。
    pub generation: i64,
    /// 块表里的块数（全库）。
    pub total_chunks: i64,
    /// 这一代里**已经有口径一致向量**的块数。
    pub completed_chunks: i64,
}

/// 进度回调：每写完一块向量就如实报一次 `(已完成, 总数)`。
///
/// 为什么做成回调而不是「跑完再报」：作业状态里的 `progress` 要在**跑的过程中**也是
/// 真值（一整库重建可能跑好几分钟、跨好几轮唤醒）。语义模块自己不认识任务，
/// 写进 `jobs.progress_*` 这件事由作业执行者传进来的闭包做；同步入口传一个空闭包。
pub(crate) type ProgressSink<'a> = &'a mut dyn FnMut(&mut Core, i64, i64) -> Result<()>;

/// 建语义索引（同步、跑完才返回）：重建块 → 开新代次 → 复用/补算向量 → 原子切换。
///
/// 模型缺失或加载失败时**返回诚实的错误、不改任何状态**（连块都不重建）。
/// `source_scope` 与 `rebuild_text_chunks` 同义：`Some([])` 是「什么都不看」，
/// 什么都不做。公开入口是 `Core::build_semantic_index`（见 `lib.rs`）。
///
/// **没有截止点就不会让出**：这条路径与任务化之前的行为逐字一致（跑完才返回）。
/// 可中断的分段路径是 `build_with_deadline`，由 `semantic_index` 作业执行者用。
pub(crate) fn build(
    core: &mut Core,
    source_scope: Option<&[String]>,
) -> Result<SemanticBuildReport> {
    match build_with_deadline(core, source_scope, None, &mut |_, _, _| Ok(()))? {
        SemanticBuildOutcome::Done(report) => Ok(report),
        // 没有截止点却让出了：这是实现错了，如实报出来，不假装跑完。
        SemanticBuildOutcome::Yielded(checkpoint) => Err(CoreError::InvalidState {
            entity: "语义索引",
            id: "build".to_owned(),
            state: format!(
                "没有预算截止点却让出了（第 {} 代已完成 {} / {} 块）",
                checkpoint.generation, checkpoint.completed_chunks, checkpoint.total_chunks
            ),
        }),
    }
}

/// 建语义索引，**带一个截止点**：到点就在块与块之间让出，下次续跑。
///
/// 分段的粒度与保证：
///
/// - **至少算完一块再让出**（`missing` 非空时）。这是「每次唤醒都必须前进」的保证：
///   否则一个极小预算的唤醒会算了 0 块就让出，下一轮还是 0 块，永远建不完；
/// - 让出**不丢进度**：向量是一块一个事务写进去的（`write_vector`），
///   `building_generation` 留着，下一轮 `missing_chunks` 只挑还没算的块；
/// - 让出**不激活半成品**：原子切换（`active_generation = building`）只在全部块算完后
///   发生，让出路径根本不走那一步；
/// - **续跑前照旧核对口径**（`has_foreign_vectors` → 整代废弃）：让出不是绕过 #58 的理由，
///   中途换过模型就整代按当前模型重算，绝不混两个向量空间。
///
/// 不可中断的部分（如实说明）：`chunks::rebuild` 的整日重打包与「从旧代次整批复用」的那条
/// `INSERT ... SELECT` 各自是一段批量 SQL，没有中途取消点；分段只在**向量计算**这一环
/// （真实代价的大头，也是唯一在模型上跑的环）。
pub(crate) fn build_with_deadline(
    core: &mut Core,
    source_scope: Option<&[String]>,
    deadline: Option<std::time::Instant>,
    progress: ProgressSink<'_>,
) -> Result<SemanticBuildOutcome> {
    if matches!(source_scope, Some([])) {
        return Ok(SemanticBuildOutcome::Done(SemanticBuildReport {
            generation: 0,
            total_chunks: 0,
            reused_chunks: 0,
            embedded_chunks: 0,
            resumed_chunks: 0,
            truncated_chunks: 0,
            truncated_tokens: 0,
        }));
    }
    // 先拿嵌入器：拿不到就在这里失败，**不改任何状态**（块也不重建）。
    let embedder = core.embedder()?;
    // 重建块（没变的块保住 id，因此保住旧代次的向量）。
    chunks::rebuild(core, source_scope)?;
    build_generation(core, &*embedder, deadline, progress)
}

fn semantic_unavailable(reason: String) -> CoreError {
    CoreError::InvalidState {
        entity: "语义索引",
        id: "local-embedding".to_owned(),
        state: reason,
    }
}

/// 这个错误是不是「本地嵌入器拿不到」（没配 / 配了但加载失败）？
///
/// 作业执行者（`jobs::run_semantic_index`）据此把任务落回 `waiting_configuration`
/// ——等配置，不是失败，不退避也不消耗重试额度。判据只有一处，避免调用方各自去
/// 比对错误里的字符串。
pub(crate) fn is_embedder_unavailable(error: &CoreError) -> bool {
    matches!(
        error,
        CoreError::InvalidState { entity, id, .. }
            if *entity == "语义索引" && id == "local-embedding"
    )
}

/// 写进 `chunk_vectors` 的一套口径。三个字段与表里的列一一对应，**三者全同**才算
/// 「同一个向量空间」：
///
/// - `model_version` 由模型文件哈希导出，换模型文件必然变；
/// - `dims` 是向量维度，不同模型可能不同；
/// - `storage` 是向量的编码格式（`f32` / `int8`，见 `VectorStorage`）。
///
/// 口径不同的一批向量放在一起算余弦，量出来的相似度没有意义（两个空间各说各话），
/// 所以这三项也是「能不能复用/续跑」的唯一判据。
struct Caliber<'a> {
    model_version: &'a str,
    dims: i64,
    storage: &'a str,
}

impl Caliber<'_> {
    fn of(embedder: &dyn Embedder, storage: VectorStorage) -> Caliber<'_> {
        Caliber {
            model_version: embedder.model_version(),
            dims: embedder.dims() as i64,
            // `wire()` 是 `&'static str`，可以活到 `Caliber<'a>` 的任何生命周期。
            storage: storage.wire(),
        }
    }
}

/// 这个代次里与 `caliber` 口径不一致的向量**行数**（`model_version` / `dims` /
/// `storage` 三项一起比）。有一条不一致就说明这一代里混进了**另一个向量空间**的向量。
fn foreign_row_count(core: &Core, generation: i64, caliber: &Caliber<'_>) -> Result<i64> {
    let foreign: i64 = core.conn.query_row(
        "SELECT COUNT(*) FROM chunk_vectors WHERE generation = ?1 \
           AND (model_version <> ?2 OR dims <> ?3 OR storage <> ?4)",
        params![generation, caliber.model_version, caliber.dims, caliber.storage],
        |row| row.get(0),
    )?;
    Ok(foreign)
}

/// 这个代次里有没有与当前口径不一致的向量。有一条不一致就说明这一代是**上一个模型**
/// （或上一种存储格式）留下的半成品，不能在其基础上续算。
fn has_foreign_vectors(core: &Core, generation: i64, caliber: &Caliber<'_>) -> Result<bool> {
    Ok(foreign_row_count(core, generation, caliber)? > 0)
}

/// 丢弃一个代次：按 `generation` 清掉它的**全部**向量。
///
/// - **幂等**：没有行时 `DELETE` 影响 0 行，再跑一次结果一样；中断在任何一步都不会
///   留下更坏的状态。
/// - **如实**：清理是整代的，不会留下属于旧模型的孤儿行让后面的统计或检索再撞上。
/// - **代次号继续用**：清空之后它就是一代干净的、由当前模型从零算起的新代次，没必要
///   跳号——跳号只会让「中断 → 换模型 → 续跑」多出一批必须记得清掉的旧行。
fn discard_generation(core: &Core, generation: i64) -> Result<()> {
    core.conn.execute(
        "DELETE FROM chunk_vectors WHERE generation = ?1",
        params![generation],
    )?;
    Ok(())
}

/// 开代次 → 复用/补算 → 原子切换。嵌入器已经确认可用。
///
/// `deadline` 为 `Some` 时是**可中断的分段路径**：向量计算在块与块之间检查截止点，
/// 到点就返回 `Yielded`（代次留着、进度留着、**不切换**），下一轮接着算没算完的块。
/// `None` 表示跑完才返回（同步入口的行为）。
fn build_generation(
    core: &mut Core,
    embedder: &dyn Embedder,
    deadline: Option<std::time::Instant>,
    progress: ProgressSink<'_>,
) -> Result<SemanticBuildOutcome> {
    let caliber = Caliber::of(embedder, core.vector_storage);
    let (active, building) = chunks::generations(core)?;
    // 开新代次：中断过就从它续跑，否则在生效代次之后 +1。**不在原地更新 active**。
    let generation = match building {
        Some(value) => value,
        None => active.map_or(1, |value| value + 1),
    };
    // **续跑前核对口径**：`building_generation` 是上一次中断（或取消、或让出）留下的。
    // 若那一代里已经有向量，但有任何一条的 `model_version` / `dims` / `storage` 与
    // **当前嵌入器**不一致，就说明中途换过模型或换过存储格式——整代废弃、清干净，
    // 按当前模型从零重算。
    //
    // 不核对会怎样（评审在 #58 指出的）：旧模型的半成品被 `missing_chunks` 当成
    // 「已完成」，剩下的块由新模型补算，最后切出一个**混合代次**——一个不是同一个
    // 向量空间的代次（#80 之前搜索会跨空间打分；现在语义这一路整路不作数），
    // 而 `index_status.model_version` 也只能挑其中一行误报。
    //
    // **分段让出不能绕过这一步**：每一次续跑（含预算让出后的下一轮）都从这里开始，
    // 核对永远是「接着算」的前提。
    if let Some(value) = building {
        if has_foreign_vectors(core, value, &caliber)? {
            discard_generation(core, value)?;
        }
    }
    core.conn.execute(
        "UPDATE index_meta SET building_generation = ?1 WHERE id = 1",
        params![generation],
    )?;

    let total_chunks: i64 = core
        .conn
        .query_row("SELECT COUNT(*) FROM text_chunks", [], |row| row.get(0))?;

    // 复用：把旧代次里口径一致的向量整批复制到本代次。口径 = model_version +
    // dims + storage 都相同；块 id 是内容的函数，所以内容没变的块必然被复制到。
    let reused_chunks = match active {
        None => 0,
        Some(source_generation) => core.conn.execute(
            "INSERT INTO chunk_vectors (chunk_id, generation, model_version, dims, storage, \
                                        vector, created_at) \
             SELECT v.chunk_id, ?1, v.model_version, v.dims, v.storage, v.vector, ?2 \
             FROM chunk_vectors v JOIN text_chunks t ON t.chunk_id = v.chunk_id \
             WHERE v.generation = ?3 AND v.model_version = ?4 AND v.dims = ?5 AND v.storage = ?6 \
             ON CONFLICT(chunk_id, generation) DO NOTHING",
            params![
                generation,
                crate::embedding::now_iso(),
                source_generation,
                caliber.model_version,
                caliber.dims,
                caliber.storage,
            ],
        )? as i64,
    };

    // 补齐：本代次里还没有**口径一致**向量的块。逐块提交：中途中断也留着
    // building_generation，下一轮从这儿继续（不会激活半成品）。
    let missing = missing_chunks(core, generation, &caliber)?;
    // 续跑的块数 = 本代次里**已经存在的、口径一致的**向量数：总数减去这一轮复用的、
    // 再减去这一轮要补算的。换模型时上一轮那批被整代清掉，这里自然回到 0。
    let resumed_chunks = (total_chunks - reused_chunks - missing.len() as i64).max(0);
    let started_with: i64 = reused_chunks + resumed_chunks;
    let mut embedded_this_run: i64 = 0;
    // #109：整块算向量时如果入口做了 token 截断，这里如实累计（向量仍然写、索引仍然
    // 能用；但不能假装「这一块完整参与了向量」）。
    let mut truncated_chunks: i64 = 0;
    let mut truncated_tokens: i64 = 0;
    for (chunk_id, text) in &missing {
        let embedding = embedder.embed_document(text)?;
        if let Some(truncation) = embedding.truncation {
            truncated_chunks += 1;
            truncated_tokens += truncation.dropped_tokens as i64;
        }
        let vector = embedding.vector;
        if vector.len() as i64 != caliber.dims {
            return Err(CoreError::InvalidState {
                entity: "语义嵌入",
                id: chunk_id.clone(),
                state: format!(
                    "算出来的是 {} 维，与模型声明的 {} 维不符",
                    vector.len(),
                    caliber.dims
                ),
            });
        }
        write_vector(
            core,
            chunk_id,
            generation,
            caliber.model_version,
            core.vector_storage,
            &vector,
        )?;
        embedded_this_run += 1;
        // 进度与向量在同一个提交之后报：库里的向量数就是进度数。
        progress(core, started_with + embedded_this_run, total_chunks)?;

        // **预算到点就让出**（每算一块看一次钟）：但**至少算完一块**才允许让出
        // （`embedded_this_run >= 1` 永远为真，这里写出来是为了让「每次唤醒都前进」
        // 这条保证在代码里看得见）。写向量是逐块的独立事务，让出不会丢进度、
        // 也不会激活半成品。
        if let Some(deadline) = deadline {
            if std::time::Instant::now() >= deadline {
                return Ok(SemanticBuildOutcome::Yielded(SemanticBuildCheckpoint {
                    generation,
                    total_chunks,
                    // 让出这一刻「已完成」的真值：开跑时就有的 + 这一轮算完的。
                    completed_chunks: started_with + embedded_this_run,
                }));
            }
        }
    }

    // 原子切换 + 清理旧代次。一个事务：不存在「一半新一半旧」的服务状态。
    let tx = core.conn.transaction()?;
    tx.execute(
        "UPDATE index_meta SET active_generation = ?1, building_generation = NULL WHERE id = 1",
        params![generation],
    )?;
    tx.execute(
        "DELETE FROM chunk_vectors WHERE generation < ?1",
        params![generation],
    )?;
    tx.commit()?;

    Ok(SemanticBuildOutcome::Done(SemanticBuildReport {
        generation,
        total_chunks,
        reused_chunks,
        embedded_chunks: missing.len() as i64,
        resumed_chunks,
        truncated_chunks,
        truncated_tokens,
    }))
}

/// 本代次里还没有**口径一致**向量的块（按日期与块内顺序，确定性）。
///
/// 「已完成」的判据不是「`(chunk_id, generation)` 这一行存在」，而是「这一行存在**且**
/// `model_version` / `dims` / `storage` 与当前嵌入器一致」。存在但口径不一致的行
/// （历史遗留的混合代次）算**没完成**，会被当前模型重算覆盖——这是 `write_vector` 的
/// `ON CONFLICT ... DO UPDATE` 那条路，不会留下旧行。
fn missing_chunks(
    core: &Core,
    generation: i64,
    caliber: &Caliber<'_>,
) -> Result<Vec<(String, String)>> {
    let mut statement = core.conn.prepare(
        "SELECT t.chunk_id, t.text FROM text_chunks t \
         LEFT JOIN chunk_vectors v ON v.chunk_id = t.chunk_id AND v.generation = ?1 \
           AND v.model_version = ?2 AND v.dims = ?3 AND v.storage = ?4 \
         WHERE v.chunk_id IS NULL ORDER BY t.day_key, t.ordinal, t.chunk_id",
    )?;
    let rows = statement
        .query_map(
            params![generation, caliber.model_version, caliber.dims, caliber.storage],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// 写一行向量。自己的事务：中途中断也不会留下半行。
///
/// `ON CONFLICT ... DO UPDATE` 同时是**覆盖旧口径遗留行**的入口：同一
/// `(chunk_id, generation)` 若已经有一行属于别的模型，这里把它改成当前口径的向量，
/// 不会新旧并存。
fn write_vector(
    core: &Core,
    chunk_id: &str,
    generation: i64,
    model_version: &str,
    storage: VectorStorage,
    vector: &[f32],
) -> Result<()> {
    core.conn.execute(
        "INSERT INTO chunk_vectors (chunk_id, generation, model_version, dims, storage, vector, \
                                    created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) \
         ON CONFLICT(chunk_id, generation) DO UPDATE SET \
             model_version = excluded.model_version, dims = excluded.dims, \
             storage = excluded.storage, vector = excluded.vector, created_at = excluded.created_at",
        params![
            chunk_id,
            generation,
            model_version,
            vector.len() as i64,
            storage.wire(),
            storage.encode(vector)?,
            embedding::now_iso(),
        ],
    )?;
    Ok(())
}

// ------------------------------------------------------------ 语义检索

/// `candidates` 的结果：**口径一致、算出来的候选**，还是**口径不一致、根本不该算**。
///
/// 做成显式的二分（而不是「返回空 Vec，让调用方猜原因」）是因为两者要说的话不一样：
/// 「算出来是空」是「没有相近的内容」，口径不一致是「这一路不作数，重建索引即可恢复」。
/// 后者绝不能静默变成空结果——那会把「跨空间算分」换成另一种静默。
pub(crate) enum SemanticCandidates {
    /// 口径一致：这是算出来的候选（可能为空——没有相近的内容）。
    Scored(Vec<SemanticCandidate>),
    /// 生效代次与当前嵌入器不是同一个向量空间：**没有进入计算**，语义这一路不作数。
    ForeignSpace(CaliberMismatch),
}

/// 一次语义候选计算的结果：候选 + 查询侧有没有被 token 截断（issue #109）。
///
/// 查询超长不再是错误：入口截到 510 个正文 token 再算，[`TokenTruncation`] 由这里
/// 带出去，让检索会话在 `warnings` 里如实说一句（而不是让用户以为整句都参与了检索）。
pub(crate) struct SemanticOutcome {
    pub candidates: SemanticCandidates,
    pub query_truncation: Option<TokenTruncation>,
}

/// 一条「块 × 篇」候选。分数属于整块，按 span 归属到各自的篇。
#[derive(Debug, Clone)]
pub(crate) struct SemanticCandidate {
    /// 篇的标识：来源篇 = `source_id`，记录自己的文字 = `capture_id`。
    pub group_id: String,
    /// 证据块。
    pub chunk_id: String,
    /// 这一篇在块里的第几段。
    pub span_ordinal: i64,
    /// 证据块正文的哈希（`text_chunks.text`，**不含**任何篇标识 / 库内 UUID）。
    ///
    /// 同分兜底用：`chunk_id` 与 `group_id` 都含 v7 UUID，同一份内容重新建库就会换一批；
    /// 这个键只由块正文决定，是「跨库也稳定」的那一层（issue #57）。正文完全相同的
    /// 两个块才会落到更后面的 `chunk_id`，那时用户看到的摘录已经一样。
    pub content_key: String,
    pub score: f32,
    pub capture_id: String,
    pub source_id: String,
    pub source_revision_id: String,
    pub kind: String,
    pub coverage: Coverage,
    pub day_key: String,
    pub start_char: i64,
    pub end_char: i64,
}

/// 生效代次；没有就是 `None`。
pub(crate) fn active_generation(core: &Core) -> Result<Option<i64>> {
    Ok(chunks::generations(core)?.0)
}

/// 语义候选：算查询向量 → 全表扫当前代次的向量 → 按 span 展开 → 过滤 → 排序 → 折叠。
///
/// **口径判据在进入计算之前**（见 `caliber_mismatch`）：生效代次与当前嵌入器不是同一个
/// 向量空间时，这里**直接返回 `ForeignSpace`**——不生成查询向量、不读一行向量、不算
/// 一次余弦。这不是逐行 dims 检查能兜住的（逐行只能发现「这一行坏了」，发现不了
/// 「整代是另一个模型建的」；而跨空间的余弦会给出**看起来正常**的分数）。
///
/// **折叠在分页之前**：同一篇的多个块命中只留得分最高的那一个，所以「一篇长材料」
/// 不会在结果里出现十次（消费契约第 1 条）。
pub(crate) fn candidates(
    core: &Core,
    embedder: &dyn Embedder,
    query: &str,
    filters: &SearchFilters,
) -> Result<SemanticOutcome> {
    candidates_impl(core, embedder, query, filters, RowMode::Streaming)
}

/// 取行的两种方式。**生产只用 `Streaming`**；`LoadAll` 是测试用的参考实现
/// （把整代向量先收进 `Vec`，也就是这一片改掉的那条老路），用来钉住两条路径
/// 的结果逐字节一致。
#[derive(Clone, Copy, PartialEq, Eq)]
enum RowMode {
    Streaming,
    LoadAll,
}

fn candidates_impl(
    core: &Core,
    embedder: &dyn Embedder,
    query: &str,
    filters: &SearchFilters,
    mode: RowMode,
) -> Result<SemanticOutcome> {
    let profiling = profile_sink::enabled();
    let total_start = profiling.then(Instant::now);
    let Some(generation) = active_generation(core)? else {
        return Ok(SemanticOutcome {
            candidates: SemanticCandidates::Scored(Vec::new()),
            query_truncation: None,
        });
    };
    if let Some(mismatch) = caliber_mismatch(core, embedder)? {
        return Ok(SemanticOutcome {
            candidates: SemanticCandidates::ForeignSpace(mismatch),
            query_truncation: None,
        });
    }
    if query.trim().is_empty() {
        return Ok(SemanticOutcome {
            candidates: SemanticCandidates::Scored(Vec::new()),
            query_truncation: None,
        });
    }
    let embed_start = profiling.then(Instant::now);
    let query_embedding = embedder.embed_query(query)?;
    if let Some(start) = embed_start {
        profile_sink::add(|profile| profile.embed_ns += elapsed_ns(start));
    }
    let query_truncation = query_embedding.truncation;
    let query_vector = query_embedding.vector;
    if query_vector.len() != embedder.dims() {
        return Err(CoreError::InvalidState {
            entity: "语义检索",
            id: "query".to_owned(),
            state: format!(
                "查询向量是 {} 维，与模型声明的 {} 维不符",
                query_vector.len(),
                embedder.dims()
            ),
        });
    }

    // 查询向量的模长只算一次（原来每行都重算一遍 left_norm）：三条路径共用同一个值，
    // 分数因此逐位一致。
    let query_norm: f32 = query_vector.iter().map(|value| value * value).sum();

    // **快速路径（#90）**：默认过滤器下，先用一张表扫完向量、选 top-K 块，再只取这
    // 些块的元数据。10 万块上「逐行 join span + capture」占查询总时间的一半以上，
    // 而默认过滤条件用不上那些列——它们只在最后 K 条上核对一次。条件与回退规则见
    // `fast_path_eligible` / `candidates_fast`；任何一步核对不上（有块被过滤掉）就
    // 整条回退到下面的精确 SQL，绝不拿「前 K 条里恰好没被过滤」当完整结果。
    if mode == RowMode::Streaming && fast_path_eligible(filters) {
        if let Some(scored) =
            candidates_fast(
                core,
                &query_vector,
                query_norm,
                generation,
                filters,
                core.semantic_scan_depth,
                profiling,
            )?
        {
            if profiling {
                profile_sink::add(|profile| profile.fast_path = true);
            }
            let ranked = rank_and_fold(core, scored, profiling)?;
            if let Some(start) = total_start {
                profile_sink::add(|profile| profile.total_ns += elapsed_ns(start));
            }
            return Ok(SemanticOutcome {
                candidates: SemanticCandidates::Scored(ranked),
                query_truncation,
            });
        }
    }

    // **来源篇必须仍是当前修订**：块的 id 与向量都按内容存，`revise_text` 之后、下一次
    // `build_semantic_index` 之前，旧修订的块与向量**仍在生效代次里**——不过滤就会把旧正文
    // 当成当前内容召回（#32 明确禁止；关键词那一路在 #54 已按同一口径修过）。
    // 记录自写文字的 span 没有来源修订（空串），单独按 `capture_id` 处理即可。
    let (kinds, scope) = crate::search_session::filters_to_params(filters)?;
    let sql = "SELECT v.vector, v.dims, v.storage, s.chunk_id, s.ordinal, s.capture_id, \
                      s.source_id, s.source_revision_id, s.start_char, s.end_char, s.kind, \
                      s.coverage, c.day_key \
               FROM chunk_vectors v \
               JOIN chunk_spans s ON s.chunk_id = v.chunk_id \
               JOIN captures c ON c.id = s.capture_id \
               WHERE v.generation = ?1 \
                 AND (?2 = 1 OR c.state <> 'trashed') \
                 AND (?3 IS NULL OR c.day_key >= ?3) \
                 AND (?4 IS NULL OR c.day_key <= ?4) \
                 AND (?5 IS NULL OR s.kind IN (SELECT value FROM json_each(?5))) \
                 AND (?6 IS NULL OR s.source_id IN (SELECT value FROM json_each(?6))) \
                   AND (s.source_revision_id = '' \
                        OR EXISTS (SELECT 1 FROM source_items si \
                                   WHERE si.source_id = s.source_id \
                                     AND si.current_revision_id = s.source_revision_id))";
    let prepare_start = profiling.then(Instant::now);
    let mut statement = core.conn.prepare(sql)?;
    if let Some(start) = prepare_start {
        profile_sink::add(|profile| profile.prepare_ns += elapsed_ns(start));
    }
    // **流式扫描**：一行一行地取、解码、算余弦，算完就丢。不要 `collect` 成
    // `Vec<Vec<u8>>`——那正是「10 万块 f32 = 195 MiB/次查询」的来源：把整代向量
    // 连同 SQLite 的一份拷贝同时放进堆里，峰值是向量总量的两倍左右。
    //
    // 现在堆里只留**候选的元数据**（篇标识、块标识、区间、分数），每个候选几十字节；
    // 向量字节的生命周期只有一行。这样查询峰值内存与「向量精度/维度」解耦：
    // int8 与 f32 的峰值差，只来自候选数量而不是向量宽度。
    let params = params![
        generation,
        i64::from(filters.include_trashed),
        filters.from_day_key,
        filters.to_day_key,
        kinds,
        scope,
    ];
    let scored: Vec<SemanticCandidate> = match mode {
        // 生产路径：一行一行地取、解码、算余弦，算完就丢。
        RowMode::Streaming => {
            let mut rows = statement.query(params)?;
            let mut scored = Vec::new();
            let mut scanned = 0_u64;
            while let Some(row) = {
                let step_start = profiling.then(Instant::now);
                let next = rows.next()?;
                if let (Some(start), true) = (step_start, next.is_some()) {
                    profile_sink::add(|profile| profile.step_ns += elapsed_ns(start));
                }
                next
            } {
                let fetch_start = profiling.then(Instant::now);
                let raw = raw_row(row)?;
                if let Some(start) = fetch_start {
                    profile_sink::add(|profile| profile.fetch_ns += elapsed_ns(start));
                }
                scored.push(candidate_of(raw, &query_vector, query_norm, profiling)?);
                scanned += 1;
            }
            if profiling {
                profile_sink::add(|profile| profile.rows = scanned);
            }
            scored
        }
        // 测试参考：先全部收进 `Vec<RawRow>`（连同每行的向量字节），再算。
        RowMode::LoadAll => {
            let rows: Vec<RawRow> = statement
                .query_map(params, raw_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let mut scored = Vec::with_capacity(rows.len());
            for raw in rows {
                scored.push(candidate_of(raw, &query_vector, query_norm, profiling)?);
            }
            if profiling {
                profile_sink::add(|profile| profile.rows = scored.len() as u64);
            }
            scored
        }
    };

    let ranked = rank_and_fold(core, scored, profiling)?;
    if let Some(start) = total_start {
        profile_sink::add(|profile| profile.total_ns += elapsed_ns(start));
    }
    Ok(SemanticOutcome {
        candidates: SemanticCandidates::Scored(ranked),
        query_truncation,
    })
}

// ------------------------------------------------------------ 快速路径（#90）

/// 快速路径最多保留多少个**块**（按篇折叠之前）。
///
/// 4 096 块 = 默认每页 20 条时的 200 页。这不是 ANN：保留范围内的名次仍然是逐条精算的，
/// 只是**不再保留第 4 096 块之后的结果**——10 万块 × 数千条命中本来也翻不到那么深。
/// 为什么不无限保留：那等于把整代向量的元数据都取回来（正是这条快速路径省掉的 join）。
///
/// 10 万块上的「深度 vs 延迟」实测（`--scan-depth`）：int8 2 048/4 096/8 192 块
/// = P50 136/156/200 ms；f32 = 347/366/411 ms。取 4 096 是在「够深」与「P95 进
/// 200 ms」之间选的；上限与曲线写在 `docs/architecture/m5-大库可用性.md`。测试
/// `fast_scan_falls_back_when_a_top_candidate_is_filtered_out` 钉住回退行为，既有
/// `streaming_scan_matches_the_load_all_reference` 钉住与精确路径逐位一致。
pub(crate) const FAST_SCAN_DEPTH: usize = 4_096;

/// 只有**默认过滤器**才走单表扫描。
///
/// 日期区间 / 类型 / 来源范围都会把候选限制在库的一个子集里，而单表扫描里没有这些列：
/// 先按分数取 top-K 再过滤，可能把「分数不高但确实符合过滤条件」的块整个漏掉。那种
/// 查询仍走精确 SQL（候选集通常也小得多）。`include_trashed` 不在此列——它只影响
/// 后置核对，核对不上会整条回退。
fn fast_path_eligible(filters: &SearchFilters) -> bool {
    filters.from_day_key.is_none()
        && filters.to_day_key.is_none()
        && filters.kinds.is_empty()
        && filters.source_scope.is_none()
}

/// 快速路径选中的一条：分数 + 块所在行的 rowid。
#[derive(Debug, Clone, Copy)]
struct ScoredRow {
    score: f32,
    rowid: i64,
}

impl PartialEq for ScoredRow {
    fn eq(&self, other: &Self) -> bool {
        self.score.to_bits() == other.score.to_bits() && self.rowid == other.rowid
    }
}

impl Eq for ScoredRow {}

/// 保留 `PartialOrd` 只是 `BinaryHeap` / `Ord` 的 trait 适配：真正的排序口径在
/// `Ord::cmp` 里，已经用 `total_cmp`（见下）。这里**不是**第二套比较规则，也没有
/// `unwrap_or(Equal)`——不会因为 NaN 退化成「相等」。
impl PartialOrd for ScoredRow {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for ScoredRow {
    fn cmp(&self, other: &Self) -> Ordering {
        // 反向比较：`BinaryHeap` 是最大堆，这里要让「分数最低」的那条浮到堆顶，
        // 它才是下一个该被替换掉的候选。
        other
            .score
            .total_cmp(&self.score)
            .then_with(|| other.rowid.cmp(&self.rowid))
    }
}

/// 单表扫描 + 只取 top-K 元数据。返回 `None` 表示**必须回退**到精确路径。
///
/// 回退条件只有一条：选中的 K 个块里有任何一个没通过后置过滤（被回收 / 修订过期）。
/// 那时「前 K 条恰好都有效」不能证明「第 K+1 条没有更该出现」，所以不猜。
fn candidates_fast(
    core: &Core,
    query_vector: &[f32],
    query_norm: f32,
    generation: i64,
    filters: &SearchFilters,
    depth: usize,
    profiling: bool,
) -> Result<Option<Vec<SemanticCandidate>>> {
    let scan_start = profiling.then(Instant::now);
    let mut heap: BinaryHeap<ScoredRow> = BinaryHeap::with_capacity(depth + 1);
    let mut scanned = 0_u64;
    {
        let mut statement = core
            .conn
            .prepare("SELECT rowid, dims, storage, vector FROM chunk_vectors WHERE generation = ?1")?;
        let mut rows = statement.query(params![generation])?;
        while let Some(row) = {
            let step_start = profiling.then(Instant::now);
            let next = rows.next()?;
            if let (Some(start), true) = (step_start, next.is_some()) {
                profile_sink::add(|profile| profile.step_ns += elapsed_ns(start));
            }
            next
        } {
            let fetch_start = profiling.then(Instant::now);
            let rowid: i64 = row.get(0)?;
            let dims: i64 = row.get(1)?;
            let storage = match row.get_ref(2)? {
                ValueRef::Text(bytes) => std::str::from_utf8(bytes)
                    .ok()
                    .and_then(VectorStorage::from_wire)
                    .ok_or_else(|| CoreError::CorruptedData {
                        message: format!(
                            "不认识的向量存储格式：{}",
                            String::from_utf8_lossy(bytes)
                        ),
                    })?,
                _ => {
                    return Err(CoreError::CorruptedData {
                        message: "chunk_vectors.storage 不是文本".to_owned(),
                    })
                }
            };
            let bytes = match row.get_ref(3)? {
                ValueRef::Blob(bytes) => bytes,
                _ => {
                    return Err(CoreError::CorruptedData {
                        message: "chunk_vectors.vector 不是 BLOB".to_owned(),
                    })
                }
            };
            if let Some(start) = fetch_start {
                profile_sink::add(|profile| profile.fetch_ns += elapsed_ns(start));
            }
            let score_start = profiling.then(Instant::now);
            let score = score_of(storage, bytes, query_vector, query_norm, dims, "")?;
            if let Some(start) = score_start {
                profile_sink::add(|profile| profile.cosine_ns += elapsed_ns(start));
            }
            scanned += 1;
            let keep_start = profiling.then(Instant::now);
            if heap.len() < depth {
                heap.push(ScoredRow { score, rowid });
            } else if let Some(worst) = heap.peek() {
                if score > worst.score {
                    heap.pop();
                    heap.push(ScoredRow { score, rowid });
                }
            }
            if let Some(start) = keep_start {
                profile_sink::add(|profile| profile.assemble_ns += elapsed_ns(start));
            }
        }
    }
    if let Some(start) = scan_start {
        profile_sink::add(|profile| profile.fast_scan_ns += elapsed_ns(start));
    }
    if profiling {
        profile_sink::add(|profile| profile.rows = scanned);
    }

    let mut selected: Vec<ScoredRow> = heap.into_vec();
    if selected.is_empty() {
        return Ok(Some(Vec::new()));
    }
    // 按 rowid 升序去取元数据：选中的块散布在整表里，顺序访问时 SQLite 的页命中率
    // 明显更好（10 万块、8192 个块行上量到 ~20% 的差别）。结果顺序不受影响——
    // `rank_and_fold` 之后会按分数与同分键重新排。
    selected.sort_unstable_by_key(|row| row.rowid);
    let scores: HashMap<i64, f32> = selected
        .iter()
        .map(|row| (row.rowid, row.score))
        .collect();
    let ids = selected
        .iter()
        .map(|row| row.rowid.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let meta_start = profiling.then(Instant::now);
    let sql = "SELECT v.rowid, s.chunk_id, s.ordinal, s.capture_id, s.source_id, \
                s.source_revision_id, s.start_char, s.end_char, s.kind, s.coverage, c.day_key \
         FROM chunk_vectors v \
         JOIN chunk_spans s ON s.chunk_id = v.chunk_id \
         JOIN captures c ON c.id = s.capture_id \
         WHERE v.rowid IN (SELECT value FROM json_each(?1)) \
           AND v.generation = ?2 \
           AND (?3 = 1 OR c.state <> 'trashed') \
           AND (s.source_revision_id = '' \
                OR EXISTS (SELECT 1 FROM source_items si \
                           WHERE si.source_id = s.source_id \
                             AND si.current_revision_id = s.source_revision_id))";
    let mut statement = core.conn.prepare(sql)?;
    let mut rows = statement.query(params![
        format!("[{ids}]"),
        generation,
        i64::from(filters.include_trashed)
    ])?;
    let mut scored = Vec::with_capacity(selected.len());
    let mut surviving_chunks: HashSet<String> = HashSet::with_capacity(selected.len());
    while let Some(row) = {
        let step_start = profiling.then(Instant::now);
        let next = rows.next()?;
        if let (Some(start), true) = (step_start, next.is_some()) {
            profile_sink::add(|profile| profile.fast_meta_query_ns += elapsed_ns(start));
        }
        next
    } {
        let build_start = profiling.then(Instant::now);
        let rowid: i64 = row.get(0)?;
        let chunk_id: String = row.get(1)?;
        let Some(&score) = scores.get(&rowid) else {
            return Err(CoreError::CorruptedData {
                message: format!("快速扫描取回的块行 {rowid} 不在选中的 top-K 里"),
            });
        };
        surviving_chunks.insert(chunk_id.clone());
        let source_id: String = row.get(4)?;
        scored.push(SemanticCandidate {
            group_id: source_id.clone(),
            chunk_id,
            span_ordinal: row.get(2)?,
            // 只在「前五道键完全相同」的并列组里查块正文再算；见 `resolve_ties`。
            content_key: String::new(),
            score,
            capture_id: row.get(3)?,
            source_id,
            source_revision_id: row.get(5)?,
            kind: row.get(8)?,
            coverage: Coverage::from_wire(&row.get::<_, String>(9)?)
                .unwrap_or(Coverage::Unavailable),
            day_key: row.get(10)?,
            start_char: row.get(6)?,
            end_char: row.get(7)?,
        });
        if let Some(start) = build_start {
            profile_sink::add(|profile| profile.fast_meta_build_ns += elapsed_ns(start));
        }
    }
    if let Some(start) = meta_start {
        profile_sink::add(|profile| profile.fast_meta_ns += elapsed_ns(start));
    }
    // 选中的块有一条没通过后置过滤 ⇒ 可能漏掉了排在其后的有效候选，整条回退。
    if surviving_chunks.len() != selected.len() {
        return Ok(None);
    }
    Ok(Some(scored))
}

/// 直接按**存储字节**算余弦：不先解成 `Vec<f32>`，也不逐行重算查询向量的模长。
///
/// 流式扫描、全量参考路径、快速路径三处共用这一个函数，所以三处的分数逐位一致
/// （`streaming_scan_matches_the_load_all_reference` 钉住这一点）。
fn score_of(
    storage: VectorStorage,
    bytes: &[u8],
    query_vector: &[f32],
    query_norm: f32,
    row_dims: i64,
    chunk_id: &str,
) -> Result<f32> {
    let (dot, norm) = match storage {
        VectorStorage::F32 => {
            if !bytes.len().is_multiple_of(4) {
                return Err(CoreError::CorruptedData {
                    message: format!("向量字节数 {} 不是 4 的倍数", bytes.len()),
                });
            }
            let vector_dims = bytes.len() / 4;
            check_dims(vector_dims, row_dims, query_vector.len(), chunk_id)?;
            dot_norm_f32(bytes, query_vector)
        }
        VectorStorage::Int8 => {
            if bytes.len() < 4 {
                return Err(CoreError::CorruptedData {
                    message: format!("int8 向量字节数 {} 连一个 scale 都放不下", bytes.len()),
                });
            }
            let scale = f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
            if !scale.is_finite() || scale <= 0.0 {
                return Err(CoreError::CorruptedData {
                    message: format!("int8 向量的 scale 是 {scale}，不是正的有限数"),
                });
            }
            let vector_dims = bytes.len() - 4;
            check_dims(vector_dims, row_dims, query_vector.len(), chunk_id)?;
            dot_norm_int8(&bytes[4..], query_vector, scale)
        }
    };
    let denominator = query_norm.sqrt() * norm.sqrt();
    // 分母本身可能是 NaN/Inf（查询或向量含非有限值）：`NaN > 0.0` 为 false，
    // 若不先拦住，下面会静默返回 0.0，看起来像「两条向量正交」——这是最坏的一种
    // 撒谎。先在这里报错，再由下面的分数有限性检查兜住 Inf。
    if !denominator.is_finite() {
        return Err(CoreError::CorruptedData {
            message: format!(
                "向量{}的模长不是有限数（查询 {}、库内 {}）；查询或库里的向量含 NaN/Inf",
                if chunk_id.is_empty() {
                    "（快速路径）".to_owned()
                } else {
                    format!(" {chunk_id} ")
                },
                query_norm,
                norm,
            ),
        });
    }
    let score = if denominator > 0.0 {
        dot / denominator
    } else {
        0.0
    };
    // 查询向量或库里的字节都可能含 NaN/Inf（旧库、坏模型输出、手工写坏的行）。
    // 分数一旦不是有限数，`partial_cmp` 的全序就断了（NaN 与任何数比较都不等），
    // `sort_by` 会乱序甚至 panic；而且 NaN 会被当成「最高分」蒙混进结果。
    // 这里统一报 CorruptedData，让调用方如实失败，不把 NaN 当分数传下去。
    if !score.is_finite() {
        return Err(CoreError::CorruptedData {
            message: format!(
                "向量{}算出的余弦不是有限数（{score}）；查询或库里的向量含 NaN/Inf",
                if chunk_id.is_empty() {
                    "（快速路径）".to_owned()
                } else {
                    format!(" {chunk_id} ")
                }
            ),
        });
    }
    Ok(score)
}

/// 点积 + 自身模长平方，直接从 BLOB 字节读 f32。
///
/// x86_64 上运行时探测 AVX2（8 路），否则走四个独立累加器的标量版。两条路径
/// **共用同一个实现**（同一次查询里不会一半 AVX2 一半标量），所以快速路径与全量
/// 参考路径的分数仍然逐位一致。
fn dot_norm_f32(bytes: &[u8], query: &[f32]) -> (f32, f32) {
    #[cfg(target_arch = "x86_64")]
    {
        if std::is_x86_feature_detected!("avx2") {
            // SAFETY: 上面刚在运行时探测过 AVX2。
            return unsafe { dot_norm_f32_avx2(bytes, query) };
        }
    }
    dot_norm_f32_scalar(bytes, query)
}

/// AVX2 版：一次 8 个 f32，非对齐加载（BLOB 指针对齐没有承诺）。
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn dot_norm_f32_avx2(bytes: &[u8], query: &[f32]) -> (f32, f32) {
    use std::arch::x86_64::*;
    debug_assert_eq!(bytes.len(), query.len() * 4);
    let len = query.len();
    let mut dot = _mm256_setzero_ps();
    let mut norm = _mm256_setzero_ps();
    let values = bytes.as_ptr();
    let queries = query.as_ptr();
    let mut index = 0_usize;
    while index + 8 <= len {
        let value = _mm256_loadu_ps(values.add(index * 4).cast());
        let query8 = _mm256_loadu_ps(queries.add(index));
        dot = _mm256_add_ps(dot, _mm256_mul_ps(value, query8));
        norm = _mm256_add_ps(norm, _mm256_mul_ps(value, value));
        index += 8;
    }
    let mut tail_dot = 0_f32;
    let mut tail_norm = 0_f32;
    while index < len {
        // x86_64 是小端。
        let value = f32::from_le_bytes([
            bytes[index * 4],
            bytes[index * 4 + 1],
            bytes[index * 4 + 2],
            bytes[index * 4 + 3],
        ]);
        tail_dot += value * query[index];
        tail_norm += value * value;
        index += 1;
    }
    (
        horizontal_sum_avx2(dot) + tail_dot,
        horizontal_sum_avx2(norm) + tail_norm,
    )
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn horizontal_sum_avx2(value: std::arch::x86_64::__m256) -> f32 {
    use std::arch::x86_64::*;
    let low = _mm256_castps256_ps128(value);
    let high = _mm256_extractf128_ps(value, 1);
    let mut sum = _mm_add_ps(low, high);
    sum = _mm_add_ps(sum, _mm_movehl_ps(sum, sum));
    sum = _mm_add_ss(sum, _mm_shuffle_ps(sum, sum, 0x55));
    _mm_cvtss_f32(sum)
}

/// 标量版（非 x86_64 或无 AVX2 时）。四个累加器拆开依赖链。
fn dot_norm_f32_scalar(bytes: &[u8], query: &[f32]) -> (f32, f32) {
    debug_assert_eq!(bytes.len(), query.len() * 4);
    let mut dot = [0_f32; 4];
    let mut norm = [0_f32; 4];
    let mut index = 0_usize;
    let mut cursor = bytes.as_ptr();
    #[cfg(target_endian = "little")]
    while index + 4 <= query.len() {
        // SAFETY: `index + 4 <= query.len()` 且 bytes.len() == query.len()*4，
        // 因此四次 4 字节读都在 BLOB 范围内；`read_unaligned` 不要求对齐。
        let values = unsafe {
            [
                std::ptr::read_unaligned(cursor.cast::<f32>()),
                std::ptr::read_unaligned(cursor.add(4).cast::<f32>()),
                std::ptr::read_unaligned(cursor.add(8).cast::<f32>()),
                std::ptr::read_unaligned(cursor.add(12).cast::<f32>()),
            ]
        };
        for lane in 0..4 {
            let value = values[lane];
            let query = query[index + lane];
            dot[lane] += value * query;
            norm[lane] += value * value;
        }
        index += 4;
        cursor = unsafe { cursor.add(16) };
    }
    #[cfg(not(target_endian = "little"))]
    while index + 4 <= query.len() {
        for lane in 0..4 {
            let chunk = &bytes[(index + lane) * 4..(index + lane) * 4 + 4];
            let value = f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
            let query = query[index + lane];
            dot[lane] += value * query;
            norm[lane] += value * value;
        }
        index += 4;
    }
    // 尾数（维度不是 4 的倍数；BGE 是 512，正常走不到）。
    while index < query.len() {
        let value = f32::from_le_bytes([
            bytes[index * 4],
            bytes[index * 4 + 1],
            bytes[index * 4 + 2],
            bytes[index * 4 + 3],
        ]);
        dot[0] += value * query[index];
        norm[0] += value * value;
        index += 1;
    }
    (
        (dot[0] + dot[1]) + (dot[2] + dot[3]),
        (norm[0] + norm[1]) + (norm[2] + norm[3]),
    )
}

/// int8 版：`value = q * scale`。x86_64 上用 AVX2 把 8 个 i8 扩成 f32 再乘加。
fn dot_norm_int8(bytes: &[u8], query: &[f32], scale: f32) -> (f32, f32) {
    #[cfg(target_arch = "x86_64")]
    {
        if std::is_x86_feature_detected!("avx2") {
            // SAFETY: 上面刚在运行时探测过 AVX2。
            return unsafe { dot_norm_int8_avx2(bytes, query, scale) };
        }
    }
    dot_norm_int8_scalar(bytes, query, scale)
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn dot_norm_int8_avx2(bytes: &[u8], query: &[f32], scale: f32) -> (f32, f32) {
    use std::arch::x86_64::*;
    debug_assert_eq!(bytes.len(), query.len());
    let len = query.len();
    let mut dot = _mm256_setzero_ps();
    let mut norm = _mm256_setzero_ps();
    let scale8 = _mm256_set1_ps(scale);
    let values = bytes.as_ptr();
    let queries = query.as_ptr();
    let mut index = 0_usize;
    while index + 8 <= len {
        let raw = _mm_loadl_epi64(values.add(index).cast());
        let ints = _mm256_cvtepi8_epi32(raw);
        let value = _mm256_mul_ps(_mm256_cvtepi32_ps(ints), scale8);
        let query8 = _mm256_loadu_ps(queries.add(index));
        dot = _mm256_add_ps(dot, _mm256_mul_ps(value, query8));
        norm = _mm256_add_ps(norm, _mm256_mul_ps(value, value));
        index += 8;
    }
    let mut tail_dot = 0_f32;
    let mut tail_norm = 0_f32;
    while index < len {
        let value = f32::from(bytes[index] as i8) * scale;
        tail_dot += value * query[index];
        tail_norm += value * value;
        index += 1;
    }
    (
        horizontal_sum_avx2(dot) + tail_dot,
        horizontal_sum_avx2(norm) + tail_norm,
    )
}

fn dot_norm_int8_scalar(bytes: &[u8], query: &[f32], scale: f32) -> (f32, f32) {
    debug_assert_eq!(bytes.len(), query.len());
    let mut dot = [0_f32; 4];
    let mut norm = [0_f32; 4];
    let mut index = 0_usize;
    while index + 4 <= query.len() {
        for lane in 0..4 {
            let value = f32::from(bytes[index + lane] as i8) * scale;
            let query = query[index + lane];
            dot[lane] += value * query;
            norm[lane] += value * value;
        }
        index += 4;
    }
    while index < query.len() {
        let value = f32::from(bytes[index] as i8) * scale;
        dot[0] += value * query[index];
        norm[0] += value * value;
        index += 1;
    }
    (
        (dot[0] + dot[1]) + (dot[2] + dot[3]),
        (norm[0] + norm[1]) + (norm[2] + norm[3]),
    )
}

/// 与 `candidate_of` 里的两条维度校验同口径：先对行上的 `dims`，再对查询向量。
fn check_dims(
    vector_dims: usize,
    row_dims: i64,
    query_dims: usize,
    chunk_id: &str,
) -> Result<()> {
    let row_dims = usize::try_from(row_dims).unwrap_or(usize::MAX);
    if vector_dims != row_dims {
        return Err(CoreError::CorruptedData {
            message: format!("向量字节解出 {vector_dims} 维，但行上记的是 {row_dims} 维"),
        });
    }
    if vector_dims != query_dims {
        let where_ = if chunk_id.is_empty() {
            String::new()
        } else {
            format!("块 {chunk_id} 的")
        };
        return Err(CoreError::CorruptedData {
            message: format!(
                "{where_}向量是 {vector_dims} 维、行上记的是 {row_dims} 维，查询向量是 \
                 {query_dims} 维：两者不是同一个向量空间，不做余弦"
            ),
        });
    }
    Ok(())
}

/// 从当前行取出 13 列。流式与全量两条路径共用，保证「取出来的东西」完全一样。
fn raw_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RawRow> {
    Ok(RawRow {
        vector: row.get(0)?,
        dims: row.get(1)?,
        storage: row.get(2)?,
        chunk_id: row.get(3)?,
        span_ordinal: row.get(4)?,
        capture_id: row.get(5)?,
        source_id: row.get(6)?,
        source_revision_id: row.get(7)?,
        start_char: row.get(8)?,
        end_char: row.get(9)?,
        kind: row.get(10)?,
        coverage: row.get(11)?,
        day_key: row.get(12)?,
    })
}

/// 一行候选的原始数据。流式路径逐行构造、算完即弃；测试用的全量路径先
/// `collect` 成 `Vec<RawRow>` 再算，用来钉住「两条路径逐字节一致」。
struct RawRow {
    vector: Vec<u8>,
    dims: i64,
    storage: String,
    chunk_id: String,
    span_ordinal: i64,
    capture_id: String,
    source_id: String,
    source_revision_id: String,
    start_char: i64,
    end_char: i64,
    kind: String,
    coverage: String,
    day_key: String,
}

/// 把一行的字节解码、校验、算余弦，变成一条候选。两条路径共用这一段，
/// 唯一的差别是「行是一次一行地取，还是先收成一堆」。
fn candidate_of(
    row: RawRow,
    query_vector: &[f32],
    query_norm: f32,
    profiling: bool,
) -> Result<SemanticCandidate> {
    // **按行上的 `storage` 分派解码，不猜。** 整代口径已经在进计算前核对过
    // （`caliber_mismatch`），所以正常走到这里的行都是当前格式；这里仍然逐行
    // 解析，是为了让「历史遗留的混合代次」在报错时说得清是哪一行、哪种格式，
    // 而不是拿一个默认格式解出一串垃圾分数。
    //
    // 解码、维度校验与点积是同一段（`score_of`），**不先分配 `Vec<f32>`**：与快速路径
    // 共用这个函数，三条路径的分数才会逐位一致。
    //
    // 兜底（正常走不到）：整代口径已经在进计算前核对过，所以这里的每一行**应当**
    // 与查询向量同维。真的不同维时**如实报错**，绝不静默 `zip` 到短的那一段——那会
    // 给出一个看起来正常的分数。口径写在
    // `docs/architecture/m2-向量索引与换代.md` 的「消费契约」里。
    let storage =
        VectorStorage::from_wire(&row.storage).ok_or_else(|| CoreError::CorruptedData {
            message: format!("不认识的向量存储格式：{}", row.storage),
        })?;
    let cosine_start = profiling.then(Instant::now);
    let score = score_of(
        storage,
        &row.vector,
        query_vector,
        query_norm,
        row.dims,
        &row.chunk_id,
    )?;
    if let Some(start) = cosine_start {
        profile_sink::add(|profile| profile.cosine_ns += elapsed_ns(start));
    }
    let assemble_start = profiling.then(Instant::now);
    let candidate = SemanticCandidate {
        group_id: row.source_id.clone(),
        chunk_id: row.chunk_id,
        span_ordinal: row.span_ordinal,
        // 只在「前五道键完全相同」的并列组里查块正文再算；见 `resolve_ties`。
        content_key: String::new(),
        score,
        capture_id: row.capture_id,
        source_id: row.source_id,
        source_revision_id: row.source_revision_id,
        kind: row.kind,
        coverage: Coverage::from_wire(&row.coverage).unwrap_or(Coverage::Unavailable),
        day_key: row.day_key,
        start_char: row.start_char,
        end_char: row.end_char,
    };
    if let Some(start) = assemble_start {
        profile_sink::add(|profile| profile.assemble_ns += elapsed_ns(start));
    }
    Ok(candidate)
}

/// 排序 + 按篇折叠。流式、全量参考与快速路径共用，保证结果只差在取行方式。
///
/// 排序走**七道全序**（issue #57）：`score DESC → day_key DESC → start_char →
/// end_char → span_ordinal → content_key → chunk_id`。分两段实现：
/// [`position_order`] 先排前五道；[`resolve_ties`] 只对「前五道完全相同」的并列组
/// 回表读块正文、算 `content_key`，再在组内按 [`rank_order`] 收尾。这样绝大多数候选
/// 不必为排序读一遍正文，而同分时的先后仍然只由内容决定（跨库稳定）。
///
/// 快速路径（#90）选出的 top-K 也走这里：被裁掉的块本来就不会出现在结果里，
/// 裁剪与并列组解析不冲突；保留范围内的名次仍然是逐条精算的。
fn rank_and_fold(
    core: &Core,
    mut scored: Vec<SemanticCandidate>,
    profiling: bool,
) -> Result<Vec<SemanticCandidate>> {
    // 分数从高到低；同分按日期近到远，再按**真正由内容决定**的键：篇内区间与段序号。
    //
    // 为什么同分时**不能**先比 `chunk_id`（issue #57）：`chunk_id` 的哈希输入里有
    // 篇标识（`src_`/`cap_` 前缀的 v7 UUID），而篇标识每个库都不一样。于是**同一份
    // 内容重新建一遍库就会换一批 chunk_id**，两个同分块的相对大小也就跟着抛硬币——
    // 同一篇的一块 A 与一块 B 得分一样时，谁当证据在两次建库之间会变，用户看到的
    // 摘录跟着变。`start_char` / `end_char` 是篇内区间（块切法的函数），
    // `span_ordinal` 是块内位置，两者都只由内容与切块规则决定，跨库稳定。
    //
    // 但只靠（日期、篇内区间、段序号）还不够：**不同篇**的两段完全可以有相同的
    // （日期、起点、终点、段序号）——例如同一天导入的两份等长文件，各自被同一条
    // 规则切成两块。这时若直接落到 `chunk_id`，又会回到「每个库不一样」的老问题；
    // 而两条候选的正文**不一定**相同（它们是不同的篇），摘录会真的换人。所以倒数
    // 第二道键是 `content_key`（块正文哈希）：只由内容决定，跨库稳定。`chunk_id`
    // 只留作最后的**全序兜底**：只有正文也完全相同才会走到它——那时两条命中的
    // 摘录已经一模一样，顺序不影响用户看到什么。
    //
    // 同分规则的契约写在 `docs/architecture/m2-向量索引与换代.md` 的「消费契约」里，
    // 「排序并列必须全序」写在 `docs/architecture/m2-检索会话.md`。
    let sort_start = profiling.then(Instant::now);
    scored.sort_by(position_order);
    resolve_ties(core, &mut scored)?;
    if let Some(start) = sort_start {
        profile_sink::add(|profile| profile.sort_ns += elapsed_ns(start));
    }

    // 折叠：同一篇只留第一条（也就是排序最靠前的那一块）。
    //
    // `HashSet` 只做「这个 group_id 见过没有」的成员查询，**从不迭代**；输出顺序完全
    // 来自上面已经是全序的 `scored`，所以进程间随机的 `RandomState` 影响不到结果。
    let fold_start = profiling.then(Instant::now);
    let mut seen: HashSet<String> = HashSet::new();
    let folded: Vec<SemanticCandidate> = scored
        .into_iter()
        .filter(|candidate| seen.insert(candidate.group_id.clone()))
        .collect();
    if let Some(start) = fold_start {
        profile_sink::add(|profile| profile.fold_ns += elapsed_ns(start));
    }
    Ok(folded)
}

/// 语义候选的**前五道**排序键：`score DESC → day_key DESC → start_char →
/// end_char → span_ordinal`。全部由分数与内容位置决定，跨库稳定。
///
/// 它们之后才轮到 `content_key` 与 `chunk_id`（见 [`rank_order`]）。这样分两段是为了
/// 性能：`content_key` 要读块正文、算 sha256，10 万块全算一遍不值得；只对前五道键
/// **完全相同**的并列组才去取正文（见 [`resolve_ties`]）。
fn position_order(left: &SemanticCandidate, right: &SemanticCandidate) -> std::cmp::Ordering {
    // `total_cmp` 而不是 `partial_cmp(...).unwrap_or(Equal)`：NaN 在偏序里与任何数
    // 都「不可比」，退回 Equal 会破坏全序（比较不传递），`sort_by` 因此可能乱序甚至
    // panic。`total_cmp` 给 NaN 一个确定位置（排在 +Inf 之后），排序始终是全序。
    // 正常路径上 `score_of` 已经拒绝非有限分数；这里是排序层自己的防线。
    right
        .score
        .total_cmp(&left.score)
        .then_with(|| right.day_key.cmp(&left.day_key))
        .then_with(|| left.start_char.cmp(&right.start_char))
        .then_with(|| left.end_char.cmp(&right.end_char))
        .then_with(|| left.span_ordinal.cmp(&right.span_ordinal))
}

/// 语义候选的全序：`score DESC → day_key DESC → start_char → end_char →
/// span_ordinal → content_key → chunk_id`。
///
/// `span_ordinal` / `content_key` / `chunk_id` 这三道的顺序是刻意分开的：
/// 段序号是内容决定的定位，`content_key` 是内容决定的正文哈希，`chunk_id` 只是
/// 「同一个库内再兜底」——它不是内容的函数（哈希输入含篇标识 UUID），所以只允许在
/// 正文也完全相同时生效。判据与来历见
/// `docs/architecture/m2-向量索引与换代.md` 消费契约第 6 条。
fn rank_order(left: &SemanticCandidate, right: &SemanticCandidate) -> std::cmp::Ordering {
    position_order(left, right)
        .then_with(|| left.content_key.cmp(&right.content_key))
        .then_with(|| left.chunk_id.cmp(&right.chunk_id))
}

/// 给「前五道键完全相同」的并列组补上 `content_key`，再在组内按 [`rank_order`] 排。
///
/// 只有并列组才去读块正文：绝大多数候选在 [`position_order`] 上已经分出了先后，
/// 不值得为它们各读一遍正文。并列组通常很小（同一位置的两块），所以这条路径既稳又便宜。
///
/// 与快速路径的 top-K 裁剪的关系：裁剪发生在扫描阶段（只保留分数最高的 K 个块），
/// 被裁掉的块本来就不会出现在任何结果里；这里只给**留下的**候选补全序，两者不冲突。
fn resolve_ties(core: &Core, scored: &mut [SemanticCandidate]) -> Result<()> {
    let mut start = 0;
    while start < scored.len() {
        let mut end = start + 1;
        while end < scored.len()
            && position_order(&scored[start], &scored[end]) == std::cmp::Ordering::Equal
        {
            end += 1;
        }
        if end - start > 1 {
            let mut chunk_ids: Vec<&str> = scored[start..end]
                .iter()
                .map(|candidate| candidate.chunk_id.as_str())
                .collect();
            chunk_ids.sort_unstable();
            chunk_ids.dedup();
            // **不能拼成 `IN (?, ?, ...)`**：SQLite 一条语句的变量数有上限（默认
            // 32766），一个超大并列组（同分、同位置、同段序号）会直接报
            // `too many SQL variables`，把整条查询打成错误。照 `delete_stale_chunks`
            // 的做法先把 id 放进临时表，再用主键 join 取正文：变量数与并列组大小无关。
            core.conn.execute_batch(
                "DROP TABLE IF EXISTS temp.semantic_tie_chunks; \
                 CREATE TEMP TABLE semantic_tie_chunks (chunk_id TEXT PRIMARY KEY);",
            )?;
            {
                let tx = core.conn.unchecked_transaction()?;
                {
                    let mut insert = tx.prepare(
                        "INSERT OR IGNORE INTO semantic_tie_chunks (chunk_id) VALUES (?1)",
                    )?;
                    for chunk_id in &chunk_ids {
                        insert.execute(params![chunk_id])?;
                    }
                }
                tx.commit()?;
            }
            let mut statement = core.conn.prepare(
                "SELECT t.chunk_id, t.text FROM text_chunks t \
                 JOIN semantic_tie_chunks tie ON tie.chunk_id = t.chunk_id",
            )?;
            let rows = statement.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            let mut texts: std::collections::HashMap<String, String> =
                std::collections::HashMap::with_capacity(end - start);
            for row in rows {
                let (chunk_id, text) = row?;
                texts.insert(chunk_id, text);
            }
            drop(statement);
            core.conn
                .execute_batch("DROP TABLE temp.semantic_tie_chunks;")?;
            for candidate in &mut scored[start..end] {
                let text = texts.get(&candidate.chunk_id).ok_or_else(|| {
                    CoreError::CorruptedData {
                        message: format!(
                            "候选块的正文不见了：chunk_id = {}（块表与向量表不一致）",
                            candidate.chunk_id
                        ),
                    }
                })?;
                candidate.content_key = crate::support::content_hash(text);
            }
            scored[start..end].sort_by(rank_order);
        }
        start = end;
    }
    Ok(())
}

/// 融合/量测用：这一篇的**证据正文**的稳定哈希（与 `materialize` 取摘录用的是同一段
/// 正文：来源篇 = span 基准正文，记录文字 = `draft_text`）。
///
/// 混合检索的同分兜底要一个**只由内容决定**的键（#57 的教训：`chunk_id` / `source_id`
/// 里有每库不同的 v7 UUID）。正文已经不在了（派生内容被重提取删掉）时返回 `None`，
/// 那一篇本来也materialize 不出来。
pub(crate) fn content_key(core: &Core, candidate: &SemanticCandidate) -> Result<Option<String>> {
    let text = if candidate.source_revision_id.is_empty() {
        core.conn
            .query_row(
                "SELECT draft_text FROM captures WHERE id = ?1",
                params![candidate.capture_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
    } else {
        chunks::source_piece_text(&core.conn, &candidate.source_id, &candidate.source_revision_id)?
            .map(|piece| piece.text)
    };
    Ok(text.map(|value| crate::support::content_hash(&value)))
}

/// 把一页候选变成命中。整段正文只在这一步取，按页取。
///
/// 摘录与定位取**该篇自己那一段**（`start_char` / `end_char`），不是整块拼接文本。
pub(crate) fn materialize(core: &Core, page: &[SemanticCandidate]) -> Result<Vec<SearchHit>> {
    let mut hits = Vec::with_capacity(page.len());
    for candidate in page {
        let is_capture = candidate.source_revision_id.is_empty();
        let (piece_text, locator, title) = if is_capture {
            let text: String = core.conn.query_row(
                "SELECT draft_text FROM captures WHERE id = ?1",
                params![candidate.capture_id],
                |row| row.get(0),
            )?;
            (text, None, None)
        } else {
            let Some(piece) =
                chunks::source_piece_text(&core.conn, &candidate.source_id, &candidate.source_revision_id)?
            else {
                // 派生内容可能已经被重提取删掉（块还在）。这一篇没有正文可取，
                // 如实跳过，而不是拿整块拼接文本顶上去。
                continue;
            };
            let locator = locator_for_span(
                &piece,
                candidate.start_char,
                candidate.end_char,
                &candidate.source_revision_id,
            );
            let title = source_title(core, &candidate.source_revision_id)?;
            (piece.text, Some(locator), title)
        };

        let snippet = snippet_of_piece(&piece_text, candidate.start_char, candidate.end_char);
        hits.push(SearchHit {
            hit_id: hit_id_of(candidate),
            group_id: candidate.group_id.clone(),
            source_kind: SourceKind::from_wire(&candidate.kind),
            matched_by: vec![MatchedBy::Semantic],
            coverage: candidate.coverage,
            source_id: if is_capture {
                None
            } else {
                Some(candidate.source_id.clone())
            },
            revision_id: if is_capture {
                None
            } else {
                Some(candidate.source_revision_id.clone())
            },
            day_key: Some(candidate.day_key.clone()),
            title,
            snippet,
            // 语义命中没有「哪几个字对上」这回事：不给假高亮。
            highlights: Vec::new(),
            locator,
        });
    }
    Ok(hits)
}

/// 命中的稳定标识：按**篇**给，不按证据块——同一篇在不同查询里证据块可以不同，
/// 但用户看到的同一条命中不该换 id。与关键词那一路对齐：片段 `source_id`、
/// 记录文字 `cap_<capture_id>`。
fn hit_id_of(candidate: &SemanticCandidate) -> String {
    if candidate.source_revision_id.is_empty() {
        format!("cap_{}", candidate.capture_id)
    } else {
        candidate.source_id.clone()
    }
}

/// 这一截正文对应的原件定位：找**包含区间起点**的那个片段，用它自己的定位（页码 /
/// 时间码 / 图像区域）。找不到就退回一个文本区间定位，不编。
fn locator_for_span(
    piece: &chunks::PieceText,
    start_char: i64,
    end_char: i64,
    source_revision_id: &str,
) -> SourceLocator {
    let start = usize::try_from(start_char).unwrap_or(0);
    if let Some((_, _, locator)) = piece
        .segments
        .iter()
        .find(|(segment_start, segment_end, _)| *segment_start <= start && start < *segment_end)
    {
        return locator.clone();
    }
    SourceLocator::text_range(source_revision_id, start_char, end_char)
}

/// 来源修订的原件名（有资产时）。与关键词那一路同一个口径。
fn source_title(core: &Core, source_revision_id: &str) -> Result<Option<String>> {
    Ok(core
        .conn
        .query_row(
            "SELECT a.original_name FROM source_revisions sr \
             LEFT JOIN assets a ON a.id = sr.asset_id WHERE sr.revision_id = ?1",
            params![source_revision_id],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()?
        .flatten())
}

/// 摘录：该篇自己那一段（span 区间）截断，不做假高亮。
fn snippet_of_piece(piece_text: &str, start_char: i64, end_char: i64) -> Option<String> {
    let characters: Vec<char> = piece_text.chars().collect();
    let start = usize::try_from(start_char).unwrap_or(0).min(characters.len());
    let end = usize::try_from(end_char).unwrap_or(start).min(characters.len());
    if start >= end {
        return None;
    }
    let slice = &characters[start..end];
    let mut snippet = String::new();
    if start > 0 {
        snippet.push('…');
    }
    if slice.len() > SEMANTIC_SNIPPET_CHARS {
        snippet.extend(&slice[..SEMANTIC_SNIPPET_CHARS]);
        snippet.push('…');
    } else {
        snippet.extend(slice);
    }
    Some(snippet)
}

/// 量测用：按篇折叠后的 `(篇标识, 余弦分数)`，按分数从高到低。
///
/// 产品路径**不读**它：契约的 `SearchHit` 上没有 score 字段（等 #51 的混合排序
/// 再一起定）。它是给质量集量测用的——「`no_answer` 类语义路会不会乱报」、
/// 「要不要设一个分数阈值」这类问题只能看分数。
///
/// 与 `search.start` 的语义路**共用同一段候选与折叠逻辑**，所以量到的分数与线上
/// 排序完全一致，不是另一套实现。
pub fn ranking(
    core: &mut Core,
    query: &str,
    filters: &SearchFilters,
) -> Result<Vec<(String, f32)>> {
    ranking_with_mode(core, query, filters, RowMode::Streaming)
}

/// 仅供测试：全量载入路径的同一份排名，用来与流式路径逐字节对照。
#[doc(hidden)]
pub(crate) fn ranking_load_all_for_test(
    core: &mut Core,
    query: &str,
    filters: &SearchFilters,
) -> Result<Vec<(String, f32)>> {
    ranking_with_mode(core, query, filters, RowMode::LoadAll)
}

fn ranking_with_mode(
    core: &mut Core,
    query: &str,
    filters: &SearchFilters,
    mode: RowMode,
) -> Result<Vec<(String, f32)>> {
    if active_generation(core)?.is_none() {
        return Ok(Vec::new());
    }
    let embedder = core.embedder()?;
    // 口径不一致时给**空排名**：与产品路径「语义这一路不作数」同一口径，
    // 而不是把跨空间的分数当成量测结果报出去。
    match candidates_impl(core, &*embedder, query, filters, mode)?.candidates {
        SemanticCandidates::ForeignSpace(_) => Ok(Vec::new()),
        SemanticCandidates::Scored(candidates) => Ok(candidates
            .into_iter()
            .map(|candidate| (candidate.group_id, candidate.score))
            .collect()),
    }
}

/// 生效代次那一批向量的**整代口径**：模型版本、维度、存储格式的去重值。
///
/// 一次 `SELECT DISTINCT model_version, dims, storage` 取全，不再为每一项各扫一遍
/// 整代向量。三个字段各自去重后**按序**保存，便于状态与日志稳定。
///
/// 为什么不是「读一行就完事」：`chunk_vectors` 的主键是 `(chunk_id, generation)`，
/// 一个代次里理论上可以混进不止一种口径（本片修复前的「A 写一半 → 换成 B 续跑」
/// 就会产出这种代次，历史库里也可能留下）。随便挑一行报出去是**撒谎**——它既不代表
/// 整代，也掩盖了「这一代不是同一个向量空间」这件事（检索路径从 #80 起整路不作数）。
///
/// 空集 = 没有生效代次，或那一代一行向量都没有：没有口径可报。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ActiveCaliber {
    /// 整代出现过的 `model_version`（去重、字典序）。
    pub versions: Vec<String>,
    /// 整代出现过的 `dims`（去重、升序）。
    pub dims: Vec<i64>,
    /// 整代出现过的 `storage`（去重、字典序）。
    pub storages: Vec<String>,
}

impl ActiveCaliber {
    /// 有效代次里有没有向量可查。
    pub(crate) fn is_empty(&self) -> bool {
        self.versions.is_empty() && self.dims.is_empty() && self.storages.is_empty()
    }
}

pub(crate) fn active_caliber(core: &Core) -> Result<ActiveCaliber> {
    let Some(generation) = active_generation(core)? else {
        return Ok(ActiveCaliber::default());
    };
    let mut statement = core.conn.prepare(
        "SELECT DISTINCT model_version, dims, storage FROM chunk_vectors \
         WHERE generation = ?1 ORDER BY model_version, dims, storage",
    )?;
    let rows = statement.query_map(params![generation], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    let mut caliber = ActiveCaliber::default();
    for row in rows {
        let (version, dims, storage) = row?;
        caliber.versions.push(version);
        caliber.dims.push(dims);
        caliber.storages.push(storage);
    }
    caliber.versions.sort_unstable();
    caliber.versions.dedup();
    caliber.dims.sort_unstable();
    caliber.dims.dedup();
    caliber.storages.sort_unstable();
    caliber.storages.dedup();
    Ok(caliber)
}

/// 生效代次与**当前嵌入器**不是同一个向量空间时的如实说法（检索路径唯一的判据来源）。
///
/// 什么时候会不一样（都是真实会发生的）：
/// - `Single(旧模型)`：用户换了模型配置、还没重建索引。**最常见的真实情形**；
/// - `Mixed`：历史遗留的混合代次（#58 修复前激活的）；
/// - 三项里任意一项不同：`model_version`（模型文件变了）/ `dims`（维度变了）/
///   `storage`（存储格式变了）。三者**全同**才算同一个向量空间。
///
/// 这样一批向量的余弦相似度**全是垃圾但看起来正常**（有排序、有分数），所以判据必须
/// 在进入计算之前生效：`candidates` 一发现不一致就整路不作数。
pub(crate) struct CaliberMismatch {
    /// 生效代次里出现的模型版本（去重、字典序）：整代是旧模型时一个，混合代次多个。
    pub versions: Vec<String>,
    /// 生效代次里出现的维度（去重、升序）。
    pub dims: Vec<i64>,
    /// 生效代次里出现的存储格式（去重、字典序）。
    pub storages: Vec<String>,
    /// 与当前嵌入器口径不一致的向量行数。
    pub foreign_rows: i64,
    /// 当前嵌入器的三个口径字段（消息里的另一半）。
    pub current_version: String,
    pub current_dims: i64,
    pub current_storage: String,
}

impl CaliberMismatch {
    /// 给人看的、**可执行**的一句话。纯语义与混合共用同一套措辞（判据只有一处）。
    ///
    /// 必须说清三件事：生效的是什么、与当前什么不一样、怎么恢复（重建）。
    pub(crate) fn reason(&self) -> String {
        let stored = match self.versions.as_slice() {
            [] => "（一代里一行向量都没有）".to_owned(),
            [only] => format!("模型 {only}"),
            many => format!(
                "历史遗留的混合代次（{} 种模型版本：{}）",
                many.len(),
                many.join(" / ")
            ),
        };
        format!(
            "当前生效的向量代次是用另一个模型（或另一套口径）建的：{stored}\
             （维度 {}、存储 {}），其中 {} 行与当前嵌入器 {}（维度 {}、存储 {}）\
             口径不一致，两边不是同一个向量空间；跨空间的余弦相似度不作数，\
             所以语义这一路不参与。\
             重建语义索引（build_semantic_index）即可按当前模型整代重算并恢复",
            join_numbers(&self.dims),
            self.storages.join(" / "),
            self.foreign_rows,
            self.current_version,
            self.current_dims,
            self.current_storage,
        )
    }
}

/// `[512, 384]` → `"512 / 384"`（消息里用的维度串）。
fn join_numbers(values: &[i64]) -> String {
    values
        .iter()
        .map(i64::to_string)
        .collect::<Vec<_>>()
        .join(" / ")
}

/// 生效代次与当前嵌入器的口径对照：`Some` = **不是同一个向量空间，语义这一路不作数**。
///
/// `None` 有两种情形，都不算跨空间：没有生效代次，或生效代次里一行向量都没有
/// （没有可算的东西）——那两条路由各自既有的「没有生效代次 / 没有块」分支处理。
///
/// 判据是**整代**的：一条与当前口径不一致就整路不作数。不做「挑出坏行、留下好行」
/// 这种混合——整代里出现另一种口径本身就说明这一代不是当前模型建的。
pub(crate) fn caliber_mismatch(
    core: &Core,
    embedder: &dyn Embedder,
) -> Result<Option<CaliberMismatch>> {
    let Some(generation) = active_generation(core)? else {
        return Ok(None);
    };
    let caliber = Caliber::of(embedder, core.vector_storage);
    let foreign_rows = foreign_row_count(core, generation, &caliber)?;
    if foreign_rows == 0 {
        return Ok(None);
    }
    let active = active_caliber(core)?;
    Ok(Some(CaliberMismatch {
        versions: active.versions,
        dims: active.dims,
        storages: active.storages,
        foreign_rows,
        current_version: caliber.model_version.to_owned(),
        current_dims: caliber.dims,
        current_storage: caliber.storage.to_owned(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 一条只关心排序键的候选。除 `chunk_id` / `content_key` / `score` 外都用同一组值，
    /// 这样测试能精确钉住「哪一道键在决胜」。
    fn candidate(chunk_id: &str, content_key: &str, score: f32) -> SemanticCandidate {
        SemanticCandidate {
            group_id: "grp".to_owned(),
            chunk_id: chunk_id.to_owned(),
            span_ordinal: 0,
            content_key: content_key.to_owned(),
            score,
            capture_id: "cap".to_owned(),
            source_id: "src".to_owned(),
            source_revision_id: "rev".to_owned(),
            kind: "text".to_owned(),
            coverage: Coverage::Complete,
            day_key: "2026-09-20".to_owned(),
            start_char: 0,
            end_char: 390,
        }
    }

    /// issue #57 的回归（纯排序层，不建库、不依赖随机 UUID）：
    /// 两条候选的（分数、日期、篇内区间、段序号）全部相同、**正文不同**时，
    /// 顺序必须由 `content_key` 决定；把 `chunk_id` 故意排成相反顺序也不能翻盘。
    ///
    /// 这条测试就是「去掉 `content_key` 这一道键就会红」的那一条：删掉
    /// `rank_order` 里的 `.then_with(|| left.content_key.cmp(&right.content_key))`，
    /// 下面的左/右顺序会跟着 `chunk_id` 翻过来。
    #[test]
    fn equal_position_different_content_is_ordered_by_content_not_chunk_id() {
        // 正文 A 的块 id 故意大于正文 B（旧逻辑会把 B 排前面）。
        let content_a = candidate("chunk_zzz", "aaaa", 1.0);
        let content_b = candidate("chunk_aaa", "bbbb", 1.0);
        assert_eq!(
            rank_order(&content_a, &content_b),
            std::cmp::Ordering::Less,
            "同分同区间时必须先比正文哈希（content_key），不能先比库内 chunk_id"
        );
        assert_eq!(
            rank_order(&content_b, &content_a),
            std::cmp::Ordering::Greater
        );

        // 反过来：正文相同（content_key 相同）时才允许 `chunk_id` 兜底。
        let same_text_low = candidate("chunk_aaa", "same", 1.0);
        let same_text_high = candidate("chunk_zzz", "same", 1.0);
        assert_eq!(
            rank_order(&same_text_low, &same_text_high),
            std::cmp::Ordering::Less
        );
    }

    /// 全序：任意输入顺序排完都一样；分数 / 日期 / 区间仍按既定方向。
    #[test]
    fn candidate_order_is_total_and_deterministic() {
        let mut forward = vec![
            candidate("chunk_a", "aa", 0.5),
            candidate("chunk_b", "bb", 0.9),
            candidate("chunk_c", "cc", 0.9),
            candidate("chunk_d", "aa", 0.5),
        ];
        let mut backward = forward.clone();
        forward.sort_by(rank_order);
        backward.reverse();
        backward.sort_by(rank_order);
        let ids = |items: &[SemanticCandidate]| {
            items
                .iter()
                .map(|item| item.chunk_id.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(ids(&forward), ids(&backward), "换输入顺序不能换结果");
        assert_eq!(
            forward.first().map(|item| item.score),
            Some(0.9),
            "分数高的在前"
        );
    }
    /// NaN/Inf 不能变成「看起来正常的分数」：查询向量或库里的字节含非有限值时，
    /// `score_of` 必须报 `CorruptedData`。否则 NaN 会一路传到排序与结果里。
    #[test]
    fn score_of_rejects_non_finite_scores() {
        let finite = VectorStorage::F32.encode(&[1.0, 0.0]).unwrap();
        let cases: [(&str, Vec<u8>, Vec<f32>); 4] = [
            (
                "查询向量含 NaN",
                finite.clone(),
                vec![f32::NAN, 0.0],
            ),
            (
                "查询向量含 Inf",
                finite.clone(),
                vec![f32::INFINITY, 0.0],
            ),
            (
                "库里的 f32 向量含 NaN",
                [1.0_f32.to_le_bytes(), f32::NAN.to_le_bytes()].concat(),
                vec![1.0, 0.0],
            ),
            (
                "库里的 f32 向量含 Inf",
                [1.0_f32.to_le_bytes(), f32::INFINITY.to_le_bytes()].concat(),
                vec![1.0, 0.0],
            ),
        ];
        for (label, bytes, query) in cases {
            let error = score_of(VectorStorage::F32, &bytes, &query, 1.0, 2, "chunk_x")
                .unwrap_err();
            let CoreError::CorruptedData { message } = error else {
                panic!("{label}：应当报 CorruptedData，实际是 {error:?}");
            };
            assert!(
                message.contains("不是有限数"),
                "{label}：错误消息要说明分数不是有限数：{message}"
            );
        }
    }

    /// `position_order` 必须是全序：NaN 也要有确定位置，绝不能因为不可比就退化成
    /// `Equal`（那会让 `sort_by` 的 comparator 不传递）。
    #[test]
    fn position_order_is_total_with_non_finite_scores() {
        let mut candidates = [
            candidate("chunk_nan", "", f32::NAN),
            candidate("chunk_inf", "", f32::INFINITY),
            candidate("chunk_one", "", 1.0),
            candidate("chunk_neg_inf", "", f32::NEG_INFINITY),
        ];
        // 直接钉住比较器的传递性：旧实现把 NaN 与任何数的比较都退化成 Equal，
        // 于是同时得到「NaN == 1」「NaN == 2」，却又「1 < 2」——等价关系不传递，
        // `sort_by` 因此可以乱序甚至 panic。`total_cmp` 不会让这两条同时成立。
        let nan = candidate("chunk_nan", "", f32::NAN);
        let one = candidate("chunk_one", "", 1.0);
        let two = candidate("chunk_two", "", 2.0);
        assert!(
            !(position_order(&nan, &one) == std::cmp::Ordering::Equal
                && position_order(&nan, &two) == std::cmp::Ordering::Equal)
                || position_order(&one, &two) == std::cmp::Ordering::Equal,
            "比较器必须传递：不能把 NaN 同时当成等于 1 和 2，却又认为 1 != 2"
        );
        candidates.sort_by(position_order);
        for pair in candidates.windows(2) {
            assert_ne!(
                position_order(&pair[0], &pair[1]),
                std::cmp::Ordering::Greater,
                "排序结果必须满足 comparator 全序"
            );
        }
        assert!(candidates[0].score.is_nan(), "total_cmp 把 NaN 排在最后（降序的最前）");
        assert_eq!(candidates[1].score, f32::INFINITY);
        assert_eq!(candidates[3].score, f32::NEG_INFINITY);
    }

    /// 超大并列组（> SQLite 单语句变量上限 32766）不能拼 `IN (?, ...)`。
    /// 退回旧实现会直接报 `too many SQL variables`，这条测试会红。
    #[test]
    fn resolve_ties_handles_groups_larger_than_the_sqlite_variable_limit() {
        let count = 33_000_usize;
        let mut core = Core::open_in_memory().unwrap();
        {
            let tx = core.conn.transaction().unwrap();
            {
                let mut insert = tx
                    .prepare(
                        "INSERT INTO text_chunks (chunk_id, ordinal, text, kind, coverage, \
                         day_key, chunker_version, created_at) \
                         VALUES (?1, ?2, ?3, 'text', 'complete', '2026-09-20', 'v1', \
                         '2026-09-20T00:00:00.000Z')",
                    )
                    .unwrap();
                for index in 0..count {
                    insert
                        .execute(params![
                            format!("chunk_{index:05}"),
                            index as i64,
                            format!("正文 {index}")
                        ])
                        .unwrap();
                }
            }
            tx.commit().unwrap();
        }
        let mut scored: Vec<SemanticCandidate> = (0..count)
            .map(|index| {
                let mut item = candidate(&format!("chunk_{index:05}"), "", 0.5);
                item.chunk_id = format!("chunk_{index:05}");
                item
            })
            .collect();
        resolve_ties(&core, &mut scored).unwrap();
        assert!(
            scored.iter().all(|item| !item.content_key.is_empty()),
            "每个并列候选都要补上 content_key"
        );
    }

}

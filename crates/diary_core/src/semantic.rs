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
//! 按生效代次取全部向量，算余弦相似度，按 `chunk_spans` 展开成「块 × 篇」候选，
//! 分数从高到低排，再按篇折叠。**全表扫描**是这一片的诚实做法：没有 ANN 索引、
//! 也没有 mmap（见文档「存储方案评估」与「没有做到的」），10 万块的规模数字是
//! 算出来的投影，不是实测。
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

use std::collections::HashSet;
use std::sync::Arc;

use rusqlite::{params, OptionalExtension};

use crate::chunks;
use crate::embedding::{
    self, Embedder, EmbeddingConfig, EnvEmbedding, TokenTruncation,
};
use crate::error::{CoreError, Result};
use crate::model::{
    Coverage, MatchedBy, SearchFilters, SearchHit, SourceKind, SourceLocator,
};
use crate::Core;

/// `chunk_vectors.storage` 目前唯一支持的取值。
pub(crate) const STORAGE_F32: &str = "f32";

/// 语义摘录最多这么多字符（超出截断加省略号）。取该篇自己那一段，不是整块正文。
const SEMANTIC_SNIPPET_CHARS: usize = 160;

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
}

impl SemanticRuntime {
    /// 从环境变量初始化。只 stat 文件，不加载模型。
    pub(crate) fn from_env() -> Self {
        let slot = match embedding::config_from_env() {
            EnvEmbedding::Unset => EmbedderSlot::NotConfigured,
            EnvEmbedding::Invalid(reason) => EmbedderSlot::Failed(reason),
            EnvEmbedding::Ready(config) => match config.check_files() {
                Ok(()) => EmbedderSlot::Configured(config),
                // 缺文件也是**诚实的原因**，不是「未就绪」。
                Err(reason) => EmbedderSlot::Failed(reason),
            },
        };
        Self { slot }
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
                    "没有配置本地模型：设 DIARY_MODEL_DIR（模型目录）与 DIARY_ORT_DYLIB（ORT 动态库）\
                     之后才能建语义索引；现在只有关键词索引可用"
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
/// - `storage` 是向量的编码格式（现在只有 `f32`）。
///
/// 口径不同的一批向量放在一起算余弦，量出来的相似度没有意义（两个空间各说各话），
/// 所以这三项也是「能不能复用/续跑」的唯一判据。
struct Caliber<'a> {
    model_version: &'a str,
    dims: i64,
    storage: &'a str,
}

impl Caliber<'_> {
    fn of(embedder: &dyn Embedder) -> Caliber<'_> {
        Caliber {
            model_version: embedder.model_version(),
            dims: embedder.dims() as i64,
            storage: STORAGE_F32,
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

/// 一个代次里某一列的**去重取值**（有序，便于日志与测试稳定）。
///
/// `column` 只由本模块的常量字符串传入（"model_version" / "dims" / "storage"），
/// 不是调用方数据；这里的字符串拼接因此不构成注入面。
fn distinct_values<T: rusqlite::types::FromSql>(
    core: &Core,
    column: &str,
    generation: i64,
) -> Result<Vec<T>> {
    let mut statement = core
        .conn
        .prepare(&format!(
            "SELECT DISTINCT {column} FROM chunk_vectors WHERE generation = ?1 ORDER BY {column}"
        ))?;
    let values = statement
        .query_map(params![generation], |row| row.get::<_, T>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(values)
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
    let caliber = Caliber::of(embedder);
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
            caliber.storage,
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
    storage: &str,
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
            storage,
            embedding::vector_bytes(vector),
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
    let query_embedding = embedder.embed_query(query)?;
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

    // **来源篇必须仍是当前修订**：块的 id 与向量都按内容存，`revise_text` 之后、下一次
    // `build_semantic_index` 之前，旧修订的块与向量**仍在生效代次里**——不过滤就会把旧正文
    // 当成当前内容召回（#32 明确禁止；关键词那一路在 #54 已按同一口径修过）。
    // 记录自写文字的 span 没有来源修订（空串），单独按 `capture_id` 处理即可。
    let (kinds, scope) = crate::search_session::filters_to_params(filters)?;
    let sql = "SELECT v.vector, v.dims, v.storage, s.chunk_id, s.ordinal, s.capture_id, \
                      s.source_id, s.source_revision_id, s.start_char, s.end_char, s.kind, \
                      s.coverage, c.day_key \
               FROM chunk_vectors v \
               JOIN text_chunks t ON t.chunk_id = v.chunk_id \
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
    let mut statement = core.conn.prepare(sql)?;
    let rows = statement
        .query_map(
            params![
                generation,
                i64::from(filters.include_trashed),
                filters.from_day_key,
                filters.to_day_key,
                kinds,
                scope,
            ],
            |row| {
                Ok((
                    row.get::<_, Vec<u8>>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, i64>(8)?,
                    row.get::<_, i64>(9)?,
                    row.get::<_, String>(10)?,
                    row.get::<_, String>(11)?,
                    row.get::<_, String>(12)?,
                ))
            },
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    let mut scored: Vec<SemanticCandidate> = Vec::with_capacity(rows.len());
    for row in rows {
        if row.2 != STORAGE_F32 {
            // 只认 f32。别的格式（将来 int8）要显式支持，不能猜着算。
            return Err(CoreError::CorruptedData {
                message: format!("不认识的向量存储格式：{}", row.2),
            });
        }
        let vector = embedding::bytes_to_vector(&row.0)?;
        if vector.len() as i64 != row.1 {
            return Err(CoreError::CorruptedData {
                message: format!(
                    "向量字节解出 {} 维，但行上记的是 {} 维",
                    vector.len(),
                    row.1
                ),
            });
        }
        // 兜底（正常走不到）：整代口径已经在进计算前核对过，所以这里的每一行**应当**
        // 与查询向量同维。真的不同维时**如实报错**，绝不退回 `cosine` 的静默 `zip`
        // ——那会把两条不同空间的向量截到短的那一段，给出一个看起来正常的分数
        // （原来 `cosine` 的隐患正是这一条）。口径写在
        // `docs/architecture/m2-向量索引与换代.md` 的「消费契约」里。
        if vector.len() != query_vector.len() {
            return Err(CoreError::CorruptedData {
                message: format!(
                    "块 {} 的向量是 {} 维、行上记的是 {} 维，查询向量是 {} 维：\
                     两者不是同一个向量空间，不做余弦",
                    row.3,
                    vector.len(),
                    row.1,
                    query_vector.len()
                ),
            });
        }
        let score = cosine(&query_vector, &vector);
        scored.push(SemanticCandidate {
            group_id: row.6.clone(),
            chunk_id: row.3,
            span_ordinal: row.4,
            score,
            capture_id: row.5,
            source_id: row.6,
            source_revision_id: row.7,
            kind: row.10,
            coverage: Coverage::from_wire(&row.11).unwrap_or(Coverage::Unavailable),
            day_key: row.12,
            start_char: row.8,
            end_char: row.9,
        });
    }

    // 分数从高到低；同分按日期近到远，再按**真正由内容决定**的键：篇内区间与段序号。
    //
    // 为什么同分时**不能**先比 `chunk_id`（issue #57）：`chunk_id` 的哈希输入里有
    // 篇标识（`src_`/`cap_` 前缀的 v7 UUID），而篇标识每个库都不一样。于是**同一份
    // 内容重新建一遍库就会换一批 chunk_id**，两个同分块的相对大小也就跟着抛硬币——
    // 同一篇的一块 A 与一块 B 得分一样时，谁当证据在两次建库之间会变，用户看到的
    // 摘录跟着变。`start_char` / `end_char` 是篇内区间（块切法的函数），
    // `span_ordinal` 是块内位置，三者都只由内容与切块规则决定，跨库稳定。
    //
    // `chunk_id` 只留作最后的**全序兜底**：只有两条候选的（日期、篇内区间、段序号）
    // 都完全相同才会走到它——那时两条命中的内容已经一模一样，顺序不影响用户看到什么。
    // 同分规则的契约写在 `docs/architecture/m2-向量索引与换代.md` 的「消费契约」里。
    scored.sort_by(|left, right| {
        right
            .score
            .partial_cmp(&left.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| right.day_key.cmp(&left.day_key))
            .then_with(|| left.start_char.cmp(&right.start_char))
            .then_with(|| left.end_char.cmp(&right.end_char))
            .then_with(|| left.span_ordinal.cmp(&right.span_ordinal))
            .then_with(|| left.chunk_id.cmp(&right.chunk_id))
    });

    // 折叠：同一篇只留第一条（也就是得分最高的那块）。
    let mut seen: HashSet<String> = HashSet::new();
    let folded: Vec<SemanticCandidate> = scored
        .into_iter()
        .filter(|candidate| seen.insert(candidate.group_id.clone()))
        .collect();
    Ok(SemanticOutcome {
        candidates: SemanticCandidates::Scored(folded),
        query_truncation,
    })
}

/// 余弦相似度。向量已经 L2 归一化，所以正常情况下就是内积；仍然按真余弦算，
/// 免得将来出现没归一化的行就悄悄错掉。
///
/// **前置条件：两个切片同维**。调用方（`candidates`）在算之前就核对过；这里再
/// `debug_assert` 一次是为了钉住「不许静默 `zip` 到短的那一段」这条口径——真出现
/// 不同维时，`debug` 构建直接炸出来，而不是返回一个 0 分或半截分数。
fn cosine(left: &[f32], right: &[f32]) -> f32 {
    debug_assert_eq!(
        left.len(),
        right.len(),
        "不同维的向量不能算余弦（口径详见 m2-向量索引与换代.md 的消费契约）"
    );
    let mut dot = 0_f32;
    let mut left_norm = 0_f32;
    let mut right_norm = 0_f32;
    for (a, b) in left.iter().zip(right) {
        dot += a * b;
        left_norm += a * a;
        right_norm += b * b;
    }
    let denominator = left_norm.sqrt() * right_norm.sqrt();
    if denominator > 0.0 {
        dot / denominator
    } else {
        0.0
    }
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
    if active_generation(core)?.is_none() {
        return Ok(Vec::new());
    }
    let embedder = core.embedder()?;
    // 口径不一致时给**空排名**：与产品路径「语义这一路不作数」同一口径，
    // 而不是把跨空间的分数当成量测结果报出去。
    match candidates(core, &*embedder, query, filters)?.candidates {
        SemanticCandidates::ForeignSpace(_) => Ok(Vec::new()),
        SemanticCandidates::Scored(candidates) => Ok(candidates
            .into_iter()
            .map(|candidate| (candidate.group_id, candidate.score))
            .collect()),
    }
}

/// 生效代次那一批向量的模型口径（状态里要报的**真值**）。
///
/// 为什么不是「读一行 `model_version` 就完事」：`chunk_vectors` 的主键是
/// `(chunk_id, generation)`，一个代次里理论上可以混进不止一种模型版本（本片修复前的
/// 「A 写一半 → 换成 B 续跑」就会产出这种代次，历史库里也可能留下）。这时候
/// 随便挑一行报出去是**撒谎**——它既不代表整代，也掩盖了「这一代不是同一个向量空间」
/// 这件事（检索路径从 #80 起会整路不作数，绝不拿跨空间的分数）。
///
/// 本仓库的口径是「宁可如实说、不要假装干净」，所以按整代给三种结果：
/// `Absent`（没有生效代次，或那一代一行向量都没有——没有版本可报）、
/// `Single`（整代同一个版本，可以如实报）、
/// `Mixed`（不止一种版本：调用方把 `model_version` 报成 `None`，并在 `reasons` 里
/// 点名「口径不一致、需要重建」，而不是挑一行装作干净）。
///
/// 同时，**修复路径保证正常操作下再也产不出 `Mixed` 代次**：续跑前核对口径，
/// 不一致就整代废弃重算；`missing_chunks` 也只认口径一致的行。所以 `Mixed` 只会是
/// 历史遗留数据，正是要用「如实报出来 + 提示重建」处理的那一种。
pub(crate) enum ActiveModelVersion {
    /// 没有生效代次，或生效代次里没有向量：没有版本可报。
    Absent,
    /// 整代都是同一个模型版本。
    Single(String),
    /// 整代里有不止一种模型版本（去重后按字典序，便于测试与日志稳定）。
    Mixed(Vec<String>),
}

pub(crate) fn active_model_version(core: &Core) -> Result<ActiveModelVersion> {
    let Some(generation) = active_generation(core)? else {
        return Ok(ActiveModelVersion::Absent);
    };
    let versions: Vec<String> = distinct_values(core, "model_version", generation)?;
    Ok(match versions.len() {
        0 => ActiveModelVersion::Absent,
        // `len() == 1` 时 `next()` 一定有值。
        1 => ActiveModelVersion::Single(versions.into_iter().next().expect("长度是 1")),
        _ => ActiveModelVersion::Mixed(versions),
    })
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
    let caliber = Caliber::of(embedder);
    let foreign_rows = foreign_row_count(core, generation, &caliber)?;
    if foreign_rows == 0 {
        return Ok(None);
    }
    Ok(Some(CaliberMismatch {
        versions: distinct_values(core, "model_version", generation)?,
        dims: distinct_values(core, "dims", generation)?,
        storages: distinct_values(core, "storage", generation)?,
        foreign_rows,
        current_version: caliber.model_version.to_owned(),
        current_dims: caliber.dims,
        current_storage: caliber.storage.to_owned(),
    }))
}

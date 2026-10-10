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
//! `Core::build_semantic_index` 一次跑完：
//!
//! 1. **重建块**（`chunks::rebuild`）：只删消失的块 + UPSERT，内容没变的块保住 id
//!    与既有向量；
//! 2. **开新代次**：`building_generation = COALESCE(building_generation, COALESCE(active_generation, 0) + 1)`。
//!    正在服务的检索继续读旧那一代，界面不会突然空掉；
//! 3. **复用 + 补算**：新代次里还没有向量的块，先尝试从旧代次**整批复制**口径一致
//!    （`model_version` / `dims` / `storage` 都相同）的向量，剩下的才算。
//!    这正是「没变的块不重复算」；
//! 4. **原子切换**：一个事务里 `active_generation = building_generation;
//!    building_generation = NULL`，并清掉 `generation < active` 的旧向量。
//!    不存在「一半新一半旧」的服务状态，也不会每重建一次就把存储翻倍。
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

use std::collections::HashSet;
use std::sync::Arc;

use rusqlite::{params, OptionalExtension};

use crate::chunks;
use crate::embedding::{
    self, Embedder, EmbeddingConfig, EnvEmbedding,
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
    /// 上一次中断留下的、本代次里已经有的向量数（续跑时不为 0）。
    pub resumed_chunks: i64,
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

/// 建语义索引：重建块 → 开新代次 → 复用/补算向量 → 原子切换。
///
/// 模型缺失或加载失败时**返回诚实的错误、不改任何状态**（连块都不重建）。
/// `source_scope` 与 `rebuild_text_chunks` 同义：`Some([])` 是「什么都不看」，
/// 什么都不做。公开入口是 `Core::build_semantic_index`（见 `lib.rs`）。
pub(crate) fn build(
    core: &mut Core,
    source_scope: Option<&[String]>,
) -> Result<SemanticBuildReport> {
    if matches!(source_scope, Some([])) {
        return Ok(SemanticBuildReport {
            generation: 0,
            total_chunks: 0,
            reused_chunks: 0,
            embedded_chunks: 0,
            resumed_chunks: 0,
        });
    }
    // 先拿嵌入器：拿不到就在这里失败，**不改任何状态**（块也不重建）。
    let embedder = core.embedder()?;
    // 重建块（没变的块保住 id，因此保住旧代次的向量）。
    chunks::rebuild(core, source_scope)?;
    build_generation(core, &*embedder)
}

fn semantic_unavailable(reason: String) -> CoreError {
    CoreError::InvalidState {
        entity: "语义索引",
        id: "local-embedding".to_owned(),
        state: reason,
    }
}

/// 开代次 → 复用/补算 → 原子切换。嵌入器已经确认可用。
fn build_generation(core: &mut Core, embedder: &dyn Embedder) -> Result<SemanticBuildReport> {
    let (active, building) = chunks::generations(core)?;
    // 开新代次：中断过就从它续跑，否则在生效代次之后 +1。**不在原地更新 active**。
    let generation = match building {
        Some(value) => value,
        None => active.map_or(1, |value| value + 1),
    };
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
                embedder.model_version(),
                embedder.dims() as i64,
                STORAGE_F32,
            ],
        )? as i64,
    };

    // 补齐：本代次里还没有向量的块。逐块提交：中途中断也留着 building_generation，
    // 下一轮从这儿继续（不会激活半成品）。
    let missing = missing_chunks(core, generation)?;
    let resumed_chunks = (total_chunks - reused_chunks - missing.len() as i64).max(0);
    let dims = embedder.dims();
    for (chunk_id, text) in &missing {
        let vector = embedder.embed_document(text)?;
        if vector.len() != dims {
            return Err(CoreError::InvalidState {
                entity: "语义嵌入",
                id: chunk_id.clone(),
                state: format!("算出来的是 {} 维，与模型声明的 {dims} 维不符", vector.len()),
            });
        }
        write_vector(core, chunk_id, generation, embedder.model_version(), &vector)?;
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

    Ok(SemanticBuildReport {
        generation,
        total_chunks,
        reused_chunks,
        embedded_chunks: missing.len() as i64,
        resumed_chunks,
    })
}

/// 本代次里还没有向量的块（按日期与块内顺序，确定性）。
fn missing_chunks(core: &Core, generation: i64) -> Result<Vec<(String, String)>> {
    let mut statement = core.conn.prepare(
        "SELECT t.chunk_id, t.text FROM text_chunks t \
         LEFT JOIN chunk_vectors v ON v.chunk_id = t.chunk_id AND v.generation = ?1 \
         WHERE v.chunk_id IS NULL ORDER BY t.day_key, t.ordinal, t.chunk_id",
    )?;
    let rows = statement
        .query_map(params![generation], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// 写一行向量。自己的事务：中途中断也不会留下半行。
fn write_vector(
    core: &Core,
    chunk_id: &str,
    generation: i64,
    model_version: &str,
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
            STORAGE_F32,
            embedding::vector_bytes(vector),
            embedding::now_iso(),
        ],
    )?;
    Ok(())
}

// ------------------------------------------------------------ 语义检索

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
/// **折叠在分页之前**：同一篇的多个块命中只留得分最高的那一个，所以「一篇长材料」
/// 不会在结果里出现十次（消费契约第 1 条）。
pub(crate) fn candidates(
    core: &Core,
    embedder: &dyn Embedder,
    query: &str,
    filters: &SearchFilters,
) -> Result<Vec<SemanticCandidate>> {
    let Some(generation) = active_generation(core)? else {
        return Ok(Vec::new());
    };
    if query.trim().is_empty() {
        return Ok(Vec::new());
    }
    let query_vector = embedder.embed_query(query)?;
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
                 AND (?6 IS NULL OR s.source_id IN (SELECT value FROM json_each(?6)))";
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
    Ok(folded)
}

/// 余弦相似度。向量已经 L2 归一化，所以正常情况下就是内积；仍然按真余弦算，
/// 免得将来出现没归一化的行就悄悄错掉。
fn cosine(left: &[f32], right: &[f32]) -> f32 {
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
    Ok(candidates(core, &*embedder, query, filters)?
        .into_iter()
        .map(|candidate| (candidate.group_id, candidate.score))
        .collect())
}

/// 生效代次那一批向量的 `model_version`（状态里要报的真值）。没有生效代次时 `None`。
pub(crate) fn model_version(core: &Core) -> Result<Option<String>> {
    let Some(generation) = active_generation(core)? else {
        return Ok(None);
    };
    let version: Option<String> = core
        .conn
        .query_row(
            "SELECT model_version FROM chunk_vectors WHERE generation = ?1 LIMIT 1",
            params![generation],
            |row| row.get(0),
        )
        .optional()?;
    Ok(version)
}

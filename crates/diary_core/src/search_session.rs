//! 检索会话：契约第 2.6 节与第 4.4 节的 `search.*`。
//!
//! 三条规则决定了这里的形状：
//!
//! 1. **旧查询的结果不能覆盖新查询**。核心按 `sessionId` + `queryRevision` 双校验；
//!    会话不存在、或索引在会话期间变了，一律报 `search_expired` 让前端重新发起，
//!    而不是把过期结果糊到新界面上。
//! 2. **翻页绑定原会话**。游标里带着会话号，拿别的会话的游标来翻报 `cursor_expired`。
//! 3. **取消只停止后续处理，不删任何原件**。取消后 `phase` 是 `cancelled`，
//!    再翻页会被拒（`invalid_state`），但已经返回的结果仍然可读。
//!
//! 会话状态放在内存里：它记录的是「这一次查询翻到哪、快照还有效吗」，不是需要
//! 长期保存的业务数据。重开资料库后旧 `sessionId` 一律 `search_expired`。
//!
//! # 三种模式
//!
//! - `Keyword`：纯关键词（B3a/B3d）；
//! - `Semantic`：纯语义（B3c-2），模型或生效代次缺失时**空结果 + 诚实警告**；
//! - `Hybrid`：RRF 融合（B3c-3，issue [#51](https://github.com/xingxue-ux/unwritten-diary/issues/51)）。
//!   两路各取**篇**为单位的前 `RRF_CANDIDATE_DEPTH` 条，`score = Σ 1/(RRF_K + rank)`，
//!   相关度优先、同档按时间降序、再按**内容决定**的键（正文哈希，不用任何 UUID）。
//!   语义未就绪时**退化为关键词并在 `warnings` 里明说**，不静默。

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::embedding::Embedder;
use crate::error::{CoreError, Result};
use crate::model::{
    Coverage, MatchedBy, SearchFilters, SearchHit, SearchMode, SearchPhase, SearchRequest,
    SearchSnapshot, SourceKind, TextRange,
};
use crate::search;
use crate::semantic;
use crate::support;
use crate::Core;

/// 一页最多这么多条。契约的默认每页 20；上限挡住「一次要一万条」这种请求。
const MAX_PAGE_SIZE: i64 = 100;

/// 摘录窗口：命中位置前后各留这么多字符。
const SNIPPET_PADDING: usize = 24;

/// RRF 的 `k`。`score = Σ_paths 1/(k + rank_path)`，`rank` 从 1 起。
///
/// `k = 60` 是 Cormack 等 2009 提出 RRF 时的常用值，**没有**在 36 条合成语料上
/// 调参：那个规模撑不起「调出来的最优」，留下的只有过拟合痕迹。
const RRF_K: i64 = 60;

/// 融合前每一路最多取这么多**篇**（不是块、不是片段）。
///
/// 关键词那一路先按篇折叠再截断，语义那一路本来就是按篇折叠的（#32 消费契约第 1 条），
/// 所以两条路在同一个单位上取深度，融合才是公平的。
const RRF_CANDIDATE_DEPTH: usize = 50;

/// 关键词那一路为「按篇折叠 + 正文哈希」最多读这么多条**文档**的正文
/// （`4 × RRF_CANDIDATE_DEPTH`）。
///
/// 关键词模式本来只按页读正文；混合要在候选阶段折叠、算内容哈希，全读会把几万条
/// 命中的正文拉进内存。所以只读排序最靠前的一段：正常数据（一篇 1–2 段）200 条足够
/// 折出 50 篇；病态数据（前 200 条全是一个来源的段）会折出更少的篇——这是刻意的取舍，
/// 写在文档的「没有做到的」里。
const RRF_KEYWORD_OVERSCAN: usize = RRF_CANDIDATE_DEPTH * 4;

/// 语义一路的**证据门槛**（余弦相似度）。低于它的候选算「这一路没有证据」，不参与
/// 融合；一条都不剩时，语义这一路就是不贡献结果——`no_answer` 四条的「4/4 空」靠
/// 它守住（纯 kNN 没有「没有答案」这个概念，只能靠分数拒绝）。
///
/// 取值来自 `tests/quality/search_quality_set.json` 上的**阈值扫描**（0.20–0.50，
/// 步进 0.02，表在 `docs/architecture/m2-向量索引与换代.md`），不是拍脑袋；扫描用的
/// 探针入口是 `diary_probe hybrid-quality`，它通过 `set_hybrid_min_score_for_test`
/// 在同一条产品路径上换阈值。
pub const HYBRID_SEMANTIC_MIN_SCORE: f32 = 0.40;

/// 融合口径的版本号：`RRF_K` / `RRF_CANDIDATE_DEPTH` / 关键词扫描窗口 / 语义门槛
/// 任何一项变了都要改这一行（照 `CHUNKER_VERSION` 的先例）。文档与探针输出都引用它，
/// 测试钉住这个字符串。
pub const HYBRID_FUSION_VERSION: &str = "hybrid-rrf-k60-depth50-over200-min0.40";

/// 索引指纹：关键词索引代次 + 语义生效代次。会话期间有人写过索引、或语义索引
/// 换了一代，它就会变，快照随即作废。
///
/// 原来只有关键词那一个代次。语义这一路要跟着生效代次走：一代向量被切走之后，
/// 会话手里的「块 → 篇」排序已经过期，旧结果不该继续糊在新界面上。
/// 关键词会话不看语义代次，语义修改不会无谓地打断关键词翻页；**混合会话要看**——
/// 融合结果里有一半来自语义。
type IndexFingerprint = (i64, Option<i64>);

/// 会话里一条命中的引用。两条路的「一条结果」不是同一种东西：
///
/// - 关键词：一个索引文档（派生片段 / 记录文字），正文按页现取；
/// - 语义：**一篇**（消费契约要求按篇上报），证据块与篇内区间一起记着；
/// - 混合：**一篇**，两路各自的证据都留着，`matchedBy` 如实标。
///
/// 会话里只存引用，正文仍然按页现取。
#[derive(Debug, Clone)]
pub(crate) enum RankedRef {
    Keyword(search::DocRef),
    Semantic(semantic::SemanticCandidate),
    Hybrid(HybridHit),
}

/// 混合检索融合后的一条：以**篇**为单位。
///
/// 两路各自的证据都留着：关键词那一路有高亮（正文里哪几个字对上），语义那一路没有。
/// 一条命中两路都有时，用关键词那份正文（有高亮），`matchedBy` 标成两者都有。
#[derive(Debug, Clone)]
pub(crate) struct HybridHit {
    /// 关键词那一路的证据（篇内哪个片段 / 记录文字）。
    pub keyword: Option<search::DocRef>,
    /// 语义那一路的证据（篇内得分最高的那一块）。
    pub semantic: Option<semantic::SemanticCandidate>,
    /// 如实的命中来源：`[Keyword]` / `[Semantic]` / `[Keyword, Semantic]`。
    pub matched_by: Vec<MatchedBy>,
}

/// 融合过程中的一条候选（排序完就转成 `RankedRef`）。`content_key` 只在排序时用。
struct FusedCandidate {
    group_id: String,
    score: f32,
    day_key: Option<String>,
    /// 正文哈希：同分兜底键，**由内容决定**（不用 UUID）。
    content_key: String,
    keyword: Option<search::DocRef>,
    semantic: Option<semantic::SemanticCandidate>,
}

/// 一个检索会话的内存状态。
pub(crate) struct SearchSessionState {
    session_id: String,
    query_revision: i64,
    request: SearchRequest,
    /// 命中的引用，已按各自的排序规则排好。只存引用：整段正文按页现取，别把几万条
    /// 正文常驻内存。关键词的引用带来源是因为两张表的 `doc_id` 会撞号。
    ordered: Vec<RankedRef>,
    /// 上一页返回的引用，`search.snapshot` 要能原样再给一次。
    last_page: Vec<RankedRef>,
    /// 下一页的游标；没有下一页时为 None。
    next_cursor: Option<String>,
    fingerprint: IndexFingerprint,
    phase: SearchPhase,
    cancelled: bool,
    index_coverage: Coverage,
    warnings: Vec<String>,
}

pub(crate) fn start(
    core: &mut Core,
    request: SearchRequest,
    query_revision: i64,
) -> Result<SearchSnapshot> {
    let page_size = request.page_size.clamp(1, MAX_PAGE_SIZE);
    let needle = request.query.trim().to_lowercase();
    let mut warnings = Vec::new();

    if request.filters.include_old_diary_versions {
        warnings.push("日记历史版本还没有进索引，这个开关暂时没有效果".to_owned());
    }

    let (ordered, index_coverage, phase) = match request.mode {
        SearchMode::Semantic => {
            semantic_search(core, &request, &needle, page_size, &mut warnings)?
        }
        SearchMode::Hybrid => hybrid_search(core, &request, &needle, page_size, &mut warnings)?,
        SearchMode::Keyword => keyword_search(core, &request, &needle, page_size, &mut warnings)?,
    };

    let session_id = support::new_id("search");
    let fingerprint = fingerprint(core, request.mode)?;
    let (last_page, next_cursor) = page_window(&session_id, &ordered, 0, page_size);

    let state = SearchSessionState {
        session_id: session_id.clone(),
        query_revision,
        request: SearchRequest {
            query: request.query,
            mode: request.mode,
            filters: request.filters,
            page_size,
        },
        ordered,
        last_page,
        next_cursor,
        fingerprint,
        phase,
        cancelled: false,
        index_coverage,
        warnings,
    };
    let snapshot = materialize(core, &state, &state.last_page)?;
    core.search_sessions_mut().insert(session_id, state);
    Ok(snapshot)
}

/// 关键词那一路：候选 → 引用 → 覆盖状态。阶段值沿用原来的口径（还有下一页时
/// `keyword_ready`，否则 `done`）。
fn keyword_search(
    core: &Core,
    request: &SearchRequest,
    needle: &str,
    page_size: i64,
    warnings: &mut Vec<String>,
) -> Result<(Vec<RankedRef>, Coverage, SearchPhase)> {
    let ordered: Vec<RankedRef> = if needle.is_empty() {
        warnings.push("查询是空的，没有可匹配的内容".to_owned());
        Vec::new()
    } else {
        search::ranked_matches(core, needle, &request.filters)?
            .into_iter()
            .map(|row| RankedRef::Keyword(row.doc))
            .collect()
    };
    let coverage = index_coverage(core)?;
    if coverage != Coverage::Complete {
        warnings.push("索引还没有覆盖全部内容，结果可能不完整".to_owned());
    }
    let phase = if ordered.len() > page_size as usize {
        SearchPhase::KeywordReady
    } else {
        SearchPhase::Done
    };
    Ok((ordered, coverage, phase))
}

/// 语义那一路。契约要求**不静默降级**：模型缺失或没有生效代次时返回**空结果 +
/// 明确警告**，而不是拿关键词结果冒充「语义」。
///
/// 语义结果是一次算完的（没有「后面还会更好」的阶段），所以 `phase` 恒为 `done`；
/// 翻页由 `cursor` 表达，不由 `phase` 表达。
fn semantic_search(
    core: &mut Core,
    request: &SearchRequest,
    needle: &str,
    _page_size: i64,
    warnings: &mut Vec<String>,
) -> Result<(Vec<RankedRef>, Coverage, SearchPhase)> {
    if needle.is_empty() {
        warnings.push("查询是空的，没有可匹配的内容".to_owned());
        return Ok((Vec::new(), semantic_coverage(core, &request.filters)?, SearchPhase::Done));
    }
    let coverage = semantic_coverage(core, &request.filters)?;
    if semantic::active_generation(core)?.is_none() {
        warnings.push(
            "语义索引还没有生效的代次：先跑一次 build_semantic_index；这次没有结果\
             （没有退化成关键词，那会让「语义」这个标签撒谎）"
                .to_owned(),
        );
        return Ok((Vec::new(), coverage, SearchPhase::Done));
    }
    // 模型/运行库拿不到时也是**空结果 + 诚实的原因**，不降级。
    let embedder = match core.embedder() {
        Ok(embedder) => embedder,
        Err(error) => {
            warnings.push(format!(
                "语义检索不可用：{error}；这次没有结果（没有退化成关键词）"
            ));
            return Ok((Vec::new(), coverage, SearchPhase::Done));
        }
    };
    let candidates = semantic::candidates(core, &*embedder, needle, &request.filters)?;
    if candidates.is_empty() {
        // 「没有结果」就要说「没有结果」，不要伪装成「没有内容」。
        warnings.push("语义检索没有找到相近的内容".to_owned());
    }
    if coverage != Coverage::Complete {
        warnings.push("语义索引还没有覆盖全部内容，结果可能不完整".to_owned());
    }
    let ordered = candidates.into_iter().map(RankedRef::Semantic).collect();
    Ok((ordered, coverage, SearchPhase::Done))
}

/// 混合那一路：关键词 + 语义，RRF 融合，以**篇**为单位。
///
/// 流程：两路各取候选（先按篇折叠、再截断到 `RRF_CANDIDATE_DEPTH`）→ 逐路给
/// `1/(RRF_K + rank)` → 同篇相加 → 按 `score DESC → day_key DESC → 内容哈希 ASC`
/// 排序 → 交给会话分页。语义一路低于 `core.hybrid_min_score`（产品默认是
/// `HYBRID_SEMANTIC_MIN_SCORE`）的候选算「没有证据」，不参与融合。
///
/// 退化（语义未就绪：没模型 / 没生效代次）：**按关键词执行 + 在 `warnings` 里明说**。
/// 不静默——那会让「混合」这个标签撒谎；也不报「空结果」——那会把一条本来能用的
/// 关键词查询变成什么都没有。
fn hybrid_search(
    core: &mut Core,
    request: &SearchRequest,
    needle: &str,
    page_size: i64,
    warnings: &mut Vec<String>,
) -> Result<(Vec<RankedRef>, Coverage, SearchPhase)> {
    if needle.is_empty() {
        warnings.push("查询是空的，没有可匹配的内容".to_owned());
        return Ok((
            Vec::new(),
            hybrid_coverage(core, &request.filters)?,
            SearchPhase::Done,
        ));
    }

    // 语义这一路先确认就绪（拿嵌入器是懒加载，可能失败）。**不在这里直接返回**：
    // 先记下原因，等两边的覆盖状态都算完再一起决定退化与否，这样 warnings 的顺序
    // 不依赖「哪一步先失败」。
    let mut embedder: Option<Arc<dyn Embedder>> = None;
    let mut degraded: Option<String> = None;
    if semantic::active_generation(core)?.is_none() {
        degraded = Some("语义索引还没有生效的代次（先跑一次 build_semantic_index）".to_owned());
    } else {
        match core.embedder() {
            Ok(handle) => embedder = Some(handle),
            Err(error) => degraded = Some(format!("语义检索不可用：{error}")),
        }
    }
    if let Some(reason) = degraded {
        let (ordered, coverage, phase) =
            keyword_search(core, request, needle, page_size, warnings)?;
        // 退化路径也要按 `Hybrid` 的引用形状返回：会话是按 mode 派发实体化的，
        // 直接塞 `Keyword` 引用会让 `materialize_hybrid` 一条都认不出来（空结果）。
        let ordered = ordered
            .into_iter()
            .map(|reference| match reference {
                RankedRef::Keyword(doc) => RankedRef::Hybrid(HybridHit {
                    keyword: Some(doc),
                    semantic: None,
                    matched_by: vec![MatchedBy::Keyword],
                }),
                other => other,
            })
            .collect();
        warnings.push(format!(
            "混合检索退化为关键词：{reason}；这次只跑了关键词那一路（不是静默降级）"
        ));
        return Ok((ordered, coverage, phase));
    }
    let embedder = embedder.expect("确认就绪之后一定有嵌入器");

    let keyword_coverage = index_coverage(core)?;
    let semantic_coverage = semantic_coverage(core, &request.filters)?;
    let coverage = weakest_coverage(keyword_coverage, semantic_coverage);

    // —— 关键词那一路：按篇折叠 → 截断 → 名次 ——
    //
    // 注意：关键词那一路**不是**按篇出的（一个来源可以被抽成多段，每段一个索引
    // 文档），所以这里必须自己按 `groupId` 折叠。issue #51 的正文说「两路各自已经
    // 按篇折叠」，对语义成立、对关键词不成立；这件事写在文档的「没有做到的」里。
    let keyword_matches = search::ranked_matches(core, needle, &request.filters)?;
    // 只把排序最靠前的 `RRF_KEYWORD_OVERSCAN` 条文档读进来：按篇折叠与正文哈希都要
    // 正文/篇标识，而关键词这一路本来只按页读正文——几万条命中全读进来是这一片最
    // 容易踩的性能坑。200 条文档对「一篇 1–2 段」的正常数据足够折出 50 篇。
    let window = &keyword_matches[..keyword_matches.len().min(RRF_KEYWORD_OVERSCAN)];
    let keyword_refs: Vec<search::DocRef> = window.iter().map(|row| row.doc).collect();
    let mut segment_rows: HashMap<search::DocRef, search::PageRow> = search::load_page(core, &keyword_refs)?
        .into_iter()
        .map(|row| (row.doc, row))
        .collect();
    let mut capture_rows: HashMap<search::DocRef, search::CapturePageRow> =
        search::load_capture_page(core, &keyword_refs)?
            .into_iter()
            .map(|row| (row.doc, row))
            .collect();

    // 名次用**同名次**（competition ranking）：`sort_key` 的前三项（日期、出现位置、
    // 家族）都相同时视为并列第一/并列第几名，取这一组里最靠前的名次。理由：那三项
    // 相同时，关键词这一路没有任何分数能分出高下，第 4 项是**库内**的 `doc_id`
    // （插入顺序），拿它给两篇同分内容发不同的 RRF 分等于让融合结果跟建库顺序挂钩。
    // 并列之后，先后交给融合自己的同分键（内容哈希）决定。
    let mut keyword_groups: Vec<(String, search::DocRef, Option<String>, String, usize)> = Vec::new();
    let mut seen_groups: HashSet<String> = HashSet::new();
    let mut previous_prefix: Option<(i64, i64, i64)> = None;
    let mut group_rank = 0_usize;
    for (position, row) in window.iter().enumerate() {
        let key = sort_key(row);
        let prefix = (key.0, key.1, key.2);
        // 同名次：与上一行同前缀就沿用上一行的名次，否则用当前 1 起的位置。
        let current_rank = if previous_prefix.as_ref() == Some(&prefix) {
            group_rank
        } else {
            position + 1
        };
        previous_prefix = Some(prefix);
        group_rank = current_rank;

        let (group_id, day_key, text) = if let Some(page) = segment_rows.remove(&row.doc) {
            (page.source_id, page.day_key, page.text)
        } else if let Some(page) = capture_rows.remove(&row.doc) {
            (page.capture_id, Some(page.day_key), page.text)
        } else {
            // 索引行还在、正文行没了（重提取的中间态）：这一条如实跳过。
            continue;
        };
        if !seen_groups.insert(group_id.clone()) {
            continue;
        }
        keyword_groups.push((
            group_id,
            row.doc,
            day_key,
            support::content_hash(&text),
            current_rank,
        ));
        if keyword_groups.len() >= RRF_CANDIDATE_DEPTH {
            break;
        }
    }

    // —— 语义那一路：候选已按篇折叠，过门槛，再截断 ——
    let mut semantic_candidates = semantic::candidates(core, &*embedder, needle, &request.filters)?;
    let top_score = semantic_candidates.first().map(|candidate| candidate.score);
    let min_score = core.hybrid_min_score();
    semantic_candidates.retain(|candidate| candidate.score >= min_score);
    semantic_candidates.truncate(RRF_CANDIDATE_DEPTH);
    if semantic_candidates.is_empty() {
        match top_score {
            Some(top) => warnings.push(format!(
                "语义一路最像的内容相似度 {top:.3}，低于证据门槛 {min_score:.2}，\
                 这一路没有贡献结果"
            )),
            None => warnings.push("语义检索没有找到相近的内容".to_owned()),
        }
    }
    if keyword_coverage != Coverage::Complete {
        warnings.push("关键词索引还没有覆盖全部内容，结果可能不完整".to_owned());
    }
    if semantic_coverage != Coverage::Complete {
        warnings.push("语义索引还没有覆盖全部内容，结果可能不完整".to_owned());
    }

    // —— 融合：同篇相加 RRF 分 ——
    let mut fused: Vec<FusedCandidate> = Vec::new();
    let mut fused_index: HashMap<String, usize> = HashMap::new();
    for (group_id, doc, day_key, content_key, rank) in keyword_groups {
        fused_index.insert(group_id.clone(), fused.len());
        fused.push(FusedCandidate {
            group_id,
            score: rrf_score(rank),
            day_key,
            content_key,
            keyword: Some(doc),
            semantic: None,
        });
    }
    for (position, candidate) in semantic_candidates.into_iter().enumerate() {
        let score = rrf_score(position + 1);
        match fused_index.get(&candidate.group_id) {
            Some(&index) => {
                fused[index].score += score;
                fused[index].semantic = Some(candidate);
            }
            None => {
                // 语义独有：正文哈希只在这一路上算。
                let content_key = semantic::content_key(core, &candidate)?
                    .unwrap_or_else(|| support::content_hash(""));
                fused_index.insert(candidate.group_id.clone(), fused.len());
                fused.push(FusedCandidate {
                    group_id: candidate.group_id.clone(),
                    score,
                    day_key: Some(candidate.day_key.clone()),
                    content_key,
                    keyword: None,
                    semantic: Some(candidate),
                });
            }
        }
    }

    // 排序：**相关度优先，同档按时间降序**，再按内容决定的键，最后才是库内兜底。
    //
    // 为什么最后一项不用 `groupId` 打头（issue #57 的教训）：`groupId` 是 v7 UUID，
    // 每个库都不一样；同分时先比它，同一份内容在两个库里就会给出不同的先后。
    // `content_key` 是正文哈希，只由内容决定。真的走到 `groupId` 那一项时，两边的
    // 正文哈希已经相同（内容一样），谁先谁后不影响用户看到什么。
    fused.sort_by(|left, right| {
        right
            .score
            .partial_cmp(&left.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| right.day_key.cmp(&left.day_key))
            .then_with(|| left.content_key.cmp(&right.content_key))
            .then_with(|| left.group_id.cmp(&right.group_id))
    });

    if fused.is_empty() {
        warnings.push("两路都没有达到证据门槛的内容，这次没有结果".to_owned());
    }

    let ordered: Vec<RankedRef> = fused
        .into_iter()
        .map(|entry| {
            let matched_by = entry.matched_by();
            RankedRef::Hybrid(HybridHit {
                keyword: entry.keyword,
                semantic: entry.semantic,
                matched_by,
            })
        })
        .collect();
    Ok((ordered, coverage, SearchPhase::Done))
}

impl FusedCandidate {
    /// 如实的 `matchedBy`：两边都有就是两者都有。
    fn matched_by(&self) -> Vec<MatchedBy> {
        match (&self.keyword, &self.semantic) {
            (Some(_), Some(_)) => vec![MatchedBy::Keyword, MatchedBy::Semantic],
            (Some(_), None) => vec![MatchedBy::Keyword],
            (None, Some(_)) => vec![MatchedBy::Semantic],
            (None, None) => Vec::new(),
        }
    }
}

/// 一路的名次换算成 RRF 分：`1/(k + rank)`，`rank` 从 1 起。
fn rrf_score(rank: usize) -> f32 {
    1.0 / (RRF_K as f32 + rank as f32)
}

/// 混合的覆盖程度：两路里**较弱**的那个（一路没覆盖完，融合结果就可能不完整）。
fn hybrid_coverage(core: &mut Core, filters: &SearchFilters) -> Result<Coverage> {
    let keyword = index_coverage(core)?;
    let semantic = semantic_coverage(core, filters)?;
    Ok(weakest_coverage(keyword, semantic))
}

/// 覆盖程度取弱：`complete` 最好、`unavailable` 最差（与文档里的取值序一致）。
fn weakest_coverage(left: Coverage, right: Coverage) -> Coverage {
    fn rank(coverage: Coverage) -> u8 {
        match coverage {
            Coverage::Complete => 3,
            Coverage::Partial => 2,
            Coverage::MetadataOnly => 1,
            Coverage::Unavailable => 0,
        }
    }
    if rank(left) <= rank(right) {
        left
    } else {
        right
    }
}

/// 语义那一路的覆盖程度：没有生效代次或没有块是 `unavailable`，生效代次没覆盖
/// 完范围内的块是 `partial`，否则 `complete`。
fn semantic_coverage(core: &Core, filters: &SearchFilters) -> Result<Coverage> {
    let scope = filters.source_scope.as_deref();
    let counts = crate::chunks::counts(core, scope)?;
    if semantic::active_generation(core)?.is_none() || counts.total == 0 {
        return Ok(Coverage::Unavailable);
    }
    Ok(if counts.embedded == counts.total {
        Coverage::Complete
    } else {
        Coverage::Partial
    })
}

pub(crate) fn next_page(
    core: &mut Core,
    session_id: &str,
    cursor: Option<&str>,
) -> Result<SearchSnapshot> {
    // 先把游标解出来再借用会话：借错了顺序就编译不过，顺便逼着校验游标归属。
    let offset = match cursor {
        Some(value) => parse_cursor(value, session_id)?,
        None => None,
    };

    let mut state = core
        .search_sessions_mut()
        .remove(session_id)
        .ok_or_else(|| CoreError::SearchExpired {
            reason: format!("会话 {session_id} 不存在或已随重启丢失，请重新发起查询"),
        })?;

    let result = (|| -> Result<SearchSnapshot> {
        if state.cancelled {
            return Err(CoreError::InvalidState {
                entity: "检索会话",
                id: state.session_id.clone(),
                state: "cancelled".to_owned(),
            });
        }
        ensure_fresh(core, &state)?;

        let start = match offset {
            // 显式游标：必须指向这一页之后，否则就是拿旧游标重翻。
            Some(value) => {
                let expected = state.next_cursor.as_deref().map(parse_offset).transpose()?;
                if Some(value) != expected {
                    return Err(CoreError::CursorExpired {
                        reason: format!(
                            "游标与当前进度对不上（会话 {}，收到 {}）",
                            state.session_id, value
                        ),
                    });
                }
                value
            }
            None => match state.next_cursor.as_deref() {
                Some(cursor) => parse_offset(cursor)?,
                // 已经到末页，没有下一页了。**不能**退回第 0 条重来——那会把第一页
                // 当成新一页返回（审查发现的就是这一条），而且「一直翻到空页为止」
                // 的客户端会变成死循环。返回空页，`phase` 已经是 done。
                None => return materialize(core, &state, &[]),
            },
        };

        let (page, cursor) = page_window(
            &state.session_id,
            &state.ordered,
            start,
            state.request.page_size,
        );
        state.last_page = page;
        state.next_cursor = cursor;
        state.phase = phase_after_page(state.request.mode, state.next_cursor.is_none());
        materialize(core, &state, &state.last_page)
    })();

    // 无论成功失败都要把会话放回去：失败（比如游标过期）不该把整个会话弄没。
    core.search_sessions_mut().insert(session_id.to_owned(), state);
    result
}

pub(crate) fn snapshot(core: &Core, session_id: &str) -> Result<SearchSnapshot> {
    let state = core
        .search_sessions()
        .get(session_id)
        .ok_or_else(|| CoreError::SearchExpired {
            reason: format!("会话 {session_id} 不存在或已随重启丢失，请重新发起查询"),
        })?;
    ensure_fresh(core, state)?;
    materialize(core, state, &state.last_page)
}

pub(crate) fn cancel(core: &mut Core, session_id: &str) -> Result<SearchSnapshot> {
    let mut state = core
        .search_sessions_mut()
        .remove(session_id)
        .ok_or_else(|| CoreError::SearchExpired {
            reason: format!("会话 {session_id} 不存在或已随重启丢失，请重新发起查询"),
        })?;
    state.cancelled = true;
    state.phase = SearchPhase::Cancelled;
    state.next_cursor = None;
    let result = materialize(core, &state, &state.last_page);
    // 取消后还留在表里：这样 `search.snapshot` 还能读到「已取消」这个状态，
    // 前端不至于因为会话突然消失而把它当成过期（那是两件事）。
    core.search_sessions_mut().insert(session_id.to_owned(), state);
    result
}

// ------------------------------------------------------------ 内部

fn ensure_fresh(core: &Core, state: &SearchSessionState) -> Result<()> {
    if fingerprint(core, state.request.mode)? != state.fingerprint {
        return Err(CoreError::SearchExpired {
            reason: "索引在这次查询期间变了，快照已失效；请重新发起查询".to_owned(),
        });
    }
    Ok(())
}

/// 关键词会话的指纹是关键词代次；语义与混合会话还要带上生效的向量代次——切了一代
/// 之后旧的「块 → 篇」排序与融合结果都已经过期。关键词会话**不看**语义代次：
/// 向量换代不该无谓地打断正在翻页的关键词查询。
fn fingerprint(core: &Core, mode: SearchMode) -> Result<IndexFingerprint> {
    let epoch = search::index_epoch(core)?;
    if matches!(mode, SearchMode::Semantic | SearchMode::Hybrid) {
        Ok((epoch, semantic::active_generation(core)?))
    } else {
        Ok((epoch, None))
    }
}

/// 一页之后该报什么阶段。语义与混合都是一次算完的（没有「后面还会更好」的阶段），
/// 阶段恒为 `done`；翻页由 `cursor` 表达，不由 `phase` 表达。
fn phase_after_page(mode: SearchMode, is_last_page: bool) -> SearchPhase {
    if is_last_page || matches!(mode, SearchMode::Semantic | SearchMode::Hybrid) {
        SearchPhase::Done
    } else {
        SearchPhase::KeywordReady
    }
}

/// 索引覆盖：复用 `indexes.status` 的口径，但只关心「能不能搜」。
fn index_coverage(core: &Core) -> Result<Coverage> {
    Ok(crate::search::status(core, None)?.coverage)
}

/// 取一页与下一页游标。越界或空结果时下一页游标是 None。
fn page_window<T: Clone>(
    session_id: &str,
    ordered: &[T],
    start: usize,
    page_size: i64,
) -> (Vec<T>, Option<String>) {
    if start >= ordered.len() {
        return (Vec::new(), None);
    }
    let end = (start + page_size as usize).min(ordered.len());
    let page = ordered[start..end].to_vec();
    let cursor = if end < ordered.len() {
        Some(format!("{session_id}:{}", end))
    } else {
        None
    };
    (page, cursor)
}

/// 游标是 `<sessionId>:<offset>`。会话号对不上就是 `cursor_expired`——
/// 契约要求翻页绑定原会话，不能拿别的会话的游标接着翻。
fn parse_cursor(value: &str, session_id: &str) -> Result<Option<usize>> {
    let (owner, offset) = value.rsplit_once(':').ok_or_else(|| CoreError::CursorExpired {
        reason: format!("游标格式不对：{value}"),
    })?;
    if owner != session_id {
        return Err(CoreError::CursorExpired {
            reason: format!("游标属于别的会话（{owner}），不能用来翻当前会话"),
        });
    }
    Ok(Some(offset.parse::<usize>().map_err(|_| CoreError::CursorExpired {
        reason: format!("游标里的位置不是数字：{value}"),
    })?))
}

fn parse_offset(cursor: &str) -> Result<usize> {
    cursor
        .rsplit_once(':')
        .and_then(|(_, offset)| offset.parse::<usize>().ok())
        .ok_or_else(|| CoreError::CursorExpired {
            reason: format!("游标格式不对：{cursor}"),
        })
}

/// 把这一页的引用变成命中对象。整段正文只在这一步取，按页取。
///
/// 页是**显式传入**的（而不是只读 `state.last_page`）：末页之后再翻要能给出空页，
/// 又不该把会话里「当前这一页」改掉——`search.snapshot` 还要能原样再给一次。
///
/// 关键词那一路两路分两次取（两张表的 `doc_id` 各自从 1 开始，一条 UNION 就分不清
/// 来源），取完按原顺序合并；语义那一路按篇出结果，摘录与定位取该篇自己那一段；
/// 混合那一路两路都有时用关键词那份正文（有高亮），`matchedBy` 如实标。
fn materialize(
    core: &Core,
    state: &SearchSessionState,
    page: &[RankedRef],
) -> Result<SearchSnapshot> {
    let results = match state.request.mode {
        SearchMode::Semantic => {
            let candidates: Vec<semantic::SemanticCandidate> = page
                .iter()
                .filter_map(|reference| match reference {
                    RankedRef::Semantic(candidate) => Some(candidate.clone()),
                    _ => None,
                })
                .collect();
            semantic::materialize(core, &candidates)?
        }
        SearchMode::Hybrid => materialize_hybrid(core, state, page)?,
        SearchMode::Keyword => materialize_keyword(core, state, page)?,
    };

    Ok(SearchSnapshot {
        session_id: state.session_id.clone(),
        query_revision: state.query_revision,
        phase: state.phase,
        results,
        cursor: state.next_cursor.clone(),
        index_coverage: state.index_coverage,
        warnings: state.warnings.clone(),
    })
}

/// 关键词那一路的命中实体化（按传入顺序）。
fn materialize_keyword(
    core: &Core,
    state: &SearchSessionState,
    page: &[RankedRef],
) -> Result<Vec<SearchHit>> {
    let refs: Vec<search::DocRef> = page
        .iter()
        .filter_map(|reference| match reference {
            RankedRef::Keyword(doc) => Some(*doc),
            _ => None,
        })
        .collect();
    let needle = state.request.query.trim().to_lowercase();
    let mut hits = keyword_hits(core, &needle, &refs)?;
    // 按传入的顺序合并：先后只由排序键决定，不受「哪条 SQL 先返回」影响。
    Ok(refs.iter().filter_map(|doc| hits.remove(doc)).collect())
}

/// 混合那一路的命中实体化：一条融合命中只出一条 `SearchHit`。
///
/// 两路都有证据时以**关键词那份**为准：它带高亮（正文里哪几个字对上），语义那一路
/// 本来就没有「哪几个字对上」这回事。关键词那份正文取不到（重提取中间态）时退回
/// 语义那份，仍然取不到就如实跳过——不编。
fn materialize_hybrid(
    core: &Core,
    state: &SearchSessionState,
    page: &[RankedRef],
) -> Result<Vec<SearchHit>> {
    let needle = state.request.query.trim().to_lowercase();
    let keyword_refs: Vec<search::DocRef> = page
        .iter()
        .filter_map(|reference| match reference {
            RankedRef::Hybrid(entry) => entry.keyword,
            _ => None,
        })
        .collect();
    let mut keyword_hits = keyword_hits(core, &needle, &keyword_refs)?;

    let mut results = Vec::with_capacity(page.len());
    for reference in page {
        let RankedRef::Hybrid(entry) = reference else {
            continue;
        };
        let mut hit = entry
            .keyword
            .and_then(|doc| keyword_hits.remove(&doc))
            .or_else(|| {
                entry.semantic.as_ref().and_then(|candidate| {
                    semantic::materialize(core, std::slice::from_ref(candidate))
                        .ok()
                        .and_then(|hits| hits.into_iter().next())
                })
            });
        if let Some(hit) = hit.as_mut() {
            hit.matched_by.clone_from(&entry.matched_by);
            results.push(hit.clone());
        }
    }
    Ok(results)
}

/// 关键词那一路的命中，按 `DocRef` 归位（两张表的 `doc_id` 各自从 1 开始，一条
/// UNION 就分不清来源，所以分两次取）。返回 `HashMap` 是刻意的：调用方自己按
/// 传入顺序取用，先后不受「哪条 SQL 先返回」影响。
fn keyword_hits(
    core: &Core,
    needle: &str,
    refs: &[search::DocRef],
) -> Result<HashMap<search::DocRef, SearchHit>> {
    let mut segments: HashMap<search::DocRef, _> = search::load_page(core, refs)?
        .into_iter()
        .map(|row| (row.doc, row))
        .collect();
    let mut captures: HashMap<search::DocRef, _> = search::load_capture_page(core, refs)?
        .into_iter()
        .map(|row| (row.doc, row))
        .collect();

    let mut hits = HashMap::with_capacity(refs.len());
    for doc in refs {
        if let Some(row) = segments.remove(doc) {
            let (snippet, highlights) = snippet_of(&row.text, needle);
            hits.insert(
                *doc,
                SearchHit {
                    hit_id: row.segment_id.clone(),
                    group_id: row.source_id.clone(),
                    source_kind: SourceKind::from_wire(&row.kind),
                    matched_by: vec![MatchedBy::Keyword],
                    coverage: row.coverage,
                    source_id: Some(row.source_id),
                    revision_id: Some(row.source_revision_id.clone()),
                    day_key: row.day_key,
                    title: row.asset_name,
                    snippet,
                    highlights,
                    locator: Some(row.locator),
                },
            );
        } else if let Some(row) = captures.remove(doc) {
            // 记录文字就是用户自己写的原文：没有原件、没有更细的定位，所以
            // `locator` / `source_id` / `revision_id` / `title` 照实给 None，
            // 覆盖状态是 Complete（没有提取这一步可言）。一段文字天然自成一个组。
            let (snippet, highlights) = snippet_of(&row.text, needle);
            hits.insert(
                *doc,
                SearchHit {
                    hit_id: format!("cap_{}", row.capture_id),
                    group_id: row.capture_id,
                    source_kind: SourceKind::Text,
                    matched_by: vec![MatchedBy::Keyword],
                    coverage: Coverage::Complete,
                    source_id: None,
                    revision_id: None,
                    day_key: Some(row.day_key),
                    title: None,
                    snippet,
                    highlights,
                    locator: None,
                },
            );
        }
    }
    Ok(hits)
}

/// 命中附近的摘录与高亮。高亮下标**相对摘录**，不是相对整段正文。
fn snippet_of(text: &str, needle: &str) -> (Option<String>, Vec<TextRange>) {
    let characters: Vec<char> = text.chars().collect();
    let needle_length = needle.chars().count();
    let lowered = text.to_lowercase();
    let match_start = lowered
        .find(needle)
        .map(|byte_index| lowered[..byte_index].chars().count());
    let Some(start) = match_start else {
        return (None, Vec::new());
    };

    let window_start = start.saturating_sub(SNIPPET_PADDING);
    let window_end = (start + needle_length + SNIPPET_PADDING).min(characters.len());
    let mut snippet = String::new();
    let mut offset = 0_i64;
    if window_start > 0 {
        snippet.push('…');
        offset = 1;
    }
    snippet.extend(&characters[window_start..window_end]);
    if window_end < characters.len() {
        snippet.push('…');
    }

    let highlight_start = (start - window_start) as i64 + offset;
    (
        Some(snippet),
        vec![TextRange {
            start: highlight_start,
            end: highlight_start + needle_length as i64,
        }],
    )
}

/// 命中的排序键：日期从近到远，再按出现位置，再按家族，最后按写入顺序。
///
/// 家族序（片段在前、记录文字在后）是必需的：两张表的 `doc_id` 各自从 1 开始，
/// 同日期、同位置时直接比 `doc_id` 的话，片段 1 与记录 1 谁排前面就取决于这个
/// 撞号的巧合——结果不可复现，测试也会飘。
///
/// 最后一项 `doc_id` 是**库内**顺序，跨库不稳定。混合检索读这个键时只用前三项
/// （日期、位置、家族）判并列，见 `hybrid_search` 里的同名次；跨库稳定交给融合
/// 自己的内容哈希。纯关键词那一路仍按此键排（这条口径没变）。
///
/// 「整词命中优先」仍然没做：那需要在候选阶段为每一行查一次词表，代价随候选数
/// 线性增长；混合检索的分数来自 RRF 名次，不来自关键词侧的整词匹配。
pub(crate) fn sort_key(row: &search::RankedMatch) -> (i64, i64, i64, i64) {
    // 没有 day_key 的排最后（用很小的负数代表「很旧」）。
    let day = row
        .day_key
        .as_deref()
        .and_then(|value| {
            // day_key 是 YYYY-MM-DD，直接当字符串比较即可，排序时转成可比数字。
            let mut parts = value.split('-');
            let year = parts.next()?.parse::<i64>().ok()?;
            let month = parts.next()?.parse::<i64>().ok()?;
            let day = parts.next()?.parse::<i64>().ok()?;
            Some(year * 10_000 + month * 100 + day)
        })
        .unwrap_or(i64::MIN);
    (-day, row.position, i64::from(row.doc.capture), row.doc.doc_id)
}

/// 会话表只给本模块用；放在 `Core` 上的小访问器避免把字段公开出去。
impl Core {
    pub(crate) fn search_sessions(&self) -> &HashMap<String, SearchSessionState> {
        &self.search_sessions
    }

    pub(crate) fn search_sessions_mut(&mut self) -> &mut HashMap<String, SearchSessionState> {
        &mut self.search_sessions
    }

    /// 混合检索的语义证据门槛。产品路径用 `HYBRID_SEMANTIC_MIN_SCORE`；
    /// 量测/测试可以改（见 `set_hybrid_min_score_for_test`）。
    pub(crate) fn hybrid_min_score(&self) -> f32 {
        self.hybrid_min_score
    }
}

/// 过滤条件转成 SQL 参数；这里只做类型检查，不做语义解释。
pub(crate) fn filters_to_params(filters: &SearchFilters) -> Result<(Option<String>, Option<String>)> {
    let kinds = if filters.kinds.is_empty() {
        None
    } else {
        Some(serde_json::to_string(
            &filters
                .kinds
                .iter()
                .map(|kind| kind.wire())
                .collect::<Vec<_>>(),
        )?)
    };
    // `Some([])` 也照原样序列化成空 JSON 数组：`IN (SELECT value FROM json_each('[]'))`
    // 匹配不到任何行，于是空范围是「什么都不看」。早先这里把空数组折成 `None`
    // （=看全部），那与 `indexes.status` 的口径相反，同一组过滤条件会在状态与结果
    // 里给出互相矛盾的答案。
    let scope = match &filters.source_scope {
        Some(values) => Some(serde_json::to_string(values)?),
        None => None,
    };
    Ok((kinds, scope))
}

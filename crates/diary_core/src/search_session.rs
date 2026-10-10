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

use std::collections::HashMap;

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

/// 索引指纹：关键词索引代次 + 语义生效代次。会话期间有人写过索引、或语义索引
/// 换了一代，它就会变，快照随即作废。
///
/// 原来只有关键词那一个代次。语义这一路要跟着生效代次走：一代向量被切走之后，
/// 会话手里的「块 → 篇」排序已经过期，旧结果不该继续糊在新界面上。
/// 关键词会话不看语义代次，语义修改不会无谓地打断关键词翻页。
type IndexFingerprint = (i64, Option<i64>);

/// 会话里一条命中的引用。两条路的「一条结果」不是同一种东西：
///
/// - 关键词：一个索引文档（派生片段 / 记录文字），正文按页现取；
/// - 语义：**一篇**（消费契约要求按篇上报），证据块与篇内区间一起记着。
///
/// 会话里只存引用，正文仍然按页现取。
#[derive(Debug, Clone)]
pub(crate) enum RankedRef {
    Keyword(search::DocRef),
    Semantic(semantic::SemanticCandidate),
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

    if request.mode == SearchMode::Hybrid {
        // 混合检索是 #51（B3c-3）。**如实降级**到关键词并说清楚——假装按混合跑了、
        // 却不出分数，是骗人。纯语义（`Semantic`）不再降级：它真的跑语义那一路。
        warnings.push(
            "混合检索还没实现（#51），这次按关键词执行；语义那一路可以单独用 semantic 模式"
                .to_owned(),
        );
    }
    if request.filters.include_old_diary_versions {
        warnings.push("日记历史版本还没有进索引，这个开关暂时没有效果".to_owned());
    }

    let (ordered, index_coverage, phase) = if request.mode == SearchMode::Semantic {
        semantic_search(
            core,
            &request,
            &needle,
            page_size,
            &mut warnings,
        )?
    } else {
        keyword_search(core, &request, &needle, page_size, &mut warnings)?
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

/// 关键词会话的指纹是关键词代次；语义会话还要带上生效的向量代次——切了一代之后
/// 旧的「块 → 篇」排序已经过期。关键词会话**不看**语义代次：向量换代不该无谓地
/// 打断正在翻页的关键词查询。
fn fingerprint(core: &Core, mode: SearchMode) -> Result<IndexFingerprint> {
    let epoch = search::index_epoch(core)?;
    if mode == SearchMode::Semantic {
        Ok((epoch, semantic::active_generation(core)?))
    } else {
        Ok((epoch, None))
    }
}

/// 一页之后该报什么阶段。语义没有「后面还会更好」的阶段，恒为 `done`。
fn phase_after_page(mode: SearchMode, is_last_page: bool) -> SearchPhase {
    if is_last_page || mode == SearchMode::Semantic {
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
/// 来源），取完按原顺序合并；语义那一路按篇出结果，摘录与定位取该篇自己那一段。
fn materialize(
    core: &Core,
    state: &SearchSessionState,
    page: &[RankedRef],
) -> Result<SearchSnapshot> {
    let results = if state.request.mode == SearchMode::Semantic {
        let candidates: Vec<semantic::SemanticCandidate> = page
            .iter()
            .filter_map(|reference| match reference {
                RankedRef::Semantic(candidate) => Some(candidate.clone()),
                RankedRef::Keyword(_) => None,
            })
            .collect();
        semantic::materialize(core, &candidates)?
    } else {
        materialize_keyword(core, state, page)?
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

/// 关键词那一路的命中实体化。
fn materialize_keyword(
    core: &Core,
    state: &SearchSessionState,
    page: &[RankedRef],
) -> Result<Vec<SearchHit>> {
    let needle = state.request.query.trim().to_lowercase();
    let refs: Vec<search::DocRef> = page
        .iter()
        .filter_map(|reference| match reference {
            RankedRef::Keyword(doc) => Some(*doc),
            RankedRef::Semantic(_) => None,
        })
        .collect();
    // 两路分开取：两张表的 doc_id 各自从 1 开始，一条 UNION 就分不清来源。
    let mut segments: HashMap<search::DocRef, _> = search::load_page(core, &refs)?
        .into_iter()
        .map(|row| (row.doc, row))
        .collect();
    let mut captures: HashMap<search::DocRef, _> = search::load_capture_page(core, &refs)?
        .into_iter()
        .map(|row| (row.doc, row))
        .collect();

    // 按传入的顺序合并：先后只由排序键决定，不受「哪条 SQL 先返回」影响。
    let mut results = Vec::with_capacity(refs.len());
    for doc in &refs {
        if let Some(row) = segments.remove(doc) {
            let (snippet, highlights) = snippet_of(&row.text, &needle);
            results.push(SearchHit {
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
            });
        } else if let Some(row) = captures.remove(doc) {
            // 记录文字就是用户自己写的原文：没有原件、没有更细的定位，所以
            // `locator` / `source_id` / `revision_id` / `title` 照实给 None，
            // 覆盖状态是 Complete（没有提取这一步可言）。一段文字天然自成一个组。
            let (snippet, highlights) = snippet_of(&row.text, &needle);
            results.push(SearchHit {
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
            });
        }
    }
    Ok(results)
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
/// 为什么不用「整词命中优先」：那需要在候选阶段为每一行查一次词表，代价随候选数
/// 线性增长；这一片先把可解释的排序做出来，等混合检索带分数进来再一起做。
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

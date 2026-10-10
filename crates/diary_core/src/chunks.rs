//! 文本块：把「篇」按 B3c-1 的打包规则（`chunker`）切成可存储、可换代的块，写进
//! `text_chunks` 与 `chunk_spans`。
//!
//! **这一片不接模型**：只建块，不算向量。所以 `chunk_vectors` / `index_meta` 只建了
//! 结构，`embedded_chunks` 恒为 0，状态里的 `semantic_index_ready` 也就只能是 false。
//!
//! 三条口径（消费侧的解释见 `docs/architecture/m2-向量索引与换代.md`，别处不要另立）：
//!
//! 1. **块的单位是「日子」，不是「记录」**。B3c-1 的打包规则是「同一天的短篇可以合进
//!    一块」，而「同一天的短篇」可能来自**不同记录**——所以块上没有 `capture_id`，
//!    一块属于哪几篇由 `chunk_spans` 回答。用户看到的东西仍然以「篇」为单位。
//! 2. **span 上是权威值**。每一段自己的 `capture_id` / `kind` / `coverage` 写在
//!    `chunk_spans` 上；`text_chunks` 上的 `kind` / `coverage` 只是便于块级过滤的
//!    聚合（块 `kind` 取第一段，块 `coverage` 取各段里最弱的）。
//! 3. **`start_char` / `end_char` 是篇内区间**，不是块内偏移：摘录要用该篇自己那一段，
//!    而不是整块拼接文本（整块里混着别的篇，给用户看是不对的）。
//!
//! 正文从哪来：**只读库里已有的东西**，绝不重新解析原件。
//!
//! - 来源篇：`source_items.current_revision_id` → `extracted_contents` →
//!   `extracted_segments` 按 `ordinal` 用 `\n` 连接。用 `\n` 连接是为了保住**段尾**
//!   ——B3c-1 的断点只认换行，把段落粘成一整行会让「段尾断」失效。
//! - 记录自己的文字：`captures.draft_text`。B3d 已经把它纳入关键词检索，语义这一路
//!   也必须能搜到自己写的话。
//!
//! **去重规则**：`kind = 'text'` 且 `author_type = 'user'` 的来源**跳过**——它的正文
//! 就是它所属记录的 `draft_text`（`commit` 时复制出来的版本，见 `Core::commit`），
//! 不跳过就会有同一句话的两个载体：一条来自 `draft_text`、一条来自这个来源的派生片段
//! （issue #44 记过这个坑）。规则写在 SQL 的 `NOT (...)` 里，并有测试钉住。

use std::collections::HashMap;

use rusqlite::{params, params_from_iter};

use crate::chunker::{pack_pieces, Piece, CHUNKER_VERSION};
use crate::error::Result;
use crate::model::Coverage;
use crate::search::placeholders;
use crate::support;
use crate::Core;

/// 一篇待打包的正文。`chunker::Piece` 只借用文本，所以这里自己持有。
struct PieceRow {
    /// 篇的标识：来源篇用 `source_id`，记录自己的文字用 `capture_id`。
    ///
    /// 两者不会撞号（`support::new_id` 的前缀不同：`src_` / `cap_`），所以可以用
    /// 一个字段当键；关键词那一路的 `groupId` 也正是这两个值，语义这一路必须与它一致。
    piece_id: String,
    capture_id: String,
    /// 来源篇是当前修订的 id；记录自己的文字是**空串**（它不对应任何 source 修订，
    /// 消费者应回到 `captures.draft_text` 去找原文）。
    source_revision_id: String,
    day_key: String,
    kind: String,
    coverage: Coverage,
    text: String,
    /// 打包顺序：`day_key` → `occurred_at` → `capture_id` → 篇内位置（`draft_text` 在
    /// -1，排在本记录的来源前面）→ 篇标识。
    ///
    /// 排序必须**确定**，而且 `day_key` 必须在最前：B3c-1 只在「相邻两篇同一天」时才
    /// 合块，按 `occurred_at` 排会让不同日子的篇交错（本地日期与 UTC 顺序可以不一致），
    /// 于是每一篇都各自成块、打包规则形同失效。
    order: (String, String, String, i64, String),
}

/// 库里块的计数。`total` 是范围内的块数，`embedded` 是其中有生效代次向量的块数。
pub(crate) struct ChunkCounts {
    pub total: i64,
    pub embedded: i64,
}

/// 一篇来源篇的查询结果（`collect_pieces` 内部用）。
struct SourcePieceRow {
    source_id: String,
    capture_id: String,
    kind: String,
    position: i64,
    source_revision_id: String,
    coverage: String,
    content_id: String,
    day_key: String,
    occurred_at: String,
}

/// `index_meta` 里的两个代次：正在服务的、正在构建的。两个都为 `None` 表示还没有
/// 任何一代向量（这一片没有模型，所以必然如此）。
pub(crate) fn generations(core: &Core) -> Result<(Option<i64>, Option<i64>)> {
    let pair = core.conn.query_row(
        "SELECT active_generation, building_generation FROM index_meta WHERE id = 1",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    Ok(pair)
}

/// 重建文本块；`source_scope` 为空表示整个资料库。返回写进库的块数。
///
/// 范围语义与 `search::rebuild` 一致：
///
/// - `None` = 全库；
/// - `Some([])` = 什么都不看，产出 0 块（**不**退化成「看全部」）；
/// - `Some(scope)` = 拥有范围内来源的那些记录。
///
/// 重打包的单位是**日子**：分块规则按 `day_key` 把同一天的短篇合块，所以只重打包
/// 范围内那几篇会让「同一天的块」随重建范围变化，同一份内容在两次不同范围的重建之后
/// 会有两种块边界。这里的做法是：算出范围内记录所在的那些日子，**整日删除、整日重算**
/// ——于是「按范围重建」的结果与「全库重建之后取这些日子」完全一致。代价是范围重建也会
/// 顺带重写同一天里范围外那几篇的块；这是为了不把它们的块删掉却不再建（那会让已经索引
/// 过的内容凭空消失）。
pub(crate) fn rebuild(core: &mut Core, source_scope: Option<&[String]>) -> Result<i64> {
    // 空范围：什么都不看，连事务都不用开（没有要删的、也没有要写的）。
    if matches!(source_scope, Some([])) {
        return Ok(0);
    }

    // `?` 的份数就是 scope 的长度。下面每条语句里这个子查询**只出现一次**，所以
    // 绑定一份即可；写成两份会报 InvalidParameterCount（B3d 在状态查询上踩过这个坑）。
    let scope_values: Vec<String> = source_scope.map(<[String]>::to_vec).unwrap_or_default();
    // 「拥有范围内来源的记录」。
    let in_scope = match source_scope {
        Some(scope) => format!(
            "SELECT si.capture_id FROM source_items si WHERE si.source_id IN ({})",
            placeholders(scope.len())
        ),
        None => String::new(),
    };
    // 要重算的日子。`None` 时不做日子过滤（整库）。
    let day_filter = if in_scope.is_empty() {
        String::new()
    } else {
        format!(
            " AND c.day_key IN (SELECT c2.day_key FROM captures c2 WHERE c2.id IN ({in_scope}))"
        )
    };
    // 要重算的日子（删除时用同一个条件）。
    let day_condition = if in_scope.is_empty() {
        "1 = 1".to_owned()
    } else {
        format!("day_key IN (SELECT c.day_key FROM captures c WHERE c.id IN ({in_scope}))")
    };

    let tx = core.conn.transaction()?;

    // 整日取篇：范围重建要连范围外同一天的那几篇一起重算（见函数说明）。
    let pieces = collect_pieces(&tx, &day_filter, &scope_values)?;

    let refs: Vec<Piece<'_>> = pieces
        .iter()
        .map(|piece| Piece {
            id: &piece.piece_id,
            day_key: &piece.day_key,
            text: &piece.text,
        })
        .collect();
    let packed = pack_pieces(&refs);
    let by_id: HashMap<&str, &PieceRow> = pieces
        .iter()
        .map(|piece| (piece.piece_id.as_str(), piece))
        .collect();

    // **只删已经消失的块**，不是先把整张表清掉。
    //
    // 块 id 是内容的函数（见 `chunk_id_of`），所以内容没变的块重建后还是同一个 id。
    // 保留它意味着保留它的向量——否则「重建块 → 算向量 → 切代次」这条路上，重建会
    // 把正在服务的那一代向量**级联删掉**（`chunk_vectors` 是 `ON DELETE CASCADE`），
    // 换代期间语义检索出现空洞，代次机制也就白设了。
    let new_ids: Vec<String> = packed
        .iter()
        .map(|chunk| {
            let day_key = by_id
                .get(chunk.spans[0].piece_id.as_str())
                .expect("打包出来的块必然由收集到的篇组成")
                .day_key
                .as_str();
            chunk_id_of(day_key, chunk)
        })
        .collect();
    delete_stale_chunks(&tx, &day_condition, &scope_values, &new_ids)?;

    if packed.is_empty() {
        tx.commit()?;
        return Ok(0);
    }

    let now = support::to_iso(support::now());
    let mut written = 0_i64;
    // 同一天内的序号：`ordinal` 是「这一天里的第几块」，与块的 id 无关，所以内容
    // 变了它就跟着变——这是给人看与排序用的，不作为身份。
    let mut current_day = String::new();
    let mut ordinal = 0_i64;
    for chunk in &packed {
        let first = by_id
            .get(chunk.spans[0].piece_id.as_str())
            .expect("打包出来的块必然由收集到的篇组成");
        if first.day_key != current_day {
            current_day = first.day_key.clone();
            ordinal = 0;
        }
        write_chunk(&tx, chunk, first, ordinal, &by_id, &now)?;
        ordinal += 1;
        written += 1;
    }
    tx.commit()?;
    Ok(written)
}

/// 一条块与它的 span 的落库。
fn write_chunk(
    tx: &rusqlite::Transaction<'_>,
    chunk: &crate::chunker::Chunk,
    first: &PieceRow,
    ordinal: i64,
    by_id: &HashMap<&str, &PieceRow>,
    now: &str,
) -> Result<()> {
    let day_key = first.day_key.as_str();
    // 块的 `kind` 取第一段（权威值在 span 上，这里只是便于块级过滤的聚合）；
    // 块的 `coverage` 取各段里最弱的那个（最弱的那个才是这一块真实的可信程度）。
    let coverage = chunk
        .spans
        .iter()
        .map(|span| {
            by_id
                .get(span.piece_id.as_str())
                .expect("span 的篇必须来自本次收集")
                .coverage
        })
        .min_by_key(|coverage| coverage_rank(*coverage))
        .expect("块至少有一段");

    let chunk_id = chunk_id_of(day_key, chunk);

    // **UPSERT，不是 `INSERT OR REPLACE`**：REPLACE 在 SQLite 里等于「先删后插」，
    // 会触发 `chunk_vectors` 的级联删除——正好把这次重建想保住的东西删掉。
    tx.execute(
        "INSERT INTO text_chunks (chunk_id, ordinal, text, kind, coverage, day_key, \
         chunker_version, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8) \
         ON CONFLICT(chunk_id) DO UPDATE SET ordinal = excluded.ordinal, text = excluded.text, \
         kind = excluded.kind, coverage = excluded.coverage, day_key = excluded.day_key, \
         chunker_version = excluded.chunker_version, created_at = excluded.created_at",
        params![
            chunk_id,
            ordinal,
            chunk.text,
            first.kind,
            coverage.wire(),
            day_key,
            CHUNKER_VERSION,
            now,
        ],
    )?;
    // span 没有下游引用，先删后写最省事；向量挂在块上，不受这里影响。
    tx.execute(
        "DELETE FROM chunk_spans WHERE chunk_id = ?1",
        params![chunk_id],
    )?;
    for (span_ordinal, span) in chunk.spans.iter().enumerate() {
        let piece = by_id
            .get(span.piece_id.as_str())
            .expect("span 的篇必须来自本次收集");
        tx.execute(
            "INSERT INTO chunk_spans (chunk_id, ordinal, capture_id, source_id, \
             source_revision_id, start_char, end_char, kind, coverage) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                chunk_id,
                span_ordinal as i64,
                piece.capture_id,
                piece.piece_id,
                piece.source_revision_id,
                span.start,
                span.end,
                piece.kind,
                piece.coverage.wire(),
            ],
        )?;
    }
    Ok(())
}

/// 块 id 是**内容的函数**，不是随机 id：同一份内容反复重建得到同一批 id。
///
/// 为什么必须是内容的函数：向量按 `chunk_id` 存，(a) 只比 id 就能看出「哪些块是新的」，
/// 不必逐块比文本；(b) 内容没变的块能在重建后保住自己的向量——这是「重建块 → 算向量
/// → 切代次」不出现空洞的前提。`support::new_id` 用的是带时间戳的 v7 UUID，两次重建
/// 必然不同，做不了这两件事。
fn chunk_id_of(day_key: &str, chunk: &crate::chunker::Chunk) -> String {
    let mut parts: Vec<String> = vec![
        CHUNKER_VERSION.to_owned(),
        day_key.to_owned(),
        chunk.text.clone(),
    ];
    for span in &chunk.spans {
        parts.push(span.piece_id.clone());
        parts.push(span.start.to_string());
        parts.push(span.end.to_string());
    }
    format!(
        "chunk_{}",
        support::fingerprint(&parts.iter().map(String::as_str).collect::<Vec<_>>())
    )
}

/// 删掉「这次重建之后已经不存在」的块（连同它们的 span 与向量）。
///
/// `keep` 为空表示这一天（或整库）不该有任何块，全部删掉。绑定顺序与 SQL 里的
/// 占位符一致：先 scope（`day_condition` 里那一个子查询），再 `keep` 列表。
fn delete_stale_chunks(
    tx: &rusqlite::Transaction<'_>,
    day_condition: &str,
    scope_values: &[String],
    keep: &[String],
) -> Result<()> {
    let sql = if keep.is_empty() {
        format!("DELETE FROM text_chunks WHERE {day_condition}")
    } else {
        format!(
            "DELETE FROM text_chunks WHERE {day_condition} AND chunk_id NOT IN ({})",
            placeholders(keep.len())
        )
    };
    let mut values: Vec<&str> = scope_values.iter().map(String::as_str).collect();
    values.extend(keep.iter().map(String::as_str));
    tx.execute(&sql, params_from_iter(values))?;
    Ok(())
}

/// 收集要打包的篇：记录自己的文字 + 范围内来源的当前修订正文。
///
/// 两路都用同一套「日子」过滤（`day_filter`）取，而不是先查出范围内记录的 id 再
/// `IN (...)`：整库重建时那可能是上万个 id，而 SQLite 的变量数有上限。
/// `day_filter` 里的 `?` 份数正好是 `scope_values` 的长度。
fn collect_pieces(
    tx: &rusqlite::Transaction<'_>,
    day_filter: &str,
    scope_values: &[String],
) -> Result<Vec<PieceRow>> {
    let mut pieces: Vec<PieceRow> = Vec::new();

    // 记录自己的文字：非空才算（空/全空白的篇由 `pack_pieces` 的 trim 兜底跳过）。
    //
    // 在回收站里的记录照样收：`search::rebuild` 也不按 state 过滤（检索时才过滤），
    // 两边保持一致——块是派生物，范围大小不该因为一条记录进回收站就变。
    {
        let sql = format!(
            "SELECT c.id, c.day_key, c.occurred_at, c.draft_text FROM captures c \
             WHERE 1=1{day_filter} AND {} ORDER BY c.day_key, c.occurred_at, c.id",
            crate::search::non_empty_text("c.draft_text")
        );
        let mut statement = tx.prepare(&sql)?;
        let rows = statement
            .query_map(params_from_iter(scope_values), |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for (capture_id, day_key, occurred_at, text) in rows {
            pieces.push(PieceRow {
                piece_id: capture_id.clone(),
                capture_id: capture_id.clone(),
                // 记录自己的文字没有对应的 source 修订：`captures.draft_text` 才是
                // 它的正文。留空串而不是硬塞一个修订 id——`revise_text` 之后记录里
                // 的 source 文字与 `draft_text` 已经不是同一份了，写一个「看起来对」
                // 的修订反而会误导消费者。
                source_revision_id: String::new(),
                kind: "text".to_owned(),
                // 用户自己写下来的原文：没有「只提取到一部分」这回事。
                coverage: Coverage::Complete,
                order: (
                    day_key.clone(),
                    occurred_at,
                    capture_id.clone(),
                    -1,
                    capture_id.clone(),
                ),
                day_key,
                text,
            });
        }
    }

    // 来源篇：当前修订 + 当前修订的派生内容。JOIN 而不是 LEFT JOIN 是刻意的——没有
    // 当前修订、或当前修订还没提取过的来源，本来就没有正文可打包。
    {
        let sql = format!(
            "SELECT si.source_id, si.capture_id, si.kind, si.position, ec.source_revision_id, \
                    ec.coverage, ec.id, c.day_key, c.occurred_at \
             FROM source_items si \
             JOIN captures c ON c.id = si.capture_id \
             JOIN source_revisions sr ON sr.revision_id = si.current_revision_id \
             JOIN extracted_contents ec ON ec.source_id = si.source_id \
                  AND ec.source_revision_id = si.current_revision_id \
             WHERE 1=1{day_filter} AND NOT (si.kind = 'text' AND sr.author_type = 'user') \
             ORDER BY c.day_key, c.occurred_at, c.id, si.position, si.source_id"
        );
        let rows: Vec<SourcePieceRow> = {
            let mut statement = tx.prepare(&sql)?;
            let rows = statement
                .query_map(params_from_iter(scope_values), |row| {
                    Ok(SourcePieceRow {
                        source_id: row.get(0)?,
                        capture_id: row.get(1)?,
                        kind: row.get(2)?,
                        position: row.get(3)?,
                        source_revision_id: row.get(4)?,
                        coverage: row.get(5)?,
                        content_id: row.get(6)?,
                        day_key: row.get(7)?,
                        occurred_at: row.get(8)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows
        };
        for row in rows {
            let text = segment_text(tx, &row.content_id)?;
            pieces.push(PieceRow {
                piece_id: row.source_id.clone(),
                capture_id: row.capture_id.clone(),
                source_revision_id: row.source_revision_id,
                kind: row.kind,
                coverage: Coverage::from_wire(&row.coverage).unwrap_or(Coverage::Unavailable),
                order: (
                    row.day_key.clone(),
                    row.occurred_at,
                    row.capture_id,
                    row.position,
                    row.source_id,
                ),
                day_key: row.day_key,
                text,
            });
        }
    }

    pieces.sort_by(|left, right| left.order.cmp(&right.order));
    Ok(pieces)
}

/// 一篇来源的正文：`extracted_segments` 按 `ordinal` 用 `\n` 连接。
///
/// 用 `\n` 而不是空格：B3c-1 的断点只认段尾（换行），粘成一整行会让「段尾断」失效。
/// 于是 span 的 `start_char` / `end_char` 是**这个连接结果里**的字符区间——消费者要
/// 还原这一段，也按同一条规则拼（不要把区间当成 `extracted_contents.text` 的下标）。
fn segment_text(tx: &rusqlite::Transaction<'_>, content_id: &str) -> Result<String> {
    let mut statement =
        tx.prepare("SELECT text FROM extracted_segments WHERE content_id = ?1 ORDER BY ordinal")?;
    let rows = statement
        .query_map(params![content_id], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows.join("\n"))
}

/// 覆盖程度的强弱：`unavailable` < `metadata_only` < `partial` < `complete`。
///
/// 块的聚合 `coverage` 取最弱的那一段：一块里只要有一段是残缺的，这一块就不是完整的。
fn coverage_rank(coverage: Coverage) -> u8 {
    match coverage {
        Coverage::Unavailable => 0,
        Coverage::MetadataOnly => 1,
        Coverage::Partial => 2,
        Coverage::Complete => 3,
    }
}

/// 范围内的块数与其中有生效代次向量的块数。
///
/// 范围语义与关键词那一路同一套：一条记录属于范围，当且仅当它拥有范围内的来源；
/// 空白范围是「什么都不看」。块只要有一段来自范围里的记录，就算范围里的块——块可能
/// 混着范围外的篇，但它的内容确实属于这个范围。
pub(crate) fn counts(core: &Core, source_scope: Option<&[String]>) -> Result<ChunkCounts> {
    let scope_values: Vec<String> = source_scope.map(<[String]>::to_vec).unwrap_or_default();
    // 范围判断只需要「这一段所属的记录拥有范围内的来源」，与关键词那一路同一套语义。
    let scope_filter = match source_scope {
        None => String::new(),
        Some([]) => " AND 0".to_owned(),
        Some(scope) => format!(
            " AND EXISTS (SELECT 1 FROM source_items si WHERE si.capture_id = s.capture_id \
             AND si.source_id IN ({}))",
            placeholders(scope.len())
        ),
    };

    let total_sql = format!(
        "SELECT COUNT(*) FROM text_chunks t WHERE EXISTS \
         (SELECT 1 FROM chunk_spans s WHERE s.chunk_id = t.chunk_id{scope_filter})"
    );
    let total = core
        .conn
        .query_row(&total_sql, params_from_iter(&scope_values), |row| row.get(0))?;

    // 没有生效代次就是 0：不是「碰巧没查到」，是没有可算的代次。
    let embedded = match generations(core)?.0 {
        None => 0,
        Some(generation) => {
            let sql = format!(
                "SELECT COUNT(*) FROM text_chunks t WHERE EXISTS \
                 (SELECT 1 FROM chunk_spans s WHERE s.chunk_id = t.chunk_id{scope_filter}) \
                 AND EXISTS (SELECT 1 FROM chunk_vectors v WHERE v.chunk_id = t.chunk_id \
                 AND v.generation = ?)"
            );
            // 占位符顺序：范围过滤在前（`scope_values.len()` 个），代次在后。
            let mut values: Vec<&dyn rusqlite::ToSql> = scope_values
                .iter()
                .map(|value| value as &dyn rusqlite::ToSql)
                .collect();
            values.push(&generation);
            core.conn
                .query_row(&sql, params_from_iter(values), |row| row.get(0))?
        }
    };
    Ok(ChunkCounts { total, embedded })
}

//! 关键词索引：jieba 分词 + 2-gram 补充（并集），以及索引覆盖状态。
//!
//! 选型来自 M0 实测（`docs/architecture/M0-技术验证.md` 第 2 节）：中文连续文本
//! 用 `unicode61` 是 0 召回，`trigram` 只覆盖三字以上，所以要自己建索引。
//!
//! 三条不能丢的结论，都体现在下面的代码里：
//!
//! 1. **单字也要进索引**。任务书要求一至二字查询可靠；jieba 会把「妈妈」切成
//!    一个词，只建词的索引就查不到「妈」。
//! 2. **2-gram 与分词是并集，不是回退**。「面试官」会被切成一个词，查询「面试」
//!    在分词索引里查不到那部分记录，但它们在 2-gram 索引里是有的。只做回退
//!    就永远补不回这些。
//! 3. **候选之后必须回原文核对**。2-gram 只是候选生成：`妈妈` 的两个 gram 都在
//!    的片段里，也可能出现「妈……妈」而不连续。核对用 `instr`，所以召回是精确的
//!    ——一条真正包含查询的文本一定包含查询的全部 gram。
//!
//! 词项统一存小写。SQLite 的 `lower()` 只折叠 ASCII，中文没有大小写，
//! 其余语系的大小写折叠不在这一片的能力范围内（见文档「没有做到的」）。

use std::collections::HashSet;
use std::sync::OnceLock;

use jieba_rs::Jieba;
use rusqlite::{params, Transaction};

use crate::error::Result;
use crate::model::{Coverage, IndexStatus};
use crate::Core;

/// 分词器与索引布局的版本。
///
/// 改了分词方式、gram 宽度或词项归一化，就要改这个字符串：旧行按版本识别为
/// 过期，需要重建，而不是同新行混在一张表里。
pub const TOKENIZER_VERSION: &str = "jieba-0.11-gram1-v1";

/// jieba 词典是懒加载的。
///
/// 核心启动时不加载它（任务书 2 节：启动不做重活）；建索引或检索时才付
/// 词典的解析代价。词典常驻内存的数字记在架构文档里。
fn jieba() -> &'static Jieba {
    static JIEBA: OnceLock<Jieba> = OnceLock::new();
    JIEBA.get_or_init(Jieba::new)
}

/// 建索引时保留的字符：空白与控制字符不建。
///
/// 其余一律保留——标点、emoji、路径分隔符都可能是用户真的会搜索的东西
/// （「原样输入是普通检索文字」）。
fn meaningful(character: char) -> bool {
    !character.is_whitespace() && !character.is_control()
}

/// 一段文本的词项集合：单字 + 每段连续文本的 2-gram + jieba 词。
///
/// 用集合去重：同一个 gram 在一段里出现多次只写一行，`(term, doc_id)` 主键
/// 本来也只允许一行，先在这里去重可以省掉大量重复的 INSERT。
pub fn grams_for(text: &str) -> Vec<String> {
    let lowered = text.to_lowercase();
    let mut grams: HashSet<String> = HashSet::new();

    // 按空白切成连续段：跨空白造出来的 2-gram（"a b" 造出 "ab"）不是文本里
    // 真实存在的子串，进索引只会让候选集虚胖。
    for run in lowered.split(|character: char| !meaningful(character)) {
        let characters: Vec<char> = run.chars().collect();
        for character in &characters {
            grams.insert(character.to_string());
        }
        for window in characters.windows(2) {
            grams.insert(window.iter().collect());
        }
    }

    for token in jieba().cut(&lowered, false) {
        let word = token.word.trim();
        if word.chars().count() >= 2 && word.chars().all(meaningful) {
            grams.insert(word.to_owned());
        }
    }

    grams.into_iter().collect()
}

/// 待索引的一个片段。来源与内容 ID 不进来：索引表里不重复存它们。
pub(crate) struct SegmentInput<'a> {
    pub segment_id: &'a str,
    pub text: &'a str,
}

/// 把一段派生文本写进索引，在调用方的事务里执行。
///
/// 可重入：同一个 `segment_id` 先清后写，不会留旧词项。
pub(crate) fn index_segment(tx: &Transaction<'_>, segment: SegmentInput<'_>) -> Result<()> {
    tx.execute(
        "DELETE FROM search_docs WHERE segment_id = ?1",
        params![segment.segment_id],
    )?;
    tx.execute(
        "INSERT INTO search_docs (segment_id, text_length, tokenizer_version) \
         VALUES (?1, ?2, ?3)",
        params![
            segment.segment_id,
            segment.text.chars().count() as i64,
            TOKENIZER_VERSION,
        ],
    )?;
    let doc_id = tx.last_insert_rowid();

    {
        let mut statement =
            tx.prepare("INSERT OR IGNORE INTO search_grams (term, doc_id) VALUES (?1, ?2)")?;
        for gram in grams_for(segment.text) {
            statement.execute(params![gram, doc_id])?;
        }
    }
    Ok(())
}

/// 重建一份派生内容的索引。返回索引了多少个片段。
pub(crate) fn index_content(core: &mut Core, content_id: &str) -> Result<i64> {
    let tx = core.conn.transaction()?;
    // 先清这一份内容的旧索引：重复索引不该产生重复行。
    tx.execute(
        "DELETE FROM search_docs WHERE segment_id IN \
         (SELECT id FROM extracted_segments WHERE content_id = ?1)",
        params![content_id],
    )?;

    let segments: Vec<(String, String)> = {
        let mut statement = tx.prepare(
            "SELECT id, text FROM extracted_segments WHERE content_id = ?1 ORDER BY ordinal",
        )?;
        let rows = statement
            .query_map(params![content_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };

    let mut indexed = 0_i64;
    for (segment_id, text) in &segments {
        index_segment(
            &tx,
            SegmentInput {
                segment_id,
                text,
            },
        )?;
        indexed += 1;
    }
    tx.commit()?;
    Ok(indexed)
}

/// 重建关键词索引。`source_scope` 为空表示整个资料库。
///
/// 整库重建是重活，产品路径上它应该是一个可取消的任务；这一片先提供同步入口，
/// 让覆盖状态与增量索引有可靠的校准方式（`indexes.rebuild` 的任务化见 #31）。
pub(crate) fn rebuild(core: &mut Core, source_scope: Option<&[String]>) -> Result<i64> {
    let content_ids: Vec<String> = match source_scope {
        Some(scope) => {
            let sql = format!(
                "SELECT id FROM extracted_contents WHERE source_id IN ({})",
                placeholders(scope.len())
            );
            let mut statement = core.conn.prepare(&sql)?;
            let rows = statement
                .query_map(rusqlite::params_from_iter(scope.iter()), |row| {
                    row.get::<_, String>(0)
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows
        }
        None => {
            let mut statement = core.conn.prepare("SELECT id FROM extracted_contents")?;
            let rows = statement
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows
        }
    };

    let mut indexed = 0_i64;
    for content_id in &content_ids {
        indexed += index_content(core, content_id)?;
    }
    Ok(indexed)
}

/// 索引覆盖状态。`source_scope` 为空表示整个资料库。
pub(crate) fn status(core: &Core, source_scope: Option<&[String]>) -> Result<IndexStatus> {
    // 范围过滤按表限定列名拼：`extracted_segments` 自己没有 source_id，
    // 要通过 `extracted_contents` 才能判断它属于哪个来源。
    let scope_values: Vec<String> = source_scope
        .map(<[String]>::to_vec)
        .unwrap_or_default();
    let scope_filter = |column: &str| -> String {
        match source_scope {
            // 空范围是「什么都不看」，不能退化成「看全部」。
            Some([]) => " AND 0".to_owned(),
            Some(scope) => format!(" AND {column} IN ({})", placeholders(scope.len())),
            None => String::new(),
        }
    };

    let count = |sql: &str| -> Result<i64> {
        Ok(core
            .conn
            .query_row(sql, rusqlite::params_from_iter(scope_values.iter()), |row| {
                row.get(0)
            })?)
    };

    let total_segments = count(&format!(
        "SELECT COUNT(*) FROM extracted_segments s \
         JOIN extracted_contents c ON c.id = s.content_id WHERE 1=1{}",
        scope_filter("c.source_id")
    ))?;
    // search_docs 不重复存来源：范围过滤 join 回派生内容表。
    let docs_in_scope = format!(
        "SELECT COUNT(*) FROM search_docs d \
         JOIN extracted_segments s ON s.id = d.segment_id \
         JOIN extracted_contents c ON c.id = s.content_id WHERE 1=1{}",
        scope_filter("c.source_id")
    );
    let indexed_segments = count(&docs_in_scope)?;
    let stale_segments = count(&format!(
        "SELECT COUNT(*) FROM search_docs d \
         JOIN extracted_segments s ON s.id = d.segment_id \
         JOIN extracted_contents c ON c.id = s.content_id \
         WHERE 1=1{} AND d.tokenizer_version <> '{TOKENIZER_VERSION}'",
        scope_filter("c.source_id")
    ))?;
    // 提取失败的材料没有片段可索引，要单独算：否则「失败」在状态里完全看不见。
    let failed_sources = count(&format!(
        "SELECT COUNT(*) FROM extracted_contents WHERE 1=1{} AND status = 'failed'",
        scope_filter("source_id")
    ))?;
    let indexed_chars = count(&format!(
        "SELECT COALESCE(SUM(d.text_length), 0) FROM search_docs d \
         JOIN extracted_segments s ON s.id = d.segment_id \
         JOIN extracted_contents c ON c.id = s.content_id WHERE 1=1{}",
        scope_filter("c.source_id")
    ))?;
    let scoped_doc_ids = format!(
        "SELECT d.doc_id FROM search_docs d \
         JOIN extracted_segments s ON s.id = d.segment_id \
         JOIN extracted_contents c ON c.id = s.content_id WHERE 1=1{}",
        scope_filter("c.source_id")
    );
    let index_rows = count(&format!(
        "SELECT COUNT(*) FROM search_grams WHERE doc_id IN ({scoped_doc_ids})"
    ))?;
    let index_terms = count(&format!(
        "SELECT COUNT(DISTINCT term) FROM search_grams WHERE doc_id IN ({scoped_doc_ids})"
    ))?;

    // 索引表实际占用的页。`dbstat` 是 SQLite 的调试用虚表，本仓库的 bundled
    // 构建开了它；读不到就如实报 0 并在 reasons 里说明，不编一个数出来。
    let index_bytes = {
        let sql = "SELECT COALESCE(SUM(pgsize), 0) FROM dbstat WHERE name IN \
                   ('search_docs', 'search_grams', 'sqlite_autoindex_search_docs_1')";
        core.conn
            .query_row(sql, [], |row| row.get::<_, i64>(0))
            .unwrap_or_default()
    };
    let dbstat_available = index_bytes > 0 || indexed_segments == 0;

    let pending_segments = (total_segments - indexed_segments).max(0) + stale_segments;
    let keyword_index_ready = indexed_segments > 0 && stale_segments == 0;

    let coverage = if total_segments == 0 && failed_sources == 0 {
        Coverage::Unavailable
    } else if total_segments == indexed_segments && stale_segments == 0 && failed_sources == 0 {
        Coverage::Complete
    } else if indexed_segments == 0 {
        Coverage::Unavailable
    } else {
        Coverage::Partial
    };

    let mut reasons = Vec::new();
    if total_segments == 0 {
        reasons.push(if failed_sources > 0 {
            format!("{failed_sources} 份材料的正文解析失败，没有可索引的内容（材料数，不是片段数）")
        } else {
            "还没有可索引的正文片段：材料要先完成提取".to_owned()
        });
    } else {
        if pending_segments > 0 {
            reasons.push(format!(
                "{pending_segments} 个片段还没进索引：{stale_segments} 个属于旧分词器版本"
            ));
        }
        if failed_sources > 0 {
            reasons.push(format!(
                "{failed_sources} 份材料的正文解析失败，检索看不到它们的内容（材料数，不是片段数）"
            ));
        }
    }
    if !dbstat_available {
        reasons.push("这个构建读不到索引占用（dbstat 不可用），index_bytes 报 0".to_owned());
    }
    // 语义索引还没接：如实写清楚，而不是让前端以为「索引已就绪」包含它。
    reasons.push("语义索引尚未接入（见 issue #32）：当前只有关键词索引".to_owned());

    Ok(IndexStatus {
        coverage,
        keyword_index_ready,
        semantic_index_ready: false,
        tokenizer_version: TOKENIZER_VERSION.to_owned(),
        model_version: None,
        chunker_version: None,
        indexed_segments,
        total_segments,
        pending_segments,
        stale_segments,
        failed_sources,
        indexed_chars,
        index_rows,
        index_terms,
        index_bytes,
        reasons,
    })
}

/// 检索候选：返回命中片段 ID，按 `doc_id` 升序（稳定、可复现）。
///
/// `ORDER BY g.doc_id` 而不是 `d.doc_id`：倒排表的主键是 `(term, doc_id)`，
/// 固定 term 之后它天然按 doc_id 有序，SQLite 能直接顺扫并在 `LIMIT` 处停下。
/// 写成 `d.doc_id` 时规划器不认这个顺序，实测会退化成「把几万条候选全排进
/// 临时 B 树再取前 20 条」——10 万片段上一字不差是 45.9 ms 对 0.018 ms。
///
/// 这一片只提供候选与精确核对，排序与会话留给 #31。
pub(crate) fn candidates(core: &Core, query: &str, limit: usize) -> Result<Vec<String>> {
    let (filter, needle) = match candidate_filter(query) {
        Some(pair) => pair,
        None => return Ok(Vec::new()),
    };

    let mut statement = core.conn.prepare(
        "SELECT d.segment_id FROM search_grams g \
         CROSS JOIN search_docs d ON d.doc_id = g.doc_id \
         CROSS JOIN extracted_segments s ON s.id = d.segment_id \
         WHERE g.term = ?1 AND instr(lower(s.text), ?2) > 0 \
         ORDER BY g.doc_id LIMIT ?3",
    )?;
    let rows = statement.query_map(params![filter, needle, limit as i64], |row| {
        row.get::<_, String>(0)
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// 命中数量。测试与测量用它核对召回是否完整。
pub(crate) fn count_matches(core: &Core, query: &str) -> Result<i64> {
    let (filter, needle) = match candidate_filter(query) {
        Some(pair) => pair,
        None => return Ok(0),
    };
    // `CROSS JOIN` 把连接顺序写成「倒排表 → 文档行 → 片段」，和候选查询一致。
    // 诚实说明：在 SQLite 3.51 上实测 `JOIN` 与 `CROSS JOIN` 的计划**本来就相同**
    // （10 万片段上都是 38–39 ms），这不是性能修复，而是防规划器变化——
    // 一旦顺序变了，候选与计数就会给同一件事两个数字，没人知道该信哪个。
    //
    // 这个计数的代价由「过滤 gram 的候选行数」决定：二字查询约 40 ms，
    // 「面试官」也差不多，因为它的过滤 gram 是「面试」，命中 4.18 万行。
    // 按各 bigram 取交集实测能降到 8 ms（见架构文档），产品路径不需要全量计数，
    // 所以没有实现。
    let count = core.conn.query_row(
        "SELECT COUNT(*) FROM search_grams g \
         CROSS JOIN search_docs d ON d.doc_id = g.doc_id \
         CROSS JOIN extracted_segments s ON s.id = d.segment_id \
         WHERE g.term = ?1 AND instr(lower(s.text), ?2) > 0",
        params![filter, needle],
        |row| row.get(0),
    )?;
    Ok(count)
}

/// 候选生成的过滤条件：查询里最长的一段连续文本的首个词项。
///
/// 为什么是「最长那段的首个 gram」而不是「全部 gram 取交集」：真正包含整个
/// 查询的文本，一定包含这一段，也就一定包含它的首个 gram——所以这是召回的
/// 上界（超集），不会漏。取交集能缩小候选，但多出来的每轮查询在真实首屏上
/// 未必划算；最终核对始终是 `instr`，精确性不受影响。
fn candidate_filter(query: &str) -> Option<(String, String)> {
    let needle = query.trim().to_lowercase();
    if needle.is_empty() {
        return None;
    }
    let longest = needle
        .split(|character: char| !meaningful(character))
        .filter(|run| !run.is_empty())
        .max_by_key(|run| run.chars().count())?;

    let characters: Vec<char> = longest.chars().collect();
    let filter = if characters.len() == 1 {
        characters[0].to_string()
    } else {
        characters[..2].iter().collect()
    };
    Some((filter, needle))
}

fn placeholders(count: usize) -> String {
    std::iter::repeat_n("?", count).collect::<Vec<_>>().join(", ")
}
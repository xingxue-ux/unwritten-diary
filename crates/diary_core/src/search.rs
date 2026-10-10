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
use crate::model::{
    Coverage, IndexStatus, LocatorType, SearchFilters, SourceKind, SourceLocator,
};
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
    bump_index_epoch(tx)?;
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

/// 把索引代次 +1。每次索引写入都要在自己的事务里调一次。
///
/// 会话拿它当快照指纹：只要有人写过索引，进行中的会话就会在下次翻页时报
/// `search_expired`。**不要**换成「数行数 + 求和 doc_id」那类做法——删了再插
/// 时 SQLite 会复用 rowid，那种指纹在「重建同一个片段」时完全不变（见 v6 迁移
/// 的说明）。
pub(crate) fn bump_index_epoch(tx: &Transaction<'_>) -> Result<()> {
    tx.execute("UPDATE search_index_epoch SET epoch = epoch + 1 WHERE id = 1", [])?;
    Ok(())
}

/// 当前索引代次。会话开始时记下来，翻页时比对。
pub(crate) fn index_epoch(core: &Core) -> Result<i64> {
    Ok(core
        .conn
        .query_row("SELECT epoch FROM search_index_epoch WHERE id = 1", [], |row| {
            row.get(0)
        })?)
}

/// 待索引的一条记录文字（`captures.draft_text`）。
pub(crate) struct CaptureInput<'a> {
    pub capture_id: &'a str,
    pub text: &'a str,
}

/// 刷新一条记录文字的索引，在调用方的事务里执行。
///
/// 可重入：同一个 `capture_id` 先清后写。文字 trim 后为空时**只删不写**：留一条
/// 空文档会让「已索引的记录数」虚高，覆盖状态也会说假话（那正是这一片要修的毛病）。
pub(crate) fn index_capture(tx: &Transaction<'_>, capture: CaptureInput<'_>) -> Result<()> {
    // 写记录文字的索引同样要推进代次：改自己写的字必须让进行中的会话失效，
    // 否则旧查询会覆盖新内容（审查在 B3b 那边指出的正是这一类问题）。
    bump_index_epoch(tx)?;
    tx.execute(
        "DELETE FROM search_capture_docs WHERE capture_id = ?1",
        params![capture.capture_id],
    )?;
    if capture.text.trim().is_empty() {
        return Ok(());
    }
    tx.execute(
        "INSERT INTO search_capture_docs (capture_id, text_length, tokenizer_version) \
         VALUES (?1, ?2, ?3)",
        params![
            capture.capture_id,
            capture.text.chars().count() as i64,
            TOKENIZER_VERSION,
        ],
    )?;
    let doc_id = tx.last_insert_rowid();

    {
        let mut statement = tx.prepare(
            "INSERT OR IGNORE INTO search_capture_grams (term, doc_id) VALUES (?1, ?2)",
        )?;
        for gram in grams_for(capture.text) {
            statement.execute(params![gram, doc_id])?;
        }
    }
    Ok(())
}
/// 重建关键词索引。`source_scope` 为空表示整个资料库。
///
/// 两路都要重建：派生内容片段（`search_docs`）与用户自己写的记录文字
/// （`search_capture_docs`）。范围里的记录全部重刷一遍，文字为空的记录会在
/// `index_capture` 里只删不写，所以「清空过的记录」也不会留下旧词项。
///
/// 返回值是两边加起来重建的文档数（片段 + 记录文字），不是片段数——名字里的
/// 「索引」涵盖两张表，只数片段会让调用方以为记录文字没重建。
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

    // 记录文字一路：范围里的记录全部重刷（不只是有文字的）。空范围是
    // 「什么都不看」，与片段那一路同义——这里用 `Some([])` 分支显式表达，
    // 不靠 SQL 的 `IN ()` 恰好什么都不匹配。
    let captures: Vec<(String, String)> = match source_scope {
        Some([]) => Vec::new(),
        Some(scope) => {
            let sql = format!(
                "SELECT c.id, c.draft_text FROM captures c WHERE EXISTS \
                 (SELECT 1 FROM source_items si WHERE si.capture_id = c.id \
                  AND si.source_id IN ({}))",
                placeholders(scope.len())
            );
            let mut statement = core.conn.prepare(&sql)?;
            let rows = statement
                .query_map(rusqlite::params_from_iter(scope.iter()), |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows
        }
        None => {
            let mut statement = core.conn.prepare("SELECT id, draft_text FROM captures")?;
            let rows = statement
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows
        }
    };
    {
        let tx = core.conn.transaction()?;
        for (capture_id, text) in &captures {
            index_capture(
                &tx,
                CaptureInput {
                    capture_id,
                    text,
                },
            )?;
            if !text.trim().is_empty() {
                indexed += 1;
            }
        }
        tx.commit()?;
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
    // 记录文字没有自己的 source_id：一条记录属于这个范围，当且仅当它拥有范围内的
    // 来源（记录文字是随着这条记录的材料一起被看到的）。空范围仍然是「什么都不看」，
    // 与片段那一路、以及 `ranked_matches` 的过滤条件保持同一套语义。
    let capture_scope_filter = match source_scope {
        Some([]) => " AND 0".to_owned(),
        Some(scope) => format!(
            " AND EXISTS (SELECT 1 FROM source_items si WHERE si.capture_id = c.id \
             AND si.source_id IN ({}))",
            placeholders(scope.len())
        ),
        None => String::new(),
    };

    // 绑定的范围参数份数**跟着 SQL 里的占位符个数走**：一条子查询可能带一个范围
    // 过滤，也可能像下面数词项的 UNION 那样带两个（两路各自过滤一次）。写死成
    // 「只绑一份」时，范围查询会报 InvalidParameterCount(1, 2)——只有带 sourceScope
    // 调用才会触发，全库查询看不出来。这一片就踩到过，测试是
    // `scope_filters_status_and_empty_scope_means_nothing`。
    let count = |sql: &str| -> Result<i64> {
        let placeholders = sql.matches('?').count();
        let mut values: Vec<&String> = Vec::with_capacity(placeholders);
        if !scope_values.is_empty() {
            while values.len() < placeholders {
                values.extend(scope_values.iter());
            }
        }
        Ok(core
            .conn
            .query_row(sql, rusqlite::params_from_iter(values), |row| row.get(0))?)
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
    ))? + count(&format!(
        "SELECT COALESCE(SUM(d.text_length), 0) FROM search_capture_docs d \
         JOIN captures c ON c.id = d.capture_id WHERE 1=1{}",
        capture_scope_filter
    ))?;
    let scoped_doc_ids = format!(
        "SELECT d.doc_id FROM search_docs d \
         JOIN extracted_segments s ON s.id = d.segment_id \
         JOIN extracted_contents c ON c.id = s.content_id WHERE 1=1{}",
        scope_filter("c.source_id")
    );
    // 记录文字那一路的文档行；index_rows / index_terms 要把两边都算上，
    // 否则「索引里有多少行」只数了一半。
    let scoped_capture_doc_ids = format!(
        "SELECT d.doc_id FROM search_capture_docs d \
         JOIN captures c ON c.id = d.capture_id WHERE 1=1{}",
        capture_scope_filter
    );
    let index_rows = count(&format!(
        "SELECT COUNT(*) FROM search_grams WHERE doc_id IN ({scoped_doc_ids})"
    ))? + count(&format!(
        "SELECT COUNT(*) FROM search_capture_grams WHERE doc_id IN ({scoped_capture_doc_ids})"
    ))?;
    // 词项要取**并集**再数：同一个词可能既出现在派生片段里、又出现在用户自己写的
    // 文字里（比如两边都有「妈妈」），分别 COUNT(DISTINCT) 再相加会把它算两次，
    // 状态数字偏大（审查发现的）。UNION 自带去重，所以外面直接 COUNT(*)。
    let index_terms = count(&format!(
        "SELECT COUNT(*) FROM (\
             SELECT term FROM search_grams WHERE doc_id IN ({scoped_doc_ids}) \
             UNION \
             SELECT term FROM search_capture_grams WHERE doc_id IN ({scoped_capture_doc_ids})\
         )"
    ))?;

    // ------------------------------------------------------------ 记录文字
    //
    // 「有文字」用与增量索引同一套空白定义（见 `non_empty_text` 的说明）：直接用
    // SQLite 自带的 `trim` 会把「只打了一个全角空格」的草稿算进总数，而增量索引
    // 已经把它删掉了，覆盖状态就永远报 partial 且没办法修好。
    let has_text = non_empty_text("c.draft_text");
    let live_captures = format!(
        " FROM captures c WHERE 1=1{capture_scope_filter} \
         AND c.state <> 'trashed' AND {has_text}"
    );
    let total_captures = count(&format!("SELECT COUNT(*){live_captures}"))?;
    let indexed_captures = count(&format!(
        "SELECT COUNT(*) FROM search_capture_docs d JOIN captures c ON c.id = d.capture_id \
         WHERE 1=1{capture_scope_filter} AND c.state <> 'trashed' AND {has_text} \
         AND d.tokenizer_version = '{TOKENIZER_VERSION}'"
    ))?;
    let stale_captures = count(&format!(
        "SELECT COUNT(*) FROM search_capture_docs d JOIN captures c ON c.id = d.capture_id \
         WHERE 1=1{capture_scope_filter} AND c.state <> 'trashed' AND {has_text} \
         AND d.tokenizer_version <> '{TOKENIZER_VERSION}'"
    ))?;

    // 索引表实际占用的页。`dbstat` 是 SQLite 的调试用虚表，本仓库的 bundled
    // 构建开了它；读不到就如实报 0 并在 reasons 里说明，不编一个数出来。
    //
    // **这个数字永远是整库的**：dbstat 按表/索引汇总页数，页在来源之间是共享的，
    // 没法按 source_scope 拆分。范围查询照样给出整库值，但会在 reasons 里写明，
    // 免得前端把整库大小当成某个来源的索引大小（审查就是这么发现的）。
    let index_bytes = {
        let sql = "SELECT COALESCE(SUM(pgsize), 0) FROM dbstat WHERE name IN \
                   ('search_docs', 'search_grams', 'sqlite_autoindex_search_docs_1', \
                    'search_capture_docs', 'search_capture_grams', \
                    'sqlite_autoindex_search_capture_docs_1', \
                    'text_chunks', 'idx_text_chunks_day', 'sqlite_autoindex_text_chunks_1', \
                    'chunk_spans', 'idx_chunk_spans_source', 'idx_chunk_spans_capture', \
                    'idx_chunk_spans_revision', 'sqlite_autoindex_chunk_spans_1', \
                    'chunk_vectors', 'sqlite_autoindex_chunk_vectors_1', 'index_meta')";
        core.conn
            .query_row(sql, [], |row| row.get::<_, i64>(0))
            .unwrap_or_default()
    };
    // 明明有索引却读到 0 才算 dbstat 不可用；两路都没索引时 0 是正常的。
    let dbstat_available =
        index_bytes > 0 || (indexed_segments == 0 && indexed_captures == 0);

    let pending_segments = (total_segments - indexed_segments).max(0) + stale_segments;
    // 记录文字那一路的 pending：总数里已经排除了回收站与空文字，而 indexed 只算
    // 当前分词器版本，所以差值同时涵盖了「没进索引」与「版本过期」两种。
    let pending_captures = (total_captures - indexed_captures).max(0);
    let segments_covered = total_segments == indexed_segments && stale_segments == 0;
    let captures_covered = total_captures == indexed_captures;
    let keyword_index_ready = (indexed_segments > 0 || indexed_captures > 0)
        && stale_segments == 0
        && stale_captures == 0;

    // 覆盖状态要**同时**看两边：只有片段与记录文字都覆盖完（且没有解析失败）
    // 才算 complete。只算片段的话，「自己写的字全都还没进索引」会被报成 complete
    // ——那正是修这一片之前的真实状态，也是最容易骗到人的一处。
    let coverage = if total_segments == 0 && total_captures == 0 && failed_sources == 0 {
        Coverage::Unavailable
    } else if segments_covered && captures_covered && failed_sources == 0 {
        Coverage::Complete
    } else if indexed_segments == 0 && indexed_captures == 0 {
        Coverage::Unavailable
    } else {
        Coverage::Partial
    };

    // ------------------------------------------------------------ 语义这一路
    //
    // 语义这一路的数字全部**算出来**：生效代次、块数与其中有这一代向量的块数、
    // 生效代次那一批向量的 `model_version`。没有向量时 `model_version` 是 `None`
    // ——那是「库里的真值」，不是「这一片还没接模型」这种会被时间打脸的话。
    let (active_generation, _building_generation) = crate::chunks::generations(core)?;
    let chunk_counts = crate::chunks::counts(core, source_scope)?;
    // 「就绪」要求有一代向量在服务，并且这一代把范围内的块都覆盖了。
    let semantic_index_ready = active_generation.is_some()
        && chunk_counts.total > 0
        && chunk_counts.embedded == chunk_counts.total;
    let model_version = crate::semantic::model_version(core)?;

    let mut reasons = Vec::new();
    if total_segments == 0 && total_captures == 0 {
        reasons.push(if failed_sources > 0 {
            format!("{failed_sources} 份材料的正文解析失败，没有可索引的内容（材料数，不是片段数）")
        } else {
            "还没有可索引的内容：材料要先完成提取，或者先写下点什么".to_owned()
        });
    } else {
        if pending_segments > 0 {
            reasons.push(format!(
                "{pending_segments} 个片段还没进索引：{stale_segments} 个属于旧分词器版本"
            ));
        }
        if pending_captures > 0 {
            reasons.push(format!(
                "{pending_captures} 条记录的文字还没进索引：{stale_captures} 条属于旧分词器版本"
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
    } else if source_scope.is_some() {
        reasons.push(
            "index_bytes 是整库索引占用：dbstat 只能按表统计，无法按来源拆分".to_owned(),
        );
    }
    // 语义索引这一路：只有**真的没就绪**时才说一句原因，而且判断依据是库里的状态
    // （生效代次 + 块的向量覆盖）加上**模型的真实可用性**，不是写死的常量文案。
    //
    // 「模型加载失败」与「还没有配置模型」是**两件事**，前者必须把失败原因原样说出来
    // ——把错误盖成「未就绪」等于骗人（见 `embedding` 模块的说明）。
    // 四段与上面 `semantic_index_ready` 的取值**严格互补**：就绪时没有原因，
    // 有原因时一定不就绪。
    let semantic_reason = if active_generation.is_none() {
        match core.embedder_readiness() {
            crate::semantic::EmbedderReadiness::NotConfigured => Some(
                "语义索引未就绪：还没有配置本地模型（DIARY_MODEL_DIR / DIARY_ORT_DYLIB），\
                 当前只有关键词索引"
                    .to_owned(),
            ),
            crate::semantic::EmbedderReadiness::Failed(reason) => Some(format!(
                "语义索引未就绪：本地模型不可用（{reason}）"
            )),
            crate::semantic::EmbedderReadiness::Configured
            | crate::semantic::EmbedderReadiness::Ready => Some(
                "语义索引未就绪：还没有生效的向量代次（模型已就绪，等一次 build_semantic_index）"
                    .to_owned(),
            ),
        }
    } else if chunk_counts.total == 0 {
        // 有生效代次却没有任何块：要么还没重建过块，要么范围内的内容本来就没有正文。
        Some("语义索引未就绪：范围内还没有文本块".to_owned())
    } else if chunk_counts.embedded < chunk_counts.total {
        // 换代中间态：新代次还没写满。这时候旧代次可能还在服务，但覆盖已经不完整。
        Some(format!(
            "语义索引未就绪：生效代次只覆盖了 {} 块里的 {} 块",
            chunk_counts.total, chunk_counts.embedded
        ))
    } else {
        None
    };
    if let Some(reason) = semantic_reason {
        reasons.push(reason);
    }

    Ok(IndexStatus {
        coverage,
        keyword_index_ready,
        semantic_index_ready,
        tokenizer_version: TOKENIZER_VERSION.to_owned(),
        // 生效代次那一批 `chunk_vectors.model_version`（算出来的真值）。
        model_version,
        // 块真的进库了（B3c-1 的文档承诺过「接进索引之后才该有值」），所以从这一片起
        // 它有值：库里块的 `chunker_version` 与它不一致，就说明块要按新规则重算。
        chunker_version: Some(crate::chunker::CHUNKER_VERSION.to_owned()),
        total_chunks: chunk_counts.total,
        embedded_chunks: chunk_counts.embedded,
        indexed_segments,
        total_segments,
        pending_segments,
        stale_segments,
        indexed_captures,
        total_captures,
        failed_sources,
        indexed_chars,
        index_rows,
        index_terms,
        index_bytes,
        reasons,
    })
}

/// SQL 里「这段文字不是空白」的表达式，与增量索引用的 Rust `trim()` 同一套空白定义。
///
/// SQLite 自带的 `trim(X)` 只去掉 ASCII 空白（含空格、制表、换行等），而 Rust 的
/// `str::trim` 认整个 Unicode White_Space（全角空格 U+3000、NBSP U+00A0 等）。
/// 两边不统一就会出一个修不好的状态：「只打了一个全角空格」的草稿被索引侧判为空
/// （行被删掉），却被状态侧算进「有文字的记录数」，覆盖状态于是永远报 partial，
/// 而重建也只会再删一次。所以状态与迁移里的计数都走这个表达式。
pub(crate) fn non_empty_text(column: &str) -> String {
    // 下面列的就是 Unicode White_Space 的全部码位。SQLite 的 trim 按字符（不是按
    // 字节）去掉两端出现在集合里的字符，所以「空格 + 这些 char()」等价于 Rust 的 trim。
    format!(
        "trim({column}, ' ' || char(9) || char(10) || char(11) || char(12) || char(13) \
         || char(0x85) || char(0xA0) || char(0x1680) || char(0x2000) || char(0x2001) \
         || char(0x2002) || char(0x2003) || char(0x2004) || char(0x2005) || char(0x2006) \
         || char(0x2007) || char(0x2008) || char(0x2009) || char(0x200A) || char(0x2028) \
         || char(0x2029) || char(0x202F) || char(0x205F) || char(0x3000)) <> ''"
    )
}

/// 检索候选：返回命中片段 ID，按 `doc_id` 升序（稳定、可复现）。
///
/// 只覆盖派生片段（B3a 的范围），记录文字不在这里——片段 ID 这个返回值表达不了
/// 「一条记录的文字」。产品检索路径是 `search.start`，它把两路合在一起搜。
///
/// `ORDER BY g.doc_id` 而不是 `d.doc_id`：倒排表的主键是 `(term, doc_id)`，
/// 固定 term 之后它天然按 doc_id 有序，SQLite 能直接顺扫并在 `LIMIT` 处停下。
/// 写成 `d.doc_id` 时规划器不认这个顺序，实测会退化成「把几万条候选全排进
/// 临时 B 树再取前 20 条」——10 万片段上一字不差是 45.9 ms 对 0.018 ms。
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
///
/// 与 `candidates` 一样只数派生片段：它核对的是「片段索引召回全不全」，
/// 记录文字那一路的命中数没有稳定的旧口径可比（它是新加的表）。
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

/// SQL 里的 `?` 占位符串。文本块那一路也要按同一套规则拼范围过滤，所以对 crate 可见。
pub(crate) fn placeholders(count: usize) -> String {
    std::iter::repeat_n("?", count).collect::<Vec<_>>().join(", ")
}

// ------------------------------------------------------------ 会话用的查询

/// 一次索引文档引用的来源：`search_docs`（派生片段）还是 `search_capture_docs`
/// （用户自己写的记录文字）。
///
/// 两张表的 `doc_id` 各自从 1 开始，会撞号，所以引用必须带来源。**不**用
/// 「负数 doc_id」或「加一个大偏移」这类技巧：那要求每一处读写 doc_id 的地方都
/// 记住同一个约定，忘一处就是很难发现的串号 bug（把记录文字的 id 当成片段去取）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct DocRef {
    /// true 表示记录文字，false 表示派生片段。
    pub capture: bool,
    pub doc_id: i64,
}

/// 一条候选的排序键（不含正文：几万条候选时不要把正文都读进来）。
pub(crate) struct RankedMatch {
    /// 命中文档的引用（两张表的 doc_id 会撞号，所以必须连来源一起记）。
    pub doc: DocRef,
    /// 命中在正文里的位置（1 开始，字符数）。
    pub position: i64,
    pub day_key: Option<String>,
}

/// 一页里一条派生片段命中要展示的全部字段。
pub(crate) struct PageRow {
    pub doc: DocRef,
    pub segment_id: String,
    pub text: String,
    pub day_key: Option<String>,
    pub kind: String,
    pub source_id: String,
    pub source_revision_id: String,
    pub coverage: Coverage,
    pub asset_name: Option<String>,
    pub locator: SourceLocator,
}

/// 一页里一条记录文字命中要展示的字段。
///
/// 它比 `PageRow` 少得多：记录文字就是用户原文，没有原件、没有定位、没有覆盖
/// 状态可谈（`SearchHit` 里对应的字段照实给 `None` / `Complete`）。
pub(crate) struct CapturePageRow {
    pub doc: DocRef,
    pub capture_id: String,
    pub text: String,
    pub day_key: String,
}

/// 带过滤条件的候选，已按会话的排序规则排好。
///
/// 两条查询各管一路（派生片段 / 记录文字），在 Rust 里合并后统一排序：两张表的
/// `doc_id` 不能直接放在一起比大小，所以合并的是带来源的 `DocRef`。
///
/// 排序：日期从近到远 → 出现位置 → 家族（片段在前）→ 写入顺序。**没有**做
/// 「整词命中优先」：那要为每一行查一次词表，代价随候选数线性增长；等混合检索
/// 带分数进来再一起做。
pub(crate) fn ranked_matches(
    core: &Core,
    needle: &str,
    filters: &SearchFilters,
) -> Result<Vec<RankedMatch>> {
    let Some((filter, _)) = candidate_filter(needle) else {
        return Ok(Vec::new());
    };
    let (kinds, scope) = crate::search_session::filters_to_params(filters)?;

    // 派生片段一路：过滤条件全部下到 SQL（与记录文字那一路同一套语义）。
    let mut matches: Vec<RankedMatch> = {
        let mut statement = core.conn.prepare(
            "SELECT d.doc_id, instr(lower(s.text), ?2), c.day_key \
             FROM search_grams g \
             CROSS JOIN search_docs d ON d.doc_id = g.doc_id \
             CROSS JOIN extracted_segments s ON s.id = d.segment_id \
             JOIN extracted_contents ec ON ec.id = s.content_id \
             JOIN source_items src ON src.source_id = ec.source_id \
             JOIN captures c ON c.id = src.capture_id \
             WHERE g.term = ?1 AND instr(lower(s.text), ?2) > 0 \
               AND (?3 = 1 OR c.state <> 'trashed') \
               AND (?4 IS NULL OR c.day_key >= ?4) \
               AND (?5 IS NULL OR c.day_key <= ?5) \
               AND (?6 IS NULL OR src.kind IN (SELECT value FROM json_each(?6))) \
               AND (?7 IS NULL OR src.source_id IN (SELECT value FROM json_each(?7)))",
        )?;
        let rows = statement
            .query_map(
                params![
                    filter,
                    needle,
                    i64::from(filters.include_trashed),
                    filters.from_day_key,
                    filters.to_day_key,
                    kinds,
                    scope,
                ],
                |row| {
                    Ok(RankedMatch {
                        doc: DocRef {
                            capture: false,
                            doc_id: row.get(0)?,
                        },
                        position: row.get(1)?,
                        day_key: row.get(2)?,
                    })
                },
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };

    // 记录文字一路。它属于 `text` 类型：`kinds` 非空且不含 `Text` 时整路不参与
    // ——那表示这个过滤器把这一路排除了，而不是「恰好没命中」。
    if filters.kinds.is_empty() || filters.kinds.contains(&SourceKind::Text) {
        let mut statement = core.conn.prepare(
            "SELECT d.doc_id, instr(lower(c.draft_text), ?2), c.day_key \
             FROM search_capture_grams g \
             CROSS JOIN search_capture_docs d ON d.doc_id = g.doc_id \
             CROSS JOIN captures c ON c.id = d.capture_id \
             WHERE g.term = ?1 AND instr(lower(c.draft_text), ?2) > 0 \
               AND (?3 = 1 OR c.state <> 'trashed') \
               AND (?4 IS NULL OR c.day_key >= ?4) \
               AND (?5 IS NULL OR c.day_key <= ?5) \
               AND (?6 IS NULL OR EXISTS (SELECT 1 FROM source_items si \
                    WHERE si.capture_id = c.id \
                      AND si.source_id IN (SELECT value FROM json_each(?6))))",
        )?;
        let rows = statement
            .query_map(
                params![
                    filter,
                    needle,
                    i64::from(filters.include_trashed),
                    filters.from_day_key,
                    filters.to_day_key,
                    scope,
                ],
                |row| {
                    Ok(RankedMatch {
                        doc: DocRef {
                            capture: true,
                            doc_id: row.get(0)?,
                        },
                        position: row.get(1)?,
                        day_key: row.get(2)?,
                    })
                },
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        matches.extend(rows);
    }

    matches.sort_by_key(crate::search_session::sort_key);
    Ok(matches)
}

/// 按引用取这一页要展示的片段行；只处理片段引用（`capture == false`）。
///
/// 正文只在这里读，按页读。两张表分两次取是刻意的：它们的 `doc_id` 各自从 1
/// 开始，一条 SQL 把两边 UNION 起来就分不清哪一行来自哪张表。
pub(crate) fn load_page(core: &Core, refs: &[DocRef]) -> Result<Vec<PageRow>> {
    let doc_ids: Vec<i64> = refs
        .iter()
        .filter(|doc| !doc.capture)
        .map(|doc| doc.doc_id)
        .collect();
    if doc_ids.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = placeholders(doc_ids.len());
    let sql = format!(
        "SELECT d.doc_id, s.id, s.text, c.day_key, src.kind, ec.source_id, s.source_revision_id, \
                ec.coverage, a.original_name, \
                s.locator_type, s.text_start, s.text_end, s.start_ms, s.end_ms, s.page_number, \
                s.block_id, s.rect_left, s.rect_top, s.rect_right, s.rect_bottom, s.asset_id \
         FROM search_docs d \
         JOIN extracted_segments s ON s.id = d.segment_id \
         JOIN extracted_contents ec ON ec.id = s.content_id \
         JOIN source_items src ON src.source_id = ec.source_id \
         JOIN captures c ON c.id = src.capture_id \
         LEFT JOIN source_revisions sr ON sr.revision_id = s.source_revision_id \
         LEFT JOIN assets a ON a.id = sr.asset_id \
         WHERE d.doc_id IN ({placeholders})"
    );
    let mut statement = core.conn.prepare(&sql)?;
    let rows = statement
        .query_map(rusqlite::params_from_iter(doc_ids.iter()), |row| {
            let coverage: String = row.get(7)?;
            let locator_type: String = row.get(9)?;
            let rect = match (
                row.get::<_, Option<f64>>(16)?,
                row.get::<_, Option<f64>>(17)?,
                row.get::<_, Option<f64>>(18)?,
                row.get::<_, Option<f64>>(19)?,
            ) {
                (Some(left), Some(top), Some(right), Some(bottom)) => {
                    Some([left, top, right, bottom])
                }
                _ => None,
            };
            Ok(PageRow {
                doc: DocRef {
                    capture: false,
                    doc_id: row.get(0)?,
                },
                segment_id: row.get(1)?,
                text: row.get(2)?,
                day_key: row.get(3)?,
                kind: row.get(4)?,
                source_id: row.get(5)?,
                source_revision_id: row.get(6)?,
                coverage: Coverage::from_wire(&coverage).unwrap_or(Coverage::Unavailable),
                asset_name: row.get(8)?,
                locator: SourceLocator {
                    locator_type: LocatorType::from_wire(&locator_type)
                        .unwrap_or(LocatorType::TextRange),
                    source_revision_id: row.get(6)?,
                    text_start: row.get(10)?,
                    text_end: row.get(11)?,
                    start_ms: row.get(12)?,
                    end_ms: row.get(13)?,
                    page_number: row.get(14)?,
                    block_id: row.get(15)?,
                    rect,
                    asset_id: row.get(20)?,
                },
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// 按引用取这一页要展示的记录文字行；只处理记录引用（`capture == true`）。
pub(crate) fn load_capture_page(core: &Core, refs: &[DocRef]) -> Result<Vec<CapturePageRow>> {
    let doc_ids: Vec<i64> = refs
        .iter()
        .filter(|doc| doc.capture)
        .map(|doc| doc.doc_id)
        .collect();
    if doc_ids.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = placeholders(doc_ids.len());
    let sql = format!(
        "SELECT d.doc_id, d.capture_id, c.draft_text, c.day_key \
         FROM search_capture_docs d JOIN captures c ON c.id = d.capture_id \
         WHERE d.doc_id IN ({placeholders})"
    );
    let mut statement = core.conn.prepare(&sql)?;
    let rows = statement
        .query_map(rusqlite::params_from_iter(doc_ids.iter()), |row| {
            Ok(CapturePageRow {
                doc: DocRef {
                    capture: true,
                    doc_id: row.get(0)?,
                },
                capture_id: row.get(1)?,
                text: row.get(2)?,
                day_key: row.get(3)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

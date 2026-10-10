//! B3c-2（前半）：文本块的存储子结构、换代元数据与退化路径。
//!
//! 这一片**不接模型**：只把「篇」按 B3c-1 的打包规则变成可存储、可换代的块。所以这里
//! 断言的重点不是检索质量，而是几件结构性的事实：
//!
//! - 块按**日子**打包，可以跨记录（一块两段来自两条记录）；
//! - `chunk_spans` 上是权威值（`capture_id` / `kind` / `coverage` / 篇内区间），
//!   `text_chunks` 上的 `kind` / `coverage` 只是便于块级过滤的聚合；
//! - 记录自己写的文字**只有一个载体**：`commit` 造出来的 `text` + `user` 来源要跳过；
//! - 重建可重入、范围语义与关键词那一路一致；
//! - 没有模型时状态如实报「未就绪」，而且**关键词检索一行都不受影响**；
//! - v7 库升到 v8 时旧数据一行未动，也不回填块。
//!
//! 风格照 `search_capture_test.rs`：真库、真导入、断言带 reason。

use std::path::Path;

use chrono::{DateTime, TimeZone, Utc};
use diary_core::{
    Core, Coverage, CreateDraftInput, ImportManifest, ImportOrigin, ImportRequest, SearchFilters,
    SearchMode, SearchRequest, SearchSnapshot, CHUNKER_VERSION,
};
use rusqlite::params;
use sha2::{Digest, Sha256};

/// 一行 `chunk_spans` 的断言用快照。
struct SpanRow {
    ordinal: i64,
    capture_id: String,
    source_id: String,
    source_revision_id: String,
    start_char: i64,
    end_char: i64,
    kind: String,
    coverage: String,
}

fn sha_of(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

fn new_core(dir: &Path) -> Core {
    Core::open(dir.join("library.sqlite")).unwrap()
}

/// 固定用 UTC+8 的上午：`day_key` 与 occurred_at 的本地日期一致。
fn at(year: i32, month: u32, day: u32) -> DateTime<Utc> {
    at_hour(year, month, day, 4)
}

fn at_hour(year: i32, month: u32, day: u32, hour: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(year, month, day, hour, 0, 0).unwrap()
}

/// 保存草稿正文。revision 现读现用：导入等操作也可能推进记录的 revision。
fn save(core: &mut Core, capture_id: &str, text: &str, operation: &str) {
    let revision = core.get_capture(capture_id).unwrap().revision;
    core.save_draft(capture_id, text, revision, operation).unwrap();
}

/// 建一条记录；`text` 非空时顺手写下来。返回 capture_id。
fn write_capture(core: &mut Core, day: DateTime<Utc>, text: &str, operation: &str) -> String {
    let capture_id = core
        .create_draft(CreateDraftInput {
            occurred_at: Some(day),
            time_zone: "Asia/Shanghai",
            utc_offset_minutes: 480,
            operation_id: &format!("{operation}-create"),
        })
        .unwrap()
        .id;
    if !text.is_empty() {
        save(core, &capture_id, text, &format!("{operation}-save"));
    }
    capture_id
}

/// 往一条记录上导入文本并提取，返回 source_id。
fn import_and_extract(
    core: &mut Core,
    capture_id: &str,
    name: &str,
    text: &str,
    operation: &str,
) -> String {
    let bytes = text.as_bytes();
    let ticket = core
        .prepare_import(ImportRequest {
            capture_id,
            display_name: name,
            mime_hint: Some("text/plain"),
            size_hint: Some(bytes.len() as i64),
            origin: ImportOrigin::Picker,
            operation_id: &format!("{operation}-prepare"),
        })
        .unwrap();
    std::fs::write(&ticket.staging_ticket, bytes).unwrap();
    core.finish_import(
        &ticket.import_id,
        &ticket.staging_ticket,
        ImportManifest {
            copied_bytes: bytes.len() as i64,
            sha256: sha_of(bytes),
            detected_mime: "text/plain".to_owned(),
            original_name: name.to_owned(),
        },
    )
    .unwrap();
    // 取最后挂上去的那一个：有草稿文字、又提交过的记录，position 0 是它自己的文字版本。
    let source_id = core
        .get_capture(capture_id)
        .unwrap()
        .ordered_source_ids
        .last()
        .cloned()
        .expect("导入的材料应当挂到记录上");
    core.extract_source(&source_id).unwrap();
    source_id
}

fn search(core: &mut Core, query: &str) -> SearchSnapshot {
    core.start_search(
        SearchRequest {
            query: query.to_owned(),
            mode: SearchMode::Keyword,
            filters: SearchFilters::default(),
            page_size: 20,
        },
        1,
    )
    .unwrap()
}

fn open(path: &Path) -> rusqlite::Connection {
    rusqlite::Connection::open(path).unwrap()
}

fn count(conn: &rusqlite::Connection, sql: &str, table: &str) -> i64 {
    conn.query_row(sql, [], |row| row.get(0))
        .unwrap_or_else(|error| panic!("查 {table} 的计数失败：{error}"))
}

// ------------------------------------------------------------ 建块与聚合

/// 建块的形状：块的 `kind` / `coverage` 是 span 的聚合，span 上才是权威值。
///
/// 同一天三条记录各一篇：记录 A 自己写的文字（`text` / complete）、记录 B 与 C 各一份
/// 导入正文（`file`）。三篇都短，所以按 B3c-1 的规则合进**一块**——这一块因此跨了三条
/// 记录，`text_chunks` 上不可能有 `capture_id`，只能看 `chunk_spans`。
#[test]
fn a_chunk_packs_pieces_across_records_and_spans_are_authoritative() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.sqlite");
    let mut core = new_core(dir.path());

    let a = write_capture(&mut core, at_hour(2026, 9, 20, 4), "今天妈妈打电话来。", "opA");
    let b = write_capture(&mut core, at_hour(2026, 9, 20, 5), "", "opB");
    let source_b = import_and_extract(&mut core, &b, "天气.txt", "今天风很大。\n", "opB-mat");
    let c = write_capture(&mut core, at_hour(2026, 9, 20, 6), "", "opC");
    let source_c = import_and_extract(&mut core, &c, "桂花.txt", "路边的桂花开了。\n", "opC-mat");

    // 把 C 那一篇标成 partial：验证块的聚合 coverage 取**最弱**的那一段。
    open(&path)
        .execute(
            "UPDATE extracted_contents SET coverage = 'partial' WHERE source_id = ?1",
            params![source_c],
        )
        .unwrap();

    let written = core.rebuild_text_chunks(None).unwrap();
    assert_eq!(written, 1, "同一天的三篇短正文应当合成一块");

    let conn = open(&path);
    let (chunk_id, ordinal, text, kind, coverage, day, chunker): (
        String,
        i64,
        String,
        String,
        String,
        String,
        String,
    ) = conn
        .query_row(
            "SELECT chunk_id, ordinal, text, kind, coverage, day_key, chunker_version \
             FROM text_chunks",
            [],
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
        .unwrap();
    assert_eq!(ordinal, 0, "同一天里的第一块");
    assert_eq!(day, "2026-09-20");
    assert_eq!(chunker, CHUNKER_VERSION, "库里的块要记下切块规则版本");
    assert_eq!(
        text, "今天妈妈打电话来。\n\n今天风很大。\n\n路边的桂花开了。",
        "块正文是各篇去掉两端空白后按打包顺序拼接（分隔是空行）"
    );
    assert_eq!(
        kind, "text",
        "块的 kind 是聚合值，取第一段（记录 A 自己写的文字）的类型"
    );
    assert_eq!(
        coverage, "partial",
        "块的 coverage 是聚合值，取各段里最弱的那个（不拿 complete 粉饰）"
    );

    let spans: Vec<SpanRow> = {
        let mut statement = conn
            .prepare(
                "SELECT ordinal, capture_id, source_id, source_revision_id, start_char, end_char, \
                 kind, coverage FROM chunk_spans WHERE chunk_id = ?1 ORDER BY ordinal",
            )
            .unwrap();
        let rows = statement
            .query_map(params![chunk_id], |row| {
                Ok(SpanRow {
                    ordinal: row.get(0)?,
                    capture_id: row.get(1)?,
                    source_id: row.get(2)?,
                    source_revision_id: row.get(3)?,
                    start_char: row.get(4)?,
                    end_char: row.get(5)?,
                    kind: row.get(6)?,
                    coverage: row.get(7)?,
                })
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        rows
    };
    assert_eq!(spans.len(), 3, "三篇各占一段");
    let described: Vec<(i64, &str, &str, &str, String)> = spans
        .iter()
        .map(|span| {
            (
                span.ordinal,
                span.capture_id.as_str(),
                span.source_id.as_str(),
                span.kind.as_str(),
                span.coverage.clone(),
            )
        })
        .collect();
    assert_eq!(
        described,
        vec![
            (0, a.as_str(), a.as_str(), "text", "complete".to_owned()),
            (1, b.as_str(), source_b.as_str(), "file", "complete".to_owned()),
            (2, c.as_str(), source_c.as_str(), "file", "partial".to_owned()),
        ],
        "span 上是权威值：每段自己的记录、篇标识、类型与覆盖程度"
    );

    // 第一段：记录 A 自己写的文字。
    assert_eq!(
        spans[0].source_revision_id, "",
        "记录自己的文字不对应任何 source 修订，如实留空"
    );
    assert_eq!(
        spans[0].start_char, 0,
        "start/end 是这篇自己的字符区间（排他）"
    );
    assert_eq!(
        spans[0].end_char,
        "今天妈妈打电话来。".chars().count() as i64
    );
    // 第二、三段：各自独立的篇内区间。
    assert_eq!(spans[1].start_char, 0);
    assert_eq!(spans[1].end_char, "今天风很大。".chars().count() as i64);
    assert_eq!(spans[2].start_char, 0);
    assert_eq!(spans[2].end_char, "路边的桂花开了。".chars().count() as i64);

    // 跨记录合块：一块三段，来自三条不同的记录——所以块上没有 capture_id。
    let distinct: i64 = conn
        .query_row(
            "SELECT COUNT(DISTINCT capture_id) FROM chunk_spans WHERE chunk_id = ?1",
            params![chunk_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(distinct, 3, "这一块确实跨了三条记录");

    // 来源篇要记下修订：检索结果才能回溯到具体的版本。
    assert!(
        !spans[1].source_revision_id.is_empty() && !spans[2].source_revision_id.is_empty(),
        "来源篇的 source_revision_id 不能是空串"
    );

    let status = core.index_status(None).unwrap();
    assert_eq!(status.total_chunks, 1);
}

/// 单独的来源篇不与自己所在记录的文字混为一谈，也不与别天的篇合块。
#[test]
fn pieces_on_different_days_never_share_a_chunk() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    let a = write_capture(&mut core, at(2026, 9, 20), "", "opA");
    import_and_extract(&mut core, &a, "A.txt", "今天天气不错。\n", "opA-mat");
    let b = write_capture(&mut core, at(2026, 9, 21), "", "opB");
    import_and_extract(&mut core, &b, "B.txt", "路边的桂花开了。\n", "opB-mat");
    write_capture(&mut core, at(2026, 9, 22), "妈妈来做饭。", "opC");

    let written = core.rebuild_text_chunks(None).unwrap();
    assert_eq!(written, 3, "三天各一块");
    let conn = open(&dir.path().join("library.sqlite"));
    assert_eq!(count(&conn, "SELECT COUNT(*) FROM text_chunks", "text_chunks"), 3);
    assert_eq!(
        count(&conn, "SELECT COUNT(*) FROM chunk_spans", "chunk_spans"),
        3,
        "每块一段"
    );
    // 「同一天内的顺序」是给人看的序号：每天各自从 0 开始。
    let ordinals: Vec<(String, i64)> = {
        let mut statement = conn
            .prepare("SELECT day_key, ordinal FROM text_chunks ORDER BY day_key")
            .unwrap();
        let rows = statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        rows
    };
    assert_eq!(
        ordinals,
        vec![
            ("2026-09-20".to_owned(), 0),
            ("2026-09-21".to_owned(), 0),
            ("2026-09-22".to_owned(), 0),
        ]
    );
}

/// 打包在块表里成立：同一天的两篇短来源进同一块、占两个 span。
#[test]
fn two_short_sources_on_the_same_day_share_one_chunk() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.sqlite");
    let mut core = new_core(dir.path());

    let a = write_capture(&mut core, at(2026, 9, 20), "", "opA");
    let first = import_and_extract(&mut core, &a, "薄荷.txt", "薄荷长满了花盆。\n", "opA-mat1");
    let second = import_and_extract(&mut core, &a, "快递.txt", "快递放在门口。\n", "opA-mat2");
    assert_ne!(first, second, "前提：这是两篇不同的来源");

    let written = core.rebuild_text_chunks(None).unwrap();
    assert_eq!(written, 1, "同一天的两篇短正文应当合成一块");

    let conn = open(&path);
    let (text, kind, coverage): (String, String, String) = conn
        .query_row(
            "SELECT text, kind, coverage FROM text_chunks",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(text, "薄荷长满了花盆。\n\n快递放在门口。");
    assert_eq!(kind, "file", "两篇都是导入的正文");
    assert_eq!(coverage, "complete");
    let span_sources: Vec<String> = {
        let mut statement = conn
            .prepare("SELECT source_id FROM chunk_spans ORDER BY ordinal")
            .unwrap();
        let rows = statement
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        rows
    };
    assert_eq!(
        span_sources,
        vec![first, second],
        "两个 span 各是一篇，顺序按来源在记录里的位置"
    );
}

// ------------------------------------------------------------ 去重规则

/// `kind = 'text'` 且 `author_type = 'user'` 的来源要跳过：它的正文就是这条记录的
/// `draft_text`，不跳过同一句话会有两个载体（issue #44 记过这个坑）。
///
/// 这里直接往库里补一条这样的派生内容：`commit` 造出来的文字来源**没有原件**，
/// `extract_source` 会以「没有关联原件」拒绝（`InvalidState`），走不到提取那一步。
/// 补的这一行就是「万一它被提取过」的样子——去重规则要在那之前就挡住。
#[test]
fn the_records_own_text_source_is_not_a_second_carrier() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.sqlite");
    let mut core = new_core(dir.path());

    let capture_id = write_capture(&mut core, at(2026, 9, 20), "妈妈打电话来。", "op1");
    let revision = core.get_capture(&capture_id).unwrap().revision;
    let text_revision = core
        .commit(&capture_id, revision, "op-commit")
        .unwrap()
        .original_text_revision
        .expect("提交有内容的草稿会造出一个原始文字版本");
    assert_eq!(
        text_revision.author_type,
        diary_core::AuthorType::User,
        "这是用户自己写的文字版本，正是要去重的那个来源"
    );

    open(&path)
        .execute_batch(&format!(
            "INSERT INTO extracted_contents (id, source_id, source_revision_id, extractor_id, \
                 extractor_version, text, status, coverage, created_at) \
             VALUES ('ext-user-text', '{}', '{}', 'plain_text', '1', '妈妈打电话来。', 'ready', \
                 'complete', '2026-09-20T04:00:00.000Z');\
             INSERT INTO extracted_segments (id, content_id, ordinal, text, locator_type, \
                 source_revision_id) \
             VALUES ('seg-user-text', 'ext-user-text', 0, '妈妈打电话来。', 'text_range', '{}');",
            text_revision.source_id, text_revision.revision_id, text_revision.revision_id,
        ))
        .unwrap();

    let written = core.rebuild_text_chunks(None).unwrap();
    assert_eq!(written, 1, "同一句话只该产生一块");
    let conn = open(&path);
    assert_eq!(
        count(&conn, "SELECT COUNT(*) FROM chunk_spans", "chunk_spans"),
        1,
        "同一句话只该有一个载体（记录文字），不是两个"
    );
    let (source_id, capture, revision): (String, String, String) = conn
        .query_row(
            "SELECT source_id, capture_id, source_revision_id FROM chunk_spans",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        source_id, capture_id,
        "留下的是记录自己的文字，不是那个 text 来源"
    );
    assert_eq!(capture, capture_id);
    assert_eq!(revision, "", "记录自己的文字没有 source 修订");
    assert_ne!(source_id, text_revision.source_id);
}

// ------------------------------------------------------------ 可重入

/// 重建可重入：连跑两次块数与 span 数不变，块 id 也不变（块 id 是内容的函数），
/// 而且主键上没有重复行。
#[test]
fn rebuilding_is_reentrant_and_chunk_ids_are_stable() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.sqlite");
    let mut core = new_core(dir.path());
    let a = write_capture(&mut core, at(2026, 9, 20), "今天妈妈打电话来。", "opA");
    import_and_extract(&mut core, &a, "天气.txt", "今天风很大。\n", "opA-mat");
    write_capture(&mut core, at(2026, 9, 21), "路边的桂花开了。", "opB");

    let first = core.rebuild_text_chunks(None).unwrap();
    let ids = |conn: &rusqlite::Connection| -> Vec<String> {
        let mut statement = conn
            .prepare("SELECT chunk_id FROM text_chunks ORDER BY chunk_id")
            .unwrap();
        let rows = statement
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        rows
    };
    let conn = open(&path);
    let before = ids(&conn);
    let spans_before = count(&conn, "SELECT COUNT(*) FROM chunk_spans", "chunk_spans");
    assert_eq!(before.len() as i64, first);

    let second = core.rebuild_text_chunks(None).unwrap();
    assert_eq!(second, first, "同一份内容重建的块数不变");
    let conn = open(&path);
    assert_eq!(
        ids(&conn),
        before,
        "块 id 是内容的函数：内容没变，重建得到同一批 id"
    );
    assert_eq!(
        count(&conn, "SELECT COUNT(*) FROM chunk_spans", "chunk_spans"),
        spans_before,
        "先删后写，span 不会翻倍"
    );
    let (total, distinct): (i64, i64) = conn
        .query_row(
            "SELECT COUNT(*), COUNT(DISTINCT chunk_id) FROM text_chunks",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(total, distinct, "chunk_id 不能重复");
}

// ------------------------------------------------------------ 范围语义

/// 范围语义与 `rebuild_keyword_index` 一致：`Some([])` 什么都不看，
/// 单来源范围只算拥有该来源的记录。
#[test]
fn scope_selects_records_that_own_a_source_in_scope() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.sqlite");
    let mut core = new_core(dir.path());

    let a = write_capture(&mut core, at(2026, 9, 20), "", "opA");
    let source_a = import_and_extract(&mut core, &a, "A.txt", "今天天气不错。\n", "opA-mat");
    let b = write_capture(&mut core, at(2026, 9, 21), "", "opB");
    let source_b = import_and_extract(&mut core, &b, "B.txt", "路边的桂花开了。\n", "opB-mat");
    // 没有材料的记录：它只属于自己的日子，不属于任何来源范围。
    let c = write_capture(&mut core, at(2026, 9, 22), "妈妈来做饭。", "opC");

    assert_eq!(core.rebuild_text_chunks(None).unwrap(), 3);
    let conn = open(&path);
    let all_ids: Vec<String> = {
        let mut statement = conn
            .prepare("SELECT chunk_id FROM text_chunks ORDER BY day_key")
            .unwrap();
        let rows = statement
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        rows
    };
    assert_eq!(all_ids.len(), 3);

    // 空范围：什么都不看。返回值是 0，而且**不改动**已有的块（与关键词那一路同义：
    // 「没有要看的东西」不是「把索引清空」）。
    let empty: [String; 0] = [];
    assert_eq!(
        core.rebuild_text_chunks(Some(&empty)).unwrap(),
        0,
        "空范围不产出块"
    );
    let conn = open(&path);
    assert_eq!(
        count(&conn, "SELECT COUNT(*) FROM text_chunks", "text_chunks"),
        3,
        "空范围不改动已有的块"
    );
    assert_eq!(core.index_status(Some(&empty)).unwrap().total_chunks, 0);

    // 单来源范围：只有拥有该来源的记录参与重算，别的日子一块不动。
    let scope = vec![source_a.clone()];
    assert_eq!(core.rebuild_text_chunks(Some(&scope)).unwrap(), 1);
    let conn = open(&path);
    assert_eq!(count(&conn, "SELECT COUNT(*) FROM text_chunks", "text_chunks"), 3);
    let day_of = |conn: &rusqlite::Connection, chunk_id: &str| -> String {
        conn.query_row(
            "SELECT day_key FROM text_chunks WHERE chunk_id = ?1",
            params![chunk_id],
            |row| row.get(0),
        )
        .unwrap()
    };
    assert_eq!(
        day_of(&conn, &all_ids[0]),
        "2026-09-20",
        "范围里那一天的块还是同一块（重建是按日子重打包）"
    );
    assert_eq!(day_of(&conn, &all_ids[1]), "2026-09-21");
    assert_eq!(day_of(&conn, &all_ids[2]), "2026-09-22");

    let status = core.index_status(Some(&scope)).unwrap();
    assert_eq!(status.total_chunks, 1, "单来源范围只算拥有该来源的记录");
    assert_eq!(core.index_status(None).unwrap().total_chunks, 3);

    // 没有材料的记录不属于任何来源范围——它的块不该被算进来。
    let scope_b = vec![source_b.clone()];
    let status_b = core.index_status(Some(&scope_b)).unwrap();
    assert_eq!(status_b.total_chunks, 1);
    let conn = open(&path);
    let scoped_ids: Vec<String> = {
        let mut statement = conn
            .prepare(
                "SELECT t.chunk_id FROM text_chunks t WHERE EXISTS (SELECT 1 FROM chunk_spans s \
                 WHERE s.chunk_id = t.chunk_id AND s.capture_id = ?1)",
            )
            .unwrap();
        let rows = statement
            .query_map(params![c], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        rows
    };
    // 记录 C 的确有块（全库重建里有），但它没有来源，所以不在任何来源范围里。
    assert_eq!(scoped_ids.len(), 1);
    assert_ne!(scoped_ids[0], all_ids[1], "C 的块不是 B 的那一块");
}

// ------------------------------------------------------------ 状态与退化

/// 状态如实报「有块、没有向量、语义没就绪」，而关键词那一路一行都不变。
#[test]
fn status_reports_chunks_and_stays_honest_about_semantics() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    let a = write_capture(&mut core, at(2026, 9, 20), "今天妈妈打电话来。", "opA");
    import_and_extract(&mut core, &a, "天气.txt", "今天风很大。\n", "opA-mat");
    write_capture(&mut core, at(2026, 9, 21), "路边的桂花开了。", "opB");

    // 还没重建块：`chunker_version` 也已经**有值**——从这一片起块真的会进库，
    // 再报 None 就是假话（B3c-1 的文档里写明了「接进去之后才该有值」）。
    let before = core.index_status(None).unwrap();
    assert_eq!(before.total_chunks, 0, "没有重建过就没有块");
    assert_eq!(before.embedded_chunks, 0);
    assert_eq!(
        before.chunker_version.as_deref(),
        Some(CHUNKER_VERSION),
        "块真的进库了，版本要如实报出来"
    );
    assert!(!before.semantic_index_ready);
    assert_eq!(before.model_version, None, "这一片没有模型可报");
    assert!(before.keyword_index_ready, "关键词那一路是好的");
    assert!(
        before
            .reasons
            .iter()
            .any(|reason| reason.contains("语义索引未就绪")),
        "reasons 要说清语义那一路为什么没就绪：{:?}",
        before.reasons
    );

    core.rebuild_text_chunks(None).unwrap();

    let after = core.index_status(None).unwrap();
    assert_eq!(after.total_chunks, 2, "两天各一块");
    assert_eq!(
        after.embedded_chunks, 0,
        "这一片没有模型：没有任何一块有生效代次的向量"
    );
    assert!(!after.semantic_index_ready);
    assert_eq!(after.chunker_version.as_deref(), Some(CHUNKER_VERSION));
    assert_eq!(
        after.reasons.iter().filter(|reason| reason.contains("未就绪")).count(),
        1,
        "「未就绪」只该说一次：{:?}",
        after.reasons
    );

    // 关键词那一路的所有数字都不因为多了块而变化。
    assert_eq!(after.coverage, before.coverage, "coverage 仍然只描述关键词那一路");
    assert_eq!(after.keyword_index_ready, before.keyword_index_ready);
    assert_eq!(after.indexed_segments, before.indexed_segments);
    assert_eq!(after.total_segments, before.total_segments);
    assert_eq!(after.indexed_captures, before.indexed_captures);
    assert_eq!(after.index_rows, before.index_rows);
    assert_eq!(after.index_terms, before.index_terms);

    // 空范围：块数也是 0，不能退化成「看全部」。
    let empty: [String; 0] = [];
    assert_eq!(core.index_status(Some(&empty)).unwrap().total_chunks, 0);
}

/// 退化路径：没有模型、没有向量时，关键词检索完全照旧。
#[test]
fn keyword_search_is_untouched_by_chunks_without_a_model() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    let a = write_capture(&mut core, at(2026, 9, 20), "妈妈打电话来。", "opA");
    import_and_extract(&mut core, &a, "信.txt", "妈妈后来又写了一封信。\n", "opA-mat");

    let before = search(&mut core, "妈妈");
    let ids_before: Vec<String> = before.results.iter().map(|hit| hit.hit_id.clone()).collect();
    assert_eq!(ids_before.len(), 2, "记录文字与派生片段各命中一条");

    let written = core.rebuild_text_chunks(None).unwrap();
    assert!(written > 0, "先确保真的有块进库，否则这条测试测不到东西");

    let after = search(&mut core, "妈妈");
    let ids_after: Vec<String> = after.results.iter().map(|hit| hit.hit_id.clone()).collect();
    assert_eq!(ids_after, ids_before, "命中一行都不该变");
    assert_eq!(
        after.index_coverage, before.index_coverage,
        "coverage 的语义不变"
    );
    assert_eq!(after.results[0].snippet, before.results[0].snippet);
    // 块不该出现在关键词这一路里：它只认片段与记录文字。
    assert_eq!(core.count_search_matches("妈妈").unwrap(), 1);
    assert!(!core.index_status(None).unwrap().semantic_index_ready);
    assert_eq!(
        after.index_coverage,
        Coverage::Complete,
        "覆盖程度照旧是关键词那一路的口径"
    );
}

/// 状态是**算出来的**，不是写死的：把一代向量标成生效，`embedded_chunks` 与
/// `semantic_index_ready` 会跟着变，那句「未就绪」也会换成真正剩下的问题。
///
/// 产品路径现在到不了这个状态（这一片没有模型），所以这里直接改 `index_meta` 并塞
/// 一行向量。它钉住两件事：`counts` 里 `embedded` 那条分支真的会跑；将来某一代被
/// 激活之后状态会自己跟上，不必回来改常量。
#[test]
fn status_follows_the_active_generation() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.sqlite");
    let mut core = new_core(dir.path());
    write_capture(&mut core, at(2026, 9, 20), "妈妈打电话来。", "opA");
    write_capture(&mut core, at(2026, 9, 21), "路边的桂花开了。", "opB");
    core.rebuild_text_chunks(None).unwrap();

    let before = core.index_status(None).unwrap();
    assert_eq!(before.total_chunks, 2);
    assert_eq!(before.embedded_chunks, 0);
    assert!(!before.semantic_index_ready);
    assert!(before.reasons.iter().any(|reason| reason.contains("未就绪")));

    let conn = open(&path);
    conn.execute_batch(
        "INSERT INTO chunk_vectors (chunk_id, generation, model_version, dims, storage, \
             vector, created_at) \
         SELECT chunk_id, 1, 'test-model', 2, 'f32', x'0000', '2026-10-10T00:00:00.000Z' \
         FROM text_chunks;\
         UPDATE index_meta SET active_generation = 1;",
    )
    .unwrap();

    let ready = core.index_status(None).unwrap();
    assert_eq!(ready.embedded_chunks, 2, "生效代次覆盖了全部块");
    assert!(
        ready.semantic_index_ready,
        "有一代向量在服务、而且把范围内的块都覆盖了，就是就绪"
    );
    assert!(
        !ready.reasons.iter().any(|reason| reason.contains("未就绪")),
        "就绪之后不能再说自己未就绪：{:?}",
        ready.reasons
    );
    assert_eq!(
        ready.model_version.as_deref(),
        Some("test-model"),
        "model_version 是生效代次那一批向量的真值，不再恒为 None"
    );

    // 换代中间态：生效代次只覆盖了一部分。这时不能报就绪，而且要说清剩下多少——
    // 换成「真正剩下的那个问题」，而不是继续念「没有模型」。
    conn.execute(
        "DELETE FROM chunk_vectors WHERE chunk_id = (SELECT chunk_id FROM text_chunks LIMIT 1)",
        [],
    )
    .unwrap();
    let partial = core.index_status(None).unwrap();
    assert_eq!(partial.embedded_chunks, 1);
    assert!(!partial.semantic_index_ready);
    assert!(
        partial
            .reasons
            .iter()
            .any(|reason| reason.contains("只覆盖了 2 块里的 1 块")),
        "换代中间态要说清覆盖到哪一步：{:?}",
        partial.reasons
    );

    // 有生效代次但一块都没有：也不算就绪。
    conn.execute_batch("DELETE FROM chunk_vectors; DELETE FROM text_chunks;")
        .unwrap();
    let empty = core.index_status(None).unwrap();
    assert_eq!(empty.total_chunks, 0);
    assert_eq!(empty.embedded_chunks, 0);
    assert!(!empty.semantic_index_ready);
    assert!(
        empty
            .reasons
            .iter()
            .any(|reason| reason.contains("还没有文本块")),
        "{:?}",
        empty.reasons
    );
}

// ------------------------------------------------------------ v7 → v8 迁移

/// v7 库升到 v8：建出新表，旧数据一行未动，而且**不回填块**（块由重建入口产出）。
///
/// 照 `search_capture_test.rs` 里 `v6_library_migrates_and_backfills_capture_text` 的
/// 做法：把库手工退回到 v7 的样子（删掉 v8 的四张表、版本号退回去），再用 `Core::open`
/// 打开。这不是拿老构建真的建过库，验的是升级路径本身。
#[test]
fn v7_library_migrates_to_v8_without_backfilling_chunks() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.sqlite");

    let capture_id = {
        let mut core = new_core(dir.path());
        assert_eq!(core.schema_version().unwrap(), 8);
        let capture_id = write_capture(&mut core, at(2026, 9, 20), "妈妈打电话来。", "op1");
        import_and_extract(&mut core, &capture_id, "天气.txt", "今天天气不错。\n", "op-mat");
        assert!(core.rebuild_text_chunks(None).unwrap() > 0);
        capture_id
    };

    // 旧表的口径：迁移不该动它们一个字节。
    let legacy_counts = |conn: &rusqlite::Connection| -> Vec<i64> {
        [
            "captures",
            "source_items",
            "source_revisions",
            "extracted_contents",
            "extracted_segments",
            "search_docs",
            "search_grams",
            "search_capture_docs",
            "search_capture_grams",
            "domain_events",
        ]
        .iter()
        .map(|table| count(conn, &format!("SELECT COUNT(*) FROM {table}"), table))
        .collect()
    };
    let before = legacy_counts(&open(&path));

    {
        let conn = open(&path);
        conn.execute_batch(
            "DROP TABLE chunk_vectors;
             DROP TABLE chunk_spans;
             DROP TABLE text_chunks;
             DROP TABLE index_meta;
             UPDATE schema_migrations SET version = 7;
             PRAGMA user_version = 7;",
        )
        .unwrap();
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, 7, "先确认真的退回到了 v7");
    }

    let mut core = Core::open(&path).unwrap();
    assert_eq!(core.schema_version().unwrap(), 8, "打开时要升到 v8");

    // 旧数据一行未动。
    assert_eq!(
        legacy_counts(&open(&path)),
        before,
        "升级不该碰 v1–v7 的任何一张表"
    );
    // 记录文字与派生片段照旧能搜。
    assert_eq!(search(&mut core, "妈妈").results.len(), 1);
    assert_eq!(search(&mut core, "天气").results.len(), 1);

    // 新表建好了，但**没有回填块**：块是派生数据，由重建入口按需产出。
    let conn = open(&path);
    assert_eq!(
        count(&conn, "SELECT COUNT(*) FROM text_chunks", "text_chunks"),
        0,
        "迁移不回填块：块由重建入口产出"
    );
    assert_eq!(
        count(&conn, "SELECT COUNT(*) FROM chunk_spans", "chunk_spans"),
        0
    );
    let (active, building): (Option<i64>, Option<i64>) = conn
        .query_row(
            "SELECT active_generation, building_generation FROM index_meta WHERE id = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("v8 要建出 index_meta 的单行元数据");
    assert_eq!((active, building), (None, None), "还没有任何一代向量");

    let status = core.index_status(None).unwrap();
    assert_eq!(status.total_chunks, 0);
    assert_eq!(status.embedded_chunks, 0);

    // 升上来之后重建入口可用，块数与状态对得上。
    let rebuilt = core.rebuild_text_chunks(None).unwrap();
    assert!(rebuilt > 0, "迁移出的表要真的能用");
    assert_eq!(core.index_status(None).unwrap().total_chunks, rebuilt);
    assert_eq!(
        core.get_capture(&capture_id).unwrap().draft_text,
        "妈妈打电话来。",
        "升级之后记录内容原样保留"
    );
}

/// 重建块**不能**把「内容没变的块」的向量级联删掉。
///
/// 回归：`chunk_vectors` 对 `text_chunks` 是 `ON DELETE CASCADE`，而块重建原来是
/// 「先清空这一段/整库，再重写」。那样一来「重建块 → 算向量 → 切代次」这条路上，
/// 重建会把**正在服务的那一代**向量删掉，换代期间语义检索出现空洞——代次机制也就
/// 白设了。块 id 是内容的函数，所以正确做法是「只删消失的块 + UPSERT 保留其余」。
#[test]
fn rebuilding_keeps_vectors_of_unchanged_chunks() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.sqlite");
    let mut core = Core::open(&path).unwrap();
    write_capture(&mut core, at(2026, 9, 20), "妈妈打电话来说她体检结果不太好。", "op1");
    let built = core.rebuild_text_chunks(None).unwrap();
    assert!(built >= 1, "至少有一块");

    // 塞一行「当前代次」的假向量，模拟已经算过向量的状态。
    {
        let conn = open(&path);
        conn.execute_batch(
            "INSERT INTO chunk_vectors (chunk_id, generation, model_version, dims, storage, \
                 vector, created_at) \
             SELECT chunk_id, 1, 'test-model', 2, 'f32', x'0000', '2026-10-10T00:00:00.000Z' \
             FROM text_chunks;\
             UPDATE index_meta SET active_generation = 1;",
        )
        .unwrap();
    }
    assert_eq!(
        core.index_status(None).unwrap().embedded_chunks,
        built,
        "假向量应当被算成「已嵌入」"
    );

    // 再重建一次：内容没变，块 id 不变，向量必须还在。
    let again = core.rebuild_text_chunks(None).unwrap();
    assert_eq!(again, built, "重建同样的内容，块数不变");
    assert_eq!(
        core.index_status(None).unwrap().embedded_chunks,
        built,
        "内容没变的块重建后必须保住自己的向量（级联删除是被禁止的）"
    );
    let conn = open(&path);
    assert_eq!(
        count(&conn, "SELECT COUNT(*) FROM chunk_vectors", "chunk_vectors"),
        built,
        "向量行数不该因为一次重建而减少"
    );
}

/// 内容真的变了，旧块（连同它的向量）应当消失，新块建出来。
#[test]
fn rebuilding_drops_chunks_that_no_longer_exist() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.sqlite");
    let mut core = Core::open(&path).unwrap();
    let capture_id = write_capture(&mut core, at(2026, 9, 20), "第一版：妈妈打电话来。", "op1");
    let built = core.rebuild_text_chunks(None).unwrap();
    assert!(built >= 1);

    let old_ids: Vec<String> = {
        let conn = open(&path);
        let mut statement = conn
            .prepare("SELECT chunk_id FROM text_chunks ORDER BY chunk_id")
            .unwrap();
        statement
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
    };
    {
        let conn = open(&path);
        conn.execute_batch(
            "INSERT INTO chunk_vectors (chunk_id, generation, model_version, dims, storage, \
                 vector, created_at) \
             SELECT chunk_id, 1, 'test-model', 2, 'f32', x'0000', '2026-10-10T00:00:00.000Z' \
             FROM text_chunks;\
             UPDATE index_meta SET active_generation = 1;",
        )
        .unwrap();
    }

    // 改掉记录正文，再重建：旧块应当消失，它的向量也跟着走。
    let revision = core.get_capture(&capture_id).unwrap().revision;
    core.save_draft(&capture_id, "第二版：同事约我周末去爬山。", revision, "op2")
        .unwrap();
    core.rebuild_text_chunks(None).unwrap();

    let conn = open(&path);
    let new_ids: Vec<String> = {
        let mut statement = conn
            .prepare("SELECT chunk_id FROM text_chunks ORDER BY chunk_id")
            .unwrap();
        statement
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
    };
    assert!(
        new_ids.iter().all(|id| !old_ids.contains(id)),
        "正文变了，块 id 应当全变（块 id 是内容的函数）"
    );
    assert_eq!(
        count(&conn, "SELECT COUNT(*) FROM chunk_vectors", "chunk_vectors"),
        0,
        "消失的块不该留下孤儿向量"
    );
    assert_eq!(
        core.index_status(None).unwrap().embedded_chunks,
        0,
        "新块还没有向量"
    );
}

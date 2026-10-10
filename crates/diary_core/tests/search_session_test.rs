//! B3b 检索会话测试：分页、游标归属、快照失效、取消、过滤与摘录。
//!
//! 语料用真实路径造：建记录 → 导入文本 → 提取（同时入索引），再走 `search.*`。

use std::path::Path;

use chrono::{TimeZone, Utc};
use diary_core::{
    Core, Coverage, CreateDraftInput, ErrorCode, ImportManifest, ImportOrigin, ImportRequest,
    MatchedBy, SearchFilters, SearchMode, SearchPhase, SearchRequest, SourceKind,
};
use sha2::{Digest, Sha256};

fn sha_of(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

fn new_core(dir: &Path) -> Core {
    Core::open_in_memory_at(dir).unwrap()
}

/// 建一条指定日期的记录，导入文本并提取（提取会把正文写进索引）。
fn add_material(
    core: &mut Core,
    name: &str,
    text: &str,
    occurred: chrono::DateTime<Utc>,
    operation: &str,
) -> String {
    let capture_id = core
        .create_draft(CreateDraftInput {
            occurred_at: Some(occurred),
            time_zone: "Asia/Shanghai",
            utc_offset_minutes: 480,
            operation_id: &format!("{operation}-create"),
        })
        .unwrap()
        .id;
    add_material_to(core, &capture_id, name, text, operation, "op-mat")
}

/// 往一条**已有**记录里导入文本并提取。
fn add_material_to(
    core: &mut Core,
    capture_id: &str,
    name: &str,
    text: &str,
    operation: &str,
    suffix: &str,
) -> String {
    let bytes = text.as_bytes();
    let ticket = core
        .prepare_import(ImportRequest {
            capture_id,
            display_name: name,
            mime_hint: Some("text/plain"),
            size_hint: Some(bytes.len() as i64),
            origin: ImportOrigin::Picker,
            // operationId 要唯一：同一个 operation 前缀下可能导入多份材料。
            operation_id: &format!("{operation}-prepare-{suffix}"),
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
    let source_id = core
        .get_capture(capture_id)
        .unwrap()
        .ordered_source_ids
        .first()
        .cloned()
        .unwrap();
    core.extract_source(&source_id).unwrap();
    source_id
}

fn request(query: &str, page_size: i64) -> SearchRequest {
    SearchRequest {
        query: query.to_owned(),
        mode: SearchMode::Keyword,
        filters: SearchFilters::default(),
        page_size,
    }
}

/// 十段都含「妈妈」的文本，用空行分段。
fn ten_paragraphs() -> String {
    let mut text = String::new();
    for index in 0..10 {
        text.push_str(&format!("第{index}段，妈妈今天打电话来，聊了很久。"));
        text.push_str("\n\n");
    }
    text
}

#[test]
fn paging_is_stable_and_stops_at_the_end() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    add_material(
        &mut core,
        "日记.txt",
        &ten_paragraphs(),
        Utc.with_ymd_and_hms(2026, 9, 20, 4, 0, 0).unwrap(),
        "op1",
    );

    let first = core.start_search(request("妈妈", 3), 1).unwrap();
    assert_eq!(first.phase, SearchPhase::KeywordReady);
    assert_eq!(first.query_revision, 1);
    assert_eq!(first.results.len(), 3);
    assert!(first.cursor.is_some());
    assert!(first.warnings.is_empty(), "{:?}", first.warnings);
    assert_eq!(first.index_coverage, Coverage::Complete);

    let mut seen: Vec<String> = first.results.iter().map(|hit| hit.hit_id.clone()).collect();
    let mut cursor = first.cursor.clone();
    let mut pages = 1;
    while let Some(value) = cursor {
        let page = core.search_next_page(&first.session_id, Some(&value)).unwrap();
        pages += 1;
        seen.extend(page.results.iter().map(|hit| hit.hit_id.clone()));
        cursor = page.cursor.clone();
        if cursor.is_none() {
            assert_eq!(page.phase, SearchPhase::Done, "最后一页应当是 done");
        }
    }

    assert_eq!(pages, 4, "10 条按每页 3 条应当是 4 页");
    assert_eq!(seen.len(), 10, "翻完所有页应当不重不漏");
    let unique: std::collections::HashSet<_> = seen.iter().collect();
    assert_eq!(unique.len(), 10, "同一片段不能在两页里重复出现");

    // 旧游标不能重翻：拿第一页的游标再来一次是 cursor_expired。
    let stale = first.cursor.clone().unwrap();
    let error = core
        .search_next_page(&first.session_id, Some(&stale))
        .unwrap_err();
    assert_eq!(error.code(), ErrorCode::CursorExpired);
}

#[test]
fn cursor_from_another_session_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    add_material(
        &mut core,
        "日记.txt",
        &ten_paragraphs(),
        Utc.with_ymd_and_hms(2026, 9, 20, 4, 0, 0).unwrap(),
        "op1",
    );

    let first = core.start_search(request("妈妈", 3), 1).unwrap();
    let other = core.start_search(request("妈妈", 3), 2).unwrap();
    let foreign = other.cursor.clone().unwrap();

    let error = core
        .search_next_page(&first.session_id, Some(&foreign))
        .unwrap_err();
    assert_eq!(
        error.code(),
        ErrorCode::CursorExpired,
        "只能在自己的会话里翻页"
    );
}

#[test]
fn index_change_invalidates_the_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    add_material(
        &mut core,
        "日记.txt",
        &ten_paragraphs(),
        Utc.with_ymd_and_hms(2026, 9, 20, 4, 0, 0).unwrap(),
        "op1",
    );

    let session = core.start_search(request("妈妈", 3), 1).unwrap();

    // 会话期间又导入了新材料：索引变了，旧快照必须作废。
    add_material(
        &mut core,
        "新日记.txt",
        "妈妈又打了一次电话。\n\n第二段也提到妈妈。\n",
        Utc.with_ymd_and_hms(2026, 9, 21, 4, 0, 0).unwrap(),
        "op2",
    );

    let next = session.cursor.clone().unwrap();
    let error = core
        .search_next_page(&session.session_id, Some(&next))
        .unwrap_err();
    assert_eq!(error.code(), ErrorCode::SearchExpired);

    let error = core.search_snapshot(&session.session_id).unwrap_err();
    assert_eq!(error.code(), ErrorCode::SearchExpired);
}

#[test]
fn cancel_stops_paging_but_keeps_the_snapshot_readable() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    add_material(
        &mut core,
        "日记.txt",
        &ten_paragraphs(),
        Utc.with_ymd_and_hms(2026, 9, 20, 4, 0, 0).unwrap(),
        "op1",
    );

    let session = core.start_search(request("妈妈", 3), 1).unwrap();
    let cancelled = core.cancel_search(&session.session_id).unwrap();
    assert_eq!(cancelled.phase, SearchPhase::Cancelled);
    assert!(cancelled.cursor.is_none(), "取消后不该再给下一页游标");
    assert_eq!(
        cancelled.results.len(),
        session.results.len(),
        "取消不删除已经返回的结果"
    );

    // 快照仍可读（状态是「已取消」，不是「过期」）。
    let again = core.search_snapshot(&session.session_id).unwrap();
    assert_eq!(again.phase, SearchPhase::Cancelled);

    // 但不能再翻页。
    let cursor = session.cursor.clone().unwrap();
    let error = core
        .search_next_page(&session.session_id, Some(&cursor))
        .unwrap_err();
    assert_eq!(error.code(), ErrorCode::InvalidState);
}

#[test]
fn empty_query_and_degraded_modes_say_so() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    add_material(
        &mut core,
        "日记.txt",
        &ten_paragraphs(),
        Utc.with_ymd_and_hms(2026, 9, 20, 4, 0, 0).unwrap(),
        "op1",
    );

    let empty = core.start_search(request("   ", 10), 1).unwrap();
    assert!(empty.results.is_empty());
    assert_eq!(empty.phase, SearchPhase::Done);
    assert!(
        empty.warnings.iter().any(|warning| warning.contains("空")),
        "空查询要说清楚：{:?}",
        empty.warnings
    );

    let mut hybrid = request("妈妈", 10);
    hybrid.mode = SearchMode::Hybrid;
    hybrid.filters.include_old_diary_versions = true;
    let downgraded = core.start_search(hybrid, 2).unwrap();
    assert!(
        downgraded
            .warnings
            .iter()
            .any(|warning| warning.contains("混合检索退化为关键词")),
        "没有模型时混合必须如实说退化为关键词（#51）：{:?}",
        downgraded.warnings
    );
    assert!(
        downgraded
            .warnings
            .iter()
            .any(|warning| warning.contains("历史版本")),
        "历史版本开关没有效果也要说：{:?}",
        downgraded.warnings
    );
    // 降级不等于不返回结果。
    assert!(!downgraded.results.is_empty());
    assert!(downgraded
        .results
        .iter()
        .all(|hit| hit.matched_by == vec![MatchedBy::Keyword]));
}

#[test]
fn filters_apply_to_day_range_scope_and_kind() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    let old_source = add_material(
        &mut core,
        "旧的.txt",
        "妈妈上个月来过。\n\n那天的饭菜很好。\n",
        Utc.with_ymd_and_hms(2026, 8, 10, 4, 0, 0).unwrap(),
        "op-old",
    );
    let new_source = add_material(
        &mut core,
        "新的.txt",
        "妈妈今天打过电话。\n\n她说下周再来。\n",
        Utc.with_ymd_and_hms(2026, 9, 20, 4, 0, 0).unwrap(),
        "op-new",
    );

    let all = core.start_search(request("妈妈", 10), 1).unwrap();
    assert_eq!(all.results.len(), 2);
    // 日期从近到远：新的排在前面。
    assert_eq!(
        all.results[0].source_id.as_deref(),
        Some(new_source.as_str())
    );

    let mut only_old = request("妈妈", 10);
    only_old.filters.to_day_key = Some("2026-09-01".to_owned());
    let filtered = core.start_search(only_old, 2).unwrap();
    assert_eq!(filtered.results.len(), 1);
    assert_eq!(
        filtered.results[0].source_id.as_deref(),
        Some(old_source.as_str())
    );

    let mut only_new = request("妈妈", 10);
    only_new.filters.from_day_key = Some("2026-09-01".to_owned());
    let filtered = core.start_search(only_new, 3).unwrap();
    assert_eq!(filtered.results.len(), 1);
    assert_eq!(
        filtered.results[0].source_id.as_deref(),
        Some(new_source.as_str())
    );

    let mut scoped = request("妈妈", 10);
    scoped.filters.source_scope = Some(vec![old_source.clone()]);
    let filtered = core.start_search(scoped, 4).unwrap();
    assert_eq!(filtered.results.len(), 1);
    assert_eq!(filtered.results[0].day_key.as_deref(), Some("2026-08-10"));

    // 今天能被索引的内容都是「文件」类型（文本导入走的是 file）；
    // 用 image 过滤应当一条都不剩，证明过滤真的到了 SQL 里。
    let mut images = request("妈妈", 10);
    images.filters.kinds = vec![SourceKind::Image];
    let filtered = core.start_search(images, 5).unwrap();
    assert!(filtered.results.is_empty());

    let mut files = request("妈妈", 10);
    files.filters.kinds = vec![SourceKind::File];
    let filtered = core.start_search(files, 6).unwrap();
    assert_eq!(filtered.results.len(), 2);
}

#[test]
fn snippet_and_highlights_point_at_the_match() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    add_material(
        &mut core,
        "长文.txt",
        &format!(
            "{}妈妈{}\n",
            "前面有很多铺垫".repeat(6),
            "后面还有很多内容".repeat(6)
        ),
        Utc.with_ymd_and_hms(2026, 9, 20, 4, 0, 0).unwrap(),
        "op1",
    );

    let snapshot = core.start_search(request("妈妈", 10), 1).unwrap();
    let hit = &snapshot.results[0];
    let snippet = hit.snippet.as_deref().unwrap();

    assert_eq!(hit.highlights.len(), 1);
    let range = hit.highlights[0];
    let highlighted: String = snippet
        .chars()
        .skip(range.start as usize)
        .take((range.end - range.start) as usize)
        .collect();
    assert_eq!(highlighted, "妈妈", "高亮区间必须正好落在查询词上");
    assert!(
        snippet.starts_with('…') && snippet.ends_with('…'),
        "两边都截断时要有省略号：{snippet}"
    );
    // 命中对象还要带上原件名与定位信息。
    assert_eq!(hit.title.as_deref(), Some("长文.txt"));
    assert!(hit.locator.is_some());
    assert_eq!(hit.source_kind, SourceKind::File);
}

#[test]
fn rebuilding_the_same_segment_invalidates_the_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.sqlite");
    let mut core = Core::open(&path).unwrap();
    add_material(
        &mut core,
        "材料.txt",
        "妈妈打电话来了。\n",
        Utc.with_ymd_and_hms(2026, 9, 20, 4, 0, 0).unwrap(),
        "op1",
    );

    let session = core.start_search(request("妈妈", 10), 1).unwrap();
    assert_eq!(session.results.len(), 1);

    // 直接用第二个连接改掉派生正文（提取器换版本时就是这个效果），并记下重建前的
    // 文档行——改完要确认「片段数与 doc_id 集合都没变」，否则这条测试测的就不是
    // 审查指出的那个机制了。
    let (docs_before, ids_before) = {
        let conn = rusqlite::Connection::open(&path).unwrap();
        // 提取器会把正文规整过（去掉行尾换行等），所以按内容匹配不可靠：
        // 取这一条片段并断言拿对了，再改它。
        let (segment_id, old_text): (String, String) = conn
            .query_row("SELECT id, text FROM extracted_segments LIMIT 1", [], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .expect("先确认片段真的在库里");
        assert!(
            old_text.contains("打电话"),
            "先确认拿到的是那条片段，实际内容：{old_text:?}"
        );
        conn.execute(
            "UPDATE extracted_segments SET text = ?1 WHERE id = ?2",
            rusqlite::params!["妈妈没有再回来。\n", segment_id],
        )
        .unwrap();
        snapshot_docs(&conn)
    };

    // 同一个 Core、同一个会话：走公开的重建入口，重新索引同一个片段。
    core.rebuild_keyword_index(None).unwrap();

    let (docs_after, ids_after) = snapshot_docs(&rusqlite::Connection::open(&path).unwrap());
    assert_eq!(docs_after, docs_before, "前提：片段数没变");
    assert_eq!(
        ids_after, ids_before,
        "前提：SQLite 复用了同一个 doc_id——不然这条测试测的不是审查指出的机制"
    );

    // 内容真的换了：旧词不再命中，新词命中。
    assert!(
        core.start_search(request("打电话", 10), 2)
            .unwrap()
            .results
            .is_empty(),
        "旧词不该再命中"
    );
    assert_eq!(
        core.start_search(request("回来", 10), 3)
            .unwrap()
            .results
            .len(),
        1,
        "新词应当命中"
    );

    // 而进行中的旧会话必须发现快照已经失效，不能把新正文套进旧结果。
    let error = core
        .search_next_page(&session.session_id, session.cursor.as_deref())
        .unwrap_err();
    assert_eq!(
        error.code(),
        ErrorCode::SearchExpired,
        "重建同一个片段（行数与 doc_id 都没变）也必须让快照失效"
    );
}

/// 索引文档的行数，以及 doc_id 的集合。
fn snapshot_docs(conn: &rusqlite::Connection) -> (i64, Vec<i64>) {
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM search_docs", [], |row| row.get(0))
        .unwrap();
    let ids: Vec<i64> = conn
        .prepare("SELECT doc_id FROM search_docs ORDER BY doc_id")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    (count, ids)
}

#[test]
fn next_page_after_the_last_page_returns_an_empty_page() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    add_material(
        &mut core,
        "日记.txt",
        &ten_paragraphs(),
        Utc.with_ymd_and_hms(2026, 9, 20, 4, 0, 0).unwrap(),
        "op1",
    );

    // 一次翻到末页。
    let first = core.start_search(request("妈妈", 20), 1).unwrap();
    assert_eq!(first.phase, SearchPhase::Done);
    assert!(first.cursor.is_none());
    assert_eq!(first.results.len(), 10);

    // 末页之后再翻：必须是空页，不能从第 0 条重来（那会把第一页当成新一页）。
    let again = core.search_next_page(&first.session_id, None).unwrap();
    assert!(
        again.results.is_empty(),
        "末页之后再翻不该返回任何命中（返回了 {} 条）",
        again.results.len()
    );
    assert_eq!(again.phase, SearchPhase::Done);
    assert!(again.cursor.is_none());

    // 而且不该把会话里「当前这一页」改掉：快照仍然给得回最后一页。
    let snapshot = core.search_snapshot(&first.session_id).unwrap();
    assert_eq!(snapshot.results.len(), 10, "快照不该被空翻页污染");
}

#[test]
fn sessions_do_not_survive_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.sqlite");
    let session_id = {
        let mut core = Core::open(&path).unwrap();
        add_material(
            &mut core,
            "日记.txt",
            &ten_paragraphs(),
            Utc.with_ymd_and_hms(2026, 9, 20, 4, 0, 0).unwrap(),
            "op1",
        );
        core.start_search(request("妈妈", 3), 1).unwrap().session_id
    };

    let core = Core::open(&path).unwrap();
    let error = core.search_snapshot(&session_id).unwrap_err();
    assert_eq!(
        error.code(),
        ErrorCode::SearchExpired,
        "会话是内存态，重开后必须报过期而不是给旧结果"
    );
}

#[test]
fn unknown_session_reports_expired_not_not_found() {
    let dir = tempfile::tempdir().unwrap();
    let core = new_core(dir.path());
    let error = core.search_snapshot("search_不存在").unwrap_err();
    assert_eq!(error.code(), ErrorCode::SearchExpired);
    assert!(error.retryable(), "契约给这两个码的处理是「重新发起」");
}
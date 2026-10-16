//! B3d：用户自己写的文字（`captures.draft_text`）进关键词检索。
//!
//! 覆盖四类以前搜不到的东西与它们的边界：写下的字能命中、改字与清空、
//! 记录文字与派生片段同时命中、覆盖状态与范围/类型过滤、重建与 v5→v6 迁移回填。
//!
//! 语料与路径都按产品路径造：`create_draft` → `save_draft` 写文字，导入 → 提取
//! 产派生片段。断言带 reason，失败时能直接看出是哪一层的语义错了。

use std::path::Path;

use chrono::{DateTime, TimeZone, Utc};
use diary_core::{
    Core, Coverage, CreateDraftInput, ErrorCode, ImportManifest, ImportOrigin, ImportRequest,
    MatchedBy, SearchFilters, SearchHit, SearchMode, SearchRequest, SearchSnapshot, SourceKind,
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

/// 固定用 UTC+8 的上午：`day_key` 与 occurred_at 的本地日期一致。
fn at(year: i32, month: u32, day: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(year, month, day, 4, 0, 0).unwrap()
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
    let source_id = core
        .get_capture(capture_id)
        .unwrap()
        .ordered_source_ids
        .first()
        .cloned()
        .expect("导入的材料应当挂到记录上");
    core.extract_source(&source_id).unwrap();
    source_id
}

fn request(query: &str, filters: SearchFilters) -> SearchRequest {
    SearchRequest {
        query: query.to_owned(),
        mode: SearchMode::Keyword,
        filters,
        page_size: 20,
    }
}

fn search(core: &mut Core, query: &str) -> SearchSnapshot {
    core.start_search(request(query, SearchFilters::default()), 1)
        .unwrap()
}

/// 把高亮区间从摘录里切出来，用来核对它正好落在查询词上。
fn highlighted(hit: &SearchHit) -> String {
    let snippet = hit.snippet.as_deref().expect("命中必须带摘录");
    let range = hit.highlights.first().expect("命中必须带高亮");
    snippet
        .chars()
        .skip(range.start as usize)
        .take((range.end - range.start) as usize)
        .collect()
}

fn only_hit(snapshot: &SearchSnapshot) -> &SearchHit {
    assert_eq!(snapshot.results.len(), 1, "这个库只该有一条命中");
    &snapshot.results[0]
}

// ------------------------------------------------------------ 自己写的字能被搜到

#[test]
fn draft_text_is_searchable_as_a_text_hit() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    let capture_id = write_capture(
        &mut core,
        at(2026, 9, 20),
        "今天妈妈打电话来，聊了很久。",
        "op1",
    );

    let snapshot = search(&mut core, "妈妈");
    assert_eq!(
        snapshot.index_coverage,
        Coverage::Complete,
        "只有记录文字、且已进索引时覆盖状态就该是 complete：{:?}",
        snapshot.warnings
    );
    let hit = only_hit(&snapshot);
    assert_eq!(
        hit.source_kind,
        SourceKind::Text,
        "自己写的字属于 text 类型"
    );
    assert_eq!(hit.matched_by, vec![MatchedBy::Keyword]);
    assert_eq!(hit.coverage, Coverage::Complete);
    assert_eq!(
        hit.hit_id,
        format!("cap_{capture_id}"),
        "记录文字的命中 id 是 cap_ + captureId"
    );
    assert_eq!(hit.group_id, capture_id, "一条记录自成一个组");
    assert_eq!(
        hit.day_key.as_deref(),
        Some("2026-09-20"),
        "命中要带这条记录的日期"
    );
    assert!(
        hit.locator.is_none(),
        "记录文字没有可定位的原件，locator 必须如实为 None"
    );
    assert!(hit.source_id.is_none(), "记录文字不属于任何 source");
    assert!(hit.revision_id.is_none());
    assert!(hit.title.is_none());

    let snippet = hit.snippet.as_deref().unwrap();
    assert!(snippet.contains("妈妈"), "摘录要包含查询词：{snippet}");
    assert_eq!(highlighted(hit), "妈妈", "高亮必须正好落在查询词上");
}

#[test]
fn kinds_gate_the_capture_text_path() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    write_capture(&mut core, at(2026, 9, 20), "今天妈妈打电话来。", "op1");

    // 记录文字属于 text：kinds 为空或含 text 时才参与。
    let none = core
        .start_search(request("妈妈", SearchFilters::default()), 1)
        .unwrap();
    assert_eq!(none.results.len(), 1);

    let texts = core
        .start_search(
            request(
                "妈妈",
                SearchFilters {
                    kinds: vec![SourceKind::Text],
                    ..Default::default()
                },
            ),
            2,
        )
        .unwrap();
    assert_eq!(texts.results.len(), 1, "显式要 text 时记录文字要参与");

    let images = core
        .start_search(
            request(
                "妈妈",
                SearchFilters {
                    kinds: vec![SourceKind::Image],
                    ..Default::default()
                },
            ),
            3,
        )
        .unwrap();
    assert!(
        images.results.is_empty(),
        "只要图片时记录文字整路不参与，不能因为它是 text 就漏进来"
    );
}

// ------------------------------------------------------------ 改字、清空

#[test]
fn editing_text_replaces_the_old_terms() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.sqlite");
    let mut core = Core::open(&path).unwrap();
    let capture_id = write_capture(&mut core, at(2026, 9, 20), "妈妈打电话来。", "op1");
    assert_eq!(search(&mut core, "妈妈").results.len(), 1);

    save(&mut core, &capture_id, "爸爸来家里吃饭。", "op2");

    assert!(
        search(&mut core, "妈妈").results.is_empty(),
        "改字之后旧词不能再命中——索引必须是先删后写，不是累积"
    );
    assert_eq!(search(&mut core, "爸爸").results.len(), 1);

    // 直接看库：还是那一行文档，没有留下旧词项。
    let conn = rusqlite::Connection::open(&path).unwrap();
    let docs: i64 = conn
        .query_row("SELECT COUNT(*) FROM search_capture_docs", [], |row| {
            row.get(0)
        })
        .unwrap();
    let old_terms: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM search_capture_grams WHERE term = '妈妈'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(docs, 1, "一条记录只该有一行文档");
    assert_eq!(old_terms, 0, "旧词项必须随文档行一起清掉");
}

#[test]
fn clearing_text_removes_the_index_row() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.sqlite");
    let mut core = Core::open(&path).unwrap();
    let capture_id = write_capture(&mut core, at(2026, 9, 20), "妈妈打电话来。", "op1");

    let before: i64 = rusqlite::Connection::open(&path)
        .unwrap()
        .query_row("SELECT COUNT(*) FROM search_capture_docs", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(before, 1);

    save(&mut core, &capture_id, "", "op2");

    assert!(
        search(&mut core, "妈妈").results.is_empty(),
        "清空之后不能再命中"
    );
    let after: i64 = rusqlite::Connection::open(&path)
        .unwrap()
        .query_row("SELECT COUNT(*) FROM search_capture_docs", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(after, 0, "空文字不能留一条空文档让计数虚高");

    let status = core.index_status(None).unwrap();
    assert_eq!(status.total_captures, 0);
    assert_eq!(status.indexed_captures, 0);
}

#[test]
fn whitespace_only_text_is_not_a_capture() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    let capture_id = write_capture(&mut core, at(2026, 9, 20), "", "op1");
    import_and_extract(&mut core, &capture_id, "天气.txt", "今天风很大。\n", "op-mat");
    // 只打了一个全角空格（U+3000）：Rust 的 trim 认它是空白，索引侧会删掉这一行。
    // 状态侧的 SQL 必须用同一套空白定义，否则这里会永远报 partial 且修不好。
    save(&mut core, &capture_id, "\u{3000}", "op2");

    let status = core.index_status(None).unwrap();
    assert_eq!(
        status.total_captures, 0,
        "全角空格不算「有文字」：{:?}",
        status.reasons
    );
    assert_eq!(status.indexed_captures, 0);
    assert_eq!(
        status.coverage,
        Coverage::Complete,
        "材料索引是完好的，不能被一个空白草稿拖成 partial：{:?}",
        status.reasons
    );
    assert!(search(&mut core, "\u{3000}").results.is_empty());
    assert_eq!(search(&mut core, "风").results.len(), 1, "材料那一路照旧能搜");
}

// ------------------------------------------------------------ 两路命中

#[test]
fn capture_text_and_segment_hits_are_distinct_and_fragment_comes_first() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    let capture_id = write_capture(&mut core, at(2026, 9, 20), "", "op1");
    let source_id = import_and_extract(
        &mut core,
        &capture_id,
        "日记.txt",
        "妈妈打电话来。\n",
        "op-mat",
    );
    save(&mut core, &capture_id, "妈妈打电话来，聊了很久。", "op-save");

    let snapshot = search(&mut core, "妈妈");
    assert_eq!(
        snapshot.results.len(),
        2,
        "记录文字与派生片段各命中一条：{:?}",
        snapshot
            .results
            .iter()
            .map(|hit| hit.hit_id.clone())
            .collect::<Vec<_>>()
    );
    let segment = &snapshot.results[0];
    let capture = &snapshot.results[1];

    assert!(
        segment.locator.is_some(),
        "同日期同位置时片段排在记录文字前面（家族序）"
    );
    assert_eq!(segment.source_id.as_deref(), Some(source_id.as_str()));
    assert_eq!(segment.source_kind, SourceKind::File);
    assert_eq!(capture.locator, None);
    assert_eq!(capture.hit_id, format!("cap_{capture_id}"));

    assert_ne!(segment.hit_id, capture.hit_id, "两条命中的 id 必须不同");
    assert_ne!(segment.group_id, capture.group_id, "两条命中不属于同一组");
    let unique: std::collections::HashSet<&String> =
        snapshot.results.iter().map(|hit| &hit.hit_id).collect();
    assert_eq!(unique.len(), 2, "两路命中不能互相重复");
}

#[test]
fn commit_keeps_the_draft_text_indexed() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    let capture_id = write_capture(&mut core, at(2026, 9, 20), "妈妈打电话来。", "op1");
    let revision = core.get_capture(&capture_id).unwrap().revision;

    let committed = core
        .commit(&capture_id, revision, "op-commit")
        .unwrap()
        .capture;
    // commit 只把草稿文字复制成原始文字版本，草稿正文本身不变——所以那条索引
    // 依然有效，不需要在 commit 里重建（钉住这个行为，改动它时这里会红）。
    assert_eq!(
        committed.draft_text, "妈妈打电话来。",
        "提交不该清空或改写草稿正文"
    );
    assert_eq!(committed.state, diary_core::CaptureState::Committed);

    let snapshot = search(&mut core, "妈妈");
    assert_eq!(snapshot.results.len(), 1);
    assert_eq!(snapshot.results[0].hit_id, format!("cap_{capture_id}"));

    let status = core.index_status(None).unwrap();
    assert_eq!(status.total_captures, 1, "提交后的记录仍然算「有文字」");
    assert_eq!(status.indexed_captures, 1);
}

// ------------------------------------------------------------ 覆盖状态

#[test]
fn status_counts_captures_and_is_not_complete_when_one_is_missing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.sqlite");
    let mut core = Core::open(&path).unwrap();
    let first = write_capture(&mut core, at(2026, 9, 20), "妈妈打电话来。", "op1");
    let second = write_capture(&mut core, at(2026, 9, 21), "加班到很晚。", "op2");
    // 顺带一条派生片段，证明两边的覆盖都要算。
    import_and_extract(&mut core, &first, "风.txt", "今天风很大。\n", "op-mat");

    let status = core.index_status(None).unwrap();
    assert_eq!(status.total_captures, 2);
    assert_eq!(status.indexed_captures, 2);
    assert_eq!(status.total_segments, 1);
    assert_eq!(status.indexed_segments, 1);
    assert_eq!(status.coverage, Coverage::Complete);
    assert!(status.keyword_index_ready);
    assert!(
        status.index_rows > 0,
        "index_rows 要把记录文字的词项行算进去"
    );

    // 手动删掉一条记录的索引行：覆盖状态必须立刻不再是 complete。
    rusqlite::Connection::open(&path)
        .unwrap()
        .execute(
            "DELETE FROM search_capture_docs WHERE capture_id = ?1",
            rusqlite::params![second],
        )
        .unwrap();

    let after = core.index_status(None).unwrap();
    assert_eq!(after.total_captures, 2);
    assert_eq!(after.indexed_captures, 1, "少了一条记录的文字");
    assert_ne!(
        after.coverage,
        Coverage::Complete,
        "记录文字没覆盖完就不是 complete：{:?}",
        after.reasons
    );
    assert_eq!(after.coverage, Coverage::Partial);
    assert!(
        after
            .reasons
            .iter()
            .any(|reason| reason.contains("1 条记录的文字还没进索引")),
        "reasons 要说清是记录文字没进索引：{:?}",
        after.reasons
    );
    assert!(search(&mut core, "加班").results.is_empty());
    assert_eq!(search(&mut core, "妈妈").results.len(), 1, "别的记录不受影响");
}

// ------------------------------------------------------------ 重建

#[test]
fn rebuild_restores_the_capture_text_index() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.sqlite");
    let mut core = Core::open(&path).unwrap();
    write_capture(&mut core, at(2026, 9, 20), "妈妈打电话来。", "op1");
    let before = core.index_status(None).unwrap();
    assert_eq!(before.indexed_captures, 1);

    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch("DELETE FROM search_capture_grams; DELETE FROM search_capture_docs;")
            .unwrap();
    }
    assert!(
        search(&mut core, "妈妈").results.is_empty(),
        "索引行删掉之后就该搜不到"
    );
    let broken = core.index_status(None).unwrap();
    assert_eq!(broken.indexed_captures, 0);
    assert_ne!(broken.coverage, Coverage::Complete);

    let rebuilt = core.rebuild_keyword_index(None).unwrap();
    assert_eq!(
        rebuilt, 1,
        "返回值要把重建的记录文字一起算上（这里没有片段）"
    );
    assert_eq!(search(&mut core, "妈妈").results.len(), 1, "重建后又能搜到");

    let after = core.index_status(None).unwrap();
    assert_eq!(after.indexed_captures, 1);
    assert_eq!(after.coverage, Coverage::Complete);

    // 再重建一次不能留下重复行。
    let again = core.rebuild_keyword_index(None).unwrap();
    assert_eq!(again, 1);
    assert_eq!(
        core.index_status(None).unwrap().index_rows,
        after.index_rows,
        "重复重建必须是先清后写"
    );
}

// ------------------------------------------------------------ 范围过滤

#[test]
fn source_scope_maps_to_captures_that_own_a_source_in_scope() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());

    // A 与 C 各有一份材料（所以它们属于某个来源范围），B 没有任何材料。
    let a = write_capture(&mut core, at(2026, 9, 20), "", "opA");
    let source_a = import_and_extract(&mut core, &a, "A.txt", "今天天气不错。\n", "opA-mat");
    save(&mut core, &a, "妈妈打电话来。", "opA-save");

    let b = write_capture(&mut core, at(2026, 9, 21), "妈妈在加班。", "opB");

    let c = write_capture(&mut core, at(2026, 9, 22), "", "opC");
    let source_c = import_and_extract(&mut core, &c, "C.txt", "路边的桂花开了。\n", "opC-mat");
    save(&mut core, &c, "妈妈来做饭。", "opC-save");

    let all = search(&mut core, "妈妈");
    assert_eq!(all.results.len(), 3, "不限范围时三条记录的文字都算");

    let scoped = |core: &mut Core,
                  scope: Vec<String>,
                  revision: i64,
                  query: &str|
     -> SearchSnapshot {
        core.start_search(
            request(
                query,
                SearchFilters {
                    source_scope: Some(scope),
                    ..Default::default()
                },
            ),
            revision,
        )
        .unwrap()
    };

    let only_a = scoped(&mut core, vec![source_a.clone()], 2, "妈妈");
    assert_eq!(only_a.results.len(), 1);
    assert_eq!(
        only_a.results[0].hit_id,
        format!("cap_{a}"),
        "拥有该来源的记录，它的文字要算进这个范围"
    );

    let only_c = scoped(&mut core, vec![source_c], 3, "妈妈");
    assert_eq!(only_c.results.len(), 1);
    assert_eq!(only_c.results[0].hit_id, format!("cap_{c}"));

    // 空范围是「什么都不看」：记录文字与片段两路都不能退化成看全部。
    let empty_scope = scoped(&mut core, vec![], 4, "妈妈");
    assert!(
        empty_scope.results.is_empty(),
        "空范围对记录文字也是 0 条：{:?}",
        empty_scope
            .results
            .iter()
            .map(|hit| hit.hit_id.clone())
            .collect::<Vec<_>>()
    );
    let empty_fragments = scoped(&mut core, vec![], 5, "天气");
    assert!(
        empty_fragments.results.is_empty(),
        "空范围对片段那一路同样是 0 条（两边语义必须一致）"
    );

    // 不属于任何范围的记录（没有任何材料）在单来源范围里也不出现。
    assert!(
        !only_a.results.iter().any(|hit| hit.group_id == b),
        "没有材料的记录不属于任何来源范围"
    );

    // 不限范围时片段那一路还在：证明 scope 只是过滤，不是把索引关了。
    assert_eq!(search(&mut core, "天气").results.len(), 1);
}

// ------------------------------------------------------------ 会话失效

#[test]
fn editing_text_invalidates_a_running_session() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    let capture_id = write_capture(&mut core, at(2026, 9, 20), "妈妈打电话来。", "op1");

    let session = search(&mut core, "妈妈");
    assert_eq!(session.results.len(), 1);

    // 会话期间改了这条记录的文字：快照必须作废，不能拿旧结果糊弄新查询。
    save(&mut core, &capture_id, "爸爸来做饭。", "op2");

    let error = core.search_snapshot(&session.session_id).unwrap_err();
    assert_eq!(
        error.code(),
        ErrorCode::SearchExpired,
        "改了记录文字也算「索引变了」：{error}"
    );
}

// ------------------------------------------------------------ v5 → v6 迁移回填

/// 迁移回填：把库退回到「v6 的样子」（删掉 v7 的两张表、user_version 与
/// schema_migrations 退回 5），再用 `Core::open` 打开，验证两边的索引都在。
///
/// 这一步**不是**用一个老构建真的建过一次库：v5 状态是手工重建的（v6 的表删掉、
/// 版本号退回去），所以它验证的是「升级路径会建表并回填」这件事，而不是
/// 「老二进制写出的文件字节级长什么样」。后者需要一个旧构建产物，代价太大。
#[test]
fn v6_library_migrates_and_backfills_capture_text() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.sqlite");

    let capture_id = {
        let mut core = Core::open(&path).unwrap();
        assert_eq!(core.schema_version().unwrap(), 8);
        let capture_id = write_capture(&mut core, at(2026, 9, 20), "妈妈打电话来。", "op1");
        // 派生内容那一路也要有一条，验证升级不碰 v5 的索引。
        import_and_extract(&mut core, &capture_id, "天气.txt", "今天天气不错。\n", "op-mat");
        capture_id
    };

    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        // 「退回 v6」要把它之后的版本建的东西都拿掉，否则迁移会重复建表
        // （v7 的两张索引表、v8 的块与换代表）。v8 的表没有外键指向 v5–v7，
        // 但先删子表更稳。
        conn.execute_batch(
            "DROP TABLE search_capture_grams;
             DROP TABLE search_capture_docs;
             DROP TABLE chunk_vectors;
             DROP TABLE chunk_spans;
             DROP TABLE text_chunks;
             DROP TABLE index_meta;
             UPDATE schema_migrations SET version = 6;
             PRAGMA user_version = 6;",
        )
        .unwrap();
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, 6, "先确认真的退回到了 v6（代次表已在 v6，记录文字表在 v7）");
    }

    let mut core = Core::open(&path).unwrap();
    assert_eq!(
        core.schema_version().unwrap(),
        8,
        "打开时要升到最新（这里的重点是 v7 的回填；v8 只是建块表，不碰这些数据）"
    );

    // 记录文字被回填，能直接搜到。
    let snapshot = search(&mut core, "妈妈");
    assert_eq!(snapshot.results.len(), 1, "回填后记录文字要能被搜到");
    assert_eq!(snapshot.results[0].hit_id, format!("cap_{capture_id}"));

    // v5 已有的片段索引一行都没动，照旧能搜。
    assert_eq!(
        search(&mut core, "天气").results.len(),
        1,
        "升级不该动派生内容的索引"
    );

    let conn = rusqlite::Connection::open(&path).unwrap();
    let (docs, version, length): (i64, String, i64) = conn
        .query_row(
            "SELECT COUNT(*), MAX(tokenizer_version), COALESCE(SUM(text_length), 0) \
             FROM search_capture_docs",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(docs, 1, "回填只该建一行文档");
    assert_eq!(version, diary_core::TOKENIZER_VERSION, "版本要跟当前构建一致");
    assert_eq!(length, "妈妈打电话来。".chars().count() as i64);

    let status = core.index_status(None).unwrap();
    assert_eq!(status.total_captures, 1);
    assert_eq!(status.indexed_captures, 1);
    assert_eq!(status.coverage, Coverage::Complete);
}

// ------------------------------------------------------------ 内存库不落盘也要能用

#[test]
fn capture_index_works_on_an_in_memory_library() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    write_capture(&mut core, at(2026, 9, 20), "面试官问了很多，我有点紧张。", "op1");

    assert_eq!(search(&mut core, "面试官").results.len(), 1);
    assert_eq!(search(&mut core, "面试").results.len(), 1, "2-gram 并集要生效");
    assert_eq!(search(&mut core, "紧").results.len(), 1, "单字也要能搜到");
}
/// 状态里的词项数必须是**两路词项的并集**。
///
/// 审查发现：`index_terms` 原来是分别对 `search_grams`、`search_capture_grams`
/// 做 `COUNT(DISTINCT term)` 再相加，两路都含同一个词（比如「妈妈」）时会算两次。
#[test]
fn index_terms_counts_shared_terms_once() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.sqlite");
    let mut core = Core::open(&path).unwrap();

    // 同一条记录：自己写的文字里有「妈妈」，导入材料的片段里也有「妈妈」。
    let capture_id = write_capture(&mut core, at(2026, 9, 20), "妈妈打电话来。", "op1");
    import_and_extract(
        &mut core,
        &capture_id,
        "信.txt",
        "妈妈后来又写了一封信。\n",
        "op-mat",
    );

    let status = core.index_status(None).unwrap();
    let conn = rusqlite::Connection::open(&path).unwrap();
    let union: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM (\
                 SELECT term FROM search_grams \
                 UNION \
                 SELECT term FROM search_capture_grams)",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let two_path_sum: i64 = conn
        .query_row(
            "SELECT (SELECT COUNT(DISTINCT term) FROM search_grams) \
                  + (SELECT COUNT(DISTINCT term) FROM search_capture_grams)",
            [],
            |row| row.get(0),
        )
        .unwrap();

    assert!(
        two_path_sum > union,
        "前提：两路确实有重叠词项，否则这条测试测不到东西（并集 {union}，分别计数相加 {two_path_sum}）"
    );
    assert_eq!(
        status.index_terms, union,
        "index_terms 必须是两路词项的并集（并集 {union}，分别计数相加 {two_path_sum}）"
    );
}

/// 带范围的状态查询必须能穿过「两路词项取并集」那条 SQL。
///
/// 回归：数词项的 UNION 子查询里**有两处范围过滤**，绑定的参数份数必须跟着
/// SQL 里的占位符个数走。写死成一份时，范围查询会报
/// `InvalidParameterCount(1, 2)`——全库查询（`index_status(None)`）看不出来，
/// 只有带 `sourceScope` 调用才会触发，而且是 SQLite 层的报错，不测就发现不了。
#[test]
fn scoped_status_survives_the_union_query() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.sqlite");
    let mut core = Core::open(&path).unwrap();

    // 一条记录：自己写的文字 + 导入材料，两路都有内容。
    let capture_id = write_capture(&mut core, at(2026, 9, 20), "妈妈打电话来。", "op1");
    let source_id = import_and_extract(
        &mut core,
        &capture_id,
        "材料.txt",
        "妈妈后来又写了一封信。\n",
        "op-mat",
    );

    let scoped = core
        .index_status(Some(std::slice::from_ref(&source_id)))
        .unwrap();
    assert_eq!(scoped.total_segments, 1);
    assert_eq!(scoped.indexed_segments, 1);
    assert_eq!(
        scoped.total_captures, 1,
        "拥有该来源的记录，它的文字要算进这个范围"
    );
    assert_eq!(scoped.indexed_captures, 1);
    assert_eq!(scoped.coverage, Coverage::Complete);
    assert!(scoped.index_terms > 0, "并集查询要有结果");
    assert!(scoped.index_rows > 0);

    // 空范围仍然是「什么都不看」；这条路径上一条范围过滤都没有（占位符为 0），
    // 绑定也不能出错。
    let empty: [String; 0] = [];
    let none = core.index_status(Some(&empty)).unwrap();
    assert_eq!(none.total_segments, 0);
    assert_eq!(none.total_captures, 0);
    assert_eq!(none.index_rows, 0);
    assert_eq!(none.index_terms, 0);
}

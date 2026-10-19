//! 回收站（B7）：状态机、检索/AI 可见性、改归属日、预览令牌与永久清理。
//!
//! 这一片不接 UI，但每个断言都对着契约 3.1 / 4.1 / 8 节与验收里那几条：
//!
//! - `trash → restore` 往返后正文、来源、转写仍可读，`revision` 只增不减；
//! - 回收站里的记录**检索不到**（关键词与语义都不到），也不进 AI 日记材料；
//! - `previewPurge` 的清单与实际删掉的行**逐项一致**；
//! - 令牌过期 / 篡改 / 换记录一律拒绝；
//! - `purge` 之后 `captures` / `sources` / `extracted_*` / `text_chunks` / 向量都没了，
//!   `PurgeResult` 的计数与实际一致；
//! - `moveDay` 改 `day_key`，检索按新 day 命中、旧 day 不再出现；
//! - 同 `operationId` 重放返回同结果、不重复发事件；
//! - purge 之后做的备份/恢复不会把它带回来。

use std::path::{Path, PathBuf};

use diary_core::{
    CaptureState, Core, CreateDraftInput, EmbedOptions, Embedder, Embedding, ErrorCode, EventType,
    ImportManifest, ImportOrigin, ImportRequest, ProvenanceLink, SearchFilters, SearchMode,
    SearchRequest, SourceLocator,
};
use rusqlite::{params, Connection};
use sha2::{Digest, Sha256};

// ------------------------------------------------------------------ 夹具

/// 确定性 stub：向量只由正文哈希决定。这里不断言语义相似度，只断言「回收站里的
/// 记录不参与语义候选」——哈希向量足够。
struct HashEmbedder;

impl Embedder for HashEmbedder {
    fn model_version(&self) -> &str {
        "recycle-stub@000000000000"
    }

    fn dims(&self) -> usize {
        8
    }

    fn options(&self) -> EmbedOptions {
        EmbedOptions::default()
    }

    fn embed_document(&self, text: &str) -> diary_core::Result<Embedding> {
        Ok(Embedding::untruncated(diary_core::text_hash_vector(
            text,
            self.dims(),
        )))
    }

    fn embed_query(&self, text: &str) -> diary_core::Result<Embedding> {
        Ok(Embedding::untruncated(diary_core::text_hash_vector(
            text,
            self.dims(),
        )))
    }
}

fn new_core(dir: &Path) -> Core {
    Core::open_with_embedder_for_test(dir.join("library.sqlite"), Box::new(HashEmbedder))
        .expect("打开资料库")
}

fn at(day: u32) -> chrono::DateTime<chrono::Utc> {
    use chrono::TimeZone;
    chrono::Utc.with_ymd_and_hms(2026, 9, day, 4, 0, 0).unwrap()
}

fn sha_of(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

/// 新建 → 写正文 → 提交，返回 capture_id。
fn commit_text(core: &mut Core, day: u32, text: &str, op: &str) -> String {
    let capture_id = core
        .create_draft(CreateDraftInput {
            occurred_at: Some(at(day)),
            time_zone: "Asia/Shanghai",
            utc_offset_minutes: 480,
            operation_id: &format!("{op}-create"),
        })
        .expect("创建草稿")
        .id;
    let revision = core
        .save_draft(&capture_id, text, 1, &format!("{op}-save"))
        .expect("保存草稿")
        .revision;
    core.commit(&capture_id, revision, &format!("{op}-commit"))
        .expect("提交记录");
    capture_id
}

fn extract_all(core: &mut Core, capture_id: &str) {
    let capture = core.get_capture(capture_id).unwrap();
    for source_id in capture.ordered_source_ids {
        core.extract_source(&source_id).expect("提取当前来源");
    }
}

/// 导入一个文本原件（走真正的 prepare → 写暂存 → finish 路径），返回
/// `(asset_id, source_id)`。
fn import_text(
    core: &mut Core,
    capture_id: &str,
    name: &str,
    text: &str,
    op: &str,
) -> (String, String) {
    let bytes = text.as_bytes();
    let ticket = core
        .prepare_import(ImportRequest {
            capture_id,
            display_name: name,
            mime_hint: Some("text/plain"),
            size_hint: Some(bytes.len() as i64),
            origin: ImportOrigin::Picker,
            operation_id: &format!("{op}-prepare"),
        })
        .expect("申请导入票据");
    std::fs::write(&ticket.staging_ticket, bytes).unwrap();
    let status = core
        .finish_import(
            &ticket.import_id,
            &ticket.staging_ticket,
            ImportManifest {
                copied_bytes: bytes.len() as i64,
                sha256: sha_of(bytes),
                detected_mime: "text/plain".to_owned(),
                original_name: name.to_owned(),
            },
        )
        .expect("导入完成");
    let asset_id = status.asset_id.expect("导入一定产出资产");
    let source_id = core
        .get_capture(capture_id)
        .unwrap()
        .ordered_source_ids
        .last()
        .cloned()
        .expect("导入后一定多一个来源");
    (asset_id, source_id)
}

fn keyword_count(core: &mut Core, query: &str, day: Option<&str>, include_trashed: bool) -> usize {
    let filters = SearchFilters {
        from_day_key: day.map(str::to_owned),
        to_day_key: day.map(str::to_owned),
        include_trashed,
        ..SearchFilters::default()
    };
    core.start_search(
        SearchRequest {
            query: query.to_owned(),
            mode: SearchMode::Keyword,
            filters,
            page_size: 20,
        },
        1,
    )
    .expect("发起检索")
    .results
    .len()
}

fn diary_captures(core: &Core, day: &str) -> i64 {
    core.get_diary_day(day)
        .map(|day| day.material_counts.captures)
        .unwrap_or(0)
}

/// 直接写两版「引用 A」与一版「只引用 B」的日记版本，并把当前指针放在 A 的那一版。
///
/// 这里不走 `diary.generate`：生成要远端 Provider，而这一片要验的是**清理时怎么
/// 处理已经存在的引用**，不是生成质量。写入形状与 `diary::insert_version` 一致。
fn seed_diary_versions(
    path: &Path,
    day: &str,
    source_a: &str,
    revision_a: &str,
    source_b: &str,
    revision_b: &str,
) -> (String, String, String) {
    let conn = Connection::open(path).expect("外部连接");
    let links = |source_id: &str, revision_id: &str| {
        let link = ProvenanceLink {
            source_id: source_id.to_owned(),
            source_revision_id: revision_id.to_owned(),
            locator: SourceLocator::text_range(revision_id, 0, 1),
            body_range: None,
            note: None,
        };
        serde_json::to_string(&[link]).unwrap()
    };
    for (id, body, source_id, revision_id, created_at) in [
        (
            "ver_ref1",
            "第一版基于这条记录。",
            source_a,
            revision_a,
            "2026-09-20T10:00:00Z",
        ),
        (
            "ver_ref2",
            "第二版也基于这条记录。",
            source_a,
            revision_a,
            "2026-09-20T11:00:00Z",
        ),
        (
            "ver_survivor",
            "这一版只基于另一条记录。",
            source_b,
            revision_b,
            "2026-09-20T12:00:00Z",
        ),
    ] {
        conn.execute(
            "INSERT INTO diary_versions (id, day_key, parent_version_id, origin, body, \
             source_snapshot_id, style_snapshot_id, generation_info, provenance_links, \
             operation_id, created_at) VALUES (?1, ?2, NULL, 'generated', ?3, NULL, NULL, \
             NULL, ?4, NULL, ?5)",
            params![id, day, body, links(source_id, revision_id), created_at],
        )
        .unwrap();
    }
    conn.execute(
        "INSERT INTO diary_days (day_key, time_zone, current_version_id, updated_at, revision) \
         VALUES (?1, 'Asia/Shanghai', 'ver_ref2', '2026-09-20T11:00:00Z', 2)",
        params![day],
    )
    .unwrap();
    (
        "ver_ref1".to_owned(),
        "ver_ref2".to_owned(),
        "ver_survivor".to_owned(),
    )
}

struct Fixture {
    dir: tempfile::TempDir,
    path: PathBuf,
    core: Core,
    day: String,
    capture_a: String,
    capture_b: String,
    source_a: String,
    source_a_import: String,
    asset_a: String,
    sha_a: String,
    ref1: String,
    ref2: String,
    survivor: String,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.sqlite");
    let mut core = new_core(dir.path());
    let day = "2026-09-20".to_owned();

    let capture_a = commit_text(&mut core, 20, "外婆的樟木箱里有旧照片。", "op-a");
    let capture_b = commit_text(&mut core, 20, "海边的风把帽子吹走了。", "op-b");
    extract_all(&mut core, &capture_a);
    extract_all(&mut core, &capture_b);
    let source_a = core.get_capture(&capture_a).unwrap().ordered_source_ids[0].clone();
    let revision_a = core
        .extracted_content(&source_a)
        .unwrap()
        .expect("文字来源已经提取")
        .source_revision_id;

    let bytes = "导入的发票写着樟木箱的编号。".as_bytes().to_vec();
    let (asset_a, source_a_import) = import_text(
        &mut core,
        &capture_a,
        "发票.txt",
        "导入的发票写着樟木箱的编号。",
        "op-imp",
    );
    core.extract_source(&source_a_import).unwrap();
    let source_b = core.get_capture(&capture_b).unwrap().ordered_source_ids[0].clone();
    let revision_b = core
        .extracted_content(&source_b)
        .unwrap()
        .unwrap()
        .source_revision_id;

    core.build_semantic_index(None).unwrap();

    let (ref1, ref2, survivor) =
        seed_diary_versions(&path, &day, &source_a, &revision_a, &source_b, &revision_b);

    Fixture {
        dir,
        path,
        core,
        day,
        capture_a,
        capture_b,
        source_a,
        source_a_import,
        asset_a,
        sha_a: sha_of(&bytes),
        ref1,
        ref2,
        survivor,
    }
}

// ------------------------------------------------------------------ 往返

#[test]
fn trash_and_restore_round_trip_keeps_content_and_bumps_revision_once_per_write() {
    let mut fixture = fixture();
    let core = &mut fixture.core;
    let a = fixture.capture_a.clone();
    let source_a = fixture.source_a.clone();
    let before = core.get_capture(&a).unwrap();
    let extracted_before = core.extracted_content(&source_a).unwrap().unwrap();

    let trashed = core.trash_capture(&a, before.revision, "op-trash").unwrap();
    assert_eq!(trashed.state, CaptureState::Trashed);
    assert_eq!(trashed.revision, before.revision + 1);
    assert_eq!(trashed.draft_text, before.draft_text);
    assert_eq!(
        core.extracted_content(&source_a).unwrap().unwrap().text,
        extracted_before.text,
        "回收站里的正文/转写仍可读"
    );

    // 同一个 operationId 重放：同结果，不重复发事件。
    let replay = core.trash_capture(&a, before.revision, "op-trash").unwrap();
    assert_eq!(replay, trashed);
    let trash_events = core
        .events_since(0)
        .unwrap()
        .into_iter()
        .filter(|event| event.event_type == EventType::CaptureTrashed && event.entity_id == a)
        .count();
    assert_eq!(trash_events, 1, "重放不能重复发事件");

    let restored = core
        .restore_capture(&a, trashed.revision, "op-restore")
        .unwrap();
    assert_eq!(restored.state, CaptureState::Committed, "恢复回原状态");
    assert_eq!(restored.revision, trashed.revision + 1);

    let replay = core
        .restore_capture(&a, trashed.revision, "op-restore")
        .unwrap();
    assert_eq!(replay, restored);
    let restore_events = core
        .events_since(0)
        .unwrap()
        .into_iter()
        .filter(|event| event.event_type == EventType::CaptureRestored && event.entity_id == a)
        .count();
    assert_eq!(restore_events, 1);

    // 草稿进回收站后恢复回草稿：这也是「原状态」，不能被统一成 committed。
    let draft = core
        .create_draft(CreateDraftInput {
            occurred_at: Some(at(21)),
            time_zone: "Asia/Shanghai",
            utc_offset_minutes: 480,
            operation_id: "op-draft",
        })
        .unwrap();
    let trashed_draft = core
        .trash_capture(&draft.id, draft.revision, "op-trash-draft")
        .unwrap();
    let restored_draft = core
        .restore_capture(&draft.id, trashed_draft.revision, "op-restore-draft")
        .unwrap();
    assert_eq!(restored_draft.state, CaptureState::Draft);
}

#[test]
fn trash_state_machine_rejects_wrong_revision_and_wrong_state() {
    let mut fixture = fixture();
    let core = &mut fixture.core;
    let a = fixture.capture_a.clone();
    let capture = core.get_capture(&a).unwrap();

    let conflict = core
        .trash_capture(&a, capture.revision + 1, "op-bad-revision")
        .unwrap_err();
    assert_eq!(conflict.code(), ErrorCode::RevisionConflict);

    let trashed = core
        .trash_capture(&a, capture.revision, "op-trash")
        .unwrap();
    let again = core
        .trash_capture(&a, trashed.revision, "op-trash-again")
        .unwrap_err();
    assert_eq!(again.code(), ErrorCode::InvalidState);

    let restore_ok = core
        .restore_capture(&a, trashed.revision, "op-restore")
        .unwrap();
    let restore_again = core
        .restore_capture(&a, restore_ok.revision, "op-restore-again")
        .unwrap_err();
    assert_eq!(restore_again.code(), ErrorCode::InvalidState);
}

// ------------------------------------------------------------------ 可见性

#[test]
fn trashed_capture_is_hidden_from_keyword_semantic_and_ai_materials() {
    let mut fixture = fixture();
    let core = &mut fixture.core;
    let a = fixture.capture_a.clone();
    let source_a_import = fixture.source_a_import.clone();
    let day = fixture.day.clone();

    let visible_before = keyword_count(core, "樟木箱", None, false);
    assert!(
        visible_before >= 2,
        "正文与导入来源都应可搜：{visible_before}"
    );
    assert_eq!(
        keyword_count(core, "樟木箱", Some(&day), false),
        visible_before
    );
    let semantic_before = core
        .semantic_ranking("樟木箱", &SearchFilters::default())
        .unwrap();
    assert!(
        semantic_before
            .iter()
            .any(|(id, _)| id == &a || id == &source_a_import),
        "前提：语义候选里有 A：{semantic_before:?}"
    );
    assert_eq!(diary_captures(core, &day), 2);

    let capture = core.get_capture(&a).unwrap();
    let trashed = core
        .trash_capture(&a, capture.revision, "op-trash")
        .unwrap();

    assert_eq!(
        keyword_count(core, "樟木箱", None, false),
        0,
        "回收站里的记录不能出现在默认检索里"
    );
    assert!(keyword_count(core, "海边的风", None, false) >= 1);
    assert_eq!(
        keyword_count(core, "樟木箱", None, true),
        visible_before,
        "includeTrashed 才看得到，且必须是显式要求"
    );
    let semantic_after = core
        .semantic_ranking("樟木箱", &SearchFilters::default())
        .unwrap();
    assert!(
        !semantic_after
            .iter()
            .any(|(id, _)| id == &a || id == &source_a_import),
        "回收站里的记录不能出现在语义候选里：{semantic_after:?}"
    );
    assert_eq!(
        diary_captures(core, &day),
        1,
        "AI 日记材料按 state != 'trashed' 排除"
    );

    core.restore_capture(&a, trashed.revision, "op-restore")
        .unwrap();
    assert_eq!(keyword_count(core, "樟木箱", None, false), visible_before);
    assert_eq!(diary_captures(core, &day), 2);
}

// ------------------------------------------------------------------ moveDay

#[test]
fn move_day_changes_attribution_and_rebuilds_chunks_for_both_days() {
    let mut fixture = fixture();
    let core = &mut fixture.core;
    let a = fixture.capture_a.clone();
    let before = core.get_capture(&a).unwrap();
    let occurred_at = before.occurred_at;

    let visible_before = keyword_count(core, "樟木箱", Some("2026-09-20"), false);
    assert!(visible_before >= 2);
    assert_eq!(keyword_count(core, "樟木箱", Some("2026-09-21"), false), 0);

    let moved = core.move_day(&a, "2026-09-21", before.revision).unwrap();
    assert_eq!(moved.day_key, "2026-09-21");
    assert_eq!(moved.revision, before.revision + 1);
    assert_eq!(moved.occurred_at, occurred_at, "调整不改录音/事件时间戳");

    assert_eq!(
        keyword_count(core, "樟木箱", Some("2026-09-20"), false),
        0,
        "旧 day 不再出现"
    );
    assert_eq!(
        keyword_count(core, "樟木箱", Some("2026-09-21"), false),
        visible_before,
        "检索按新 day 命中"
    );

    // 块也真的搬了：块正文里有这句话的块，归属日只能是新 day。
    let chunk_days: Vec<String> = {
        let conn = Connection::open(&fixture.path).unwrap();
        let mut statement = conn
            .prepare("SELECT day_key FROM text_chunks WHERE text LIKE '%樟木箱%'")
            .unwrap();
        statement
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
    };
    assert!(!chunk_days.is_empty(), "前提：块里真的有这段正文");
    assert!(
        chunk_days.iter().all(|day| day == "2026-09-21"),
        "旧 day 不能留下块：{chunk_days:?}"
    );
    let moved_events = core
        .events_since(0)
        .unwrap()
        .into_iter()
        .filter(|event| event.event_type == EventType::CaptureMovedDay && event.entity_id == a)
        .count();
    assert_eq!(moved_events, 1);

    // 同一天是真正的 no-op：不推进 revision、不发事件。
    let same = core.move_day(&a, "2026-09-21", moved.revision).unwrap();
    assert_eq!(same.revision, moved.revision);
    assert_eq!(
        core.events_since(0)
            .unwrap()
            .into_iter()
            .filter(|event| event.event_type == EventType::CaptureMovedDay)
            .count(),
        1
    );

    let invalid = core.move_day(&a, "2026-9-1", moved.revision).unwrap_err();
    assert_eq!(invalid.code(), ErrorCode::InvalidArgument);

    // 回收站里的记录不能改归属日：先恢复。
    let trashed = core.trash_capture(&a, moved.revision, "op-trash").unwrap();
    let error = core
        .move_day(&a, "2026-09-22", trashed.revision)
        .unwrap_err();
    assert_eq!(error.code(), ErrorCode::InvalidState);
}

// ------------------------------------------------------------------ preview / purge

#[test]
fn preview_and_purge_delete_exactly_the_previewed_rows() {
    let mut fixture = fixture();
    let core = &mut fixture.core;
    let a = fixture.capture_a.clone();
    let b = fixture.capture_b.clone();
    let source_a = fixture.source_a.clone();
    let source_a_import = fixture.source_a_import.clone();
    let asset_a = fixture.asset_a.clone();
    let day = fixture.day.clone();
    let refs = vec![fixture.ref2.clone(), fixture.ref1.clone()];
    let survivor = fixture.survivor.clone();
    let blob_path =
        fixture
            .dir
            .path()
            .join(format!("blobs/{}/{}", &fixture.sha_a[..2], fixture.sha_a));
    assert!(blob_path.is_file(), "前提：原件文件真的在磁盘上");

    let capture = core.get_capture(&a).unwrap();
    core.trash_capture(&a, capture.revision, "op-trash")
        .unwrap();
    let preview = core.preview_purge(&a).unwrap();
    assert_eq!(preview.capture_id, a);
    assert_eq!(preview.asset_ids, vec![asset_a.clone()]);
    assert_eq!(
        preview.derived_content_ids.len(),
        2,
        "文字来源 + 导入来源各一条派生内容"
    );
    assert_eq!(preview.diary_versions_still_referencing, refs);

    let result = core
        .purge_capture(&a, &preview.preview_token, "op-purge")
        .unwrap();
    assert_eq!(result.purged_asset_ids, preview.asset_ids);
    assert_eq!(
        result.purged_derived_content_ids,
        preview.derived_content_ids
    );
    assert_eq!(
        result.purged_diary_version_ids,
        preview.diary_versions_still_referencing
    );

    // 记录、来源、派生内容、块、向量都没了。
    assert_eq!(
        core.get_capture(&a).unwrap_err().code(),
        ErrorCode::NotFound
    );
    assert_eq!(core.asset_stats().unwrap(), (0, 0));
    assert!(core.extracted_content(&source_a).unwrap().is_none());
    assert!(core.extracted_content(&source_a_import).unwrap().is_none());
    assert!(!blob_path.exists(), "blob 文件也要删掉");

    let conn = Connection::open(&fixture.path).unwrap();
    let capture_a_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM captures WHERE id = ?1",
            params![a],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(capture_a_count, 0);
    let capture_b_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM captures WHERE id = ?1",
            params![b],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(capture_b_count, 1, "别的记录不能受牵连");
    let source_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM source_items WHERE capture_id = ?1",
            params![a],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(source_count, 0);
    let revision_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM source_revisions WHERE source_id IN (?1, ?2)",
            params![source_a, source_a_import],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(revision_count, 0);
    let derived_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM extracted_contents WHERE source_id IN (?1, ?2)",
            params![source_a, source_a_import],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(derived_count, 0);
    let segment_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM extracted_segments WHERE content_id IN \
             (SELECT id FROM extracted_contents WHERE source_id IN (?1, ?2))",
            params![source_a, source_a_import],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(segment_count, 0);
    let chunk_text_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM text_chunks WHERE text LIKE '%樟木箱%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(chunk_text_count, 0, "块正文里不能留下被清理的文字");
    let span_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM chunk_spans WHERE capture_id = ?1",
            params![a],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(span_count, 0);
    let vector_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM chunk_vectors v JOIN text_chunks t ON t.chunk_id = v.chunk_id \
             WHERE t.text LIKE '%樟木箱%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(vector_count, 0);
    let version_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM diary_versions WHERE id IN (?1, ?2)",
            params![fixture.ref1, fixture.ref2],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(version_count, 0);
    let current: String = conn
        .query_row(
            "SELECT current_version_id FROM diary_days WHERE day_key = ?1",
            params![day],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        current, survivor,
        "当前版本被清掉后换到同一天剩下的最新一版"
    );
    let day_revision: i64 = conn
        .query_row(
            "SELECT revision FROM diary_days WHERE day_key = ?1",
            params![day],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(day_revision, 3);

    // 别的记录照常可搜；被清理的那条不在。
    assert_eq!(keyword_count(core, "樟木箱", None, false), 0);
    assert!(keyword_count(core, "海边的风", None, false) >= 1);
    assert_eq!(diary_captures(core, &day), 1);
    let purged_events = core
        .events_since(0)
        .unwrap()
        .into_iter()
        .filter(|event| event.event_type == EventType::CapturePurged && event.entity_id == a)
        .count();
    assert_eq!(purged_events, 1);

    // 同 operationId 重放：同结果、不重复发事件（记录已经没了也照常返回回执）。
    let replay = core
        .purge_capture(&a, &preview.preview_token, "op-purge")
        .unwrap();
    assert_eq!(replay, result);
    assert_eq!(
        core.events_since(0)
            .unwrap()
            .into_iter()
            .filter(|event| event.event_type == EventType::CapturePurged)
            .count(),
        1
    );

    // 备份/恢复不把已清理的内容带回来。
    let package = fixture.dir.path().join("after-purge.diarybak");
    core.create_backup(package.to_str().unwrap()).unwrap();
    let restored_dir = fixture.dir.path().join("restored");
    let report = core
        .restore_backup(package.to_str().unwrap(), restored_dir.to_str().unwrap())
        .unwrap();
    let mut restored = Core::open(&report.library_path).unwrap();
    assert_eq!(
        restored.get_capture(&a).unwrap_err().code(),
        ErrorCode::NotFound
    );
    assert_eq!(restored.asset_stats().unwrap(), (0, 0));
    assert_eq!(keyword_count(&mut restored, "樟木箱", None, false), 0);
    assert!(keyword_count(&mut restored, "海边的风", None, false) >= 1);
    assert!(restored.extracted_content(&source_a).unwrap().is_none());
    let restored_versions: i64 = Connection::open(&report.library_path)
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM diary_versions WHERE id IN (?1, ?2)",
            params![fixture.ref1, fixture.ref2],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(restored_versions, 0);
}

#[test]
fn preview_tokens_reject_tamper_wrong_capture_expiry_and_reuse() {
    let mut fixture = fixture();
    let core = &mut fixture.core;
    let a = fixture.capture_a.clone();

    let capture = core.get_capture(&a).unwrap();
    let _trashed = core
        .trash_capture(&a, capture.revision, "op-trash")
        .unwrap();

    // 没有预览就清理：拒绝。
    let error = core
        .purge_capture(&a, "no-such-token", "op-purge")
        .unwrap_err();
    assert_eq!(error.code(), ErrorCode::InvalidState);

    // 篡改令牌：拒绝。
    let preview = core.preview_purge(&a).unwrap();
    let error = core
        .purge_capture(&a, "forged-token", "op-forged")
        .unwrap_err();
    assert_eq!(error.code(), ErrorCode::InvalidState);

    // 换一条记录用同一个令牌：拒绝，而且原令牌仍然有效（绑定检查不改状态）。
    let other = commit_text(core, 22, "另一条记录。", "op-other");
    let other_capture = core.get_capture(&other).unwrap();
    let other_trashed = core
        .trash_capture(&other, other_capture.revision, "op-trash-other")
        .unwrap();
    let error = core
        .purge_capture(&other, &preview.preview_token, "op-wrong-capture")
        .unwrap_err();
    assert_eq!(error.code(), ErrorCode::InvalidState);
    core.purge_capture(&a, &preview.preview_token, "op-purge")
        .expect("原令牌仍然有效");

    // 一次性：清理成功之后再拿同一个令牌删别的东西，必须拒绝。
    let error = core
        .purge_capture(&other, &preview.preview_token, "op-reuse")
        .unwrap_err();
    assert!(matches!(
        error.code(),
        ErrorCode::InvalidState | ErrorCode::NotFound
    ));

    // 过期：TTL 设 0，预览之后立刻就是过期状态。
    core.set_purge_preview_ttl_for_test(0);
    let other_preview = core.preview_purge(&other).unwrap();
    let error = core
        .purge_capture(&other, &other_preview.preview_token, "op-expired")
        .unwrap_err();
    assert_eq!(error.code(), ErrorCode::InvalidState);
    core.set_purge_preview_ttl_for_test(600);

    // 不在回收站里的记录不能预览/清理。
    let live = commit_text(core, 23, "还没进回收站。", "op-live");
    let error = core.preview_purge(&live).unwrap_err();
    assert_eq!(error.code(), ErrorCode::InvalidState);
    let error = core
        .purge_capture(&live, &other_preview.preview_token, "op-live-purge")
        .unwrap_err();
    assert_eq!(error.code(), ErrorCode::InvalidState);

    // 预览之后记录被改过（这里用恢复再移入推进 revision）：旧令牌必须作废。
    let stale_preview = core.preview_purge(&other).unwrap();
    core.restore_capture(&other, other_trashed.revision, "op-restore-other")
        .unwrap();
    core.trash_capture(&other, other_trashed.revision + 1, "op-trash-other-again")
        .unwrap();
    let error = core
        .purge_capture(&other, &stale_preview.preview_token, "op-stale")
        .unwrap_err();
    assert_eq!(error.code(), ErrorCode::InvalidState);
    let fresh = core.preview_purge(&other).unwrap();
    core.purge_capture(&other, &fresh.preview_token, "op-purge-other")
        .unwrap();
}

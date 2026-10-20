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
    ImportManifest, ImportOrigin, ImportRequest, NativeRecordingStatus, ProvenanceLink,
    RecordingState, SearchFilters, SearchMode, SearchRequest, SegmentManifest, SourceLocator,
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
        .purge_capture(&a, &preview.preview_token, true, "op-purge")
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
    assert!(result.retained_diary_version_ids.is_empty());
    assert!(result.purged_ai_response_ids.is_empty());
    assert!(result.retained_source_snapshot_ids.is_empty());
    assert!(
        result.cleaned_receipt_count > 0,
        "这条记录的 commit/trash 回执里带正文，清理必须清掉它们"
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
        .purge_capture(&a, &preview.preview_token, true, "op-purge")
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
        .purge_capture(&a, "no-such-token", true, "op-purge")
        .unwrap_err();
    assert_eq!(error.code(), ErrorCode::InvalidState);

    // 篡改令牌：拒绝。
    let preview = core.preview_purge(&a).unwrap();
    let error = core
        .purge_capture(&a, "forged-token", true, "op-forged")
        .unwrap_err();
    assert_eq!(error.code(), ErrorCode::InvalidState);

    // 换一条记录用同一个令牌：拒绝，而且原令牌仍然有效（绑定检查不改状态）。
    let other = commit_text(core, 22, "另一条记录。", "op-other");
    let other_capture = core.get_capture(&other).unwrap();
    let other_trashed = core
        .trash_capture(&other, other_capture.revision, "op-trash-other")
        .unwrap();
    let error = core
        .purge_capture(&other, &preview.preview_token, true, "op-wrong-capture")
        .unwrap_err();
    assert_eq!(error.code(), ErrorCode::InvalidState);
    core.purge_capture(&a, &preview.preview_token, true, "op-purge")
        .expect("原令牌仍然有效");

    // 一次性：清理成功之后再拿同一个令牌删别的东西，必须拒绝。
    let error = core
        .purge_capture(&other, &preview.preview_token, true, "op-reuse")
        .unwrap_err();
    assert!(matches!(
        error.code(),
        ErrorCode::InvalidState | ErrorCode::NotFound
    ));

    // 过期：TTL 设 0，预览之后立刻就是过期状态。
    core.set_purge_preview_ttl_for_test(0);
    let other_preview = core.preview_purge(&other).unwrap();
    let error = core
        .purge_capture(&other, &other_preview.preview_token, true, "op-expired")
        .unwrap_err();
    assert_eq!(error.code(), ErrorCode::InvalidState);
    core.set_purge_preview_ttl_for_test(600);

    // 不在回收站里的记录不能预览/清理。
    let live = commit_text(core, 23, "还没进回收站。", "op-live");
    let error = core.preview_purge(&live).unwrap_err();
    assert_eq!(error.code(), ErrorCode::InvalidState);
    let error = core
        .purge_capture(&live, &other_preview.preview_token, true, "op-live-purge")
        .unwrap_err();
    assert_eq!(error.code(), ErrorCode::InvalidState);

    // 预览之后记录被改过（这里用恢复再移入推进 revision）：旧令牌必须作废。
    let stale_preview = core.preview_purge(&other).unwrap();
    core.restore_capture(&other, other_trashed.revision, "op-restore-other")
        .unwrap();
    core.trash_capture(&other, other_trashed.revision + 1, "op-trash-other-again")
        .unwrap();
    let error = core
        .purge_capture(&other, &stale_preview.preview_token, true, "op-stale")
        .unwrap_err();
    assert_eq!(error.code(), ErrorCode::InvalidState);
    let fresh = core.preview_purge(&other).unwrap();
    core.purge_capture(&other, &fresh.preview_token, true, "op-purge-other")
        .unwrap();
}

// ------------------------------------------------------------------ #155 复审 P1-1

#[test]
fn trash_refuses_captures_with_in_flight_import_or_recording() {
    let mut fixture = fixture();
    let core = &mut fixture.core;
    let b = fixture.capture_b.clone();
    let capture = core.get_capture(&b).unwrap();

    // 在途导入：trash 拒绝，消息里点名是哪一类会话。
    let ticket = core
        .prepare_import(ImportRequest {
            capture_id: &b,
            display_name: "在途.txt",
            mime_hint: Some("text/plain"),
            size_hint: Some(4),
            origin: ImportOrigin::Picker,
            operation_id: "op-inflight-import",
        })
        .unwrap();
    let error = core
        .trash_capture(&b, capture.revision, "op-trash-inflight-import")
        .unwrap_err();
    assert_eq!(error.code(), ErrorCode::InvalidState);
    assert!(error.to_string().contains("导入"), "{error}");
    core.cancel_import(&ticket.import_id).unwrap();

    // 取消之后可以正常移入。
    let trashed = core
        .trash_capture(&b, capture.revision, "op-trash-b")
        .unwrap();
    core.restore_capture(&b, trashed.revision, "op-restore-b")
        .unwrap();

    // 在途录音（preparing）同样拒绝。
    let recording = core.prepare_recording(&b, "op-inflight-recording").unwrap();
    let current = core.get_capture(&b).unwrap();
    let error = core
        .trash_capture(&b, current.revision, "op-trash-inflight-recording")
        .unwrap_err();
    assert_eq!(error.code(), ErrorCode::InvalidState);
    assert!(error.to_string().contains("录音"), "{error}");

    // error 是已收尾状态（不在在途清单里），之后可以移入。
    core.update_recording_state(NativeRecordingStatus {
        recording_id: recording.recording_id.clone(),
        state: RecordingState::Error,
        wall_clock: at(20),
        elapsed_ms: 0,
        persisted_through_ms: 0,
        input_device_changed: false,
        message: Some("测试收尾".to_owned()),
    })
    .unwrap();
    let current = core.get_capture(&b).unwrap();
    core.trash_capture(&b, current.revision, "op-trash-b-after-recording")
        .unwrap();
}

#[test]
fn trashed_capture_is_read_only_for_revise_import_and_recording_writes() {
    let mut fixture = fixture();
    let core = &mut fixture.core;
    let a = fixture.capture_a.clone();
    let source_a = fixture.source_a.clone();
    let capture = core.get_capture(&a).unwrap();
    let trashed = core
        .trash_capture(&a, capture.revision, "op-trash")
        .unwrap();

    // 改正文：回收站里只读。
    let error = core
        .revise_text(
            &source_a,
            "回收站里不该被改写",
            trashed.revision,
            "op-revise-trashed",
        )
        .unwrap_err();
    assert_eq!(error.code(), ErrorCode::InvalidState);
    assert!(error.to_string().contains("回收站"), "{error}");

    // 新的导入/录音票据：不再签发。
    let error = core
        .prepare_import(ImportRequest {
            capture_id: &a,
            display_name: "回收站里.txt",
            mime_hint: Some("text/plain"),
            size_hint: Some(4),
            origin: ImportOrigin::Picker,
            operation_id: "op-import-trashed",
        })
        .unwrap_err();
    assert_eq!(error.code(), ErrorCode::InvalidState);
    let error = core.prepare_recording(&a, "op-recording-trashed").unwrap_err();
    assert_eq!(error.code(), ErrorCode::InvalidState);

    // 旧库/异常路径（会话在 trash 之前就存在）：finish/finalize/register/updateState
    // 都必须拒绝，且不能搬动暂存字节。
    let staging = fixture.dir.path().join("staging/legacy-import.part");
    std::fs::create_dir_all(staging.parent().unwrap()).unwrap();
    std::fs::write(&staging, b"legacy").unwrap();
    {
        let conn = Connection::open(&fixture.path).unwrap();
        conn.execute(
            "INSERT INTO import_sessions (id, capture_id, display_name, origin, \
             staging_rel_path, state, copied_bytes, created_at, updated_at) \
             VALUES ('imp_legacy', ?1, '旧导入', 'picker', 'staging/legacy-import.part', \
             'prepared', 0, '2026-09-20T04:00:00Z', '2026-09-20T04:00:00Z')",
            params![a],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO recording_sessions (id, capture_id, asset_id, staging_rel_path, \
             state, started_at, updated_at) \
             VALUES ('rec_legacy', ?1, 'asset_legacy', 'staging/rec_legacy', 'recording', \
             '2026-09-20T04:00:00Z', '2026-09-20T04:00:00Z')",
            params![a],
        )
        .unwrap();
    }
    let error = core
        .finish_import(
            "imp_legacy",
            staging.to_str().unwrap(),
            ImportManifest {
                copied_bytes: 6,
                sha256: sha_of(b"legacy"),
                detected_mime: "text/plain".to_owned(),
                original_name: "旧导入.txt".to_owned(),
            },
        )
        .unwrap_err();
    assert_eq!(error.code(), ErrorCode::InvalidState);
    assert!(staging.is_file(), "被拒绝的 finish 不能搬走/删掉暂存文件");

    let error = core
        .finalize_recording("rec_legacy", 0, "user")
        .unwrap_err();
    assert_eq!(error.code(), ErrorCode::InvalidState);
    let error = core
        .register_segment(
            "rec_legacy",
            0,
            SegmentManifest {
                segment_id: "seg_legacy".to_owned(),
                relative_file_name: "0.part".to_owned(),
                duration_ms: 1000,
                sha256: sha_of(b"segment"),
                byte_size: 7,
                closed_at: Some(at(20)),
            },
        )
        .unwrap_err();
    assert_eq!(error.code(), ErrorCode::InvalidState);
    let error = core
        .update_recording_state(NativeRecordingStatus {
            recording_id: "rec_legacy".to_owned(),
            state: RecordingState::Paused,
            wall_clock: at(20),
            elapsed_ms: 1000,
            persisted_through_ms: 0,
            input_device_changed: false,
            message: None,
        })
        .unwrap_err();
    assert_eq!(error.code(), ErrorCode::InvalidState);

    // preview 也拒绝：旧库里真有在途会话时，先收尾再清理。
    let error = core.preview_purge(&a).unwrap_err();
    assert_eq!(error.code(), ErrorCode::InvalidState);
    assert!(error.to_string().contains("录音"), "{error}");
}

// ------------------------------------------------------------------ #155 复审 P1-2 / #161

#[test]
fn purge_cleans_body_bearing_receipts_and_ai_responses_but_reports_snapshots() {
    let mut fixture = fixture();
    let core = &mut fixture.core;
    let a = fixture.capture_a.clone();
    let b = fixture.capture_b.clone();
    let source_a = fixture.source_a.clone();
    let day = fixture.day.clone();

    // 一条围绕 A 的 AI 回应（请求与正文都带正文），以及一条只围绕 B 的。
    {
        let conn = Connection::open(&fixture.path).unwrap();
        for (id, capture_id, request, body) in [
            (
                "rsp_a",
                a.clone(),
                "问：樟木箱里有什么？",
                "回答：外婆的樟木箱里有旧照片。",
            ),
            ("rsp_b", b.clone(), "问：海边的风。", "回答：帽子被吹走了。"),
        ] {
            conn.execute(
                "INSERT INTO ai_responses (id, request_text, capture_ids, provider_id, model, \
                 body, state, error_code, created_at, updated_at) \
                 VALUES (?1, ?2, ?3, NULL, NULL, ?4, 'ready', NULL, \
                 '2026-09-20T05:00:00Z', '2026-09-20T05:00:00Z')",
                params![id, request, serde_json::to_string(&[capture_id]).unwrap(), body],
            )
            .unwrap();
        }
        // 两份输入快照：一份冻结了 A 的材料，一份只冻结 B。
        for (id, hash, materials) in [
            (
                "snp_a",
                "hash-a",
                serde_json::json!([
                    {"capture_id": a.clone(), "text": "外婆的樟木箱里有旧照片。"},
                    {"capture_id": b.clone(), "text": "海边的风把帽子吹走了。"}
                ]),
            ),
            (
                "snp_b",
                "hash-b",
                serde_json::json!([{"capture_id": b.clone(), "text": "海边的风把帽子吹走了。"}]),
            ),
        ] {
            let body = serde_json::json!({"day_key": day.clone(), "materials": materials});
            conn.execute(
                "INSERT INTO diary_source_snapshots (id, day_key, snapshot_hash, body, \
                 prompt_version, provider_id, model, generation_options, covered_sources, \
                 unprocessed_sources, created_at) \
                 VALUES (?1, ?2, ?3, ?4, 'prompt-test', NULL, NULL, '{}', 1, 0, \
                 '2026-09-20T05:00:00Z')",
                params![id, &day, hash, serde_json::to_string(&body).unwrap()],
            )
            .unwrap();
        }
    }

    // error 状态的导入会故意保留暂存文件；purge 必须把它一起收尾。
    let legacy_staging = fixture.dir.path().join("staging/legacy-error.part");
    std::fs::create_dir_all(legacy_staging.parent().unwrap()).unwrap();
    std::fs::write(&legacy_staging, b"legacy staging bytes").unwrap();
    {
        let conn = Connection::open(&fixture.path).unwrap();
        conn.execute(
            "INSERT INTO import_sessions (id, capture_id, display_name, origin, \
             staging_rel_path, state, copied_bytes, created_at, updated_at) \
             VALUES ('imp_error_legacy', ?1, '失败的导入', 'picker', \
             'staging/legacy-error.part', 'error', 0, \
             '2026-09-20T05:00:00Z', '2026-09-20T05:00:00Z')",
            params![a],
        )
        .unwrap();
    }

    // 先改一次正文，留下一个 result_json 带正文的 revise_text 回执。
    let capture = core.get_capture(&a).unwrap();
    core.revise_text(
        &source_a,
        "外婆的樟木箱里有旧照片，还有一张车票。",
        capture.revision,
        "op-a-revise",
    )
    .unwrap();
    let capture = core.get_capture(&a).unwrap();
    let revision = capture.revision;
    core.trash_capture(&a, revision, "op-a-trash").unwrap();

    let preview = core.preview_purge(&a).unwrap();
    assert_eq!(preview.ai_response_ids, vec!["rsp_a".to_owned()]);
    assert_eq!(
        preview.retained_source_snapshot_ids,
        vec!["snp_a".to_owned()],
        "只列真的冻结了 A 材料的快照，不把 B 的算进来"
    );

    let result = core
        .purge_capture(&a, &preview.preview_token, false, "op-a-purge")
        .unwrap();
    assert_eq!(result.purged_ai_response_ids, vec!["rsp_a".to_owned()]);
    assert_eq!(
        result.retained_source_snapshot_ids,
        vec!["snp_a".to_owned()],
        "快照按 #161 方案 A 如实保留并报告"
    );
    assert!(result.cleaned_receipt_count > 0, "带正文的回执必须清掉");

    // 同一 operationId 重放：回执里没有正文；这里的三个写入回执都已清掉，重放
    // 落到「记录/来源不存在」，不会把被删的正文吐回来。
    let error = core.commit(&a, revision, "op-a-commit").unwrap_err();
    assert_eq!(error.code(), ErrorCode::NotFound);
    let error = core
        .trash_capture(&a, revision, "op-a-trash")
        .unwrap_err();
    assert_eq!(error.code(), ErrorCode::NotFound);
    let error = core
        .revise_text(
            &source_a,
            "外婆的樟木箱里有旧照片，还有一张车票。",
            revision,
            "op-a-revise",
        )
        .unwrap_err();
    assert_eq!(error.code(), ErrorCode::NotFound);

    let conn = Connection::open(&fixture.path).unwrap();
    let receipts_with_capture: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM operation_receipts \
             WHERE kind <> 'captures.purge' AND instr(result_json, ?1) > 0",
            params![a],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(receipts_with_capture, 0, "非 purge 回执里不能再留 A 的正文");

    let a_responses: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM ai_responses WHERE id = 'rsp_a'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(a_responses, 0);
    let b_responses: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM ai_responses WHERE id = 'rsp_b'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(b_responses, 1, "别的记录的 AI 回应不能受牵连");

    // #161 验收：快照行还在、body 里仍可能含 A 的正文——所以结果必须如实报告，
    // 而不是假装「永久清理」把快照也删干净了。长期解是 #161 方案 C（映射表）。
    assert!(!legacy_staging.exists(), "error 导入的暂存文件也要随 purge 收尾");

    let snapshot_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM diary_source_snapshots", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(snapshot_count, 2);
    let snapshot_with_text: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM diary_source_snapshots WHERE body LIKE '%外婆的樟木箱%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(snapshot_with_text, 1, "方案 A 下快照 body 保留，数量已在结果里报告");

    // B 仍然完整可搜。
    assert!(keyword_count(core, "海边的风", None, false) >= 1);

    // 预览/清理返回的快照与回执口径写进文档：这里把结果里的数量钉住。
    assert_eq!(
        result.retained_source_snapshot_ids.len(),
        1,
        "purge 的返回值必须让用户看到保留了几份快照"
    );
}

// ------------------------------------------------------------------ #155 复审 P1-3

#[test]
fn purge_can_keep_diary_versions_when_the_user_declines() {
    let mut fixture = fixture();
    let core = &mut fixture.core;
    let a = fixture.capture_a.clone();
    let day = fixture.day.clone();
    let refs = vec![fixture.ref2.clone(), fixture.ref1.clone()];

    let capture = core.get_capture(&a).unwrap();
    core.trash_capture(&a, capture.revision, "op-trash")
        .unwrap();
    let preview = core.preview_purge(&a).unwrap();
    assert_eq!(preview.diary_versions_still_referencing, refs);

    let result = core
        .purge_capture(&a, &preview.preview_token, false, "op-purge-keep")
        .unwrap();
    assert!(result.purged_diary_version_ids.is_empty());
    assert_eq!(result.retained_diary_version_ids, refs);

    // 版本与当前指针一个都没动。
    let conn = Connection::open(&fixture.path).unwrap();
    let version_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM diary_versions WHERE id IN (?1, ?2)",
            params![fixture.ref1, fixture.ref2],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(version_count, 2);
    let current: String = conn
        .query_row(
            "SELECT current_version_id FROM diary_days WHERE day_key = ?1",
            params![day],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(current, fixture.ref2);

    // 同一个 operationId 换选择就是换了内容：报幂等冲突，不静默按旧结果返回。
    let error = core
        .purge_capture(&a, &preview.preview_token, true, "op-purge-keep")
        .unwrap_err();
    assert_eq!(error.code(), ErrorCode::IdempotencyConflict);
}

// ------------------------------------------------------------------ #155 复审 P1-4

fn capture_scope_counts(path: &Path, captures: &[String]) -> Vec<(&'static str, i64)> {
    let conn = Connection::open(path).unwrap();
    let ids = serde_json::to_string(captures).unwrap();
    let queries: [(&'static str, &'static str); 9] = [
        (
            "captures",
            "SELECT COUNT(*) FROM captures WHERE id IN (SELECT value FROM json_each(?1))",
        ),
        (
            "source_items",
            "SELECT COUNT(*) FROM source_items \
             WHERE capture_id IN (SELECT value FROM json_each(?1))",
        ),
        (
            "source_revisions",
            "SELECT COUNT(*) FROM source_revisions WHERE source_id IN \
             (SELECT source_id FROM source_items \
              WHERE capture_id IN (SELECT value FROM json_each(?1)))",
        ),
        (
            "assets",
            "SELECT COUNT(*) FROM assets WHERE id IN \
             (SELECT sr.asset_id FROM source_revisions sr JOIN source_items si \
              ON si.source_id = sr.source_id \
              WHERE si.capture_id IN (SELECT value FROM json_each(?1)))",
        ),
        (
            "extracted_contents",
            "SELECT COUNT(*) FROM extracted_contents WHERE source_id IN \
             (SELECT source_id FROM source_items \
              WHERE capture_id IN (SELECT value FROM json_each(?1)))",
        ),
        (
            "extracted_segments",
            "SELECT COUNT(*) FROM extracted_segments WHERE content_id IN \
             (SELECT id FROM extracted_contents WHERE source_id IN \
              (SELECT source_id FROM source_items \
               WHERE capture_id IN (SELECT value FROM json_each(?1))))",
        ),
        (
            "chunk_spans",
            "SELECT COUNT(*) FROM chunk_spans \
             WHERE capture_id IN (SELECT value FROM json_each(?1))",
        ),
        (
            "text_chunks",
            "SELECT COUNT(*) FROM text_chunks WHERE chunk_id IN \
             (SELECT chunk_id FROM chunk_spans \
              WHERE capture_id IN (SELECT value FROM json_each(?1)))",
        ),
        (
            "chunk_vectors",
            "SELECT COUNT(*) FROM chunk_vectors WHERE chunk_id IN \
             (SELECT chunk_id FROM chunk_spans \
              WHERE capture_id IN (SELECT value FROM json_each(?1)))",
        ),
    ];
    queries
        .iter()
        .map(|(table, sql)| {
            let count: i64 = conn.query_row(sql, params![&ids], |row| row.get(0)).unwrap();
            (*table, count)
        })
        .collect()
}

#[test]
fn purging_one_capture_leaves_the_other_captures_rows_and_files_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.sqlite");
    let mut core = new_core(dir.path());

    // A 与 B 同一天、共享一份原件；C 在另一天。A 的正文与导入都超过 400 字，
    // 各自成为独立的块；B/C 的短篇块不会因为 A 被清理而改变块 id，因此向量也保留。
    let a_text = "樟木箱".repeat(150);
    let a_unique = "A 的独有发票。".repeat(90);
    let shared = "共享原件：同一张发票。".repeat(40);
    let b_text = "海边的风把帽子吹走了。";
    let b_import = "B 的发票：海边。";
    let c_text = "山顶的雪。";
    let c_import = "C 的原件：山顶的雪。";

    let a = commit_text(&mut core, 20, &a_text, "op-a");
    let b = commit_text(&mut core, 20, b_text, "op-b");
    let c = commit_text(&mut core, 21, c_text, "op-c");
    let (_, source_a_unique) = import_text(&mut core, &a, "A独有.txt", &a_unique, "op-a-unique");
    let (_, source_a_shared) = import_text(&mut core, &a, "共享A.txt", &shared, "op-a-shared");
    let (_, source_b_shared) = import_text(&mut core, &b, "共享B.txt", &shared, "op-b-shared");
    let (_, source_b_import) = import_text(&mut core, &b, "B.txt", b_import, "op-b-import");
    let (_, source_c) = import_text(&mut core, &c, "C.txt", c_import, "op-c-import");
    extract_all(&mut core, &a);
    extract_all(&mut core, &b);
    extract_all(&mut core, &c);
    for source in [
        &source_a_unique,
        &source_a_shared,
        &source_b_shared,
        &source_b_import,
        &source_c,
    ] {
        core.extract_source(source).unwrap();
    }
    core.build_semantic_index(None).unwrap();

    let scoped = vec![b.clone(), c.clone()];
    let before = capture_scope_counts(&path, &scoped);
    assert!(
        before.iter().all(|(table, count)| {
            // 块与向量可能因打包方式为空？不：B/C 都应有块与向量。
            *count > 0 || *table == "chunk_vectors"
        }),
        "前提：B/C 各表都有行：{before:?}"
    );
    assert!(
        before
            .iter()
            .find(|(table, _)| *table == "chunk_vectors")
            .unwrap()
            .1
            > 0,
        "前提：B/C 的块已经有向量"
    );
    let blobs_before: i64 = Connection::open(&path)
        .unwrap()
        .query_row("SELECT COUNT(*) FROM blobs", [], |row| row.get(0))
        .unwrap();
    assert_eq!(blobs_before, 4, "共享 + A 独有 + B 独有 + C 独有");

    let shared_path = dir
        .path()
        .join(format!("blobs/{}/{}", &sha_of(shared.as_bytes())[..2], sha_of(shared.as_bytes())));
    let a_unique_sha = sha_of(a_unique.as_bytes());
    let a_unique_path = dir
        .path()
        .join(format!("blobs/{}/{}", &a_unique_sha[..2], a_unique_sha));
    let c_sha = sha_of(c_import.as_bytes());
    let c_path = dir.path().join(format!("blobs/{}/{}", &c_sha[..2], c_sha));
    assert!(shared_path.is_file());
    assert!(a_unique_path.is_file());
    assert!(c_path.is_file());

    // 清掉 A（A 自己的日记版本一条都没有，这里只验「不删多」）。
    let capture = core.get_capture(&a).unwrap();
    core.trash_capture(&a, capture.revision, "op-trash-a")
        .unwrap();
    let preview = core.preview_purge(&a).unwrap();
    core.purge_capture(&a, &preview.preview_token, true, "op-purge-a")
        .unwrap();

    let after = capture_scope_counts(&path, &scoped);
    assert_eq!(after, before, "purge A 之后 B/C 的逐表行数一行都不能少");

    // B/C 的磁盘文件、共享 blob 都还在；A 独有 blob 行与文件才没了。
    assert!(shared_path.is_file(), "共享 blob 文件不能删");
    assert!(c_path.is_file(), "另一天的原件文件不能删");
    assert!(!a_unique_path.exists(), "A 独有的 blob 文件要删掉");
    let conn = Connection::open(&path).unwrap();
    let blob_count_after: i64 = conn
        .query_row("SELECT COUNT(*) FROM blobs", [], |row| row.get(0))
        .unwrap();
    assert_eq!(blob_count_after, 3, "只删不再被引用的 A 独有 blob");
    let shared_rows: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM blobs WHERE sha256 = ?1",
            params![sha_of(shared.as_bytes())],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(shared_rows, 1);

    // B/C 仍可检索，A 的文字从块正文里消失。
    assert_eq!(keyword_count(&mut core, "樟木箱", None, false), 0);
    assert!(keyword_count(&mut core, "海边的风", None, false) >= 1);
    assert!(keyword_count(&mut core, "山顶的雪", None, false) >= 1);
}

#[test]
fn purge_refuses_while_an_asset_lease_is_open() {
    let mut fixture = fixture();
    let core = &mut fixture.core;
    let a = fixture.capture_a.clone();
    let capture = core.get_capture(&a).unwrap();
    core.trash_capture(&a, capture.revision, "op-trash")
        .unwrap();
    let preview = core.preview_purge(&a).unwrap();
    let lease = core
        .open_asset(&fixture.asset_a, "preview")
        .expect("打开预览租约");

    let error = core
        .purge_capture(&a, &preview.preview_token, true, "op-purge-lease")
        .unwrap_err();
    assert_eq!(error.code(), ErrorCode::InvalidState);
    assert!(error.to_string().contains("原件正在"), "{error}");

    // 释放租约后同一个预览令牌仍然有效。
    core.release_asset(&lease.lease_id).unwrap();
    core.purge_capture(&a, &preview.preview_token, true, "op-purge-lease")
        .unwrap();
}

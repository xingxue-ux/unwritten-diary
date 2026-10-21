//! B4c（issue #87）：正文编辑、指定当前版本与合并。
//!
//! 覆盖：`diary.saveEdit`（新 user_edit 版本成为当前、旧版退出默认视图、修订链正确、
//! 基版一字未改）、`diary.setCurrent`（同一天来回切、只认自己的版本）、`diary.merge`
//! （新 merge 版本、来源可追溯、来源版本全部保留）、`expectedRevision` 冲突零改动、
//! 三个方法的幂等与冲突、AI 版本不被顶掉、空正文与超长正文。
//!
//! **全部不联网**：出网一律走 `ScriptedTransport`（回放 + 记账）。
//!
//! 可见性口径（与 B4b 一致，见 `docs/architecture/m3-日记生成与版本.md`）：
//! - **默认视图**是这一天的当前版本（`diary.getDay.currentVersionId`）；旧版本退成候选
//!   （`candidateVersionIds`），正文要**按需按 id 取**（`diary.getVersion` / `listVersions`）；
//! - **检索**里日记版本（含当前版）本来就没有进索引：默认看不到，带
//!   `includeOldDiaryVersions` 时核心**如实警告**并不返回假命中——这一片沿用 B4b 的
//!   诚实退化，不假装过滤生效。所以「旧正文退出默认检索」在日记这一路是恒真的。

use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;

use diary_core::{
    Core, CreateDraftInput, DiaryGenerationReason, DiaryVersionOrigin, ErrorCode, HttpResponse,
    Provider, ProviderCapability, ProviderTestStatus, ScriptedTransport, SearchFilters, SearchMode,
    SearchRequest, Transport, MERGE_PROMPT_VERSION,
};
use rusqlite::Connection;
use sha2::{Digest, Sha256};

// ------------------------------------------------------------------ 工具

fn new_core(dir: &Path) -> Core {
    let mut core = Core::open(dir.join("library.sqlite")).unwrap();
    core.set_transport_for_test(Arc::new(diary_core::PanicTransport::new()) as Arc<dyn Transport>);
    core
}

fn core_with_transport(dir: &Path, script: Vec<HttpResponse>) -> (Core, Arc<ScriptedTransport>) {
    let transport = Arc::new(ScriptedTransport::ok(script));
    let mut core = new_core(dir);
    core.set_transport_for_test(transport.clone() as Arc<dyn Transport>);
    (core, transport)
}

fn provider_config(id: &str) -> Provider {
    Provider {
        id: id.to_owned(),
        label: "测试服务".to_owned(),
        adapter_id: diary_core::OPENAI_COMPATIBLE.to_owned(),
        endpoint: Some("http://127.0.0.1:11434/v1".to_owned()),
        model: Some("qwen-test".to_owned()),
        capabilities: [ProviderCapability::GenerateText]
            .into_iter()
            .collect::<HashSet<_>>(),
        credential_ref: None,
        enabled: true,
        test_status: ProviderTestStatus::Untested,
    }
}

fn save_provider(core: &mut Core) -> String {
    core.save_provider(&provider_config("prv_a"), Some("sk-test-secret-0123456789"))
        .unwrap()
        .id
}

fn chat_ok(text: &str) -> HttpResponse {
    HttpResponse::new(
        200,
        serde_json::json!({
            "model": "qwen-test",
            "choices": [{"message": {"role": "assistant", "content": text}}],
        })
        .to_string(),
    )
}

/// 建一条当天的记录（提交后正文进索引），返回 captureId。
fn capture_with_text(core: &mut Core, text: &str, day: &str, operation_id: &str) -> String {
    let occurred_at = match day {
        "2026-10-16" => "2026-10-16T02:00:00Z",
        "2026-10-17" => "2026-10-17T02:00:00Z",
        other => panic!("测试只准备了这两天：{other}"),
    };
    let capture = core
        .create_draft(CreateDraftInput {
            occurred_at: Some(
                chrono::DateTime::parse_from_rfc3339(occurred_at)
                    .unwrap()
                    .with_timezone(&chrono::Utc),
            ),
            time_zone: "Asia/Shanghai",
            utc_offset_minutes: 480,
            operation_id: &format!("{operation_id}-create"),
        })
        .unwrap();
    assert_eq!(capture.day_key, day);
    core.save_draft(&capture.id, text, capture.revision, operation_id)
        .unwrap();
    core.commit(
        &capture.id,
        capture.revision + 1,
        &format!("{operation_id}-commit"),
    )
    .unwrap();
    capture.id
}

/// 某个记录的第一篇来源 id（改正文时要用）。
fn first_source(core: &Core, capture_id: &str) -> String {
    core.get_capture(capture_id)
        .unwrap()
        .ordered_source_ids
        .first()
        .cloned()
        .expect("提交过的记录一定有来源")
}

fn drive(core: &mut Core) {
    core.run_due_jobs(chrono::Utc::now(), 30_000, "test", 16)
        .unwrap();
}

fn day_of(core: &Core, day: &str) -> diary_core::DiaryDay {
    core.get_diary_day(day).unwrap()
}

fn versions(core: &Core, day: &str) -> Vec<diary_core::DiaryVersion> {
    core.list_diary_versions(day, None, 50).unwrap().versions
}

/// 这一天的 `revision`：**持久单调的状态推进计数**（`DiaryDay.revision`）。
///
/// 调用方从 `diary.getDay` 读它、原样回传给 `expectedRevision`（契约 2.5 / 4.3）。
/// 这里不再从版本数推算：**换当前版本也推进它**，而 #101 之前那条「切换不改变
/// revision」的口径正是假的冲突保护。
fn revision_of(day: &diary_core::DiaryDay) -> i64 {
    day.revision
}

/// 某个事件类型的 `revision` 序列（按 sequence 升序）：用来钉事件带的是哪一步的值。
fn event_revisions(path: &Path, event_type: &str) -> Vec<i64> {
    let conn = Connection::open(path).unwrap();
    let mut statement = conn
        .prepare("SELECT revision FROM domain_events WHERE type = ?1 ORDER BY sequence")
        .unwrap();
    let revisions = statement
        .query_map(rusqlite::params![event_type], |row| row.get::<_, i64>(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    revisions
}

fn keyword_search(core: &mut Core, query: &str) -> diary_core::SearchSnapshot {
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

/// 一条日记版本行的**整行指纹**：任何一个字节被改动都会变。
fn version_row(path: &Path, version_id: &str) -> String {
    Connection::open(path)
        .unwrap()
        .query_row(
            "SELECT id || '|' || day_key || '|' || COALESCE(parent_version_id, '') || '|' || \
             origin || '|' || body || '|' || COALESCE(source_snapshot_id, '') || '|' || \
             COALESCE(style_snapshot_id, '') || '|' || COALESCE(generation_info, '') || '|' || \
             provenance_links || '|' || COALESCE(operation_id, '') || '|' || created_at \
             FROM diary_versions WHERE id = ?1",
            rusqlite::params![version_id],
            |row| row.get(0),
        )
        .unwrap()
}

/// 与日记有关的那几张表的整体指纹：用来断言「失败调用零改动」。
///
/// 含版本（连正文）、天、回执与事件：任何一行被插入/改动/删除都会变。
fn diary_fingerprint(path: &Path) -> String {
    let conn = Connection::open(path).unwrap();
    let mut hasher = Sha256::new();
    let queries = [
        "SELECT id, day_key, COALESCE(parent_version_id, ''), origin, body, \
         COALESCE(source_snapshot_id, ''), COALESCE(style_snapshot_id, ''), \
         COALESCE(generation_info, ''), provenance_links, COALESCE(operation_id, ''), created_at \
         FROM diary_versions ORDER BY id",
        "SELECT day_key, time_zone, COALESCE(current_version_id, ''), updated_at, revision \
         FROM diary_days ORDER BY day_key",
        "SELECT operation_id, kind, request_fingerprint, result_json, created_at \
         FROM operation_receipts ORDER BY operation_id",
        "SELECT sequence, event_id, type, entity_id, revision, emitted_at \
         FROM domain_events ORDER BY sequence",
    ];
    for query in queries {
        let mut statement = conn.prepare(query).unwrap();
        let rows: Vec<String> = statement
            .query_map([], |row| {
                let mut line = String::new();
                for index in 0..row.as_ref().column_count() {
                    // 列类型不固定（TEXT / INTEGER），所以按值取，不按 String 取。
                    let text = match row.get_ref(index)? {
                        rusqlite::types::ValueRef::Null => "<null>".to_owned(),
                        rusqlite::types::ValueRef::Integer(value) => value.to_string(),
                        rusqlite::types::ValueRef::Real(value) => value.to_string(),
                        rusqlite::types::ValueRef::Text(bytes) => {
                            String::from_utf8_lossy(bytes).into_owned()
                        }
                        rusqlite::types::ValueRef::Blob(bytes) => format!("<blob:{}>", bytes.len()),
                    };
                    line.push_str(&text);
                    line.push('\u{1f}');
                }
                line.push('\n');
                Ok(line)
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        for row in rows {
            hasher.update(row.as_bytes());
        }
    }
    format!("{:x}", hasher.finalize())
}

fn event_count(path: &Path, event_type: &str) -> i64 {
    Connection::open(path)
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM domain_events WHERE type = ?1",
            rusqlite::params![event_type],
            |row| row.get(0),
        )
        .unwrap()
}

/// 生成第一版并让它成为当前：这是「编辑/合并」的基版来源。
fn generated_base(core: &mut Core, body: &str, op: &str) -> diary_core::DiaryVersion {
    core.generate_diary("2026-10-16", None, DiaryGenerationReason::Manual, op)
        .unwrap();
    drive(core);
    let all = versions(core, "2026-10-16");
    assert_eq!(all.len(), 1, "第一版应当是唯一的版本");
    let base = all[0].clone();
    assert_eq!(base.origin, DiaryVersionOrigin::Generated);
    assert_eq!(base.body, body);
    assert_eq!(
        day_of(core, "2026-10-16").current_version_id.as_deref(),
        Some(base.id.as_str()),
        "首次生成成为当前"
    );
    base
}

// ------------------------------------------------------ 编辑：新版本成为当前

#[test]
fn edit_writes_a_new_current_version_and_leaves_the_base_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.sqlite");
    let (mut core, _transport) =
        core_with_transport(dir.path(), vec![chat_ok("AI 写的第一版：今天去公园散步。")]);
    save_provider(&mut core);
    capture_with_text(&mut core, "今天去公园散步。", "2026-10-16", "op-a");
    let base = generated_base(&mut core, "AI 写的第一版：今天去公园散步。", "op-gen-1");
    assert_eq!(revision_of(&day_of(&core, "2026-10-16")), 1, "一版就是 revision 1");

    let base_row_before = version_row(&path, &base.id);
    let edited = core
        .save_diary_edit(
            &base.id,
            "我自己改的：今天去公园，风很大，走了两小时。",
            1,
            "op-edit-1",
        )
        .unwrap();

    // 新版本：origin=user_edit、修订链指向基版、operationId 记在版本行上。
    assert_eq!(edited.origin, DiaryVersionOrigin::UserEdit);
    assert_eq!(edited.parent_version_id.as_deref(), Some(base.id.as_str()));
    assert_eq!(edited.day_key, base.day_key);
    assert_eq!(edited.body, "我自己改的：今天去公园，风很大，走了两小时。");
    assert!(edited.source_snapshot_id.is_none(), "手写版本没有生成快照");
    assert!(edited.generation_info.is_none(), "手写版本不是生成");
    let stored_op: Option<String> = Connection::open(&path)
        .unwrap()
        .query_row(
            "SELECT operation_id FROM diary_versions WHERE id = ?1",
            rusqlite::params![edited.id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(stored_op.as_deref(), Some("op-edit-1"));

    // 新版本成为当前；旧版退成候选（默认视图里看到的是新正文）。
    let day = day_of(&core, "2026-10-16");
    assert_eq!(
        day.current_version_id.as_deref(),
        Some(edited.id.as_str()),
        "用户手写编辑就是「我要用这一版」"
    );
    assert_eq!(day.candidate_version_ids, vec![base.id.clone()]);
    assert_eq!(revision_of(&day), 2, "新增一版让 revision 单调 +1");
    let current_body = core
        .get_diary_version(day.current_version_id.as_deref().unwrap())
        .unwrap()
        .body;
    assert_eq!(current_body, edited.body);

    // 基版整行一字未改（含正文、快照引用、来源关联、时间戳）。
    assert_eq!(
        version_row(&path, &base.id),
        base_row_before,
        "编辑绝不原地改基版"
    );
    assert_eq!(core.get_diary_version(&base.id).unwrap().body, base.body);

    // 旧版仍可按需取回：listVersions 最新在前、getVersion 拿到原文。
    let page = versions(&core, "2026-10-16");
    assert_eq!(page.len(), 2);
    // 不断言列表的先后：`listVersions` 按 created_at（墙钟）排，而本机（WSL）的墙钟
    // 会跳。这里钉的是「两版都还在、都能取回」，顺序另有契约口径（最新在前）。
    let mut ids: Vec<String> = page
        .iter()
        .map(|version| version.id.clone())
        .collect();
    ids.sort();
    let mut expected_ids = vec![base.id.clone(), edited.id.clone()];
    expected_ids.sort();
    assert_eq!(ids, expected_ids);
    assert_eq!(core.get_diary_version(&base.id).unwrap().body, base.body);
    assert_eq!(
        core.get_diary_version(&base.id).unwrap().origin,
        DiaryVersionOrigin::Generated,
        "AI 的那一版还在，来源标记也没变"
    );

    // 事件：新版本发了 diary.versionCreated，切当前发了 diary.currentChanged。
    assert_eq!(event_count(&path, "diary.versionCreated"), 2);
    assert_eq!(event_count(&path, "diary.currentChanged"), 2);
}

#[test]
fn edited_body_leaves_the_default_view_while_old_versions_stay_readable() {
    let dir = tempfile::tempdir().unwrap();
    let (mut core, _transport) = core_with_transport(
        dir.path(),
        vec![chat_ok("旧正文：哨兵甲在昨天的日记里出现了。")],
    );
    save_provider(&mut core);
    capture_with_text(&mut core, "今天去公园散步。", "2026-10-16", "op-a");
    let base = generated_base(&mut core, "旧正文：哨兵甲在昨天的日记里出现了。", "op-gen-1");

    core.save_diary_edit(&base.id, "新正文：哨兵乙是改过之后才有的。", 1, "op-edit-1")
        .unwrap();

    // 默认视图 = 这一天的当前版本：看到的是新正文。
    let day = day_of(&core, "2026-10-16");
    let current = core
        .get_diary_version(day.current_version_id.as_deref().unwrap())
        .unwrap();
    assert_eq!(current.body, "新正文：哨兵乙是改过之后才有的。");
    assert!(
        !current.body.contains("哨兵甲"),
        "旧正文不在这一天的当前视图里"
    );
    assert_eq!(day.candidate_version_ids, vec![base.id.clone()]);

    // 按需取回：旧版正文一字不差地还在（「看原版」这条路的落点）。
    let old = core.get_diary_version(&base.id).unwrap();
    assert_eq!(old.body, "旧正文：哨兵甲在昨天的日记里出现了。");
    assert_eq!(versions(&core, "2026-10-16").len(), 2);

    // 检索这一路：日记版本（当前版与旧版）都不在索引里。
    assert!(
        keyword_search(&mut core, "哨兵甲").results.is_empty(),
        "旧正文不在默认检索里"
    );
    assert!(
        keyword_search(&mut core, "哨兵乙").results.is_empty(),
        "新正文也不在检索里——日记版本没有进索引，不能让开关看起来「过滤生效」了"
    );

    // 带开关：如实说「还没有进索引」，不返回假命中。
    let snapshot = core
        .start_search(
            SearchRequest {
                query: "哨兵甲".to_owned(),
                mode: SearchMode::Keyword,
                filters: SearchFilters {
                    include_old_diary_versions: true,
                    ..SearchFilters::default()
                },
                page_size: 20,
            },
            1,
        )
        .unwrap();
    assert!(snapshot.results.is_empty(), "不返回假命中");
    assert!(
        snapshot
            .warnings
            .iter()
            .any(|warning| warning.contains("日记历史版本")),
        "如实说明这一路还没有接：{:?}",
        snapshot.warnings
    );
    assert!(
        snapshot
            .warnings
            .iter()
            .any(|warning| warning.contains("diary.getVersion")),
        "旧版正文的取回路径要写在警告里（带开关不是「能搜到」，而是按需按 id 读）：{:?}",
        snapshot.warnings
    );

    // 用户自己的材料没有被这次编辑动过。
    let capture_text: String = Connection::open(dir.path().join("library.sqlite"))
        .unwrap()
        .query_row(
            "SELECT draft_text FROM captures WHERE day_key = '2026-10-16'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(capture_text, "今天去公园散步。");
}

#[test]
fn edit_chain_is_monotonic_and_can_start_from_an_older_version() {
    let dir = tempfile::tempdir().unwrap();
    let (mut core, _transport) = core_with_transport(dir.path(), vec![chat_ok("第一版。")]);
    save_provider(&mut core);
    capture_with_text(&mut core, "今天的记录。", "2026-10-16", "op-a");
    let v1 = generated_base(&mut core, "第一版。", "op-gen-1");

    let v2 = core.save_diary_edit(&v1.id, "第二版。", 1, "op-e2").unwrap();
    assert_eq!(v2.parent_version_id.as_deref(), Some(v1.id.as_str()));
    let v3 = core.save_diary_edit(&v2.id, "第三版。", 2, "op-e3").unwrap();
    assert_eq!(v3.parent_version_id.as_deref(), Some(v2.id.as_str()));
    assert_eq!(revision_of(&day_of(&core, "2026-10-16")), 3);

    // 退回第一版：旧当前版变候选、不删除，正文都还在。
    let day = core
        .set_diary_current("2026-10-16", &v1.id, 3)
        .unwrap();
    assert_eq!(day.current_version_id.as_deref(), Some(v1.id.as_str()));
    assert_eq!(day.revision, 4, "换当前版本本身就是一次状态推进");
    let mut candidates = day.candidate_version_ids.clone();
    candidates.sort();
    let mut expected = vec![v2.id.clone(), v3.id.clone()];
    expected.sort();
    assert_eq!(candidates, expected);

    // 从旧版（第一版）再改一次：链仍然指向它，revision 继续单调 +1。
    let v4 = core
        .save_diary_edit(&v1.id, "从第一版改出来的第四版。", 4, "op-e4")
        .unwrap();
    assert_eq!(v4.parent_version_id.as_deref(), Some(v1.id.as_str()));
    let day = day_of(&core, "2026-10-16");
    assert_eq!(day.current_version_id.as_deref(), Some(v4.id.as_str()));
    assert_eq!(revision_of(&day), 5);
    for version in versions(&core, "2026-10-16") {
        match version.id.as_str() {
            id if id == v1.id => assert_eq!(version.body, "第一版。"),
            id if id == v2.id => assert_eq!(version.body, "第二版。"),
            id if id == v3.id => assert_eq!(version.body, "第三版。"),
            id if id == v4.id => assert_eq!(version.body, "从第一版改出来的第四版。"),
            other => panic!("多出来一版：{other}"),
        }
    }
}

// ------------------------------------------------------ expectedRevision 冲突

#[test]
fn stale_expected_revision_is_refused_with_zero_changes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.sqlite");
    let (mut core, _transport) = core_with_transport(dir.path(), vec![chat_ok("第一版。")]);
    save_provider(&mut core);
    capture_with_text(&mut core, "今天的记录。", "2026-10-16", "op-a");
    let v1 = generated_base(&mut core, "第一版。", "op-gen-1");
    let v2 = core.save_diary_edit(&v1.id, "第二版。", 1, "op-e2").unwrap();
    assert_eq!(revision_of(&day_of(&core, "2026-10-16")), 2);

    let before = diary_fingerprint(&path);

    let error = core
        .save_diary_edit(&v2.id, "拿着过期的 revision 来改。", 1, "op-stale-edit")
        .unwrap_err();
    assert_eq!(error.code(), ErrorCode::RevisionConflict);
    assert!(
        error.to_string().contains("2026-10-16"),
        "冲突消息里要写清是哪一天：{error}"
    );

    let error = core
        .set_diary_current("2026-10-16", &v1.id, 99)
        .unwrap_err();
    assert_eq!(error.code(), ErrorCode::RevisionConflict);

    let error = core
        .merge_diary_versions(&v2.id, std::slice::from_ref(&v1.id), "合并。", 1, "op-stale-merge")
        .unwrap_err();
    assert_eq!(error.code(), ErrorCode::RevisionConflict);

    assert_eq!(
        diary_fingerprint(&path),
        before,
        "被拒的调用不许留下任何痕迹（版本、当前、回执、事件都不许变）"
    );

    // 被拒的 operationId 没有被占用：拿着正确的 revision 重试就能成功。
    let retried = core
        .save_diary_edit(&v2.id, "拿着过期的 revision 来改。", 2, "op-stale-edit")
        .unwrap();
    assert_eq!(retried.origin, DiaryVersionOrigin::UserEdit);
    assert_eq!(revision_of(&day_of(&core, "2026-10-16")), 3);
}

// ------------------------------------------------------ 指定当前版本

#[test]
fn set_current_switches_back_and_forth_with_an_honest_day_view() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.sqlite");
    let (mut core, _transport) = core_with_transport(dir.path(), vec![chat_ok("AI 的那一版。")]);
    save_provider(&mut core);
    capture_with_text(&mut core, "今天的记录。", "2026-10-16", "op-a");
    let generated = generated_base(&mut core, "AI 的那一版。", "op-gen-1");
    let mine = core
        .save_diary_edit(&generated.id, "我自己的那一版。", 1, "op-edit-1")
        .unwrap();

    let changed_before = event_count(&path, "diary.currentChanged");

    // 切到 AI 候选：用户版本退成候选，两版都还在。
    let day = core
        .set_diary_current("2026-10-16", &generated.id, 2)
        .unwrap();
    assert_eq!(day.current_version_id.as_deref(), Some(generated.id.as_str()));
    assert_eq!(day.candidate_version_ids, vec![mine.id.clone()]);
    assert_eq!(day.revision, 3, "换当前版本推进 revision");
    assert_eq!(
        core.get_diary_version(&generated.id).unwrap().body,
        "AI 的那一版。"
    );

    // 切回用户版本。
    let day = core.set_diary_current("2026-10-16", &mine.id, 3).unwrap();
    assert_eq!(day.current_version_id.as_deref(), Some(mine.id.as_str()));
    assert_eq!(day.candidate_version_ids, vec![generated.id.clone()]);
    assert_eq!(day.revision, 4);

    // 再切一次到已经是当前的版本：无操作（不发事件、不加版本、不推进 revision）。
    let again = core.set_diary_current("2026-10-16", &mine.id, 4).unwrap();
    assert_eq!(again, day, "重复指定同一个当前版本收敛到同一个状态");
    assert_eq!(
        event_count(&path, "diary.currentChanged") - changed_before,
        2,
        "只有两次真的换了当前版本"
    );
    assert_eq!(versions(&core, "2026-10-16").len(), 2, "切换不产生版本");
    assert_eq!(revision_of(&again), 4, "无操作的切换不推进 revision");
    // 事件带的是**推进后**的 revision：生成 1、编辑 2、两次切换 3 / 4。
    assert_eq!(
        event_revisions(&path, "diary.currentChanged"),
        vec![1, 2, 3, 4],
        "diary.currentChanged 的 revision 必须是当天推进后的值"
    );
}

// ------------------------------------------------------ 双窗口并发切换（#101 回归）

/// **真的双窗口语义**：两个窗口（同一个库文件上的两个 `Core`）读到同一个
/// `expectedRevision`，先切的那个成功，后切的那个必须报 `revision_conflict` ——
/// 而不是「两个都成功、后一个静默覆盖前一个的选择」。
///
/// 这条测试在修复前是红的：`revision` 那时是「版本数」，`setCurrent` 不推进它，
/// 于是第二个窗口拿着同一个 revision 照样能切成功，把第一个窗口的选择顶掉。
#[test]
fn two_windows_switching_with_the_same_revision_conflict() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.sqlite");
    let (mut core, _transport) = core_with_transport(dir.path(), vec![chat_ok("AI 的那一版。")]);
    save_provider(&mut core);
    capture_with_text(&mut core, "今天的记录。", "2026-10-16", "op-a");
    let generated = generated_base(&mut core, "AI 的那一版。", "op-gen-1");
    let mine = core
        .save_diary_edit(&generated.id, "我自己的那一版。", 1, "op-edit-1")
        .unwrap();
    drop(core);

    // 两个窗口：同一个库文件上的两个会话，各自读一次这一天，看到同一个 revision。
    let mut window_a = Core::open(&path).unwrap();
    window_a.set_transport_for_test(Arc::new(diary_core::PanicTransport::new()) as Arc<dyn Transport>);
    let mut window_b = Core::open(&path).unwrap();
    window_b.set_transport_for_test(Arc::new(diary_core::PanicTransport::new()) as Arc<dyn Transport>);
    let seen_a = window_a.get_diary_day("2026-10-16").unwrap().revision;
    let seen_b = window_b.get_diary_day("2026-10-16").unwrap().revision;
    assert_eq!((seen_a, seen_b), (2, 2), "两个窗口看到同一个 revision");

    // 窗口 A 先切到自己想要的那一版：成功，并且这一天的 revision 推进了。
    let after_a = window_a
        .set_diary_current("2026-10-16", &generated.id, seen_a)
        .unwrap();
    assert_eq!(
        after_a.current_version_id.as_deref(),
        Some(generated.id.as_str())
    );
    assert_eq!(after_a.revision, seen_a + 1, "指针变更推进 revision");

    let before_b = diary_fingerprint(&path);
    // 窗口 B 拿着**同一个** revision 来切自己想要的版本：必须冲突、零改动。
    let error = window_b
        .set_diary_current("2026-10-16", &mine.id, seen_b)
        .unwrap_err();
    assert_eq!(
        error.code(),
        ErrorCode::RevisionConflict,
        "第二个窗口不许静默覆盖第一个窗口的选择"
    );
    assert_eq!(diary_fingerprint(&path), before_b, "被拒绝的切换零改动");
    assert_eq!(
        window_a.get_diary_day("2026-10-16").unwrap().current_version_id.as_deref(),
        Some(generated.id.as_str()),
        "A 的选择还在"
    );

    // 窗口 B 重新读（拿到新 revision）再切：这一次成功。
    let fresh = window_b.get_diary_day("2026-10-16").unwrap().revision;
    assert_eq!(fresh, seen_a + 1);
    let after_b = window_b
        .set_diary_current("2026-10-16", &mine.id, fresh)
        .unwrap();
    assert_eq!(after_b.current_version_id.as_deref(), Some(mine.id.as_str()));
    assert_eq!(after_b.revision, fresh + 1);
}

#[test]
fn set_current_refuses_foreign_and_missing_versions_with_zero_changes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.sqlite");
    let (mut core, _transport) = core_with_transport(
        dir.path(),
        vec![chat_ok("十六号的第一版。"), chat_ok("十七号的第一版。")],
    );
    save_provider(&mut core);
    capture_with_text(&mut core, "十六号的记录。", "2026-10-16", "op-a");
    capture_with_text(&mut core, "十七号的记录。", "2026-10-17", "op-b");
    core.generate_diary("2026-10-16", None, DiaryGenerationReason::Manual, "op-gen-16")
        .unwrap();
    core.generate_diary("2026-10-17", None, DiaryGenerationReason::Manual, "op-gen-17")
        .unwrap();
    drive(&mut core);
    let foreign = versions(&core, "2026-10-17")[0].clone();
    let mine = versions(&core, "2026-10-16")[0].clone();

    let before = diary_fingerprint(&path);

    let error = core
        .set_diary_current("2026-10-16", &foreign.id, 1)
        .unwrap_err();
    assert_eq!(error.code(), ErrorCode::InvalidArgument);
    assert!(
        error.to_string().contains("2026-10-17"),
        "消息里要写清它属于哪一天：{error}"
    );

    let error = core
        .set_diary_current("2026-10-16", "dver_不存在", 1)
        .unwrap_err();
    assert_eq!(error.code(), ErrorCode::NotFound);

    let error = core
        .set_diary_current("2026-10-16", &mine.id, 99)
        .unwrap_err();
    assert_eq!(error.code(), ErrorCode::RevisionConflict);

    let error = core.set_diary_current("16-10-2026", &mine.id, 1).unwrap_err();
    assert_eq!(error.code(), ErrorCode::InvalidArgument);

    assert_eq!(diary_fingerprint(&path), before, "拒绝就是零改动");
}

// ------------------------------------------------------ 合并

#[test]
fn merge_creates_a_traceable_version_and_keeps_every_source() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.sqlite");
    let (mut core, _transport) = core_with_transport(dir.path(), vec![chat_ok("AI 的第一版。")]);
    save_provider(&mut core);
    capture_with_text(&mut core, "今天的记录。", "2026-10-16", "op-a");
    let v1 = generated_base(&mut core, "AI 的第一版。", "op-gen-1");
    let v2 = core.save_diary_edit(&v1.id, "我改过的第二版。", 1, "op-e2").unwrap();

    let row_v1 = version_row(&path, &v1.id);
    let row_v2 = version_row(&path, &v2.id);

    let merged = core
        .merge_diary_versions(&v2.id, std::slice::from_ref(&v1.id), "合并后的正文。", 2, "op-merge-1")
        .unwrap();

    // 新版本：origin=merge、链指向基版、成为当前。
    assert_eq!(merged.origin, DiaryVersionOrigin::Merge);
    assert_eq!(merged.parent_version_id.as_deref(), Some(v2.id.as_str()));
    assert_eq!(merged.body, "合并后的正文。");
    assert!(merged.source_snapshot_id.is_none());
    let day = day_of(&core, "2026-10-16");
    assert_eq!(day.current_version_id.as_deref(), Some(merged.id.as_str()));
    assert_eq!(revision_of(&day), 3);

    // 来源可追溯：由哪几版合成，写在 generationOptions 里（契约的 DiaryVersion 没有专门字段）。
    let info = merged.generation_info.as_ref().expect("合并版本带审计信息");
    assert_eq!(info.prompt_version, MERGE_PROMPT_VERSION);
    assert_eq!(info.prompt_version, "none", "没有提示词参与，不假装有过生成");
    assert!(info.provider_id.is_none());
    assert!(info.model.is_none());
    assert_eq!(info.generation_options["modelCalls"], "0");
    assert_eq!(info.generation_options["mergedBy"], "user_confirmed");
    assert_eq!(info.generation_options["mergeBaseVersionId"], v2.id);
    assert_eq!(info.generation_options["mergeCandidateVersionIds"], v1.id);
    assert_eq!(
        info.generation_options["mergeSourceVersionIds"],
        format!("{},{}", v2.id, v1.id),
        "基版在前，候选按字典序"
    );

    // 参与合并的版本全部保留、原文一字未改、都能取回。
    assert_eq!(version_row(&path, &v1.id), row_v1);
    assert_eq!(version_row(&path, &v2.id), row_v2);
    assert_eq!(core.get_diary_version(&v1.id).unwrap().body, "AI 的第一版。");
    assert_eq!(
        core.get_diary_version(&v2.id).unwrap().body,
        "我改过的第二版。"
    );
    assert_eq!(versions(&core, "2026-10-16").len(), 3);

    // 多条来源（三版合一）：候选顺序没有语义，规范形按字典序。
    let v4 = core.save_diary_edit(&merged.id, "第四版。", 3, "op-e4").unwrap();
    let merged_two = core
        .merge_diary_versions(
            &v4.id,
            &[v2.id.clone(), v1.id.clone()],
            "三条来源合并出来的正文。",
            4,
            "op-merge-2",
        )
        .unwrap();
    let info = merged_two.generation_info.as_ref().unwrap();
    let mut expected_sources = vec![v1.id.clone(), v2.id.clone()];
    expected_sources.sort();
    let mut actual_sources: Vec<String> = info.generation_options["mergeCandidateVersionIds"]
        .split(',')
        .map(str::to_owned)
        .collect();
    actual_sources.sort();
    assert_eq!(actual_sources, expected_sources);
    assert_eq!(versions(&core, "2026-10-16").len(), 5);
}

#[test]
fn merge_order_has_no_meaning_and_replay_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.sqlite");
    let (mut core, _transport) = core_with_transport(dir.path(), vec![chat_ok("AI 的第一版。")]);
    save_provider(&mut core);
    capture_with_text(&mut core, "今天的记录。", "2026-10-16", "op-a");
    let v1 = generated_base(&mut core, "AI 的第一版。", "op-gen-1");
    let v2 = core.save_diary_edit(&v1.id, "第二版。", 1, "op-e2").unwrap();
    let v3 = core.save_diary_edit(&v2.id, "第三版。", 2, "op-e3").unwrap();

    let first = core
        .merge_diary_versions(
            &v3.id,
            &[v1.id.clone(), v2.id.clone()],
            "合并后的正文。",
            3,
            "op-merge",
        )
        .unwrap();
    // 同一个 operationId + 同一组来源（顺序反过来）→ 命中回执，返回原版本，不产第二版。
    let again = core
        .merge_diary_versions(
            &v3.id,
            &[v2.id.clone(), v1.id.clone()],
            "合并后的正文。",
            3,
            "op-merge",
        )
        .unwrap();
    assert_eq!(again.id, first.id, "来源顺序没有语义");
    assert_eq!(versions(&core, "2026-10-16").len(), 4, "没有第二个合并版本");

    // 换了正文 → 幂等冲突，零改动。
    let before = diary_fingerprint(&path);
    let error = core
        .merge_diary_versions(
            &v3.id,
            &[v1.id.clone(), v2.id.clone()],
            "换了一段正文。",
            3,
            "op-merge",
        )
        .unwrap_err();
    assert_eq!(error.code(), ErrorCode::IdempotencyConflict);
    assert_eq!(diary_fingerprint(&path), before);
}

#[test]
fn merge_refuses_bad_inputs_with_zero_changes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.sqlite");
    let (mut core, _transport) = core_with_transport(
        dir.path(),
        vec![chat_ok("十六号的第一版。"), chat_ok("十七号的第一版。")],
    );
    save_provider(&mut core);
    capture_with_text(&mut core, "十六号的记录。", "2026-10-16", "op-a");
    capture_with_text(&mut core, "十七号的记录。", "2026-10-17", "op-b");
    core.generate_diary("2026-10-16", None, DiaryGenerationReason::Manual, "op-gen-16")
        .unwrap();
    core.generate_diary("2026-10-17", None, DiaryGenerationReason::Manual, "op-gen-17")
        .unwrap();
    drive(&mut core);
    let v16 = versions(&core, "2026-10-16")[0].clone();
    let v17 = versions(&core, "2026-10-17")[0].clone();
    let v16_second = core
        .save_diary_edit(&v16.id, "十六号的第二版。", 1, "op-e16")
        .unwrap();

    let before = diary_fingerprint(&path);
    let cases: Vec<(Vec<String>, &str, String, ErrorCode)> = vec![
        (vec![], "没有候选版本。", "op-m1".to_owned(), ErrorCode::InvalidArgument),
        (
            vec![v16.id.clone(), v16.id.clone()],
            "重复候选。",
            "op-m2".to_owned(),
            ErrorCode::InvalidArgument,
        ),
        (
            vec![v16_second.id.clone()],
            "候选里含基版自己。",
            "op-m3".to_owned(),
            ErrorCode::InvalidArgument,
        ),
        (
            vec![v17.id.clone()],
            "跨天合并。",
            "op-m4".to_owned(),
            ErrorCode::InvalidArgument,
        ),
        (
            vec!["dver_不存在".to_owned()],
            "候选不存在。",
            "op-m5".to_owned(),
            ErrorCode::NotFound,
        ),
        (
            vec![v16.id.clone()],
            "",
            "op-m6".to_owned(),
            ErrorCode::InvalidArgument,
        ),
        (
            vec![v16.id.clone()],
            "没有 operationId。",
            String::new(),
            ErrorCode::InvalidArgument,
        ),
    ];
    for (candidates, body, operation_id, expected_code) in cases {
        let error = core
            .merge_diary_versions(&v16_second.id, &candidates, body, 2, &operation_id)
            .unwrap_err();
        assert_eq!(
            error.code(),
            expected_code,
            "用例：{body:?} / 候选 {candidates:?} → {error}"
        );
    }
    assert_eq!(diary_fingerprint(&path), before, "全部拒绝，全部零改动");
}

// ------------------------------------------------------ 幂等（编辑）

#[test]
fn edit_replay_returns_the_original_version_and_conflicts_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.sqlite");
    let (mut core, _transport) = core_with_transport(dir.path(), vec![chat_ok("AI 的第一版。")]);
    save_provider(&mut core);
    capture_with_text(&mut core, "今天的记录。", "2026-10-16", "op-a");
    let base = generated_base(&mut core, "AI 的第一版。", "op-gen-1");

    let first = core
        .save_diary_edit(&base.id, "编辑后的正文。", 1, "op-edit")
        .unwrap();
    let versions_after_first = versions(&core, "2026-10-16").len();
    assert_eq!(versions_after_first, 2);

    // 重放：**先查回执再查 revision**，所以 revision 已经变成 2 也照样返回原版本。
    let again = core
        .save_diary_edit(&base.id, "编辑后的正文。", 1, "op-edit")
        .unwrap();
    assert_eq!(again, first, "同一条 operationId 返回原结果");
    assert_eq!(versions(&core, "2026-10-16").len(), versions_after_first);
    assert_eq!(event_count(&path, "diary.versionCreated"), 2);

    let before = diary_fingerprint(&path);
    // 换了正文 → 幂等冲突。
    let error = core
        .save_diary_edit(&base.id, "换了一段正文。", 1, "op-edit")
        .unwrap_err();
    assert_eq!(error.code(), ErrorCode::IdempotencyConflict);
    // 换了 expectedRevision → 也算换了内容（指纹含 expectedRevision，与 captures.saveDraft 同口径）。
    let error = core
        .save_diary_edit(&base.id, "编辑后的正文。", 2, "op-edit")
        .unwrap_err();
    assert_eq!(error.code(), ErrorCode::IdempotencyConflict);
    // 换了基版 → 幂等冲突。
    let error = core
        .save_diary_edit(&first.id, "编辑后的正文。", 1, "op-edit")
        .unwrap_err();
    assert_eq!(error.code(), ErrorCode::IdempotencyConflict);
    assert_eq!(diary_fingerprint(&path), before, "幂等冲突零改动");
}

// ------------------------------------------------------ AI 版本不被顶掉

#[test]
fn user_edit_never_displaces_the_ai_version() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.sqlite");
    let (mut core, _transport) = core_with_transport(
        dir.path(),
        vec![chat_ok("AI 的第一版。"), chat_ok("新材料之后的 AI 候选。")],
    );
    save_provider(&mut core);
    let capture_id = capture_with_text(&mut core, "今天去公园。", "2026-10-16", "op-a");
    let source_id = first_source(&core, &capture_id);

    let generated = generated_base(&mut core, "AI 的第一版。", "op-gen-1");
    let mine = core
        .save_diary_edit(&generated.id, "我自己写的那一版。", 1, "op-edit-1")
        .unwrap();

    // 新材料到了，自动整理只能进候选：用户版本与 AI 那一版都不许被顶掉。
    let capture_revision = core.get_capture(&capture_id).unwrap().revision;
    core.revise_text(&source_id, "晚上又去了一次公园。", capture_revision, "op-revise")
        .unwrap();
    core.generate_diary(
        "2026-10-16",
        None,
        DiaryGenerationReason::Automatic,
        "op-gen-2",
    )
    .unwrap();
    drive(&mut core);

    let day = day_of(&core, "2026-10-16");
    assert_eq!(
        day.current_version_id.as_deref(),
        Some(mine.id.as_str()),
        "用户的版本是当前，AI 不许顶掉"
    );
    let all = versions(&core, "2026-10-16");
    assert_eq!(all.len(), 3, "生成的候选 + 用户版本 + AI 的第一版");
    assert_eq!(day.candidate_version_ids.len(), 2);
    assert!(day.candidate_version_ids.contains(&generated.id));
    for version in &all {
        match version.id.as_str() {
            id if id == generated.id => {
                assert_eq!(version.origin, DiaryVersionOrigin::Generated);
                assert_eq!(version.body, "AI 的第一版。");
            }
            id if id == mine.id => {
                assert_eq!(version.origin, DiaryVersionOrigin::UserEdit);
                assert_eq!(version.body, "我自己写的那一版。");
            }
            _ => assert_eq!(version.origin, DiaryVersionOrigin::Generated),
        }
    }
    // AI 正文仍然不在用户材料的存储里（导出用户原件时不会把它当用户的话带走）。
    let leaked: i64 = Connection::open(&path)
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM captures WHERE draft_text LIKE '%AI 的第一版%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(leaked, 0);
}

// ------------------------------------------------------ 空正文 / 超长正文

#[test]
fn empty_bodies_are_refused_and_long_bodies_are_kept_verbatim() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.sqlite");
    let (mut core, _transport) = core_with_transport(dir.path(), vec![chat_ok("AI 的第一版。")]);
    save_provider(&mut core);
    capture_with_text(&mut core, "今天的记录。", "2026-10-16", "op-a");
    let base = generated_base(&mut core, "AI 的第一版。", "op-gen-1");

    // 空 / 纯空白：拒绝，零改动。
    let before = diary_fingerprint(&path);
    for body in ["", "   ", "\n\t  \n"] {
        let error = core
            .save_diary_edit(&base.id, body, 1, &format!("op-empty-{}", body.len()))
            .unwrap_err();
        assert_eq!(error.code(), ErrorCode::InvalidArgument, "空正文不是一次编辑");
    }
    let error = core
        .merge_diary_versions(&base.id, &["dver_x".to_owned()], "  ", 1, "op-empty-merge")
        .unwrap_err();
    assert_eq!(error.code(), ErrorCode::InvalidArgument);
    assert_eq!(diary_fingerprint(&path), before, "空正文不落库");

    // 超长正文：**不设上限**，原样保存（契约没有给 body 定长度上限，核心也不截断用户的话）。
    let long = "长".repeat(50_000);
    let edited = core
        .save_diary_edit(&base.id, &long, 1, "op-long")
        .unwrap();
    assert_eq!(edited.body.chars().count(), 50_000);
    assert_eq!(core.get_diary_version(&edited.id).unwrap().body, long);

    // 前后空白与换行也是用户的字：原样保存，不悄悄 trim。
    let spaced = "  开头有空格，结尾有换行。\n\n";
    let spaced_version = core
        .save_diary_edit(&edited.id, spaced, 2, "op-spaced")
        .unwrap();
    assert_eq!(
        core.get_diary_version(&spaced_version.id).unwrap().body,
        spaced
    );

    // 长正文的合并同样原样保存。
    let long_merge = "合".repeat(20_000);
    let merged = core
        .merge_diary_versions(
            &spaced_version.id,
            std::slice::from_ref(&base.id),
            &long_merge,
            3,
            "op-long-merge",
        )
        .unwrap();
    assert_eq!(merged.body, long_merge);
    assert_eq!(versions(&core, "2026-10-16").len(), 4);
}
// ------------------------------------------------------ schema 迁移（#101；合并后是 v13）

/// 老库（v10 的样子：`diary_days` 没有 `revision` 列、也没有 v11 的两张整理表）
/// 升级到**合并后的最新版本（14）**：**回填的 revision 是「这一天的版本数」**——
/// 正是升级前调用方从 `getDay` 算出来的那个数字（契约 2.5 的老口径），所以升级不会让
/// 任何在飞的 `expectedRevision` 失效。这一次 `setCurrent` 的历史（老口径下不计数）
/// 也确实无法还原，回填不假装它算过。
#[test]
fn v10_library_migrates_to_latest_with_revision_backfilled() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.sqlite");
    let (mut core, _transport) = core_with_transport(dir.path(), vec![chat_ok("第一版。")]);
    save_provider(&mut core);
    capture_with_text(&mut core, "今天的记录。", "2026-10-16", "op-a");
    let v1 = generated_base(&mut core, "第一版。", "op-gen-1");
    let v2 = core.save_diary_edit(&v1.id, "第二版。", 1, "op-e2").unwrap();
    assert_eq!(day_of(&core, "2026-10-16").revision, 2);
    // 新口径：换一次当前版本，revision 走到 3（老口径会停在 2）。
    core.set_diary_current("2026-10-16", &v1.id, 2).unwrap();
    assert_eq!(day_of(&core, "2026-10-16").revision, 3);
    drop(core);

    // 把库退回真正的 v10 样子：去掉 v12 加的列与 v11 的两张整理表，
    // 版本与当前指针都留着。
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "ALTER TABLE diary_days DROP COLUMN revision;
             DROP TABLE diary_model_calls;
             DROP TABLE settings;
             UPDATE schema_migrations SET version = 10;
             PRAGMA user_version = 10;",
        )
        .unwrap();
        assert!(
            conn.query_row("SELECT revision FROM diary_days", [], |row| row
                .get::<_, i64>(0))
                .is_err(),
            "老表结构里确实没有 revision 列"
        );
    }

    let mut core = Core::open(&path).unwrap();
    assert_eq!(core.schema_version().unwrap(), diary_core::SCHEMA_VERSION);
    assert_eq!(
        diary_core::SCHEMA_VERSION,
        18,
        "11（整理表）+ 12（revision 列）合并成 13，B6 插件表抬到 14，\
         #128 复审的自动重建标记 + 口径索引抬到 15；16（回收站列）/ 17（styles 表）\
         与 18（settings 表）三段都由本支一并带上"
    );
    // 两段迁移都跑了：v11 的两张表在，v12 的列在（下面用行为断言 revision）。
    {
        let conn = Connection::open(&path).unwrap();
        let tables: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' \
                 AND name IN ('settings', 'diary_model_calls')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(tables, 2, "v11 的两张表都补上了");
    }

    let day = day_of(&core, "2026-10-16");
    assert_eq!(day.revision, 2, "回填成版本数（不是老口径之外的任何东西）");
    assert_eq!(
        day.current_version_id.as_deref(),
        Some(v1.id.as_str()),
        "迁移不动当前版本"
    );
    assert_eq!(versions(&core, "2026-10-16").len(), 2, "版本一行不少");

    // 迁移后的库接着用：从回填的 2 往上推进，冲突保护照旧。
    let day = core.set_diary_current("2026-10-16", &v2.id, 2).unwrap();
    assert_eq!(day.revision, 3);
    let error = core
        .set_diary_current("2026-10-16", &v1.id, 2)
        .unwrap_err();
    assert_eq!(error.code(), ErrorCode::RevisionConflict);
}

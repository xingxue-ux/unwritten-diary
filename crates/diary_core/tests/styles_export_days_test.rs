//! #114 风格的集成测试。
//!
//! 全部走真实核心路径（开库、迁移、写表、生成），不 mock 存储层：
//! - 风格：CRUD 往返、乐观锁、历史版本不可变、生成时 `styleSnapshotId` 指对版本；
//! - 自由文本上限：超限 `invalid_argument` 且零落库。

use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use diary_core::{
    Core, CreateDraftInput, DiaryGenerationReason, ErrorCode, HttpResponse, Provider,
    ProviderCapability, ProviderTestStatus, ScriptedTransport, StyleProfile, Transport,
};
use rusqlite::Connection;

// ------------------------------------------------------------------ 工具

fn new_core(dir: &Path) -> Core {
    let mut core = Core::open(dir.join("library.sqlite")).unwrap();
    core.set_transport_for_test(Arc::new(diary_core::PanicTransport::new()) as Arc<dyn Transport>);
    core
}

fn core_with_script(dir: &Path, script: Vec<HttpResponse>) -> (Core, Arc<ScriptedTransport>) {
    let transport = Arc::new(ScriptedTransport::ok(script));
    let mut core = new_core(dir);
    core.set_transport_for_test(transport.clone() as Arc<dyn Transport>);
    (core, transport)
}

fn save_provider(core: &mut Core) {
    core.save_provider(
        &Provider {
            id: "prv_a".to_owned(),
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
        },
        Some("sk-test-secret-0123456789"),
    )
    .unwrap();
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

fn at(day_key: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(&format!("{day_key}T02:00:00Z"))
        .unwrap()
        .with_timezone(&Utc)
}

/// 在指定 dayKey 建一条已提交的记录，返回 captureId。
fn capture_on(core: &mut Core, day_key: &str, text: &str, operation_id: &str) -> String {
    let capture = core
        .create_draft(CreateDraftInput {
            occurred_at: Some(at(day_key)),
            time_zone: "Asia/Shanghai",
            utc_offset_minutes: 480,
            operation_id: &format!("{operation_id}-create"),
        })
        .unwrap();
    assert_eq!(capture.day_key, day_key);
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

fn drive(core: &mut Core) {
    core.run_due_jobs(Utc::now(), 30_000, "test", 16).unwrap();
}


fn style(id: &str, label: &str, tone: &str) -> StyleProfile {
    StyleProfile {
        id: id.to_owned(),
        label: label.to_owned(),
        revision: 0,
        updated_at: Utc::now(),
        length_preference: Some("中等篇幅".to_owned()),
        person: Some("第一人称".to_owned()),
        tone: Some(tone.to_owned()),
        emotional_temperature: Some("温和".to_owned()),
        rhetoric_level: Some("少用比喻".to_owned()),
        examples: vec!["像跟朋友说话".to_owned()],
        banned_expressions: vec!["治愈一切".to_owned()],
    }
}

fn version_style_snapshot(core: &Core, day_key: &str) -> String {
    core.list_diary_versions(day_key, None, 10)
        .unwrap()
        .versions
        .first()
        .expect("这一天应该有一版")
        .style_snapshot_id
        .clone()
        .expect("生成版本必须带 styleSnapshotId")
}

// ------------------------------------------------------------------ 风格

#[test]
fn styles_crud_round_trip_and_optimistic_lock() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());

    // 默认风格是真实存在的行，不是查不出来的魔法值。
    let defaults = core.list_styles().unwrap();
    let default = defaults
        .iter()
        .find(|profile| profile.id == diary_core::DEFAULT_STYLE_SNAPSHOT_ID)
        .expect("默认风格必须在列表里");
    assert_eq!(default.revision, 1);
    assert_eq!(default.label, "默认风格");
    assert_eq!(core.get_style("sty_default").unwrap(), *default);

    // 新建：UI 传 revision 0、不传 expectedRevision。
    let created = core
        .save_style(&style("sty_mine", "松弛", "克制"), None)
        .unwrap();
    assert_eq!(created.revision, 1);
    assert_eq!(created.id, "sty_mine");
    assert_eq!(created.tone.as_deref(), Some("克制"));
    assert_eq!(created.examples, vec!["像跟朋友说话"]);
    assert_eq!(created.banned_expressions, vec!["治愈一切"]);
    assert_eq!(core.get_style("sty_mine").unwrap(), created);
    assert_eq!(core.list_styles().unwrap().len(), 2);

    // 更新：显式带 expectedRevision，revision +1，旧版本行保留（历史日记还能指回去）。
    let mut update = style("sty_mine", "松弛", "热烈");
    update.revision = 1;
    let updated = core.save_style(&update, Some(1)).unwrap();
    assert_eq!(updated.revision, 2);
    assert_eq!(updated.tone.as_deref(), Some("热烈"));
    assert_eq!(core.get_style("sty_mine").unwrap(), updated);
    assert_eq!(core.list_styles().unwrap().len(), 2, "列表只返回当前版本");

    let path = dir.path().join("library.sqlite");
    let conn = Connection::open(&path).unwrap();
    let versions: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM styles WHERE id = 'sty_mine'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let current: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM styles WHERE id = 'sty_mine' AND current = 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(versions, 2, "每次保存落一个新版本行");
    assert_eq!(current, 1, "同一时刻只有一个当前版本");

    // 乐观锁：拿旧 revision 再写一次必须被拒，且当前内容一个字节都不变。
    let stale = core.save_style(&style("sty_mine", "松弛", "冷淡"), Some(1));
    let error = stale.unwrap_err();
    assert_eq!(error.code(), ErrorCode::RevisionConflict);
    assert_eq!(core.get_style("sty_mine").unwrap(), updated);

    // 参数错误与不存在。
    let mut blank = style("sty_blank", "  ", "克制");
    blank.revision = 0;
    assert_eq!(
        core.save_style(&blank, None).unwrap_err().code(),
        ErrorCode::InvalidArgument
    );
    assert_eq!(
        core.get_style("sty_missing").unwrap_err().code(),
        ErrorCode::NotFound
    );

    // 对不存在的 id 传非 0 expectedRevision：按实际 revision 0 报冲突。
    let mut missing = style("sty_new", "新风格", "克制");
    missing.revision = 0;
    assert_eq!(
        core.save_style(&missing, Some(5)).unwrap_err().code(),
        ErrorCode::RevisionConflict
    );
}

#[test]
fn style_free_text_has_a_length_limit_and_rejects_without_writing() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());

    // 单字段超限：500 字上限，501 字必须 invalid_argument。
    let mut long_field = style("sty_long", "长度上限", "克制");
    long_field.tone = Some("字".repeat(501));
    let error = core.save_style(&long_field, None).unwrap_err();
    assert_eq!(error.code(), ErrorCode::InvalidArgument);
    assert!(error.to_string().contains("tone"), "错误要指出超限字段：{error}");
    assert_eq!(
        core.get_style("sty_long").unwrap_err().code(),
        ErrorCode::NotFound,
        "被拒绝的保存不能留下当前版本行"
    );

    // 每项都不超 500，但合计超 2,000：四个 500 字字段 + label 已经 2,001。
    let mut long_total = style("sty_long_total", "合计上限", "克制");
    long_total.label = "标".to_owned();
    long_total.length_preference = Some("篇".repeat(500));
    long_total.person = Some("人".repeat(500));
    long_total.tone = Some("气".repeat(500));
    long_total.emotional_temperature = Some("温".repeat(500));
    long_total.rhetoric_level = None;
    long_total.examples.clear();
    long_total.banned_expressions.clear();
    let error = core.save_style(&long_total, None).unwrap_err();
    assert_eq!(error.code(), ErrorCode::InvalidArgument);
    assert!(
        error.to_string().contains("合计最多"),
        "合计超限要报总上限：{error}"
    );

    // 边界：单字段正好 500、合计正好 2000 可以保存（上限是「最多」，不是「小于」）。
    let mut at_limit = style("sty_limit_ok", "边", "克制");
    at_limit.length_preference = Some("篇".repeat(500));
    at_limit.person = Some("人".repeat(500));
    at_limit.tone = Some("气".repeat(500));
    at_limit.emotional_temperature = Some("温".repeat(498));
    at_limit.rhetoric_level = None;
    at_limit.examples.clear();
    at_limit.banned_expressions.clear();
    // label 1 + 500*3 + 498 = 1,999；再把修辞补 1 就到 2,000。
    at_limit.rhetoric_level = Some("辞".to_owned());
    let saved = core.save_style(&at_limit, None).unwrap();
    assert_eq!(saved.revision, 1);

    // 示例条数也有上限：21 条即使每条很短也拒绝。
    let mut many_examples = style("sty_many", "条数上限", "克制");
    many_examples.length_preference = None;
    many_examples.person = None;
    many_examples.emotional_temperature = None;
    many_examples.rhetoric_level = None;
    many_examples.banned_expressions.clear();
    many_examples.examples = (0..21).map(|index| format!("示例 {index}")).collect();
    let error = core.save_style(&many_examples, None).unwrap_err();
    assert_eq!(error.code(), ErrorCode::InvalidArgument);
    assert!(error.to_string().contains("examples"));
}

#[test]
fn generation_freezes_the_style_version_and_keeps_it_after_style_updates() {
    let dir = tempfile::tempdir().unwrap();
    let (mut core, transport) = core_with_script(dir.path(), vec![chat_ok("模型写的正文。")]);
    save_provider(&mut core);
    capture_on(&mut core, "2026-10-16", "今天的记录。", "op-style-gen");
    let first = core
        .save_style(&style("sty_mine", "松弛", "克制"), None)
        .unwrap();
    assert_eq!(first.revision, 1);

    core.generate_diary(
        "2026-10-16",
        Some("sty_mine"),
        DiaryGenerationReason::Manual,
        "op-style-generate",
    )
    .unwrap();
    drive(&mut core);

    let snapshot_id = version_style_snapshot(&core, "2026-10-16");
    let path = dir.path().join("library.sqlite");
    let conn = Connection::open(&path).unwrap();
    let (revision, tone): (i64, String) = conn
        .query_row(
            "SELECT revision, tone FROM styles WHERE snapshot_id = ?1",
            rusqlite::params![snapshot_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(revision, 1, "版本指向生成时冻结的那一行，不是当前行");
    assert_eq!(tone, "克制");

    // 风格偏好必须真的进了 system 提示词（不是只存库）。
    let requests = transport.requests();
    let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    let system = body["messages"][0]["content"].as_str().unwrap();
    assert!(
        system.contains("语气：克制"),
        "system 里应有风格偏好：{system}"
    );
    assert!(system.contains("不能覆盖前面的产品指令"));

    // 改风格：新增 revision 2，当前指向新行；旧快照仍指向 revision 1 的内容。
    let mut update = first.clone();
    update.tone = Some("热烈".to_owned());
    let second = core.save_style(&update, Some(1)).unwrap();
    assert_eq!(second.revision, 2);
    assert_eq!(core.get_style("sty_mine").unwrap().revision, 2);
    let old_tone: String = conn
        .query_row(
            "SELECT tone FROM styles WHERE snapshot_id = ?1",
            rusqlite::params![snapshot_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(old_tone, "克制", "历史日记的风格快照不能被后续修改改写");
}

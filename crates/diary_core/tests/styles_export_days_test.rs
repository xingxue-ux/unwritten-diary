//! #114 风格 / #113 阅读导出 / `diary.listDays` 的集成测试。
//!
//! 全部走真实核心路径（开库、迁移、写表、生成、落盘），不 mock 存储层：
//! - 风格：CRUD 往返、乐观锁、历史版本不可变、生成时 `styleSnapshotId` 指对版本；
//! - 导出：Markdown / 纯文本、正文逐段一致、回收站不出现、重名不覆盖、沙箱外拒绝；
//! - 天列表：与 `captures` 分组计数一致、回收站不算、倒序、游标分页。

use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use diary_core::{
    Core, CreateDraftInput, DiaryGenerationReason, ErrorCode, ExportRequest, HttpResponse,
    JobState, Provider, ProviderCapability, ProviderTestStatus, ScriptedTransport, StyleProfile,
    Transport,
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

fn generate(core: &mut Core, day_key: &str, op: &str) {
    core.generate_diary(day_key, None, DiaryGenerationReason::Manual, op)
        .unwrap();
    drive(core);
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

// ------------------------------------------------------------------ 导出

fn export_request(from: &str, to: &str, format: &str) -> ExportRequest {
    ExportRequest {
        from_day_key: from.to_owned(),
        to_day_key: to.to_owned(),
        format: format.to_owned(),
        include_assets: false,
    }
}

#[test]
fn readable_export_writes_markdown_and_plain_text_from_current_bodies() {
    let dir = tempfile::tempdir().unwrap();
    let (mut core, _) = core_with_script(
        dir.path(),
        vec![
            chat_ok("第一天的第一段。\n\n第一天的第二段。"),
            chat_ok("第二天的正文。"),
        ],
    );
    save_provider(&mut core);
    capture_on(&mut core, "2026-10-14", "十四号的记录。", "op-day-14");
    capture_on(&mut core, "2026-10-15", "十五号的记录。", "op-day-15");
    generate(&mut core, "2026-10-14", "op-gen-14");
    generate(&mut core, "2026-10-15", "op-gen-15");

    // Markdown：正文逐字保留，日期标题按升序。
    let markdown = core
        .create_export(&export_request("2026-10-14", "2026-10-15", "markdown"))
        .unwrap();
    assert_eq!(markdown.format, "markdown");
    assert_eq!(markdown.day_keys, vec!["2026-10-14", "2026-10-15"]);
    assert_eq!(markdown.day_count, 2);
    assert!(!markdown.restorable, "阅读导出不能用于恢复");
    assert_eq!(
        markdown.package_ticket.as_deref(),
        Some(markdown.path.as_str())
    );
    let markdown_bytes = std::fs::read(&markdown.path).unwrap();
    assert_eq!(markdown.byte_size, markdown_bytes.len() as i64);
    let markdown_text = String::from_utf8(markdown_bytes).unwrap();
    assert!(markdown_text.contains("## 2026-10-14"));
    assert!(markdown_text.contains("第一天的第一段。\n\n第一天的第二段。"));
    assert!(markdown_text.contains("## 2026-10-15"));
    assert!(markdown_text.contains("第二天的正文。"));
    assert!(markdown_text.contains("不能用于恢复资料库"));

    // 纯文本：同一天只有一次导出，但格式与日期分隔不同，正文仍逐段一致。
    let plain = core
        .create_export(&export_request("2026-10-15", "2026-10-15", "plain_text"))
        .unwrap();
    assert_eq!(plain.format, "plain_text");
    assert_eq!(plain.day_keys, vec!["2026-10-15"]);
    let plain_text = std::fs::read_to_string(&plain.path).unwrap();
    assert!(plain_text.contains("2026-10-15\n----------\n\n第二天的正文。"));
    assert!(plain_text.contains("不能用于恢复资料库"));
    assert_eq!(plain.byte_size, plain_text.len() as i64);

    // 导出任务是一条**已完成**的记录，jobs.get 读得到（不是排队中的任务）。
    let job = core.get_job(&markdown.job_id).unwrap();
    assert_eq!(job.kind, "export");
    assert_eq!(job.state, JobState::Succeeded);
}

#[test]
fn export_never_overwrites_and_never_includes_trashed_days() {
    let dir = tempfile::tempdir().unwrap();
    let (mut core, _) = core_with_script(
        dir.path(),
        vec![chat_ok("十四号的日记。"), chat_ok("十五号的日记。")],
    );
    save_provider(&mut core);
    capture_on(&mut core, "2026-10-14", "十四号的记录。", "op-14");
    let day15 = capture_on(&mut core, "2026-10-15", "十五号的记录。", "op-15");
    generate(&mut core, "2026-10-14", "op-gen-14");
    generate(&mut core, "2026-10-15", "op-gen-15");

    let first = core
        .create_export(&export_request("2026-10-14", "2026-10-15", "markdown"))
        .unwrap();
    let first_text = std::fs::read_to_string(&first.path).unwrap();
    // 同名再导一次：不覆盖，换后缀，两份文件都在且内容独立。
    let second = core
        .create_export(&export_request("2026-10-14", "2026-10-15", "markdown"))
        .unwrap();
    assert_ne!(first.path, second.path);
    assert!(first.path.ends_with(".md"));
    assert!(second.path.ends_with("-2.md"));
    assert!(Path::new(&first.path).is_file());
    assert!(Path::new(&second.path).is_file());
    assert_eq!(std::fs::read_to_string(&first.path).unwrap(), first_text);

    // 把十五号的记录放进回收站（回收站线的方法还没合进来，测试直接改状态）：
    // 那一天的旧日记版本不能再出现在导出里。
    Connection::open(dir.path().join("library.sqlite"))
        .unwrap()
        .execute(
            "UPDATE captures SET state = 'trashed' WHERE id = ?1",
            rusqlite::params![day15],
        )
        .unwrap();
    let without_trash = core
        .create_export(&export_request("2026-10-14", "2026-10-15", "markdown"))
        .unwrap();
    let text = std::fs::read_to_string(&without_trash.path).unwrap();
    assert_eq!(without_trash.day_keys, vec!["2026-10-14"]);
    assert!(text.contains("十四号的日记。"));
    assert!(!text.contains("十五号的日记。"), "回收站里的日子不能进导出");
    assert!(!text.contains("十五号的记录。"), "草稿/记录正文不能进导出");

    // 整段范围都只剩回收站内容：明确报 not_found，不产空文件。
    assert_eq!(
        core.create_export(&export_request("2026-10-15", "2026-10-15", "markdown"))
            .unwrap_err()
            .code(),
        ErrorCode::NotFound
    );
}

#[test]
fn export_accepts_a_non_zero_padded_start_day_key() {
    let dir = tempfile::tempdir().unwrap();
    let (mut core, _) = core_with_script(
        dir.path(),
        vec![chat_ok("五号正文。"), chat_ok("十四号正文。"), chat_ok("十五号正文。")],
    );
    save_provider(&mut core);
    capture_on(&mut core, "2026-10-05", "五号的记录。", "op-05");
    capture_on(&mut core, "2026-10-14", "十四号的记录。", "op-14");
    capture_on(&mut core, "2026-10-15", "十五号的记录。", "op-15");
    generate(&mut core, "2026-10-05", "op-gen-05");
    generate(&mut core, "2026-10-14", "op-gen-14");
    generate(&mut core, "2026-10-15", "op-gen-15");

    // 非零填充的 `2026-10-5` 能通过日期解析，但字典序上小于 `2026-10-05`。
    // 如果绑定的是原始字符串，这个范围会静默丢掉 10 月 1–19 日。
    let ticket = core
        .create_export(&export_request("2026-10-5", "2026-10-15", "markdown"))
        .expect("非零填充的起点必须按日期解析后绑定，不能静默排除整个范围");
    assert_eq!(
        ticket.day_keys,
        vec!["2026-10-05", "2026-10-14", "2026-10-15"]
    );
    let text = std::fs::read_to_string(&ticket.path).unwrap();
    assert!(text.contains("## 2026-10-05"));
    assert!(text.contains("五号正文。"));
    assert!(text.contains("十四号正文。"));
    assert!(text.contains("十五号正文。"));
}

#[test]
fn export_rejects_bad_arguments_and_paths_outside_the_sandbox() {
    let dir = tempfile::tempdir().unwrap();
    let (mut core, _) = core_with_script(dir.path(), vec![chat_ok("正文。")]);
    save_provider(&mut core);
    capture_on(&mut core, "2026-10-14", "记录。", "op-a");
    generate(&mut core, "2026-10-14", "op-gen");

    // 格式只认契约里的两种。
    assert_eq!(
        core.create_export(&export_request("2026-10-14", "2026-10-14", "pdf"))
            .unwrap_err()
            .code(),
        ErrorCode::InvalidArgument
    );
    // 附原件：本构建明确拒绝，不静默忽略。
    let mut with_assets = export_request("2026-10-14", "2026-10-14", "markdown");
    with_assets.include_assets = true;
    let error = core.create_export(&with_assets).unwrap_err();
    assert_eq!(error.code(), ErrorCode::InvalidArgument);
    assert!(error.to_string().contains("includeAssets"));
    // 起点晚于终点。
    assert_eq!(
        core.create_export(&export_request("2026-10-15", "2026-10-14", "markdown"))
            .unwrap_err()
            .code(),
        ErrorCode::InvalidArgument
    );

    // 沙箱外：宿主上库在临时目录，强制启用 OHOS 判定后必须拒绝，且不创建导出目录。
    core.set_export_sandbox_only_for_test(true);
    let error = core
        .create_export(&export_request("2026-10-14", "2026-10-14", "markdown"))
        .unwrap_err();
    assert_eq!(error.code(), ErrorCode::PermissionDenied);
    assert!(!dir.path().join("exports").exists(), "拒绝时不能先建目录");
}

// ------------------------------------------------------------------ 天列表

#[test]
fn list_days_matches_capture_counts_excludes_trash_and_paginates() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    capture_on(&mut core, "2026-10-05", "五号一条。", "op-05");
    capture_on(&mut core, "2026-10-14", "十四号一条。", "op-14");
    let trash_me = capture_on(&mut core, "2026-10-15", "十五号会被回收的一条。", "op-15a");
    capture_on(&mut core, "2026-10-15", "十五号保留的一条。", "op-15b");
    capture_on(&mut core, "2026-10-16", "十六号一条。", "op-16");
    Connection::open(dir.path().join("library.sqlite"))
        .unwrap()
        .execute(
            "UPDATE captures SET state = 'trashed' WHERE id = ?1",
            rusqlite::params![trash_me],
        )
        .unwrap();

    let page = core.list_days(None, None, None, 20).unwrap();
    assert_eq!(
        page.days
            .iter()
            .map(|day| day.day_key.as_str())
            .collect::<Vec<_>>(),
        vec!["2026-10-16", "2026-10-15", "2026-10-14", "2026-10-05"],
        "倒序、回收站里的记录不计数"
    );
    assert_eq!(page.days[1].material_counts.captures, 1);
    assert_eq!(page.days[0].material_counts.captures, 1);
    assert_eq!(page.next_cursor, None);

    // 与 captures 表按 day 分组的计数逐天一致。
    let conn = Connection::open(dir.path().join("library.sqlite")).unwrap();
    let sql_counts: Vec<(String, i64)> = {
        let mut statement = conn
            .prepare(
                "SELECT day_key, COUNT(*) FROM captures WHERE state != 'trashed' \
                 GROUP BY day_key ORDER BY day_key DESC",
            )
            .unwrap();
        statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
    };
    assert_eq!(sql_counts.len(), page.days.len());
    for (day, (key, count)) in page.days.iter().zip(sql_counts) {
        assert_eq!(day.day_key, key);
        assert_eq!(day.material_counts.captures, count);
    }

    // 分页：pageSize = 1，游标原样回传能翻完，最后没有 nextCursor。
    let first = core.list_days(None, None, None, 1).unwrap();
    assert_eq!(first.days.len(), 1);
    assert_eq!(first.days[0].day_key, "2026-10-16");
    let cursor = first.next_cursor.expect("还有下一页");
    let second = core.list_days(None, None, Some(&cursor), 1).unwrap();
    assert_eq!(second.days[0].day_key, "2026-10-15");
    let third = core
        .list_days(None, None, second.next_cursor.as_deref(), 1)
        .unwrap();
    assert_eq!(third.days[0].day_key, "2026-10-14");
    let fourth = core
        .list_days(None, None, third.next_cursor.as_deref(), 1)
        .unwrap();
    assert_eq!(fourth.days[0].day_key, "2026-10-05");
    assert_eq!(fourth.next_cursor, None);

    // 闭区间过滤。
    let only_15 = core
        .list_days(Some("2026-10-15"), Some("2026-10-15"), None, 20)
        .unwrap();
    assert_eq!(only_15.days.len(), 1);
    assert_eq!(only_15.days[0].day_key, "2026-10-15");

    // 非零填充的 dayKey：解析成功就要按规范形比较/绑定，不能按原始字符串的
    // 字典序把范围悄悄缩小（`2026-10-05` < `2026-10-5`）。
    let padded = core
        .list_days(Some("2026-10-5"), Some("2026-10-15"), None, 20)
        .expect("非零填充的 fromDayKey 必须按日期解析");
    assert_eq!(
        padded
            .days
            .iter()
            .map(|day| day.day_key.as_str())
            .collect::<Vec<_>>(),
        vec!["2026-10-15", "2026-10-14", "2026-10-05"],
        "非零填充的起点不能静默排除 10 月 1–19 日"
    );

    // 单日范围也用非零填充：解析后两边相等，不是 invalid_argument。
    let single = core
        .list_days(Some("2026-10-5"), Some("2026-10-5"), None, 20)
        .unwrap();
    assert_eq!(single.days.len(), 1);
    assert_eq!(single.days[0].day_key, "2026-10-05");

    // 坏参数：游标过期码；from > to 是 invalid_argument。
    assert_eq!(
        core.list_days(None, None, Some("not-a-cursor"), 20)
            .unwrap_err()
            .code(),
        ErrorCode::CursorExpired
    );
    assert_eq!(
        core.list_days(Some("2026-10-16"), Some("2026-10-14"), None, 20)
            .unwrap_err()
            .code(),
        ErrorCode::InvalidArgument
    );
}

/// #168 复审的回归：**键列表与逐天详情必须来自同一个读事务（同一快照）**。
///
/// 双连接并发：读连接在 `list_days` 里先查 `day_key` 列表，再逐天 `get_day`；写连接
/// 在这两步之间把某天最后一条材料挪进回收站。本分支还没有 `trash_capture` 入口
/// （回收站是 #155 那条线），所以这里按上面那条测试的同一口径，用一个真库连接
/// `UPDATE captures SET state = 'trashed'`。
///
/// 未修版本里两步分属两个快照：写连接的提交只要落在中间，键列表里仍留着这一天，
/// 而 `get_day` 会报 `not_found`（这一天既没有材料也没有版本）——**整页失败**，
/// 也就是「键有、详情没」。修好之后键与详情同快照：这一天要么带着那 1 条材料出现，
/// 要么整天空缺，绝不会出现「在键列表里、详情却是空的」。
#[test]
fn list_days_keys_and_details_come_from_one_snapshot_under_concurrent_trash() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    // 目标天：只有一条材料、没有 `diary_days` 行、也没有版本——材料被挪走的那一瞬间，
    // 这一天在 `get_day` 眼里正好是「既没有材料也没有版本」，也就是 not_found 的条件。
    let only = capture_on(&mut core, "2026-10-19", "十九号唯一一条。", "op-19");
    // 搭出上下文天：让这一页不止一天，翻页键也不止一个。
    capture_on(&mut core, "2026-10-18", "十八号一条。", "op-18");
    capture_on(&mut core, "2026-10-17", "十七号一条。", "op-17");

    let path = dir.path().join("library.sqlite");
    let writer = std::thread::spawn({
        let path = path.clone();
        move || {
            let conn = Connection::open(&path).unwrap();
            // 读连接在 WAL 下不挡写，写字这边只可能自己撞上检查点；给足等待时间，
            // 别让测试因为 `database is locked` 而假红。
            conn.busy_timeout(std::time::Duration::from_secs(5)).unwrap();
            let original: String = conn
                .query_row(
                    "SELECT state FROM captures WHERE id = ?1",
                    rusqlite::params![only],
                    |row| row.get(0),
                )
                .unwrap();
            let mut flips = 0_u32;
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            // 一进一出各一次提交：任何一次提交落在「查键之后、取详情之前」的窗口里，
            // 未修版本都会把这一天带进键列表、又在取详情时找不到。
            while std::time::Instant::now() < deadline {
                for state in ["trashed", original.as_str()] {
                    conn.execute(
                        "UPDATE captures SET state = ?1 WHERE id = ?2",
                        rusqlite::params![state, only],
                    )
                    .unwrap();
                    flips += 1;
                }
            }
            flips
        }
    });

    let mut reads = 0_u32;
    let mut saw_target = 0_u32;
    while !writer.is_finished() {
        let page = core
            .list_days(None, None, None, 50)
            .expect("并发写不该让整页失败：键列表与逐天详情必须来自同一快照");
        for day in &page.days {
            let counts = &day.material_counts;
            assert!(
                counts.captures + counts.processed + counts.pending + counts.failed >= 1,
                "{} 出现在键列表里，详情却是空的（键有、详情没）",
                day.day_key
            );
            if day.day_key == "2026-10-19" {
                saw_target += 1;
                assert_eq!(
                    counts.captures, 1,
                    "这一页的键与详情必须来自同一快照：这一天只有那一条材料"
                );
            }
        }
        reads += 1;
    }
    let flips = writer.join().unwrap();
    assert!(
        reads > 0 && flips > 0,
        "读写两侧都要真的跑起来（reads={reads}, flips={flips}）"
    );
    assert!(
        saw_target > 0,
        "目标天至少要带着详情出现过一次，这条测试才不是空转"
    );
}

//! B1a 的存储与记录路径测试。
//!
//! 这些用例对应验收计划里的 E02（重启后原文一致）、E04（未确认的不伪称已保存）、
//! E05（同一 operationId 重复提交）与 E15（跨午夜归属）的后端部分。

use chrono::{TimeZone, Utc};
use diary_core::{CaptureState, Core, CoreError, CreateDraftInput, ErrorCode};

fn new_input<'a>(operation_id: &'a str) -> CreateDraftInput<'a> {
    CreateDraftInput {
        occurred_at: None,
        time_zone: "Asia/Shanghai",
        utc_offset_minutes: 480,
        operation_id,
    }
}

fn core_with_draft() -> (Core, String) {
    let mut core = Core::open_in_memory().expect("打开内存库");
    let capture = core.create_draft(new_input("op-create")).expect("创建草稿");
    (core, capture.id)
}

#[test]
fn schema_version_is_reported() {
    let core = Core::open_in_memory().unwrap();
    assert_eq!(core.schema_version().unwrap(), diary_core::SCHEMA_VERSION);
}

#[test]
fn commit_creates_original_text_revision() {
    let (mut core, id) = core_with_draft();

    let saved = core
        .save_draft(&id, "今天下午面试完，走出大楼的时候风很大。", 1, "op-save")
        .unwrap();
    assert_eq!(saved.revision, 2);
    assert!(saved.durable, "提交后必须报告 durable");

    let committed = core.commit(&id, 2, "op-commit").unwrap();
    assert_eq!(committed.capture.state, CaptureState::Committed);
    assert_eq!(committed.capture.revision, 3);
    let revision = committed
        .original_text_revision
        .expect("提交时应创建原始文字版本");
    assert_eq!(
        revision.text.as_deref(),
        Some("今天下午面试完，走出大楼的时候风很大。")
    );
    assert_eq!(committed.capture.ordered_source_ids, vec![revision.source_id]);
}

#[test]
fn same_operation_id_returns_the_same_result_without_duplicating() {
    let (mut core, id) = core_with_draft();
    core.save_draft(&id, "第一版", 1, "op-save").unwrap();

    let first = core.commit(&id, 2, "op-commit").unwrap();
    // 用同一个 operationId 重复提交：必须返回原结果，且不再推进 revision。
    let second = core.commit(&id, 2, "op-commit").unwrap();

    assert_eq!(first, second);
    let capture = core.get_capture(&id).unwrap();
    assert_eq!(capture.revision, first.capture.revision);
    assert_eq!(
        capture.ordered_source_ids.len(),
        1,
        "重复提交不能制造第二个来源"
    );
}

#[test]
fn same_operation_id_with_different_content_is_a_conflict() {
    let mut core = Core::open_in_memory().unwrap();
    core.create_draft(new_input("op-create")).unwrap();

    let error = core
        .create_draft(CreateDraftInput {
            occurred_at: Some(Utc.with_ymd_and_hms(2026, 9, 27, 9, 0, 0).unwrap()),
            ..new_input("op-create")
        })
        .unwrap_err();

    assert_eq!(error.code(), ErrorCode::IdempotencyConflict);
    assert_eq!(error.code().wire(), "idempotency_conflict");
}

#[test]
fn stale_revision_is_rejected_and_content_is_kept() {
    let (mut core, id) = core_with_draft();
    core.save_draft(&id, "第一次", 1, "op-1").unwrap();

    let error = core.save_draft(&id, "第二次", 1, "op-2").unwrap_err();
    assert_eq!(error.code(), ErrorCode::RevisionConflict);

    // 用户的内容没有被覆盖：库里仍是第一次保存的内容，revision 也没被推进。
    let capture = core.get_capture(&id).unwrap();
    assert_eq!(capture.draft_text, "第一次");
    assert_eq!(capture.revision, 2);
}

#[test]
fn draft_saves_do_not_create_permanent_versions() {
    let (mut core, id) = core_with_draft();
    for index in 0..5 {
        core.save_draft(&id, &format!("第 {index} 次按键"), 1 + index, &format!("op-{index}"))
            .unwrap();
    }

    let committed = core.commit(&id, 6, "op-commit").unwrap();
    let revision = committed.original_text_revision.unwrap();
    assert_eq!(revision.text.as_deref(), Some("第 4 次按键"));
    assert_eq!(
        committed.capture.ordered_source_ids.len(),
        1,
        "五次草稿保存只应在提交时产生一个永久版本"
    );
}

#[test]
fn committed_capture_cannot_be_saved_as_draft() {
    let (mut core, id) = core_with_draft();
    core.save_draft(&id, "内容", 1, "op-save").unwrap();
    core.commit(&id, 2, "op-commit").unwrap();

    let error = core.save_draft(&id, "提交后再存草稿", 3, "op-late").unwrap_err();
    assert_eq!(error.code(), ErrorCode::InvalidState);
}

#[test]
fn revise_text_keeps_the_old_revision() {
    let (mut core, id) = core_with_draft();
    core.save_draft(&id, "原始说法", 1, "op-save").unwrap();
    let committed = core.commit(&id, 2, "op-commit").unwrap();
    let source_id = committed.original_text_revision.unwrap().source_id;

    let revised = core
        .revise_text(&source_id, "改过的说法", 3, "op-revise")
        .unwrap();
    assert_eq!(revised.text.as_deref(), Some("改过的说法"));
    assert!(revised.parent_revision_id.is_some(), "必须记住父版本");

    let capture = core.get_capture(&id).unwrap();
    assert_eq!(capture.revision, 4, "修订也要推进记录的 revision");
}

#[test]
fn unicode_and_emoji_round_trip() {
    let (mut core, id) = core_with_draft();
    let text = "妈妈离职了，晚上又觉得还行 🙂 —— 真的吗？";
    core.save_draft(&id, text, 1, "op-save").unwrap();
    let committed = core.commit(&id, 2, "op-commit").unwrap();

    assert_eq!(committed.capture.draft_text, text);
    assert_eq!(
        committed.original_text_revision.unwrap().text.as_deref(),
        Some(text)
    );
    assert_eq!(core.get_capture(&id).unwrap().draft_text, text);
}

#[test]
fn day_key_follows_the_local_offset_not_utc() {
    let mut core = Core::open_in_memory().unwrap();
    // 东八区的 2026-09-28 00:30 在 UTC 还是 09-27 16:30。
    let occurred = Utc.with_ymd_and_hms(2026, 9, 27, 16, 30, 0).unwrap();
    let capture = core
        .create_draft(CreateDraftInput {
            occurred_at: Some(occurred),
            ..new_input("op-midnight")
        })
        .unwrap();
    assert_eq!(capture.day_key, "2026-09-28");

    // 零偏移下同一条记录属于 09-27。
    let utc_capture = core
        .create_draft(CreateDraftInput {
            occurred_at: Some(occurred),
            time_zone: "UTC",
            utc_offset_minutes: 0,
            operation_id: "op-midnight-utc",
        })
        .unwrap();
    assert_eq!(utc_capture.day_key, "2026-09-27");
}

#[test]
fn unknown_id_reports_not_found() {
    let core = Core::open_in_memory().unwrap();
    let error = core.get_capture("cap_不存在").unwrap_err();
    assert_eq!(error.code(), ErrorCode::NotFound);
    assert_eq!(error.code().wire(), "not_found");
}

#[test]
fn events_are_emitted_in_order_on_every_write() {
    let (mut core, id) = core_with_draft();
    core.save_draft(&id, "内容", 1, "op-save").unwrap();
    core.commit(&id, 2, "op-commit").unwrap();

    let events = core.events_page(0, 1000).unwrap().events;
    assert!(events.len() >= 3, "创建、保存、提交都应留下事件");
    let mut previous = 0;
    for event in &events {
        assert!(event.sequence > previous, "序号必须单调递增");
        previous = event.sequence;
        assert_eq!(event.entity_id, id);
        assert_eq!(event.event_type.wire(), "capture.changed");
    }
    // 游标之后的读取不能重复给出旧事件。
    let tail = core
        .events_page(events[1].sequence, 1000)
        .unwrap()
        .events;
    assert_eq!(tail.len(), events.len() - 2);
}

#[test]
fn events_since_page_is_bounded_and_reads_forward() {
    let (mut core, id) = core_with_draft();
    let mut revision = 1;
    for index in 0..5 {
        revision = core
            .save_draft(
                &id,
                &format!("分页事件 {index}"),
                revision,
                &format!("op-page-{index}"),
            )
            .unwrap()
            .revision;
    }
    assert!(revision > 1);

    let all = core.events_page(0, 1000).unwrap().events;
    assert!(all.len() > 4, "五次保存至少留下五条事件");

    // 每页两条：返回值必须是有界的一批，且从游标之后继续。
    let first = core.events_since_page(0, 2).unwrap();
    assert_eq!(first, all[..2].to_vec());
    let second = core
        .events_since_page(first.last().unwrap().sequence, 2)
        .unwrap();
    assert_eq!(second, all[2..4].to_vec());
    assert!(second[0].sequence > first[1].sequence);

    // 0 被夹成 1，不会静默返回空；超大值被夹成 1000，不会绕过上限。
    assert_eq!(core.events_since_page(0, 0).unwrap().len(), 1);
    assert_eq!(core.events_since_page(0, usize::MAX).unwrap(), all);
}

#[test]
fn list_captures_pages_deterministically() {
    let mut core = Core::open_in_memory().unwrap();
    let base = Utc.with_ymd_and_hms(2026, 9, 27, 10, 0, 0).unwrap();
    for index in 0..5 {
        core.create_draft(CreateDraftInput {
            occurred_at: Some(base + chrono::Duration::minutes(index)),
            ..new_input(&format!("op-{index}"))
        })
        .unwrap();
    }

    let first = core.list_captures(None, None, 2).unwrap();
    assert_eq!(first.captures.len(), 2);
    assert!(first.next_cursor.is_some());

    let second = core
        .list_captures(None, first.next_cursor.as_deref(), 2)
        .unwrap();
    assert_eq!(second.captures.len(), 2);
    // 两页之间不能出现重复。
    for left in &first.captures {
        for right in &second.captures {
            assert_ne!(left.id, right.id);
        }
    }

    let third = core
        .list_captures(None, second.next_cursor.as_deref(), 2)
        .unwrap();
    assert_eq!(third.captures.len(), 1);
    assert!(third.next_cursor.is_none(), "最后一页不该再给游标");

    // day_key 过滤只返回当天的记录。
    let shanghai = core.list_captures(Some("2026-09-27"), None, 10).unwrap();
    assert_eq!(shanghai.captures.len(), 5);
    let empty = core.list_captures(Some("2020-01-01"), None, 10).unwrap();
    assert!(empty.captures.is_empty());
}

#[test]
fn committed_data_survives_reopening_the_library() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.sqlite");
    let id;
    {
        let mut core = Core::open(&path).unwrap();
        let capture = core.create_draft(new_input("op-create")).unwrap();
        id = capture.id.clone();
        core.save_draft(&id, "关机前写下的内容", 1, "op-save").unwrap();
        core.commit(&id, 2, "op-commit").unwrap();
    }

    // 重新打开：已确认的记录必须在。
    let core = Core::open(&path).unwrap();
    let capture = core.get_capture(&id).unwrap();
    assert_eq!(capture.state, CaptureState::Committed);
    assert_eq!(capture.draft_text, "关机前写下的内容");
    assert_eq!(capture.ordered_source_ids.len(), 1);

    let events = core.events_page(0, 1000).unwrap().events;
    assert!(events.len() >= 3, "事件也要留下来");
}

#[test]
fn error_codes_match_the_contract() {
    // 契约第 7 节的 wire 字符串不能悄悄改。
    assert_eq!(ErrorCode::NotFound.wire(), "not_found");
    assert_eq!(ErrorCode::InvalidState.wire(), "invalid_state");
    assert_eq!(ErrorCode::InvalidArgument.wire(), "invalid_argument");
    assert_eq!(ErrorCode::RevisionConflict.wire(), "revision_conflict");
    assert_eq!(ErrorCode::IdempotencyConflict.wire(), "idempotency_conflict");
    assert_eq!(ErrorCode::StorageFull.wire(), "storage_full");
    // B4a 的远端服务错误码（契约第 7 节）。
    assert_eq!(
        ErrorCode::ProviderNotConfigured.wire(),
        "provider_not_configured"
    );
    assert_eq!(
        ErrorCode::AuthenticationFailed.wire(),
        "authentication_failed"
    );
    assert_eq!(ErrorCode::NetworkUnavailable.wire(), "network_unavailable");
    assert_eq!(ErrorCode::RateLimited.wire(), "rate_limited");
    assert_eq!(ErrorCode::QuotaExceeded.wire(), "quota_exceeded");
    assert_eq!(ErrorCode::ProviderTimeout.wire(), "provider_timeout");
    assert_eq!(ErrorCode::ModelUnavailable.wire(), "model_unavailable");

    let not_found = Core::open_in_memory()
        .unwrap()
        .get_capture("cap_x")
        .unwrap_err();
    assert!(!not_found.retryable(), "找不到目标不该被标成可重试");
    assert!(matches!(not_found, CoreError::NotFound { .. }));
}
#[test]
fn a_full_database_maps_to_storage_full() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.sqlite");
    let mut core = Core::open(&path).unwrap();

    // 造「写满了」：把资料库可用的页数压到极小。没有 root 时没法造真的满盘，
    // 但 SQLite 走的都是同一条 SQLITE_FULL 路径。
    core.set_page_limit_for_test(2).unwrap();

    let mut storage_full = None;
    for index in 0..200 {
        let result = core.create_draft(CreateDraftInput {
            occurred_at: Some(Utc.with_ymd_and_hms(2026, 9, 27, 9, 0, 0).unwrap()),
            operation_id: &format!("op-fill-{index}"),
            ..new_input("op-unused")
        });
        if let Err(error) = result {
            storage_full = Some(error);
            break;
        }
    }

    let error = storage_full.expect("页数受限后应当写不下去");
    assert_eq!(
        error.code(),
        ErrorCode::StorageFull,
        "写满必须落到 storage_full，而不是笼统的数据库错误：{error}"
    );

    // 写不进去之后，之前已经确认的数据不能受影响。
    let surviving = core.list_captures(None, None, 100).unwrap();
    assert!(!surviving.captures.is_empty(), "已确认的记录必须还在");
}

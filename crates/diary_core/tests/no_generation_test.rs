use std::sync::Arc;

use chrono::{Duration, Utc};
use diary_core::{
    Core, CreateDraftInput, DiaryGenerationReason, JobPriority, JobState, NewJob, PanicTransport,
    Transport,
};

fn core(dir: &std::path::Path) -> Core {
    let mut core = Core::open_in_memory_at(dir).unwrap();
    core.set_transport_for_test(Arc::new(PanicTransport::new()) as Arc<dyn Transport>);
    core
}

#[test]
fn generation_is_rejected_before_snapshot_or_job_creation() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = core(dir.path());
    assert!(!diary_core::release_policy::GENERATION_ENABLED);
    let error = core
        .generate_diary(
            "2026-10-30",
            None,
            DiaryGenerationReason::Manual,
            "disabled",
        )
        .unwrap_err();
    assert!(error.to_string().contains("不提供日记生成"));
    assert!(core.list_jobs(None, 100).unwrap().is_empty());
}

#[test]
fn retained_generation_jobs_are_cancelled_while_local_index_jobs_run() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = core(dir.path());
    let generate = core
        .enqueue_job(NewJob::new("diary_generate", JobPriority::SaveAndRecording))
        .unwrap();
    let respond = core
        .enqueue_job(NewJob::new("respond", JobPriority::SaveAndRecording))
        .unwrap();
    let index = core
        .enqueue_job(NewJob::new("index", JobPriority::Maintenance))
        .unwrap();
    let summary = core.run_due_jobs(Utc::now(), 5000, "test", 100).unwrap();
    assert_eq!(
        core.get_job(&generate.id).unwrap().state,
        JobState::Cancelled
    );
    assert_eq!(
        core.get_job(&respond.id).unwrap().state,
        JobState::Cancelled
    );
    assert_eq!(core.get_job(&index.id).unwrap().state, JobState::Succeeded);
    assert_eq!(summary.failed, 0);
    assert_eq!(summary.succeeded, 1);
}

#[test]
fn enabled_legacy_schedule_does_not_queue_generation_or_rewrite_records() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = core(dir.path());
    let draft = core
        .create_draft(CreateDraftInput {
            occurred_at: Some(Utc::now() - Duration::days(2)),
            time_zone: "Asia/Shanghai",
            utc_offset_minutes: 480,
            operation_id: "record",
        })
        .unwrap();
    let saved = core
        .save_draft(&draft.id, "今天和朋友在学校散步。", draft.revision, "save")
        .unwrap();
    core.commit(&draft.id, saved.revision, "commit").unwrap();
    let settings = core.organize_settings().unwrap();
    let mut enabled = settings.clone();
    enabled.enabled = true;
    core.update_organize_settings(&enabled, settings.revision)
        .unwrap();
    let result = core.plan_organize(Utc::now()).unwrap();
    assert!(result.queued_days.is_empty());
    core.run_due_jobs(Utc::now(), 5000, "foreground", 100)
        .unwrap();
    assert_eq!(
        core.get_capture(&draft.id).unwrap().draft_text,
        "今天和朋友在学校散步。"
    );
    assert!(core
        .list_jobs(None, 100)
        .unwrap()
        .iter()
        .all(|j| j.kind != "diary_generate"));
    assert!(!core
        .list_days(None, None, None, 100)
        .unwrap()
        .days
        .is_empty());
}

#[test]
fn actual_http_transport_rejects_official_and_custom_endpoints_before_network() {
    let transport = diary_core::UreqTransport::new();
    for url in [
        "https://api.deepseek.com/models",
        "http://127.0.0.1:9/v1/chat/completions",
    ] {
        let error = transport
            .send(&diary_core::HttpRequest::new("GET", url, 1))
            .unwrap_err();
        assert!(error.to_string().contains("不连接在线模型服务"));
    }
}

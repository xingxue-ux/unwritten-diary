//! B1c 的录音会话与任务队列测试。
//!
//! 对应验收计划 E14（录音进程被终止后重开，已确认片段恢复、缺口可见）
//! 与任务书 3.3、7.1 节的要求。

use chrono::{Duration, Utc};
use diary_core::{
    CaptureState, Core, CreateDraftInput, ErrorCode, JobPriority, JobState, NativeRecordingStatus,
    NewJob, RecordingState, RecordingTicket, SegmentManifest,
};
use sha2::{Digest, Sha256};

fn sha_of(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

fn new_core(dir: &std::path::Path) -> Core {
    Core::open_in_memory_at(dir).unwrap()
}

fn draft(core: &mut Core) -> String {
    core.create_draft(CreateDraftInput {
        occurred_at: None,
        time_zone: "Asia/Shanghai",
        utc_offset_minutes: 480,
        operation_id: "op-create",
    })
    .unwrap()
    .id
}

/// 写一个片段文件并登记。
fn add_segment(
    core: &mut Core,
    ticket: &RecordingTicket,
    index: i64,
    bytes: &[u8],
    duration_ms: i64,
) -> diary_core::SegmentReceipt {
    let name = format!("seg-{index}.bin");
    std::fs::write(
        std::path::Path::new(&ticket.staging_ticket).join(&name),
        bytes,
    )
    .unwrap();
    core.register_segment(
        &ticket.recording_id,
        index,
        SegmentManifest {
            segment_id: format!("seg{index}"),
            relative_file_name: name,
            duration_ms,
            sha256: sha_of(bytes),
            byte_size: bytes.len() as i64,
            closed_at: None,
        },
    )
    .unwrap()
}

#[test]
fn registering_the_same_segment_twice_returns_the_same_receipt() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    let capture_id = draft(&mut core);
    let ticket = core.prepare_recording(&capture_id, "op-rec").unwrap();

    let first = add_segment(&mut core, &ticket, 0, b"first segment", 500);
    let name = "seg-0.bin";
    let second = core
        .register_segment(
            &ticket.recording_id,
            0,
            SegmentManifest {
                segment_id: "seg0".to_owned(),
                relative_file_name: name.to_owned(),
                duration_ms: 500,
                sha256: sha_of(b"first segment"),
                byte_size: 13,
                closed_at: None,
            },
        )
        .unwrap();
    assert_eq!(first, second, "同序号同内容必须返回原回执");

    // 换个内容占同一个序号：必须报冲突，不能悄悄覆盖。
    std::fs::write(
        std::path::Path::new(&ticket.staging_ticket).join(name),
        b"different content",
    )
    .unwrap();
    let conflict = core.register_segment(
        &ticket.recording_id,
        0,
        SegmentManifest {
            segment_id: "seg0".to_owned(),
            relative_file_name: name.to_owned(),
            duration_ms: 999,
            sha256: sha_of(b"different content"),
            byte_size: 17,
            closed_at: None,
        },
    );
    assert_eq!(conflict.unwrap_err().code(), ErrorCode::IdempotencyConflict);
}

#[test]
fn a_segment_is_verified_against_its_declared_hash() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    let capture_id = draft(&mut core);
    let ticket = core.prepare_recording(&capture_id, "op-rec").unwrap();
    std::fs::write(
        std::path::Path::new(&ticket.staging_ticket).join("seg.bin"),
        b"real bytes",
    )
    .unwrap();

    let error = core
        .register_segment(
            &ticket.recording_id,
            0,
            SegmentManifest {
                segment_id: "seg0".to_owned(),
                relative_file_name: "seg.bin".to_owned(),
                duration_ms: 100,
                sha256: sha_of(b"what the platform claimed"),
                byte_size: 10,
                closed_at: None,
            },
        )
        .unwrap_err();
    assert_eq!(error.code(), ErrorCode::IntegrityFailed);
}

#[test]
fn finalize_builds_one_logical_audio_asset() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    let capture_id = draft(&mut core);
    let ticket = core.prepare_recording(&capture_id, "op-rec").unwrap();

    add_segment(&mut core, &ticket, 0, b"AAAA", 1000);
    add_segment(&mut core, &ticket, 1, b"BBBB", 2000);
    add_segment(&mut core, &ticket, 2, b"CCCC", 3000);

    let result = core
        .finalize_recording(&ticket.recording_id, 2, "user")
        .unwrap();
    assert_eq!(result.state, RecordingState::Saved);
    assert_eq!(result.segment_count, 3);
    assert_eq!(result.logical_duration_ms, 6000);
    assert!(result.transcription_queued, "转写任务应当在同一个事务里排上");
    assert_eq!(result.capture_id, capture_id, "finalize 要报出它挂到哪条记录");
    assert_eq!(
        result.capture_revision, 2,
        "finalize 推进一步 revision，客户端要拿这个值当 commit 的 expected_revision"
    );

    // 逻辑音频真在文件库里，内容是按序拼起来的。
    let lease = core.open_asset(&result.asset_id, "play").unwrap();
    assert_eq!(std::fs::read(&lease.handle).unwrap(), b"AAAABBBBCCCC");

    // 片段进了逻辑音频，暂存目录清掉了。
    assert!(!std::path::Path::new(&ticket.staging_ticket).exists());

    // 材料挂到了记录上。
    let capture = core.get_capture(&capture_id).unwrap();
    assert_eq!(capture.revision, 2);
    assert_eq!(capture.ordered_source_ids.len(), 1);

    // 录音 finalize 只推进 revision，**不会**把记录变成 committed；要客户端再 commit。
    assert_eq!(
        capture.state,
        CaptureState::Draft,
        "finalize 的契约是「音频已保存」，不是「记录已成篇」"
    );
    let commit = core
        .commit(&result.capture_id, result.capture_revision, "op-commit")
        .unwrap();
    assert_eq!(commit.capture.state, CaptureState::Committed);
    assert_eq!(commit.capture.revision, result.capture_revision + 1);
    assert!(
        commit.original_text_revision.is_none(),
        "空草稿 + 音频来源：commit 不该凭空造一条空文字修订"
    );
    assert_eq!(
        core.count_jobs(Some(&[JobState::Queued])).unwrap(),
        1,
        "plain commit 不排任务，转写仍然只有 finalize 排的那一条"
    );
    // 同一个 operation_id 重试拿到同一份回执：录音提交失败后重试的幂等依据。
    let retry = core
        .commit(&result.capture_id, result.capture_revision, "op-commit")
        .unwrap();
    assert_eq!(retry, commit);

    // 重复最终化必须被拒绝。
    let again = core.finalize_recording(&ticket.recording_id, 2, "user");
    assert_eq!(again.unwrap_err().code(), ErrorCode::InvalidState);
}

#[test]
fn transcription_is_queued_in_the_same_transaction_as_finalize() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    let capture_id = draft(&mut core);
    let ticket = core.prepare_recording(&capture_id, "op-rec").unwrap();
    add_segment(&mut core, &ticket, 0, b"audio", 500);

    let result = core.finalize_recording(&ticket.recording_id, 0, "user").unwrap();
    let jobs = core.list_jobs(None, 10).unwrap();
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].kind, "transcribe");
    assert_eq!(jobs[0].state, JobState::Queued);
    assert_eq!(jobs[0].target_ids, vec![result.asset_id]);
    assert_eq!(jobs[0].priority, JobPriority::BackgroundExtract.value());
}

#[test]
fn a_gap_is_reported_instead_of_pretending_the_recording_is_complete() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    let capture_id = draft(&mut core);
    let ticket = core.prepare_recording(&capture_id, "op-rec").unwrap();

    // 第 1 段丢了，只有 0 和 2。
    add_segment(&mut core, &ticket, 0, b"AAAA", 1000);
    add_segment(&mut core, &ticket, 2, b"CCCC", 3000);

    let result = core
        .finalize_recording(&ticket.recording_id, 2, "interrupted")
        .unwrap();
    assert_eq!(
        result.state,
        RecordingState::Recoverable,
        "有缺口就不能只报 saved"
    );

    let recovery = core.recover_recording(Some(&ticket.recording_id)).unwrap();
    assert_eq!(recovery.closed_segment_indexes, vec![0, 2]);
    assert!(recovery.gap_ms > 0, "缺口时长要给出估算");
    assert!(
        recovery.notes.iter().any(|note| note.contains("缺 1 个片段")),
        "缺口必须写清楚：{:?}",
        recovery.notes
    );
}

#[test]
fn a_killed_recording_comes_back_as_recoverable() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("library.sqlite");

    let (recording_id, capture_id, queued) = {
        let mut core = Core::open(&db).unwrap();
        let capture_id = draft(&mut core);
        let ticket = core.prepare_recording(&capture_id, "op-rec").unwrap();
        add_segment(&mut core, &ticket, 0, b"AAAA", 1000);
        add_segment(&mut core, &ticket, 1, b"BBBB", 1000);
        core.update_recording_state(NativeRecordingStatus {
            recording_id: ticket.recording_id.clone(),
            state: RecordingState::Recording,
            wall_clock: Utc::now(),
            elapsed_ms: 2000,
            persisted_through_ms: 2000,
            input_device_changed: false,
            message: None,
        })
        .unwrap();
        // 进程在这里被杀掉，没有 finalize。
        (ticket.recording_id.clone(), capture_id, ticket)
    };

    let mut core = Core::open(&db).unwrap();
    let recovery = core.recover_recording(None).unwrap();
    assert_eq!(recovery.recording_id.as_deref(), Some(recording_id.as_str()));
    assert_eq!(recovery.state, RecordingState::Recoverable);
    assert_eq!(recovery.closed_segment_indexes, vec![0, 1]);
    assert!(
        recovery.notes.iter().any(|note| note.contains("尾巴")),
        "要说清楚尾部可能缺：{:?}",
        recovery.notes
    );

    // 已封闭片段还在，可以直接救回来。
    let result = core
        .finalize_recording(&recording_id, 1, "recovered")
        .unwrap();
    assert_eq!(result.segment_count, 2);
    assert_eq!(
        result.capture_id, capture_id,
        "恢复后 finalize 也必须报出原来的记录"
    );
    assert_eq!(
        result.capture_revision, 2,
        "恢复后 finalize 的 revision 同样是权威值，可直接用于 commit"
    );
    let lease = core.open_asset(&result.asset_id, "play").unwrap();
    assert_eq!(std::fs::read(&lease.handle).unwrap(), b"AAAABBBB");
    // 恢复路径也要能提交成正式记录，否则救回来仍停在 draft。
    let commit = core
        .commit(&result.capture_id, result.capture_revision, "op-recovered-commit")
        .unwrap();
    assert_eq!(commit.capture.state, CaptureState::Committed);
    assert_eq!(commit.capture.revision, result.capture_revision + 1);
    let _ = queued;
}

/// 录音**中途**崩溃、尾段还没封闭登记：恢复不能把「已收到」当成「已持久化」。
///
/// 场景与真机一致：2 秒一段、录到 3.5 秒时进程被杀。第 0 段（2,000 ms）登记成功，
/// 第 1 段还开着（1.5 秒的 PCM 只在内存/未封闭文件里），采集器最后一次上报收到
/// 3,500 ms。恢复必须：
/// - `closed_segment_indexes` 只含已登记的第 0 段；
/// - `persisted_ms` = 已登记片段之和（2,000），**不是** 3,500；
/// - `received_ms` = 3,500，并在 notes 里如实写「有 1,500 ms 可能是未封闭的尾巴」；
/// - 按已封闭序号 finalize 出来的音频时长 = 2,000 ms。
#[test]
fn a_crash_before_the_tail_is_registered_does_not_claim_it_is_persisted() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("library.sqlite");

    let recording_id = {
        let mut core = Core::open(&db).unwrap();
        let capture_id = draft(&mut core);
        let ticket = core.prepare_recording(&capture_id, "op-rec").unwrap();
        // 第 0 段：2 秒，已封闭并登记成功。
        add_segment(&mut core, &ticket, 0, b"AAAA", 2000);
        // 采集器在 3.5 秒时上报：收到 3500，已登记 2000（第 1 段还没封闭）。
        core.update_recording_state(NativeRecordingStatus {
            recording_id: ticket.recording_id.clone(),
            state: RecordingState::Recording,
            wall_clock: Utc::now(),
            elapsed_ms: 3500,
            persisted_through_ms: 2000,
            input_device_changed: false,
            message: None,
        })
        .unwrap();
        // 进程在这里被杀掉：第 1 段的 1.5 秒 PCM 没来得及登记。
        ticket.recording_id
    };

    let mut core = Core::open(&db).unwrap();
    let recovery = core.recover_recording(None).unwrap();
    assert_eq!(recovery.recording_id.as_deref(), Some(recording_id.as_str()));
    assert_eq!(recovery.state, RecordingState::Recoverable);
    assert_eq!(
        recovery.closed_segment_indexes,
        vec![0],
        "只有登记成功的那一段算数"
    );
    assert_eq!(
        recovery.persisted_ms, 2000,
        "persisted 只能等于已登记片段之和，不能把收到的 3500 报成已持久化"
    );
    assert_eq!(recovery.received_ms, 3500, "received 是采集器上报的收到量");
    assert_eq!(
        recovery.received_ms - recovery.persisted_ms,
        1500,
        "两者之差就是可能丢失的尾巴"
    );
    assert!(
        recovery
            .notes
            .iter()
            .any(|note| note.contains("未封闭的尾巴") && note.contains("1500 ms")),
        "必须如实说明可能丢掉的尾巴：{:?}",
        recovery.notes
    );

    // 恢复出的音频时长 = 已登记片段之和（2 秒），不是收到的 3.5 秒。
    let result = core
        .finalize_recording(&recording_id, 0, "recovered")
        .unwrap();
    assert_eq!(result.state, RecordingState::Saved);
    assert_eq!(result.segment_count, 1);
    assert_eq!(result.logical_duration_ms, 2000);
    let lease = core.open_asset(&result.asset_id, "play").unwrap();
    assert_eq!(std::fs::read(&lease.handle).unwrap(), b"AAAA");
}

/// 正常停止：最后一段先封闭登记，再上报 stopping。此时 received 必须等于 persisted。
#[test]
fn a_normal_stop_reports_received_equal_to_persisted() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    let capture_id = draft(&mut core);
    let ticket = core.prepare_recording(&capture_id, "op-rec").unwrap();
    // 2 秒 + 2 秒 + 1 秒：停止时最后 1 秒也封闭并登记成功。
    add_segment(&mut core, &ticket, 0, b"AAAA", 2000);
    add_segment(&mut core, &ticket, 1, b"BBBB", 2000);
    add_segment(&mut core, &ticket, 2, b"CC", 1000);

    let session = core
        .update_recording_state(NativeRecordingStatus {
            recording_id: ticket.recording_id.clone(),
            state: RecordingState::Stopping,
            wall_clock: Utc::now(),
            elapsed_ms: 5000,
            persisted_through_ms: 5000,
            input_device_changed: false,
            message: None,
        })
        .unwrap();
    assert_eq!(session.elapsed_ms, 5000);
    assert_eq!(session.durable_through_ms, 5000);

    let result = core
        .finalize_recording(&ticket.recording_id, 2, "user_stop")
        .unwrap();
    assert_eq!(result.logical_duration_ms, 5000);

    let recovery = core
        .recover_recording(Some(&ticket.recording_id))
        .unwrap();
    assert_eq!(recovery.persisted_ms, recovery.received_ms);
    assert_eq!(recovery.persisted_ms, 5000);
    assert!(
        recovery.notes.iter().any(|note| note.contains("没有可见的未登记尾巴")),
        "正常停止不该虚报尾巴：{:?}",
        recovery.notes
    );
}

#[test]
fn interrupted_is_not_the_same_as_paused() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    let capture_id = draft(&mut core);
    let ticket = core.prepare_recording(&capture_id, "op-rec").unwrap();
    add_segment(&mut core, &ticket, 0, b"AAAA", 100);

    let session = core
        .update_recording_state(NativeRecordingStatus {
            recording_id: ticket.recording_id.clone(),
            state: RecordingState::Interrupted,
            wall_clock: Utc::now(),
            elapsed_ms: 100,
            persisted_through_ms: 100,
            input_device_changed: false,
            message: Some("来电中断".to_owned()),
        })
        .unwrap();
    assert_eq!(session.state, RecordingState::Interrupted);
    assert_eq!(session.segment_count, 1);
    assert_ne!(session.state.wire(), "paused");
}

#[test]
fn commit_with_jobs_is_one_transaction() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    let capture_id = draft(&mut core);
    core.save_draft(&capture_id, "记录内容", 1, "op-save").unwrap();

    let job = NewJob::new("extract", JobPriority::BackgroundExtract)
        .targeting(vec![capture_id.clone()])
        .with_snapshot("snapshot-1");
    let committed = core
        .commit_with_jobs(&capture_id, 2, "op-commit", &[job])
        .unwrap();
    assert_eq!(committed.capture.state, diary_core::CaptureState::Committed);

    let jobs = core.list_jobs(None, 10).unwrap();
    assert_eq!(jobs.len(), 1, "提交与入队要么都成、要么都不成");
    assert_eq!(jobs[0].kind, "extract");

    // 同一个 operationId 再提交一次，但换了任务：内容不同 → 冲突。
    let other = NewJob::new("index", JobPriority::AutoOrganize);
    let conflict = core.commit_with_jobs(&capture_id, 3, "op-commit", &[other]);
    assert_eq!(conflict.unwrap_err().code(), ErrorCode::IdempotencyConflict);
}

#[test]
fn jobs_are_claimed_by_priority() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());

    core.enqueue_job(NewJob::new("maintenance", JobPriority::Maintenance)).unwrap();
    core.enqueue_job(NewJob::new("auto-organize", JobPriority::AutoOrganize)).unwrap();
    core.enqueue_job(NewJob::new("save-flush", JobPriority::SaveAndRecording)).unwrap();

    let now = Utc::now();
    let first = core.claim_next_due_job(now).unwrap().unwrap();
    assert_eq!(first.kind, "save-flush");
    assert_eq!(first.state, JobState::Running);
    assert_eq!(first.attempt_count, 1);

    let second = core.claim_next_due_job(now).unwrap().unwrap();
    assert_eq!(second.kind, "auto-organize");
    let third = core.claim_next_due_job(now).unwrap().unwrap();
    assert_eq!(third.kind, "maintenance");
    assert!(core.claim_next_due_job(now).unwrap().is_none());
}

#[test]
fn a_failing_job_backs_off_and_then_gives_up() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    let mut job = NewJob::new("transcribe", JobPriority::BackgroundExtract);
    job.max_attempts = 2;
    let queued = core.enqueue_job(job).unwrap();

    // 第一次尝试失败 → 退避等待。
    let now = Utc::now();
    core.claim_next_due_job(now).unwrap().unwrap();
    let after_fail = core
        .fail_job(&queued.id, "network_unavailable", "断网", now)
        .unwrap();
    assert_eq!(after_fail.state, JobState::RetryWait);
    assert_eq!(after_fail.attempt_count, 1);
    let next = after_fail.next_attempt_at.expect("要有下次尝试时间");
    assert!(next > now, "退避时间必须在将来");

    // 还没到点，领不到。
    assert!(core.claim_next_due_job(now).unwrap().is_none());

    // 到点后再试一次，这次用完额度 → failed。
    let later = now + Duration::minutes(10);
    core.claim_next_due_job(later).unwrap().unwrap();
    let exhausted = core
        .fail_job(&queued.id, "provider_timeout", "超时", later)
        .unwrap();
    assert_eq!(exhausted.state, JobState::Failed);
    assert_eq!(exhausted.attempt_count, 2);
    assert_eq!(exhausted.error_code.as_deref(), Some("provider_timeout"));
}

#[test]
fn manual_retry_resets_the_attempt_budget_and_cancel_keeps_originals() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    let capture_id = draft(&mut core);
    let mut job = NewJob::new("transcribe", JobPriority::BackgroundExtract);
    job.max_attempts = 1;
    let queued = core.enqueue_job(job).unwrap();

    let now = Utc::now();
    core.claim_next_due_job(now).unwrap().unwrap();
    let failed = core.fail_job(&queued.id, "network_unavailable", "断网", now).unwrap();
    assert_eq!(failed.state, JobState::Failed);
    assert!(failed.requires_user_action, "已放弃的任务要等用户处理");

    // 人工重试把这一轮的尝试次数清零（B5 的语义：点一次重试，再给一整份额度），
    // 但历史尝试记录不删——`job_attempts` 里仍然看得到「试过几次」。
    let retried = core.retry_job_idempotent(&queued.id, "op-retry").unwrap();
    assert_eq!(retried.state, JobState::Queued);
    assert_eq!(retried.attempt_count, 0);
    assert_eq!(retried.max_attempts, 1);
    assert!(retried.error_code.is_none());
    assert!(!retried.requires_user_action);
    // 同一个 operationId 重试返回原结果。
    assert_eq!(core.retry_job_idempotent(&queued.id, "op-retry").unwrap(), retried);

    let cancelled = core.cancel_job(&queued.id, "op-cancel").unwrap();
    assert_eq!(cancelled.state, JobState::Cancelled);
    // 取消任务不影响任何原件。
    let capture = core.get_capture(&capture_id).unwrap();
    assert_eq!(capture.state, diary_core::CaptureState::Draft);
    assert_eq!(
        core.cancel_job(&queued.id, "op-cancel-2").unwrap_err().code(),
        ErrorCode::InvalidState,
        "已经结束的任务不能再取消"
    );
}

#[test]
fn next_wakeup_reflects_pending_work() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    assert!(core.next_wakeup().unwrap().is_none(), "没有待办就不该唤醒");

    let queued = core
        .enqueue_job(NewJob::new("extract", JobPriority::BackgroundExtract))
        .unwrap();
    let immediate = core.next_wakeup().unwrap().expect("排队中就该马上唤醒");
    assert!(immediate <= Utc::now() + Duration::seconds(1));

    // 失败退避之后，唤醒时刻变成退避时间。
    let now = Utc::now();
    core.claim_next_due_job(now).unwrap().unwrap();
    core.fail_job(&queued.id, "network_unavailable", "断网", now)
        .unwrap();
    let later = core.next_wakeup().unwrap().unwrap();
    assert!(later > now);
}

#[test]
fn the_queue_survives_reopening_the_library() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("library.sqlite");
    let job_id = {
        let mut core = Core::open(&db).unwrap();
        core.enqueue_job(NewJob::new("transcribe", JobPriority::BackgroundExtract))
            .unwrap()
            .id
    };

    let core = Core::open(&db).unwrap();
    let job = core.get_job(&job_id).unwrap();
    assert_eq!(job.state, JobState::Queued);
    assert_eq!(core.list_jobs(Some(&[JobState::Queued]), 10).unwrap().len(), 1);
}

#[test]
fn attention_acknowledgement_is_recorded_once() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    assert!(!core.is_attention_acknowledged("provider-missing").unwrap());
    core.acknowledge_attention("provider-missing").unwrap();
    assert!(core.is_attention_acknowledged("provider-missing").unwrap());
    // 再记一次不报错，也不会变成两条。
    core.acknowledge_attention("provider-missing").unwrap();
    assert!(core.is_attention_acknowledged("provider-missing").unwrap());
}

#[test]
fn unknown_recording_or_job_reports_not_found() {
    let dir = tempfile::tempdir().unwrap();
    let core = new_core(dir.path());
    assert_eq!(
        core.recording_session("rec_missing").unwrap_err().code(),
        ErrorCode::NotFound
    );
    assert_eq!(
        core.get_job("job_missing").unwrap_err().code(),
        ErrorCode::NotFound
    );
    // 没有任何未完成录音时，恢复不该报错。
    let recovery = core.recover_recording(None).unwrap();
    assert_eq!(recovery.state, RecordingState::Idle);
    assert!(recovery.recording_id.is_none());
}

/// 回归：按状态计数必须精确，不能用 `list_jobs` 的长度代替。
///
/// 桥接的 `info().recovery.pendingJobs` 曾经写成
/// `list_jobs(Some(&[Queued, RetryWait]), 1000)?.len()`，而 `list()` 为分页把
/// limit 夹在 200 以内（`jobs.rs`），所以 201 个以上的待办任务会被少报成 200。
/// 这条测试把两个数字都钉住：计数必须精确，而分页列表本来就只有 200 条。
#[test]
fn counting_pending_jobs_is_exact_beyond_the_page_limit() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());

    const TOTAL: usize = 250;
    for index in 0..TOTAL {
        core.enqueue_job(
            NewJob::new("extract", JobPriority::BackgroundExtract).targeting(vec![format!("src_{index}")]),
        )
        .unwrap();
    }

    assert_eq!(
        core.count_jobs(Some(&[JobState::Queued, JobState::RetryWait]))
            .unwrap(),
        TOTAL as i64,
        "计数必须精确，不能被分页上限截断"
    );
    assert_eq!(
        core.list_jobs(Some(&[JobState::Queued, JobState::RetryWait]), 1000)
            .unwrap()
            .len(),
        200,
        "分页列表的上限就是 200——正因如此，不能拿它的长度当计数"
    );
    // 不带状态筛选时，总数同样精确。
    assert_eq!(core.count_jobs(None).unwrap(), TOTAL as i64);
}

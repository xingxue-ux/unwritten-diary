//! B5 的任务驱动测试：按 kind 执行、退避、补做、没有处理器、幂等与 limit。
//!
//! 对应验收计划 E29（省电/退出错过的任务保留、下次运行时补做、**不谎报准时**）
//! 与 issue #65 的验收标准。全部用例不依赖模型或网络。

use std::path::Path;

use chrono::{Duration, Utc};
use diary_core::{
    Core, CreateDraftInput, ErrorCode, EventType, ImportManifest, ImportOrigin, ImportRequest,
    JobPriority, JobState, NewJob, ProcessingStatus,
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

fn draft(core: &mut Core, operation_id: &str) -> String {
    core.create_draft(CreateDraftInput {
        occurred_at: None,
        time_zone: "Asia/Shanghai",
        utc_offset_minutes: 480,
        operation_id,
    })
    .unwrap()
    .id
}

/// 把一个纯文本文件导入资料库，返回 (capture_id, source_id)。
///
/// 导入完成时队列里**已经**有一条 `extract` 任务（B1b/B2 的接线），所以这些用例
/// 不需要自己入队就能驱动真实的提取。
fn import_text(core: &mut Core, text: &[u8], name: &str, operation_id: &str) -> (String, String) {
    let capture_id = draft(core, &format!("{operation_id}-create"));
    let ticket = core
        .prepare_import(ImportRequest {
            capture_id: &capture_id,
            display_name: name,
            mime_hint: Some("text/plain"),
            size_hint: Some(text.len() as i64),
            origin: ImportOrigin::Picker,
            operation_id: &format!("{operation_id}-prepare"),
        })
        .unwrap();
    std::fs::write(&ticket.staging_ticket, text).unwrap();
    let status = core
        .finish_import(
            &ticket.import_id,
            &ticket.staging_ticket,
            ImportManifest {
                copied_bytes: text.len() as i64,
                sha256: sha_of(text),
                detected_mime: "text/plain".to_owned(),
                original_name: name.to_owned(),
            },
        )
        .unwrap();
    assert!(status.asset_id.is_some(), "导入完成后应当有资产");
    let source_id = core
        .get_capture(&capture_id)
        .unwrap()
        .ordered_source_ids
        .first()
        .cloned()
        .expect("材料应当挂到记录上");
    (capture_id, source_id)
}

fn job_changed_count(core: &Core) -> usize {
    core.events_since(0)
        .unwrap()
        .into_iter()
        .filter(|event| event.event_type == EventType::JobChanged)
        .count()
}

/// 走产品检索路径的一次关键词查询。
fn keyword_search(core: &mut Core, query: &str) -> diary_core::SearchSnapshot {
    core.start_search(
        diary_core::SearchRequest {
            query: query.to_owned(),
            mode: diary_core::SearchMode::Keyword,
            filters: diary_core::SearchFilters::default(),
            page_size: 20,
        },
        1,
    )
    .unwrap()
}

/// 成功路径：导入排下的 `extract` 被驱动跑完，来源真的提取了，任务进 succeeded；
/// 闸门在领取前重评估，导入顺带排的 `index` 在**同一次调用**里被拉起并跑完。
#[test]
fn the_driver_extracts_an_imported_source_and_succeeds() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    let (_, source_id) = import_text(
        &mut core,
        "第一段正文\n\n第二段正文".as_bytes(),
        "笔记.txt",
        "op1",
    );

    let queued = core.list_jobs(Some(&[JobState::Queued]), 10).unwrap();
    assert_eq!(queued.len(), 1, "导入完成就该有一条 extract 任务");
    assert_eq!(queued[0].kind, "extract");

    let summary = core.run_due_jobs(Utc::now(), 60_000, "test", 10).unwrap();
    assert_eq!(
        summary.processed, 2,
        "extract 跑完，依赖闸门把 index 在同一轮拉起并跑掉"
    );
    assert_eq!(summary.succeeded, 2);
    assert_eq!(summary.failed, 0);
    assert_eq!(summary.retrying, 0);
    // 链条一次走完：没有任务再留在等待态，闸门也没有终态失败任何任务。
    assert_eq!(summary.waiting_dependency, 0, "链走完了就不该再等");
    assert_eq!(summary.dependency_failed, 0);
    // 入队即到期（next_attempt_at 为空）的任务不算补做：队列里没有它「本该何时跑」
    // 的信息，把它算进去会让刚入队就被跑的任务也变成「补做」。
    assert_eq!(summary.backfilled, 0, "入队即到期的任务不是补做");
    assert_eq!(summary.due_backlog, 0, "跑完了就没有到期积压");

    let job = core.get_job(&queued[0].id).unwrap();
    assert_eq!(job.state, JobState::Succeeded);
    assert!(job.error_code.is_none());
    assert!(!job.requires_user_action);

    // 来源真的被提取了，而且和关键词索引在同一个事务里。
    let content = core
        .extracted_content(&source_id)
        .unwrap()
        .expect("提取结果应当落库");
    assert_eq!(content.status, ProcessingStatus::Ready);
    assert!(content.text.contains("第一段正文"));
    assert!(
        core.count_search_matches("正文").unwrap() >= 1,
        "提取出来的内容要立刻能搜到"
    );
    assert!(
        core.index_status(Some(&[source_id]))
            .unwrap()
            .keyword_index_ready,
        "索引任务跑完后这条来源报就绪"
    );
}

/// 失败路径：结构性失败（修订不存在）→ 尝试次数递增、退避落在将来、到上限进「已放弃」。
#[test]
fn a_failing_extract_backs_off_and_then_gives_up() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    let mut job = NewJob::new("extract", JobPriority::BackgroundExtract)
        .targeting(vec!["rev_不存在".to_owned()]);
    job.max_attempts = 2;
    let queued = core.enqueue_job(job).unwrap();

    let now = Utc::now();
    let first = core.run_due_jobs(now, 60_000, "test", 10).unwrap();
    assert_eq!(first.processed, 1);
    assert_eq!(first.succeeded, 0);
    assert_eq!(first.failed, 0, "还有额度时不该直接放弃");
    assert_eq!(first.retrying, 1);
    assert_eq!(first.backfilled, 0);

    let after = core.get_job(&queued.id).unwrap();
    assert_eq!(after.state, JobState::RetryWait);
    assert_eq!(after.attempt_count, 1, "尝试次数要如实递增");
    assert_eq!(after.error_code.as_deref(), Some("not_found"));
    let next = after.next_attempt_at.expect("退避要有下次尝试时间");
    assert!(next > now, "退避时间必须落在将来");

    // 还没到点：驱动领不到它，也就没有跑。
    let too_early = core.run_due_jobs(now, 60_000, "test", 10).unwrap();
    assert_eq!(too_early.processed, 0, "退避没到点不能被领走");
    assert_eq!(too_early.due_backlog, 0);
    assert!(too_early.next_wakeup.is_some(), "还有待办就要给出下次唤醒");

    // 到点后再跑一次：额度用完 → 已放弃，等用户处理。
    let later = now + Duration::minutes(10);
    let second = core.run_due_jobs(later, 60_000, "test", 10).unwrap();
    assert_eq!(second.processed, 1);
    assert_eq!(second.failed, 1);
    assert_eq!(second.retrying, 0);
    // 补做口径的另一半：过了退避点才被跑上的重试，就是「早该跑、这次才补上」。
    assert_eq!(second.backfilled, 1, "过了退避点的重试属于补做");

    let exhausted = core.get_job(&queued.id).unwrap();
    assert_eq!(exhausted.state, JobState::Failed);
    assert_eq!(exhausted.attempt_count, 2);
    assert_eq!(exhausted.error_code.as_deref(), Some("not_found"));
    assert!(
        exhausted.requires_user_action,
        "「已放弃」必须让用户看得见"
    );
    assert!(exhausted.next_attempt_at.is_none(), "放弃了就不再排期");
}

/// 人工重试：`failed` → `retry_job` → 清零并可以再次被驱动执行。
#[test]
fn manual_retry_puts_a_given_up_job_back_in_the_queue() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    let mut job = NewJob::new("extract", JobPriority::BackgroundExtract)
        .targeting(vec!["rev_不存在".to_owned()]);
    job.max_attempts = 1;
    let queued = core.enqueue_job(job).unwrap();

    let now = Utc::now();
    let given_up = core.run_due_jobs(now, 60_000, "test", 10).unwrap();
    assert_eq!(given_up.failed, 1);
    assert_eq!(core.get_job(&queued.id).unwrap().state, JobState::Failed);

    let retried = core.retry_job(&queued.id).unwrap();
    assert_eq!(retried.state, JobState::Queued);
    assert_eq!(retried.attempt_count, 0, "人工重试把这一轮的尝试次数清零");
    assert!(retried.error_code.is_none());
    assert!(!retried.requires_user_action);
    assert!(retried.next_attempt_at.is_none(), "重试后立刻可跑");

    // 归零之后确实又被驱动执行了一次（这条还是会失败，但尝试次数重新从 1 开始）。
    let again = core.run_due_jobs(now, 60_000, "test", 10).unwrap();
    assert_eq!(again.processed, 1, "重试后必须能再次被驱动");
    let after = core.get_job(&queued.id).unwrap();
    assert_eq!(after.state, JobState::Failed, "又用完了一次额度");
    assert_eq!(after.attempt_count, 1);

    // 只有「已放弃」的任务能人工重试。
    let fresh = core
        .enqueue_job(
            NewJob::new("extract", JobPriority::Maintenance)
                .targeting(vec!["rev_也_不存在".to_owned()]),
        )
        .unwrap();
    assert_eq!(
        core.retry_job(&fresh.id).unwrap_err().code(),
        ErrorCode::InvalidState,
        "还没放弃的任务不需要人工重试"
    );
}

/// E29：到期任务在进程退出期间保留，重开后补做；如实算作「补做」而不是「准时完成」。
#[test]
fn a_job_missed_while_the_library_was_closed_is_backfilled_after_reopening() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("library.sqlite");
    let now = Utc::now();
    let (job_id, source_id);

    {
        let mut core = Core::open(&db).unwrap();
        let (_, source) = import_text(&mut core, "关机前导入的材料".as_bytes(), "材料.txt", "op1");
        source_id = source;

        // 导入顺带排的那条任务先取消掉，好让这次观察只落在「错过的那条」上。
        let auto = core.list_jobs(Some(&[JobState::Queued]), 10).unwrap().remove(0);
        core.cancel_job(&auto.id, "op-cancel-auto").unwrap();

        // 一条**排期在两小时前**的任务：它本来就该在那个时刻跑，但进程不在。
        let missed = core
            .enqueue_job(
                NewJob::new("extract", JobPriority::BackgroundExtract)
                    .targeting(auto.target_ids.clone())
                    .with_snapshot(auto.input_snapshot_hash.clone().unwrap_or_default())
                    .scheduled_at(now - Duration::hours(2)),
            )
            .unwrap();
        assert!(
            missed.next_attempt_at.expect("排期要真的落库") < now,
            "排期必须在过去，这条用例才有意义"
        );
        job_id = missed.id;
        // 资料库在这里关掉：模拟「退出/省电期间错过了它」。
    }

    let mut core = Core::open(&db).unwrap();
    // 关库重开之后任务还在，而且仍然记着它该在什么时候跑。
    let before = core.get_job(&job_id).unwrap();
    assert_eq!(before.state, JobState::Queued);
    assert!(before.next_attempt_at.expect("排期要留着") < now);

    let summary = core.run_due_jobs(now, 60_000, "test", 10).unwrap();
    assert_eq!(
        summary.processed, 2,
        "重开之后必须能跑到它；它跑完，导入排的来源索引任务也在同一轮被拉起跑掉"
    );
    assert_eq!(summary.succeeded, 2, "补做也要真的做完");
    assert_eq!(
        summary.backfilled, 1,
        "本来早该跑、这次才补上：必须如实算作补做（被拉起的索引任务不是补做）"
    );
    // 补做的任务不会因为没有准时跑就被丢掉：来源真的提取了。
    let content = core
        .extracted_content(&source_id)
        .unwrap()
        .expect("补做之后提取结果要在");
    assert!(content.text.contains("关机前导入"));

    // 状态摘要如实呈现：已经补做完了，没有积压，也没有谎报「准时」的字段。
    let status = core.job_status().unwrap();
    assert_eq!(status.succeeded, 2, "补做的提取与它带起的索引任务都成功了");
    assert_eq!(status.queued, 0);
    assert_eq!(status.due_backlog, 0);
}

/// 没有执行者的 kind：进「已放弃」+ `no_handler` + `requires_user_action`，绝不静默成功。
#[test]
fn a_kind_without_a_handler_is_never_silently_completed() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    let queued = core
        .enqueue_job(
            NewJob::new("transcribe", JobPriority::BackgroundExtract)
                .targeting(vec!["asset_录音".to_owned()]),
        )
        .unwrap();

    let summary = core.run_due_jobs(Utc::now(), 60_000, "test", 10).unwrap();
    assert_eq!(summary.processed, 1);
    assert_eq!(summary.succeeded, 0, "没有执行者绝不能算成功");
    assert_eq!(summary.failed, 1, "要走「已放弃」");

    let job = core.get_job(&queued.id).unwrap();
    assert_eq!(job.state, JobState::Failed);
    assert_eq!(job.error_code.as_deref(), Some("no_handler"));
    assert!(job.requires_user_action, "要出现在状态里、能人工重试");
    assert!(
        job.next_attempt_at.is_none(),
        "没有处理器不会自己出现，不该排退避重试"
    );
    assert_eq!(job.attempt_count, 1);

    // 状态摘要看得到它，而且只有 kind/错误码/时间，没有用户内容。
    let status = core.job_status().unwrap();
    assert_eq!(status.failed, 1);
    assert_eq!(status.due_backlog, 0);
    let error = status
        .recent_errors
        .first()
        .expect("已放弃的任务要出现在最近错误里");
    assert_eq!(error.job_id, queued.id);
    assert_eq!(error.kind, "transcribe");
    assert_eq!(error.error_code, "no_handler");
    assert!(error.requires_user_action);

    // B4a 的 Provider 到位之前，人工重试也只会得到同样的结论（不会变成成功）。
    let retried = core.retry_job(&queued.id).unwrap();
    assert_eq!(retried.state, JobState::Queued);
    let again = core.run_due_jobs(Utc::now(), 60_000, "test", 10).unwrap();
    assert_eq!(again.failed, 1);
    assert_eq!(
        core.get_job(&queued.id).unwrap().error_code.as_deref(),
        Some("no_handler")
    );
}

/// 幂等：同一业务键重复入队只有一条任务，驱动只执行一次。
#[test]
fn the_same_business_key_is_enqueued_once_and_run_once() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());

    let spec = || {
        let mut job = NewJob::new("extract", JobPriority::BackgroundExtract)
            .targeting(vec!["rev_同一条".to_owned()])
            .with_snapshot("snapshot-1");
        job.max_attempts = 1;
        job
    };
    let first = core.enqueue_job(spec()).unwrap();
    let second = core.enqueue_job(spec()).unwrap();
    assert_eq!(first.id, second.id, "同业务键必须返回已有任务");
    assert_eq!(core.count_jobs(None).unwrap(), 1, "不能产生第二条任务");

    // 驱动只执行一次：如果入队产生了重复任务，processed 会是 2。
    let summary = core.run_due_jobs(Utc::now(), 60_000, "test", 10).unwrap();
    assert_eq!(summary.processed, 1, "驱动只执行一次");
    let after = core.get_job(&first.id).unwrap();
    assert_eq!(after.attempt_count, 1, "只尝试过一次");

    // 换了业务键就是另一条任务（快照不同）。
    let other = core
        .enqueue_job(
            NewJob::new("extract", JobPriority::BackgroundExtract)
                .targeting(vec!["rev_同一条".to_owned()])
                .with_snapshot("snapshot-2"),
        )
        .unwrap();
    assert_ne!(other.id, first.id);
    assert_eq!(core.count_jobs(None).unwrap(), 2);

    // 已经到终态（failed）之后，同一个键可以再排一条：终态不再参与幂等判断。
    let again = core
        .enqueue_job(
            NewJob::new("extract", JobPriority::BackgroundExtract)
                .targeting(vec!["rev_同一条".to_owned()])
                .with_snapshot("snapshot-1"),
        )
        .unwrap();
    assert_ne!(again.id, first.id, "终态任务不该挡住重新入队");
    assert_eq!(core.count_jobs(None).unwrap(), 3);
}

/// limit 语义：到期任务多于 limit 时只跑 limit 个，其余留在排队中，summary 如实。
#[test]
fn the_limit_caps_one_run_and_the_rest_stay_queued() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    for index in 0..5 {
        core.enqueue_job(
            NewJob::new("transcribe", JobPriority::Maintenance)
                .targeting(vec![format!("asset_{index}")]),
        )
        .unwrap();
    }

    let now = Utc::now();
    let summary = core.run_due_jobs(now, 60_000, "test", 2).unwrap();
    assert_eq!(summary.processed, 2, "只跑 limit 个");
    assert_eq!(summary.failed, 2);
    assert_eq!(summary.due_backlog, 3, "被截住的到期任务要如实报出来");
    assert_eq!(core.count_jobs(Some(&[JobState::Queued])).unwrap(), 3);
    assert_eq!(core.count_jobs(Some(&[JobState::Failed])).unwrap(), 2);
    assert!(
        summary.next_wakeup.is_some(),
        "还有排队任务就要给出下次唤醒"
    );

    // limit = 0 表示本次一个都不跑（预算是 0），积压照实报。
    let nothing = core.run_due_jobs(now, 60_000, "test", 0).unwrap();
    assert_eq!(nothing.processed, 0);
    assert_eq!(nothing.due_backlog, 3);
    assert_eq!(core.count_jobs(Some(&[JobState::Queued])).unwrap(), 3);

    // 再跑两次就把剩下的做完。
    assert_eq!(core.run_due_jobs(now, 60_000, "test", 10).unwrap().processed, 3);
    assert_eq!(core.run_due_jobs(now, 60_000, "test", 10).unwrap().processed, 0);
}

/// 毫秒预算：只在任务之间判断；`budget_ms = 0` 一条都不跑、如实报积压，
/// 没跑过的任务绝不算成 processed / failed；预算给够就能把积压跑完（不会丢）。
#[test]
fn the_time_budget_caps_a_run_without_lying_about_progress() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    for index in 0..5 {
        core.enqueue_job(
            NewJob::new("transcribe", JobPriority::Maintenance)
                .targeting(vec![format!("asset_{index}")]),
        )
        .unwrap();
    }

    let now = Utc::now();
    let nothing = core.run_due_jobs(now, 0, "startup", 64).unwrap();
    assert_eq!(nothing.processed, 0, "预算 0：一条都不跑");
    assert_eq!(nothing.succeeded, 0);
    assert_eq!(nothing.failed, 0, "没跑过的任务绝不能算成失败");
    assert_eq!(nothing.retrying, 0);
    assert_eq!(nothing.backfilled, 0);
    assert_eq!(nothing.due_backlog, 5, "被预算截住的到期任务要如实报出来");
    assert_eq!(nothing.waiting_dependency, 0);
    assert_eq!(nothing.started_at, now, "开始时刻就是调用方给的 now");
    assert_eq!(nothing.trigger, "startup", "唤醒来源如实记进摘要");
    assert_eq!(
        core.count_jobs(Some(&[JobState::Queued])).unwrap(),
        5,
        "预算截住的仍然留在排队中"
    );
    assert!(nothing.next_wakeup.is_some(), "还有活就该给出下次唤醒");

    // 预算给够：一轮跑完（条数上限不挡），一条都没丢。
    let all = core.run_due_jobs(now, 60_000, "foreground", 64).unwrap();
    assert_eq!(all.processed, 5);
    assert_eq!(all.succeeded, 0);
    assert_eq!(all.failed, 5, "transcribe 没有处理器 → 已放弃");
    assert_eq!(all.due_backlog, 0);
    assert_eq!(all.trigger, "foreground");
    assert_eq!(core.count_jobs(Some(&[JobState::Queued])).unwrap(), 0);
}

/// `job.changed` 在入队/开始/成功/失败/重试/取消时都发出。
#[test]
fn job_changed_is_emitted_on_every_step() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());

    // 导入排两条任务（extract + 等提取的 index），各发一次入队事件。
    let (_, source_id) = import_text(&mut core, b"plain text", "note.txt", "op1");
    assert_eq!(job_changed_count(&core), 2, "入队要发 job.changed");

    // 一次驱动把 extract → index 走完（闸门在领取前重评估，链在同一轮推进）：
    // 2 次「开始」+ 2 次「成功」+ 1 次「index 被拉起」= 5 次。
    let success = core.run_due_jobs(Utc::now(), 60_000, "test", 10).unwrap();
    assert_eq!(success.succeeded, 2, "extract 与 index 都要真的跑完");
    assert_eq!(success.waiting_dependency, 0);
    assert_eq!(
        job_changed_count(&core),
        7,
        "2 次开始 + 2 次成功 + index 拉起一次"
    );
    assert!(core.extracted_content(&source_id).unwrap().is_some());

    // 失败与重试路径。
    let mut job = NewJob::new("extract", JobPriority::BackgroundExtract)
        .targeting(vec!["rev_不存在".to_owned()]);
    job.max_attempts = 2;
    let queued = core.enqueue_job(job).unwrap();
    assert_eq!(job_changed_count(&core), 8);

    let now = Utc::now();
    core.run_due_jobs(now, 60_000, "test", 10).unwrap();
    assert_eq!(job_changed_count(&core), 10, "开始 + 失败退避各一次");
    core.run_due_jobs(now + Duration::minutes(10), 60_000, "test", 10).unwrap();
    assert_eq!(job_changed_count(&core), 12, "开始 + 已放弃各一次");

    core.retry_job(&queued.id).unwrap();
    assert_eq!(job_changed_count(&core), 13, "人工重试要发");

    core.cancel_job(&queued.id, "op-cancel").unwrap();
    assert_eq!(job_changed_count(&core), 14, "取消要发");
}

/// 上次没跑完（进程被杀）的任务：运行开始时回收，不会被永久跳过。
#[test]
fn an_interrupted_run_is_recovered_instead_of_being_lost() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    let (_, source_id) = import_text(&mut core, b"interrupted text", "note.txt", "op1");
    let queued = core.list_jobs(Some(&[JobState::Queued]), 10).unwrap().remove(0);

    // 手动领取一条任务后什么都不做：这正是「进程在任务进行中被杀」在库里留下的样子。
    let now = Utc::now();
    let claimed = core.claim_next_due_job(now).unwrap().unwrap();
    assert_eq!(claimed.state, JobState::Running);
    assert_eq!(claimed.attempt_count, 1);

    let summary = core.run_due_jobs(now, 60_000, "test", 10).unwrap();
    assert_eq!(summary.recovered, 1, "运行开始要回收没跑完的任务");
    assert_eq!(
        summary.processed, 2,
        "回收的提取跑完，闸门把导入排的索引任务在同一轮拉起跑掉"
    );
    assert_eq!(summary.succeeded, 2, "回收之后要真的跑完");
    let job = core.get_job(&queued.id).unwrap();
    assert_eq!(job.state, JobState::Succeeded);
    assert!(core.extracted_content(&source_id).unwrap().is_some());
}

/// 任务失败不影响本地优先：关键词检索、查看、保存新记录照常。
#[test]
fn local_first_features_keep_working_while_jobs_fail() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());

    let capture_id = draft(&mut core, "op-create");
    core.save_draft(&capture_id, "今天买了牛奶", 1, "op-save").unwrap();
    core.commit(&capture_id, 2, "op-commit").unwrap();

    // 一条必然失败的提取 + 一条没有处理器的转写。
    core.enqueue_job(
        NewJob::new("extract", JobPriority::BackgroundExtract)
            .targeting(vec!["rev_不存在".to_owned()]),
    )
    .unwrap();
    core.enqueue_job(
        NewJob::new("transcribe", JobPriority::BackgroundExtract)
            .targeting(vec!["asset_不存在".to_owned()]),
    )
    .unwrap();

    let summary = core.run_due_jobs(Utc::now(), 60_000, "test", 10).unwrap();
    assert_eq!(summary.processed, 2);
    assert_eq!(summary.succeeded, 0, "两条都该失败，不能粉饰");

    // 本地优先照旧：搜得到、看得到、还能继续写。
    // 走产品检索路径（`search.start`）：记录文字那一路不在 `count_search_matches`
    // 的口径里（那个只数派生片段）。
    let found = keyword_search(&mut core, "牛奶");
    assert!(
        !found.results.is_empty(),
        "用户自己写的文字照样能搜到"
    );
    let capture = core.get_capture(&capture_id).unwrap();
    assert_eq!(capture.draft_text, "今天买了牛奶");
    assert_eq!(capture.state, diary_core::CaptureState::Committed);

    let second = draft(&mut core, "op-create-2");
    core.save_draft(&second, "第二条记录", 1, "op-save-2").unwrap();
    assert_eq!(core.get_capture(&second).unwrap().draft_text, "第二条记录");
    assert!(
        !keyword_search(&mut core, "第二条").results.is_empty(),
        "新写的内容也要立刻能搜到"
    );
}
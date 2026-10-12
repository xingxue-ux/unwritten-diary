//! issue #70 的前半：任务依赖链闸门 + 关键词索引任务化。
//!
//! 覆盖五条**用真实调用**钉住的验收：
//! - 导入 → 提取 → 索引任务跑完 → 关键词一路可搜。**只调一次** `run_due_jobs`：
//!   闸门在**每次领取前**重评估，链在**一次**驱动调用里走完（评审 #74 指出的缺口）；
//! - 「材料还没提取完就请求索引」→ 落在 `waiting_dependency`、原因可见、不消耗重试额度；
//!   只要还留着等待态，摘要就必须给出**可用的** `nextWakeup`（不能是 null）；
//! - 被条数上限截住的路径仍可能需要两轮：第一轮摘要要给出可用的下一次唤醒时刻；
//! - 前置不可能再满足（提取被取消）→ 终态失败 + 明确错误码，不挂死、不假装成功；
//! - 不变式专测：有等待态 ⟺ 必须有可用的 `nextWakeup`（前置 `Broken` 时不留等待态）；
//! - #67 二轮复审的回归：排在未来到期的索引任务，前置提前完成时**不被提前执行**；
//!   等待期与晋升都保留原排期，`nextWakeup` 如实指向未来时刻；按时执行不计补做，
//!   排期已过才跑的仍计入补做。
//!
//! 只做**关键词**索引任务化；语义索引要等 #58 的向量接入合入（见 m4 文档）。

use std::path::Path;

use chrono::{Duration, Utc};
use diary_core::{
    Core, CreateDraftInput, ImportManifest, ImportOrigin, ImportRequest, JobPriority, JobState,
    NewJob, SearchFilters, SearchMode, SearchRequest,
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

/// 导入一段文本，返回 (capture_id, source_id)。
///
/// 导入完成时队列里已经排下两条任务：`extract`（对修订）与 `index`（对来源，
/// 初始落在 `waiting_dependency`）——这些用例不手动入队、不手动 claim/complete。
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
    core.finish_import(
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
    let source_id = core
        .get_capture(&capture_id)
        .unwrap()
        .ordered_source_ids
        .first()
        .cloned()
        .expect("材料应当挂到记录上");
    (capture_id, source_id)
}

/// 走产品检索路径的一次关键词查询（记录正文那一路也算在里面）。
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

fn waiting_index_jobs(core: &Core) -> Vec<diary_core::Job> {
    core.list_jobs(Some(&[JobState::WaitingDependency]), 20)
        .unwrap()
        .into_iter()
        .filter(|job| job.kind == "index")
        .collect()
}

/// 排期在库里以 ISO 文本存，精度是**毫秒**：用例里的期望值也截到毫秒才可比。
fn to_millis(at: chrono::DateTime<Utc>) -> chrono::DateTime<Utc> {
    chrono::DateTime::from_timestamp_millis(at.timestamp_millis()).expect("毫秒时间戳可还原")
}

/// 端到端链：导入 → 提取 → 索引任务跑完。**只调一次** `run_due_jobs`：
/// 闸门在每次领取前重评估，提取跑完的一刻索引任务就被拉起并在**同一轮**跑掉，
/// 不把「链能不能走完」寄托在平台层肯不肯再叫一次上。全程不手动 claim/complete。
#[test]
fn the_import_to_extract_to_index_chain_advances_without_manual_claiming() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    let (_, source_id) = import_text(
        &mut core,
        "第一段正文\n\n第二段正文".as_bytes(),
        "笔记.txt",
        "op-chain",
    );

    // 导入排下两条：extract 排队、index 在等它。
    let queued = core.list_jobs(Some(&[JobState::Queued]), 10).unwrap();
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0].kind, "extract");
    let waiting = waiting_index_jobs(&core);
    assert_eq!(waiting.len(), 1, "索引任务要落在等待态");
    assert_eq!(waiting[0].target_ids, vec![source_id.clone()]);
    assert_eq!(waiting[0].error_code.as_deref(), Some("waiting_extract"));
    assert_eq!(waiting[0].attempt_count, 0, "等前置不动尝试次数");

    // **只调一次**驱动：extract 跑完，闸门立刻认账并把 index 拉起、在同一轮跑完。
    let round = core.run_due_jobs(Utc::now(), 60_000, "foreground", 64).unwrap();
    assert_eq!(round.processed, 2, "一次调用要把 extract 与 index 都跑掉");
    assert_eq!(round.succeeded, 2);
    assert_eq!(round.waiting_dependency, 0, "链走完了就不该再留在等待态");
    assert_eq!(round.dependency_failed, 0);
    assert_eq!(round.due_backlog, 0);
    assert_eq!(round.next_wakeup, None, "没有待办了，下次唤醒如实为空");
    // 没跑的任务一条都没被谎报成跑过。
    assert_eq!(round.processed, round.succeeded + round.failed + round.retrying);

    let index_job = core.get_job(&waiting[0].id).unwrap();
    assert_eq!(
        index_job.state,
        JobState::Succeeded,
        "索引任务必须在这一次驱动调用里跑完"
    );
    assert!(index_job.error_code.is_none(), "前置满足后不该留着在等的原因");
    assert_eq!(index_job.attempt_count, 1, "只尝试过一次");

    // 关键词索引就绪（真实调用，不是伪造）。
    let after = keyword_search(&mut core, "正文");
    assert!(!after.results.is_empty(), "索引任务跑完后照样搜得到");
    assert!(
        core.count_search_matches("正文").unwrap() >= 1,
        "片段索引里要有内容"
    );
    assert!(
        core.index_status(Some(&[source_id]))
            .unwrap()
            .keyword_index_ready,
        "这条来源的关键词索引就绪"
    );
}

/// 在等前置不是失败：不消耗重试额度，原因在状态里看得见；前置一满足，
/// **同一次**驱动调用里就会被拉起并跑完。
#[test]
fn waiting_for_extraction_does_not_spend_attempts_and_is_visible() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    import_text(&mut core, b"waiting body", "note.txt", "op-wait");
    let waiting = waiting_index_jobs(&core);
    assert_eq!(waiting.len(), 1);

    // 预算为 0 的一轮：什么都不跑，但闸门照旧把等待态维持在真相上（不是失败）。
    let idle = core.run_due_jobs(Utc::now(), 0, "startup", 64).unwrap();
    assert_eq!(idle.processed, 0);
    assert_eq!(idle.waiting_dependency, 1);
    assert_eq!(idle.due_backlog, 1, "提取任务还在排队等跑");
    // 不变式：有等待态就必须给出可用的下一次唤醒（它等的是 queued 的 extract）。
    assert!(
        idle.next_wakeup.is_some(),
        "有等待态却报「没有待办」就是挂死，不能这样"
    );

    // 状态摘要里看得见「在等」，per-job 的原因码说清在等哪一环。
    let status = core.job_status().unwrap();
    assert_eq!(status.waiting_dependency, 1);
    assert_eq!(status.failed, 0, "等前置不是失败");
    let still_waiting = core.get_job(&waiting[0].id).unwrap();
    assert_eq!(still_waiting.state, JobState::WaitingDependency);
    assert_eq!(still_waiting.error_code.as_deref(), Some("waiting_extract"));
    assert_eq!(still_waiting.attempt_count, 0, "等了这么多轮也没动额度");
    assert!(!still_waiting.requires_user_action);

    // 一次驱动：提取跑完，闸门在领取前重评估，索引任务在同一轮被拉起并跑完。
    let round = core.run_due_jobs(Utc::now(), 60_000, "foreground", 64).unwrap();
    assert_eq!(round.succeeded, 2, "extract 与 index 在同一次调用里都跑完");
    assert_eq!(round.waiting_dependency, 0);
    let done = core.get_job(&waiting[0].id).unwrap();
    assert_eq!(done.state, JobState::Succeeded);
    assert_eq!(done.attempt_count, 1, "真正跑过一次，才记一次尝试");
}

/// 索引任务跑前跑后都搜得到；旧检索快照在索引换代后**如实失效**，
/// 而不是拿半截/过期索引假装还准。
///
/// 这里用 `max_jobs = 1` 造出「还需要两轮」的中间态：第一轮只跑提取，闸门在**收尾**
/// 时把索引任务从等待态推进到 `queued`（不是留在 `waiting_dependency`），摘要必须
/// 给出可用的下一次唤醒时刻；第二轮才真正跑索引任务。评审点名要的第 2 条断言
/// （需要两轮时第一轮 `nextWakeup` 非 null）就钉在这里。
#[test]
fn keyword_search_stays_available_across_the_index_job_and_old_snapshots_expire() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    import_text(&mut core, "重建前也能搜到的正文".as_bytes(), "note.txt", "op-live");
    let waiting = waiting_index_jobs(&core);
    assert_eq!(waiting.len(), 1);
    let index_id = waiting[0].id.clone();

    // 条数上限 = 1：这一轮只跑提取（索引任务被截在下一轮）。
    let truncated = core.run_due_jobs(Utc::now(), 60_000, "foreground", 1).unwrap();
    assert_eq!(truncated.processed, 1, "这一轮只跑一条");
    assert_eq!(truncated.succeeded, 1);
    assert_eq!(
        truncated.waiting_dependency, 0,
        "前置已满足，不能留在等待态（否则下一轮可能没人叫）"
    );
    assert!(
        truncated.next_wakeup.is_some(),
        "还有 queued 的索引任务没跑，第一次摘要必须给出可用唤醒时刻"
    );
    let promoted = core.get_job(&index_id).unwrap();
    assert_eq!(promoted.state, JobState::Queued, "被拉到 queued 等下一次领取");
    assert_eq!(promoted.attempt_count, 0, "被拉起不算跑过，不扣额度");

    // 提取与增量索引在同一个事务里落库：索引任务还没跑也搜得到，先发起一个检索会话。
    let session = keyword_search(&mut core, "正文");
    assert!(!session.results.is_empty());

    // 第二轮：索引任务（重建）跑完。
    let round = core.run_due_jobs(Utc::now(), 60_000, "foreground", 64).unwrap();
    assert_eq!(round.succeeded, 1);
    assert_eq!(round.next_wakeup, None, "没有待办了");
    assert_eq!(core.get_job(&index_id).unwrap().state, JobState::Succeeded);
    assert!(waiting_index_jobs(&core).is_empty());

    // 旧会话不假装还准：索引代次变了就报 search_expired，让前端重取。
    let expired = core.search_snapshot(&session.session_id);
    assert_eq!(
        expired.unwrap_err().code(),
        diary_core::ErrorCode::SearchExpired,
        "索引换代后旧快照必须如实失效"
    );

    // 重新发起检索：重建完的索引照样搜得到。
    let again = keyword_search(&mut core, "正文");
    assert!(!again.results.is_empty(), "重建之后索引仍然可用");
    let status = core.index_status(None).unwrap();
    assert!(status.keyword_index_ready, "整库关键词索引就绪");
}

/// 前置不可能再满足（提取被取消）：终态失败 + 明确错误码，不挂死也不假装成功。
#[test]
fn an_index_job_whose_extraction_can_never_happen_fails_with_a_clear_code() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    import_text(&mut core, b"never extracted", "note.txt", "op-broken");
    let waiting = waiting_index_jobs(&core);
    assert_eq!(waiting.len(), 1);

    // 用户把那条提取任务取消掉：派生内容永远不会出现，也就没有「将来的执行者」了。
    let extract = core
        .list_jobs(Some(&[JobState::Queued]), 10)
        .unwrap()
        .into_iter()
        .find(|job| job.kind == "extract")
        .expect("导入要排一条提取任务");
    core.cancel_job(&extract.id, "op-cancel-extract").unwrap();

    let round = core.run_due_jobs(Utc::now(), 60_000, "foreground", 64).unwrap();
    assert_eq!(round.processed, 0, "闸门拦下的任务一行执行者都没跑");
    assert_eq!(round.failed, 0, "没跑过就不能算进「跑过的失败」");
    assert_eq!(round.dependency_failed, 1, "闸门的终态失败要如实报数");
    assert_eq!(round.waiting_dependency, 0);

    let job = core.get_job(&waiting[0].id).unwrap();
    assert_eq!(job.state, JobState::Failed);
    assert_eq!(
        job.error_code.as_deref(),
        Some("dependency_extract_unmet"),
        "错误码要说清是「extract 这一环等不到了」"
    );
    assert!(job.requires_user_action, "要看得见、要能人工处理");
    assert_eq!(job.attempt_count, 0, "没有执行者跑过，就不该记尝试次数");
    assert!(job.next_attempt_at.is_none(), "没有退避风暴");

    // 状态摘要看得到它（kind + 错误码 + 时间，没有用户内容）。
    let status = core.job_status().unwrap();
    assert_eq!(status.failed, 1);
    assert_eq!(status.waiting_dependency, 0);
    let error = status
        .recent_errors
        .first()
        .expect("终态失败要出现在最近错误里");
    assert_eq!(error.job_id, waiting[0].id);
    assert_eq!(error.kind, "index");
    assert_eq!(error.error_code, "dependency_extract_unmet");
    assert!(error.requires_user_action);

    // 下次唤醒不该被这条挂死任务拖住。
    assert!(round.next_wakeup.is_none());
}

/// 不变式（评审 #74 点名要钉的那条）：一轮结束后**还留着等待态**，摘要就必须给出
/// 可用的下一次唤醒时刻——不允许 `nextWakeup = None` 却留着等待态，那会让按摘要
/// 安排唤醒的平台层不再叫驱动，链就停住了。等待态只可能等在 `queued`/`retry_wait`
/// 的前置上；前置判 `Broken` 时本轮已把它终态失败，不会留下等待态。
#[test]
fn a_run_never_reports_no_wakeup_while_jobs_are_waiting() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    import_text(&mut core, b"still waiting", "note.txt", "op-invariant");

    // 预算 0：什么都不跑，闸门把 index 留在等待态（它等的是 queued 的 extract）。
    let idle = core.run_due_jobs(Utc::now(), 0, "startup", 64).unwrap();
    assert_eq!(idle.waiting_dependency, 1);
    assert!(
        idle.next_wakeup.is_some(),
        "有等待态就必须有可用的下一次唤醒"
    );

    // 任务面板摘要同一条不变式（`job_status` 走同一个 `next_wakeup`）。
    let status = core.job_status().unwrap();
    assert_eq!(status.waiting_dependency, 1);
    assert!(status.next_wakeup.is_some());

    // 前置被判 `Broken`（提取被取消）：不留等待态，也就不需要「下一次唤醒」；
    // 「等待态」与「nextWakeup = None」不会同时出现。
    let extract = core
        .list_jobs(Some(&[JobState::Queued]), 10)
        .unwrap()
        .into_iter()
        .find(|job| job.kind == "extract")
        .expect("导入要排一条提取任务");
    core.cancel_job(&extract.id, "op-cancel-invariant").unwrap();
    let broken = core.run_due_jobs(Utc::now(), 0, "startup", 64).unwrap();
    assert_eq!(broken.waiting_dependency, 0, "前置不可能满足就终态失败");
    assert_eq!(broken.dependency_failed, 1);
    assert!(broken.next_wakeup.is_none(), "既没有等待态也没有待办，才允许为空");
}

/// #67 二轮复审的回归（评审点名的场景）：排一条「一小时后执行」的索引任务，此时
/// 前置（提取）还没完成；跑驱动让提取**提前完成**，那条索引任务**绝不能被提前执行**。
///
/// 修法后的三处口径一起钉在这里：
/// - **等待期**（`defer_for_dependency`）与**晋升**（`promote_dependency`）都保留
///   原 `next_attempt_at`，所以「依赖刚满足但排期还在未来」的任务停在
///   `queued` + 未来排期，既有的领取判据 `next_attempt_at IS NULL OR <= now`
///   自然拦住它（不需要另加状态）；
/// - **领取**（`claim_next_due_record`）必须「依赖已满足」与「已到期」同时成立；
/// - 排期不丢：它作为 `queued` 任务进 `next_wakeup`，摘要与状态如实给出那个未来时刻；
/// - 补做不被污染：按时（`due_at == now`）被领走**不算**补做。
#[test]
fn an_index_job_due_in_the_future_is_not_run_early_when_its_extract_finishes_first() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    let (_, source_id) = import_text(&mut core, b"scheduled body", "note.txt", "op-future");

    let now = Utc::now();
    let due = to_millis(now + Duration::hours(1));

    // 导入顺带排的那条来源索引任务先取消掉，好让这次观察只落在「一小时后」的那条上。
    let auto = waiting_index_jobs(&core)
        .into_iter()
        .next()
        .expect("导入要排一条在等的索引任务");
    core.cancel_job(&auto.id, "op-cancel-auto-index").unwrap();

    // 一条**排在一小时后**的索引任务；此刻前置（提取）还没跑。
    let scheduled = core
        .enqueue_job(
            NewJob::new("index", JobPriority::Maintenance)
                .targeting(vec![source_id.clone()])
                .scheduled_at(due),
        )
        .unwrap();
    assert_eq!(scheduled.next_attempt_at, Some(due), "排期要真的落库");
    assert_eq!(scheduled.state, JobState::Queued);

    // 跑一轮：提取提前完成，闸门把它认账并晋升——但排期未到，不能领它。
    let first = core.run_due_jobs(now, 60_000, "foreground", 64).unwrap();

    // 断言 1：本轮只跑了提取；排期在一小时后的索引任务没有被提前执行。
    assert_eq!(
        first.processed, 1,
        "本轮只该跑提取：排期未到的索引任务不能被领走"
    );
    assert_eq!(first.succeeded, 1);
    assert_eq!(first.backfilled, 0, "提取是入队即到期，不算补做");
    assert_eq!(first.waiting_dependency, 0, "前置已满足，不再留在等待态");
    assert_eq!(first.due_backlog, 0, "排期还在未来，就不算「到期积压」");

    let promoted = core.get_job(&scheduled.id).unwrap();
    assert_eq!(
        promoted.state,
        JobState::Queued,
        "前置满足后回到 queued，等排期到才跑"
    );
    assert_eq!(
        promoted.next_attempt_at,
        Some(due),
        "晋升不能丢掉原计划时间（否则会被立刻领走）"
    );
    assert!(promoted.error_code.is_none(), "前置满足后不该留着在等的原因");
    assert_eq!(promoted.attempt_count, 0, "没被提前领走，就不该记尝试");

    // 断言 3：到期时间没丢——摘要与状态给出的下次唤醒就是那个未来时刻。
    assert_eq!(
        first.next_wakeup,
        Some(due),
        "晋升后唤醒时刻要如实指向原排期，不是 None、也不是「现在」"
    );
    assert_eq!(core.next_wakeup().unwrap(), Some(due));
    assert_eq!(core.job_status().unwrap().next_wakeup, Some(due));

    // 断言 2 / 4 的另一半：把 now 推到排期那一刻（按时）才跑，它被执行，且不计补做。
    let on_time = core.run_due_jobs(due, 60_000, "foreground", 64).unwrap();
    assert_eq!(on_time.processed, 1, "到点了就该跑");
    assert_eq!(on_time.succeeded, 1);
    assert_eq!(
        on_time.backfilled, 0,
        "按时执行（领取时排期未过）不能算成补做"
    );
    assert_eq!(on_time.next_wakeup, None, "跑完没有待办了");
    let done = core.get_job(&scheduled.id).unwrap();
    assert_eq!(done.state, JobState::Succeeded);
    assert_eq!(done.attempt_count, 1, "真正跑过一次才记一次尝试");
    assert!(
        core.index_status(Some(std::slice::from_ref(&source_id)))
            .unwrap()
            .keyword_index_ready,
        "到点跑完，这条来源的关键词索引就绪"
    );
}

/// 断言 4 的另一半：**排期已过、驱动当时没跑、后来才跑**的情形**仍计** `backfilled`。
///
/// 与上一条用例同一条代码路径（等待期保留排期），只是排期在过去：前置完成、晋升回
/// `queued` 时它已经到期，于是**同一轮**被领走，领取时排期已过 → 如实算作补做。
/// 两条方向都钉住，避免「一改保留排期，就把补做统计钉死成 0」。
#[test]
fn an_index_job_whose_scheduled_time_already_passed_is_still_backfilled() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    let (_, source_id) = import_text(&mut core, b"overdue body", "note.txt", "op-overdue");

    let now = Utc::now();
    let due = now - Duration::hours(2);

    let auto = waiting_index_jobs(&core)
        .into_iter()
        .next()
        .expect("导入要排一条在等的索引任务");
    core.cancel_job(&auto.id, "op-cancel-auto-overdue").unwrap();

    // 排期在两小时前：本来早该跑，但等前置等到了现在。
    let scheduled = core
        .enqueue_job(
            NewJob::new("index", JobPriority::Maintenance)
                .targeting(vec![source_id.clone()])
                .scheduled_at(due),
        )
        .unwrap();
    assert!(scheduled.next_attempt_at.expect("排期要落库") < now);

    // 提取跑完 → 晋升回 queued（排期已过）→ 同一轮就能领走执行。
    let round = core.run_due_jobs(now, 60_000, "foreground", 64).unwrap();
    assert_eq!(round.processed, 2, "提取与到期的索引任务都跑掉");
    assert_eq!(round.succeeded, 2);
    assert_eq!(
        round.backfilled, 1,
        "排期已过、这次才跑上：必须如实算作补做（提取那条入队即到期，不算）"
    );
    assert_eq!(core.get_job(&scheduled.id).unwrap().state, JobState::Succeeded);
    assert!(round.next_wakeup.is_none(), "没有待办了");
}
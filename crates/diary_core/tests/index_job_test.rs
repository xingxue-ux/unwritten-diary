//! issue #70 的前半：任务依赖链闸门 + 关键词索引任务化。
//!
//! 覆盖三条**用真实调用**钉住的验收：
//! - 导入 → 提取 → 索引任务跑完 → 关键词一路可搜（一条链走完，不手动 claim/complete）；
//! - 「材料还没提取完就请求索引」→ 落在 `waiting_dependency`、原因可见、不消耗重试额度，
//!   前置一满足就在下一轮被拉起；
//! - 前置不可能再满足（提取被取消）→ 终态失败 + 明确错误码，不挂死、不假装成功。
//!
//! 只做**关键词**索引任务化；语义索引要等 #58 的向量接入合入（见 m4 文档）。

use std::path::Path;

use chrono::Utc;
use diary_core::{
    Core, CreateDraftInput, ImportManifest, ImportOrigin, ImportRequest, JobState, SearchFilters,
    SearchMode, SearchRequest,
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

/// 端到端链：导入 → 提取 → 索引任务跑完，全程走驱动（不手动 claim/complete）。
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

    // 第一轮：只推进提取，索引任务仍然在等（一轮推进一层依赖）。
    let first = core.run_due_jobs(Utc::now(), 60_000, "foreground", 64).unwrap();
    assert_eq!(first.processed, 1, "这一轮只跑提取");
    assert_eq!(first.succeeded, 1);
    assert_eq!(first.waiting_dependency, 1, "索引任务如实算在等待里");
    assert_eq!(first.dependency_failed, 0);

    // **索引任务还没跑完**，关键词一路照旧可用（提取与建索引在同一个事务里落库）。
    let early = keyword_search(&mut core, "正文");
    assert!(!early.results.is_empty(), "索引任务没跑完也要搜得到");

    // 第二轮：前置已满足，索引任务被拉起并跑完。
    let second = core.run_due_jobs(Utc::now(), 60_000, "foreground", 64).unwrap();
    assert_eq!(second.processed, 1, "这一轮跑索引任务");
    assert_eq!(second.succeeded, 1);
    assert_eq!(second.waiting_dependency, 0);
    assert_eq!(second.due_backlog, 0);

    let index_job = core.get_job(&waiting[0].id).unwrap();
    assert_eq!(index_job.state, JobState::Succeeded);
    assert!(index_job.error_code.is_none(), "前置满足后不该留着在等的原因");
    assert_eq!(index_job.attempt_count, 1, "只尝试过一次");

    // 索引修完之后关键词仍然可搜（真实调用，不是伪造）。
    let after = keyword_search(&mut core, "正文");
    assert!(!after.results.is_empty(), "索引任务跑完后照样搜得到");
    assert!(
        core.count_search_matches("正文").unwrap() >= 1,
        "片段索引里要有内容"
    );

    // 没跑的任务一条都没被谎报成跑过。
    assert_eq!(
        second.processed,
        second.succeeded + second.failed + second.retrying
    );
}

/// 在等前置不是失败：不消耗重试额度，原因在状态里看得见，前置一满足下一轮就跑。
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

    // 状态摘要里看得见「在等」，per-job 的原因码说清在等哪一环。
    let status = core.job_status().unwrap();
    assert_eq!(status.waiting_dependency, 1);
    assert_eq!(status.failed, 0, "等前置不是失败");
    let still_waiting = core.get_job(&waiting[0].id).unwrap();
    assert_eq!(still_waiting.state, JobState::WaitingDependency);
    assert_eq!(still_waiting.error_code.as_deref(), Some("waiting_extract"));
    assert_eq!(still_waiting.attempt_count, 0, "等了这么多轮也没动额度");
    assert!(!still_waiting.requires_user_action);

    // 下一轮：提取跑完，索引任务仍在等（一轮一层）。
    let extract_round = core.run_due_jobs(Utc::now(), 60_000, "foreground", 64).unwrap();
    assert_eq!(extract_round.succeeded, 1);
    assert_eq!(extract_round.waiting_dependency, 1);
    assert_eq!(
        core.get_job(&waiting[0].id).unwrap().attempt_count,
        0,
        "提取跑完这一轮，索引任务还是没被扣额度"
    );

    // 再下一轮：前置满足 → 被拉起并跑完。
    let index_round = core.run_due_jobs(Utc::now(), 60_000, "foreground", 64).unwrap();
    assert_eq!(index_round.succeeded, 1);
    assert_eq!(index_round.waiting_dependency, 0);
    let done = core.get_job(&waiting[0].id).unwrap();
    assert_eq!(done.state, JobState::Succeeded);
    assert_eq!(done.attempt_count, 1, "真正跑过一次，才记一次尝试");
}

/// 索引任务跑前跑后都搜得到；旧检索快照在索引换代后**如实失效**，
/// 而不是拿半截/过期索引假装还准（这就是「重建期间不停服」的边界）。
#[test]
fn keyword_search_stays_available_across_the_index_job_and_old_snapshots_expire() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    import_text(&mut core, "重建前也能搜到的正文".as_bytes(), "note.txt", "op-live");

    // 提取先跑（索引任务还没跑），此时关键词可用，并发起一个检索会话。
    core.run_due_jobs(Utc::now(), 60_000, "foreground", 64).unwrap();
    let session = keyword_search(&mut core, "正文");
    assert!(!session.results.is_empty());
    assert_eq!(
        waiting_index_jobs(&core).len(),
        1,
        "这一刻索引任务还没跑"
    );

    // 索引任务跑完（重建）。
    let round = core.run_due_jobs(Utc::now(), 60_000, "foreground", 64).unwrap();
    assert_eq!(round.succeeded, 1);
    assert_eq!(waiting_index_jobs(&core).len(), 0);

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
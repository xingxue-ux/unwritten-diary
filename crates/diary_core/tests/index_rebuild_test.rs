//! issue #70 后半：`indexes.rebuild(scope, reason, operationId)` 的入口与 `scope` 编码。
//!
//! 覆盖：
//! - `scope` 编码的规范面：`all`（整库）、单个来源、多个来源、顺序无语义、空列表、
//!   重复 id、越界字符、不存在的来源——每条都有明确错误码；
//! - 幂等：同一个 `(scope, 原因, operationId)` 只排一条任务（任务完成后再调也是同一条），
//!   同一个 operationId 换了 scope / 原因报 `idempotency_conflict`；
//! - 桥接前的端到端：整库重建只排任务（异步），驱动跑完任务后 `index_status` 就绪，
//!   整条链不手动 claim/complete；
//! - 指定来源的重建在提取未完成时落 `waiting_dependency`，提取一完成就被拉起
//!   （闸门每次领取前重评估，链在同一次驱动调用里走完）；
//! - 整库重建跑前跑后关键词都可用，旧检索快照如实失效（沿用 #74 的口径，真实调用）。

use std::path::Path;

use diary_core::{
    Core, CreateDraftInput, ErrorCode, ImportManifest, ImportOrigin, ImportRequest, JobState,
    SearchFilters, SearchMode, SearchRequest,
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

/// 导入一段文本，返回来源 id。导入完成时队列里已经有 extract + 等提取的 index。
fn import_text(core: &mut Core, text: &[u8], name: &str, operation_id: &str) -> String {
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
    core.get_capture(&capture_id)
        .unwrap()
        .ordered_source_ids
        .first()
        .cloned()
        .expect("材料应当挂到记录上")
}

fn keyword_hits(core: &mut Core, query: &str) -> usize {
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
    .results
    .len()
}

fn index_jobs(core: &Core) -> Vec<diary_core::Job> {
    core.list_jobs(None, 50)
        .unwrap()
        .into_iter()
        .filter(|job| job.kind == "index")
        .collect()
}

/// 跑多轮驱动直到没有到期任务；返回跑过的轮数。
fn drive_until_idle(core: &mut Core) -> usize {
    for round in 0..8 {
        let summary = core
            .run_due_jobs(chrono::Utc::now(), 60_000, "foreground", 64)
            .unwrap();
        if summary.due_backlog == 0 && summary.waiting_dependency == 0 {
            return round + 1;
        }
    }
    panic!("驱动跑了 8 轮还有到期或等待的任务，链没走完");
}

#[test]
fn scope_all_is_a_whole_library_job_with_no_targets() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());

    let job = core.schedule_index_rebuild("all", "manual", "op-all").unwrap();
    assert_eq!(job.kind, "index");
    assert!(job.target_ids.is_empty(), "整库用空目标表示");
    assert_eq!(job.state, JobState::Queued);
    assert_eq!(job.priority, 10, "整库重建是最低优先级的维护任务");
    assert!(
        job.input_snapshot_hash.is_some(),
        "任务要带冻结输入的快照哈希"
    );
    assert_eq!(job.attempt_count, 0);
}

#[test]
fn scope_source_lists_are_normalized_and_order_is_not_semantic() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    let a = import_text(&mut core, b"first", "a.txt", "op-a");
    let b = import_text(&mut core, b"second", "b.txt", "op-b");

    let single = core
        .schedule_index_rebuild(&format!("sources:{a}"), "manual", "op-single")
        .unwrap();
    assert_eq!(single.target_ids, vec![a.clone()]);

    let multi = core
        .schedule_index_rebuild(&format!("sources:{b},{a}"), "manual", "op-multi")
        .unwrap();
    assert_eq!(
        multi.target_ids,
        vec![a.clone(), b.clone()],
        "顺序无语义：解析时排成规范形"
    );

    // 换顺序但同一个 operationId：是**同一次操作**，命中同一条任务。
    let reordered = core
        .schedule_index_rebuild(&format!("sources:{a},{b}"), "manual", "op-multi")
        .unwrap();
    assert_eq!(reordered.id, multi.id, "顺序不同的等价 scope 命中同一条任务");
}

#[test]
fn malformed_scopes_are_rejected_with_invalid_argument() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    let a = import_text(&mut core, b"body", "a.txt", "op-a");

    let cases = [
        String::new(),
        "ALL".to_owned(),
        "all ".to_owned(),
        " all".to_owned(),
        "sources".to_owned(),
        "sources:".to_owned(),
        format!("sources:{a},,{a}"),
        format!("sources:{a},{a}"),
        "sources:src has space".to_owned(),
        "sources:src/slash".to_owned(),
        a.clone(), // 忘了 sources: 前缀
    ];
    for (index, scope) in cases.iter().enumerate() {
        let error = core
            .schedule_index_rebuild(scope, "manual", &format!("op-bad-{index}"))
            .unwrap_err();
        assert_eq!(
            error.code(),
            ErrorCode::InvalidArgument,
            "scope「{scope}」应报 invalid_argument"
        );
    }

    // 非法输入一条任务都不该排下去。
    let jobs = index_jobs(&core);
    assert_eq!(jobs.len(), 1, "只该有导入排下的那条来源索引任务");
    assert_eq!(jobs[0].target_ids, vec![a]);
}

#[test]
fn an_unknown_source_is_not_found_and_no_job_is_queued() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());

    let missing = "src_00000000000000000000000000000000";
    let error = core
        .schedule_index_rebuild(&format!("sources:{missing}"), "manual", "op-missing")
        .unwrap_err();
    assert_eq!(error.code(), ErrorCode::NotFound);
    assert!(
        index_jobs(&core).is_empty(),
        "不排一条注定 not_found 终态失败的任务"
    );
}

#[test]
fn the_same_operation_is_idempotent_even_after_it_finishes() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    let source = import_text(&mut core, b"idempotent body", "note.txt", "op-imp");

    let first = core
        .schedule_index_rebuild("all", "manual", "op-rebuild")
        .unwrap();
    let second = core
        .schedule_index_rebuild("all", "manual", "op-rebuild")
        .unwrap();
    assert_eq!(second.id, first.id, "重复调用返回同一条任务");
    let all_jobs: Vec<_> = index_jobs(&core)
        .into_iter()
        .filter(|job| job.target_ids.is_empty())
        .collect();
    assert_eq!(all_jobs.len(), 1, "重复调用不产生第二条任务");

    // 任务跑完之后再调同一个 operationId：还是同一条（不重新排）。
    let done = drive_until_idle(&mut core);
    assert!(done >= 1);
    assert_eq!(
        core.get_job(&first.id).unwrap().state,
        JobState::Succeeded,
        "整库重建任务要真的被驱动跑完"
    );
    let replay = core
        .schedule_index_rebuild("all", "manual", "op-rebuild")
        .unwrap();
    assert_eq!(replay.id, first.id, "重放返回原任务");
    assert_eq!(
        replay.state,
        JobState::Succeeded,
        "重放读的是任务此刻的状态，不是入队那一刻的旧快照"
    );
    assert_eq!(
        index_jobs(&core)
            .into_iter()
            .filter(|job| job.target_ids.is_empty())
            .count(),
        1,
        "重放不能再排一条"
    );

    // 同一个 operationId 换了内容（scope 或原因）→ 幂等冲突。
    let changed_scope = core
        .schedule_index_rebuild(&format!("sources:{source}"), "manual", "op-rebuild")
        .unwrap_err();
    assert_eq!(changed_scope.code(), ErrorCode::IdempotencyConflict);
    let changed_reason = core
        .schedule_index_rebuild("all", "因为改过正文", "op-rebuild")
        .unwrap_err();
    assert_eq!(changed_reason.code(), ErrorCode::IdempotencyConflict);
}

#[test]
fn a_whole_library_rebuild_job_advances_the_chain_to_a_ready_index() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    let source = import_text(
        &mut core,
        "第一段正文\n\n第二段正文".as_bytes(),
        "note.txt",
        "op-chain",
    );

    // 入口只排任务，不同步重建：返回的那一刻索引任务还没跑（材料也还没提取）。
    let job = core
        .schedule_index_rebuild("all", "因为改过正文", "op-rebuild")
        .unwrap();
    assert_eq!(job.state, JobState::Queued);
    assert!(job.target_ids.is_empty());
    assert_eq!(
        keyword_hits(&mut core, "正文"),
        0,
        "提取还没跑，正文还没进索引"
    );

    // 一整库任务的前置是「没有活着的 extract」：预算 0 的一轮只跑闸门，它落等待态，
    // 原因说清在等哪一环，且不消耗额度（提取这一环确实还排着队）。
    core.run_due_jobs(chrono::Utc::now(), 0, "manual", 64)
        .unwrap();
    let waiting = core.get_job(&job.id).unwrap();
    assert_eq!(waiting.state, JobState::WaitingDependency);
    assert_eq!(waiting.error_code.as_deref(), Some("waiting_extract"));
    assert_eq!(waiting.attempt_count, 0, "等前置不是失败，不扣额度");
    assert!(!waiting.requires_user_action);

    let rounds = drive_until_idle(&mut core);
    assert_eq!(
        rounds, 1,
        "闸门在每次领取前重评估：extract → index 一条链在一次驱动调用里走完"
    );

    let done = core.get_job(&job.id).unwrap();
    assert_eq!(done.state, JobState::Succeeded);
    assert!(done.error_code.is_none());

    // 关键词仍然可搜，覆盖状态说就绪。
    assert!(keyword_hits(&mut core, "正文") > 0);
    let status = core.index_status(None).unwrap();
    assert!(status.keyword_index_ready, "整库关键词索引就绪");
    assert!(
        core.index_status(Some(&[source])).unwrap().keyword_index_ready,
        "这条来源的覆盖也报就绪"
    );

    // 没有任何任务被谎报成跑过。
    let failed = core.count_jobs(Some(&[JobState::Failed])).unwrap();
    assert_eq!(failed, 0, "这条链上没有失败任务");
}

#[test]
fn a_source_scoped_rebuild_waits_for_extraction_before_running() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    let source = import_text(&mut core, b"needs extraction", "note.txt", "op-wait");

    let job = core
        .schedule_index_rebuild(&format!("sources:{source}"), "manual", "op-scoped")
        .unwrap();
    assert_eq!(job.target_ids, vec![source.clone()]);

    // 预算为 0 的一轮只跑闸门：前置（提取）还没满足 → 落等待态，不消耗额度。
    let idle = core
        .run_due_jobs(chrono::Utc::now(), 0, "manual", 64)
        .unwrap();
    assert_eq!(idle.processed, 0, "预算 0 一条都不跑");
    let waiting = core.get_job(&job.id).unwrap();
    assert_eq!(waiting.state, JobState::WaitingDependency);
    assert_eq!(waiting.error_code.as_deref(), Some("waiting_extract"));
    assert_eq!(waiting.attempt_count, 0, "等前置不是失败，不扣额度");
    assert!(!waiting.requires_user_action);
    // 不变式：有等待态就必须给出可用的下一次唤醒。
    assert!(idle.next_wakeup.is_some(), "等在 queued 的提取上，唤醒时刻必须可用");

    // 条数上限 = 1：这一轮只跑提取。闸门在**收尾时**把后继推进到 queued
    //（不是留在 waiting_dependency），摘要给出可用的下次唤醒——「还需要两轮」的
    // 路径也必须诚实，不能是 null 却留着等待态。
    let extracted = core
        .run_due_jobs(chrono::Utc::now(), 60_000, "foreground", 1)
        .unwrap();
    assert_eq!(extracted.processed, 1, "条数上限只让这一轮跑一条");
    assert_eq!(extracted.succeeded, 1);
    assert_eq!(
        extracted.waiting_dependency, 0,
        "前置已满足，不能留在等待态"
    );
    assert!(extracted.next_wakeup.is_some(), "还有 queued 的索引任务");
    let promoted = core.get_job(&job.id).unwrap();
    assert_eq!(promoted.state, JobState::Queued, "被拉到 queued 等下次领取");
    assert_eq!(promoted.attempt_count, 0, "被拉起不算跑过，不扣额度");

    // 下一次驱动把排下的索引任务跑完（导入自己排的来源索引任务也一并跑掉）。
    let indexed = core
        .run_due_jobs(chrono::Utc::now(), 60_000, "foreground", 64)
        .unwrap();
    assert_eq!(
        indexed.succeeded, 2,
        "手工排的指定来源任务 + 导入排的来源索引任务都在这一次里跑完"
    );
    let done = core.get_job(&job.id).unwrap();
    assert_eq!(done.state, JobState::Succeeded);
    assert_eq!(done.attempt_count, 1, "真正跑过一次才记一次尝试");
    assert!(
        core.index_status(Some(&[source]))
            .unwrap()
            .keyword_index_ready
    );
}

#[test]
fn keyword_search_stays_available_across_a_whole_library_rebuild() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    import_text(&mut core, "重建前也能搜到的正文".as_bytes(), "note.txt", "op-live");

    // 先提取（导入排的索引任务还没跑），此时发起一个检索会话。
    core.run_due_jobs(chrono::Utc::now(), 60_000, "foreground", 64)
        .unwrap();
    let session = core
        .start_search(
            SearchRequest {
                query: "正文".to_owned(),
                mode: SearchMode::Keyword,
                filters: SearchFilters::default(),
                page_size: 20,
            },
            1,
        )
        .unwrap();
    assert!(!session.results.is_empty(), "索引任务没跑完也搜得到");

    // 排一条整库重建并跑掉它。
    let job = core
        .schedule_index_rebuild("all", "manual", "op-live-all")
        .unwrap();
    drive_until_idle(&mut core);
    assert_eq!(core.get_job(&job.id).unwrap().state, JobState::Succeeded);

    // 旧会话不假装还准：索引代次变了就报 search_expired，让前端重取。
    let expired = core.search_snapshot(&session.session_id);
    assert_eq!(
        expired.unwrap_err().code(),
        ErrorCode::SearchExpired,
        "整库重建换代后旧快照必须如实失效"
    );

    // 重新检索：重建完的索引照样搜得到。
    assert!(keyword_hits(&mut core, "正文") > 0);
    assert!(core.index_status(None).unwrap().keyword_index_ready);
}
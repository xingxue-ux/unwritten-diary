//! #146：`Capture.processing_summary` 四个桶的口径回归。
//!
//! 这些用例只走产品路径（提交 / 导入 / 驱动 / 取消 / 确认），不直接改库：
//! - 空记录（刚建的草稿）全 0 是**唯一**合法的全 0；
//! - 有来源、没任务、也没可检索正文 ⇒ `unprocessed`；
//! - extract + index 两条活任务（含 `waiting_dependency`）⇒ `processing`；
//! - 驱动把提取与索引跑完 ⇒ `searchable`；
//! - 终态失败 ⇒ 不算 `processing`，仍算 `unprocessed`（**不是做完了**）；
//! - `requires_user_action` 的任务 ⇒ `needs_attention`，`attention_acks` 之后归零。
//!
//! 口径写死在 `crates/diary_core/src/processing.rs` 与
//! `docs/development/01-共享接口契约.md` 2.1 节；改口径时两处与本文件一起改。

use std::path::Path;

use chrono::Utc;
use diary_core::{
    Core, CreateDraftInput, ImportManifest, ImportOrigin, ImportRequest, JobPriority, JobState,
    NewJob, ProcessingStatus, ProcessingSummary,
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

/// 往一条已有记录里导入一个文件，返回 (source_id, asset_id)。
fn import_into(
    core: &mut Core,
    capture_id: &str,
    bytes: &[u8],
    name: &str,
    mime: &str,
    operation_id: &str,
) -> (String, String) {
    let ticket = core
        .prepare_import(ImportRequest {
            capture_id,
            display_name: name,
            mime_hint: Some(mime),
            size_hint: Some(bytes.len() as i64),
            origin: ImportOrigin::Picker,
            operation_id: &format!("{operation_id}-prepare"),
        })
        .unwrap();
    std::fs::write(&ticket.staging_ticket, bytes).unwrap();
    let status = core
        .finish_import(
            &ticket.import_id,
            &ticket.staging_ticket,
            ImportManifest {
                copied_bytes: bytes.len() as i64,
                sha256: sha_of(bytes),
                detected_mime: mime.to_owned(),
                original_name: name.to_owned(),
            },
        )
        .unwrap();
    let asset_id = status.asset_id.expect("导入完成后应当有资产");
    let source_id = core
        .get_capture(capture_id)
        .unwrap()
        .ordered_source_ids
        .last()
        .cloned()
        .expect("材料应当挂到记录上");
    (source_id, asset_id)
}

fn import_text(
    core: &mut Core,
    capture_id: &str,
    text: &[u8],
    name: &str,
    operation_id: &str,
) -> (String, String) {
    import_into(core, capture_id, text, name, "text/plain", operation_id)
}

fn summary(core: &Core, capture_id: &str) -> ProcessingSummary {
    core.get_capture(capture_id).unwrap().processing_summary
}

/// 把队列里还活着的任务全部取消。用来造「有来源、没任务」的现场：
/// 任务记录丢了 / 被用户取消之后，来源不能因为「没人处理」就看起来正常。
fn cancel_open_jobs(core: &mut Core) {
    for job in core.list_jobs(None, 200).unwrap() {
        if !job.state.is_terminal() {
            core.cancel_job(&job.id, &format!("op-cancel-{}", job.id))
                .unwrap();
        }
    }
}

#[test]
fn draft_without_sources_is_the_only_legal_zero() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    let capture_id = draft(&mut core, "op-create");

    let capture = core.get_capture(&capture_id).unwrap();
    assert!(capture.ordered_source_ids.is_empty());
    assert_eq!(capture.processing_summary, ProcessingSummary::default());

    let page = core.list_captures(None, None, 10).unwrap();
    assert_eq!(
        page.captures[0].processing_summary,
        ProcessingSummary::default(),
        "列表路径也不能给空记录编数字"
    );
}

#[test]
fn committed_text_is_searchable_and_the_commit_result_is_not_stale() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    let capture_id = draft(&mut core, "op-create");
    core.save_draft(&capture_id, "今天风很大。", 1, "op-save")
        .unwrap();

    let committed = core.commit(&capture_id, 2, "op-commit").unwrap();
    // 提交返回值也是 Capture：它必须和重新读出来的记录一致，不能是一份提交前的
    // 全 0 快照（回执里存的也会是它）。
    assert_eq!(
        committed.capture.processing_summary.searchable, 1,
        "提交出来的文字来源已经进了 search_capture_docs"
    );
    assert_eq!(committed.capture.processing_summary.unprocessed, 0);

    let loaded = summary(&core, &capture_id);
    assert_eq!(loaded, committed.capture.processing_summary);
    assert_eq!(loaded.searchable, 1);
    assert_eq!(loaded.processing, 0);
    assert_eq!(loaded.needs_attention, 0);
}

#[test]
fn imported_source_with_queued_and_waiting_jobs_is_processing() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    let capture_id = draft(&mut core, "op-create");
    import_text(&mut core, &capture_id, b"first body", "a.txt", "imp-1");

    let queued = core.list_jobs(None, 10).unwrap();
    assert_eq!(queued.len(), 2, "导入排了 extract 与 index 两条任务");
    assert!(queued.iter().any(|job| job.kind == "extract"));
    assert!(queued
        .iter()
        .any(|job| job.kind == "index" && job.state == JobState::WaitingDependency));

    let current = summary(&core, &capture_id);
    assert_eq!(
        current.processing, 2,
        "queued 的 extract 与 waiting_dependency 的 index 都是活任务"
    );
    assert_eq!(current.unprocessed, 0, "有活任务就不算没人处理");
    assert_eq!(current.searchable, 0, "还没提取，不能算可检索");
    assert_eq!(current.needs_attention, 0);
}

#[test]
fn driver_extracts_and_indexes_then_the_source_is_searchable() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    let capture_id = draft(&mut core, "op-create");
    import_text(
        &mut core,
        &capture_id,
        b"body one\n\nbody two",
        "a.txt",
        "imp-1",
    );

    let run = core.run_due_jobs(Utc::now(), 60_000, "test", 10).unwrap();
    assert_eq!(run.failed, 0, "提取与索引都不该失败");
    assert_eq!(run.succeeded, 2);

    let current = summary(&core, &capture_id);
    assert_eq!(current.searchable, 1, "当前修订已经进了 search_docs");
    assert_eq!(current.processing, 0, "任务都跑完了");
    assert_eq!(current.unprocessed, 0);
    assert_eq!(current.needs_attention, 0);
}

#[test]
fn sources_without_jobs_are_unprocessed_and_the_count_is_per_source() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    let capture_id = draft(&mut core, "op-create");
    import_text(&mut core, &capture_id, b"one", "a.txt", "imp-1");
    import_text(&mut core, &capture_id, b"two", "b.txt", "imp-2");
    cancel_open_jobs(&mut core);

    let current = summary(&core, &capture_id);
    assert_eq!(current.unprocessed, 2, "两条来源都没正文、也没活任务");
    assert_eq!(current.processing, 0);
    assert_eq!(current.searchable, 0);
    assert_eq!(current.needs_attention, 0);
}

#[test]
fn unsupported_file_stays_unprocessed_even_after_extraction_is_recorded() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    let capture_id = draft(&mut core, "op-create");
    let (source_id, _) = import_into(
        &mut core,
        &capture_id,
        b"\x00\x01\x02",
        "mystery.bin",
        "application/octet-stream",
        "imp-1",
    );

    let run = core.run_due_jobs(Utc::now(), 60_000, "test", 10).unwrap();
    assert_eq!(run.failed, 0, "提取器自己裁决的失败不是任务失败");

    let content = core
        .extracted_content(&source_id)
        .unwrap()
        .expect("失败也要落一条派生内容");
    assert_eq!(content.status, ProcessingStatus::Failed);

    let current = summary(&core, &capture_id);
    assert_eq!(
        current.unprocessed, 1,
        "提取记录是 failed：不能因为任务成功就猜成可检索"
    );
    assert_eq!(current.searchable, 0);
    assert_eq!(current.processing, 0);
    assert_eq!(current.needs_attention, 0);
}

#[test]
fn failed_extract_is_not_processing_and_not_searchable() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    let capture_id = draft(&mut core, "op-create");
    let (source_id, asset_id) =
        import_text(&mut core, &capture_id, b"will disappear", "a.txt", "imp-1");
    cancel_open_jobs(&mut core);

    // 原件不在：提取会走「换个时间可能就好」的退避。把额度设成 1，一次就到终态。
    let lease = core.open_asset(&asset_id, "preview").unwrap();
    std::fs::remove_file(&lease.handle).unwrap();
    core.release_asset(&lease.lease_id).unwrap();

    let mut job =
        NewJob::new("extract", JobPriority::BackgroundExtract).targeting(vec![source_id.clone()]);
    job.max_attempts = 1;
    let queued = core.enqueue_job(job).unwrap();

    let run = core.run_due_jobs(Utc::now(), 60_000, "test", 10).unwrap();
    assert_eq!(run.failed, 1);

    let failed = core.get_job(&queued.id).unwrap();
    assert_eq!(failed.state, JobState::Failed);
    assert!(failed.requires_user_action);
    assert!(
        failed.attention_key.is_none(),
        "fail() 用尽额度不给 attention_key；它不能被误算成已确认"
    );

    let current = summary(&core, &capture_id);
    assert_eq!(current.processing, 0, "终态失败不是处理中");
    assert_eq!(current.searchable, 0);
    assert_eq!(
        current.unprocessed, 1,
        "提取失败不是做完了：来源仍然没有可检索正文"
    );
    assert_eq!(
        current.needs_attention, 1,
        "没有 attention_key 的待处理任务也必须在 needsAttention 里，不能藏起来"
    );
}

#[test]
fn acknowledging_attention_clears_it_but_not_unprocessed() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    let capture_id = draft(&mut core, "op-create");
    let (source_id, _) = import_text(&mut core, &capture_id, b"body", "a.txt", "imp-1");
    cancel_open_jobs(&mut core);

    let job = core
        .enqueue_job(
            NewJob::new("future_kind", JobPriority::Maintenance).targeting(vec![source_id]),
        )
        .unwrap();
    let run = core.run_due_jobs(Utc::now(), 60_000, "test", 10).unwrap();
    assert_eq!(run.failed, 1, "没有执行者的 kind 要走「已放弃」");

    let failed = core.get_job(&job.id).unwrap();
    let key = failed
        .attention_key
        .clone()
        .expect("终态失败要带 attention_key");

    let before = summary(&core, &capture_id);
    assert_eq!(before.needs_attention, 1);
    assert_eq!(before.unprocessed, 1);

    core.acknowledge_attention(&key).unwrap();
    let after = summary(&core, &capture_id);
    assert_eq!(after.needs_attention, 0, "确认之后不再催");
    assert_eq!(
        after.unprocessed, 1,
        "确认只表示「看过了」，没有把没做完的事抹掉"
    );
}

#[test]
fn a_job_targeting_the_capture_itself_counts_as_processing() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    let capture_id = draft(&mut core, "op-create");
    core.save_draft(&capture_id, "文字", 1, "op-save").unwrap();
    core.commit(&capture_id, 2, "op-commit").unwrap();

    // `commit_with_jobs` 允许调用方排任意任务；target 是记录 id 时也要归到这条记录。
    let job = core
        .enqueue_job(
            NewJob::new("future_kind", JobPriority::Maintenance)
                .targeting(vec![capture_id.clone()]),
        )
        .unwrap();

    let current = summary(&core, &capture_id);
    assert_eq!(current.processing, 1);
    assert_eq!(current.searchable, 1, "文字来源已经可检索");
    assert_eq!(current.unprocessed, 0);

    core.cancel_job(&job.id, "op-cancel").unwrap();
    assert_eq!(summary(&core, &capture_id).processing, 0);
}

#[test]
fn whole_library_job_is_not_attributed_to_any_capture() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    let capture_id = draft(&mut core, "op-create");
    core.save_draft(&capture_id, "文字", 1, "op-save").unwrap();
    core.commit(&capture_id, 2, "op-commit").unwrap();

    // 空 target_ids = 整库重建（`indexes.rebuild(scope=all)`）：它不属于任何一条记录。
    core.enqueue_job(NewJob::new("index", JobPriority::Maintenance))
        .unwrap();

    let current = summary(&core, &capture_id);
    assert_eq!(current.processing, 0, "整库任务不摊给每条记录");
    assert_eq!(current.searchable, 1);
    assert_eq!(current.unprocessed, 0);
}

#[test]
fn list_returns_each_capture_summary_without_cross_contamination() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());

    let committed = draft(&mut core, "op-create-a");
    core.save_draft(&committed, "已提交的文字", 1, "op-save-a")
        .unwrap();
    core.commit(&committed, 2, "op-commit-a").unwrap();

    let importing = draft(&mut core, "op-create-b");
    import_text(&mut core, &importing, b"still importing", "b.txt", "imp-b");

    let page = core.list_captures(None, None, 10).unwrap();
    assert_eq!(page.captures.len(), 2);

    let text_capture = page
        .captures
        .iter()
        .find(|capture| capture.id == committed)
        .unwrap();
    assert_eq!(text_capture.processing_summary.searchable, 1);
    assert_eq!(text_capture.processing_summary.processing, 0);

    let file_capture = page
        .captures
        .iter()
        .find(|capture| capture.id == importing)
        .unwrap();
    assert_eq!(file_capture.processing_summary.processing, 2);
    assert_eq!(file_capture.processing_summary.searchable, 0);
}

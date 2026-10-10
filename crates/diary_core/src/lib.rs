//! 「不写日记」设备内本地核心。
//!
//! 已落地：记录路径（B1a）、原件文件库与导入（B1b）、录音与队列（B1c）、
//! 提取与来源定位（B2）、关键词索引与覆盖状态（B3a）、检索会话 `search.*`（B3b）、
//! 用户自己写的文字进检索（B3d）。向量与混合排序还没有。
//!
//! 三条硬性约束贯穿本 crate：
//! 1. **不丢已确认的记录**：写入落在同一个事务里，事务提交后才返回 durable。
//! 2. **幂等**：可重试写入带 operationId，重复提交返回原结果；
//!    同一个 operationId 换了内容报 `idempotency_conflict`。
//! 3. **乐观锁**：带 expectedRevision 的写入不匹配就报 `revision_conflict`，
//!    绝不静默覆盖用户内容。

mod assets;
mod chunker;
mod error;
mod extractors;
mod jobs;
/// 数据对象。公开是为了让桥接层的 codegen 能扫描它们（见 flutter_rust_bridge.yaml）。
pub mod model;
mod recordings;
mod schema;
mod search;
mod search_session;
mod support;

pub use assets::ImportRequest;
pub use extractors::{
    builtin_extractors, ExtractionInput, ExtractionOutcome, Extractor,
};
pub use error::{CoreError, ErrorCode, Result};
pub use model::{
    Asset, AssetLease, AssetStorageState, AuthorType, Capture, CapturePage, CaptureState,
    CommitResult, DomainEvent, DraftSaveResult, EventType, ImportManifest, ImportOrigin,
    Coverage, ExtractedContent, ExtractedSegment, ImportState, ImportStatus, ImportTicket,
    IndexStatus, Job, JobPriority, JobProgress, JobState, LocatorType, NativeRecordingStatus,
    NewJob,
    MatchedBy, ProcessingStatus, ProcessingSummary, RecordingFinalizeResult, RecordingRecovery,
    RecordingSession, RecordingState, RecordingTicket, SegmentManifest, SegmentReceipt,
    SearchFilters, SearchHit, SearchMode, SearchPhase, SearchRequest, SearchSnapshot,
    SourceItem, SourceKind, SourceLocation, SourceLocator, SourceRevision, TextRange,
};
pub use chunker::{chunks_for, TextChunk, CHUNKER_VERSION, CHUNK_OVERLAP_CHARS, MAX_CHUNK_CHARS};
pub use schema::SCHEMA_VERSION;
pub use search::TOKENIZER_VERSION;

use std::collections::HashMap;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension, Transaction};

/// 创建草稿的入参。
#[derive(Debug, Clone)]
pub struct CreateDraftInput<'a> {
    /// 事件时间；为空时用当前时间。
    pub occurred_at: Option<DateTime<Utc>>,
    /// 设备当前的 IANA 时区名，例如 `Asia/Shanghai`。
    pub time_zone: &'a str,
    /// 事件发生时该时区的偏移分钟数。
    pub utc_offset_minutes: i32,
    pub operation_id: &'a str,
}

/// 资料库句柄。
pub struct Core {
    conn: Connection,
    /// 文件库根目录：资料库文件所在目录。内存库没有根目录，导入相关方法会报错。
    root: Option<PathBuf>,
    /// 只读租约，内存态，不持久化。
    leases: HashMap<String, assets::LeaseRecord>,
    /// 检索会话，内存态：它记录的是「这一次查询的翻页与失效判断」，不是业务数据。
    search_sessions: HashMap<String, search_session::SearchSessionState>,
}

/// 一行的原始形态：先取出来，再按业务语义解析，避免把解析错误塞进 SQL 层。
type RawCapture = (String, i64, String, String, String, String, String, i64, String, String);

const CAPTURE_COLUMNS: &str = "id, revision, state, occurred_at, created_at, updated_at, \
                                time_zone, utc_offset_minutes, day_key, draft_text";

impl Core {
    /// 打开（或创建）指定路径的资料库。文件库根目录就是它所在的目录。
    pub fn open(path: impl AsRef<std::path::Path>) -> Result<Self> {
        let path = path.as_ref();
        let root = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .map_or_else(|| PathBuf::from("."), std::path::Path::to_path_buf);
        Self::prepare(Connection::open(path)?, Some(root))
    }

    /// 内存库，只用于测试。没有文件系统根目录，导入相关方法会报 `invalid_state`。
    pub fn open_in_memory() -> Result<Self> {
        Self::prepare(Connection::open_in_memory()?, None)
    }

    /// 内存库 + 一个文件系统根目录，用于测试导入而不必落一个真库文件。
    pub fn open_in_memory_at(root: impl AsRef<std::path::Path>) -> Result<Self> {
        Self::prepare(
            Connection::open_in_memory()?,
            Some(root.as_ref().to_path_buf()),
        )
    }

    fn prepare(mut conn: Connection, root: Option<PathBuf>) -> Result<Self> {
        // WAL 提高并发读写表现；synchronous=FULL 保证「提交了就是落盘了」，
        // 只用 NORMAL 会让进程崩溃时丢掉最后几个事务，与 durable 承诺不符。
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "FULL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        schema::migrate(&mut conn)?;
        if let Some(dir) = &root {
            std::fs::create_dir_all(dir)?;
        }
        // 收拾上次没走完的导入与录音：都如实标成「没完成」，不假装成功。
        assets::recover_on_open(&conn)?;
        recordings::recover_on_open(&conn)?;
        Ok(Self {
            conn,
            root,
            leases: HashMap::new(),
            search_sessions: HashMap::new(),
        })
    }

    /// 文件库根目录。内存库没有根目录，导入相关方法会因此报错。
    pub(crate) fn root_dir(&self) -> Result<&std::path::Path> {
        self.root.as_deref().ok_or_else(|| CoreError::InvalidState {
            entity: "资料库",
            id: "<内存库>".to_owned(),
            state: "内存库没有文件系统根目录，不能导入原件".to_owned(),
        })
    }

    pub fn schema_version(&self) -> Result<i64> {
        Ok(self
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))?)
    }

    /// 仅供测试：把资料库可用的页数压小，用来触发 SQLite 的 `SQLITE_FULL`。
    ///
    /// 没有 root 权限时无法造一个真的满盘环境，这里用页数上限走同一条错误路径。
    #[doc(hidden)]
    pub fn set_page_limit_for_test(&self, pages: i64) -> Result<()> {
        self.conn.pragma_update(None, "max_page_count", pages)?;
        Ok(())
    }

    // ------------------------------------------------------------ 记录路径

    /// 创建草稿，契约第 4.1 节 `captures.createDraft`。
    pub fn create_draft(&mut self, input: CreateDraftInput<'_>) -> Result<Capture> {
        let occurred_hint = input.occurred_at.map(support::to_iso).unwrap_or_default();
        let fingerprint = support::fingerprint(&[
            "create_draft",
            &occurred_hint,
            input.time_zone,
            &input.utc_offset_minutes.to_string(),
        ]);
        if let Some(json) = self.receipt("create_draft", &fingerprint, input.operation_id)? {
            return Ok(serde_json::from_str(&json)?);
        }

        let now = support::now();
        let occurred_at = input.occurred_at.unwrap_or(now);
        let capture = Capture {
            id: support::new_id("cap"),
            revision: 1,
            state: CaptureState::Draft,
            occurred_at,
            created_at: now,
            updated_at: now,
            time_zone: input.time_zone.to_owned(),
            utc_offset_minutes: input.utc_offset_minutes,
            day_key: support::day_key(occurred_at, input.utc_offset_minutes),
            ordered_source_ids: Vec::new(),
            draft_text: String::new(),
            processing_summary: ProcessingSummary::default(),
        };

        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO captures (id, revision, state, occurred_at, created_at, updated_at, \
             time_zone, utc_offset_minutes, day_key, draft_text) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                capture.id,
                capture.revision,
                capture.state.wire(),
                support::to_iso(capture.occurred_at),
                support::to_iso(capture.created_at),
                support::to_iso(capture.updated_at),
                capture.time_zone,
                capture.utc_offset_minutes,
                capture.day_key,
                capture.draft_text,
            ],
        )?;
        store_receipt(
            &tx,
            input.operation_id,
            "create_draft",
            &fingerprint,
            &serde_json::to_string(&capture)?,
        )?;
        // 新记录的 draft_text 一定是空的：这里刷一次索引是「先清后写」的一致性
        // 保证——同一个 capture_id 被重用或重放时不会留下上一条的旧词项。
        search::index_capture(
            &tx,
            search::CaptureInput {
                capture_id: &capture.id,
                text: &capture.draft_text,
            },
        )?;
        insert_event(&tx, EventType::CaptureChanged, &capture.id, capture.revision)?;
        tx.commit()?;
        Ok(capture)
    }

    /// 保存草稿正文。频繁保存只改草稿，不制造永久版本，契约第 2.1 节。
    pub fn save_draft(
        &mut self,
        capture_id: &str,
        text: &str,
        expected_revision: i64,
        operation_id: &str,
    ) -> Result<DraftSaveResult> {
        let fingerprint = support::fingerprint(&[
            "save_draft",
            capture_id,
            &expected_revision.to_string(),
            text,
        ]);
        if let Some(json) = self.receipt("save_draft", &fingerprint, operation_id)? {
            return Ok(serde_json::from_str(&json)?);
        }

        let capture = load_capture(&self.conn, capture_id)?;
        ensure_revision(&capture, expected_revision)?;
        if capture.state != CaptureState::Draft {
            return Err(CoreError::InvalidState {
                entity: "记录",
                id: capture.id.clone(),
                state: capture.state.wire().to_owned(),
            });
        }

        let now = support::now();
        let new_revision = capture.revision + 1;
        let result = DraftSaveResult {
            revision: new_revision,
            // 事务提交后才返回 true：界面据此显示「已保存」。
            durable: true,
            saved_at: now,
        };

        let tx = self.conn.transaction()?;
        tx.execute(
            "UPDATE captures SET revision = ?1, draft_text = ?2, updated_at = ?3 WHERE id = ?4",
            params![new_revision, text, support::to_iso(now), capture.id],
        )?;
        store_receipt(
            &tx,
            operation_id,
            "save_draft",
            &fingerprint,
            &serde_json::to_string(&result)?,
        )?;
        // 草稿正文是用户自己写的文字，也要能被搜到。索引写入与保存放在同一个事务：
        // 不能出现「界面说已保存、检索却还看不到」这种两个事实的状态。
        search::index_capture(
            &tx,
            search::CaptureInput {
                capture_id: &capture.id,
                text,
            },
        )?;
        insert_event(&tx, EventType::CaptureChanged, &capture.id, new_revision)?;
        tx.commit()?;
        Ok(result)
    }

    /// 提交记录：创建原始文字版本，契约第 4.1 节 `captures.commit`。
    pub fn commit(
        &mut self,
        capture_id: &str,
        expected_revision: i64,
        operation_id: &str,
    ) -> Result<CommitResult> {
        self.commit_with_jobs(capture_id, expected_revision, operation_id, &[])
    }

    /// 提交记录，并**在同一个事务里**入队若干任务。
    ///
    /// 任务书 3.2 节要求「核心提交数据与对应的待处理任务/出站事件记录应在同一数据库
    /// 事务中完成」，否则会出现「资料已保存，索引任务却永远丢了」。
    pub fn commit_with_jobs(
        &mut self,
        capture_id: &str,
        expected_revision: i64,
        operation_id: &str,
        jobs_to_queue: &[NewJob],
    ) -> Result<CommitResult> {
        let mut parts: Vec<String> = vec![
            "commit".to_owned(),
            capture_id.to_owned(),
            expected_revision.to_string(),
        ];
        for job in jobs_to_queue {
            parts.push(job.kind.clone());
            parts.push(job.priority.value().to_string());
            parts.push(job.target_ids.join(","));
        }
        let fingerprint =
            support::fingerprint(&parts.iter().map(String::as_str).collect::<Vec<_>>());
        if let Some(json) = self.receipt("commit", &fingerprint, operation_id)? {
            return Ok(serde_json::from_str(&json)?);
        }

        let capture = load_capture(&self.conn, capture_id)?;
        ensure_revision(&capture, expected_revision)?;
        if capture.state != CaptureState::Draft {
            return Err(CoreError::InvalidState {
                entity: "记录",
                id: capture.id.clone(),
                state: capture.state.wire().to_owned(),
            });
        }

        let now = support::now();
        let new_revision = capture.revision + 1;
        let mut ordered = capture.ordered_source_ids.clone();
        let mut original_text_revision = None;

        let tx = self.conn.transaction()?;
        // 空白草稿不制造空的原始文字版本；有内容才建 source + revision。
        if !capture.draft_text.is_empty() {
            let source_id = support::new_id("src");
            let revision_id = support::new_id("rev");
            let position = ordered.len() as i64;
            tx.execute(
                "INSERT INTO source_items (source_id, capture_id, kind, position, \
                 current_revision_id) VALUES (?1, ?2, 'text', ?3, ?4)",
                params![source_id, capture.id, position, revision_id],
            )?;
            tx.execute(
                "INSERT INTO source_revisions (revision_id, source_id, parent_revision_id, text, \
                 asset_id, author_type, occurred_at) VALUES (?1, ?2, NULL, ?3, NULL, ?4, ?5)",
                params![
                    revision_id,
                    source_id,
                    capture.draft_text,
                    AuthorType::User.wire(),
                    support::to_iso(now),
                ],
            )?;
            ordered.push(source_id.clone());
            original_text_revision = Some(SourceRevision {
                revision_id,
                source_id,
                parent_revision_id: None,
                text: Some(capture.draft_text.clone()),
                asset_id: None,
                author_type: AuthorType::User,
                occurred_at: now,
            });
        }

        let mut committed = capture.clone();
        committed.revision = new_revision;
        committed.state = CaptureState::Committed;
        committed.updated_at = now;
        committed.ordered_source_ids = ordered;

        tx.execute(
            "UPDATE captures SET revision = ?1, state = 'committed', updated_at = ?2 WHERE id = ?3",
            params![new_revision, support::to_iso(now), capture.id],
        )?;

        // 任务与提交同一个事务：要么都成，要么都不成。
        let mut queued_job_ids = Vec::new();
        for job in jobs_to_queue {
            queued_job_ids.push(jobs::enqueue_in_tx(&tx, job.clone())?.id);
        }

        let result = CommitResult {
            capture: committed,
            original_text_revision,
        };
        store_receipt(
            &tx,
            operation_id,
            "commit",
            &fingerprint,
            &serde_json::to_string(&result)?,
        )?;
        insert_event(&tx, EventType::CaptureChanged, &capture.id, new_revision)?;
        for job_id in &queued_job_ids {
            insert_event(&tx, EventType::JobChanged, job_id, 1)?;
        }
        tx.commit()?;
        Ok(result)
    }

    /// 修改原始文字：创建新的 SourceRevision，旧版本保留。
    pub fn revise_text(
        &mut self,
        source_id: &str,
        text: &str,
        expected_revision: i64,
        operation_id: &str,
    ) -> Result<SourceRevision> {
        let fingerprint = support::fingerprint(&[
            "revise_text",
            source_id,
            &expected_revision.to_string(),
            text,
        ]);
        if let Some(json) = self.receipt("revise_text", &fingerprint, operation_id)? {
            return Ok(serde_json::from_str(&json)?);
        }

        let (capture_id, parent_revision_id) = load_source_item(&self.conn, source_id)?;
        let capture = load_capture(&self.conn, &capture_id)?;
        // 乐观锁以记录的 revision 为准：记录是聚合根，一次修订同时推进它。
        ensure_revision(&capture, expected_revision)?;
        if capture.state == CaptureState::Draft {
            return Err(CoreError::InvalidState {
                entity: "记录",
                id: capture.id.clone(),
                state: capture.state.wire().to_owned(),
            });
        }

        let now = support::now();
        let revision = SourceRevision {
            revision_id: support::new_id("rev"),
            source_id: source_id.to_owned(),
            parent_revision_id,
            text: Some(text.to_owned()),
            asset_id: None,
            author_type: AuthorType::User,
            occurred_at: now,
        };
        let new_revision = capture.revision + 1;

        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO source_revisions (revision_id, source_id, parent_revision_id, text, \
             asset_id, author_type, occurred_at) VALUES (?1, ?2, ?3, ?4, NULL, ?5, ?6)",
            params![
                revision.revision_id,
                revision.source_id,
                revision.parent_revision_id,
                revision.text,
                revision.author_type.wire(),
                support::to_iso(now),
            ],
        )?;
        tx.execute(
            "UPDATE source_items SET current_revision_id = ?1 WHERE source_id = ?2",
            params![revision.revision_id, source_id],
        )?;
        tx.execute(
            "UPDATE captures SET revision = ?1, updated_at = ?2 WHERE id = ?3",
            params![new_revision, support::to_iso(now), capture.id],
        )?;
        store_receipt(
            &tx,
            operation_id,
            "revise_text",
            &fingerprint,
            &serde_json::to_string(&revision)?,
        )?;
        insert_event(&tx, EventType::CaptureChanged, &capture.id, new_revision)?;
        tx.commit()?;
        Ok(revision)
    }

    // ------------------------------------------------------------ 读取

    pub fn get_capture(&self, capture_id: &str) -> Result<Capture> {
        load_capture(&self.conn, capture_id)
    }

    /// 分页读取。按 `occurred_at DESC, id DESC` 确定性排序，cursor 对调用方不透明。
    pub fn list_captures(
        &self,
        day_key: Option<&str>,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<CapturePage> {
        let (cursor_at, cursor_id) = match cursor {
            Some(raw) => {
                let (at, id) = support::split_cursor(raw)?;
                (Some(support::to_iso(at)), Some(id))
            }
            None => (None, None),
        };
        let page_size = limit.clamp(1, 100);

        let sql = format!(
            "SELECT {CAPTURE_COLUMNS} FROM captures \
             WHERE (?1 IS NULL OR day_key = ?1) \
               AND (?2 IS NULL OR occurred_at < ?2 OR (occurred_at = ?2 AND id < ?3)) \
             ORDER BY occurred_at DESC, id DESC LIMIT ?4"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt
            .query_map(
                params![day_key, cursor_at, cursor_id, (page_size + 1) as i64],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, String>(6)?,
                        row.get::<_, i64>(7)?,
                        row.get::<_, String>(8)?,
                        row.get::<_, String>(9)?,
                    ))
                },
            )?
            .collect::<rusqlite::Result<Vec<RawCapture>>>()?;

        let mut captures = Vec::with_capacity(rows.len().min(page_size));
        for raw in rows {
            captures.push(capture_from_raw(&self.conn, raw)?);
        }
        // 多取一条用来判断还有没有下一页。
        let next_cursor = if captures.len() > page_size {
            captures.truncate(page_size);
            captures
                .last()
                .map(|capture| support::join_cursor(capture.occurred_at, &capture.id))
        } else {
            None
        };

        Ok(CapturePage {
            captures,
            next_cursor,
        })
    }

    // ------------------------------------------------------------ 录音

    /// 申请录音票据与限域暂存目录，契约第 4.2 节 `recordings.prepare`。
    pub fn prepare_recording(&mut self, capture_id: &str, operation_id: &str) -> Result<RecordingTicket> {
        recordings::prepare(self, capture_id, operation_id)
    }

    /// 登记一个已封闭片段。重复登记同一序号返回原回执，内容不一致报冲突。
    pub fn register_segment(
        &mut self,
        recording_id: &str,
        segment_index: i64,
        segment: SegmentManifest,
    ) -> Result<SegmentReceipt> {
        recordings::register_segment(self, recording_id, segment_index, segment)
    }

    /// 上报原生录音状态。后端不虚构麦克风状态。
    pub fn update_recording_state(&mut self, status: NativeRecordingStatus) -> Result<RecordingSession> {
        recordings::update_state(self, status)
    }

    /// 最终化：拼出逻辑音频、建资产，并在同一事务里排一个转写任务。
    pub fn finalize_recording(
        &mut self,
        recording_id: &str,
        last_segment_index: i64,
        end_reason: &str,
    ) -> Result<RecordingFinalizeResult> {
        recordings::finalize(self, recording_id, last_segment_index, end_reason)
    }

    /// 恢复：不假设最后一段完好，缺口如实报告。
    pub fn recover_recording(&self, recording_id: Option<&str>) -> Result<RecordingRecovery> {
        recordings::recover(self, recording_id)
    }

    pub fn recording_session(&self, recording_id: &str) -> Result<RecordingSession> {
        recordings::session(self, recording_id)
    }

    // ------------------------------------------------------------ 任务队列

    pub fn enqueue_job(&mut self, job: NewJob) -> Result<Job> {
        jobs::enqueue(self, job)
    }

    pub fn get_job(&self, job_id: &str) -> Result<Job> {
        jobs::get(self, job_id)
    }

    /// 按状态筛选列出任务，按优先级与创建时间排序。
    pub fn list_jobs(&self, states: Option<&[JobState]>, limit: usize) -> Result<Vec<Job>> {
        jobs::list(self, states, limit)
    }

    /// 按状态精确计数。**不要用 `list_jobs` 的长度当计数**：它为分页把
    /// limit 夹在 200 以内，任务多的时候会少报。
    pub fn count_jobs(&self, states: Option<&[JobState]>) -> Result<i64> {
        jobs::count(self, states)
    }

    /// 领取下一个到期任务：优先级高的先跑。执行本身属于后面的切片。
    pub fn claim_next_due_job(&mut self, now: DateTime<Utc>) -> Result<Option<Job>> {
        jobs::claim_next_due(self, now)
    }

    pub fn complete_job(&mut self, job_id: &str) -> Result<Job> {
        jobs::complete(self, job_id)
    }

    /// 任务失败：还有额度就退避重试，用完了进 failed。
    pub fn fail_job(
        &mut self,
        job_id: &str,
        error_code: &str,
        message: &str,
        now: DateTime<Utc>,
    ) -> Result<Job> {
        jobs::fail(self, job_id, error_code, message, now)
    }

    /// 手动重试。额度用完了会多给一次机会，并如实记录。
    pub fn retry_job(&mut self, job_id: &str, operation_id: &str) -> Result<Job> {
        jobs::retry(self, job_id, operation_id)
    }

    /// 取消任务。取消不删除任何原件。
    pub fn cancel_job(&mut self, job_id: &str, operation_id: &str) -> Result<Job> {
        jobs::cancel(self, job_id, operation_id)
    }

    /// 建议的下次唤醒时刻；没有待办时为 None。
    pub fn next_wakeup(&self) -> Result<Option<DateTime<Utc>>> {
        jobs::next_wakeup(self)
    }

    /// 记录一个需处理的问题已展示过，避免每次重开重复通知。
    pub fn acknowledge_attention(&mut self, attention_key: &str) -> Result<()> {
        jobs::acknowledge_attention(self, attention_key)
    }

    pub fn is_attention_acknowledged(&self, attention_key: &str) -> Result<bool> {
        jobs::is_attention_acknowledged(self, attention_key)
    }

    // ------------------------------------------------------------ 提取与定位

    /// 对某个来源修订跑一次提取，结果作为可重建的派生内容存下来。
    ///
    /// 提取失败也会落一条记录（status=failed、coverage=unavailable、带错误码），
    /// 这样界面上能看到「解析失败」而不是「没有相关内容」。
    pub fn extract_source(&mut self, source_revision_id: &str) -> Result<ExtractedContent> {
        extractors::extract_source_revision(self, source_revision_id)
    }

    /// 读取某个来源当前版本的派生内容；没提取过时返回 None。
    pub fn extracted_content(&self, source_id: &str) -> Result<Option<ExtractedContent>> {
        extractors::extracted_content(self, source_id)
    }

    /// 把 sourceRef + locator 解析成前端可打开的原件与可用性。
    pub fn locate_source(&self, source_ref: &str, locator: SourceLocator) -> Result<SourceLocation> {
        extractors::locate(self, source_ref, locator)
    }

    // ------------------------------------------------------------ 关键词索引

    /// 索引覆盖状态，契约第 4.4 节 `indexes.status`。
    pub fn index_status(&self, source_scope: Option<&[String]>) -> Result<IndexStatus> {
        search::status(self, source_scope)
    }

    /// 重建关键词索引；`source_scope` 为空表示整库。
    ///
    /// 返回重建的文档数：派生内容片段 + 用户自己写的记录文字（`captures.draft_text`）。
    ///
    /// 整库重建是重活，产品路径上应当是可取消的任务；这一片先提供同步入口，
    /// 让覆盖状态与增量索引有个可靠的校准方式（任务化见 issue #31）。
    pub fn rebuild_keyword_index(&mut self, source_scope: Option<&[String]>) -> Result<i64> {
        search::rebuild(self, source_scope)
    }

    /// 关键词检索候选：命中**派生片段**的 ID，按索引写入顺序。
    ///
    /// 记录文字（`captures.draft_text`）不在这里：片段 ID 这个返回值表达不了它。
    /// 要连记录文字一起搜，走 `start_search`。
    pub fn search_candidates(&self, query: &str, limit: usize) -> Result<Vec<String>> {
        search::candidates(self, query, limit)
    }

    /// 命中数量。测试与实测脚本用它核对片段索引的召回是否完整。
    pub fn count_search_matches(&self, query: &str) -> Result<i64> {
        search::count_matches(self, query)
    }

    // ------------------------------------------------------------ 检索会话

    /// 发起一次检索，契约第 4.4 节 `search.start`。
    pub fn start_search(
        &mut self,
        request: SearchRequest,
        query_revision: i64,
    ) -> Result<SearchSnapshot> {
        search_session::start(self, request, query_revision)
    }

    /// 翻页，契约第 4.4 节 `search.nextPage`。游标不属于这个会话时报 `cursor_expired`。
    pub fn search_next_page(&mut self, session_id: &str, cursor: Option<&str>) -> Result<SearchSnapshot> {
        search_session::next_page(self, session_id, cursor)
    }

    /// 读当前快照，契约第 4.4 节 `search.snapshot`。会话不存在或索引变了报 `search_expired`。
    pub fn search_snapshot(&self, session_id: &str) -> Result<SearchSnapshot> {
        search_session::snapshot(self, session_id)
    }

    /// 取消后续处理，契约第 4.4 节 `search.cancel`。不删除任何原件。
    pub fn cancel_search(&mut self, session_id: &str) -> Result<SearchSnapshot> {
        search_session::cancel(self, session_id)
    }

    // ------------------------------------------------------------ 原件与导入

    /// 申请导入暂存位置，契约第 4.2 节 `imports.prepare`。
    pub fn prepare_import(&mut self, request: ImportRequest<'_>) -> Result<ImportTicket> {
        assets::prepare(self, request)
    }

    /// 声明复制完成，契约第 4.2 节 `imports.finish`。
    ///
    /// 核心会自己重算哈希；对不上就报 `integrity_failed` 并保留暂存文件。
    pub fn finish_import(
        &mut self,
        import_id: &str,
        staging_ticket: &str,
        manifest: ImportManifest,
    ) -> Result<ImportStatus> {
        assets::finish(self, import_id, staging_ticket, manifest)
    }

    pub fn import_status(&self, import_id: &str) -> Result<ImportStatus> {
        assets::status(self, import_id)
    }

    /// 取消导入，不影响其他已导入的材料。
    pub fn cancel_import(&mut self, import_id: &str) -> Result<()> {
        assets::cancel(self, import_id)
    }

    /// 打开只读租约，契约第 4.2 节 `assets.open`。
    pub fn open_asset(&mut self, asset_id: &str, usage: &str) -> Result<AssetLease> {
        assets::open_asset(self, asset_id, usage)
    }

    pub fn release_asset(&mut self, lease_id: &str) -> Result<()> {
        assets::release_asset(self, lease_id)
    }

    /// 还没走完的导入数量（未完成与可恢复都算）。
    pub fn count_imports_in_flight(&self) -> Result<i64> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM import_sessions WHERE state IN \
             ('prepared', 'copying', 'verifying', 'recoverable')",
            [],
            |row| row.get(0),
        )?)
    }

    /// 还没收尾的录音数量。
    pub fn count_open_recordings(&self) -> Result<i64> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM recording_sessions WHERE state IN \
             ('preparing', 'recording', 'paused', 'stopping', 'interrupted', 'recoverable')",
            [],
            |row| row.get(0),
        )?)
    }

    /// 记录数量（不含回收站）。
    ///
    /// 契约 `CoreSnapshot.captureCount` 用它；草稿也算——用户看到的「记录」里
    /// 草稿本来就在。回收站的不算，和默认检索口径一致。
    pub fn count_captures(&self) -> Result<i64> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM captures WHERE state <> 'trashed'",
            [],
            |row| row.get(0),
        )?)
    }

    /// 事件表里最大的序号；一条事件都没有时是 0。
    ///
    /// 契约 `CoreSnapshot.lastEventSequence` 用它：调用方据此知道从哪个游标续读。
    /// 不要用「拉全部事件再取最后一条」来算——那会把整张事件表读进内存。
    pub fn last_event_sequence(&self) -> Result<i64> {
        Ok(self.conn.query_row(
            "SELECT COALESCE(MAX(sequence), 0) FROM domain_events",
            [],
            |row| row.get(0),
        )?)
    }

    /// 资产数量与内容存储占用的字节数。
    pub fn asset_stats(&self) -> Result<(i64, i64)> {
        assets::asset_stats(self)
    }

    /// 当前未释放的租约数量。租约是内存态，进程重启后归零。
    pub fn active_lease_count(&self) -> usize {
        assets::active_leases(&self.leases)
    }

    /// 当前未释放的租约句柄（租约 id → 文件路径），按 id 排序。
    pub fn lease_handles(&self) -> Vec<(String, String)> {
        assets::lease_handles(self)
    }

    /// 丢弃过期租约，返回丢弃的数量。
    pub fn reap_expired_leases(&mut self) -> usize {
        assets::reap_expired_leases(self)
    }

    /// 从序号之后读取事件，契约第 6 节。
    pub fn events_since(&self, from_sequence: i64) -> Result<Vec<DomainEvent>> {
        let mut stmt = self.conn.prepare(
            "SELECT sequence, event_id, type, entity_id, revision, emitted_at \
             FROM domain_events WHERE sequence > ?1 ORDER BY sequence",
        )?;
        let rows = stmt
            .query_map(params![from_sequence], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, String>(5)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        let mut events = Vec::with_capacity(rows.len());
        for (sequence, event_id, event_type, entity_id, revision, emitted_at) in rows {
            events.push(DomainEvent {
                event_id,
                sequence,
                event_type: EventType::from_wire(&event_type).ok_or_else(|| {
                    CoreError::CorruptedData {
                        message: format!("未知事件类型：{event_type}"),
                    }
                })?,
                entity_id,
                revision,
                emitted_at: support::parse_iso(&emitted_at)?,
            });
        }
        Ok(events)
    }

    // ------------------------------------------------------------ 幂等回执

    fn receipt(
        &self,
        kind: &str,
        fingerprint: &str,
        operation_id: &str,
    ) -> Result<Option<String>> {
        let row: Option<(String, String, String)> = self
            .conn
            .query_row(
                "SELECT kind, request_fingerprint, result_json FROM operation_receipts \
                 WHERE operation_id = ?1",
                params![operation_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;

        match row {
            None => Ok(None),
            Some((stored_kind, stored_fingerprint, result_json)) => {
                if stored_kind != kind || stored_fingerprint != fingerprint {
                    return Err(CoreError::IdempotencyConflict {
                        operation_id: operation_id.to_owned(),
                    });
                }
                Ok(Some(result_json))
            }
        }
    }
}

// ------------------------------------------------------------------ 内部工具

fn ensure_revision(capture: &Capture, expected_revision: i64) -> Result<()> {
    if capture.revision != expected_revision {
        return Err(CoreError::RevisionConflict {
            expected: expected_revision,
            actual: capture.revision,
        });
    }
    Ok(())
}

fn store_receipt(
    tx: &Transaction<'_>,
    operation_id: &str,
    kind: &str,
    fingerprint: &str,
    result_json: &str,
) -> Result<()> {
    tx.execute(
        "INSERT INTO operation_receipts (operation_id, kind, request_fingerprint, result_json, \
         created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            operation_id,
            kind,
            fingerprint,
            result_json,
            support::to_iso(support::now())
        ],
    )?;
    Ok(())
}

fn insert_event(
    tx: &Transaction<'_>,
    event_type: EventType,
    entity_id: &str,
    revision: i64,
) -> Result<()> {
    tx.execute(
        "INSERT INTO domain_events (event_id, type, entity_id, revision, emitted_at) \
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            support::new_id("evt"),
            event_type.wire(),
            entity_id,
            revision,
            support::to_iso(support::now())
        ],
    )?;
    Ok(())
}

fn load_source_item(conn: &Connection, source_id: &str) -> Result<(String, Option<String>)> {
    conn.query_row(
        "SELECT capture_id, current_revision_id FROM source_items WHERE source_id = ?1",
        params![source_id],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
    )
    .optional()?
    .ok_or_else(|| CoreError::NotFound {
        entity: "来源",
        id: source_id.to_owned(),
    })
}

fn load_capture(conn: &Connection, capture_id: &str) -> Result<Capture> {
    let sql = format!("SELECT {CAPTURE_COLUMNS} FROM captures WHERE id = ?1");
    let raw: Option<RawCapture> = conn
        .query_row(&sql, params![capture_id], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
                row.get(6)?,
                row.get(7)?,
                row.get(8)?,
                row.get(9)?,
            ))
        })
        .optional()?;

    match raw {
        Some(row) => capture_from_raw(conn, row),
        None => Err(CoreError::NotFound {
            entity: "记录",
            id: capture_id.to_owned(),
        }),
    }
}

fn capture_from_raw(conn: &Connection, raw: RawCapture) -> Result<Capture> {
    let (id, revision, state, occurred_at, created_at, updated_at, time_zone, offset, day_key, draft_text) = raw;
    let mut stmt =
        conn.prepare("SELECT source_id FROM source_items WHERE capture_id = ?1 ORDER BY position")?;
    let ordered_source_ids = stmt
        .query_map(params![id], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<String>>>()?;

    Ok(Capture {
        id,
        revision,
        state: CaptureState::from_wire(&state).ok_or_else(|| CoreError::CorruptedData {
            message: format!("未知记录状态：{state}"),
        })?,
        occurred_at: support::parse_iso(&occurred_at)?,
        created_at: support::parse_iso(&created_at)?,
        updated_at: support::parse_iso(&updated_at)?,
        time_zone,
        utc_offset_minutes: i32::try_from(offset).map_err(|_| CoreError::CorruptedData {
            message: format!("时区偏移超出范围：{offset}"),
        })?,
        day_key,
        ordered_source_ids,
        draft_text,
        // B1b/B2 接上提取与索引后才会出现非零值。
        processing_summary: ProcessingSummary::default(),
    })
}
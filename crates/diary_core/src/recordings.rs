//! 录音会话接收，契约第 4.2 节与任务书 3.3 节。
//!
//! 前端平台层负责实际收音，后端负责登记。三条约定：
//! 1. **登记幂等**：`(recording_id, segment_index)` 唯一；同序号重复登记内容一致就
//!    返回原回执，内容不一致报冲突。
//! 2. **不信声明的哈希**：每个片段登记与最终化时都自己流式重算一遍。
//! 3. **不假设最后一段完好**：恢复时保留所有可解码的已封闭片段，如实报告缺口；
//!    最终化也不会因为缺片段就假装完整。

use std::fs;
use std::io::{Read, Write};

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use sha2::{Digest, Sha256};

use crate::assets::{blob_rel_path, hash_file, CONCAT_BUFFER_BYTES};
use crate::error::{CoreError, Result};
use crate::model::{
    EventType, JobPriority, NativeRecordingStatus, NewJob, RecordingFinalizeResult,
    RecordingRecovery, RecordingSession, RecordingState, RecordingTicket, SegmentManifest,
    SegmentReceipt,
};
use crate::support;
use crate::Core;

/// 申请录音票据与限域暂存目录。
pub(crate) fn prepare(
    core: &mut Core,
    capture_id: &str,
    operation_id: &str,
) -> Result<RecordingTicket> {
    let root = core.root_dir()?.to_path_buf();
    let capture = crate::load_capture(&core.conn, capture_id)?;
    // 回收站里的记录只读：不在 trash 之后开始一段新录音。
    crate::ensure_not_trashed(&capture, "开始录音")?;

    let fingerprint = support::fingerprint(&["recordings.prepare", capture_id]);
    if let Some(json) = core.receipt("recordings.prepare", &fingerprint, operation_id)? {
        return Ok(serde_json::from_str(&json)?);
    }

    let recording_id = support::new_id("rec");
    let asset_id = support::new_id("asset");
    let staging_rel_path = format!("staging/{recording_id}");
    let staging_abs = root.join(&staging_rel_path);
    fs::create_dir_all(&staging_abs)?;

    let ticket = RecordingTicket {
        recording_id: recording_id.clone(),
        asset_id,
        staging_ticket: staging_abs.to_string_lossy().into_owned(),
    };

    let now = support::to_iso(support::now());
    let tx = core.conn.transaction()?;
    tx.execute(
        "INSERT INTO recording_sessions (id, capture_id, asset_id, staging_rel_path, state, \
         started_at, updated_at) VALUES (?1, ?2, ?3, ?4, 'preparing', ?5, ?5)",
        params![
            ticket.recording_id,
            capture_id,
            ticket.asset_id,
            staging_rel_path,
            now
        ],
    )?;
    crate::store_receipt(
        &tx,
        operation_id,
        "recordings.prepare",
        &fingerprint,
        &serde_json::to_string(&ticket)?,
    )?;
    tx.commit()?;
    Ok(ticket)
}

/// 登记一个已封闭片段。
pub(crate) fn register_segment(
    core: &mut Core,
    recording_id: &str,
    segment_index: i64,
    segment: SegmentManifest,
) -> Result<SegmentReceipt> {
    if segment_index < 0 {
        return Err(CoreError::InvalidState {
            entity: "片段序号",
            id: segment_index.to_string(),
            state: "不能为负".to_owned(),
        });
    }
    let root = core.root_dir()?.to_path_buf();
    let session = load_session(&core.conn, recording_id)?;
    if session.state == RecordingState::Saved {
        return Err(CoreError::InvalidState {
            entity: "录音",
            id: recording_id.to_owned(),
            state: "已经最终化".to_owned(),
        });
    }
    // 记录在回收站里：整条聚合（含录音会话）只读。trash 入口已经拒绝在途录音，
    // 这里再挡一次，旧库/异常路径也不能把片段继续登记到 trashed 记录上。
    let capture = crate::load_capture(&core.conn, &session.capture_id)?;
    crate::ensure_not_trashed(&capture, "继续录音")?;

    let fingerprint = support::fingerprint(&[
        "register_segment",
        recording_id,
        &segment_index.to_string(),
        &segment.segment_id,
        &segment.relative_file_name,
        &segment.duration_ms.to_string(),
        &segment.byte_size.to_string(),
        &segment.sha256,
    ]);

    // 幂等：同序号登记过，就看内容指纹。
    if let Some(existing) = load_segment(&core.conn, recording_id, segment_index)? {
        if existing.fingerprint == fingerprint {
            return Ok(SegmentReceipt {
                segment_id: existing.segment_id,
                segment_index,
                durable: true,
                recorded_at: existing.closed_at,
            });
        }
        return Err(CoreError::IdempotencyConflict {
            operation_id: format!("{recording_id} 的第 {segment_index} 段"),
        });
    }

    // 文件必须在限域暂存目录里，且内容与声明一致。
    let path = root
        .join(&session.staging_rel_path)
        .join(&segment.relative_file_name);
    let actual_bytes = fs::metadata(&path)
        .map_err(|err| CoreError::IntegrityFailed {
            reason: format!("片段文件不可读：{}（{err}）", segment.relative_file_name),
        })?
        .len() as i64;
    if actual_bytes != segment.byte_size {
        return Err(CoreError::IntegrityFailed {
            reason: format!(
                "片段 {} 字节数不符：声明 {}，实际 {actual_bytes}",
                segment.segment_index_hint(segment_index),
                segment.byte_size
            ),
        });
    }
    let actual_sha = hash_file(&path)?;
    if !actual_sha.eq_ignore_ascii_case(&segment.sha256) {
        return Err(CoreError::IntegrityFailed {
            reason: format!("片段 {segment_index} 的内容哈希与声明不符"),
        });
    }

    let closed_at = segment.closed_at.unwrap_or_else(support::now);
    let tx = core.conn.transaction()?;
    tx.execute(
        "INSERT INTO recording_segments (recording_id, segment_index, segment_id, \
         relative_file_name, duration_ms, byte_size, sha256, content_fingerprint, closed_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            recording_id,
            segment_index,
            segment.segment_id,
            segment.relative_file_name,
            segment.duration_ms,
            segment.byte_size,
            segment.sha256,
            fingerprint,
            support::to_iso(closed_at),
        ],
    )?;
    // 第一段到达就意味着真的在录了；序号只增不减。
    tx.execute(
        "UPDATE recording_sessions SET state = CASE WHEN state = 'preparing' THEN 'recording' \
         ELSE state END, last_segment_index = MAX(last_segment_index, ?1), updated_at = ?2 \
         WHERE id = ?3",
        params![segment_index, support::to_iso(support::now()), recording_id],
    )?;
    tx.commit()?;

    Ok(SegmentReceipt {
        segment_id: segment.segment_id,
        segment_index,
        durable: true,
        recorded_at: closed_at,
    })
}

/// 上报原生录音状态。后端不虚构麦克风状态。
pub(crate) fn update_state(
    core: &mut Core,
    status: NativeRecordingStatus,
) -> Result<RecordingSession> {
    let session = load_session(&core.conn, &status.recording_id)?;
    if session.state == RecordingState::Saved {
        return Err(CoreError::InvalidState {
            entity: "录音",
            id: status.recording_id,
            state: "已经最终化".to_owned(),
        });
    }
    // 记录在回收站里：不再接受原生状态推进（否则会出现「回收站里的录音还在录」）。
    let capture = crate::load_capture(&core.conn, &session.capture_id)?;
    crate::ensure_not_trashed(&capture, "更新录音状态")?;

    core.conn.execute(
        "UPDATE recording_sessions SET state = ?1, elapsed_ms = ?2, durable_through_ms = ?3, \
         error_message = ?4, updated_at = ?5 WHERE id = ?6",
        params![
            status.state.wire(),
            status.elapsed_ms,
            status.persisted_through_ms,
            status.message,
            support::to_iso(support::now()),
            status.recording_id,
        ],
    )?;
    load_session(&core.conn, &status.recording_id)
        .and_then(|row| session_from(&core.conn, &row))
}

/// 最终化：把已封闭片段按序拼成逻辑音频，建资产，并排一个转写任务。
pub(crate) fn finalize(
    core: &mut Core,
    recording_id: &str,
    last_segment_index: i64,
    end_reason: &str,
) -> Result<RecordingFinalizeResult> {
    let root = core.root_dir()?.to_path_buf();
    let session = load_session(&core.conn, recording_id)?;
    if session.state == RecordingState::Saved {
        return Err(CoreError::InvalidState {
            entity: "录音",
            id: recording_id.to_owned(),
            state: "已经最终化".to_owned(),
        });
    }
    // **关键护栏**：finalize 会往记录上挂来源、建资产、推进 revision。记录已经在
    // 回收站里就拒绝，否则「回收站里的记录」会被一段后续最终化的录音继续写入。
    let capture = crate::load_capture(&core.conn, &session.capture_id)?;
    crate::ensure_not_trashed(&capture, "最终化录音")?;

    let segments = list_segments(&core.conn, recording_id)?;
    let present: Vec<&SegmentRow> = segments
        .iter()
        .filter(|row| row.segment_index <= last_segment_index)
        .collect();
    if present.is_empty() {
        return Err(CoreError::InvalidState {
            entity: "录音",
            id: recording_id.to_owned(),
            state: "没有可用的已封闭片段".to_owned(),
        });
    }

    // 逐段校验：字节数、内容哈希都要对得上。
    let staging_dir = root.join(&session.staging_rel_path);
    for row in &present {
        let path = staging_dir.join(&row.relative_file_name);
        let actual_bytes = fs::metadata(&path)
            .map_err(|err| CoreError::IntegrityFailed {
                reason: format!("片段 {} 不可读：{err}", row.segment_index),
            })?
            .len() as i64;
        if actual_bytes != row.byte_size {
            return Err(CoreError::IntegrityFailed {
                reason: format!("片段 {} 字节数不符", row.segment_index),
            });
        }
        let actual_sha = hash_file(&path)?;
        if !actual_sha.eq_ignore_ascii_case(&row.sha256) {
            return Err(CoreError::IntegrityFailed {
                reason: format!("片段 {} 内容已变化", row.segment_index),
            });
        }
    }

    // 流式拼接：内存占用与录音长度无关。
    let concat_path = root.join(format!("staging/{recording_id}.logical.part"));
    let mut hasher = Sha256::new();
    let mut total_duration_ms = 0_i64;
    {
        let mut output = fs::File::create(&concat_path)?;
        let mut buffer = vec![0_u8; CONCAT_BUFFER_BYTES];
        for row in &present {
            total_duration_ms += row.duration_ms;
            let mut input = fs::File::open(staging_dir.join(&row.relative_file_name))?;
            loop {
                let read = input.read(&mut buffer)?;
                if read == 0 {
                    break;
                }
                output.write_all(&buffer[..read])?;
                hasher.update(&buffer[..read]);
            }
        }
        output.sync_all()?;
    }
    let sha256 = format!("{:x}", hasher.finalize());
    let logical_bytes = fs::metadata(&concat_path)?.len() as i64;
    let object_ref = blob_rel_path(&sha256);
    let blob_abs = root.join(&object_ref);
    if blob_abs.is_file() {
        let _ = fs::remove_file(&concat_path);
    } else {
        if let Some(parent) = blob_abs.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::rename(&concat_path, &blob_abs)?;
    }

    // 缺口决定最终状态：有洞就不能只说 saved。
    let mut missing = Vec::new();
    for index in 0..=last_segment_index {
        if !present.iter().any(|row| row.segment_index == index) {
            missing.push(index);
        }
    }
    let final_state = if missing.is_empty() {
        RecordingState::Saved
    } else {
        RecordingState::Recoverable
    };

    let now = support::now();
    let source_id = support::new_id("src");
    let revision_id = support::new_id("rev");
    // 任务 kind 的常量在转写链那边定义：入队与执行者引用同一个字符串。
    let job = NewJob::new(crate::transcription::KIND_TRANSCRIBE, JobPriority::BackgroundExtract)
        .targeting(vec![session.asset_id.clone()])
        .with_snapshot(sha256.clone());

    let tx = core.conn.transaction()?;
    tx.execute(
        "INSERT OR IGNORE INTO blobs (sha256, object_ref, byte_size, created_at) \
         VALUES (?1, ?2, ?3, ?4)",
        params![sha256, object_ref, logical_bytes, support::to_iso(now)],
    )?;
    tx.execute(
        "INSERT INTO assets (id, sha256, object_ref, original_name, detected_mime, byte_size, \
         storage_state, import_origin, created_at, media_duration_ms) \
         VALUES (?1, ?2, ?3, ?4, 'audio/x-recording', ?5, 'ready', 'recording', ?6, ?7)",
        params![
            session.asset_id,
            sha256,
            object_ref,
            format!("录音 {}", session.started_at.format("%Y-%m-%d %H:%M")),
            logical_bytes,
            support::to_iso(now),
            total_duration_ms,
        ],
    )?;

    let position: i64 = tx.query_row(
        "SELECT COALESCE(MAX(position), -1) + 1 FROM source_items WHERE capture_id = ?1",
        params![session.capture_id],
        |row| row.get(0),
    )?;
    tx.execute(
        "INSERT INTO source_items (source_id, capture_id, kind, position, current_revision_id) \
         VALUES (?1, ?2, 'audio', ?3, ?4)",
        params![source_id, session.capture_id, position, revision_id],
    )?;
    tx.execute(
        "INSERT INTO source_revisions (revision_id, source_id, parent_revision_id, text, asset_id, \
         author_type, occurred_at) VALUES (?1, ?2, NULL, NULL, ?3, 'user', ?4)",
        params![revision_id, source_id, session.asset_id, support::to_iso(now)],
    )?;

    let capture = crate::load_capture(&tx, &session.capture_id)?;
    let new_revision = capture.revision + 1;
    tx.execute(
        "UPDATE captures SET revision = ?1, updated_at = ?2 WHERE id = ?3",
        params![new_revision, support::to_iso(now), session.capture_id],
    )?;
    tx.execute(
        "UPDATE recording_sessions SET state = ?1, ended_at = ?2, end_reason = ?3, \
         last_segment_index = MAX(last_segment_index, ?4), updated_at = ?2 WHERE id = ?5",
        params![
            final_state.wire(),
            support::to_iso(now),
            end_reason,
            last_segment_index,
            recording_id
        ],
    )?;
    // 转写任务与最终化在同一个事务里：不然会出现「音频建好了，任务永远丢了」。
    let queued = crate::jobs::enqueue_in_tx(&tx, job)?;
    crate::insert_event(&tx, EventType::AssetChanged, &session.asset_id, 1)?;
    crate::insert_event(
        &tx,
        EventType::CaptureChanged,
        &session.capture_id,
        new_revision,
    )?;
    crate::insert_event(&tx, EventType::JobChanged, &queued.id, 1)?;
    tx.commit()?;

    // 片段已经进了逻辑音频，暂存目录可以清掉。
    let _ = fs::remove_dir_all(&staging_dir);

    Ok(RecordingFinalizeResult {
        recording_id: recording_id.to_owned(),
        asset_id: session.asset_id,
        state: final_state,
        segment_count: present.len() as i64,
        transcription_queued: true,
        logical_duration_ms: total_duration_ms,
    })
}

/// 恢复：不假设最后一段完好。
pub(crate) fn recover(core: &Core, recording_id: Option<&str>) -> Result<RecordingRecovery> {
    let session = match recording_id {
        Some(id) => load_session(&core.conn, id)?,
        None => match latest_open_session(&core.conn)? {
            Some(row) => row,
            None => {
                return Ok(RecordingRecovery {
                    recording_id: None,
                    state: RecordingState::Idle,
                    closed_segment_indexes: Vec::new(),
                    persisted_ms: 0,
                    received_ms: 0,
                    gap_ms: 0,
                    notes: vec!["没有未完成的录音".to_owned()],
                })
            }
        },
    };

    let segments = list_segments(&core.conn, &session.id)?;
    let present: Vec<i64> = segments.iter().map(|row| row.segment_index).collect();
    let mut missing = Vec::new();
    for index in 0..=session.last_segment_index {
        if !present.contains(&index) {
            missing.push(index);
        }
    }

    // 已登记片段的时长之和 = 真正能拼出来的音频长度。**只有**它能当 persisted。
    let persisted_ms: i64 = segments.iter().map(|row| row.duration_ms).sum();
    // 采集器最后一次上报的「已收到」；它是上报时刻的值，所以只是下界。
    let received_ms = session.elapsed_ms.max(0);
    // 收到的比登记的多的部分：两次登记之间还没封闭的尾巴，崩溃时丢了。
    let unregistered_tail_ms = (received_ms - persisted_ms).max(0);

    // 缺口时长按已见片段的平均时长估算——估算就是估算，写在 notes 里。
    let mut notes = Vec::new();
    let average = if present.is_empty() {
        0
    } else {
        persisted_ms / present.len() as i64
    };
    let gap_ms = average * missing.len() as i64;
    if !missing.is_empty() {
        notes.push(format!(
            "缺 {} 个片段（序号 {missing:?}），缺口时长按平均段长估算为 {gap_ms} ms",
            missing.len()
        ));
    }
    // 「已登记」与「已收到」的差必须说清楚：不隐瞒可能丢掉的尾巴。
    if unregistered_tail_ms > 0 {
        notes.push(format!(
            "已登记片段共 {persisted_ms} ms，采集器最后一次上报收到 {received_ms} ms：\
             有 {unregistered_tail_ms} ms 没有对应的已登记片段，可能是未封闭的尾巴，\
             已在崩溃时丢失（收到量只是最后一次上报值，实际可能更多）"
        ));
    } else if received_ms <= 0 {
        notes.push(format!(
            "已登记片段共 {persisted_ms} ms；采集器没有上报过「收到」进度，\
             最后一段之后可能还有未封闭的尾巴，时长无法确定"
        ));
    } else {
        notes.push(format!(
            "已登记片段共 {persisted_ms} ms，采集器最后一次上报收到 {received_ms} ms，\
             没有可见的未登记尾巴；但上报可能落后最多约 1 秒，最后一段之后仍可能还有没封闭的尾巴"
        ));
    }

    Ok(RecordingRecovery {
        recording_id: Some(session.id.clone()),
        state: session.state,
        closed_segment_indexes: present,
        persisted_ms,
        received_ms,
        gap_ms,
        notes,
    })
}

pub(crate) fn session(core: &Core, recording_id: &str) -> Result<RecordingSession> {
    let row = load_session(&core.conn, recording_id)?;
    session_from(&core.conn, &row)
}

/// 重开资料库时收拾没走完的录音：有片段可救的是 `recoverable`，否则 `interrupted`。
pub(crate) fn recover_on_open(conn: &Connection) -> Result<usize> {
    let changed = conn.execute(
        "UPDATE recording_sessions SET state = CASE WHEN last_segment_index >= 0 \
         THEN 'recoverable' ELSE 'interrupted' END, updated_at = ?1 \
         WHERE state IN ('preparing', 'recording', 'paused', 'stopping')",
        params![support::to_iso(support::now())],
    )?;
    Ok(changed)
}

// ------------------------------------------------------------------ 内部

pub(crate) struct SessionRow {
    pub id: String,
    pub capture_id: String,
    pub asset_id: String,
    pub staging_rel_path: String,
    pub state: RecordingState,
    pub started_at: DateTime<Utc>,
    pub last_segment_index: i64,
    pub elapsed_ms: i64,
    pub durable_through_ms: i64,
    pub error_message: Option<String>,
}

struct SegmentRow {
    segment_index: i64,
    segment_id: String,
    relative_file_name: String,
    duration_ms: i64,
    byte_size: i64,
    sha256: String,
    fingerprint: String,
    closed_at: DateTime<Utc>,
}

impl SegmentManifest {
    /// 报错信息里用的可读序号。
    fn segment_index_hint(&self, index: i64) -> String {
        if self.relative_file_name.is_empty() {
            index.to_string()
        } else {
            format!("{index}（{}）", self.relative_file_name)
        }
    }
}

fn load_session(conn: &Connection, recording_id: &str) -> Result<SessionRow> {
    conn.query_row(
        "SELECT id, capture_id, asset_id, staging_rel_path, state, started_at, \
         last_segment_index, elapsed_ms, durable_through_ms, error_message \
         FROM recording_sessions WHERE id = ?1",
        params![recording_id],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, i64>(6)?,
                row.get::<_, i64>(7)?,
                row.get::<_, i64>(8)?,
                row.get::<_, Option<String>>(9)?,
            ))
        },
    )
    .optional()?
    .map_or_else(
        || {
            Err(CoreError::NotFound {
                entity: "录音",
                id: recording_id.to_owned(),
            })
        },
        |row| {
            Ok(SessionRow {
                id: row.0,
                capture_id: row.1,
                asset_id: row.2,
                staging_rel_path: row.3,
                state: RecordingState::from_wire(&row.4).ok_or_else(|| {
                    CoreError::CorruptedData {
                        message: format!("未知录音状态：{}", row.4),
                    }
                })?,
                started_at: support::parse_iso(&row.5)?,
                last_segment_index: row.6,
                elapsed_ms: row.7,
                durable_through_ms: row.8,
                error_message: row.9,
            })
        },
    )
}

fn latest_open_session(conn: &Connection) -> Result<Option<SessionRow>> {
    let id: Option<String> = conn
        .query_row(
            "SELECT id FROM recording_sessions WHERE state IN \
             ('preparing', 'recording', 'paused', 'stopping', 'interrupted', 'recoverable') \
             ORDER BY started_at DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .optional()?;
    match id {
        Some(id) => Ok(Some(load_session(conn, &id)?)),
        None => Ok(None),
    }
}

fn session_from(conn: &Connection, row: &SessionRow) -> Result<RecordingSession> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM recording_segments WHERE recording_id = ?1",
        params![row.id],
        |row| row.get(0),
    )?;
    Ok(RecordingSession {
        recording_id: row.id.clone(),
        asset_id: row.asset_id.clone(),
        state: row.state,
        elapsed_ms: row.elapsed_ms,
        durable_through_ms: row.durable_through_ms,
        segment_count: count,
        last_error: row.error_message.clone(),
    })
}

fn list_segments(conn: &Connection, recording_id: &str) -> Result<Vec<SegmentRow>> {
    let mut stmt = conn.prepare(
        "SELECT segment_index, segment_id, relative_file_name, duration_ms, byte_size, sha256, \
         content_fingerprint, closed_at FROM recording_segments WHERE recording_id = ?1 \
         ORDER BY segment_index",
    )?;
    let rows = stmt
        .query_map(params![recording_id], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, String>(7)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    rows.into_iter()
        .map(|row| {
            Ok(SegmentRow {
                segment_index: row.0,
                segment_id: row.1,
                relative_file_name: row.2,
                duration_ms: row.3,
                byte_size: row.4,
                sha256: row.5,
                fingerprint: row.6,
                closed_at: support::parse_iso(&row.7)?,
            })
        })
        .collect()
}

fn load_segment(
    conn: &Connection,
    recording_id: &str,
    segment_index: i64,
) -> Result<Option<SegmentRow>> {
    Ok(list_segments(conn, recording_id)?
        .into_iter()
        .find(|row| row.segment_index == segment_index))
}

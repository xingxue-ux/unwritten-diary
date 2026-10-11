//! 原件文件库与导入交接（契约第 2.3 节与第 4.2 节）。
//!
//! 核心承诺：**已确认保存的原件不丢**。用户把下载目录里的原文件删掉，日记库里的
//! 那一份仍然能打开；界面说「已保存」之前，字节必须已经在应用私有目录里落定。
//!
//! 三条设计约定：
//! 1. **不信任调用方给的哈希**。`finish` 会自己流式重算一遍，和声明比对。
//! 2. **文件先落定，再写数据库**。文件系统与 SQLite 不是一个事务；先把字节搬进
//!    内容存储（`blobs/<sha前两位>/<sha>`），再在一个事务里写 blob、asset、
//!    source 与导入日志。中途失败最多留下一个没人引用的 blob 文件，绝不会出现
//!    「数据库里有资产、磁盘上没有文件」。
//! 3. **导入日志可恢复**。进程在导入中途死掉，重开时把未完成的会话标成
//!    `recoverable`，由调用方决定重试还是取消，不假装成功。

use std::collections::HashMap;
use std::fs;
use std::io::Read;
use std::path::Path;

use chrono::{DateTime, Duration, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use sha2::{Digest, Sha256};

use crate::error::{CoreError, Result};
use crate::model::{
    Asset, AssetLease, AssetStorageState, EventType, ImportManifest, ImportOrigin, ImportState,
    ImportStatus, ImportTicket,
};
use crate::support;
use crate::Core;

/// 流式哈希与拼接的缓冲区。内存占用与文件大小无关。
pub(crate) const CONCAT_BUFFER_BYTES: usize = 64 * 1024;

/// 租约有效期。
const LEASE_MINUTES: i64 = 10;

/// 允许的资产用途，对应契约第 4.2 节 `assets.open`。
const USAGES: [&str; 3] = ["preview", "play", "export"];

/// `imports.prepare` 的入参。
#[derive(Debug, Clone)]
pub struct ImportRequest<'a> {
    pub capture_id: &'a str,
    pub display_name: &'a str,
    pub mime_hint: Option<&'a str>,
    pub size_hint: Option<i64>,
    pub origin: ImportOrigin,
    pub operation_id: &'a str,
}

/// 内存里的租约记录。租约不持久化：进程重启后句柄自然失效。
pub(crate) struct LeaseRecord {
    pub handle: String,
    pub expires_at: DateTime<Utc>,
}

/// 申请导入暂存位置。平台层只能往这个票据指向的文件里写。
pub(crate) fn prepare(core: &mut Core, request: ImportRequest<'_>) -> Result<ImportTicket> {
    let root = core.root_dir()?.to_path_buf();
    // 记录不存在就不该开票：否则会出现没人认领的暂存文件。
    let _ = crate::load_capture(&core.conn, request.capture_id)?;

    let fingerprint = support::fingerprint(&[
        "imports.prepare",
        request.capture_id,
        request.display_name,
        request.mime_hint.unwrap_or_default(),
        &request.size_hint.unwrap_or_default().to_string(),
        request.origin.wire(),
    ]);
    if let Some(json) = core.receipt("imports.prepare", &fingerprint, request.operation_id)? {
        return Ok(serde_json::from_str(&json)?);
    }

    let import_id = support::new_id("imp");
    let staging_rel_path = format!("staging/{import_id}.part");
    let staging_abs = root.join(&staging_rel_path);
    if let Some(parent) = staging_abs.parent() {
        fs::create_dir_all(parent)?;
    }

    let ticket = ImportTicket {
        import_id: import_id.clone(),
        staging_ticket: staging_abs.to_string_lossy().into_owned(),
        max_bytes: request.size_hint,
    };

    let now = support::to_iso(support::now());
    let tx = core.conn.transaction()?;
    tx.execute(
        "INSERT INTO import_sessions (id, capture_id, display_name, mime_hint, size_hint, origin, \
         staging_rel_path, state, created_at, updated_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'prepared', ?8, ?8)",
        params![
            ticket.import_id,
            request.capture_id,
            request.display_name,
            request.mime_hint,
            request.size_hint,
            request.origin.wire(),
            staging_rel_path,
            now,
        ],
    )?;
    crate::store_receipt(
        &tx,
        request.operation_id,
        "imports.prepare",
        &fingerprint,
        &serde_json::to_string(&ticket)?,
    )?;
    tx.commit()?;

    Ok(ticket)
}

/// 声明复制完成。核心自己重算哈希，比对通过才把字节收进文件库。
pub(crate) fn finish(
    core: &mut Core,
    import_id: &str,
    staging_ticket: &str,
    manifest: ImportManifest,
) -> Result<ImportStatus> {
    let root = core.root_dir()?.to_path_buf();
    let session = load_session(&core.conn, import_id)?;

    // 已经收好了：直接返回当前状态，不重复搬字节。
    if session.state == ImportState::Ready {
        return Ok(status_from(&session));
    }
    if session.state == ImportState::Error {
        return Ok(status_from(&session));
    }
    if !session.state.is_in_flight() && session.state != ImportState::Recoverable {
        return Err(CoreError::InvalidState {
            entity: "导入",
            id: import_id.to_owned(),
            state: session.state.wire().to_owned(),
        });
    }

    let staging_abs = root.join(&session.staging_rel_path);
    if staging_abs.to_string_lossy() != staging_ticket {
        return Err(CoreError::InvalidState {
            entity: "导入",
            id: import_id.to_owned(),
            state: "票据与暂存位置不一致".to_owned(),
        });
    }

    let actual_bytes = fs::metadata(&staging_abs).map(|meta| meta.len() as i64)?;
    if actual_bytes != manifest.copied_bytes {
        return fail(
            core,
            import_id,
            CoreError::IntegrityFailed {
                reason: format!(
                    "复制的字节数与声明不一致：声明 {}，实际 {actual_bytes}",
                    manifest.copied_bytes
                ),
            },
        );
    }

    let actual_sha = hash_file(&staging_abs)?;
    if !actual_sha.eq_ignore_ascii_case(&manifest.sha256) {
        return fail(
            core,
            import_id,
            CoreError::IntegrityFailed {
                reason: "复制完成后的内容哈希与声明不符".to_owned(),
            },
        );
    }

    // 先把字节搬进内容存储：相同内容复用同一个 blob，不存第二份。
    let object_ref = blob_rel_path(&actual_sha);
    let blob_abs = root.join(&object_ref);
    if blob_abs.is_file() {
        // 已经有同样的内容：暂存文件功成身退。
        let _ = fs::remove_file(&staging_abs);
    } else {
        if let Some(parent) = blob_abs.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::rename(&staging_abs, &blob_abs).or_else(|_| {
            // 跨设备时 rename 会失败，退回复制再删。
            fs::copy(&staging_abs, &blob_abs).map(|_| {
                let _ = fs::remove_file(&staging_abs);
            })
        })?;
    }

    let now = support::now();
    let asset_id = support::new_id("asset");
    let source_id = support::new_id("src");
    let revision_id = support::new_id("rev");

    let tx = core.conn.transaction()?;
    tx.execute(
        "INSERT OR IGNORE INTO blobs (sha256, object_ref, byte_size, created_at) \
         VALUES (?1, ?2, ?3, ?4)",
        params![actual_sha, object_ref, actual_bytes, support::to_iso(now)],
    )?;
    tx.execute(
        "INSERT INTO assets (id, sha256, object_ref, original_name, detected_mime, byte_size, \
         storage_state, import_origin, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'ready', ?7, ?8)",
        params![
            asset_id,
            actual_sha,
            object_ref,
            manifest.original_name,
            manifest.detected_mime,
            actual_bytes,
            session.origin.wire(),
            support::to_iso(now),
        ],
    )?;

    let position: i64 = tx.query_row(
        "SELECT COALESCE(MAX(position), -1) + 1 FROM source_items WHERE capture_id = ?1",
        params![session.capture_id],
        |row| row.get(0),
    )?;
    tx.execute(
        "INSERT INTO source_items (source_id, capture_id, kind, position, current_revision_id) \
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            source_id,
            session.capture_id,
            kind_for_mime(&manifest.detected_mime),
            position,
            revision_id
        ],
    )?;
    tx.execute(
        "INSERT INTO source_revisions (revision_id, source_id, parent_revision_id, text, asset_id, \
         author_type, occurred_at) VALUES (?1, ?2, NULL, NULL, ?3, 'import', ?4)",
        params![revision_id, source_id, asset_id, support::to_iso(now)],
    )?;

    // 记录本身也要推进 revision：导入的材料是这条记录的一部分。
    let capture = crate::load_capture(&tx, &session.capture_id)?;
    let new_revision = capture.revision + 1;
    tx.execute(
        "UPDATE captures SET revision = ?1, updated_at = ?2 WHERE id = ?3",
        params![new_revision, support::to_iso(now), session.capture_id],
    )?;
    tx.execute(
        "UPDATE import_sessions SET state = 'ready', asset_id = ?1, copied_bytes = ?2, \
         sha256 = ?3, final_rel_path = ?4, error_code = NULL, error_message = NULL, \
         updated_at = ?5 WHERE id = ?6",
        params![
            asset_id,
            actual_bytes,
            actual_sha,
            object_ref,
            support::to_iso(now),
            import_id
        ],
    )?;
    // 导入完成就排一个提取任务，和资产写在同一个事务里。
    // `job.changed` 由队列在入队时发（`jobs::enqueue_in_tx`），这里不重复发。
    let extract_job = crate::model::NewJob::new("extract", crate::model::JobPriority::BackgroundExtract)
        .targeting(vec![revision_id.clone()])
        .with_snapshot(actual_sha.clone());
    crate::jobs::enqueue_in_tx(&tx, extract_job)?;
    crate::insert_event(&tx, EventType::AssetChanged, &asset_id, 1)?;
    crate::insert_event(
        &tx,
        EventType::CaptureChanged,
        &session.capture_id,
        new_revision,
    )?;
    tx.commit()?;

    let updated = load_session(&core.conn, import_id)?;
    Ok(status_from(&updated))
}

pub(crate) fn status(core: &Core, import_id: &str) -> Result<ImportStatus> {
    Ok(status_from(&load_session(&core.conn, import_id)?))
}

/// 取消导入：删掉暂存文件与会话记录，不影响其他已导入的材料。
pub(crate) fn cancel(core: &mut Core, import_id: &str) -> Result<()> {
    let root = core.root_dir()?.to_path_buf();
    let session = load_session(&core.conn, import_id)?;
    if session.state == ImportState::Ready {
        return Err(CoreError::InvalidState {
            entity: "导入",
            id: import_id.to_owned(),
            state: "已经完成，不能取消".to_owned(),
        });
    }
    // 尽力删除：删不掉也不该拦住取消本身。
    let _ = fs::remove_file(root.join(&session.staging_rel_path));
    core.conn
        .execute("DELETE FROM import_sessions WHERE id = ?1", params![import_id])?;
    Ok(())
}

/// 打开只读租约。资产在库里但文件不见时，如实标记成 `missing`。
pub(crate) fn open_asset(core: &mut Core, asset_id: &str, usage: &str) -> Result<AssetLease> {
    let root = core.root_dir()?.to_path_buf();
    reap_expired_leases(core);
    if !USAGES.contains(&usage) {
        return Err(CoreError::InvalidState {
            entity: "资产用途",
            id: usage.to_owned(),
            state: "只支持 preview / play / export".to_owned(),
        });
    }

    let asset = load_asset(&core.conn, asset_id)?;
    if asset.storage_state != AssetStorageState::Ready {
        return Err(CoreError::AssetMissing {
            asset_id: asset_id.to_owned(),
            reason: format!("状态是 {}", asset.storage_state.wire()),
        });
    }

    let handle = root.join(&asset.object_ref);
    if !handle.is_file() {
        // 库里有记录、磁盘上没有：这是必须让用户看见的事实，不是可以糊过去的情况。
        core.conn.execute(
            "UPDATE assets SET storage_state = 'missing' WHERE id = ?1",
            params![asset_id],
        )?;
        return Err(CoreError::AssetMissing {
            asset_id: asset_id.to_owned(),
            reason: format!("文件不在：{}", asset.object_ref),
        });
    }

    let lease_id = support::new_id("lease");
    let expires_at = support::now() + Duration::minutes(LEASE_MINUTES);
    let handle_text = handle.to_string_lossy().into_owned();
    core.leases.insert(
        lease_id.clone(),
        LeaseRecord {
            handle: handle_text.clone(),
            expires_at,
        },
    );

    Ok(AssetLease {
        lease_id,
        asset_id: asset_id.to_owned(),
        usage: usage.to_owned(),
        handle: handle_text,
        expires_at,
        byte_size: asset.byte_size,
    })
}

/// 丢掉已经过期的租约。租约过期不代表文件被删，只是句柄不再有效。
pub(crate) fn reap_expired_leases(core: &mut Core) -> usize {
    let now = support::now();
    let before = core.leases.len();
    core.leases.retain(|_, record| record.expires_at > now);
    before - core.leases.len()
}

/// 未释放的租约句柄，按租约 id 排序返回。
pub(crate) fn lease_handles(core: &Core) -> Vec<(String, String)> {
    let mut handles: Vec<(String, String)> = core
        .leases
        .iter()
        .map(|(id, record)| (id.clone(), record.handle.clone()))
        .collect();
    handles.sort();
    handles
}

pub(crate) fn release_asset(core: &mut Core, lease_id: &str) -> Result<()> {
    core.leases
        .remove(lease_id)
        .map(|_| ())
        .ok_or_else(|| CoreError::NotFound {
            entity: "租约",
            id: lease_id.to_owned(),
        })
}

/// 重开资料库时收拾中断的导入：把还在中途的会话标成 `recoverable`。
///
/// 不猜「复制完了没有」——调用方可以拿 `imports.status` 看到这个状态，
/// 再决定重试 `finish` 还是 `cancel`。
pub(crate) fn recover_on_open(conn: &Connection) -> Result<usize> {
    let changed = conn.execute(
        "UPDATE import_sessions SET state = 'recoverable', updated_at = ?1 \
         WHERE state IN ('prepared', 'copying', 'verifying')",
        params![support::to_iso(support::now())],
    )?;
    Ok(changed)
}

// ------------------------------------------------------------------ 内部工具

fn fail(core: &mut Core, import_id: &str, error: CoreError) -> Result<ImportStatus> {
    let code = error.code();
    let message = error.to_string();
    core.conn.execute(
        "UPDATE import_sessions SET state = 'error', error_code = ?1, error_message = ?2, \
         updated_at = ?3 WHERE id = ?4",
        params![
            code.wire(),
            message,
            support::to_iso(support::now()),
            import_id
        ],
    )?;
    // 暂存文件故意留着：用户还可以重试或取消，不能悄悄丢掉他刚给的材料。
    Err(error)
}

pub(crate) fn blob_rel_path(sha256: &str) -> String {
    let prefix = &sha256[..2.min(sha256.len())];
    format!("blobs/{prefix}/{sha256}")
}

fn kind_for_mime(mime: &str) -> &'static str {
    if mime.starts_with("image/") {
        "image"
    } else if mime.starts_with("audio/") {
        "audio"
    } else if mime.starts_with("video/") {
        "video"
    } else {
        "file"
    }
}

/// 流式算 sha256：内存占用与文件大小无关。
pub(crate) fn hash_file(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; CONCAT_BUFFER_BYTES];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

pub(crate) struct SessionRow {
    pub id: String,
    pub capture_id: String,
    pub asset_id: Option<String>,
    pub origin: ImportOrigin,
    pub staging_rel_path: String,
    pub state: ImportState,
    pub copied_bytes: i64,
    pub size_hint: Option<i64>,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
}

fn load_session(conn: &Connection, import_id: &str) -> Result<SessionRow> {
    let row = conn
        .query_row(
            "SELECT id, capture_id, asset_id, origin, staging_rel_path, state, copied_bytes, \
             size_hint, error_code, error_message FROM import_sessions WHERE id = ?1",
            params![import_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, Option<i64>>(7)?,
                    row.get::<_, Option<String>>(8)?,
                    row.get::<_, Option<String>>(9)?,
                ))
            },
        )
        .optional()?
        .ok_or_else(|| CoreError::NotFound {
            entity: "导入",
            id: import_id.to_owned(),
        })?;

    Ok(SessionRow {
        id: row.0,
        capture_id: row.1,
        asset_id: row.2,
        origin: ImportOrigin::from_wire(&row.3).ok_or_else(|| CoreError::CorruptedData {
            message: format!("未知导入来源：{}", row.3),
        })?,
        staging_rel_path: row.4,
        state: ImportState::from_wire(&row.5).ok_or_else(|| CoreError::CorruptedData {
            message: format!("未知导入状态：{}", row.5),
        })?,
        copied_bytes: row.6,
        size_hint: row.7,
        error_code: row.8,
        error_message: row.9,
    })
}

fn status_from(session: &SessionRow) -> ImportStatus {
    ImportStatus {
        import_id: session.id.clone(),
        state: session.state,
        copied_bytes: session.copied_bytes,
        total_bytes: session.size_hint.unwrap_or(session.copied_bytes),
        asset_id: session.asset_id.clone(),
        error_code: session.error_code.clone(),
        message: session.error_message.clone(),
    }
}

fn load_asset(conn: &Connection, asset_id: &str) -> Result<Asset> {
    let row = conn
        .query_row(
            "SELECT id, sha256, object_ref, original_name, detected_mime, byte_size, \
             storage_state, import_origin, created_at, media_duration_ms, width, height \
             FROM assets WHERE id = ?1",
            params![asset_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, String>(8)?,
                    row.get::<_, Option<i64>>(9)?,
                    row.get::<_, Option<i64>>(10)?,
                    row.get::<_, Option<i64>>(11)?,
                ))
            },
        )
        .optional()?
        .ok_or_else(|| CoreError::NotFound {
            entity: "资产",
            id: asset_id.to_owned(),
        })?;

    Ok(Asset {
        id: row.0,
        sha256: row.1,
        object_ref: row.2,
        original_name: row.3,
        detected_mime: row.4,
        byte_size: row.5,
        storage_state: AssetStorageState::from_wire(&row.6).ok_or_else(|| {
            CoreError::CorruptedData {
                message: format!("未知资产状态：{}", row.6),
            }
        })?,
        import_origin: ImportOrigin::from_wire(&row.7).ok_or_else(|| {
            CoreError::CorruptedData {
                message: format!("未知资产来源：{}", row.7),
            }
        })?,
        created_at: support::parse_iso(&row.8)?,
        media_duration_ms: row.9,
        width: row.10,
        height: row.11,
    })
}

/// 资产总数与文件库占用，供测试与将来的设置页使用。
pub(crate) fn asset_stats(core: &Core) -> Result<(i64, i64)> {
    let count: i64 = core
        .conn
        .query_row("SELECT COUNT(*) FROM assets", [], |row| row.get(0))?;
    let bytes: i64 = core
        .conn
        .query_row("SELECT COALESCE(SUM(byte_size), 0) FROM blobs", [], |row| {
            row.get(0)
        })?;
    Ok((count, bytes))
}

/// 供测试使用：把内存里的租约表当成只读视图。
pub(crate) fn active_leases(leases: &HashMap<String, LeaseRecord>) -> usize {
    leases.len()
}


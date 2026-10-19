//! 回收站：移入、恢复、改归属日与永久清理（B7）。
//!
//! 契约 3.1 节的状态机是：
//!
//! ```text
//! draft ──commit──▶ committed
//!   │                  │
//!   └──────trash───────┴──▶ trashed
//!                           │
//!                  restore ─┴─▶ 移入之前的状态（draft / committed）
//!                           │
//!                  purge ───┴─▶ 记录不存在（不可逆）
//! ```
//!
//! 三条硬约束照旧：可重试写入带 `operationId`（重复提交返回原结果）、改状态带
//! `expectedRevision`（不匹配报 `revision_conflict`）、只有事务提交后才算成功。
//!
//! **回收站不复制一份数据**：`state = 'trashed'` 只是一个状态位，正文、来源、转写、
//! 块与向量都原地保留。这样恢复是「把状态改回去」，不用把原件再搬一遍；检索与 AI
//! 输入靠既有口径（`captures.state <> 'trashed'`）排除，不靠删除任何索引行。
//!
//! **永久清理是不可逆的**，所以必须先 `previewPurge` 拿一次性令牌，再带着令牌
//! `purgeCapture`。没有预览的清理一律拒绝；预览之后清单变了（又导入原件、又生成了
//! 引用它的日记版本）也会拒绝并要求重新预览——绝不按一份过期的清单删东西。

use std::collections::HashSet;
use std::fs;

use chrono::{DateTime, Duration, NaiveDate, Utc};
use rusqlite::{params, OptionalExtension};
use serde::Serialize;

use crate::error::{CoreError, Result};
use crate::model::{Capture, CaptureState, EventType, PurgePreview, PurgeResult};
use crate::support;
use crate::Core;

/// 预览令牌默认有效期。十分钟够用户看清清单再点确认，又不会让一个上午前的预览
/// 还能删东西。测试用 `Core::set_purge_preview_ttl_for_test` 改它。
pub(crate) const PURGE_PREVIEW_TTL_SECONDS: i64 = 10 * 60;

/// 一次预览在内存里的状态。**不持久化**：令牌只在当前核心会话里有效，进程重启后
/// 必须重新预览——这和「清空确认框」是同一件事，也避免把一个能删资料的令牌写进库。
pub(crate) struct PurgePreviewState {
    capture_id: String,
    /// 预览那一刻记录的 revision。预览之后任何会改变清单的写入都会推进它。
    capture_revision: i64,
    expires_at: DateTime<Utc>,
    asset_ids: Vec<String>,
    derived_content_ids: Vec<String>,
    diary_version_ids: Vec<String>,
}

/// 一次清理会碰到的全部行。预览与清理共用同一个盘点函数，避免两处口径漂移。
struct PurgePlan {
    capture: Capture,
    asset_ids: Vec<String>,
    derived_content_ids: Vec<String>,
    diary_version_ids: Vec<String>,
    blobs: Vec<BlobRef>,
}

struct BlobRef {
    sha256: String,
    object_ref: String,
}

/// 移入回收站：`draft` / `committed → trashed`，并记住原状态。
pub(crate) fn trash(
    core: &mut Core,
    capture_id: &str,
    expected_revision: i64,
    operation_id: &str,
) -> Result<Capture> {
    let fingerprint =
        support::fingerprint(&["captures.trash", capture_id, &expected_revision.to_string()]);
    if let Some(json) = core.receipt("captures.trash", &fingerprint, operation_id)? {
        return Ok(serde_json::from_str(&json)?);
    }

    let capture = crate::load_capture(&core.conn, capture_id)?;
    crate::ensure_revision(&capture, expected_revision)?;
    if capture.state == CaptureState::Trashed {
        return Err(invalid_state(&capture, "已经在回收站里"));
    }

    let now = support::now();
    let new_revision = capture.revision + 1;
    let mut trashed = capture.clone();
    trashed.state = CaptureState::Trashed;
    trashed.revision = new_revision;
    trashed.updated_at = now;

    let tx = core.conn.transaction()?;
    tx.execute(
        "UPDATE captures SET revision = ?1, state = 'trashed', trashed_from_state = ?2, \
         updated_at = ?3 WHERE id = ?4",
        params![
            new_revision,
            capture.state.wire(),
            support::to_iso(now),
            capture.id
        ],
    )?;
    // 可见性变了：进行中的检索会话要失效，下一次翻页重新按新口径查。
    crate::search::bump_index_epoch(&tx)?;
    crate::store_receipt(
        &tx,
        operation_id,
        "captures.trash",
        &fingerprint,
        &serde_json::to_string(&trashed)?,
    )?;
    crate::insert_event(&tx, EventType::CaptureTrashed, &capture.id, new_revision)?;
    tx.commit()?;
    Ok(trashed)
}

/// 从回收站恢复：`trashed →` 移入之前的状态（`draft` / `committed`）。
///
/// v16 之前的库没有 `trashed_from_state`（升级前也没有回收站入口），NULL 按
/// `committed` 解释：它是「已经确认保存过」的那一侧，不会把一份已提交的记录悄悄
/// 变成草稿。**绝不按「有没有来源」反推**：空白草稿提交后也是 `committed`。
pub(crate) fn restore(
    core: &mut Core,
    capture_id: &str,
    expected_revision: i64,
    operation_id: &str,
) -> Result<Capture> {
    let fingerprint = support::fingerprint(&[
        "captures.restore",
        capture_id,
        &expected_revision.to_string(),
    ]);
    if let Some(json) = core.receipt("captures.restore", &fingerprint, operation_id)? {
        return Ok(serde_json::from_str(&json)?);
    }

    let capture = crate::load_capture(&core.conn, capture_id)?;
    crate::ensure_revision(&capture, expected_revision)?;
    if capture.state != CaptureState::Trashed {
        return Err(invalid_state(&capture, "不在回收站里，不能恢复"));
    }

    let previous = previous_state(&core.conn, capture_id)?;
    let now = support::now();
    let new_revision = capture.revision + 1;
    let mut restored = capture.clone();
    restored.state = previous;
    restored.revision = new_revision;
    restored.updated_at = now;

    let tx = core.conn.transaction()?;
    tx.execute(
        "UPDATE captures SET revision = ?1, state = ?2, trashed_from_state = NULL, \
         updated_at = ?3 WHERE id = ?4",
        params![
            new_revision,
            previous.wire(),
            support::to_iso(now),
            capture.id
        ],
    )?;
    crate::search::bump_index_epoch(&tx)?;
    crate::store_receipt(
        &tx,
        operation_id,
        "captures.restore",
        &fingerprint,
        &serde_json::to_string(&restored)?,
    )?;
    crate::insert_event(&tx, EventType::CaptureRestored, &capture.id, new_revision)?;
    tx.commit()?;
    Ok(restored)
}

/// 只改归属日：`day_key` 变，`occurred_at` 不变（调整不改录音时间戳）。
///
/// 关键词检索的日期过滤直接读 `captures.day_key`，所以改记录行就够了；文本块按
/// 日子打包（`text_chunks.day_key` 是块的归属日），所以要把**旧日与新日**一起
/// 重打包——否则块的正文还按旧日拼、向量也还挂在旧块上。重打包与改行在同一个
/// 事务里，不留下「记录换了日子、块还是旧的」的半截状态。
pub(crate) fn move_day(
    core: &mut Core,
    capture_id: &str,
    target_day_key: &str,
    expected_revision: i64,
) -> Result<Capture> {
    validate_day_key(target_day_key)?;
    let capture = crate::load_capture(&core.conn, capture_id)?;
    crate::ensure_revision(&capture, expected_revision)?;
    if capture.state == CaptureState::Trashed {
        return Err(invalid_state(&capture, "在回收站里，先恢复再改归属日"));
    }
    // 同一天：没有可改的。直接返回，不推进 revision、不发事件——这不是一次状态推进。
    if capture.day_key == target_day_key {
        return Ok(capture);
    }

    let now = support::now();
    let new_revision = capture.revision + 1;
    let mut moved = capture.clone();
    moved.day_key = target_day_key.to_owned();
    moved.revision = new_revision;
    moved.updated_at = now;

    let tx = core.conn.transaction()?;
    tx.execute(
        "UPDATE captures SET revision = ?1, day_key = ?2, updated_at = ?3 WHERE id = ?4",
        params![
            new_revision,
            target_day_key,
            support::to_iso(now),
            capture.id
        ],
    )?;
    crate::chunks::rebuild_days_in_tx(&tx, &[capture.day_key.clone(), target_day_key.to_owned()])?;
    crate::search::bump_index_epoch(&tx)?;
    crate::insert_event(&tx, EventType::CaptureMovedDay, &capture.id, new_revision)?;
    tx.commit()?;
    Ok(moved)
}

/// 预览永久清理的影响：原件、派生内容、仍引用它的日记版本。
///
/// 只允许回收站里的记录：清理不可逆，产品路径上用户是先「移入回收站」再决定
/// 「永久清理」。令牌绑定 `capture_id`、一次性、带过期；`purge` 必须原样带上。
pub(crate) fn preview_purge(core: &mut Core, capture_id: &str) -> Result<PurgePreview> {
    let capture = crate::load_capture(&core.conn, capture_id)?;
    if capture.state != CaptureState::Trashed {
        return Err(invalid_state(&capture, "不在回收站里，不能永久清理"));
    }
    let plan = plan(core, capture_id)?;

    let now = support::now();
    // 顺手清掉过期令牌：预览是低频操作，不值得为它起一个清理任务。
    core.purge_previews
        .retain(|_, state| state.expires_at > now);
    let token = support::new_id("purge");
    let expires_at = now + Duration::seconds(core.purge_preview_ttl_seconds);
    core.purge_previews.insert(
        token.clone(),
        PurgePreviewState {
            capture_id: plan.capture.id.clone(),
            capture_revision: plan.capture.revision,
            expires_at,
            asset_ids: plan.asset_ids.clone(),
            derived_content_ids: plan.derived_content_ids.clone(),
            diary_version_ids: plan.diary_version_ids.clone(),
        },
    );

    Ok(PurgePreview {
        capture_id: plan.capture.id,
        preview_token: token,
        asset_ids: plan.asset_ids,
        derived_content_ids: plan.derived_content_ids,
        diary_versions_still_referencing: plan.diary_version_ids,
    })
}

/// 永久清理。**只由明确的用户操作发起**：必须带 `previewPurge` 发出的一次性令牌，
/// 且令牌里的清单与当前库里的清单逐项一致。
///
/// 删除范围（逐表）：
/// - `diary_versions`（仍引用它的版本；当前版本被删时把 `diary_days.current_version_id`
///   换成同一天剩下的最新一版，没有就置 NULL）；
/// - `extracted_contents`（级联 `extracted_segments` → `search_docs` → `search_grams`）；
/// - `captures`（级联 `source_items` → `source_revisions`、`import_sessions`、
///   `recording_sessions`、`chunk_spans`、`search_capture_docs` → `search_capture_grams`）；
/// - `assets`（只删确实属于这条记录的）与不再被任何资产引用的 `blobs`，以及它们在
///   文件系统里的内容文件；
/// - 该记录所在日子的 `text_chunks` / `chunk_spans` / `chunk_vectors`：**整日重打包**，
///   这样同一天别的记录的块与向量不会被误删，被清理记录的文字也不会留在块正文里。
///
/// **不删** `operation_receipts`（幂等回执）与 `domain_events`（审计事件），也不删
/// `diary_source_snapshots`：它们是操作与生成过程的审计记录，且可能被别的版本/生成
/// 记录引用；见 PR 的「没验到的」。
pub(crate) fn purge(
    core: &mut Core,
    capture_id: &str,
    preview_token: &str,
    operation_id: &str,
) -> Result<PurgeResult> {
    let fingerprint = support::fingerprint(&["captures.purge", capture_id, preview_token]);
    if let Some(json) = core.receipt("captures.purge", &fingerprint, operation_id)? {
        return Ok(serde_json::from_str(&json)?);
    }

    let now = support::now();
    let state = match core.purge_previews.get(preview_token) {
        Some(state) => state,
        None => {
            return Err(preview_error(
                preview_token,
                "令牌不存在、已使用或已过期，请重新预览",
            ))
        }
    };
    if state.expires_at <= now {
        core.purge_previews.remove(preview_token);
        return Err(preview_error(preview_token, "令牌已过期，请重新预览"));
    }
    if state.capture_id != capture_id {
        return Err(preview_error(
            preview_token,
            "令牌不属于这条记录，请重新预览",
        ));
    }

    let plan = plan(core, capture_id)?;
    if plan.capture.state != CaptureState::Trashed {
        return Err(invalid_state(&plan.capture, "不在回收站里，不能永久清理"));
    }
    // 预览之后清单或 revision 变了：不按旧清单删东西，让用户再看一眼。
    if plan.capture.revision != state.capture_revision
        || plan.asset_ids != state.asset_ids
        || plan.derived_content_ids != state.derived_content_ids
        || plan.diary_version_ids != state.diary_version_ids
    {
        core.purge_previews.remove(preview_token);
        return Err(preview_error(
            preview_token,
            "预览之后记录内容有变化，请重新预览",
        ));
    }

    let result = PurgeResult {
        capture_id: plan.capture.id.clone(),
        purged_asset_ids: plan.asset_ids.clone(),
        purged_derived_content_ids: plan.derived_content_ids.clone(),
        purged_diary_version_ids: plan.diary_version_ids.clone(),
    };

    let tx = core.conn.transaction()?;
    adjust_diary_days(&tx, &plan.diary_version_ids)?;
    delete_ids(&tx, "diary_versions", &plan.diary_version_ids)?;
    delete_ids(&tx, "extracted_contents", &plan.derived_content_ids)?;
    // 删记录本身：来源、导入/录音会话、块段、记录文字的检索行都由外键级联带走。
    tx.execute(
        "DELETE FROM captures WHERE id = ?1",
        params![plan.capture.id],
    )?;
    purge_assets(&tx, &plan)?;
    // 放在删完记录之后：collect_pieces 看不到被清理的篇，块会整日重算；
    // 旧块（正文里混着被清理文字的那些）在同一个事务里被删掉。
    crate::chunks::rebuild_days_in_tx(&tx, std::slice::from_ref(&plan.capture.day_key))?;
    crate::search::bump_index_epoch(&tx)?;

    for asset_id in &plan.asset_ids {
        crate::insert_event(&tx, EventType::AssetChanged, asset_id, 1)?;
    }
    crate::insert_event(
        &tx,
        EventType::CapturePurged,
        &plan.capture.id,
        plan.capture.revision + 1,
    )?;
    crate::store_receipt(
        &tx,
        operation_id,
        "captures.purge",
        &fingerprint,
        &serde_json::to_string(&result)?,
    )?;
    tx.commit()?;

    // 令牌一次性：提交成功后才移除；重放靠 operationId 回执，不再碰令牌。
    core.purge_previews.remove(preview_token);
    // 文件系统不在事务里：库行已经删干净，这里失败最多留下一个没人引用的 blob 文件
    // （内容寻址、不影响正确性），但必须留下可审计的记录。
    remove_blob_files(core, &plan, capture_id);
    Ok(result)
}

// ------------------------------------------------------------------ 盘点

fn plan(core: &Core, capture_id: &str) -> Result<PurgePlan> {
    let capture = crate::load_capture(&core.conn, capture_id)?;
    let source_ids: Vec<String> = {
        let mut statement = core.conn.prepare(
            "SELECT source_id FROM source_items WHERE capture_id = ?1 ORDER BY position, source_id",
        )?;
        let rows = statement
            .query_map(params![capture_id], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };

    let mut asset_ids: Vec<String> = {
        let mut statement = core.conn.prepare(
            "SELECT DISTINCT a.id FROM source_items si \
             JOIN source_revisions sr ON sr.source_id = si.source_id \
             JOIN assets a ON a.id = sr.asset_id \
             WHERE si.capture_id = ?1 ORDER BY a.id",
        )?;
        let rows = statement
            .query_map(params![capture_id], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };
    asset_ids.dedup();

    let mut blobs: Vec<BlobRef> = {
        let mut statement = core.conn.prepare(
            "SELECT DISTINCT a.sha256, a.object_ref FROM source_items si \
             JOIN source_revisions sr ON sr.source_id = si.source_id \
             JOIN assets a ON a.id = sr.asset_id \
             WHERE si.capture_id = ?1 ORDER BY a.sha256, a.object_ref",
        )?;
        let rows = statement
            .query_map(params![capture_id], |row| {
                Ok(BlobRef {
                    sha256: row.get(0)?,
                    object_ref: row.get(1)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };
    blobs.sort_by(|left, right| {
        left.sha256
            .cmp(&right.sha256)
            .then_with(|| left.object_ref.cmp(&right.object_ref))
    });
    blobs
        .dedup_by(|left, right| left.sha256 == right.sha256 && left.object_ref == right.object_ref);

    let derived_content_ids: Vec<String> = {
        let mut statement = core.conn.prepare(
            "SELECT id FROM extracted_contents \
             WHERE source_id IN (SELECT source_id FROM source_items WHERE capture_id = ?1) \
             ORDER BY id",
        )?;
        let rows = statement
            .query_map(params![capture_id], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };

    let diary_version_ids = versions_referencing(&core.conn, &source_ids)?;
    Ok(PurgePlan {
        capture,
        asset_ids,
        derived_content_ids,
        diary_version_ids,
        blobs,
    })
}

/// 仍引用这条记录的日记版本：`provenance_links` 里有一条指向它的来源。
///
/// 不用 `LIKE '%source_id%'` 扫 JSON：那样会把正文里恰好出现同一个 id 文本的行也
/// 算进来。逐行反序列化是清理路径，次数很少，换来的是「列出来的就是真的引用」。
fn versions_referencing(conn: &rusqlite::Connection, source_ids: &[String]) -> Result<Vec<String>> {
    if source_ids.is_empty() {
        return Ok(Vec::new());
    }
    let wanted: HashSet<&str> = source_ids.iter().map(String::as_str).collect();
    let mut statement = conn.prepare(
        "SELECT id, provenance_links FROM diary_versions ORDER BY created_at DESC, id DESC",
    )?;
    let rows = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut versions = Vec::new();
    for (id, links_json) in rows {
        let links: Vec<crate::model::ProvenanceLink> = serde_json::from_str(&links_json)?;
        if links
            .iter()
            .any(|link| wanted.contains(link.source_id.as_str()))
        {
            versions.push(id);
        }
    }
    Ok(versions)
}

/// 删除当前版本被清掉的日记日：先把指针换到同一天剩下的最新一版（没有就 NULL），
/// 再删版本。`diary_days.current_version_id` 有外键，顺序反了会删不动。
fn adjust_diary_days(tx: &rusqlite::Transaction<'_>, deleted_version_ids: &[String]) -> Result<()> {
    if deleted_version_ids.is_empty() {
        return Ok(());
    }
    let ids_json = serde_json::to_string(deleted_version_ids)?;
    let days: Vec<String> = {
        let mut statement = tx.prepare(
            "SELECT DISTINCT day_key FROM diary_versions \
             WHERE id IN (SELECT value FROM json_each(?1)) ORDER BY day_key",
        )?;
        let rows = statement
            .query_map(params![ids_json], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };
    let now = support::to_iso(support::now());
    for day in days {
        let current: Option<Option<String>> = tx
            .query_row(
                "SELECT current_version_id FROM diary_days WHERE day_key = ?1",
                params![day],
                |row| row.get(0),
            )
            .optional()?;
        let Some(Some(current)) = current else {
            continue;
        };
        if !deleted_version_ids.iter().any(|id| id == &current) {
            continue;
        }
        let replacement: Option<String> = tx
            .query_row(
                "SELECT id FROM diary_versions WHERE day_key = ?1 \
                 AND id NOT IN (SELECT value FROM json_each(?2)) \
                 ORDER BY created_at DESC, id DESC LIMIT 1",
                params![day, ids_json],
                |row| row.get(0),
            )
            .optional()?;
        tx.execute(
            "UPDATE diary_days SET current_version_id = ?1, revision = revision + 1, \
             updated_at = ?2 WHERE day_key = ?3",
            params![replacement, now, day],
        )?;
        let revision: i64 = tx.query_row(
            "SELECT revision FROM diary_days WHERE day_key = ?1",
            params![day],
            |row| row.get(0),
        )?;
        crate::insert_event(tx, EventType::DiaryCurrentChanged, &day, revision)?;
    }
    Ok(())
}

fn delete_ids(tx: &rusqlite::Transaction<'_>, table: &str, ids: &[String]) -> Result<()> {
    if ids.is_empty() {
        return Ok(());
    }
    // 表名只来自本模块的常量；id 走参数。
    let sql = format!("DELETE FROM {table} WHERE id = ?1");
    for id in ids {
        tx.execute(&sql, params![id])?;
    }
    Ok(())
}

fn purge_assets(tx: &rusqlite::Transaction<'_>, plan: &PurgePlan) -> Result<()> {
    if plan.asset_ids.is_empty() {
        return Ok(());
    }
    let ids_json = serde_json::to_string(&plan.asset_ids)?;
    // 删来源之后不该再有修订引用这些资产。真出现了就是数据坏了：宁可回滚，
    // 也不能留下「来源指向一个已经删掉的资产」。
    let still_referenced: i64 = tx.query_row(
        "SELECT COUNT(*) FROM source_revisions \
         WHERE asset_id IN (SELECT value FROM json_each(?1))",
        params![ids_json],
        |row| row.get(0),
    )?;
    if still_referenced > 0 {
        return Err(CoreError::CorruptedData {
            message: format!(
                "{} 个来源修订仍引用待清理的资产，清理已回滚",
                still_referenced
            ),
        });
    }
    tx.execute(
        "DELETE FROM assets WHERE id IN (SELECT value FROM json_each(?1))",
        params![ids_json],
    )?;
    let shas_json = serde_json::to_string(
        &plan
            .blobs
            .iter()
            .map(|blob| blob.sha256.clone())
            .collect::<Vec<_>>(),
    )?;
    // 同一份内容可能被别的资产共享（blobs 按 sha 去重）：只在最后一个引用者消失时
    // 才删 blob 行与文件。
    tx.execute(
        "DELETE FROM blobs WHERE sha256 IN (SELECT value FROM json_each(?1)) \
         AND NOT EXISTS (SELECT 1 FROM assets a WHERE a.sha256 = blobs.sha256)",
        params![shas_json],
    )?;
    Ok(())
}

/// 删文件系统里的原件，并把这次清理写成一行可审计的 JSON。
///
/// 核心没有进程级日志设施，所以日志落在资料库旁边的 `diagnostics/purge.jsonl`：
/// 追加写、一行一个清理，记录删了哪些 blob、哪些失败。写日志失败**不影响清理结果**
/// （事务已经提交），也不向调用方报错——清理已经完成，再报一个「日志写不了」只会
/// 让用户以为资料没删掉。
fn remove_blob_files(core: &Core, plan: &PurgePlan, capture_id: &str) {
    let Some(root) = &core.root else {
        return;
    };
    let mut deleted = Vec::new();
    let mut failures = Vec::new();
    for blob in &plan.blobs {
        // 别的资产还在用同一个 blob：文件不能删。
        let still_shared = core
            .conn
            .query_row(
                "SELECT COUNT(*) FROM blobs WHERE sha256 = ?1",
                params![blob.sha256],
                |row| row.get::<_, i64>(0),
            )
            .unwrap_or(1)
            > 0;
        if still_shared {
            continue;
        }
        let path = root.join(&blob.object_ref);
        match fs::remove_file(&path) {
            Ok(()) => deleted.push(blob.object_ref.clone()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                deleted.push(blob.object_ref.clone());
            }
            Err(error) => failures.push(format!("{}: {error}", blob.object_ref)),
        }
    }
    let entry = PurgeAudit {
        at: support::to_iso(support::now()),
        capture_id: capture_id.to_owned(),
        asset_ids: plan.asset_ids.clone(),
        deleted_blobs: deleted,
        failures,
    };
    let diagnostics = root.join("diagnostics");
    if fs::create_dir_all(&diagnostics).is_err() {
        return;
    }
    let line = serde_json::to_string(&entry).unwrap_or_else(|_| "{}".to_owned());
    use std::io::Write;
    if let Ok(mut file) = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(diagnostics.join("purge.jsonl"))
    {
        let _ = writeln!(file, "{line}");
    }
}

#[derive(Serialize)]
struct PurgeAudit {
    at: String,
    capture_id: String,
    asset_ids: Vec<String>,
    deleted_blobs: Vec<String>,
    failures: Vec<String>,
}

// ------------------------------------------------------------------ 工具

fn previous_state(conn: &rusqlite::Connection, capture_id: &str) -> Result<CaptureState> {
    let stored: Option<String> = conn.query_row(
        "SELECT trashed_from_state FROM captures WHERE id = ?1",
        params![capture_id],
        |row| row.get(0),
    )?;
    match stored.as_deref() {
        Some("draft") => Ok(CaptureState::Draft),
        Some("committed") => Ok(CaptureState::Committed),
        // 升级前没有回收站入口，NULL 是「不知道」；按 committed 解释，见函数说明。
        None => Ok(CaptureState::Committed),
        Some(other) => Err(CoreError::CorruptedData {
            message: format!("{capture_id} 的 trashed_from_state 是未知状态：{other}"),
        }),
    }
}

fn validate_day_key(value: &str) -> Result<()> {
    let parsed =
        NaiveDate::parse_from_str(value, "%Y-%m-%d").map_err(|_| CoreError::InvalidArgument {
            reason: format!("dayKey 必须是 YYYY-MM-DD，实际是「{value}」"),
        })?;
    if parsed.format("%Y-%m-%d").to_string() != value {
        return Err(CoreError::InvalidArgument {
            reason: format!("dayKey 必须是 YYYY-MM-DD，实际是「{value}」"),
        });
    }
    Ok(())
}

fn invalid_state(capture: &Capture, reason: &str) -> CoreError {
    CoreError::InvalidState {
        entity: "记录",
        id: capture.id.clone(),
        state: format!("{}（{reason}）", capture.state.wire()),
    }
}

fn preview_error(token: &str, reason: &str) -> CoreError {
    CoreError::InvalidState {
        entity: "清理预览",
        id: token.to_owned(),
        state: reason.to_owned(),
    }
}

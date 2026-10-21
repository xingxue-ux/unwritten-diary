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

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Component, Path, PathBuf};

use chrono::{DateTime, Duration, NaiveDate, Utc};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
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
#[derive(Clone)]
pub(crate) struct PurgePreviewState {
    capture_id: String,
    /// 预览那一刻记录的 revision。预览之后任何会改变清单的写入都会推进它。
    capture_revision: i64,
    expires_at: DateTime<Utc>,
    asset_ids: Vec<String>,
    derived_content_ids: Vec<String>,
    diary_version_ids: Vec<String>,
    derived_diary_version_ids: Vec<String>,
    undetermined_diary_version_ids: Vec<String>,
    ai_response_ids: Vec<String>,
    retained_snapshot_ids: Vec<String>,
}

/// 一次清理会碰到的全部行。预览与清理共用同一个盘点函数，避免两处口径漂移。
struct PurgePlan {
    capture: Capture,
    asset_ids: Vec<String>,
    derived_content_ids: Vec<String>,
    /// 与这条记录有可判定关联、可由 `delete_diary_versions` 一并删除的版本。
    diary_version_ids: Vec<String>,
    /// 由上面那些版本派生/合并来的版本：**不删**，如实列出。
    derived_diary_version_ids: Vec<String>,
    /// 归属判不出来的版本：**不删**，如实列出（判不出来就不删，更不按整日全删）。
    undetermined_diary_version_ids: Vec<String>,
    ai_response_ids: Vec<String>,
    retained_snapshot_ids: Vec<String>,
    /// 这条记录的来源与修订 id：用来认出「结果里带正文」的幂等回执并清掉。
    source_ids: Vec<String>,
    revision_ids: Vec<String>,
    blobs: Vec<BlobRef>,
    /// 这条记录的导入/录音遗留的暂存路径（`staging/...`）。已收尾的会话通常已经
    /// 自己删掉；`error` 状态的导入会故意保留暂存文件，purge 时不能把它留成孤儿。
    staging_paths: Vec<String>,
}

/// 还没收尾的录音/导入会话。它们一旦被 trash/purge 静默丢掉，用户丢的是一整段
/// 正在录的音频或正在复制的原件；所以在两个入口都拒绝，并要求先结束或取消。
struct InFlightSession {
    kind: &'static str,
    id: String,
    state: String,
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
    // 正在录音/导入的记录不能进回收站：trash 本身不丢数据，但收起后写入口是只读的，
    // 用户会以为录音还在继续；更危险的是接着 purge 会把在途会话与暂存材料一起删掉。
    // 要求先结束或取消，把「这段音频还在不在」变成用户看得见的决定。
    ensure_no_in_flight(&core.conn, &capture)?;

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
/// 日记版本按 [`scan_diary_versions`] 分三类，都如实返回：会随 `deleteDiaryVersions`
/// 删除的（`diary_versions_still_referencing`）、派生出来但**不删**的
/// （`derived_diary_version_ids`）、归属判不出来因此**不删**的
/// （`undetermined_diary_version_ids`）。后两类绝不省略：省略就等于告诉用户「没有」。
///
/// 只允许回收站里的记录：清理不可逆，产品路径上用户是先「移入回收站」再决定
/// 「永久清理」。令牌绑定 `capture_id`、一次性、带过期；`purge` 必须原样带上。
pub(crate) fn preview_purge(core: &mut Core, capture_id: &str) -> Result<PurgePreview> {
    let capture = crate::load_capture(&core.conn, capture_id)?;
    if capture.state != CaptureState::Trashed {
        return Err(invalid_state(&capture, "不在回收站里，不能永久清理"));
    }
    ensure_no_in_flight(&core.conn, &capture)?;
    let plan = plan(&core.conn, capture_id)?;

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
            derived_diary_version_ids: plan.derived_diary_version_ids.clone(),
            undetermined_diary_version_ids: plan.undetermined_diary_version_ids.clone(),
            ai_response_ids: plan.ai_response_ids.clone(),
            retained_snapshot_ids: plan.retained_snapshot_ids.clone(),
        },
    );

    Ok(PurgePreview {
        capture_id: plan.capture.id,
        preview_token: token,
        asset_ids: plan.asset_ids,
        derived_content_ids: plan.derived_content_ids,
        diary_versions_still_referencing: plan.diary_version_ids,
        ai_response_ids: plan.ai_response_ids,
        retained_source_snapshot_ids: plan.retained_snapshot_ids,
        derived_diary_version_ids: plan.derived_diary_version_ids,
        undetermined_diary_version_ids: plan.undetermined_diary_version_ids,
    })
}

/// 永久清理。**只由明确的用户操作发起**：必须带 `previewPurge` 发出的一次性令牌，
/// 且令牌里的清单与当前库里的清单逐项一致。
///
/// 删除范围（逐表）：
/// - `diary_versions`（**有可判定关联**的版本，见 [`scan_diary_versions`]；
///   `delete_diary_versions = true` 才删。删时当前版本若在其中，把
///   `diary_days.current_version_id` 换成同一天剩下的最新一版，没有就置 NULL；false 时
///   一个版本都不动，结果用 `retained_diary_version_ids` 返回）。
///   **派生版本与判不出归属的版本一个都不删**，但必须在结果的
///   `derived_diary_version_ids` / `undetermined_diary_version_ids` 里如实回填——
///   「没删」要说出来，不能报成「没有」；
/// - `extracted_contents`（级联 `extracted_segments` → `search_docs` → `search_grams`）；
/// - `captures`（级联 `source_items` → `source_revisions`、`import_sessions`、
///   `recording_sessions`、`chunk_spans`、`search_capture_docs` → `search_capture_grams`）；
/// - `assets`（只删确实属于这条记录的）与不再被任何资产引用的 `blobs`，以及它们在
///   文件系统里的内容文件；
/// - 该记录所在日子的 `text_chunks` / `chunk_spans` / `chunk_vectors`：**整日重打包**，
///   这样同一天别的记录的块与向量不会被误删，被清理记录的文字也不会留在块正文里；
/// - 引用这条记录的 `ai_responses`（请求文字与生成正文一起删，结果里列出 id）；
/// - 该记录**非 `captures.purge`** 的幂等回执：`captures.trash` / `commit` /
///   `revise_text` 等的 `result_json` 里带正文，不删的话 purge 之后拿同一个
///   `operationId` 重放会把被删的正文原样吐回来。`captures.purge` 自己的回执保留，
///   重放仍返回同一份 `PurgeResult`（里面只有 id 与计数，没有正文）。
///
/// **不删** `domain_events`（审计事件）与 `diary_source_snapshots`：后者是生成过程的
/// 不可变审计记录，且没有「快照 → 来源」映射，无法外科式脱敏（#161 方案 A）。受影响的
/// 快照 id 在预览与结果的 `retained_source_snapshot_ids` 里如实返回，**不假装删干净**。
pub(crate) fn purge(
    core: &mut Core,
    capture_id: &str,
    preview_token: &str,
    delete_diary_versions: bool,
    operation_id: &str,
) -> Result<PurgeResult> {
    let fingerprint = support::fingerprint(&[
        "captures.purge",
        capture_id,
        preview_token,
        if delete_diary_versions {
            "delete_diary_versions"
        } else {
            "keep_diary_versions"
        },
    ]);
    if let Some(json) = core.receipt("captures.purge", &fingerprint, operation_id)? {
        return Ok(serde_json::from_str(&json)?);
    }

    let now = support::now();
    let state = match core.purge_previews.get(preview_token) {
        Some(state) => state.clone(),
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

    // **IMMEDIATE + 盘点在事务里**：预览与清理之间的窗口不能再被别的连接插进来；
    // 清单与删除对着同一份快照，不会出现「查的时候有、删的时候换了」的 TOCTOU。
    crate::assets::reap_expired_leases(core);
    let tx = core
        .conn
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    let foreign_keys: i64 = tx.query_row("PRAGMA foreign_keys", [], |row| row.get(0))?;
    if foreign_keys != 1 {
        return Err(invalid_state(
            &crate::load_capture(&tx, capture_id)?,
            "外键约束没有开启，清理已拒绝（级联删除不可靠时不删任何东西）",
        ));
    }
    let plan = plan(&tx, capture_id)?;
    if plan.capture.state != CaptureState::Trashed {
        return Err(invalid_state(&plan.capture, "不在回收站里，不能永久清理"));
    }
    // 防御旧库/异常路径：trash 已经在入口拒绝在途会话，这里再挡一次，绝不静默丢掉
    // 一段正在录的音频或正在复制的原件。
    ensure_no_in_flight(&tx, &plan.capture)?;
    // 有原件正在被预览/播放/导出时拒绝清理：Windows 上打开的文件删不掉，先让调用方
    // 关闭租约，而不是「库删了、文件还在」或反过来（#155 复审 P2-5）。
    if let Some(root) = core.root.as_deref() {
        ensure_no_active_lease(&core.leases, root, &plan)?;
    }
    // 预览之后清单或 revision 变了：不按旧清单删东西，让用户再看一眼。
    if plan.capture.revision != state.capture_revision
        || plan.asset_ids != state.asset_ids
        || plan.derived_content_ids != state.derived_content_ids
        || plan.diary_version_ids != state.diary_version_ids
        || plan.derived_diary_version_ids != state.derived_diary_version_ids
        || plan.undetermined_diary_version_ids != state.undetermined_diary_version_ids
        || plan.ai_response_ids != state.ai_response_ids
        || plan.retained_snapshot_ids != state.retained_snapshot_ids
    {
        drop(tx);
        core.purge_previews.remove(preview_token);
        return Err(preview_error(
            preview_token,
            "预览之后记录内容有变化，请重新预览",
        ));
    }

    if delete_diary_versions {
        adjust_diary_days(&tx, &plan.diary_version_ids)?;
        delete_ids(&tx, "diary_versions", &plan.diary_version_ids)?;
    }
    delete_ids(&tx, "extracted_contents", &plan.derived_content_ids)?;
    delete_ids(&tx, "ai_responses", &plan.ai_response_ids)?;
    // 清掉带正文的非 purge 回执：purge 之后同一个 operationId 重放不能再吐出正文。
    let cleaned_receipt_count = delete_affected_receipts(&tx, &plan)?;
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

    let result = PurgeResult {
        capture_id: plan.capture.id.clone(),
        purged_asset_ids: plan.asset_ids.clone(),
        purged_derived_content_ids: plan.derived_content_ids.clone(),
        purged_diary_version_ids: if delete_diary_versions {
            plan.diary_version_ids.clone()
        } else {
            Vec::new()
        },
        purged_ai_response_ids: plan.ai_response_ids.clone(),
        retained_diary_version_ids: if delete_diary_versions {
            Vec::new()
        } else {
            plan.diary_version_ids.clone()
        },
        retained_source_snapshot_ids: plan.retained_snapshot_ids.clone(),
        // 这两类**一个都不删**（前者正文是用户确认的内容，后者归属判不出来），
        // 与 `delete_diary_versions` 无关：结果里如实回填，不假装删干净。
        derived_diary_version_ids: plan.derived_diary_version_ids.clone(),
        undetermined_diary_version_ids: plan.undetermined_diary_version_ids.clone(),
        cleaned_receipt_count,
    };

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
    remove_blob_files(core, &plan, &result);
    Ok(result)
}

// ------------------------------------------------------------------ 盘点

fn plan(conn: &Connection, capture_id: &str) -> Result<PurgePlan> {
    let capture = crate::load_capture(conn, capture_id)?;
    let source_ids: Vec<String> = {
        let mut statement = conn.prepare(
            "SELECT source_id FROM source_items WHERE capture_id = ?1 ORDER BY position, source_id",
        )?;
        let rows = statement
            .query_map(params![capture_id], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };

    let revision_ids: Vec<String> = {
        let mut statement = conn.prepare(
            "SELECT revision_id FROM source_revisions \
             WHERE source_id IN (SELECT source_id FROM source_items WHERE capture_id = ?1) \
             ORDER BY revision_id",
        )?;
        let rows = statement
            .query_map(params![capture_id], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };

    let mut asset_ids: Vec<String> = {
        let mut statement = conn.prepare(
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
        let mut statement = conn.prepare(
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
        let mut statement = conn.prepare(
            "SELECT id FROM extracted_contents \
             WHERE source_id IN (SELECT source_id FROM source_items WHERE capture_id = ?1) \
             ORDER BY id",
        )?;
        let rows = statement
            .query_map(params![capture_id], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };

    let versions = scan_diary_versions(conn, capture_id, &source_ids)?;
    let ai_response_ids = ai_responses_referencing(conn, capture_id)?;
    let retained_snapshot_ids = snapshots_referencing(conn, capture_id)?;
    let staging_paths = staging_paths(conn, capture_id)?;
    Ok(PurgePlan {
        capture,
        asset_ids,
        derived_content_ids,
        diary_version_ids: versions.referencing,
        derived_diary_version_ids: versions.derived,
        undetermined_diary_version_ids: versions.undetermined,
        ai_response_ids,
        retained_snapshot_ids,
        source_ids,
        revision_ids,
        blobs,
        staging_paths,
    })
}

/// 这条记录的导入/录音会话留下的暂存路径。`error` 状态的导入会故意保留暂存文件
/// （让用户重试或取消），所以永久清理必须自己收尾，不能把它留成没人认领的孤儿。
fn staging_paths(conn: &Connection, capture_id: &str) -> Result<Vec<String>> {
    let mut paths: Vec<String> = Vec::new();
    for table in ["import_sessions", "recording_sessions"] {
        let sql = format!(
            "SELECT DISTINCT staging_rel_path FROM {table} WHERE capture_id = ?1 \
             AND staging_rel_path <> '' ORDER BY staging_rel_path"
        );
        let mut statement = conn.prepare(&sql)?;
        let rows = statement
            .query_map(params![capture_id], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        paths.extend(rows);
    }
    paths.sort();
    paths.dedup();
    Ok(paths)
}

/// 日记版本的三种判定（#155 复审）。
struct DiaryVersionScan {
    /// 与这条记录**有可判定关联**的版本：`provenance_links` 指向它的来源，**或**
    /// 这一版的输入快照里冻结着它的材料（`materials[].capture_id`）。
    referencing: Vec<String>,
    /// 由 `referencing` 里的版本派生/合并来的版本（`user_edit` / `merge`）。
    derived: Vec<String>,
    /// 归属判不出来的版本。**一个都不删**，如实列出。
    undetermined: Vec<String>,
}

/// 一行 `diary_versions` 在关联判定里要用到的字段。
struct VersionRow {
    id: String,
    origin: String,
    snapshot_id: Option<String>,
    /// `provenance_links` 反序列化结果；`None` = 这一行解析不了（按判不出来处理）。
    links: Option<Vec<crate::model::ProvenanceLink>>,
    parent: Option<String>,
    /// `generationInfo.generationOptions.mergeSourceVersionIds`：合并的输入版本。
    /// `Some(空)` = 明确没有合并输入；`None` = 没写或解析不了。
    merge_sources: Option<Vec<String>>,
}

/// 盘点仍与这条记录有关的日记版本。
///
/// **为什么不能只看 `provenance_links`**（#155 复审）：草稿期生成时记录还没有
/// `source_items`（提交才把草稿正文复制成一条文字修订），快照里的那份材料
/// `source_id: None`，`link_for_material` 因此不写 provenance link。于是
/// 「来源为空 → 返回空」会把**明明是拿这条记录正文生成的版本**漏掉：确认框报
/// 「没有关联版本」，用户勾了删除也删不掉它——**界面说了谎**。
///
/// 判定口径（都是可判定的列/字段，不扫正文文本）：
/// 1. **来源关联**：`provenance_links` 里有一条 `source_id` 属于这条记录；
/// 2. **输入快照关联**：`diary_versions.source_snapshot_id` → `diary_source_snapshots.body`
///    的 `materials[]` 里有一条 `capture_id` 是这条记录。快照是生成输入的冻结清单，
///    材料条目**一定带 `capture_id`**（草稿材料缺的只是 `source_id`），所以这条关联
///    在草稿期也判得出来；
/// 3. **派生**：`parent_version_id`（用户编辑）或 `mergeSourceVersionIds`（合并）链上
///    能走到 1 / 2 认出来的版本。正文是用户确认过的内容，**不删**，但可能仍含被清理
///    记录的文字，必须列出；
/// 4. 剩下的按谱系判断：能确定「输入里没有这条记录」的（快照解析得出来、材料里没有它）
///    不算关联；**判不出来的**（快照正文解析不了、父版本行已经不在、合并输入读不出来）
///    进 `undetermined`——查不出来 ≠ 没有，**不删**，如实报。
///
/// 绝不按整日全删：判不出来时宁可少删并如实报，也不连累同一天其他记录的内容。
fn scan_diary_versions(
    conn: &rusqlite::Connection,
    capture_id: &str,
    source_ids: &[String],
) -> Result<DiaryVersionScan> {
    let wanted: HashSet<&str> = source_ids.iter().map(String::as_str).collect();
    let rows: Vec<VersionRow> = {
        let mut statement = conn.prepare(
            "SELECT id, origin, source_snapshot_id, provenance_links, parent_version_id, \
             generation_info FROM diary_versions ORDER BY created_at DESC, id DESC",
        )?;
        let collected = statement
            .query_map([], |row| {
                let links_json: String = row.get(3)?;
                let generation_info: Option<String> = row.get(5)?;
                Ok(VersionRow {
                    id: row.get(0)?,
                    origin: row.get(1)?,
                    snapshot_id: row.get(2)?,
                    links: serde_json::from_str(&links_json).ok(),
                    parent: row.get(4)?,
                    merge_sources: merge_source_ids(generation_info.as_deref()),
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        collected
    };

    // 同一份快照可能被多版引用：解析一次，结果复用。`None` = 解析不了。
    let mut snapshot_mentions: HashMap<String, Option<bool>> = HashMap::new();
    let mut status: HashMap<String, VersionStatus> = HashMap::new();

    for row in &rows {
        let direct = row
            .links
            .as_ref()
            .map(|links| {
                links
                    .iter()
                    .any(|link| wanted.contains(link.source_id.as_str()))
            })
            .unwrap_or(false);
        if direct {
            status.insert(row.id.clone(), VersionStatus::Referencing);
            continue;
        }
        // `provenance_links` 读不出来：这一行的关联无从核对，按判不出来处理。
        if row.links.is_none() {
            status.insert(row.id.clone(), VersionStatus::Undetermined);
            continue;
        }
        let Some(snapshot_id) = row.snapshot_id.as_deref() else {
            // 没有输入快照（用户编辑 / 合并 / 旧库）：谱系在下面统一解析。
            continue;
        };
        let mentions = snapshot_mentions
            .entry(snapshot_id.to_owned())
            .or_insert_with(|| snapshot_mentions_capture_or_none(conn, snapshot_id, capture_id));
        match mentions {
            Some(true) => {
                status.insert(row.id.clone(), VersionStatus::Referencing);
            }
            // 快照解析得出来、材料里没有这条记录：确定不相关（材料可能来自同一天别的记录）。
            Some(false) => {
                status.insert(row.id.clone(), VersionStatus::Unrelated);
            }
            // 快照正文解析不了：**不能**替它保证没留这条记录的文字，按判不出来处理。
            None => {
                status.insert(row.id.clone(), VersionStatus::Undetermined);
            }
        }
    }

    // 谱系传递：派生关系可能跨多版（编辑再编辑、合并再合并），迭代到不动点。
    // 表很小且这是清理路径，迭代写法比递归/拓扑排序更好核对。
    loop {
        let mut changed = false;
        for row in &rows {
            if status.contains_key(&row.id) {
                continue;
            }
            // 快照解析不了的行已经在上面定成 Undetermined，这里只剩真的没有快照的行。
            let Some(inputs) = lineage_inputs(row) else {
                // 合并信息读不出来：这一版的输入看不见，判不出来就不删。
                status.insert(row.id.clone(), VersionStatus::Undetermined);
                changed = true;
                continue;
            };
            if inputs.is_empty() {
                // 没有快照、也没有任何谱系：这一版是从哪儿来的查不出来。
                status.insert(row.id.clone(), VersionStatus::Undetermined);
                changed = true;
                continue;
            }
            let mut derived = false;
            let mut unknown = false;
            for input in &inputs {
                match status.get(input.as_str()) {
                    Some(VersionStatus::Referencing) | Some(VersionStatus::Derived) => {
                        derived = true
                    }
                    Some(VersionStatus::Undetermined) => unknown = true,
                    // 输入是「确定不相关」：这一条边不构成关联。
                    Some(VersionStatus::Unrelated) => {}
                    // 输入行不在（父版本已经没了）或还没轮到：先当判不出来。
                    None => unknown = true,
                }
            }
            if derived {
                status.insert(row.id.clone(), VersionStatus::Derived);
                changed = true;
            } else if !unknown {
                status.insert(row.id.clone(), VersionStatus::Unrelated);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    let mut scan = DiaryVersionScan {
        referencing: Vec::new(),
        derived: Vec::new(),
        undetermined: Vec::new(),
    };
    for row in &rows {
        match status.get(row.id.as_str()) {
            Some(VersionStatus::Referencing) => scan.referencing.push(row.id.clone()),
            Some(VersionStatus::Derived) => scan.derived.push(row.id.clone()),
            Some(VersionStatus::Undetermined) | None => scan.undetermined.push(row.id.clone()),
            Some(VersionStatus::Unrelated) => {}
        }
    }
    Ok(scan)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum VersionStatus {
    /// 判得出来：这一版与这条记录有关联。
    Referencing,
    /// 判得出来：这一版派生自有关联的版本。
    Derived,
    /// 判得出来：这一版与这条记录无关。
    Unrelated,
    /// 判不出来（宁可少删，如实报）。
    Undetermined,
}

/// 这一版的直接输入版本：父版本 + 合并来源。
///
/// `None` = 谱系读不出来（`origin = merge` 却解析不了 `generationInfo`）：这时**不能**
/// 当成「没有输入」，要按判不出来处理。
fn lineage_inputs(row: &VersionRow) -> Option<Vec<String>> {
    let mut inputs: Vec<String> = Vec::new();
    if let Some(parent) = row.parent.as_deref().filter(|id| !id.is_empty()) {
        inputs.push(parent.to_owned());
    }
    // 合并版本的输入记在 generationInfo 里；读不出来就等于看不见它的输入。
    if row.origin == "merge" && row.merge_sources.is_none() {
        return None;
    }
    if let Some(sources) = &row.merge_sources {
        inputs.extend(sources.iter().cloned());
    }
    inputs.sort();
    inputs.dedup();
    Some(inputs)
}

/// `generationInfo` 里的 `generationOptions.mergeSourceVersionIds`（逗号分隔）。
///
/// 落库的 JSON 是 Rust DTO 的序列化结果，**字段名是 snake_case**
/// （`generation_options`）；只有这张 map 里的键是契约的 camelCase
/// （`mergeSourceVersionIds`，见 `merge_generation_info`）。
/// `None` = 这一行没有生成信息、或它不是一次合并（没有这一项）、或解析不了。
fn merge_source_ids(generation_info: Option<&str>) -> Option<Vec<String>> {
    let value: serde_json::Value = serde_json::from_str(generation_info?).ok()?;
    let joined = value
        .get("generation_options")?
        .get("mergeSourceVersionIds")?
        .as_str()?;
    let mut ids: Vec<String> = joined
        .split(',')
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
        .collect();
    ids.sort();
    ids.dedup();
    Some(ids)
}

/// 快照 body 里的材料有没有这条记录。`None` = 快照行不在或 body 解析不了——
/// 这时**不能**回答「没有」。
fn snapshot_mentions_capture_or_none(
    conn: &Connection,
    snapshot_id: &str,
    capture_id: &str,
) -> Option<bool> {
    let body: Option<String> = conn
        .query_row(
            "SELECT body FROM diary_source_snapshots WHERE id = ?1",
            params![snapshot_id],
            |row| row.get(0),
        )
        .optional()
        .ok()
        .flatten();
    let value: serde_json::Value = serde_json::from_str(body.as_deref()?).ok()?;
    Some(snapshot_mentions_capture(&value, capture_id))
}

/// 仍引用这条记录的 AI 回应。`capture_ids` 是 JSON 数组，按元素精确比对，
/// 不做 `LIKE`：回应是围绕这条记录发起的，来源被永久清理后它的请求文字与生成正文
/// 也一起删除（口径写进契约 §8）。
fn ai_responses_referencing(conn: &Connection, capture_id: &str) -> Result<Vec<String>> {
    let mut statement = conn.prepare(
        "SELECT id FROM ai_responses \
         WHERE json_valid(capture_ids) \
           AND EXISTS (SELECT 1 FROM json_each(ai_responses.capture_ids) WHERE value = ?1) \
         ORDER BY id",
    )?;
    let rows = statement
        .query_map(params![capture_id], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// 冻结了这条记录正文的日记输入快照。
///
/// 快照没有「快照 → 来源」映射，无法把某一条材料从已冻结的 `body` 里摘掉（#161）。
/// 这里只**如实盘点**：先按 `"capture_id":"id"` 做一次索引外的候选筛选，再逐行解析
/// `materials[]` 确认——列出的一定是真的引用，不会把正文里恰好出现同一串文本的行
/// 算进来。清理不删这些行；预览与结果把 id 原样交给调用方。
fn snapshots_referencing(conn: &Connection, capture_id: &str) -> Result<Vec<String>> {
    let rows: Vec<(String, String)> = {
        let mut statement = conn.prepare(
            "SELECT id, body FROM diary_source_snapshots WHERE instr(body, ?1) > 0 ORDER BY id",
        )?;
        let collected = statement
            .query_map(params![capture_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        collected
    };
    let mut ids = Vec::new();
    for (id, body) in rows {
        let affected = match serde_json::from_str::<serde_json::Value>(&body) {
            Ok(value) => snapshot_mentions_capture(&value, capture_id),
            // 原始 body 里确实出现了这条记录的标识、但 JSON 解析不了：按受影响列表，
            // 宁可多报一份「可能保留了正文」的快照，也不替它保证没留。
            Err(_) => true,
        };
        if affected {
            ids.push(id);
        }
    }
    Ok(ids)
}

fn snapshot_mentions_capture(value: &serde_json::Value, capture_id: &str) -> bool {
    value
        .get("materials")
        .and_then(serde_json::Value::as_array)
        .map(|materials| {
            materials.iter().any(|material| {
                material.get("capture_id").and_then(serde_json::Value::as_str)
                    == Some(capture_id)
            })
        })
        .unwrap_or(false)
}

/// 删掉这条记录**非 `captures.purge`** 的幂等回执，返回删了几条。
///
/// `captures.trash` / `commit` / `revise_text` 的 `result_json` 里带正文；purge 之后
/// 同一个 `operationId` 重放会先命中回执、把正文原样吐回来。按 id 精确比对（不扫文本），
/// 只删结果里真的提到这条记录/它的来源/修订的回执；`captures.purge` 自己的回执保留，
/// 重放仍返回同一份 `PurgeResult`（只有 id 与计数，没有正文）。
fn delete_affected_receipts(
    tx: &rusqlite::Transaction<'_>,
    plan: &PurgePlan,
) -> Result<i64> {
    let mut wanted: HashSet<&str> = HashSet::new();
    wanted.insert(plan.capture.id.as_str());
    for id in &plan.source_ids {
        wanted.insert(id.as_str());
    }
    for id in &plan.revision_ids {
        wanted.insert(id.as_str());
    }
    if wanted.is_empty() {
        return Ok(0);
    }
    let rows: Vec<(String, String)> = {
        let mut statement = tx.prepare(
            "SELECT operation_id, result_json FROM operation_receipts \
             WHERE kind <> 'captures.purge' ORDER BY operation_id",
        )?;
        let collected = statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        collected
    };
    let mut deleted = 0_i64;
    for (operation_id, result_json) in rows {
        let affected = serde_json::from_str::<serde_json::Value>(&result_json)
            .map(|value| json_mentions_any(&value, &wanted))
            .unwrap_or(false);
        if affected {
            tx.execute(
                "DELETE FROM operation_receipts WHERE operation_id = ?1",
                params![operation_id],
            )?;
            deleted += 1;
        }
    }
    Ok(deleted)
}

fn json_mentions_any(value: &serde_json::Value, wanted: &HashSet<&str>) -> bool {
    match value {
        serde_json::Value::String(text) => wanted.contains(text.as_str()),
        serde_json::Value::Array(items) => items.iter().any(|item| json_mentions_any(item, wanted)),
        serde_json::Value::Object(map) => map
            .values()
            .any(|item| json_mentions_any(item, wanted)),
        _ => false,
    }
}

/// 正在录音/导入的会话：这些状态下会话还没有收尾，材料可能只在暂存区。
fn in_flight_sessions(conn: &Connection, capture_id: &str) -> Result<Vec<InFlightSession>> {
    let mut sessions = Vec::new();
    {
        let mut statement = conn.prepare(
            "SELECT id, state FROM recording_sessions WHERE capture_id = ?1 \
             AND state IN ('preparing', 'recording', 'paused', 'stopping', 'interrupted', \
             'recoverable') ORDER BY id",
        )?;
        let rows = statement
            .query_map(params![capture_id], |row| {
                Ok(InFlightSession {
                    kind: "录音",
                    id: row.get(0)?,
                    state: row.get(1)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        sessions.extend(rows);
    }
    {
        let mut statement = conn.prepare(
            "SELECT id, state FROM import_sessions WHERE capture_id = ?1 \
             AND state IN ('prepared', 'copying', 'verifying', 'recoverable') ORDER BY id",
        )?;
        let rows = statement
            .query_map(params![capture_id], |row| {
                Ok(InFlightSession {
                    kind: "导入",
                    id: row.get(0)?,
                    state: row.get(1)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        sessions.extend(rows);
    }
    Ok(sessions)
}

/// 还有没收尾的录音/导入就拒绝 trash/purge：先结束或取消，把「这段音频/这份原件
/// 还在不在」变成用户看得见的决定，而不是清理时静默连带删掉。
fn ensure_no_in_flight(conn: &Connection, capture: &Capture) -> Result<()> {
    let sessions = in_flight_sessions(conn, &capture.id)?;
    if sessions.is_empty() {
        return Ok(());
    }
    let list = sessions
        .iter()
        .map(|session| format!("{} {}（{}）", session.kind, session.id, session.state))
        .collect::<Vec<_>>()
        .join("、");
    Err(CoreError::InvalidState {
        entity: "记录",
        id: capture.id.clone(),
        state: format!(
            "还有未收尾的录音/导入：{list}；先结束或取消它们，再移入回收站或永久清理"
        ),
    })
}

/// 有原件正在被预览/播放/导出时拒绝清理：删一个打开着的文件在 Windows 上会失败，
/// 而「库行删了、文件还在」又是另一种不诚实。先让调用方释放租约再试。
fn ensure_no_active_lease(
    leases: &HashMap<String, crate::assets::LeaseRecord>,
    root: &Path,
    plan: &PurgePlan,
) -> Result<()> {
    for blob in &plan.blobs {
        let Some(path) = safe_blob_path(root, &blob.object_ref) else {
            continue;
        };
        if let Some((lease_id, _)) = leases
            .iter()
            .find(|(_, lease)| Path::new(&lease.handle) == path)
        {
            return Err(CoreError::InvalidState {
                entity: "资产租约",
                id: lease_id.clone(),
                state: "原件正在预览/播放/导出，先关闭再永久清理".to_owned(),
            });
        }
    }
    Ok(())
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
/// 追加写、一行一个清理，记录这次真的删了哪些行、哪些 blob 文件、哪些失败。写日志失败
/// **不影响清理结果**（事务已经提交），也不向调用方报错——清理已经完成，再报一个
/// 「日志写不了」只会让用户以为资料没删掉。
fn remove_blob_files(core: &Core, plan: &PurgePlan, result: &PurgeResult) {
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
        // 对象路径来自数据库：只允许 `blobs/...` 下的普通路径，绝不跟着 `..` 或绝对
        // 路径删到资料库外面去（#155 复审 P2-6）。
        let Some(path) = safe_blob_path(root, &blob.object_ref) else {
            failures.push(format!("{}: 越界的对象路径，拒绝删除", blob.object_ref));
            continue;
        };
        match fs::remove_file(&path) {
            Ok(()) => deleted.push(blob.object_ref.clone()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                deleted.push(blob.object_ref.clone());
            }
            Err(error) => failures.push(format!("{}: {error}", blob.object_ref)),
        }
    }
    // 导入/录音的暂存文件不在事务里，也不属于 blobs：error 状态的导入会故意保留
    // 暂存文件，purge 要收尾（#155 复审 P2-4）。
    let mut deleted_staging = Vec::new();
    for relative in &plan.staging_paths {
        let Some(path) = safe_relative_path(root, relative, "staging") else {
            failures.push(format!("{relative}: 越界的暂存路径，拒绝删除"));
            continue;
        };
        let removed = match fs::metadata(&path) {
            Ok(meta) if meta.is_dir() => fs::remove_dir_all(&path),
            Ok(_) => fs::remove_file(&path),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        };
        match removed {
            Ok(()) => deleted_staging.push(relative.clone()),
            Err(error) => failures.push(format!("{relative}: {error}")),
        }
    }
    let entry = PurgeAudit {
        at: support::to_iso(support::now()),
        capture_id: result.capture_id.clone(),
        asset_ids: result.purged_asset_ids.clone(),
        derived_content_ids: result.purged_derived_content_ids.clone(),
        diary_version_ids: result.purged_diary_version_ids.clone(),
        retained_diary_version_ids: result.retained_diary_version_ids.clone(),
        ai_response_ids: result.purged_ai_response_ids.clone(),
        retained_snapshot_ids: result.retained_source_snapshot_ids.clone(),
        cleaned_receipt_count: result.cleaned_receipt_count,
        deleted_blobs: deleted,
        deleted_staging,
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
    derived_content_ids: Vec<String>,
    diary_version_ids: Vec<String>,
    retained_diary_version_ids: Vec<String>,
    ai_response_ids: Vec<String>,
    retained_snapshot_ids: Vec<String>,
    cleaned_receipt_count: i64,
    deleted_blobs: Vec<String>,
    deleted_staging: Vec<String>,
    failures: Vec<String>,
}

/// 只接受 `blobs/` 下的普通相对路径：`object_ref` 来自数据库，不能让它把删除带到
/// 资料库外（`..`、绝对路径、盘符都拒绝）。不是普通文件路径时返回 `None`，由调用方
/// 记为失败并保留可审计记录。
fn safe_blob_path(root: &Path, object_ref: &str) -> Option<PathBuf> {
    safe_relative_path(root, object_ref, "blobs")
}

/// `relative` 必须落在 `root/<top>/` 下且每一段都是普通名字；`..`、绝对路径、
/// 盘符都拒绝。调用方传的 `top` 是代码里的常量（`blobs` / `staging`）。
fn safe_relative_path(root: &Path, relative_text: &str, top: &str) -> Option<PathBuf> {
    let path = root.join(relative_text);
    let relative = path.strip_prefix(root).ok()?;
    let mut components = relative.components();
    match components.next() {
        Some(Component::Normal(name)) if name == top => {}
        _ => return None,
    }
    if relative.as_os_str().is_empty() {
        return None;
    }
    if !components.all(|component| matches!(component, Component::Normal(_))) {
        return None;
    }
    Some(path)
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

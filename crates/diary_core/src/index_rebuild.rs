//! `indexes.rebuild(scope, reason, operationId)` 的入口：`scope` 编码解析 + 排一条 `index` 任务。
//!
//! 契约第 4.4 节。入口**异步**：只排任务并返回它，真正的重建由 `index` 执行者
//! （`jobs::run_index`）在驱动里跑。不在这里同步重建——同步重建会阻塞界面，而
//! 队列的退避、补做与依赖闸门已经就绪（见 `docs/architecture/m4-任务调度与补做.md`）。
//!
//! # scope 编码（v1，写进契约第 4.4 节）
//!
//! ```text
//! scope := "all"
//!        | "sources:" id ("," id)*
//! ```
//!
//! - `all`：整库。任务的 `target_ids` 为空，执行者走 `rebuild_keyword_index(None)`，
//!   与 `indexes.status` 不传来源范围同义（含记录文字那一路）；
//! - `sources:<id>[,<id>]*`：只重建这些来源。id 是核心生成的**不透明字符串**
//!   （当前形如 `src_` + 32 位小写十六进制）。这个编码把 `,` 固定成分隔符，因此
//!   id 记号只允许 `[A-Za-z0-9_]`——出现别的字符（含空白）就报 `invalid_argument`，
//!   **不需要也不定义转义**；
//! - 顺序对重建没有语义（重建是「按现状重刷」）。解析时按 id 字典序排成规范形，
//!   所以 `sources:b,a` 与 `sources:a,b` 是同一次操作；
//! - 空列表（`sources:`）、空记号（`sources:a,,b`）、重复 id（`sources:a,a`）都是
//!   `invalid_argument`：「要整库」必须写 `all`；scope 的规范形是集合；
//! - 指定的来源必须真实存在，否则 `not_found`——不排一条注定 `not_found` 终态失败
//!   的任务，与驱动「依赖不可能满足就如实失败」的口径一致。
//!
//! # 幂等
//!
//! 同一个 `(scope, 原因, operationId)` 重复调用返回**同一条任务**：`operationId`
//! 走核心既有的幂等回执（契约 1.2 节，`operation_receipts`），换了 `scope` 或原因
//! 则报 `idempotency_conflict`。回执里只存任务 id，重放时按 id **重新读当前任务**，
//! 所以返回的永远是这条任务此刻的真实状态，而不是入队那一刻的旧快照。
//! 任务自身的业务键 `(kind, target_ids, input_snapshot_hash)` 照 #67 的口径一并生效。

use rusqlite::params;

use crate::error::{CoreError, Result};
use crate::model::{Job, JobPriority, NewJob};
use crate::support;
use crate::Core;

/// 整库 scope 的字面量。共享场景文件里已经这么写（`indexes.rebuild(scope=all)`）。
pub(crate) const SCOPE_ALL: &str = "all";

/// 指定来源 scope 的前缀。
pub(crate) const SCOPE_SOURCES_PREFIX: &str = "sources:";

/// 幂等回执的 kind。与契约方法名一致，便于排查。
const RECEIPT_KIND: &str = "indexes.rebuild";

/// 解析并规范化后的 scope。
#[derive(Debug)]
pub(crate) struct RebuildScope {
    /// 规范形：已排序、无重复。**空表示整库**（与 `rebuild_keyword_index(None)` 同义）。
    pub(crate) target_ids: Vec<String>,
    /// 规范形的字符串形式，用于请求指纹与错误消息。
    pub(crate) canonical: String,
}

/// 解析 `scope` 字符串，按上面的编码规则规范化。
pub(crate) fn parse_scope(scope: &str) -> Result<RebuildScope> {
    if scope == SCOPE_ALL {
        return Ok(RebuildScope {
            target_ids: Vec::new(),
            canonical: SCOPE_ALL.to_owned(),
        });
    }

    let Some(list) = scope.strip_prefix(SCOPE_SOURCES_PREFIX) else {
        return Err(CoreError::InvalidArgument {
            reason: format!(
                "无法识别的 scope「{scope}」：整库写 `{SCOPE_ALL}`，指定来源写 \
                 `{SCOPE_SOURCES_PREFIX}<id>[,<id>]`"
            ),
        });
    };

    if list.is_empty() {
        return Err(CoreError::InvalidArgument {
            reason: format!(
                "`{SCOPE_SOURCES_PREFIX}` 后面至少要有一个来源 id；整库请写 `{SCOPE_ALL}`"
            ),
        });
    }

    let mut ids: Vec<String> = Vec::new();
    for token in list.split(',') {
        if token.is_empty() {
            return Err(CoreError::InvalidArgument {
                reason: format!("scope「{scope}」里有空的来源 id"),
            });
        }
        if let Some(bad) = token.chars().find(|ch| !is_id_char(*ch)) {
            return Err(CoreError::InvalidArgument {
                reason: format!(
                    "scope「{scope}」里的来源 id「{token}」含非法字符「{bad}」：\
                     id 只允许 [A-Za-z0-9_]（`,` 是列表分隔符，不定义转义）"
                ),
            });
        }
        if ids.iter().any(|existing| existing == token) {
            return Err(CoreError::InvalidArgument {
                reason: format!("scope「{scope}」里的来源 id「{token}」重复了"),
            });
        }
        ids.push(token.to_owned());
    }

    // 顺序没有语义：排成规范形，让同一个来源集合只有一种写法。
    ids.sort();
    let canonical = format!("{SCOPE_SOURCES_PREFIX}{}", ids.join(","));
    Ok(RebuildScope {
        target_ids: ids,
        canonical,
    })
}

/// id 记号允许的字符。核心当前生成的 id 是 `前缀_十六进制`，一定有逗号之外的形状；
/// `,` 被这个编码固定成分隔符，所以它不在字母表里。
fn is_id_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '_'
}

/// 排一条关键词索引重建任务，契约第 4.4 节 `indexes.rebuild`。
pub(crate) fn schedule(
    core: &mut Core,
    scope: &str,
    reason: &str,
    operation_id: &str,
) -> Result<Job> {
    let parsed = parse_scope(scope)?;

    // operationId 的通用幂等（契约 1.2）：同一个 id 换了内容要报冲突，重复同一次
    // 操作要返回原结果。scope 与原因都进指纹，换任何一个都算「换了内容」。
    //
    // 这一步**必须在校验来源之前**：重放时世界可能已经变了（那条来源被清掉了），
    // 但「同一次操作」仍然该返回原来那条任务，而不是突然报 not_found。
    let fingerprint = support::fingerprint(&[RECEIPT_KIND, &parsed.canonical, reason]);
    if let Some(json) = core.receipt(RECEIPT_KIND, &fingerprint, operation_id)? {
        let stored: RebuildReceipt = serde_json::from_str(&json)?;
        // 重放不返回入队那一刻的快照：按 id 读当前任务，状态才是真的。
        return crate::jobs::get(core, &stored.job_id);
    }

    // 指定来源必须真实存在：排一条注定 not_found 终态失败的任务，对谁都没好处。
    for id in &parsed.target_ids {
        let exists: i64 = core.conn.query_row(
            "SELECT COUNT(*) FROM source_items WHERE source_id = ?1",
            params![id],
            |row| row.get(0),
        )?;
        if exists == 0 {
            return Err(CoreError::NotFound {
                entity: "来源",
                id: id.clone(),
            });
        }
    }

    // 快照哈希也带上 operationId：任务规格属于哪一次操作，一眼看得出；业务键
    // `(kind, target_ids, input_snapshot_hash)` 也因此不会跨操作误合并。
    let snapshot =
        support::fingerprint(&[RECEIPT_KIND, &parsed.canonical, reason, operation_id]);
    let job = NewJob::new(crate::jobs::KIND_INDEX, JobPriority::Maintenance)
        .targeting(parsed.target_ids)
        .with_snapshot(snapshot);

    let tx = core.conn.transaction()?;
    // `job.changed` 由队列在入队时发，这里不重复发。
    let queued = crate::jobs::enqueue_in_tx(&tx, job)?;
    let receipt = serde_json::to_string(&RebuildReceipt {
        job_id: queued.id.clone(),
    })?;
    crate::store_receipt(&tx, operation_id, RECEIPT_KIND, &fingerprint, &receipt)?;
    tx.commit()?;
    Ok(queued)
}

/// 幂等回执里存的东西。只存 id：任务状态会变，存快照就会返回旧状态。
#[derive(serde::Serialize, serde::Deserialize)]
struct RebuildReceipt {
    job_id: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_is_whole_library() {
        let parsed = parse_scope("all").unwrap();
        assert!(parsed.target_ids.is_empty(), "整库用空目标表示");
        assert_eq!(parsed.canonical, "all");
    }

    #[test]
    fn source_lists_are_sorted_into_a_canonical_form() {
        let parsed = parse_scope("sources:src_b,src_a").unwrap();
        assert_eq!(parsed.target_ids, vec!["src_a", "src_b"]);
        assert_eq!(parsed.canonical, "sources:src_a,src_b");

        // 两种写法规范到同一个串：顺序没有语义。
        assert_eq!(
            parse_scope("sources:src_a,src_b").unwrap().canonical,
            parsed.canonical
        );
    }

    #[test]
    fn malformed_scopes_are_invalid_argument() {
        for scope in [
            "",
            "ALL",
            "all ",
            " all",
            "sources",
            "sources:",
            "sources:a,,b",
            "sources:a,a",
            "sources:src a",
            "sources:src/a",
            "src_00000000000000000000000000000000",
        ] {
            let error = parse_scope(scope).err().unwrap_or_else(|| {
                panic!("scope「{scope}」本应被拒绝");
            });
            assert_eq!(
                error.code(),
                crate::error::ErrorCode::InvalidArgument,
                "scope「{scope}」应报 invalid_argument"
            );
        }
    }

    #[test]
    fn ids_may_not_be_empty_or_repeated() {
        let empty = parse_scope("sources:").unwrap_err();
        assert!(empty.to_string().contains("至少要有一个"));
        let repeated = parse_scope("sources:src_a,src_a").unwrap_err();
        assert!(repeated.to_string().contains("重复"));
    }
}
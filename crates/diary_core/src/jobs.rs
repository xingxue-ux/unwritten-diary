//! 持久任务队列与驱动，契约第 4.4 节与任务书 7.1 节。
//!
//! 队列本身：入队（带业务键幂等）、领取、成功/失败、退避重试、取消、恢复、
//! 下次唤醒时刻与通知去重。
//!
//! 驱动（`run_due`）：把到期任务取出来，按 `kind` 找执行者跑掉，再把结果落回队列。
//! 目前 `extract`、`index`（关键词）与 `semantic_index`（语义）真有执行者；没有
//! 执行者的 `kind`（`transcribe` 等 B4a 的 Provider）**不会静默成功**，而是进
//! 「已放弃」+ `error_code = no_handler` 等人工处理（见 `execute`）。
//!
//! 长任务的分段：语义重建会远超一次唤醒的预算，所以 `execute` 拿到一个**截止点**，
//! 语义执行者在块与块之间检查它，到点就**在安全点让出**（回到 `queued`、进度留在
//! `progress` 上，下一轮接着算没算完的块）；关键词重建与提取**没有**这样的取消点，
//! 照旧一跑到底（见 `run_due` 的预算口径与 `HandlerOutcome`）。
//!
//! 优先级照任务书 7.1 节：保存与录音片段登记最高，整库重建最低。
//! 决定与口径写在 `docs/architecture/m4-任务调度与补做.md`。

use chrono::{DateTime, Duration, Utc};
use rusqlite::{params, OptionalExtension, Transaction};

use crate::error::{CoreError, Result};
use crate::model::{
    EventType, Job, JobErrorSummary, JobProgress, JobRunSummary, JobState, JobStatus, NewJob,
};
use crate::provider;
use crate::support;
use crate::Core;

/// 真有执行者的任务类型：导入完成后对来源修订跑已有提取路径。
pub(crate) const KIND_EXTRACT: &str = "extract";

/// 关键词索引任务：对 `target_ids` 里的**来源**重跑关键词索引重建。
///
/// `target_ids` 为空表示**整库**（`indexes.rebuild(scope=all)`），执行者走
/// `rebuild_keyword_index(None)`；非空则只重建这些来源。
///
/// 语义索引是**另一个** `kind`（`KIND_SEMANTIC_INDEX`）：它的前置、成败口径、
/// 分段方式与这一条都不一样，混在一个 kind 里只能靠分支猜（理由见 m4 文档）。
pub(crate) const KIND_INDEX: &str = "index";

/// 语义索引任务：重建文本块 → 开新代次 → 复用/补算向量 → 原子切换。
///
/// `target_ids` 为空 = **整库**（排队入口只排这一种），非空 = 只重算这些来源的块
/// （代次与向量仍是全局的，见 `Core::schedule_semantic_index`）。
///
/// 与关键词 `index` 的关系：**两个 kind**，不是一个 kind 的两个种类。它们是两条
/// 独立的索引路（表不同、成败口径不同、能不能分段不同），且
/// `indexes.rebuild(scope, …)` 的契约语义是「只排一条 `index` 任务」，把它扩成语义
/// 重建会改掉既有入口的行为。理由与取舍见 m4 文档。
pub(crate) const KIND_SEMANTIC_INDEX: &str = "semantic_index";

/// 「没有目标」的终态失败码。只有 `extract` 用它：`extract` 没有「整库」这种语义，
/// 空目标是实打实的错误。`index` / `semantic_index` 的空 `target_ids` 是整库，不是错误。
pub(crate) const ERROR_NO_TARGET: &str = "no_target";

/// `index` 在等什么（写在 `jobs.error_code` 上，**不改 schema**）：
/// 目标来源的当前修订还没提取完，而队列里还有一条活着的 `extract` 会写出来。
pub(crate) const WAITING_EXTRACT: &str = "waiting_extract";

/// `semantic_index` 在等什么：本地模型还没就绪（没配、或配了但不可用）。
///
/// 落在 `waiting_configuration` 而不是 `waiting_dependency`：它等的不是「别人跑完」，
/// 是**配置**（模型目录 / ORT 动态库）。所以它**不进 `next_wakeup`**——配置变化不是
/// 时间问题，下一次驱动（前台/唤醒/配置变化之后）由闸门重新评估并拉起它。
///
/// 它也不是「用户无需理会」的状态：任务带着 `requires_user_action = 1` 与
/// `attention_key = job-config:<kind>:waiting_model`，等用户去把模型配上。
pub(crate) const WAITING_MODEL: &str = "waiting_model";

/// `index` 的终态失败码：`extract` 这一环不会再有执行者来满足了（前置任务被取消、
/// 已放弃，又没有派生内容）。码里点明是哪一环，挂死不如如实失败。
pub(crate) const ERROR_DEPENDENCY_UNMET: &str = "dependency_extract_unmet";

/// 「最近错误」最多给几条：状态摘要不是日志导出。
pub(crate) const RECENT_ERROR_LIMIT: i64 = 5;

/// 重试退避：`30 秒 × 2^attempt_count`，封顶 1 小时（与 B1c 一致，这一片没改）。
///
/// `attempt_count` 是**已经用掉**的尝试次数：第一次失败进来时它是 1，所以实际
/// 从 60 秒起步，之后 2 分钟、4 分钟……30 秒那一档只在 0 次尝试时有意义，
/// 正常路径走不到。
///
/// 不用「无限重试」：次数上限由任务的 max_attempts 决定，超了就进 failed，
/// 由用户决定要不要手动重试。
fn backoff(attempt: i64) -> Duration {
    let factor = 1_i64 << attempt.clamp(0, 10) as u32;
    Duration::seconds(30_i64.saturating_mul(factor).min(3600))
}

/// 业务键查重：`(kind, target_ids, input_snapshot_hash)` 相同、且还处于**非终态**
/// 的任务视为同一条，重复入队返回它而不是新建。
///
/// 为什么这样拼键：`jobs` 表没有 `business_key` 列，这一片又不改 schema，所以键只能
/// 从现有列算。`target_ids` 用它的 JSON 文本，**数组顺序是语义的一部分**（多来源
/// 提取的顺序会影响结果），所以不做排序归一化。
///
/// 为什么事务内查就够：单写者 SQLite（`BridgeSession` 用互斥锁串行化同一个
/// `Core`）下，检查与插入在同一个事务里，中间没有别的写入者能插进来。
/// **DB 级的 UNIQUE 索引留到下一次 schema 变更再做**——那才是多进程写入也成立的
/// 保证，现在没有这个前提，也就不假装有。
fn find_active_by_business_key(tx: &Transaction<'_>, job: &NewJob) -> Result<Option<Job>> {
    let target_ids = serde_json::to_string(&job.target_ids)?;
    let found: Option<String> = tx
        .query_row(
            "SELECT id FROM jobs WHERE kind = ?1 AND target_ids = ?2 \
             AND input_snapshot_hash IS ?3 \
             AND state NOT IN ('succeeded', 'failed', 'cancelled') \
             ORDER BY created_at, id LIMIT 1",
            params![job.kind, target_ids, job.input_snapshot_hash],
            |row| row.get(0),
        )
        .optional()?;
    match found {
        Some(id) => Ok(Some(load_job(tx, &id)?)),
        None => Ok(None),
    }
}

/// 入队一个任务（在调用方的事务里）。命中业务键就返回已有任务，不新建、不发事件。
///
/// `job.changed` 由这里发（只在真的新建时）：调用方不用记得自己发，也就不会漏。
pub(crate) fn enqueue_in_tx(tx: &Transaction<'_>, job: NewJob) -> Result<Job> {
    enqueue_in_tx_with_state(tx, job, JobState::Queued, None)
}

/// 入队一个任务，并指定它**这一刻**的状态与「在等什么」。
///
/// 用在哪：导入完成时排的 `index` 任务，这一刻它的前置（提取）**一定**还没跑，
/// 所以直接落在 `waiting_dependency` + `error_code = waiting_extract`，用户不用等
/// 一轮驱动就能在 `job_status()` / `list_jobs()` 里看到它在等哪一环。
///
/// 它**不是失败**：`attempt_count = 0`、`next_attempt_at` 为空、不消耗重试额度；
/// 驱动**每次领取前**以及**一轮收尾时**都会重新评估前置（`resolve_dependencies`）。
pub(crate) fn enqueue_in_tx_with_state(
    tx: &Transaction<'_>,
    job: NewJob,
    state: JobState,
    waiting_reason: Option<&str>,
) -> Result<Job> {
    if let Some(existing) = find_active_by_business_key(tx, &job)? {
        return Ok(existing);
    }

    let now = support::now();
    let id = support::new_id("job");
    let target_ids = serde_json::to_string(&job.target_ids)?;
    let max_attempts = job.max_attempts.max(1);
    tx.execute(
        "INSERT INTO jobs (id, kind, state, priority, target_ids, input_snapshot_hash, \
         attempt_count, max_attempts, next_attempt_at, error_code, created_at, updated_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0, ?7, ?8, ?9, ?10, ?10)",
        params![
            id,
            job.kind,
            state.wire(),
            job.priority.value(),
            target_ids,
            job.input_snapshot_hash,
            max_attempts,
            job.next_attempt_at.map(support::to_iso),
            waiting_reason,
            support::to_iso(now),
        ],
    )?;
    crate::insert_event(tx, EventType::JobChanged, &id, 1)?;
    Ok(Job {
        id,
        kind: job.kind,
        state,
        priority: job.priority.value(),
        target_ids: job.target_ids,
        input_snapshot_hash: job.input_snapshot_hash,
        progress: None,
        attempt_count: 0,
        max_attempts,
        next_attempt_at: job.next_attempt_at,
        error_code: waiting_reason.map(str::to_owned),
        requires_user_action: false,
        attention_key: None,
        created_at: now,
        updated_at: now,
    })
}

/// 入队一个任务。
pub(crate) fn enqueue(core: &mut Core, job: NewJob) -> Result<Job> {
    let tx = core.conn.transaction()?;
    let queued = enqueue_in_tx(&tx, job)?;
    tx.commit()?;
    Ok(queued)
}

pub(crate) fn get(core: &Core, job_id: &str) -> Result<Job> {
    load_job(&core.conn, job_id)
}

/// 按状态筛选列出任务。按优先级与创建时间排序。
pub(crate) fn list(core: &Core, states: Option<&[JobState]>, limit: usize) -> Result<Vec<Job>> {
    let page = limit.clamp(1, 200);
    let mut stmt = core.conn.prepare(
        "SELECT id FROM jobs WHERE (?1 IS NULL OR state IN (SELECT value FROM json_each(?1))) \
         ORDER BY priority DESC, created_at, id LIMIT ?2",
    )?;
    let filter = match states {
        Some(values) if !values.is_empty() => Some(serde_json::to_string(
            &values.iter().map(|state| state.wire()).collect::<Vec<_>>(),
        )?),
        _ => None,
    };
    let ids = stmt
        .query_map(params![filter, page as i64], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    ids.iter().map(|id| load_job(&core.conn, id)).collect()
}

/// 按状态精确计数。
///
/// 存在的理由：`list()` 会把 limit 夹到 200（分页用），拿它 `.len()` 当计数
/// 就会在任务多的时候少报。需要数字的地方一律走这里。
pub(crate) fn count(core: &Core, states: Option<&[JobState]>) -> Result<i64> {
    let filter = match states {
        Some(values) if !values.is_empty() => Some(serde_json::to_string(
            &values.iter().map(|state| state.wire()).collect::<Vec<_>>(),
        )?),
        _ => None,
    };
    let total = core.conn.query_row(
        "SELECT COUNT(*) FROM jobs WHERE (?1 IS NULL OR state IN (SELECT value FROM json_each(?1)))",
        params![filter],
        |row| row.get(0),
    )?;
    Ok(total)
}

/// 领取到的任务，外加它**被领取前**的到期时刻。
///
/// `due_at` 单独带出来，是因为领取会把 `next_attempt_at` 清空，而「这次是不是补做」
/// 必须用领取前的值判断（见 `run_due`）。
pub(crate) struct ClaimedJob {
    pub job: Job,
    pub due_at: Option<DateTime<Utc>>,
}

/// 领取下一个到期的任务：优先级高的先跑，同优先级先来先跑。
pub(crate) fn claim_next_due(core: &mut Core, now: DateTime<Utc>) -> Result<Option<Job>> {
    Ok(claim_next_due_record(core, now)?.map(|claimed| claimed.job))
}

/// 与 `claim_next_due` 相同，但把领取前的到期时刻一起带出来。
///
/// **领取要同时满足「依赖已满足」与「已到期」两个条件**：#67 二轮复审的口径。
/// - 「依赖已满足」不由这里查询，而是由**状态**保证：等在 `waiting_dependency`
///   的任务不在本查询的 `state IN ('queued', 'retry_wait')` 里，闸门只在
///   `index_dependency` 判 `Ready` 时才把它放回 `queued`（`promote_dependency`）。
///   所以能进这里的 `queued` 任务，依赖都已经满足；
/// - 「已到期」由 `next_attempt_at IS NULL OR <= now` 保证：`NULL` 是「入队即到期」，
///   非空则要等到那一刻。`promote_dependency` / `defer_for_dependency` **保留**任务
///   原本的排期，于是「依赖刚满足、但排期还在未来」的任务会停在 `queued` 等时间到，
///   不会被提前领取（不需要另加一个「已满足但未到期」的状态）。
pub(crate) fn claim_next_due_record(
    core: &mut Core,
    now: DateTime<Utc>,
) -> Result<Option<ClaimedJob>> {
    let candidate: Option<String> = core
        .conn
        .query_row(
            "SELECT id FROM jobs WHERE state IN ('queued', 'retry_wait') \
             AND (next_attempt_at IS NULL OR next_attempt_at <= ?1) \
             ORDER BY priority DESC, created_at, id LIMIT 1",
            params![support::to_iso(now)],
            |row| row.get(0),
        )
        .optional()?;

    let Some(job_id) = candidate else {
        return Ok(None);
    };

    let job = load_job(&core.conn, &job_id)?;
    let due_at = job.next_attempt_at;
    let attempt = job.attempt_count + 1;
    // job_attempts.attempt 是**跨人工重试累计**的第几次：人工重试会把 attempt_count
    // 清零，所以不能拿它当这个编号，取历史最大值 +1。
    let attempt_number: i64 = core.conn.query_row(
        "SELECT COALESCE(MAX(attempt), 0) + 1 FROM job_attempts WHERE job_id = ?1",
        params![job_id],
        |row| row.get(0),
    )?;
    let now_text = support::to_iso(now);
    let tx = core.conn.transaction()?;
    tx.execute(
        "UPDATE jobs SET state = 'running', attempt_count = ?1, next_attempt_at = NULL, \
         updated_at = ?2 WHERE id = ?3",
        params![attempt, now_text, job_id],
    )?;
    tx.execute(
        "INSERT INTO job_attempts (job_id, attempt, started_at) VALUES (?1, ?2, ?3)",
        params![job_id, attempt_number, now_text],
    )?;
    crate::insert_event(&tx, EventType::JobChanged, &job_id, 1)?;
    tx.commit()?;

    Ok(Some(ClaimedJob {
        job: load_job(&core.conn, &job_id)?,
        due_at,
    }))
}

/// 任务成功。
pub(crate) fn complete(core: &mut Core, job_id: &str) -> Result<Job> {
    let job = load_job(&core.conn, job_id)?;
    if job.state.is_terminal() && job.state != JobState::Succeeded {
        return Err(CoreError::InvalidState {
            entity: "任务",
            id: job_id.to_owned(),
            state: job.state.wire().to_owned(),
        });
    }
    let now = support::to_iso(support::now());
    let tx = core.conn.transaction()?;
    tx.execute(
        "UPDATE jobs SET state = 'succeeded', error_code = NULL, requires_user_action = 0, \
         updated_at = ?1 WHERE id = ?2",
        params![now, job_id],
    )?;
    // 按「还没结束的那一次」找尝试记录：attempt_count 会被人工重试清零，
    // 不能再用它当行号。
    tx.execute(
        "UPDATE job_attempts SET finished_at = ?1, outcome = 'succeeded' \
         WHERE job_id = ?2 AND finished_at IS NULL",
        params![now, job_id],
    )?;
    crate::insert_event(&tx, EventType::JobChanged, job_id, 1)?;
    tx.commit()?;
    load_job(&core.conn, job_id)
}

/// 任务失败：还有额度就退避重试，用完了就进 failed（「已放弃」，等用户决定）。
pub(crate) fn fail(
    core: &mut Core,
    job_id: &str,
    error_code: &str,
    message: &str,
    now: DateTime<Utc>,
) -> Result<Job> {
    fail_with_retry_after(core, job_id, error_code, message, None, now)
}

/// 与 `fail` 相同，但可以尊重远端的 `Retry-After`（限流时它比本地退避更准）。
///
/// 仍然受 `max_attempts` 约束：远端说等一小时也照样计入一次尝试，额度用完就进
/// 「已放弃」等人工——「有限重试」不是「无限等下去」。
pub(crate) fn fail_with_retry_after(
    core: &mut Core,
    job_id: &str,
    error_code: &str,
    message: &str,
    retry_after_ms: Option<u64>,
    now: DateTime<Utc>,
) -> Result<Job> {
    let job = load_job(&core.conn, job_id)?;
    let exhausted = job.attempt_count >= job.max_attempts;
    let (state, next_attempt, requires_user_action) = if exhausted {
        // 自动重试到头了：这是需要用户看见的事，不是「安静地算了」。
        (JobState::Failed, None, 1_i64)
    } else {
        let wait = match retry_after_ms {
            Some(ms) => Duration::milliseconds(ms.min(60 * 60 * 1000) as i64),
            None => backoff(job.attempt_count),
        };
        (
            JobState::RetryWait,
            Some(support::to_iso(now + wait)),
            0_i64,
        )
    };

    let now_text = support::to_iso(now);
    let tx = core.conn.transaction()?;
    tx.execute(
        "UPDATE jobs SET state = ?1, error_code = ?2, next_attempt_at = ?3, \
         requires_user_action = ?4, updated_at = ?5 WHERE id = ?6",
        params![
            state.wire(),
            error_code,
            next_attempt,
            requires_user_action,
            now_text,
            job_id
        ],
    )?;
    tx.execute(
        "UPDATE job_attempts SET finished_at = ?1, outcome = ?2, error_code = ?3, message = ?4 \
         WHERE job_id = ?5 AND finished_at IS NULL",
        params![now_text, state.wire(), error_code, message, job_id],
    )?;
    crate::insert_event(&tx, EventType::JobChanged, job_id, 1)?;
    tx.commit()?;
    load_job(&core.conn, job_id)
}

/// 立刻放弃：不消耗重试额度，直接进「已放弃」并要求用户处理。
///
/// 用在哪：没有执行者的 `kind`（`no_handler`）。没有处理器不会因为多试几次就出现，
/// 所以退避重试只是白白拖延；但**也绝不能静默成功**——用户必须能在状态里看到它。
/// `attention_key` 让同类问题只提示一次。
pub(crate) fn fail_terminal(
    core: &mut Core,
    job_id: &str,
    error_code: &str,
    message: &str,
    attention_key: Option<&str>,
    now: DateTime<Utc>,
) -> Result<Job> {
    let now_text = support::to_iso(now);
    let tx = core.conn.transaction()?;
    tx.execute(
        "UPDATE jobs SET state = 'failed', error_code = ?1, next_attempt_at = NULL, \
         requires_user_action = 1, attention_key = ?2, updated_at = ?3 WHERE id = ?4",
        params![error_code, attention_key, now_text, job_id],
    )?;
    tx.execute(
        "UPDATE job_attempts SET finished_at = ?1, outcome = 'failed', error_code = ?2, \
         message = ?3 WHERE job_id = ?4 AND finished_at IS NULL",
        params![now_text, error_code, message, job_id],
    )?;
    crate::insert_event(&tx, EventType::JobChanged, job_id, 1)?;
    tx.commit()?;
    load_job(&core.conn, job_id)
}

/// 人工重试的共同部分：把任务清零回 `queued`。
///
/// 清零的是**这一轮的尝试次数**（`attempt_count`），不是历史：`job_attempts` 里每次
/// 尝试都留着、编号跨人工重试累计，才能回答「为什么重试了五次」。`max_attempts`
/// 不动——用户点一次重试，就再给一整份额度。调用方负责事务与事件。
fn reset_for_retry(tx: &Transaction<'_>, job_id: &str, now_text: &str) -> Result<()> {
    tx.execute(
        "UPDATE jobs SET state = 'queued', attempt_count = 0, next_attempt_at = NULL, \
         error_code = NULL, requires_user_action = 0, attention_key = NULL, updated_at = ?1 \
         WHERE id = ?2",
        params![now_text, job_id],
    )?;
    crate::insert_event(tx, EventType::JobChanged, job_id, 1)?;
    Ok(())
}

/// 只有「已放弃」（`failed`）的任务能人工重试：正在跑的、已经成功的、取消的都不行。
fn ensure_retryable(job: &Job) -> Result<()> {
    if job.state != JobState::Failed {
        return Err(CoreError::InvalidState {
            entity: "任务",
            id: job.id.clone(),
            state: format!("{}（只有已放弃的任务才能人工重试）", job.state.wire()),
        });
    }
    Ok(())
}

/// 人工重试：把「已放弃」的任务清零重来，并发出 `job.changed`。
///
/// 不带 operationId：这是用户在任务面板上点的一次动作，清零重跑本身就是它的语义。
/// 契约 `jobs.retry` 那份带 operationId 的幂等入口走 `retry_idempotent`，两条路
/// 用的是同一个清零实现。
pub(crate) fn retry(core: &mut Core, job_id: &str) -> Result<Job> {
    let job = load_job(&core.conn, job_id)?;
    ensure_retryable(&job)?;

    let now_text = support::to_iso(support::now());
    let tx = core.conn.transaction()?;
    reset_for_retry(&tx, job_id, &now_text)?;
    tx.commit()?;
    load_job(&core.conn, job_id)
}

/// 人工重试的幂等版本：同一个 operationId 重复调用返回第一次的结果。
///
/// 语义与 `retry` 完全一致（清零重来），只是多了一层 operationId 回执，
/// 供契约 `jobs.retry` 使用。
pub(crate) fn retry_idempotent(core: &mut Core, job_id: &str, operation_id: &str) -> Result<Job> {
    let fingerprint = support::fingerprint(&["jobs.retry", job_id]);
    if let Some(json) = core.receipt("jobs.retry", &fingerprint, operation_id)? {
        return Ok(serde_json::from_str(&json)?);
    }

    let job = load_job(&core.conn, job_id)?;
    ensure_retryable(&job)?;

    let now_text = support::to_iso(support::now());
    let tx = core.conn.transaction()?;
    reset_for_retry(&tx, job_id, &now_text)?;
    let updated = load_job(&tx, job_id)?;
    crate::store_receipt(
        &tx,
        operation_id,
        "jobs.retry",
        &fingerprint,
        &serde_json::to_string(&updated)?,
    )?;
    tx.commit()?;
    Ok(updated)
}

/// 取消任务。取消不删除任何原件。
pub(crate) fn cancel(core: &mut Core, job_id: &str, operation_id: &str) -> Result<Job> {
    let fingerprint = support::fingerprint(&["jobs.cancel", job_id]);
    if let Some(json) = core.receipt("jobs.cancel", &fingerprint, operation_id)? {
        return Ok(serde_json::from_str(&json)?);
    }

    let job = load_job(&core.conn, job_id)?;
    if job.state.is_terminal() {
        return Err(CoreError::InvalidState {
            entity: "任务",
            id: job_id.to_owned(),
            state: job.state.wire().to_owned(),
        });
    }

    let now = support::to_iso(support::now());
    let tx = core.conn.transaction()?;
    tx.execute(
        "UPDATE jobs SET state = 'cancelled', next_attempt_at = NULL, updated_at = ?1 WHERE id = ?2",
        params![now, job_id],
    )?;
    tx.execute(
        "UPDATE job_attempts SET finished_at = ?1, outcome = 'cancelled' \
         WHERE job_id = ?2 AND finished_at IS NULL",
        params![now, job_id],
    )?;
    let updated = load_job(&tx, job_id)?;
    crate::insert_event(&tx, EventType::JobChanged, job_id, 1)?;
    crate::store_receipt(
        &tx,
        operation_id,
        "jobs.cancel",
        &fingerprint,
        &serde_json::to_string(&updated)?,
    )?;
    tx.commit()?;
    Ok(updated)
}

/// 建议的下次唤醒时刻；没有待办时为 None。
///
/// **在等前置的任务不算「待办」**：它们要等的是别人跑完（`waiting_dependency`），
/// 不是时间到。前置那一环自己会在 `queued`/`retry_wait` 里，所以下一次唤醒时刻
/// 由它给；前置压根没有执行者时，闸门会把在等的任务终态失败，不会留下一个
/// 「永远没有下一次唤醒」的挂死任务。
///
/// 前置满足、但排期还在未来的任务（被 `promote_dependency` 放回 `queued` 且保留了
/// `next_attempt_at`）**算待办**：下面按 `queued`/`retry_wait` 的 `next_attempt_at`
/// 取最早值，于是「一小时后才该跑」的任务会把唤醒时刻如实报成那个未来时刻。
pub(crate) fn next_wakeup(core: &Core) -> Result<Option<DateTime<Utc>>> {
    let now = support::now();
    // 有立刻能跑的排队任务，就该现在唤醒。
    let ready: i64 = core.conn.query_row(
        "SELECT COUNT(*) FROM jobs WHERE state = 'queued' AND next_attempt_at IS NULL",
        [],
        |row| row.get(0),
    )?;
    if ready > 0 {
        return Ok(Some(now));
    }

    let earliest: Option<String> = core
        .conn
        .query_row(
            "SELECT MIN(next_attempt_at) FROM jobs WHERE state IN ('queued', 'retry_wait') \
             AND next_attempt_at IS NOT NULL",
            [],
            |row| row.get(0),
        )
        .optional()?
        .flatten();
    match earliest {
        Some(text) => Ok(Some(support::parse_iso(&text)?)),
        None => Ok(None),
    }
}

/// 记录一个需要用户处理的问题已经展示过，避免每次重开重复通知。
pub(crate) fn acknowledge_attention(core: &mut Core, attention_key: &str) -> Result<()> {
    core.conn.execute(
        "INSERT INTO attention_acks (attention_key, acknowledged_at) VALUES (?1, ?2) \
         ON CONFLICT(attention_key) DO UPDATE SET acknowledged_at = excluded.acknowledged_at",
        params![attention_key, support::to_iso(support::now())],
    )?;
    Ok(())
}

pub(crate) fn is_attention_acknowledged(core: &Core, attention_key: &str) -> Result<bool> {
    let found: Option<i64> = core
        .conn
        .query_row(
            "SELECT 1 FROM attention_acks WHERE attention_key = ?1",
            params![attention_key],
            |row| row.get(0),
        )
        .optional()?;
    Ok(found.is_some())
}

/// 等配置的 `attention_key` 口径：`job-config:<kind>:<code>`。
///
/// **一个形状，三个写入者共用这一处**：Provider 闸门
/// （`provider::promote_configuration_waits`）、依赖闸门（`defer_for_configuration`）、
/// 执行期（`defer_for_configuration_after_attempt`）。界面顺着它就能深链到「去配哪个
/// 服务 / 哪台机器的模型」，同一个待处理问题也只提示一次（`attention_acks`）。
pub(crate) fn configuration_attention_key(kind: &str, error_code: &str) -> String {
    format!("job-config:{kind}:{error_code}")
}

/// **执行期**才发现配置不可用（任务**已经领取、真的跑过一次**）：落回
/// `waiting_configuration`，把这次尝试如实收尾。
///
/// 用在哪：Provider 作业真的发了请求才发现 401 / 额度用尽 / 服务没了；语义索引在闸门
/// 放行（`Configured`，只表示「配了」，不表示「加载得起来」）之后真的去加载模型才发现
/// 加载失败。两类都是「配置坏了」，所以：
///
/// - **不排退避、不重试**：重试一百次还是同一个 401，等的是人去改配置（契约第 7 节
///   「等待配置，不频繁重试」）；
/// - **`requires_user_action = 1`**：等配置不是失败，但**不是一个用户无需理会的状态**
///   ——用户得去把服务 / 模型配上。这是它跨 kind、跨两条路的**唯一答案**；
/// - **`attention_key = job-config:<kind>:<code>`**：界面靠它深链到「配哪一个」；
/// - **`attempt_count` 如实记账**：这条真的领过一次、真的跑过一次
///   （`claim_next_due_record` 已经 +1），所以**不回退**它；那次尝试同时在
///   `job_attempts` 里以 `outcome = 'waiting_configuration'` 收尾，不挂在 `running` 上。
///
/// 与**闸门**那条路（`defer_for_configuration`）的区别**只有 `attempt_count` 一个
/// 字段**：闸门在领取前就把配置不满足的任务挡下，没有执行者跑过，就**一次都不记**（0）
/// ——没跑过就不计尝试，真跑了一次就如实记账，这就是全部的差别。
pub(crate) fn defer_for_configuration_after_attempt(
    core: &mut Core,
    job_id: &str,
    kind: &str,
    error_code: &str,
    message: &str,
    now: DateTime<Utc>,
) -> Result<Job> {
    let now_text = support::to_iso(now);
    let attention = configuration_attention_key(kind, error_code);
    let tx = core.conn.transaction()?;
    tx.execute(
        "UPDATE jobs SET state = 'waiting_configuration', error_code = ?1, \
         next_attempt_at = NULL, requires_user_action = 1, attention_key = ?2, updated_at = ?3 \
         WHERE id = ?4",
        params![error_code, attention, now_text, job_id],
    )?;
    // 这次尝试如实收尾（它确实跑过、确实没成），但不排下一次。
    tx.execute(
        "UPDATE job_attempts SET finished_at = ?1, outcome = 'waiting_configuration', \
         error_code = ?2, message = ?3 WHERE job_id = ?4 AND finished_at IS NULL",
        params![now_text, error_code, message, job_id],
    )?;
    crate::insert_event(&tx, EventType::JobChanged, job_id, 1)?;
    tx.commit()?;
    load_job(&core.conn, job_id)
}

// ------------------------------------------------------------------ 依赖闸门

/// 一条有前置的任务的评估结果。
enum Dependency {
    /// 前置已满足，可以跑。
    Ready,
    /// 前置还没满足，但队列里还有活着的任务会满足它：留在 `waiting_dependency`。
    Pending(&'static str),
    /// 前置是**配置**（模型没就绪）而不是「别人跑完」：落在 `waiting_configuration`。
    ///
    /// 与 `Pending` 的区别在唤醒口径上：配置变化不是时间问题，所以它**不进
    /// `next_wakeup`**，等下一次驱动（前台/唤醒/配置变化后）由闸门重新评估。
    WaitingConfiguration(&'static str),
    /// 前置不会再有执行者来满足了（或者目标本身就不成立）：终态失败，别挂死。
    Broken(&'static str),
}

/// 按 `kind` 评估一条任务的前置。只有真有前置的 kind 才被 `resolve_dependencies`
/// 查出来（`extract` / `transcribe` 不在这里：它们没有前置）。
fn job_dependency(core: &Core, job: &Job) -> Result<Dependency> {
    match job.kind.as_str() {
        KIND_INDEX => index_dependency(core, job),
        KIND_SEMANTIC_INDEX => semantic_index_dependency(core, job),
        // 走到这里说明 `resolve_dependencies` 的查询与这里的分发不一致；
        // 如实报「前置已满足」不如报错——静默放行会把没评估过的任务领走。
        other => Err(CoreError::CorruptedData {
            message: format!("任务类型「{other}」没有前置评估实现"),
        }),
    }
}

/// 评估一条 `index` 任务的前置：`target_ids` 里每个**来源**的当前修订都要「可索引」。
///
/// 口径（可解释，见 m4 文档）：
/// - **空 `target_ids` = 整库重建**（`indexes.rebuild(scope=all)`）：等队列里所有
///   **活着的** `extract` 收尾，再按现状整库重刷（`rebuild_keyword_index(None)`）。
///   它**不会**判 `Broken`——某条提取被取消只是少一份内容，不该把整库重建连坐成
///   终态失败；
/// - 来源不存在 / 没有当前修订 / 当前修订读不到 → `Broken("not_found")`；
/// - 当前修订是**文字修订**（`asset_id` 为空）：提交时正文就落库了，关键词这一路
///   没有「提取」这一环 → 视为可索引；
/// - 当前修订有资产：要有 `extracted_contents` 行才算提取完；还没有的话，看队列里
///   还有没有**活着的** `extract` 任务瞄着这个修订：
///   - 有 → `Pending("waiting_extract")`（在等哪一环说得清）；
///   - 没有 → `Broken("dependency_extract_unmet")`：没有谁会再产出它，挂死不如如实失败。
///
/// 为什么不用「提取任务成功」当条件而用「有没有派生内容」：改过正文、重跑过提取的
/// 来源会换修订，队列里那条 `extract` 的成功与否已经不是当下的真相；派生内容在不在
/// 才是「这个来源现在能不能被索引」的答案。
fn index_dependency(core: &Core, job: &Job) -> Result<Dependency> {
    if job.target_ids.is_empty() {
        // 整库：等着在跑的提取收尾，别重建到一半又被新的派生内容补一刀。
        return Ok(if live_extract_count(core)? > 0 {
            Dependency::Pending(WAITING_EXTRACT)
        } else {
            Dependency::Ready
        });
    }
    let mut waiting = false;
    for source_id in &job.target_ids {
        let current: Option<Option<String>> = core
            .conn
            .query_row(
                "SELECT current_revision_id FROM source_items WHERE source_id = ?1",
                params![source_id],
                |row| row.get(0),
            )
            .optional()?;
        let Some(Some(revision_id)) = current else {
            return Ok(Dependency::Broken(crate::error::ErrorCode::NotFound.wire()));
        };

        let asset: Option<Option<String>> = core
            .conn
            .query_row(
                "SELECT asset_id FROM source_revisions WHERE revision_id = ?1",
                params![revision_id],
                |row| row.get(0),
            )
            .optional()?;
        let Some(asset_id) = asset else {
            return Ok(Dependency::Broken(crate::error::ErrorCode::NotFound.wire()));
        };
        if asset_id.is_none() {
            // 文字修订：正文在提交时就落库并且同步进了关键词索引，没有提取这一环。
            continue;
        }

        let extracted: i64 = core.conn.query_row(
            "SELECT COUNT(*) FROM extracted_contents WHERE source_revision_id = ?1",
            params![revision_id],
            |row| row.get(0),
        )?;
        if extracted > 0 {
            continue;
        }

        // `target_ids` 是 JSON 数组，用 json_each 精确比对：`LIKE` 会把
        // `rev_a` 与 `rev_ab` 这种前缀关系看成一回事。
        let live_extract: i64 = core.conn.query_row(
            "SELECT COUNT(*) FROM jobs WHERE kind = ?1 \
             AND state NOT IN ('succeeded', 'failed', 'cancelled') \
             AND EXISTS (SELECT 1 FROM json_each(jobs.target_ids) WHERE value = ?2)",
            params![KIND_EXTRACT, revision_id],
            |row| row.get(0),
        )?;
        if live_extract > 0 {
            waiting = true;
        } else {
            return Ok(Dependency::Broken(ERROR_DEPENDENCY_UNMET));
        }
    }
    Ok(if waiting {
        Dependency::Pending(WAITING_EXTRACT)
    } else {
        Dependency::Ready
    })
}

/// 队列里还有多少条**活着的** `extract`。
///
/// 「活着」= `state NOT IN ('succeeded','failed','cancelled')`：排队、在跑、退避重试
/// 都算——它们还有机会写出派生内容。两类整库任务（关键词与语义）都等它收尾。
fn live_extract_count(core: &Core) -> Result<i64> {
    let live: i64 = core.conn.query_row(
        "SELECT COUNT(*) FROM jobs WHERE kind = ?1 \
         AND state NOT IN ('succeeded', 'failed', 'cancelled')",
        params![KIND_EXTRACT],
        |row| row.get(0),
    )?;
    Ok(live)
}

/// 评估一条 `semantic_index` 任务的前置。两段，按**由外到内**的顺序：
///
/// 1. **模型就绪**（`waiting_model`）：本地嵌入器没配、或配了但不可用（缺文件/加载
///    失败）时，落在 `waiting_configuration`。这是更外层的前置——没有模型，等提取
///    等得再久也建不出任何向量；用户能做的也只有把模型配好。配置变化之后（下一次
///    驱动）闸门会把它拉起来，不需要「下一次唤醒时刻」（配置变化不是时间问题）；
/// 2. **提取收尾**（`waiting_extract`）：与关键词整库重建同一条规则——队列里还有
///    活着的 `extract` 就等它跑完，别建到一半又被新的派生内容补一刀。
///    它**不判 `Broken`**：某条提取被取消只是少一份内容，语义重建照样该跑完整库
///    （块表里有什么就建什么），不该被连坐成终态失败。
///
/// 注意 `target_ids` 是不是空的**不影响**这两条：语义的代次与向量是**全局**的，
/// 范围只决定这一轮重算了哪些块（见 `semantic::build_with_deadline`）。
///
/// 「模型可用」的判据是 `EmbedderReadiness`，**不是**「已经加载好」：`Configured`
/// （配了、文件也在、还没建会话）是**正常的生产路径**——真正要算向量时才懒加载，
/// 启动不做重活。所以闸门在 `Configured` 时放行，加载失败由执行者按「等配置」处理。
fn semantic_index_dependency(core: &Core, _job: &Job) -> Result<Dependency> {
    match core.embedder_readiness() {
        crate::semantic::EmbedderReadiness::NotConfigured
        | crate::semantic::EmbedderReadiness::Failed(_) => {
            return Ok(Dependency::WaitingConfiguration(WAITING_MODEL));
        }
        crate::semantic::EmbedderReadiness::Configured
        | crate::semantic::EmbedderReadiness::Ready => {}
    }
    Ok(if live_extract_count(core)? > 0 {
        Dependency::Pending(WAITING_EXTRACT)
    } else {
        Dependency::Ready
    })
}

/// 重新评估所有**有前置的**任务（`index` 与 `semantic_index`）的前置，把等待态推到
/// 当下的真相。
///
/// 驱动**每次领取前**跑一次，**一轮收尾时**再跑一次（`run_due`），所以：
/// - 前置已经满足的在等任务 → 回到 `queued`，同一轮的下一次领取就能被领取；
/// - 前置仍未满足的 → 留在 `waiting_dependency`，原因写在 `error_code` 上；
/// - 前置是配置（语义索引等模型）而还没配好 → 留在 `waiting_configuration`；
/// - 前置不会再有执行者满足的 → 终态 `failed` + 明确错误码 + `requires_user_action`。
///
/// **为什么每次领取前都要重评估，而不是每轮一次**：一轮只评估一次的话，前置任务在
/// 这一轮跑完时后继仍在等待态，而 `next_wakeup` 不把等待态算成待办——队列里没有别的
/// 任务时摘要会给出 `next_wakeup = None`，按摘要安排唤醒的平台层就不会再叫驱动，
/// 链条只能碰巧再次进前台才可能走完。每次领取前重评估之后，链在**一次**驱动调用里
/// 就能走完，不把「能不能走完」寄托在平台层肯不肯再叫一次上（#19 可能因省电/退出
/// 而不叫）。预算与条数上限照旧只在任务之间判断，链式推进不会打断正在跑的任务。
///
/// 它**不领取、不执行、不动 `attempt_count`、不排退避**：在等前置不是一次失败，
/// 不该消耗重试额度。它是**纯查询**（只把状态推到真相），不消耗预算。改状态时照旧
/// 发 `job.changed`。唯一的例外是「等的是配置」那条：它**要置
/// `requires_user_action` 与 `attention_key`**（见 `defer_for_configuration`）——
/// 没有执行者跑过，所以不记尝试；但用户确实得动手，不能藏成「无事发生」。
///
/// 返回值是这一轮**被闸门终态失败**的任务数（没有任何执行者跑过），如实报进
/// `JobRunSummary::dependency_failed`。
fn resolve_dependencies(core: &mut Core, now: DateTime<Utc>) -> Result<i64> {
    let ids: Vec<String> = {
        let mut stmt = core.conn.prepare(
            "SELECT id FROM jobs WHERE kind IN (?1, ?2) \
             AND state NOT IN ('running', 'succeeded', 'failed', 'cancelled') \
             ORDER BY created_at, id",
        )?;
        let collected = stmt
            .query_map(params![KIND_INDEX, KIND_SEMANTIC_INDEX], |row| row.get(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        collected
    };

    let mut broken = 0;
    for job_id in ids {
        let job = load_job(&core.conn, &job_id)?;
        match job_dependency(core, &job)? {
            Dependency::Ready => {
                // 等待依赖与等待配置都靠这一步回到 queued（配置是靠「下一次驱动」醒来）。
                if job.state == JobState::WaitingDependency
                    || job.state == JobState::WaitingConfiguration
                {
                    promote_dependency(core, &job_id, now)?;
                }
            }
            Dependency::Pending(reason) => {
                if job.state != JobState::WaitingDependency
                    || job.error_code.as_deref() != Some(reason)
                {
                    defer_for_dependency(core, &job_id, reason, now)?;
                }
            }
            Dependency::WaitingConfiguration(reason) => {
                if job.state != JobState::WaitingConfiguration
                    || job.error_code.as_deref() != Some(reason)
                {
                    defer_for_configuration(core, &job_id, &job.kind, reason, now)?;
                }
            }
            Dependency::Broken(code) => {
                let attention = format!("job-terminal:{}:{}", job.kind, code);
                fail_terminal(
                    core,
                    &job_id,
                    code,
                    &dependency_message(code, &job),
                    Some(&attention),
                    now,
                )?;
                broken += 1;
            }
        }
    }
    Ok(broken)
}

/// 前置已满足：把在等的任务放回 `queued`，这一轮**到期就**可跑。
///
/// 等待依赖与等待配置共用这一步：两种等待都不是失败，「等的是什么」在满足的那一刻
/// 就不成立了，所以 `error_code` 一起清掉。等配置的那条还会把
/// `requires_user_action` / `attention_key` 一起清掉——「要用户去配」这件事随配置
/// 配好而消失，留着一个置位的待办会让界面在 `queued`/`running` 的任务上继续喊人去
/// 动手（与 Provider 闸门的 `promote_configuration_waits` 同口径）。
///
/// **保留 `next_attempt_at`（#67 二轮复审）**：排期是这条任务「本该何时跑」的
/// 事实，等待前置不该把它抹掉。保留之后，「依赖刚满足但排期还在未来」的任务落在
/// `queued` + 未来 `next_attempt_at`，既有的领取判据
/// （`next_attempt_at IS NULL OR <= now`）自然拦住它，不需要另加状态；同时它作为
/// `queued` 任务会进 `next_wakeup`，到期时间不丢。若清空，一条排在一小时后的索引
/// 任务会在前置提前完成的那一刻被立刻领走（本轮就被提前执行），
/// `next_wakeup` 与补做统计里也再看不到原计划时刻。
fn promote_dependency(core: &mut Core, job_id: &str, now: DateTime<Utc>) -> Result<()> {
    let tx = core.conn.transaction()?;
    tx.execute(
        "UPDATE jobs SET state = 'queued', error_code = NULL, requires_user_action = 0, \
         attention_key = NULL, updated_at = ?1 WHERE id = ?2",
        params![support::to_iso(now), job_id],
    )?;
    crate::insert_event(&tx, EventType::JobChanged, job_id, 1)?;
    tx.commit()?;
    Ok(())
}

/// 前置还没满足：留在 `waiting_dependency`，把「在等什么」写在 `error_code` 上。
///
/// 不碰 `attempt_count`、不置 `requires_user_action`：在等前置不是失败，也不需要
/// 用户做什么。
///
/// **保留 `next_attempt_at`（#67 二轮复审）**：等待期间那条任务原本的排期仍是真的，
/// 清空它会让「一小时后才该跑的索引任务」在前置提前完成时被立刻领走，而且原到期
/// 时刻从 `next_wakeup` 与补做统计里一起消失。保留排期后，等待态由它的前置任务
/// 负责唤醒（`next_wakeup` 仍只把 `queued`/`retry_wait` 当待办），前置满足再靠
/// 「是否到期」决定能不能领。
fn defer_for_dependency(
    core: &mut Core,
    job_id: &str,
    reason: &str,
    now: DateTime<Utc>,
) -> Result<()> {
    let tx = core.conn.transaction()?;
    tx.execute(
        "UPDATE jobs SET state = 'waiting_dependency', error_code = ?1, \
         requires_user_action = 0, attention_key = NULL, updated_at = ?2 WHERE id = ?3",
        params![reason, support::to_iso(now), job_id],
    )?;
    crate::insert_event(&tx, EventType::JobChanged, job_id, 1)?;
    tx.commit()?;
    Ok(())
}

/// 前置是**配置**（现在只有 `semantic_index` 等模型）：留在 `waiting_configuration`，
/// 把「在等什么」写在 `error_code` 上（`waiting_model`）。
///
/// 这是**闸门**（领取前）那条路：没有执行者跑过，所以**不记账**——`attempt_count`
/// 一个数都不动（它本来就是 0，也不该凭空记一次尝试）。它**不排退避**、**不进
/// `next_wakeup`**：配置变化不是时间问题，模型配好之后下一次 `run_due` 的闸门就会把
/// 它拉回 `queued`。
///
/// 但它**要置 `requires_user_action = 1` 并写 `attention_key`
/// （`job-config:<kind>:<code>`）**：等配置不是失败，但**不是一个用户无需理会的状态**
/// ——用户得去把模型配好，界面靠这个 key 深链到「配哪一个」。这一句**跨 kind 一把尺**：
/// Provider 的闸门（`provider::promote_configuration_waits`）与执行期那条
/// （`defer_for_configuration_after_attempt`）给的是同一个答案。
///
/// 与 `defer_for_dependency` 的区别只有状态名 + 上面那两件事，但状态名有信息量：
/// 平台层看 `JobStatus.waiting_configuration` 就知道「这不是排队等资源，是没配好」。
fn defer_for_configuration(
    core: &mut Core,
    job_id: &str,
    kind: &str,
    reason: &str,
    now: DateTime<Utc>,
) -> Result<()> {
    let attention = configuration_attention_key(kind, reason);
    let tx = core.conn.transaction()?;
    tx.execute(
        "UPDATE jobs SET state = 'waiting_configuration', error_code = ?1, \
         requires_user_action = 1, attention_key = ?2, updated_at = ?3 WHERE id = ?4",
        params![reason, attention, support::to_iso(now), job_id],
    )?;
    crate::insert_event(&tx, EventType::JobChanged, job_id, 1)?;
    tx.commit()?;
    Ok(())
}

/// 前置失败的用户可读说明。
///
/// 它会跟着这次失败进 `job_attempts.message`；**闸门拦下的任务没有尝试记录**（没有
/// 执行者跑过，不该凭空记一次尝试），所以那种情况下这个文案不会落库——用户看到的是
/// `error_code`（`not_found` / `dependency_extract_unmet`，码里点明了等的是哪一环）。
/// 留着它是给「有尝试记录」的那条路径用的，也方便将来把说明接出来。
fn dependency_message(code: &str, job: &Job) -> String {
    let targets = job.target_ids.join("、");
    match code {
        ERROR_DEPENDENCY_UNMET => format!(
            "等不到提取完成：{targets} 没有可索引的派生内容，队列里也没有活着的 extract 任务"
        ),
        _ => format!("目标来源不可用：{targets}"),
    }
}

// ------------------------------------------------------------------ 驱动

/// 处理器失败后的处置。**三种时间语义不能混**（契约第 7 节）：
///
/// - `Terminal`：重试也不会好（没有处理器、目标不存在）→ `failed` + 等人工；
/// - `Retry`：换个时间可能就好（原件暂时不在、断网、超时、限流）→ 退避重试，
///   消耗额度；
/// - `WaitConfiguration`：等用户改配置（没配置服务、鉴权失败、额度用尽）→
///   `waiting_configuration` + `requires_user_action = 1` +
///   `attention_key = job-config:<kind>:<code>`，**不消耗重试额度、不频繁重试**
///   （见 `defer_for_configuration_after_attempt`：这次真的跑过的尝试照实记账）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FailureDisposition {
    Terminal,
    Retry,
    WaitConfiguration,
}

/// 处理器的失败。
struct HandlerFailure {
    code: String,
    message: String,
    disposition: FailureDisposition,
    /// 远端 `Retry-After` 给的等待毫秒数（有就尊重它，没有就按本地退避）。
    retry_after_ms: Option<u64>,
}

impl HandlerFailure {
    fn terminal(code: &str, message: String) -> Self {
        Self {
            code: code.to_owned(),
            message,
            disposition: FailureDisposition::Terminal,
            retry_after_ms: None,
        }
    }

    /// 核心错误 → 处置。时间语义只由 `CoreError` 自己说了算（`retryable` /
    /// `waits_for_configuration`，见 `error.rs` 与 `provider::normalize` 的映射表），
    /// 执行者不用各自判断——否则同一类错误在不同 kind 下会有不同下场。
    fn from_error(error: CoreError) -> Self {
        let disposition = if error.waits_for_configuration() {
            FailureDisposition::WaitConfiguration
        } else if error.retryable() {
            FailureDisposition::Retry
        } else {
            FailureDisposition::Terminal
        };
        let retry_after_ms = match &error {
            CoreError::RateLimited { retry_after_ms, .. } => *retry_after_ms,
            _ => None,
        };
        Self {
            code: error.code().wire().to_owned(),
            message: error.to_string(),
            disposition,
            retry_after_ms,
        }
    }

    /// 旧调用点（`extract` / `index`）的口径：**换个时间可能就好**。
    ///
    /// 这里刻意不按 `CoreError` 的默认处置走：对提取来说「原件暂时不在、修订暂时
    /// 读不到」是可以等一等的（导入与提取在两个事务里，先后顺序不保证），所以这一
    /// 路一直是有界重试。Provider 那一路用 `from_error`，时间语义严格按契约第 7 节
    /// 的码来（鉴权失败等配置、超时有限重试、模型不对就停）。
    fn retryable(error: CoreError) -> Self {
        Self {
            code: error.code().wire().to_owned(),
            message: error.to_string(),
            disposition: FailureDisposition::Retry,
            retry_after_ms: None,
        }
    }
}

/// 执行者跑完一轮之后的两种**非失败**归宿。失败走 `HandlerFailure`。
enum HandlerOutcome {
    /// 跑完了：调用方 `complete`。
    Completed,
    /// 分段执行到了截止点、**在安全点让出**：回到 `queued` 立刻可再跑，
    /// 进度留在任务上；`building_generation` 与已写的向量都留着，下一轮续跑。
    ///
    /// 只有**自己知道怎么安全停下**的执行者才返回它（现在只有语义索引）。
    Yielded { completed: i64, total: i64 },
    /// 前置其实是**配置**没就绪（闸门放行时是「配了但还没加载」，真正加载失败）：
    /// 落回 `waiting_configuration`，不算失败、不退避、不消耗额度，但**要用户去配**。
    ///
    /// `reason` 是错误码（`error_code`）；`message` 是这次到底为什么没成（进
    /// `job_attempts.message`，与 Provider 那一路的失败一样，是排查用的原文）。
    WaitingConfiguration {
        reason: &'static str,
        message: String,
    },
}

/// 跑到期任务：领取 → 按 `kind` 执行 → 把结果落回队列。
///
/// 口径（都写在 `docs/architecture/m4-任务调度与补做.md`）：
/// - **`budget_ms`**：调用方（平台层）给的**时间预算**，毫秒。0 表示一个都不跑。
///   预算**只在任务之间判断**：核心不做抢占，没有取消点的任务（提取、关键词重建）
///   一旦领取就一跑到底，所以「一条任务本身比预算还长」仍然可能，预算是「这一轮
///   还能不能再开一条」。**可分段的任务（语义索引）多一层**：`execute` 拿到
///   `started + budget_ms` 这个截止点，语义执行者在块与块之间看它，到点就返回
///   `HandlerOutcome::Yielded`——状态回 `queued`（立刻可再跑）、进度写进
///   `progress_completed/total`、`building_generation` 留着，下一轮接着算没算完的
///   块。它**至少算完一块**才允许让出，所以「每次唤醒都前进」；
/// - **`max_jobs`**：防御性的**条数上限**。预算在任务之间挡长尾，它挡的是「队列很
///   长、但每条都不长」把一轮拖成没完没了的情况。被预算或条数截住的到期任务留在
///   队列里，数量如实报进 `due_backlog`；
/// - **`trigger`**：唤醒来源，如实记进摘要，供观察与排查；核心不用它改变行为；
/// - **补做**：领取时任务的 `next_attempt_at` 已经**严格早于**本次运行的开始时刻
///   （`started_at`）。`next_attempt_at` 为空表示「入队即到期」——队列里没有它
///   本该何时跑的信息，所以**不计入补做**（否则刚入队就被跑的任务也会算成补做，
///   反而误导）；这类积压由 `due_backlog` 呈现。**按时执行不算补做**：在排期那一刻
///   或更早被领走（`due_at == now`）不算，只有排在**过去**、这次才跑上的才算。
///   等待依赖期间排期被保留（见 `defer_for_dependency` / `promote_dependency`），
///   所以「排在未来、前置提前完成」的任务不会被提前跑，也就不会被误记成补做。
///   我们只说「补做」，不说「准时」；
/// - **运行开始先回收**上一次没跑完（`running`）的任务，否则那些任务会被永久丢掉；
/// - **每次领取前先重新评估依赖**（`resolve_dependencies`），并在**一轮收尾时**再评估
///   一次：前置已满足的在等任务会被拉起，在**同一轮**内被领取执行。所以「导入 → 提取
///   → 索引」这样的链在**一次**驱动调用里就能走完，不需要平台层再叫一次。
///
/// **让出的任务怎么计数**：它**被领取并执行过一段**，所以计进 `processed`；但整代
/// 还没切换，所以**不算** `succeeded`，也**不算** `failed`/`retrying`（它还活着）。
/// 于是 `processed` 可以大于 `succeeded + failed + retrying`——差额正是「这一轮让出、
/// 还在排队接着跑」的任务。它们由 `due_backlog > 0`（到期没跑完）与
/// `next_wakeup`（指向「现在」）一起说清楚；每条任务的进度在 `Job::progress` 上。
/// 摘要里**没有**单独的 `yielded` 字段：契约 `DueRunSummary` 的字段一个没多（见
/// `docs/architecture/m4-任务调度与补做.md`）。
///
/// **依赖链的不变式（一轮结束后必须成立）**：若还有任务留在 `waiting_dependency`，
/// 它们只可能是在等队列里 `queued` / `retry_wait` 的前置任务（那些任务的
/// `next_attempt_at` / 「入队即到期」就是下一次唤醒时刻），或者前置已判 `Broken`——
/// 而 `Broken` 在本轮已被闸门终态失败，不会留下等待态。因此**不允许**出现
/// 「有等待态、却 `next_wakeup = None` 且队列里没有任何到期/排队任务」：那等于把链
/// 挂死，且摘要对平台层说了谎。收尾的那次评估就是为了堵住这个缺口——前置跑完就
/// 立刻把后继推进到 `queued`（`next_wakeup` 随即给出「现在」），轮末不会留下
/// 「依赖其实已满足却还写着等待」的任务。`debug_assert` 把它钉在代码里，测试另钉一条。
pub(crate) fn run_due(
    core: &mut Core,
    now: DateTime<Utc>,
    budget_ms: u64,
    trigger: &str,
    max_jobs: usize,
) -> Result<JobRunSummary> {
    // 预算按**真实流逝时间**算，不用调用方给的 `now`：`now` 是业务时钟（测试会把它
    // 推到过去/未来来排期），拿它算预算会随调用方的时钟走。`Instant` 与业务时钟无关。
    let started = std::time::Instant::now();
    // 交给执行者的**截止点**：可分段的长任务（语义索引）在块与块之间看它，到点让出。
    // 巨大预算换算溢出（`checked_add` 给 `None`）时等于「没有截止点」，跑完为止——
    // 这比 panic 或截断成一个错误的时刻都诚实。
    let deadline = started.checked_add(std::time::Duration::from_millis(budget_ms));

    let mut summary = JobRunSummary {
        started_at: now,
        trigger: trigger.to_owned(),
        processed: 0,
        succeeded: 0,
        failed: 0,
        retrying: 0,
        backfilled: 0,
        recovered: recover_interrupted_runs(core, now)?,
        due_backlog: 0,
        waiting_dependency: 0,
        dependency_failed: 0,
        next_wakeup: None,
    };

    // 依赖闸门在**每次领取前**重评估（见下面的循环），不再只在每轮开始跑一次：
    // 上一条前置任务跑完，后继就在同一轮被拉起并执行。这一步不领取、不执行、
    // 不消耗预算：前置已经满足的回到 `queued`（同一轮的下一次领取就能跑），
    // 仍未满足的留在等待态，前置不可能再满足的终态失败并如实报数。

    let mut processed: usize = 0;
    loop {
        // 预算与条数上限**只在任务之间**判断（见上面的口径）。
        if processed >= max_jobs {
            break;
        }
        if started.elapsed().as_millis() >= u128::from(budget_ms) {
            break;
        }

        // 每次领取前重评估依赖：上一条前置任务刚跑完，它的后继在这一刻就被认账、
        // 被拉起，并在同一轮内被领取执行。闸门是纯查询，不消耗预算，也不动额度。
        summary.dependency_failed += resolve_dependencies(core, now)?;
        // 配置闸门同样在领取前评估（B4a）：服务没配好、或测过但没验过的任务
        // **不会被领取**，所以既不发请求、也不消耗尝试次数。
        provider::promote_configuration_waits(core, now)?;

        let Some(claimed) = claim_next_due_record(core, now)? else {
            break;
        };
        if let Some(due_at) = claimed.due_at {
            if due_at < now {
                summary.backfilled += 1;
            }
        }

        let job = claimed.job;
        match execute(core, &job, deadline) {
            Ok(HandlerOutcome::Completed) => {
                complete(core, &job.id)?;
                summary.succeeded += 1;
            }
            Ok(HandlerOutcome::Yielded { completed, total }) => {
                // 分段让出：进度如实写进任务，状态回到 `queued`（立刻可再跑），
                // **不算一次尝试**（它不是失败，反复让出不能耗尽 max_attempts）。
                // 它已经「被领取并执行过一段」，所以照旧计进 `processed`；但**不算**
                // `succeeded`（整代还没切换）、也不算 `failed`/`retrying`（它还活着）。
                // 摘要里「这一轮提前收手了」由 `due_backlog > 0` 与 `next_wakeup` 说出。
                set_progress(core, &job.id, completed, total)?;
                yield_back(core, &job.id, now)?;
            }
            Ok(HandlerOutcome::WaitingConfiguration { reason, message }) => {
                // 执行者发现配置其实没就绪（闸门放行时是 `Configured`，真正加载失败）。
                // 如实落回 `waiting_configuration`：不算失败、不退避、不消耗额度，但
                // **要用户去配**（`requires_user_action = 1` + `attention_key`），而且
                // 确实跑过的这一笔**照实记**。用户把模型配好之后，下一次驱动的闸门就会
                // 把它拉起来。已经写进库的向量与进度都留着（这条路径上一般一块都没算
                // ——加载失败发生在算第一块之前），下一轮从 `building_generation` 续跑。
                defer_for_configuration_after_attempt(
                    core,
                    &job.id,
                    &job.kind,
                    reason,
                    &message,
                    now,
                )?;
            }
            Err(failure) => match failure.disposition {
                FailureDisposition::Terminal => {
                    // 同类同码的问题只提示一次，不然每次重开都要再喊一遍。
                    let attention = format!("job-terminal:{}:{}", job.kind, failure.code);
                    fail_terminal(
                        core,
                        &job.id,
                        &failure.code,
                        &failure.message,
                        Some(&attention),
                        now,
                    )?;
                    summary.failed += 1;
                }
                FailureDisposition::Retry => {
                    let after = fail_with_retry_after(
                        core,
                        &job.id,
                        &failure.code,
                        &failure.message,
                        failure.retry_after_ms,
                        now,
                    )?;
                    if after.state == JobState::Failed {
                        summary.failed += 1;
                    } else {
                        summary.retrying += 1;
                    }
                }
                FailureDisposition::WaitConfiguration => {
                    // 等配置：既没有放弃、也没有排退避（不消耗重试额度）。
                    // `waiting_configuration` 的计数在 `job_status()` 里看得到，
                    // `DueRunSummary` 没有这个字段（契约没给），所以它不计入
                    // failed / retrying——不谎报「已经失败」，也不谎报「在重试」。
                    // 状态与 attention_key 的口径由函数自己保证（跨 kind 一把尺），
                    // 它只额外做一件事：把这次**真的跑过**的尝试如实记账。
                    defer_for_configuration_after_attempt(
                        core,
                        &job.id,
                        &job.kind,
                        &failure.code,
                        &failure.message,
                        now,
                    )?;
                }
            },
        }
        processed += 1;
    }

    // 一轮收尾再评估一次：把等待态推到当下的真相（前置已满足的回到 `queued`，
    // `next_wakeup` 随即给出「现在」）。这是「有等待态就必须有可用的下一次唤醒」
    // 这条不变式的兜底：循环可能因为预算 / 条数上限在这一轮的最后一条任务之后
    // 直接退出，没来得及在「领取前」认账刚跑完的前置（见函数文档）。
    summary.dependency_failed += resolve_dependencies(core, now)?;
    provider::promote_configuration_waits(core, now)?;

    summary.processed = processed as i64;
    summary.waiting_dependency = count(core, Some(&[JobState::WaitingDependency]))?;
    summary.due_backlog = count_due(core, now)?;
    summary.next_wakeup = next_wakeup(core)?;

    // 不变式：一轮结束后还留着等待态，就必须给出可用的下一次唤醒时刻。留下的等待态
    // 只可能是在等队列里 `queued`/`retry_wait` 的前置（其 `next_attempt_at` / 入队即
    // 到期就是那次唤醒），而前置已判 `Broken` 的在本轮已经被终态失败。断言在开发/
    // 测试构建里把这个缺口钉住，避免「有等待态却 next_wakeup = None」的挂死回归。
    debug_assert!(
        summary.waiting_dependency == 0 || summary.next_wakeup.is_some(),
        "依赖链不变式被破坏：还有 {} 个等待态任务，却没有可用的下一次唤醒时刻",
        summary.waiting_dependency
    );

    Ok(summary)
}

/// 按 `kind` 找执行者。
///
/// **只有真有实现的 kind 才被执行**。没有执行者的 kind（现在只有 `transcribe`：
/// 它等 B4a 的 Provider）走诚实的「没有处理器」：进「已放弃」+ `no_handler` +
/// `requires_user_action`，绝不静默成功。
///
/// `deadline` 只有**自己知道怎么安全停下**的执行者才用（现在只有语义索引）；
/// 提取与关键词重建忽略它（它们没有取消点，硬中断会留下半截状态）。
fn execute(
    core: &mut Core,
    job: &Job,
    deadline: Option<std::time::Instant>,
) -> std::result::Result<HandlerOutcome, HandlerFailure> {
    match job.kind.as_str() {
KIND_EXTRACT => run_extract(core, job).map(|()| HandlerOutcome::Completed),
        KIND_INDEX => run_index(core, job).map(|()| HandlerOutcome::Completed),
        KIND_SEMANTIC_INDEX => run_semantic_index(core, job, deadline),
        kind if kind == provider::KIND_RESPOND => {
            run_respond(core, job).map(|()| HandlerOutcome::Completed)
        }
        _ => Err(HandlerFailure::terminal(
            "no_handler",
            format!(
                "核心还没有「{}」任务的执行者（转写管线是 B4b 的事）",
                job.kind
            ),
        )),
    }
}

/// `extract` 的执行者：对任务 `target_ids` 里的来源修订跑已有提取路径。
///
/// 成败口径：**提取路径跑完就是成功**。提取器自己裁决的 `status=failed` /
/// `metadata_only` 已经如实落在 `extracted_contents` 上（带 coverage 与错误码），
/// 那才是「这个文件能不能搜」的答案；在这里再失败一次，只会让确定性的格式问题
/// 白白退避重试五分钟。只有**结构性失败**（修订不存在、原件不在文件库）才让任务
/// 失败并退避重试——那才是「换个时间可能就好」的情况。
fn run_extract(core: &mut Core, job: &Job) -> std::result::Result<(), HandlerFailure> {
    if job.target_ids.is_empty() {
        return Err(HandlerFailure::terminal(
            ERROR_NO_TARGET,
            "提取任务没有目标来源，重试也不会变好".to_owned(),
        ));
    }
    for target in &job.target_ids {
        core.extract_source(target)
            .map_err(HandlerFailure::retryable)?;
    }
    Ok(())
}

/// `respond` 的执行者：真的调用一次已配置的服务，把正文落进 `ai_responses`。
///
/// 契约 `responses.request` 的「只有主动调用才运行」在这里收口：这条任务只可能由
/// 用户的主动请求排出来，没有任何自动排期路径。空目标（缺 `target_ids`）是结构性
/// 错误，重试不会变好——按 `no_target` 终态失败，不去调用任何服务。
///
/// 失败的处置由 `CoreError` 自己决定（见 `FailureDisposition`）：没配好/鉴权失败→
/// 等配置；超时/断网/限流→退避重试；模型或端点不对→终态失败。
fn run_respond(core: &mut Core, job: &Job) -> std::result::Result<(), HandlerFailure> {
    if job.target_ids.is_empty() {
        return Err(HandlerFailure::terminal(
            provider::ERROR_NO_TARGET,
            "回应任务没有目标，重试也不会变好".to_owned(),
        ));
    }
    provider::run_respond(core, job).map_err(HandlerFailure::from_error)
}

/// `index` 的执行者：对任务 `target_ids` 里的**来源**重跑关键词索引重建。
///
/// **空 `target_ids` = 整库重建**（`indexes.rebuild(scope=all)`）：走
/// `rebuild_keyword_index(None)`，范围是整库，连没有来源的记录文字也重刷一遍。
///
/// **运行期间关键词检索照旧可用**：重建按片段先删后写、在一个事务里提交，
/// 每次写入都会 `bump_index_epoch`。检索会话的快照按 epoch 认账，索引一变旧快照
/// 如实报 `search_expired` 让前端重取，而不是拿半截索引当准的。所以「重建中」
/// 不等于「搜不到」，也不等于「搜到的可能是旧的还假装是新的」。
///
/// 成败口径：
/// - **结构性原因**（目标来源不存在）→ 终态 `failed` + 明确错误码。
///   正常路径上闸门（`resolve_dependencies`）在领取前就已经把它们终态失败了，
///   这里再挡一次是防御：绕过闸门也不能悄悄成功；
/// - **可恢复原因**（IO / 数据库暂时不可用）→ 按 #67 的 `HandlerFailure::retryable`
///   退避重试，消耗额度。
fn run_index(core: &mut Core, job: &Job) -> std::result::Result<(), HandlerFailure> {
    if job.target_ids.is_empty() {
        core.rebuild_keyword_index(None)
            .map_err(HandlerFailure::retryable)?;
        return Ok(());
    }
    for target in &job.target_ids {
        let exists: i64 = core
            .conn
            .query_row(
                "SELECT COUNT(*) FROM source_items WHERE source_id = ?1",
                params![target],
                |row| row.get(0),
            )
            .map_err(|error| HandlerFailure::retryable(error.into()))?;
        if exists == 0 {
            return Err(HandlerFailure::terminal(
                crate::error::ErrorCode::NotFound.wire(),
                format!("索引目标来源不存在：{target}"),
            ));
        }
    }
    core.rebuild_keyword_index(Some(&job.target_ids))
        .map_err(HandlerFailure::retryable)?;
    Ok(())
}

/// `semantic_index` 的执行者：建/重建语义索引，**可以分段让出**。
///
/// 范围：`target_ids` 为空 = 整库（排队入口只排这一种），非空 = 只重算这些来源的块。
/// 但**代次与向量是全局的**：一次重建总是把块表里所有块在新代次里补齐（范围只决定
/// 重算了哪些块），否则切过去会让范围外的块失去生效向量。所以「按来源重建语义索引」
/// 并不省下模型调用，只是让块重建的整日重打包范围小一点。
///
/// 进度：`progress_completed / progress_total` 写的是「这一代里已经有口径一致向量的
/// 块数 / 块表里的块数」，**每算完一块写一次**（写进度不发 `job.changed`——一次整库
/// 重建可能写几万次；状态变化照旧发事件）。让出时它是精确值，`complete` 之后是
/// `total/total`。
///
/// 成败口径：
/// - **让出**（预算到点）→ `HandlerOutcome::Yielded`：任务回 `queued`，下一轮续跑。
///   不是失败、不消耗重试额度（见 `yield_back`）；
/// - **模型拿不到**（没配 / 配了但加载失败，闸门放行时只看到「配了」）→
///   `HandlerOutcome::WaitingConfiguration(WAITING_MODEL)`：落回 `waiting_configuration`。
///   这不是失败——用户把模型配好之后闸门会重新拉起它；
/// - **可恢复原因**（IO / 数据库暂时不可用）→ `HandlerFailure::retryable`：退避重试，
///   消耗额度；`building_generation` 与已写的向量留着，额度内重试就是续跑；
/// - **结构性原因**（算出来的维度与模型声明不符等）→ 终态失败，不假装成功。
fn run_semantic_index(
    core: &mut Core,
    job: &Job,
    deadline: Option<std::time::Instant>,
) -> std::result::Result<HandlerOutcome, HandlerFailure> {
    // 空目标是整库；非空是「只重算这些来源的块」。范围语义与 `rebuild_text_chunks`
    // 一致：解析入口已经保证 id 存在，这里不再重复校验（多来源重建不是结构性错误）。
    let scope = if job.target_ids.is_empty() {
        None
    } else {
        Some(job.target_ids.as_slice())
    };

    // 进度写进**这条任务**。闭包按值捕获 `job_id`（不含 `core`），所以不会与
    // `core` 的可变借用打架。
    let job_id = job.id.clone();
    let mut progress = |core: &mut Core, completed: i64, total: i64| {
        set_progress(core, &job_id, completed, total)
    };

    match crate::semantic::build_with_deadline(core, scope, deadline, &mut progress) {
        Ok(crate::semantic::SemanticBuildOutcome::Done(report)) => {
            // 跑完了：进度补成整代（`missing` 为空时回调一次都不会响）。
            set_progress(core, &job.id, report.total_chunks, report.total_chunks)
                .map_err(HandlerFailure::retryable)?;
            Ok(HandlerOutcome::Completed)
        }
        // 让出：进度已经由回调写到让出那一刻的真值，这里只把结果如实交回驱动。
        Ok(crate::semantic::SemanticBuildOutcome::Yielded(checkpoint)) => {
            Ok(HandlerOutcome::Yielded {
                completed: checkpoint.completed_chunks,
                total: checkpoint.total_chunks,
            })
        }
        // 「拿不到嵌入器」是**配置**问题，不是失败：如实落回等待配置，并把
        // 「为什么拿不到」的原文交给驱动记进这次尝试（诊断用，不进状态摘要）。
        Err(error) if crate::semantic::is_embedder_unavailable(&error) => {
            Ok(HandlerOutcome::WaitingConfiguration {
                reason: WAITING_MODEL,
                message: error.to_string(),
            })
        }
        // 确定性的问题（维度不符、状态不允许）不拖到五轮退避之后才报：直接终态失败，
        // 让用户现在就看见。数据库/IO 这类才走退避重试（`building_generation` 与已写的
        // 向量留着，额度内的重试就是续跑）。
        Err(error) if is_structural_failure(&error) => Err(HandlerFailure::terminal(
            error.code().wire(),
            error.to_string(),
        )),
        Err(error) => Err(HandlerFailure::retryable(error)),
    }
}

/// 执行者遇到的核心错误里，哪些是**确定性**的（换个时间重试也不会变好）？
///
/// 只有 `InvalidState`（状态不允许：例如算出来的向量维度与模型声明不符）与
/// `InvalidArgument`（输入不合法）。数据库 / IO / 序列化那几类仍按可恢复处理——
/// 它们可能只是这一刻不可用（原件暂时读不到、库被锁住）。
fn is_structural_failure(error: &CoreError) -> bool {
    matches!(
        error,
        CoreError::InvalidState { .. } | CoreError::InvalidArgument { .. }
    )
}

/// 分段让出：任务回到 `queued`（`next_attempt_at` 清空 = **立刻**可再跑），
/// 并把「这次不算失败」如实落库。
///
/// 为什么 `attempt_count` 要减回去：领取时它已经 +1（`claim_next_due_record`），而
/// 「这一轮预算到点、在安全点让出」不是一次失败——一条整库重建可能要让出几十轮，
/// 按失败计会白白耗尽 `max_attempts`，把一条本来能跑完的任务变成「已放弃」。
/// `job_attempts` 里那次开始的记录**留着**（它确实开始过、也写过向量），
/// `outcome = 'yielded'` 说明它是怎么结束的。
///
/// 清空 `next_attempt_at` 而不是排退避：让出是「还想接着跑」，不是「等一等再试」。
/// 它落到 `queued` 且入队即到期，于是 `next_wakeup` 指向「现在」、
/// `due_backlog ≥ 1`——平台层照摘要立刻再叫一次驱动就继续算。
fn yield_back(core: &mut Core, job_id: &str, now: DateTime<Utc>) -> Result<()> {
    let now_text = support::to_iso(now);
    let tx = core.conn.transaction()?;
    tx.execute(
        "UPDATE jobs SET state = 'queued', error_code = NULL, next_attempt_at = NULL, \
         attempt_count = MAX(attempt_count - 1, 0), requires_user_action = 0, \
         attention_key = NULL, updated_at = ?1 WHERE id = ?2",
        params![now_text, job_id],
    )?;
    tx.execute(
        "UPDATE job_attempts SET finished_at = ?1, outcome = 'yielded' \
         WHERE job_id = ?2 AND finished_at IS NULL",
        params![now_text, job_id],
    )?;
    crate::insert_event(&tx, EventType::JobChanged, job_id, 1)?;
    tx.commit()?;
    Ok(())
}

/// 写任务进度。`total <= 0` 时清空（没有可靠进度时必须是 `None`，不虚构百分比）。
///
/// **不发 `job.changed`**：一次整库重建每算一块就写一次，几万条事件不是通知，
/// 是噪声。状态的每一次变化（让出、完成、失败…）照旧发事件；纯进度用
/// `listJobs` / `jobStatus` 轮询看。
pub(crate) fn set_progress(
    core: &mut Core,
    job_id: &str,
    completed: i64,
    total: i64,
) -> Result<()> {
    let (completed, total) = if total > 0 {
        (Some(completed.max(0)), Some(total))
    } else {
        (None, None)
    };
    core.conn.execute(
        "UPDATE jobs SET progress_completed = ?1, progress_total = ?2, updated_at = ?3 \
         WHERE id = ?4",
        params![completed, total, support::to_iso(support::now()), job_id],
    )?;
    Ok(())
}

/// 落在 `running` 的任务一定是「上一次没走完」的：同一资料库同一时刻只有一个驱动
/// 在跑（`BridgeSession` 用互斥锁把 `Core` 锁住），运行开始时不可能有别的执行者
/// 还在跑同一条任务。
///
/// 回收而不是丢掉：用过的次数**如实保留**（它确实开始过），还有额度就立刻可再跑
/// （这不是「失败了等一等」，而是「上次根本没跑完」，不该退避），次数用完就进
/// 「已放弃」等用户处理。不回收的话这些任务会被 `claim_next_due` 永远跳过。
fn recover_interrupted_runs(core: &mut Core, now: DateTime<Utc>) -> Result<i64> {
    let ids: Vec<String> = {
        let mut stmt = core
            .conn
            .prepare("SELECT id FROM jobs WHERE state = 'running' ORDER BY created_at, id")?;
        let collected = stmt
            .query_map([], |row| row.get(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        collected
    };

    let now_text = support::to_iso(now);
    let mut recovered = 0;
    for job_id in ids {
        let job = load_job(&core.conn, &job_id)?;
        let exhausted = job.attempt_count >= job.max_attempts;
        let tx = core.conn.transaction()?;
        if exhausted {
            tx.execute(
                "UPDATE jobs SET state = 'failed', error_code = 'interrupted', \
                 next_attempt_at = NULL, requires_user_action = 1, updated_at = ?1 WHERE id = ?2",
                params![now_text, job_id],
            )?;
        } else {
            tx.execute(
                "UPDATE jobs SET state = 'queued', next_attempt_at = NULL, error_code = NULL, \
                 requires_user_action = 0, updated_at = ?1 WHERE id = ?2",
                params![now_text, job_id],
            )?;
        }
        tx.execute(
            "UPDATE job_attempts SET finished_at = ?1, outcome = 'interrupted', \
             error_code = 'interrupted' WHERE job_id = ?2 AND finished_at IS NULL",
            params![now_text, job_id],
        )?;
        crate::insert_event(&tx, EventType::JobChanged, &job_id, 1)?;
        tx.commit()?;
        recovered += 1;
    }
    Ok(recovered)
}

// ------------------------------------------------------------------ 状态

/// 现在就到期的任务数（含退避已经到点的重试）。
pub(crate) fn count_due(core: &Core, now: DateTime<Utc>) -> Result<i64> {
    let total = core.conn.query_row(
        "SELECT COUNT(*) FROM jobs WHERE state IN ('queued', 'retry_wait') \
         AND (next_attempt_at IS NULL OR next_attempt_at <= ?1)",
        params![support::to_iso(now)],
        |row| row.get(0),
    )?;
    Ok(total)
}

/// 任务面板状态：各状态计数、到期积压、下次唤醒与最近错误。
///
/// 「最近错误」只从 `state IN ('failed', 'retry_wait')` 里取 `error_code` 与
/// `updated_at`，**不带 `job_attempts.message`**：那里可能有文件路径或原始报错，
/// 属于排查信息，不该跟着状态摘要流出。
pub(crate) fn status(core: &Core) -> Result<JobStatus> {
    let mut status = JobStatus {
        queued: 0,
        running: 0,
        retry_wait: 0,
        waiting_configuration: 0,
        waiting_dependency: 0,
        waiting_network: 0,
        failed: 0,
        cancelled: 0,
        succeeded: 0,
        due_backlog: 0,
        next_wakeup: None,
        recent_errors: Vec::new(),
    };

    let rows: Vec<(String, i64)> = {
        let mut stmt = core
            .conn
            .prepare("SELECT state, COUNT(*) FROM jobs GROUP BY state")?;
        let collected = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        collected
    };
    for (wire, count) in rows {
        match JobState::from_wire(&wire) {
            Some(JobState::Queued) => status.queued = count,
            Some(JobState::Running) => status.running = count,
            Some(JobState::RetryWait) => status.retry_wait = count,
            Some(JobState::WaitingConfiguration) => status.waiting_configuration = count,
            Some(JobState::WaitingDependency) => status.waiting_dependency = count,
            Some(JobState::WaitingNetwork) => status.waiting_network = count,
            Some(JobState::Failed) => status.failed = count,
            Some(JobState::Cancelled) => status.cancelled = count,
            Some(JobState::Succeeded) => status.succeeded = count,
            None => {
                return Err(CoreError::CorruptedData {
                    message: format!("未知任务状态：{wire}"),
                })
            }
        }
    }

    let errors: Vec<(String, String, String, i64, String)> = {
        let mut stmt = core.conn.prepare(
            "SELECT id, kind, error_code, requires_user_action, updated_at FROM jobs \
             WHERE error_code IS NOT NULL AND state IN ('failed', 'retry_wait') \
             ORDER BY updated_at DESC, id LIMIT ?1",
        )?;
        let collected = stmt
            .query_map(params![RECENT_ERROR_LIMIT], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        collected
    };
    status.recent_errors = errors
        .into_iter()
        .map(|(job_id, kind, error_code, requires_user_action, updated_at)| {
            Ok(JobErrorSummary {
                job_id,
                kind,
                error_code,
                occurred_at: support::parse_iso(&updated_at)?,
                requires_user_action: requires_user_action != 0,
            })
        })
        .collect::<Result<Vec<_>>>()?;

    let now = support::now();
    status.due_backlog = count_due(core, now)?;
    status.next_wakeup = next_wakeup(core)?;
    Ok(status)
}

// ------------------------------------------------------------------ 内部

pub(crate) fn load_job(conn: &rusqlite::Connection, job_id: &str) -> Result<Job> {
    let row = conn
        .query_row(
            "SELECT id, kind, state, priority, target_ids, input_snapshot_hash, \
             progress_completed, progress_total, attempt_count, max_attempts, next_attempt_at, \
             error_code, requires_user_action, attention_key, created_at, updated_at \
             FROM jobs WHERE id = ?1",
            params![job_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, Option<i64>>(6)?,
                    row.get::<_, Option<i64>>(7)?,
                    row.get::<_, i64>(8)?,
                    row.get::<_, i64>(9)?,
                    row.get::<_, Option<String>>(10)?,
                    row.get::<_, Option<String>>(11)?,
                    row.get::<_, i64>(12)?,
                    row.get::<_, Option<String>>(13)?,
                    row.get::<_, String>(14)?,
                    row.get::<_, String>(15)?,
                ))
            },
        )
        .optional()?
        .ok_or_else(|| CoreError::NotFound {
            entity: "任务",
            id: job_id.to_owned(),
        })?;

    let progress = match (row.6, row.7) {
        // 没有可靠进度时必须是 None：不虚构百分比。
        (Some(completed), Some(total)) if total > 0 => Some(JobProgress { completed, total }),
        _ => None,
    };

    Ok(Job {
        id: row.0,
        kind: row.1,
        state: JobState::from_wire(&row.2).ok_or_else(|| CoreError::CorruptedData {
            message: format!("未知任务状态：{}", row.2),
        })?,
        priority: row.3,
        target_ids: serde_json::from_str(&row.4)?,
        input_snapshot_hash: row.5,
        progress,
        attempt_count: row.8,
        max_attempts: row.9,
        next_attempt_at: row.10.map(|text| support::parse_iso(&text)).transpose()?,
        error_code: row.11,
        requires_user_action: row.12 != 0,
        attention_key: row.13,
        created_at: support::parse_iso(&row.14)?,
        updated_at: support::parse_iso(&row.15)?,
    })
}
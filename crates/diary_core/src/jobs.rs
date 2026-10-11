//! 持久任务队列与驱动，契约第 4.4 节与任务书 7.1 节。
//!
//! 队列本身：入队（带业务键幂等）、领取、成功/失败、退避重试、取消、恢复、
//! 下次唤醒时刻与通知去重。
//!
//! 驱动（`run_due`）：把到期任务取出来，按 `kind` 找执行者跑掉，再把结果落回队列。
//! 目前只有 `extract` 真有执行者；没有执行者的 `kind` **不会静默成功**，而是
//! 进「已放弃」+ `error_code = no_handler` 等人工处理（见 `execute`）。
//!
//! 优先级照任务书 7.1 节：保存与录音片段登记最高，整库重建最低。
//! 决定与口径写在 `docs/architecture/m4-任务调度与补做.md`。

use chrono::{DateTime, Duration, Utc};
use rusqlite::{params, OptionalExtension, Transaction};

use crate::error::{CoreError, Result};
use crate::model::{
    EventType, Job, JobErrorSummary, JobProgress, JobRunSummary, JobState, JobStatus, NewJob,
};
use crate::support;
use crate::Core;

/// 真有执行者的任务类型：导入完成后对来源修订跑已有提取路径。
pub(crate) const KIND_EXTRACT: &str = "extract";

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
    if let Some(existing) = find_active_by_business_key(tx, &job)? {
        return Ok(existing);
    }

    let now = support::now();
    let id = support::new_id("job");
    let target_ids = serde_json::to_string(&job.target_ids)?;
    let max_attempts = job.max_attempts.max(1);
    tx.execute(
        "INSERT INTO jobs (id, kind, state, priority, target_ids, input_snapshot_hash, \
         attempt_count, max_attempts, next_attempt_at, created_at, updated_at) \
         VALUES (?1, ?2, 'queued', ?3, ?4, ?5, 0, ?6, ?7, ?8, ?8)",
        params![
            id,
            job.kind,
            job.priority.value(),
            target_ids,
            job.input_snapshot_hash,
            max_attempts,
            job.next_attempt_at.map(support::to_iso),
            support::to_iso(now),
        ],
    )?;
    crate::insert_event(tx, EventType::JobChanged, &id, 1)?;
    Ok(Job {
        id,
        kind: job.kind,
        state: JobState::Queued,
        priority: job.priority.value(),
        target_ids: job.target_ids,
        input_snapshot_hash: job.input_snapshot_hash,
        progress: None,
        attempt_count: 0,
        max_attempts,
        next_attempt_at: job.next_attempt_at,
        error_code: None,
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
    let job = load_job(&core.conn, job_id)?;
    let exhausted = job.attempt_count >= job.max_attempts;
    let (state, next_attempt, requires_user_action) = if exhausted {
        // 自动重试到头了：这是需要用户看见的事，不是「安静地算了」。
        (JobState::Failed, None, 1_i64)
    } else {
        (
            JobState::RetryWait,
            Some(support::to_iso(now + backoff(job.attempt_count))),
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

// ------------------------------------------------------------------ 驱动

/// 处理器的失败。`terminal = true` 表示「重试也不会好」（例如没有处理器）。
struct HandlerFailure {
    code: String,
    message: String,
    terminal: bool,
}

impl HandlerFailure {
    fn terminal(code: &str, message: String) -> Self {
        Self {
            code: code.to_owned(),
            message,
            terminal: true,
        }
    }

    /// 核心错误：还有机会换个时间重试（原件暂时不在、修订暂时读不到）。
    fn retryable(error: CoreError) -> Self {
        Self {
            code: error.code().wire().to_owned(),
            message: error.to_string(),
            terminal: false,
        }
    }
}

/// 跑到期任务：领取 → 按 `kind` 执行 → 把结果落回队列。
///
/// 口径（都写在 `docs/architecture/m4-任务调度与补做.md`）：
/// - **`limit`**：本次最多跑几个，0 表示一个都不跑。被截住的到期任务留在队列里，
///   数量如实报进 `due_backlog`；
/// - **补做**：领取时任务的 `next_attempt_at` 已经早于本次运行的开始时刻
///   （`started_at`）。`next_attempt_at` 为空表示「入队即到期」——队列里没有它
///   本该何时跑的信息，所以**不计入补做**（否则刚入队就被跑的任务也会算成补做，
///   反而误导）；这类积压由 `due_backlog` 呈现。我们只说「补做」，不说「准时」；
/// - **运行开始先回收**上一次没跑完（`running`）的任务，否则那些任务会被永久丢掉。
pub(crate) fn run_due(core: &mut Core, now: DateTime<Utc>, limit: usize) -> Result<JobRunSummary> {
    let mut summary = JobRunSummary {
        started_at: now,
        processed: 0,
        succeeded: 0,
        failed: 0,
        retrying: 0,
        backfilled: 0,
        recovered: recover_interrupted_runs(core, now)?,
        due_backlog: 0,
        next_wakeup: None,
    };

    let mut processed: usize = 0;
    while processed < limit {
        let Some(claimed) = claim_next_due_record(core, now)? else {
            break;
        };
        if let Some(due_at) = claimed.due_at {
            if due_at < now {
                summary.backfilled += 1;
            }
        }

        let job = claimed.job;
        match execute(core, &job) {
            Ok(()) => {
                complete(core, &job.id)?;
                summary.succeeded += 1;
            }
            Err(failure) => {
                if failure.terminal {
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
                } else {
                    let after = fail(core, &job.id, &failure.code, &failure.message, now)?;
                    if after.state == JobState::Failed {
                        summary.failed += 1;
                    } else {
                        summary.retrying += 1;
                    }
                }
            }
        }
        processed += 1;
    }
    summary.processed = processed as i64;
    summary.due_backlog = count_due(core, now)?;
    summary.next_wakeup = next_wakeup(core)?;
    Ok(summary)
}

/// 按 `kind` 找执行者。
///
/// **只有真有实现的 kind 才被执行**。没有执行者的 kind（现在只有 `transcribe`：
/// 它等 B4a 的 Provider）走诚实的「没有处理器」：进「已放弃」+ `no_handler` +
/// `requires_user_action`，绝不静默成功。
fn execute(core: &mut Core, job: &Job) -> std::result::Result<(), HandlerFailure> {
    match job.kind.as_str() {
        KIND_EXTRACT => run_extract(core, job),
        _ => Err(HandlerFailure::terminal(
            "no_handler",
            format!(
                "核心还没有「{}」任务的执行者（转写要等 B4a 的 Provider 接上）",
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
            "no_target",
            "提取任务没有目标来源，重试也不会变好".to_owned(),
        ));
    }
    for target in &job.target_ids {
        core.extract_source(target)
            .map_err(HandlerFailure::retryable)?;
    }
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

fn load_job(conn: &rusqlite::Connection, job_id: &str) -> Result<Job> {
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
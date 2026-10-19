//! `Capture.processing_summary` 的真实计算（#146）。
//!
//! 契约第 2.1 节里 `processingSummary` 是 `Capture` 的正式字段（「未处理、处理中、
//! 可检索、需关注的数量」）。这个模块是它**唯一**的写入路径：`capture_from_raw`
//! 不再用 `ProcessingSummary::default()` 冒充答案。
//!
//! # 四个桶的口径
//!
//! 四个数字都是**此刻**的状态，不是累计；`unprocessed` / `searchable` 数**来源**，
//! `processing` / `needs_attention` 数**任务**（一个来源同时有 extract 与 index 两条
//! 活任务时，`processing` 是 2——它们确实是两条还没跑完的任务）。口径与失败任务的
//! 归属同时写在 `docs/development/01-共享接口契约.md`，两处必须一起改。
//!
//! - `unprocessed`：当前修订**还没有可检索正文**、并且**没有指向这个来源的活任务**的
//!   来源数。「可检索」的判据见下一条。失败 / 取消的提取、只拿到文件元数据
//!   （`coverage = metadata_only`）都落在这里：它们确实还搜不到，只是没有人在处理了。
//!   这是有意的——宁可显示「还有来源没处理完」，也不能因为任务进了终态就假装好了。
//! - `processing`：指向这条记录（来源 / 修订 / 资产 / 记录本身 / `ai_responses` 里
//!   的 `capture_ids`）的**活任务**数。活 = `state NOT IN ('succeeded','failed',
//!   'cancelled')`：`queued` / `running` / `retry_wait` / `waiting_dependency` /
//!   `waiting_configuration` / `waiting_network` 都算。**退避重试与等前置/等配置的
//!   任务仍然活着**，把它们排除会让这些来源既不「处理中」也不「未处理」——那正是
//!   #146 要修掉的「看起来一切正常」。
//! - `searchable`：当前修订已经有**当前关键词索引**的来源数：
//!   - 有原件的来源：`extracted_contents`（当前修订）→ `extracted_segments` →
//!     `search_docs` 至少一行；
//!   - 文字来源：同一条记录的文字已在 `search_capture_docs` 里（提交时创建的正文
//!     来源走的就是这一路，没有提取这一环）。
//!   只认当前修订：旧修订留在索引里的行不算（`index_status` 同样把它们排除）。
//! - `needs_attention`：指向这条记录的**任务**里 `requires_user_action = 1` 且
//!   `attention_key` 未被 `attention_acks` 确认的条数。`attention_key` 为空的任务
//!   无法确认，会一直算到被重试 / 取消——不替它编一个 key。
//!
//! `searchable` 与 `processing` 可以同时为非零（例如已经有旧索引、又在重建）。
//! 四个数字不是互斥分区；但**有来源就一定至少落进 unprocessed / processing /
//! searchable 之一**：这就是「只有空记录才全 0」的保证。
//!
//! # 目标 id 的语义（不假设）
//!
//! `jobs.target_ids` 是 JSON 数组，而不同 kind 瞄的东西不一样：
//! - `extract` 瞄**来源修订**（`assets::finish_import` 写的是 `revision_id`）；
//! - `index` / `semantic_index` 瞄**来源**（空数组 = 整库）；
//! - `transcribe` 瞄**资产 id**（`recordings::finalize`）；
//! - `respond` 瞄 `ai_responses.id`（记录列表在 `ai_responses.capture_ids` 里）；
//! - `diary_generate` 瞄 `diary_generations.id`（按天，不属于某一条记录）。
//!
//! 所以这里用「目标 id → 来源/记录」的多路映射，而不是拿 `target_ids` 直接比
//! `source_id`。**整库任务（`target_ids` 为空）与按天的日记生成任务不属于任何一条
//! 记录**，不计入任何 capture 的摘要——把它们摊给每条记录会让全库重建一跑，所有
//! 记录都显示「处理中」。它们在 `jobs.status` / `list_jobs` 里看得到。
//!
//! # 为什么是一条 SQL
//!
//! `captures.list` 一次最多返回 100 条记录；按记录逐条查 4 次库就是 N+1。这里把
//! 整页的 id 一次性绑定进 `src` CTE，所有聚合都在同一条语句里完成（`GROUP BY`
//! 按 capture 汇总），调用方只把结果按 id 贴回记录。`get_capture` 走同一个函数、
//! 只有一个 id，口径不会分叉。
//!
//! # 现有数据无法判定时
//!
//! 不解析 `extracted_contents.status` 来「猜成功」：`searchable` 只看当前修订有没有
//! 真的索引行。状态是未知值、提取任务丢了、修订没有 `current_revision_id`——只要
//! 索引里没有当前正文、也没有活任务，就如实算 `unprocessed`。`json_valid` 守卫让
//! 一条坏掉的 `target_ids` 不会把整条查询弄挂；那条任务也就不归属任何记录（它仍
//! 在任务面板里）。

use std::collections::HashMap;

use rusqlite::{params_from_iter, Connection};

use crate::error::Result;
use crate::model::ProcessingSummary;

/// 一次算出多条记录的摘要。返回的 map 只含有来源的记录：没有来源的记录（刚建的
/// 草稿）没有行，调用方用 `ProcessingSummary::default()`——那是全 0 唯一合法的场景。
pub(crate) fn summaries(
    conn: &Connection,
    capture_ids: &[String],
) -> Result<HashMap<String, ProcessingSummary>> {
    if capture_ids.is_empty() {
        return Ok(HashMap::new());
    }

    let sql = summary_sql(capture_ids.len());
    let mut statement = conn.prepare(&sql)?;
    let rows = statement
        .query_map(params_from_iter(capture_ids.iter()), |row| {
            Ok((
                row.get::<_, String>(0)?,
                ProcessingSummary {
                    unprocessed: u32_count(row.get::<_, i64>(1)?),
                    processing: u32_count(row.get::<_, i64>(2)?),
                    searchable: u32_count(row.get::<_, i64>(3)?),
                    needs_attention: u32_count(row.get::<_, i64>(4)?),
                },
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    Ok(rows.into_iter().collect())
}

/// 一条记录的摘要。没有来源时返回全 0（唯一合法的全 0）。
pub(crate) fn summary(conn: &Connection, capture_id: &str) -> Result<ProcessingSummary> {
    let id = capture_id.to_owned();
    Ok(summaries(conn, std::slice::from_ref(&id))?
        .remove(capture_id)
        .unwrap_or_default())
}

/// 计数是 `i64`（SQLite），契约字段是 `u32`。超出时饱和到 `u32::MAX`：
/// 真到那个量级，显示 `u32::MAX` 比 panic / 截断成一个看起来正常的小数字诚实。
fn u32_count(value: i64) -> u32 {
    u32::try_from(value.max(0)).unwrap_or(u32::MAX)
}

/// 摘要 SQL。`{placeholders}` 只由调用方按 id 个数生成（`?,?,...`），没有注入面。
///
/// CTE 的分工：
/// - `src`：这一页记录的全部来源（也是「有来源」的集合）；
/// - `indexed`：当前修订真的有关键词索引行的来源；
/// - `live_jobs` / `attention_jobs`：两道任务集合，分别用部分索引
///   `idx_jobs_live` / `idx_jobs_attention` 扫，不扫历史终态任务；
/// - `live_source_hits`：活任务 → 来源（只用于 `unprocessed` 的「没人在处理」判断）；
/// - `live_job_captures` / `attention_job_captures`：任务 → 记录（数任务条数）；
/// - `source_counts` / `processing_counts` / `attention_counts`：三路 GROUP BY，
///   最后按 capture 拼起来。
///
/// 映射用 `CROSS JOIN` 固定「先扫少数活任务、再点查目标」的顺序；写成普通 JOIN 时
/// SQLite 可能反过来先整表扫 `source_revisions`（EXPLAIN 实测过），在大库上就是
/// 一次全表扫描。
fn summary_sql(id_count: usize) -> String {
    let placeholders = std::iter::repeat_n("?", id_count)
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        r#"
WITH
src AS (
    SELECT si.capture_id, si.source_id, si.current_revision_id
    FROM source_items si
    WHERE si.capture_id IN ({placeholders})
),
indexed AS (
    SELECT DISTINCT s.capture_id, s.source_id
    FROM src s
    WHERE EXISTS (
        SELECT 1
        FROM extracted_contents ec
        JOIN extracted_segments es ON es.content_id = ec.id
        JOIN search_docs sd ON sd.segment_id = es.id
        WHERE ec.source_revision_id = s.current_revision_id
    )
    OR EXISTS (
        SELECT 1
        FROM source_revisions sr
        JOIN search_capture_docs scd ON scd.capture_id = s.capture_id
        WHERE sr.revision_id = s.current_revision_id
          AND sr.text IS NOT NULL
    )
),
live_jobs AS (
    -- `INDEXED BY` 是刻意的：不加时规划器在「目标 → 记录」那几段会改扫 jobs 的
    -- 主键索引（EXPLAIN 实测），活任务一多就退化成扫历史行。索引由
    -- `schema::ensure_processing_summary_indexes` 在 open 时保证存在。
    SELECT j.id AS job_id, t.value AS target
    FROM jobs j INDEXED BY idx_jobs_live
    JOIN json_each(CASE WHEN json_valid(j.target_ids) AND json_type(j.target_ids) = 'array'
                        THEN j.target_ids ELSE '[]' END) AS t
    WHERE j.state NOT IN ('succeeded', 'failed', 'cancelled')
),
live_source_hits AS (
    SELECT DISTINCT si.capture_id, si.source_id
    FROM live_jobs lj
    CROSS JOIN source_items si ON si.source_id = lj.target
    UNION
    SELECT DISTINCT si.capture_id, si.source_id
    FROM live_jobs lj
    CROSS JOIN source_revisions sr ON sr.revision_id = lj.target
    CROSS JOIN source_items si ON si.source_id = sr.source_id
    UNION
    SELECT DISTINCT si.capture_id, si.source_id
    FROM live_jobs lj
    CROSS JOIN source_revisions sr ON sr.asset_id = lj.target
    CROSS JOIN source_items si ON si.source_id = sr.source_id
),
live_job_captures AS (
    SELECT DISTINCT lj.job_id, si.capture_id
    FROM live_jobs lj
    CROSS JOIN source_items si ON si.source_id = lj.target
    UNION
    SELECT DISTINCT lj.job_id, si.capture_id
    FROM live_jobs lj
    CROSS JOIN source_revisions sr ON sr.revision_id = lj.target
    CROSS JOIN source_items si ON si.source_id = sr.source_id
    UNION
    SELECT DISTINCT lj.job_id, si.capture_id
    FROM live_jobs lj
    CROSS JOIN source_revisions sr ON sr.asset_id = lj.target
    CROSS JOIN source_items si ON si.source_id = sr.source_id
    UNION
    SELECT DISTINCT lj.job_id, c.id
    FROM live_jobs lj
    CROSS JOIN captures c ON c.id = lj.target
    UNION
    SELECT DISTINCT lj.job_id, ca.value
    FROM live_jobs lj
    CROSS JOIN ai_responses r ON r.id = lj.target
    CROSS JOIN json_each(CASE WHEN json_valid(r.capture_ids) AND json_type(r.capture_ids) = 'array'
                              THEN r.capture_ids ELSE '[]' END) AS ca
),
attention_jobs AS (
    SELECT j.id AS job_id, t.value AS target
    FROM jobs j INDEXED BY idx_jobs_attention
    JOIN json_each(CASE WHEN json_valid(j.target_ids) AND json_type(j.target_ids) = 'array'
                        THEN j.target_ids ELSE '[]' END) AS t
    WHERE j.requires_user_action = 1
      AND (
          j.attention_key IS NULL
          OR NOT EXISTS (SELECT 1 FROM attention_acks a WHERE a.attention_key = j.attention_key)
      )
),
attention_job_captures AS (
    SELECT DISTINCT aj.job_id, si.capture_id
    FROM attention_jobs aj
    CROSS JOIN source_items si ON si.source_id = aj.target
    UNION
    SELECT DISTINCT aj.job_id, si.capture_id
    FROM attention_jobs aj
    CROSS JOIN source_revisions sr ON sr.revision_id = aj.target
    CROSS JOIN source_items si ON si.source_id = sr.source_id
    UNION
    SELECT DISTINCT aj.job_id, si.capture_id
    FROM attention_jobs aj
    CROSS JOIN source_revisions sr ON sr.asset_id = aj.target
    CROSS JOIN source_items si ON si.source_id = sr.source_id
    UNION
    SELECT DISTINCT aj.job_id, c.id
    FROM attention_jobs aj
    CROSS JOIN captures c ON c.id = aj.target
    UNION
    SELECT DISTINCT aj.job_id, ca.value
    FROM attention_jobs aj
    CROSS JOIN ai_responses r ON r.id = aj.target
    CROSS JOIN json_each(CASE WHEN json_valid(r.capture_ids) AND json_type(r.capture_ids) = 'array'
                              THEN r.capture_ids ELSE '[]' END) AS ca
),
source_counts AS (
    SELECT
        s.capture_id,
        SUM(CASE WHEN ix.source_id IS NULL AND lh.source_id IS NULL THEN 1 ELSE 0 END) AS unprocessed,
        SUM(CASE WHEN ix.source_id IS NOT NULL THEN 1 ELSE 0 END) AS searchable
    FROM src s
    LEFT JOIN indexed ix
           ON ix.capture_id = s.capture_id AND ix.source_id = s.source_id
    LEFT JOIN live_source_hits lh
           ON lh.capture_id = s.capture_id AND lh.source_id = s.source_id
    GROUP BY s.capture_id
),
processing_counts AS (
    SELECT ljc.capture_id, COUNT(DISTINCT ljc.job_id) AS processing
    FROM live_job_captures ljc
    WHERE ljc.capture_id IN (SELECT capture_id FROM src)
    GROUP BY ljc.capture_id
),
attention_counts AS (
    SELECT ajc.capture_id, COUNT(DISTINCT ajc.job_id) AS needs_attention
    FROM attention_job_captures ajc
    WHERE ajc.capture_id IN (SELECT capture_id FROM src)
    GROUP BY ajc.capture_id
)
SELECT
    sc.capture_id,
    sc.unprocessed,
    COALESCE(pc.processing, 0),
    sc.searchable,
    COALESCE(ac.needs_attention, 0)
FROM source_counts sc
LEFT JOIN processing_counts pc ON pc.capture_id = sc.capture_id
LEFT JOIN attention_counts ac ON ac.capture_id = sc.capture_id
"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `#146` 的性能回归：聚合只扫两条**部分索引**，不能退化成全表扫
    /// `jobs` / `source_revisions`。真实列表路径（100 条记录）走的是同一条 SQL，
    /// 这里用一条记录把计划钉住。
    #[test]
    fn query_plan_uses_the_partial_indexes() {
        let mut conn = Connection::open_in_memory().unwrap();
        crate::schema::migrate(&mut conn).unwrap();
        conn.execute_batch(
            r#"
            INSERT INTO captures
                (id, revision, state, occurred_at, created_at, updated_at, time_zone,
                 utc_offset_minutes, day_key, draft_text)
            VALUES ('cap_a', 2, 'committed', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z',
                    '2026-01-01T00:00:00Z', 'UTC', 0, '2026-01-01', '');
            INSERT INTO source_items (source_id, capture_id, kind, position, current_revision_id)
            VALUES ('src_a', 'cap_a', 'text', 0, 'rev_a');
            INSERT INTO source_revisions
                (revision_id, source_id, parent_revision_id, text, asset_id, author_type, occurred_at)
            VALUES ('rev_a', 'src_a', NULL, 'body', NULL, 'user', '2026-01-01T00:00:00Z');
            INSERT INTO jobs
                (id, kind, state, priority, target_ids, attempt_count, max_attempts,
                 requires_user_action, created_at, updated_at)
            VALUES ('job_live', 'extract', 'queued', 30, '["rev_a"]', 0, 5, 0,
                    '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z');
            INSERT INTO jobs
                (id, kind, state, priority, target_ids, attempt_count, max_attempts,
                 requires_user_action, attention_key, created_at, updated_at)
            VALUES ('job_attention', 'future_kind', 'failed', 10, '["src_a"]', 0, 5, 1,
                    'job-terminal:future_kind:no_handler', '2026-01-01T00:00:00Z',
                    '2026-01-01T00:00:00Z');
            "#,
        )
        .unwrap();

        let sql = format!("EXPLAIN QUERY PLAN {}", summary_sql(1));
        let mut statement = conn.prepare(&sql).unwrap();
        let plan: Vec<String> = statement
            .query_map(["cap_a"], |row| row.get::<_, String>(3))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        for line in &plan {
            println!("PLAN {line}");
        }

        assert!(
            plan.iter().any(|line| line.contains("idx_jobs_live")),
            "活任务必须走 idx_jobs_live，不能扫整张 jobs"
        );
        assert!(
            plan.iter().any(|line| line.contains("idx_jobs_attention")),
            "待处理任务必须走 idx_jobs_attention"
        );
        for line in &plan {
            assert!(!line.starts_with("SCAN jobs"), "不能全表扫 jobs：{line}");
            assert!(
                !line.starts_with("SCAN source_revisions"),
                "不能全表扫 source_revisions：{line}"
            );
            assert!(
                !line.starts_with("SCAN source_items"),
                "不能全表扫 source_items：{line}"
            );
        }
    }
}

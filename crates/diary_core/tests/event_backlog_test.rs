//! 事件轮询的**有界读**、超前游标与保留策略（#1019 / C ABI `core.events`）。
//!
//! 这三条以前没有定义，也没有测试：
//!
//! 1. `events_since` 没有 `LIMIT`，桥面先 materialize 全部事件再截断——`from_sequence: 0`
//!    在事件表很大时会一次性读进内存；现在读的是 `events_page(from, limit)`（SQL `LIMIT
//!    limit+1`，多读一条只用来回答 `truncated`）；
//! 2. `from_sequence` 大于当前最大序号时，旧实现原样返回一个**未来游标**，调用方持续
//!    回传就永久跳过之后的事件。现在夹到当前最大序号，下一次调用就能接上；
//! 3. `domain_events` 没有保留策略。现在只保留最近 [`EVENT_BACKLOG_LIMIT`] 条（在写入的
//!    同一个事务里删），早于窗口的游标返回 `cursor_expired`（契约第 6 节：重读快照），
//!    而不是假装没有遗漏。

use chrono::{TimeZone, Utc};
use diary_core::{
    Core, CreateDraftInput, ErrorCode, EVENT_BACKLOG_LIMIT,
};
use rusqlite::params;

fn new_core(dir: &std::path::Path) -> Core {
    Core::open(dir.join("library.sqlite")).unwrap()
}

fn draft(core: &mut Core, operation: &str) -> String {
    core.create_draft(CreateDraftInput {
        occurred_at: Some(Utc.with_ymd_and_hms(2026, 10, 18, 4, 0, 0).unwrap()),
        time_zone: "Asia/Shanghai",
        utc_offset_minutes: 480,
        operation_id: operation,
    })
    .unwrap()
    .id
}

/// 一页只给 `limit` 条并如实标 `truncated`；按 `next_sequence` 继续能推到同一末尾。
#[test]
fn events_page_is_bounded_and_reports_truncation() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    for index in 0..5 {
        draft(&mut core, &format!("op-{index}"));
    }
    let max = core.last_event_sequence().unwrap();
    assert_eq!(max, 5, "五条草稿各留一条事件");

    let first = core.events_page(0, 2).unwrap();
    assert_eq!(first.events.len(), 2, "limit 是硬上限");
    assert!(first.truncated, "后面还有事件");
    assert_eq!(first.next_sequence, 2);

    let rest = core.events_page(first.next_sequence, 100).unwrap();
    assert!(!rest.truncated);
    assert_eq!(rest.events.len(), 3);
    assert_eq!(rest.next_sequence, max);
    assert_eq!(
        rest.events.iter().map(|event| event.sequence).collect::<Vec<_>>(),
        vec![3, 4, 5],
        "按 next_sequence 续读不重不漏"
    );

    // limit = 0 是空页，不读表；不会把游标推走。
    let empty = core.events_page(1, 0).unwrap();
    assert!(empty.events.is_empty());
    assert_eq!(empty.next_sequence, 1);
    assert!(!empty.truncated);
}

/// 游标**超前**时夹到当前最大序号：不报错、不返回未来序号，新事件照样收得到。
#[test]
fn an_ahead_cursor_is_clamped_to_the_current_max() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    draft(&mut core, "op-one");
    let max = core.last_event_sequence().unwrap();

    // 真实场景：调用方存了游标，库被恢复成更旧的备份——游标比库里的序号还大。
    let page = core.events_page(max + 999, 10).unwrap();
    assert!(page.events.is_empty());
    assert_eq!(
        page.next_sequence, max,
        "超前游标要夹到当前最大序号，不能返回一个未来的序号"
    );
    assert!(
        page.next_sequence < max + 999,
        "返回值必须小于调用方传的超前游标：调用方据此判断「被夹过」，而不是本来就在末尾"
    );
    assert!(!page.truncated);

    // 夹住之后的下一次写入能被收到：不是「永久跳过」。
    draft(&mut core, "op-two");
    let page = core.events_page(max, 10).unwrap();
    assert_eq!(page.events.len(), 1, "新事件必须收得到");
    assert_eq!(page.events[0].sequence, max + 1);
    assert_eq!(page.next_sequence, max + 1);
}

/// 积压有上限、写入时按窗口清理；早于保留窗口的游标 `cursor_expired`。
///
/// 用直接 SQL 一次灌满窗口（一万次写事务太慢），再让**一条真实写入**触发保留策略：
/// 这正好验证「删在写入的同一个事务里」，而不是某个后台清理恰好跑过。
#[test]
fn backlog_is_capped_and_a_pruned_cursor_expires() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.sqlite");
    let mut core = new_core(dir.path());
    draft(&mut core, "op-seed");
    let seed_max = core.last_event_sequence().unwrap();
    assert_eq!(seed_max, 1);

    // 灌满窗口：sequence 从 2 开始，共 EVENT_BACKLOG_LIMIT 条合法事件。
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        let tx = conn.unchecked_transaction().unwrap();
        for index in 0..EVENT_BACKLOG_LIMIT {
            let sequence = seed_max + 1 + index;
            tx.execute(
                "INSERT INTO domain_events (sequence, event_id, type, entity_id, revision, emitted_at) \
                 VALUES (?1, ?2, 'capture.changed', 'cap_fake', 1, '2026-10-18T04:00:00Z')",
                params![sequence, format!("evt_fake_{sequence}")],
            )
            .unwrap();
        }
        tx.commit().unwrap();
    }
    let filled = core.last_event_sequence().unwrap();
    assert_eq!(filled, seed_max + EVENT_BACKLOG_LIMIT);

    // 一条真实写入：插入新事件，并在同一个事务里把窗口外的旧事件清掉。
    draft(&mut core, "op-prune");
    let max = core.last_event_sequence().unwrap();
    assert_eq!(max, filled + 1);

    let count: i64 = {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.query_row("SELECT COUNT(*) FROM domain_events", [], |row| row.get(0))
            .unwrap()
    };
    assert_eq!(
        count, EVENT_BACKLOG_LIMIT,
        "事件表只保留最近 {EVENT_BACKLOG_LIMIT} 条"
    );
    let oldest = max - EVENT_BACKLOG_LIMIT + 1;

    // 正好接上窗口的第一条：不算缺口，能读到（且解析得出来——灌进去的是合法事件）。
    let edge = core.events_page(oldest - 1, 3).unwrap();
    assert_eq!(edge.events.len(), 3);
    assert_eq!(edge.events[0].sequence, oldest);

    // 早于窗口：中间那段已经被清掉，必须如实报 cursor_expired，不能假装完整。
    let error = core.events_page(0, 10).unwrap_err();
    assert_eq!(error.code(), ErrorCode::CursorExpired);
    let reason = error.to_string();
    assert!(
        reason.contains("保留窗口") && reason.contains(&oldest.to_string()),
        "错误要说清保留窗口在哪：{reason}"
    );

    // 超前游标仍然夹住（与保留策略互不影响）。
    let ahead = core.events_page(max + 1_000, 10).unwrap();
    assert_eq!(ahead.next_sequence, max);
    assert!(ahead.events.is_empty());
}
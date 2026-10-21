//! 回收站的 schema v16：`captures.trashed_from_state` 是纯加性迁移。
//!
//! 三条证据（与 v12 / v15 的迁移测试同一口径）：
//! 1. 空库直接建到 16，新列存在且默认 NULL；
//! 2. 真的 v15 库（有 v15 的全部结构、只是没有这一列）升到 16 后**数据不丢**、
//!    新列可写；NULL 在恢复时按 `committed` 兜底；
//! 3. 重复 open 幂等：不重复 ALTER、不重复写 `schema_migrations`。

use std::path::Path;

use diary_core::{CaptureState, Core, CreateDraftInput, ErrorCode, SCHEMA_VERSION};
use rusqlite::{params, Connection};

fn new_core(dir: &Path) -> Core {
    Core::open(dir.join("library.sqlite")).expect("打开资料库")
}

fn column_count(path: &Path, table: &str, column: &str) -> i64 {
    let conn = Connection::open(path).expect("外部连接");
    let mut statement = conn
        .prepare(&format!("PRAGMA table_info({table})"))
        .unwrap();
    let names: Vec<String> = statement
        .query_map([], |row| row.get::<_, String>(1))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    names.iter().filter(|name| *name == column).count() as i64
}

fn committed_capture(core: &mut Core, text: &str) -> String {
    let id = core
        .create_draft(CreateDraftInput {
            occurred_at: None,
            time_zone: "Asia/Shanghai",
            utc_offset_minutes: 480,
            operation_id: "op-create",
        })
        .unwrap()
        .id;
    let revision = core.save_draft(&id, text, 1, "op-save").unwrap().revision;
    core.commit(&id, revision, "op-commit").unwrap();
    id
}

#[test]
fn empty_library_lands_on_v16_with_the_new_column() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.sqlite");
    let core = new_core(dir.path());
    assert_eq!(core.schema_version().unwrap(), SCHEMA_VERSION);
    assert_eq!(SCHEMA_VERSION, 16, "v16 = 回收站的 trashed_from_state");
    assert_eq!(column_count(&path, "captures", "trashed_from_state"), 1);
    drop(core);
}

#[test]
fn a_v15_library_migrates_to_v16_without_losing_data_and_null_falls_back_to_committed() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.sqlite");
    let capture_id = {
        let mut core = new_core(dir.path());
        let id = committed_capture(&mut core, "升级前的材料。");
        assert_eq!(
            core.get_capture(&id).unwrap().state,
            CaptureState::Committed
        );
        id
    };

    let counts_before = {
        let conn = Connection::open(&path).unwrap();
        [
            "captures",
            "source_items",
            "source_revisions",
            "search_capture_docs",
            "domain_events",
        ]
        .iter()
        .map(|table| {
            conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap()
        })
        .collect::<Vec<_>>()
    };

    // 退回**真正的 v15 样子**：没有 v16 的列，版本号与迁移记录也退回去。
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "ALTER TABLE captures DROP COLUMN trashed_from_state;
             UPDATE schema_migrations SET version = 15;
             PRAGMA user_version = 15;",
        )
        .unwrap();
        assert_eq!(
            conn.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            15
        );
        assert_eq!(column_count(&path, "captures", "trashed_from_state"), 0);
    }

    let mut core = new_core(dir.path());
    assert_eq!(core.schema_version().unwrap(), SCHEMA_VERSION);
    assert_eq!(column_count(&path, "captures", "trashed_from_state"), 1);
    // 旧数据一行不丢。
    let capture = core.get_capture(&capture_id).unwrap();
    assert_eq!(capture.draft_text, "升级前的材料。");
    assert_eq!(capture.state, CaptureState::Committed);
    let counts_after = {
        let conn = Connection::open(&path).unwrap();
        [
            "captures",
            "source_items",
            "source_revisions",
            "search_capture_docs",
            "domain_events",
        ]
        .iter()
        .map(|table| {
            conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap()
        })
        .collect::<Vec<_>>()
    };
    assert_eq!(counts_before, counts_after, "迁移不动旧数据");
    // 新列默认 NULL。
    let stored: Option<String> = Connection::open(&path)
        .unwrap()
        .query_row(
            "SELECT trashed_from_state FROM captures WHERE id = ?1",
            params![capture_id],
            |row| row.get(0),
        )
        .unwrap();
    assert!(stored.is_none());

    // 新列可写：trash 记住原状态，restore 回到 committed。
    let trashed = core
        .trash_capture(&capture_id, capture.revision, "op-trash")
        .unwrap();
    let stored: Option<String> = Connection::open(&path)
        .unwrap()
        .query_row(
            "SELECT trashed_from_state FROM captures WHERE id = ?1",
            params![capture_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(stored.as_deref(), Some("committed"));
    let restored = core
        .restore_capture(&capture_id, trashed.revision, "op-restore")
        .unwrap();
    assert_eq!(restored.state, CaptureState::Committed);

    // NULL 兜底：模拟「升级前就已经在回收站、但不知道原状态」的行。
    let fallback_id = core
        .create_draft(CreateDraftInput {
            occurred_at: None,
            time_zone: "Asia/Shanghai",
            utc_offset_minutes: 480,
            operation_id: "op-fallback",
        })
        .unwrap()
        .id;
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute(
            "UPDATE captures SET state = 'trashed', trashed_from_state = NULL WHERE id = ?1",
            params![fallback_id],
        )
        .unwrap();
    }
    let current = core.get_capture(&fallback_id).unwrap();
    assert_eq!(current.state, CaptureState::Trashed);
    let restored = core
        .restore_capture(&fallback_id, current.revision, "op-fallback-restore")
        .unwrap();
    assert_eq!(
        restored.state,
        CaptureState::Committed,
        "无法得知原状态时的保守选择是 committed"
    );
    // 再恢复一次已经是 committed：状态机拒绝。
    let error = core
        .restore_capture(&fallback_id, restored.revision, "op-fallback-restore-again")
        .unwrap_err();
    assert_eq!(error.code(), ErrorCode::InvalidState);
}

#[test]
fn reopening_a_v16_library_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.sqlite");
    for _ in 0..3 {
        let core = new_core(dir.path());
        assert_eq!(core.schema_version().unwrap(), SCHEMA_VERSION);
    }
    assert_eq!(column_count(&path, "captures", "trashed_from_state"), 1);
    let migrations: i64 = Connection::open(&path)
        .unwrap()
        .query_row("SELECT COUNT(*) FROM schema_migrations", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(migrations, 1, "重复 open 不重复写迁移记录");
}

#[test]
fn a_library_that_claims_v16_but_lacks_the_column_is_repaired() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.sqlite");
    let capture_id = {
        let mut core = new_core(dir.path());
        committed_capture(&mut core, "自称 16 但缺列的材料。")
    };

    // 制造「版本号已经 16、列却没落」的坏状态：v16 守卫若写成
    // `current < 16 && !column_exists(...)`，这个库会被永远跳过、版本停在 16。
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "ALTER TABLE captures DROP COLUMN trashed_from_state;
             PRAGMA user_version = 16;",
        )
        .unwrap();
        assert_eq!(column_count(&path, "captures", "trashed_from_state"), 0);
        assert_eq!(
            conn.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            16
        );
    }

    // 纯结构判定：列不在就补，版本号不挡路。
    let core = new_core(dir.path());
    assert_eq!(core.schema_version().unwrap(), SCHEMA_VERSION);
    assert_eq!(
        column_count(&path, "captures", "trashed_from_state"),
        1,
        "自称 16 但缺列的库必须在 open 时补回这一列"
    );
    let capture = core.get_capture(&capture_id).unwrap();
    assert_eq!(capture.draft_text, "自称 16 但缺列的材料。");
    assert_eq!(capture.state, CaptureState::Committed);

    // 修复路径也要幂等：再开一次不重复 ALTER、不重复写迁移记录。
    drop(core);
    let core = new_core(dir.path());
    assert_eq!(core.schema_version().unwrap(), SCHEMA_VERSION);
    assert_eq!(column_count(&path, "captures", "trashed_from_state"), 1);
    let migrations: i64 = Connection::open(&path)
        .unwrap()
        .query_row("SELECT COUNT(*) FROM schema_migrations", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(migrations, 1, "修复路径不能重复写迁移记录");
}

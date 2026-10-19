//! schema v17（`styles` 表）的三条迁移证据：
//!
//! 1. 空库直接到 17，表在、默认风格种子在；
//! 2. 一个真的 v16 库（带回收站线的 `captures.trashed_from_state` 列）升到 17，
//!    旧数据与 v16 列原样保留；
//! 3. 重复 open 幂等：不重复种默认行、不重置用户已经改过的风格。

use diary_core::{Core, CreateDraftInput, StyleProfile, SCHEMA_VERSION};
use rusqlite::Connection;

fn draft(core: &mut Core, text: &str) -> String {
    let capture = core
        .create_draft(CreateDraftInput {
            occurred_at: Some(chrono::Utc::now()),
            time_zone: "Asia/Shanghai",
            utc_offset_minutes: 480,
            operation_id: "op-v17-create",
        })
        .unwrap();
    core.save_draft(&capture.id, text, capture.revision, "op-v17-save")
        .unwrap();
    core.commit(&capture.id, capture.revision + 1, "op-v17-commit")
        .unwrap();
    capture.id
}

fn table_exists(conn: &Connection, name: &str) -> bool {
    conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
        rusqlite::params![name],
        |row| row.get::<_, i64>(0),
    )
    .unwrap()
        > 0
}

fn column_exists(conn: &Connection, table: &str, column: &str) -> bool {
    let mut statement = conn
        .prepare(&format!("PRAGMA table_info({table})"))
        .unwrap();
    let rows = statement
        .query_map([], |row| row.get::<_, String>(1))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    rows.iter().any(|name| name == column)
}

#[test]
fn empty_library_migrates_to_v17_with_the_styles_table_and_default_seed() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.sqlite");
    let core = Core::open(&path).unwrap();
    assert_eq!(SCHEMA_VERSION, 17);
    assert_eq!(core.schema_version().unwrap(), 17);

    let conn = Connection::open(&path).unwrap();
    assert!(table_exists(&conn, "styles"));
    let (count, snapshot_id, id, revision, label, current): (
        i64,
        String,
        String,
        i64,
        String,
        i64,
    ) = conn
        .query_row(
            "SELECT COUNT(*), MIN(snapshot_id), MIN(id), MIN(revision), MIN(label), \
             MIN(current) FROM styles",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(count, 1);
    assert_eq!(snapshot_id, "sty_default");
    assert_eq!(id, "sty_default");
    assert_eq!(revision, 1);
    assert_eq!(label, "默认风格");
    assert_eq!(current, 1);

    let recorded: i64 = conn
        .query_row("SELECT MAX(version) FROM schema_migrations", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(recorded, 17);
}

#[test]
fn a_real_v16_library_with_the_trash_column_migrates_to_17_and_keeps_its_data() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.sqlite");

    // 先用当前构建建一个库、写一条数据，再把它「退回」成回收站线写出的 v16 形状：
    // v16 有 `captures.trashed_from_state`，没有 `styles` 表。
    let capture_id = {
        let mut core = Core::open(&path).unwrap();
        draft(&mut core, "v16 时期就存在的记录。")
    };
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute(
            "ALTER TABLE captures ADD COLUMN trashed_from_state TEXT",
            [],
        )
        .unwrap();
        conn.execute("UPDATE captures SET trashed_from_state = 'restorable'", [])
            .unwrap();
        conn.execute("DROP TABLE styles", []).unwrap();
        conn.pragma_update(None, "user_version", 16).unwrap();
        conn.execute("DELETE FROM schema_migrations WHERE version = 17", [])
            .unwrap();
        assert!(!table_exists(&conn, "styles"));
        assert!(column_exists(&conn, "captures", "trashed_from_state"));
    }

    // 打开即升级：不认识的 v16 列原样保留，v17 的 styles 建出来并种默认行。
    let core = Core::open(&path).unwrap();
    assert_eq!(core.schema_version().unwrap(), 17);
    assert_eq!(core.count_captures().unwrap(), 1);
    assert_eq!(
        core.get_capture(&capture_id).unwrap().draft_text,
        "v16 时期就存在的记录。"
    );
    assert_eq!(core.list_styles().unwrap().len(), 1);
    assert_eq!(core.get_style("sty_default").unwrap().revision, 1);

    let conn = Connection::open(&path).unwrap();
    assert!(table_exists(&conn, "styles"));
    assert!(column_exists(&conn, "captures", "trashed_from_state"));
    let trashed_from: String = conn
        .query_row(
            "SELECT trashed_from_state FROM captures WHERE id = ?1",
            rusqlite::params![capture_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(trashed_from, "restorable", "v16 列的值不能被迁移抹掉");
}

#[test]
fn reopening_the_same_v17_library_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.sqlite");

    let (style_id, revision) = {
        let mut core = Core::open(&path).unwrap();
        let mut profile = StyleProfile {
            id: "sty_mine".to_owned(),
            label: "我的风格".to_owned(),
            revision: 0,
            updated_at: chrono::Utc::now(),
            length_preference: Some("简短".to_owned()),
            person: None,
            tone: Some("平静".to_owned()),
            emotional_temperature: None,
            rhetoric_level: None,
            examples: Vec::new(),
            banned_expressions: Vec::new(),
        };
        let saved = core.save_style(&profile, None).unwrap();
        profile = saved;
        (profile.id, profile.revision)
    };
    assert_eq!(revision, 1);

    // 连续打开两次：迁移不再跑、默认行不重复、用户风格不被重置。
    for _ in 0..2 {
        let core = Core::open(&path).unwrap();
        assert_eq!(core.schema_version().unwrap(), 17);
        let styles = core.list_styles().unwrap();
        assert_eq!(styles.len(), 2, "默认风格 + 用户风格");
        let mine = styles
            .iter()
            .find(|profile| profile.id == style_id)
            .unwrap();
        assert_eq!(mine.revision, 1);
        assert_eq!(mine.tone.as_deref(), Some("平静"));
    }

    let conn = Connection::open(&path).unwrap();
    let default_current: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM styles WHERE id = 'sty_default' AND current = 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(default_current, 1, "默认行不能重复种");
    let migration_rows: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM schema_migrations WHERE version = 17",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(migration_rows, 1);
}

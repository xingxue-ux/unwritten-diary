//! schema v17（`styles` 表）的迁移证据：
//!
//! 1. 空库直接到 17，styles 表、默认风格种子与 v16 的回收站列都在；
//! 2. **v15 → 17**：合成 v15 库（没有 `trashed_from_state`、没有 `styles`）升级后
//!    **两样都要有**。这是「只写 v17、跳过 v16」那个数据损坏回归测试；
//! 3. v16 → 17：回收站列与数据原样保留，只加 styles；
//! 4. v17 但缺列/缺表：打开时结构兜底补回；
//! 5. 重复 open 幂等：不重复种默认行、不重置用户已经改过的风格。

use std::path::Path;

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

fn index_exists(conn: &Connection, name: &str) -> bool {
    conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' AND name = ?1",
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
    // v16 的回收站加列必须在：空库也要走 v16 的迁移段，不能因为最终版本是 17 就跳过。
    assert!(column_exists(&conn, "captures", "trashed_from_state"));
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
        assert!(
            column_exists(&conn, "captures", "trashed_from_state"),
            "空库迁移已经建出 v16 的列"
        );
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
fn a_v15_library_migrates_to_17_with_both_the_trash_column_and_styles() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.sqlite");

    // 当前构建建库会同时跑 v16 与 v17；先写数据，再把它退回**真的 v15 形状**：
    // 没有 `captures.trashed_from_state`，也没有 `styles` 表。
    let capture_id = {
        let mut core = Core::open(&path).unwrap();
        draft(&mut core, "v15 时期就存在的记录。")
    };
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "ALTER TABLE captures DROP COLUMN trashed_from_state;
             DROP TABLE styles;
             UPDATE schema_migrations SET version = 15;
             PRAGMA user_version = 15;",
        )
        .unwrap();
        assert!(!column_exists(&conn, "captures", "trashed_from_state"));
        assert!(!table_exists(&conn, "styles"));
    }

    // v15 直升 17：两段 DDL 都必须跑。只跑 v17 会让回收站列永远补不上。
    let core = Core::open(&path).unwrap();
    assert_eq!(core.schema_version().unwrap(), 17);
    assert_eq!(core.count_captures().unwrap(), 1);
    assert_eq!(
        core.get_capture(&capture_id).unwrap().draft_text,
        "v15 时期就存在的记录。"
    );
    assert_eq!(core.list_styles().unwrap().len(), 1);
    assert_eq!(core.get_style("sty_default").unwrap().revision, 1);

    let conn = Connection::open(&path).unwrap();
    assert!(
        column_exists(&conn, "captures", "trashed_from_state"),
        "v15 直升 17 必须补上 v16 的回收站列"
    );
    assert!(table_exists(&conn, "styles"), "v15 直升 17 必须建出 styles");
    let old_value: Option<String> = conn
        .query_row(
            "SELECT trashed_from_state FROM captures WHERE id = ?1",
            rusqlite::params![capture_id],
            |row| row.get(0),
        )
        .unwrap();
    assert!(old_value.is_none(), "老数据的新列是 NULL，不是编出来的状态");
}

#[test]
fn a_v17_library_missing_the_v16_column_or_styles_table_is_repaired_on_open() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.sqlite");
    let capture_id = {
        let mut core = Core::open(&path).unwrap();
        draft(&mut core, "中间构建写下的记录。")
    };
    {
        let conn = Connection::open(&path).unwrap();
        // 模拟「只写过 v17、跳过 v16」的中间构建：版本号还是 17，结构与 17 不符。
        conn.execute_batch(
            "ALTER TABLE captures DROP COLUMN trashed_from_state;
             DROP TABLE styles;",
        )
        .unwrap();
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, 17, "前提：版本号已经是最新");
        assert!(!column_exists(&conn, "captures", "trashed_from_state"));
        assert!(!table_exists(&conn, "styles"));
    }

    // 版本号已是最新也要做结构兜底，否则这个库永远缺列。
    let core = Core::open(&path).unwrap();
    assert_eq!(core.schema_version().unwrap(), 17);
    assert_eq!(core.count_captures().unwrap(), 1);
    assert_eq!(
        core.get_capture(&capture_id).unwrap().draft_text,
        "中间构建写下的记录。"
    );
    let conn = Connection::open(&path).unwrap();
    assert!(column_exists(&conn, "captures", "trashed_from_state"));
    assert!(table_exists(&conn, "styles"));
    assert_eq!(core.get_style("sty_default").unwrap().revision, 1);
}

#[test]
fn a_library_that_claims_v16_but_lacks_the_trash_column_converges_on_the_first_open() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.sqlite");
    let capture_id = {
        let mut core = Core::open(&path).unwrap();
        draft(&mut core, "自称 16 但缺列的库里的记录。")
    };
    {
        let conn = Connection::open(&path).unwrap();
        // 复现复审指出的输入：`user_version = 16`，但 v16 的列不在。
        // 旧守卫是 `current < 16 && 列不在`，这次 open 会写 17 并成功返回，
        // 列却仍然缺，要等下一次 open。
        conn.execute_batch(
            "ALTER TABLE captures DROP COLUMN trashed_from_state;
             PRAGMA user_version = 16;
             DELETE FROM schema_migrations WHERE version = 17;",
        )
        .unwrap();
        assert!(!column_exists(&conn, "captures", "trashed_from_state"));
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, 16);
    }

    let core = Core::open(&path).unwrap();
    assert_eq!(core.schema_version().unwrap(), 17);
    assert_eq!(
        core.get_capture(&capture_id).unwrap().draft_text,
        "自称 16 但缺列的库里的记录。"
    );

    let conn = Connection::open(&path).unwrap();
    assert!(
        column_exists(&conn, "captures", "trashed_from_state"),
        "自称 16 且缺列的库必须在**这一次** open 就补上，不能等下一次"
    );
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, 17);
}

#[test]
fn a_v17_library_missing_the_styles_current_index_gets_it_back_on_open() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.sqlite");
    {
        let mut core = Core::open(&path).unwrap();
        let profile = StyleProfile {
            id: "sty_index".to_owned(),
            label: "索引守卫".to_owned(),
            revision: 0,
            updated_at: chrono::Utc::now(),
            length_preference: None,
            person: None,
            tone: Some("平静".to_owned()),
            emotional_temperature: None,
            rhetoric_level: None,
            examples: Vec::new(),
            banned_expressions: Vec::new(),
        };
        core.save_style(&profile, None).unwrap();
    }
    {
        let conn = Connection::open(&path).unwrap();
        // `user_version` 还是 17：版本号不能证明索引在（与 #133 的 v15 覆盖索引同一理由）。
        conn.execute_batch(
            "DROP INDEX idx_styles_current;",
        )
        .unwrap();
        assert!(!index_exists(&conn, "idx_styles_current"));
        // 索引不在时「一条风格只有一个当前版本」不再由库保证：手工插第二行当前版本
        // 能成功。这条断言是反证，正好说明补索引不是可有可无的；插入后立刻删掉，
        // 避免把本身已违反唯一约束的数据留给「补索引」那一步。
        conn.execute(
            "INSERT INTO styles (snapshot_id, id, revision, label, updated_at, current) VALUES \
             ('stv_orphan', 'sty_index', 99, '绕过索引的当前行', '2026-10-20T00:00:00Z', 1)",
            [],
        )
        .unwrap();
        conn.execute("DELETE FROM styles WHERE snapshot_id = 'stv_orphan'", [])
            .unwrap();
    }

    let core = Core::open(&path).unwrap();
    assert_eq!(core.schema_version().unwrap(), 17);
    let conn = Connection::open(&path).unwrap();
    assert!(
        index_exists(&conn, "idx_styles_current"),
        "结构兜底必须补回 idx_styles_current，否则当前版本唯一性不再由库保证"
    );

    // 补回后部分唯一索引重新生效：同一条风格再插一个 current = 1 必须被拒。
    let duplicate = conn.execute(
        "INSERT INTO styles (snapshot_id, id, revision, label, updated_at, current)          VALUES ('stv_duplicate', 'sty_default', 99, '第二个当前行', '2026-10-20T00:00:00Z', 1)",
        [],
    );
    assert!(duplicate.is_err(), "idx_styles_current 补回后必须重新约束唯一性");
}

#[test]
fn a_real_v15_library_fixture_with_an_uncheckpointed_wal_migrates_to_v17() {
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/v15-library");
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.sqlite");
    // 必须连 `-wal` 一起拷：样本的数据还没检查点回主库，只拷 4 KiB 的
    // `library.sqlite` 会得到一个空库，那样测的就不是真旧库。
    std::fs::copy(source.join("library.sqlite"), &path).unwrap();
    std::fs::copy(
        source.join("library.sqlite-wal"),
        dir.path().join("library.sqlite-wal"),
    )
    .unwrap();

    // 迁移前先证明这是一份真 v15 库：版本 15、两条记录在 WAL 里可读。
    {
        let conn = Connection::open(&path).unwrap();
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, 15);
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM captures", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 2);
        let history: Vec<i64> = {
            let mut statement = conn
                .prepare("SELECT version FROM schema_migrations ORDER BY version")
                .unwrap();
            statement
                .query_map([], |row| row.get(0))
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap()
        };
        assert_eq!(history, vec![15], "旧库的历史行是 v15，不是当前构建补出来的");
    }

    // 当前构建打开：v15 → 17 两段（回收站列 + styles）都必须跑。
    let core = Core::open(&path).unwrap();
    assert_eq!(core.schema_version().unwrap(), 17);
    assert_eq!(core.count_captures().unwrap(), 2);
    assert_eq!(core.get_style("sty_default").unwrap().revision, 1);

    let conn = Connection::open(&path).unwrap();
    assert!(column_exists(&conn, "captures", "trashed_from_state"));
    assert!(table_exists(&conn, "styles"));
    assert!(index_exists(&conn, "idx_styles_current"));
    let texts: Vec<String> = {
        let mut statement = conn
            .prepare("SELECT draft_text FROM captures ORDER BY day_key")
            .unwrap();
        statement
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
    };
    assert_eq!(texts.len(), 2);
    assert!(texts[0].contains("样本一"));
    assert!(texts[1].contains("样本二"));
    assert!(texts[1].contains('🙂'), "旧数据里的 emoji 必须原样保留");
    let history: Vec<i64> = {
        let mut statement = conn
            .prepare("SELECT version FROM schema_migrations ORDER BY version")
            .unwrap();
        statement
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
    };
    assert_eq!(history, vec![15, 17], "v15 的历史行要保留，17 是这次落到的版本");
    let quick: String = conn
        .query_row("PRAGMA quick_check", [], |row| row.get(0))
        .unwrap();
    assert_eq!(quick, "ok");
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

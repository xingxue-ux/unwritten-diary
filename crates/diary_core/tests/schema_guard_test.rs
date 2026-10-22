//! 迁移链的**结构守卫**回归：`user_version` 只是提示，不是结构证明。
//!
//! 覆盖两类真实会遇到的形状（背景见 `schema.rs` 模块文档与
//! `docs/development/升级与迁移.md`）：
//!
//! 1. **版本号落后、结构已经在**（外部改过 `user_version`、文件级撕裂复制）。v11 之后
//!    每一段的 DDL 都必须能对着「列/表已经在」的库重复执行，否则一次 `ALTER TABLE`
//!    报重复列就把库**永久锁死**（版本号没写成 18，下次 open 还走同一条路）。
//! 2. **版本号已经是 18、结构被删**：`diary_days.revision` 与 `settings` 的列必须由
//!    每次 open 的结构兜底补回来，否则 `diary.getDay` / `settings.get` 永久失败。

use std::path::{Path, PathBuf};

use diary_core::{Core, CreateDraftInput, DEFAULT_TEMPLATE_ID, SCHEMA_VERSION};
use rusqlite::{params, Connection};

const DAY: &str = "2026-10-16";
const CAPTURE_TEXT: &str = "守卫样本一 🙂";

fn library_path(dir: &Path) -> PathBuf {
    dir.join("library.sqlite")
}

fn open(dir: &Path) -> Core {
    Core::open(library_path(dir)).unwrap()
}

fn db(dir: &Path) -> Connection {
    Connection::open(library_path(dir)).unwrap()
}

fn user_version(conn: &Connection) -> i64 {
    conn.query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap()
}

fn table_exists(conn: &Connection, name: &str) -> bool {
    conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
        params![name],
        |row| row.get::<_, i64>(0),
    )
    .unwrap()
        > 0
}

fn column_exists(conn: &Connection, table: &str, column: &str) -> bool {
    let mut statement = conn
        .prepare(&format!("PRAGMA table_info({table})"))
        .unwrap();
    let names = statement
        .query_map([], |row| row.get::<_, String>(1))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    names.iter().any(|name| name == column)
}

/// 造一个**真实 v18 库**：一条提交过的记录（含 emoji）、一个版本行、一行
/// `diary_days`（`revision = 7`，比版本数大，用来识别「迁移又回填了一遍」）、
/// 一份改过的设置（开关打开、计划时间 06:45、`revision = 2`）。
fn seed_v18_library(dir: &Path) {
    let mut core = open(dir);

    let capture = core
        .create_draft(CreateDraftInput {
            occurred_at: Some(
                chrono::DateTime::parse_from_rfc3339("2026-10-16T02:00:00Z")
                    .unwrap()
                    .with_timezone(&chrono::Utc),
            ),
            time_zone: "Asia/Shanghai",
            utc_offset_minutes: 480,
            operation_id: "op-guards-create",
        })
        .unwrap();
    assert_eq!(capture.day_key, DAY, "前提：记录落在测试用的这一天");
    core.save_draft(
        &capture.id,
        CAPTURE_TEXT,
        capture.revision,
        "op-guards-save",
    )
    .unwrap();
    core.commit(&capture.id, capture.revision + 1, "op-guards-commit")
        .unwrap();

    let mut settings = core.get_settings().unwrap();
    settings.diary_time_zone = "Asia/Shanghai".to_owned();
    settings.default_style_id = Some("sty_default".to_owned());
    settings.auto_organize_enabled = true;
    settings.auto_organize_time = "06:45".to_owned();
    let settings = core.update_settings(&settings, settings.revision).unwrap();
    assert_eq!(settings.revision, 2, "前提：设置被用户改过一次");
    drop(core);

    // `revision = 7` 不等于版本数（1）：任何重新回填都会把它改小，测试能看见。
    let conn = db(dir);
    conn.execute(
        "INSERT INTO diary_days (day_key, time_zone, current_version_id, updated_at, revision) \
         VALUES (?1, 'Asia/Shanghai', NULL, '2026-10-16T02:00:00Z', 7)",
        params![DAY],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO diary_versions (id, day_key, origin, body, provenance_links, created_at) \
         VALUES ('dvv_guards', ?1, 'user_edit', '守卫样本的版本正文', '[]', \
                 '2026-10-16T03:00:00Z')",
        params![DAY],
    )
    .unwrap();
    assert_eq!(user_version(&conn), SCHEMA_VERSION);
}

// ---------------------------------------------------------------- ① 版本号落后、结构已在

/// 用户报的那个死锁：v18 库被外部改成 `user_version = 11`（列都在），
/// 老守卫 `current < 12 || 列不在` 会再发一次 `ALTER TABLE diary_days ADD COLUMN
/// revision`，报 `duplicate column name: revision`；版本号没写成 18，**每次重开都失败**。
///
/// 修复后：v12 的守卫是纯结构判定（列在就整段跳过），open 成功、数据完好、
/// 版本号回到 18；`revision` 也不能被回填改小（那是假的冲突保护）。
#[test]
fn a_library_claiming_v11_with_the_v12_column_already_present_opens_instead_of_bricking() {
    let dir = tempfile::tempdir().unwrap();
    seed_v18_library(dir.path());

    {
        let conn = db(dir.path());
        assert!(
            column_exists(&conn, "diary_days", "revision"),
            "前提：v18 的库有 v12 的列"
        );
        // 只改版本号：这正是「外部改过版本号 / 文件级撕裂复制」留下的形状。
        conn.execute_batch("PRAGMA user_version = 11;").unwrap();
        assert_eq!(user_version(&conn), 11);
    }

    let core = open(dir.path());
    assert_eq!(core.schema_version().unwrap(), SCHEMA_VERSION);
    assert_eq!(core.count_captures().unwrap(), 1, "记录一条不能丢");

    // 关键：列已经在 ⇒ 不回填。`revision = 7` 若变成版本数 1，就是在飞的
    // `expectedRevision` 被升级悄悄改小的那个 bug。
    let day = core.get_diary_day(DAY).unwrap();
    assert_eq!(day.revision, 7, "列已经在就不能再跑 v12 的回填");
    assert_eq!(day.candidate_version_ids, vec!["dvv_guards".to_owned()]);

    let settings = core.get_settings().unwrap();
    assert!(settings.auto_organize_enabled, "用户开的开关不能被重开弄丢");
    assert_eq!(settings.auto_organize_time, "06:45");
    assert_eq!(settings.diary_time_zone, "Asia/Shanghai");
    assert_eq!(settings.revision, 2, "设置不能被重新种一遍");
    assert_eq!(
        settings.default_style_id.as_deref(),
        Some("sty_default"),
        "v18 独有的列不能被重跑 v11 弄丢"
    );

    let conn = db(dir.path());
    assert_eq!(user_version(&conn), 18, "版本号必须收敛回 18");
    let text: String = conn
        .query_row("SELECT draft_text FROM captures", [], |row| row.get(0))
        .unwrap();
    assert_eq!(text, CAPTURE_TEXT, "正文一个字符都不能丢（含 emoji）");
    assert!(
        !table_exists(&conn, "organize_settings"),
        "v11 段对着 settings 已在的库重跑出来的旧表必须被 v18 吸收掉，不能留第二份真值"
    );
    let settings_rows: i64 = conn
        .query_row("SELECT COUNT(*) FROM settings", [], |row| row.get(0))
        .unwrap();
    assert_eq!(settings_rows, 1, "settings 仍然只有一行");
    drop(conn);

    // 再开一次：第一次成功后版本号已是 18，第二次必须走「版本已最新」那条路。
    let reopened = open(dir.path());
    assert_eq!(reopened.get_diary_day(DAY).unwrap().revision, 7);
    assert_eq!(reopened.count_captures().unwrap(), 1);
}

/// 同一类形状的 v15 版本：库自称 15（v15 的列、v16/v17/v18 的结构按真实 v15 缺失），
/// open 必须把 16 / 17 / 18 三段补齐并把版本号写回 18，而不是在重复 `ALTER` 上失败。
#[test]
fn a_library_claiming_v15_with_its_own_column_present_migrates_the_rest_and_returns_to_18() {
    let dir = tempfile::tempdir().unwrap();
    seed_v18_library(dir.path());

    {
        let conn = db(dir.path());
        // 退回真实 v15 形状：settings 拆回 organize_settings、v16 列与 v17 表删掉；
        // 但 v15 自己的 `index_meta.auto_rebuild_marker` 与 v12 的 `revision` 列都在。
        conn.execute_batch(
            "ALTER TABLE settings RENAME TO organize_settings;
             ALTER TABLE organize_settings DROP COLUMN diary_time_zone;
             ALTER TABLE organize_settings DROP COLUMN default_style_id;
             ALTER TABLE captures DROP COLUMN trashed_from_state;
             DROP TABLE styles;
             PRAGMA user_version = 15;",
        )
        .unwrap();
        assert!(column_exists(&conn, "diary_days", "revision"));
        assert!(column_exists(&conn, "index_meta", "auto_rebuild_marker"));
        assert!(!table_exists(&conn, "styles"));
        assert!(!column_exists(&conn, "captures", "trashed_from_state"));
        assert_eq!(user_version(&conn), 15);
    }

    let core = open(dir.path());
    assert_eq!(core.schema_version().unwrap(), SCHEMA_VERSION);
    assert_eq!(core.count_captures().unwrap(), 1);
    assert_eq!(core.get_diary_day(DAY).unwrap().revision, 7);

    let settings = core.get_settings().unwrap();
    assert!(settings.auto_organize_enabled);
    assert_eq!(settings.auto_organize_time, "06:45");
    assert_eq!(settings.revision, 2, "v18 搬行要原样保留 revision");
    assert_eq!(settings.diary_time_zone, "UTC", "v15 没有设备时区可搬");

    let conn = db(dir.path());
    assert_eq!(user_version(&conn), 18);
    assert!(
        column_exists(&conn, "captures", "trashed_from_state"),
        "v16"
    );
    assert!(table_exists(&conn, "styles"), "v17");
    assert!(table_exists(&conn, "settings"), "v18");
    assert!(!table_exists(&conn, "organize_settings"));
    drop(conn);

    let reopened = open(dir.path());
    assert_eq!(reopened.get_diary_day(DAY).unwrap().revision, 7);
}

/// v15 的列守卫原来写的是 `current < 15 && 列不在`：版本号一旦写成 15，
/// 「自称 15 但缺这一列」的库就再也补不上（`current < 15` 是假）。
///
/// 纯结构判定之后，每次 open 只看列在不在——这条测试就是那条「&& 的另一半」。
#[test]
fn a_v15_library_missing_the_v15_column_gets_it_back_on_open() {
    let dir = tempfile::tempdir().unwrap();
    seed_v18_library(dir.path());

    {
        let conn = db(dir.path());
        assert!(column_exists(&conn, "index_meta", "auto_rebuild_marker"));
        conn.execute_batch(
            "ALTER TABLE index_meta DROP COLUMN auto_rebuild_marker;
             PRAGMA user_version = 15;",
        )
        .unwrap();
        assert!(!column_exists(&conn, "index_meta", "auto_rebuild_marker"));
    }

    let core = open(dir.path());
    assert_eq!(core.schema_version().unwrap(), SCHEMA_VERSION);

    let conn = db(dir.path());
    assert!(
        column_exists(&conn, "index_meta", "auto_rebuild_marker"),
        "v15 的列守卫必须是纯结构判定，否则 current 已是 15 时永远补不上"
    );
    assert_eq!(user_version(&conn), 18);
}

// ---------------------------------------------------------- ② 版本已是 18、结构被删掉

/// 删掉 `diary_days.revision`：open 不能「成功但永久坏掉」——补列之后
/// `diary.getDay` 必须能读，且回填只发生一次（第二次 open 不把已经推进的 revision 改小）。
#[test]
fn a_v18_library_missing_the_day_revision_column_heals_and_get_day_works() {
    let dir = tempfile::tempdir().unwrap();
    seed_v18_library(dir.path());

    {
        let conn = db(dir.path());
        conn.execute_batch("ALTER TABLE diary_days DROP COLUMN revision;")
            .unwrap();
        assert!(!column_exists(&conn, "diary_days", "revision"));
        assert_eq!(user_version(&conn), SCHEMA_VERSION, "版本号仍是 18");
    }

    let core = open(dir.path());
    assert_eq!(core.schema_version().unwrap(), SCHEMA_VERSION);
    let day = core.get_diary_day(DAY).unwrap();
    assert_eq!(day.revision, 1, "补列后按版本数回填（与 v12 迁移同一口径）");
    assert_eq!(day.candidate_version_ids, vec!["dvv_guards".to_owned()]);

    // 推进到 5 再重开：结构兜底只在列不在时动手，不能每次 open 都把 revision 重置。
    {
        let conn = db(dir.path());
        conn.execute(
            "UPDATE diary_days SET revision = 5 WHERE day_key = ?1",
            params![DAY],
        )
        .unwrap();
    }
    let reopened = open(dir.path());
    assert_eq!(
        reopened.get_diary_day(DAY).unwrap().revision,
        5,
        "补列之后的重开不能重新回填"
    );
    assert_eq!(reopened.count_captures().unwrap(), 1);
}

/// 删掉 `settings` 的列（含 v18 独有的 `diary_time_zone` / `default_style_id` 与
/// 一个 NOT NULL 无默认值的整理列）：open 不能在 `migrate` 的提前返回里跳过修复。
/// 修完 `settings.get` 必须能读，**已有行与其它列的值一个不丢**（不许 DROP 表重建）。
#[test]
fn a_v18_library_missing_settings_columns_heals_without_dropping_the_row() {
    let dir = tempfile::tempdir().unwrap();
    seed_v18_library(dir.path());

    {
        let conn = db(dir.path());
        conn.execute_batch(
            "ALTER TABLE settings DROP COLUMN diary_time_zone;
             ALTER TABLE settings DROP COLUMN default_style_id;
             ALTER TABLE settings DROP COLUMN template_id;",
        )
        .unwrap();
        assert!(!column_exists(&conn, "settings", "diary_time_zone"));
        assert!(!column_exists(&conn, "settings", "default_style_id"));
        assert!(!column_exists(&conn, "settings", "template_id"));
        assert_eq!(user_version(&conn), SCHEMA_VERSION);
    }

    let core = open(dir.path());
    assert_eq!(core.schema_version().unwrap(), SCHEMA_VERSION);
    let settings = core.get_settings().unwrap();
    // 三列按 V18 的默认补回来；`diary_time_zone` 与 `default_style_id` 丢的是数据本身，
    // 只能回到表默认（核心没有依据替用户猜）。
    assert_eq!(settings.diary_time_zone, "UTC");
    assert_eq!(settings.default_style_id, None);
    assert_eq!(
        core.organize_settings().unwrap().template_id,
        DEFAULT_TEMPLATE_ID,
        "补列的默认值与 seed_default_settings 是同一组常量"
    );
    // 没丢的列必须原样在：证明修的是列，不是把表 DROP 了重建。
    assert!(settings.auto_organize_enabled);
    assert_eq!(settings.auto_organize_time, "06:45");
    assert_eq!(settings.revision, 2);

    let conn = db(dir.path());
    let rows: i64 = conn
        .query_row("SELECT COUNT(*) FROM settings", [], |row| row.get(0))
        .unwrap();
    assert_eq!(rows, 1, "settings 仍然只有原来那一行");
    assert!(column_exists(&conn, "settings", "diary_time_zone"));
    assert!(column_exists(&conn, "settings", "default_style_id"));
    assert!(column_exists(&conn, "settings", "template_id"));
    drop(conn);

    // 第二次 open 幂等：不会再把 `updated_at` / `revision` 之类的值刷新一遍。
    let reopened = open(dir.path());
    assert_eq!(reopened.get_settings().unwrap(), settings);
}

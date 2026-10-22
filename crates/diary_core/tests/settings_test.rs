//! 完整设置对象（契约 4.4 `settings.get / update`，#108）与整理设置共用同一行。
//!
//! 覆盖：
//! - 新库的默认值（逐字段）与「整理视图 == 完整对象」的同一性；
//! - 往返（update 后 get 一致、revision +1）；
//! - 乐观锁：错误 `expectedRevision` ⇒ `revision_conflict` 且值不变；
//! - **两个入口同一份真值**：`update_organize_settings` 改开关后 `get_settings` 同步，
//!   反向也一样；`settings.update` 不覆盖模板/上限，`update_organize_settings` 不碰
//!   `diary_time_zone` / `default_style_id`；
//! - 迁移：v15 → 18（连同 v16 回收站列、v17 styles 表一起补上）、v17 → 18、
//!   以及「版本号已是 18 但结构缺失」的自愈。

use std::path::Path;

use diary_core::{Core, ErrorCode, SCHEMA_VERSION};
use rusqlite::Connection;

fn open(dir: &Path) -> Core {
    Core::open(dir.join("library.sqlite")).unwrap()
}

fn db(dir: &Path) -> Connection {
    Connection::open(dir.join("library.sqlite")).unwrap()
}

fn table_exists(conn: &Connection, name: &str) -> bool {
    conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
        [name],
        |row| row.get::<_, i64>(0),
    )
    .unwrap()
        > 0
}

fn column_exists(conn: &Connection, table: &str, column: &str) -> bool {
    conn.query_row(
        &format!("SELECT COUNT(*) FROM pragma_table_info('{table}') WHERE name = ?1"),
        [column],
        |row| row.get::<_, i64>(0),
    )
    .unwrap()
        > 0
}

fn index_exists(conn: &Connection, name: &str) -> bool {
    conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' AND name = ?1",
        [name],
        |row| row.get::<_, i64>(0),
    )
    .unwrap()
        > 0
}

// ------------------------------------------------------------------ 默认值与同一性

#[test]
fn a_new_library_returns_the_contract_defaults_field_by_field() {
    let dir = tempfile::tempdir().unwrap();
    let core = open(dir.path());
    let settings = core.get_settings().unwrap();

    assert_eq!(
        settings.diary_time_zone, "UTC",
        "核心不知道设备时区，首次是 UTC（见文档）"
    );
    assert_eq!(settings.revision, 1);
    assert!(!settings.auto_organize_enabled, "默认关是硬要求");
    assert_eq!(settings.auto_organize_time, "22:30");
    assert_eq!(settings.default_style_id, None);
    assert!(
        settings.updated_at <= chrono::Utc::now(),
        "updated_at 是迁移/首次读取时写下的真实时间"
    );

    // 同一行的两个视图：开关、计划时间、revision、updated_at 必须一致。
    let organize = core.organize_settings().unwrap();
    assert_eq!(organize.revision, settings.revision);
    assert_eq!(organize.updated_at, settings.updated_at);
    assert_eq!(organize.enabled, settings.auto_organize_enabled);
    assert_eq!(organize.auto_organize_time, settings.auto_organize_time);
    assert_eq!(organize.template_id, "merge_by_theme");
    assert_eq!(organize.max_calls_per_day, 30);
}

// ------------------------------------------------------------------ 往返与乐观锁

#[test]
fn settings_update_roundtrips_and_bumps_revision_by_exactly_one() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = open(dir.path());
    let before = core.get_settings().unwrap();

    let mut next = before.clone();
    next.diary_time_zone = "Asia/Shanghai".to_owned();
    next.auto_organize_enabled = true;
    next.auto_organize_time = "07:05".to_owned();
    next.default_style_id = Some("sty_default".to_owned());

    let updated = core.update_settings(&next, before.revision).unwrap();
    assert_eq!(updated.revision, before.revision + 1);
    assert!(updated.updated_at >= before.updated_at);
    assert_eq!(updated.diary_time_zone, "Asia/Shanghai");
    assert!(updated.auto_organize_enabled);
    assert_eq!(updated.auto_organize_time, "07:05", "HH:mm 要按规范形存");
    assert_eq!(updated.default_style_id.as_deref(), Some("sty_default"));
    assert_eq!(
        core.get_settings().unwrap(),
        updated,
        "update 后 get 必须一致"
    );
}

#[test]
fn a_stale_expected_revision_is_rejected_without_touching_a_byte() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = open(dir.path());
    let before = core.get_settings().unwrap();

    let mut next = before.clone();
    next.diary_time_zone = "Europe/Berlin".to_owned();
    next.auto_organize_enabled = true;
    next.auto_organize_time = "08:00".to_owned();

    let error = core
        .update_settings(&next, before.revision + 9)
        .unwrap_err();
    assert_eq!(error.code(), ErrorCode::RevisionConflict);
    assert_eq!(core.get_settings().unwrap(), before, "冲突必须零改动");
    assert_eq!(
        core.organize_settings().unwrap().revision,
        before.revision,
        "整理视图也不能被冲突写入推进"
    );
}

// ------------------------------------------------------------------ 同一份真值

#[test]
fn organize_and_full_settings_are_two_views_of_one_row() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = open(dir.path());

    // 先用整理入口改开关、计划时间与模板/上限。
    let mut organize = core.organize_settings().unwrap();
    organize.enabled = true;
    organize.auto_organize_time = "06:45".to_owned();
    organize.template_id = "proofread".to_owned();
    organize.max_calls_per_day = 7;
    let organize = core
        .update_organize_settings(&organize, organize.revision)
        .unwrap();

    // 再读完整对象：整理那一段必须同步。
    let full = core.get_settings().unwrap();
    assert!(
        full.auto_organize_enabled,
        "organize 改的开关必须出现在 settings.get"
    );
    assert_eq!(full.auto_organize_time, "06:45");
    assert_eq!(full.revision, organize.revision, "同一个 revision");

    // 反向：settings.update 改开关，整理视图同步。
    let mut next = full.clone();
    next.auto_organize_enabled = false;
    next.auto_organize_time = "21:40".to_owned();
    next.diary_time_zone = "Asia/Tokyo".to_owned();
    let full = core.update_settings(&next, full.revision).unwrap();
    let organize = core.organize_settings().unwrap();
    assert!(
        !organize.enabled,
        "settings.update 改的开关必须出现在 organizeSettings"
    );
    assert_eq!(organize.auto_organize_time, "21:40");
    assert_eq!(organize.revision, full.revision);

    // settings.update 不碰模板与上限；update_organize_settings 也不碰完整对象独有字段。
    assert_eq!(
        organize.template_id, "proofread",
        "settings.update 不许覆盖模板"
    );
    assert_eq!(
        organize.max_calls_per_day, 7,
        "settings.update 不许覆盖上限"
    );
    assert_eq!(
        core.get_settings().unwrap().diary_time_zone,
        "Asia/Tokyo",
        "update_organize_settings 不许覆盖 diary_time_zone"
    );

    // 存储层证据：只有一张 settings 表、一行；旧表不存在。
    let conn = db(dir.path());
    let rows: i64 = conn
        .query_row("SELECT COUNT(*) FROM settings", [], |row| row.get(0))
        .unwrap();
    assert_eq!(rows, 1, "设置只允许一行");
    assert!(
        !table_exists(&conn, "organize_settings"),
        "v18 之后旧表不能还在（否则就是两套存储）"
    );
}

// ------------------------------------------------------------------ 迁移

/// 把一个最新库退回 **v15** 的样子：settings 拆回 organize_settings、去掉 v16 的列与
/// v17 的表。返回值是退回前完整设置的 revision（v18 搬行时要原样保留）。
fn downgrade_to_v15(dir: &Path) {
    let conn = db(dir);
    conn.execute_batch(
        "ALTER TABLE settings RENAME TO organize_settings;
         ALTER TABLE organize_settings DROP COLUMN diary_time_zone;
         ALTER TABLE organize_settings DROP COLUMN default_style_id;
         ALTER TABLE captures DROP COLUMN trashed_from_state;
         DROP TABLE styles;
         UPDATE schema_migrations SET version = 15;
         PRAGMA user_version = 15;",
    )
    .unwrap();
    assert!(!table_exists(&conn, "settings"), "前提：v15 没有 settings");
    assert!(!table_exists(&conn, "styles"), "前提：v15 没有 styles");
    assert!(
        !column_exists(&conn, "captures", "trashed_from_state"),
        "前提：v15 没有回收站列"
    );
}

/// 把一个最新库退回 **v11** 的形状：`organize_settings` 单行还在（带用户参数），
/// 但 v12 的 `revision` 列、v13 的插件表、v15 的标记列、v16 的回收站列、v17 的
/// `styles` 表与 v18 的 `settings` 表都不存在。这是最严格的起点：11 → 18 要一次
/// 补上 12–18 的所有段。
fn downgrade_to_v11(dir: &Path) {
    let conn = db(dir);
    conn.execute_batch(
        "ALTER TABLE settings RENAME TO organize_settings;
         ALTER TABLE organize_settings DROP COLUMN diary_time_zone;
         ALTER TABLE organize_settings DROP COLUMN default_style_id;
         ALTER TABLE captures DROP COLUMN trashed_from_state;
         DROP TABLE styles;
         DROP TABLE plugin_grants;
         DROP TABLE plugin_installations;
         ALTER TABLE index_meta DROP COLUMN auto_rebuild_marker;
         ALTER TABLE diary_days DROP COLUMN revision;
         UPDATE schema_migrations SET version = 11;
         PRAGMA user_version = 11;",
    )
    .unwrap();
    assert!(!table_exists(&conn, "settings"), "前提：v11 没有 settings");
    assert!(!table_exists(&conn, "styles"), "前提：v11 没有 styles");
    assert!(
        table_exists(&conn, "organize_settings"),
        "前提：v11 只有 organize_settings"
    );
    assert!(
        !column_exists(&conn, "captures", "trashed_from_state"),
        "前提：v11 没有回收站列"
    );
    assert!(
        !column_exists(&conn, "diary_days", "revision"),
        "前提：v11 没有 v12 的 revision 列"
    );
}

#[test]
fn a_v15_library_migrates_to_v18_with_v16_and_v17_segments() {
    let dir = tempfile::tempdir().unwrap();
    let (organize_revision, full_revision) = {
        let mut core = open(dir.path());
        let mut organize = core.organize_settings().unwrap();
        organize.enabled = true;
        organize.auto_organize_time = "06:45".to_owned();
        organize.template_id = "compress".to_owned();
        organize.max_calls_per_day = 9;
        let organize = core
            .update_organize_settings(&organize, organize.revision)
            .unwrap();

        let mut full = core.get_settings().unwrap();
        full.diary_time_zone = "Asia/Shanghai".to_owned();
        full.default_style_id = Some("sty_default".to_owned());
        let full = core.update_settings(&full, full.revision).unwrap();
        (organize.revision, full.revision)
    };
    assert_eq!(full_revision, organize_revision + 1);
    downgrade_to_v15(dir.path());

    let core = open(dir.path());
    assert_eq!(core.schema_version().unwrap(), SCHEMA_VERSION);
    assert_eq!(SCHEMA_VERSION, 18, "本 PR 占 18");

    // v18 的搬行：整理原值保留（含 revision），完整对象独有的两格取默认。
    let settings = core.get_settings().unwrap();
    assert_eq!(
        settings.diary_time_zone, "UTC",
        "旧库没有设备时区可搬，不能瞎猜"
    );
    assert_eq!(settings.default_style_id, None, "不替用户猜一个默认风格");
    assert!(settings.auto_organize_enabled, "用户开的开关不能被升级关掉");
    assert_eq!(settings.auto_organize_time, "06:45");
    assert_eq!(settings.revision, full_revision, "revision 原样保留");

    let organize = core.organize_settings().unwrap();
    assert_eq!(organize.template_id, "compress");
    assert_eq!(organize.max_calls_per_day, 9);
    assert_eq!(organize.revision, full_revision);

    // v16 / v17 两段真的补上了，而不是只把版本号写成 18。
    let conn = db(dir.path());
    assert!(column_exists(&conn, "captures", "trashed_from_state"));
    assert!(table_exists(&conn, "styles"));
    let style_rows: i64 = conn
        .query_row("SELECT COUNT(*) FROM styles", [], |row| row.get(0))
        .unwrap();
    assert_eq!(style_rows, 1, "v17 要种一行默认风格");
    assert_eq!(
        conn.query_row("SELECT id FROM styles", [], |row| row.get::<_, String>(0))
            .unwrap(),
        "sty_default"
    );
    assert!(table_exists(&conn, "settings"));
    assert!(!table_exists(&conn, "organize_settings"), "旧表要被吸收掉");
}

/// 真 v11 形状的库升到 18：用户设过的整理参数（含非默认模板/上限/开关）一个不丢，
/// 且 12–18 的每一段结构都补上——版本号单调，不能只把 `user_version` 写成 18。
#[test]
fn a_v11_library_migrates_to_v18_keeping_organize_parameters() {
    let dir = tempfile::tempdir().unwrap();
    let saved_revision = {
        let mut core = open(dir.path());
        let mut organize = core.organize_settings().unwrap();
        organize.enabled = true;
        organize.template_id = "compress".to_owned();
        organize.auto_organize_time = "06:45".to_owned();
        organize.max_calls_per_day = 9;
        organize.max_chars = 12_345;
        organize.supplement_on_new_materials = false;
        let updated = core
            .update_organize_settings(&organize, organize.revision)
            .unwrap();
        assert_eq!(updated.revision, 2);
        updated.revision
    };
    downgrade_to_v11(dir.path());

    let core = open(dir.path());
    assert_eq!(core.schema_version().unwrap(), SCHEMA_VERSION);
    assert_eq!(SCHEMA_VERSION, 18, "本 PR 占 18");

    // v18 的搬行：v11 行的原值（含 revision）原样进 settings，完整对象独有的两格取默认。
    let settings = core.get_settings().unwrap();
    assert!(settings.auto_organize_enabled, "用户开的开关不能被升级关掉");
    assert_eq!(settings.auto_organize_time, "06:45");
    assert_eq!(settings.revision, saved_revision, "revision 原样保留");
    assert_eq!(settings.diary_time_zone, "UTC", "旧库没有设备时区可搬");
    assert_eq!(settings.default_style_id, None);

    let organize = core.organize_settings().unwrap();
    assert_eq!(organize.template_id, "compress", "模板不能被重置");
    assert_eq!(organize.max_calls_per_day, 9, "上限不能被重置");
    assert_eq!(organize.max_chars, 12_345);
    assert!(
        !organize.supplement_on_new_materials,
        "显式关掉的补做开关不能被升级打开"
    );

    // 11 → 18 的每一段结构都在（不是只把版本号写成 18）。
    let conn = db(dir.path());
    assert!(table_exists(&conn, "settings"));
    assert!(!table_exists(&conn, "organize_settings"), "旧表要被吸收掉");
    assert!(column_exists(&conn, "diary_days", "revision"), "v12 列");
    assert!(table_exists(&conn, "plugin_installations"), "v13 表");
    assert!(
        column_exists(&conn, "index_meta", "auto_rebuild_marker"),
        "v15 列"
    );
    assert!(
        column_exists(&conn, "captures", "trashed_from_state"),
        "v16 列"
    );
    assert!(table_exists(&conn, "styles"), "v17 表");
}

#[test]
fn a_v17_library_only_needs_the_settings_segment() {
    let dir = tempfile::tempdir().unwrap();
    let full_revision = {
        let mut core = open(dir.path());
        let mut organize = core.organize_settings().unwrap();
        organize.enabled = true;
        organize.auto_organize_time = "05:20".to_owned();
        organize.max_calls_per_day = 11;
        core.update_organize_settings(&organize, organize.revision)
            .unwrap();
        core.get_settings().unwrap().revision
    };

    {
        let conn = db(dir.path());
        // v17 有回收站列与 styles 表，只是没有 settings。
        conn.execute_batch(
            "ALTER TABLE settings RENAME TO organize_settings;
             ALTER TABLE organize_settings DROP COLUMN diary_time_zone;
             ALTER TABLE organize_settings DROP COLUMN default_style_id;
             INSERT INTO styles (snapshot_id, id, revision, label, updated_at, examples,
                                 banned_expressions, current)
             VALUES ('sty_custom', 'sty_custom', 1, '自定义', '2026-10-19T00:00:00Z', '[]', '[]', 0);
             UPDATE schema_migrations SET version = 17;
             PRAGMA user_version = 17;",
        )
        .unwrap();
        assert!(!table_exists(&conn, "settings"));
    }

    let core = open(dir.path());
    assert_eq!(core.schema_version().unwrap(), SCHEMA_VERSION);
    let settings = core.get_settings().unwrap();
    assert!(settings.auto_organize_enabled);
    assert_eq!(settings.auto_organize_time, "05:20");
    assert_eq!(settings.revision, full_revision);

    let conn = db(dir.path());
    let styles: i64 = conn
        .query_row("SELECT COUNT(*) FROM styles", [], |row| row.get(0))
        .unwrap();
    assert_eq!(styles, 2, "v17 已有的风格行一行不丢");
    assert_eq!(
        conn.query_row(
            "SELECT COUNT(*) FROM styles WHERE snapshot_id = 'sty_custom'",
            [],
            |row| row.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
    assert!(
        column_exists(&conn, "captures", "trashed_from_state"),
        "v17 本来就有的列不能因为补 settings 被碰掉"
    );
}

#[test]
fn a_latest_library_with_missing_structure_heals_on_open() {
    let dir = tempfile::tempdir().unwrap();
    {
        let core = open(dir.path());
        // 先确认结构都在，再手工删掉三样（模拟中间构建/手工破坏）。
        assert!(core.get_settings().is_ok());
        let conn = db(dir.path());
        assert!(table_exists(&conn, "settings"));
        assert!(table_exists(&conn, "styles"));
        assert!(column_exists(&conn, "captures", "trashed_from_state"));
        conn.execute_batch(
            "DROP TABLE settings;
             DROP TABLE styles;
             ALTER TABLE captures DROP COLUMN trashed_from_state;",
        )
        .unwrap();
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION, "版本号还是 18，但结构不全");
    }

    let core = open(dir.path());
    assert_eq!(core.schema_version().unwrap(), SCHEMA_VERSION);
    let settings = core.get_settings().unwrap();
    assert_eq!(settings.diary_time_zone, "UTC");
    assert_eq!(settings.revision, 1);
    assert!(
        !settings.auto_organize_enabled,
        "自愈补的默认行仍然是默认关"
    );

    let conn = db(dir.path());
    assert!(table_exists(&conn, "settings"));
    assert!(table_exists(&conn, "styles"));
    assert!(column_exists(&conn, "captures", "trashed_from_state"));
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM styles", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
}

/// 复审第 5 条：`ensure_v16_v17_v18_structure` 不能只看 `styles` 表。
///
/// `table_exists("styles")` 为真不能证明 `idx_styles_current` 在——索引被手工删掉
/// （或补建失败）时，「一条风格只有一个当前版本」就不再由库保证，`query_row` 会静默
/// 取任意一条。版本号是 18 也不代表索引在，所以每次 open 都要独立补。
#[test]
fn a_v18_library_missing_the_styles_current_index_gets_it_back_on_open() {
    let dir = tempfile::tempdir().unwrap();
    {
        let _core = open(dir.path());
        let conn = db(dir.path());
        assert!(index_exists(&conn, "idx_styles_current"), "前提：索引本来在");
        conn.execute_batch("DROP INDEX idx_styles_current;").unwrap();
        assert!(!index_exists(&conn, "idx_styles_current"), "前提：索引已被删");
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION, "版本号仍是 18，但索引缺了");
    }

    let core = open(dir.path());
    assert_eq!(core.schema_version().unwrap(), SCHEMA_VERSION);
    let conn = db(dir.path());
    assert!(
        index_exists(&conn, "idx_styles_current"),
        "结构兜底必须补回 idx_styles_current"
    );

    // 补回后部分唯一索引重新生效：同一条风格再插一个 current = 1 必须被拒。
    let duplicate = conn.execute(
        "INSERT INTO styles (snapshot_id, id, revision, label, updated_at, current) \
         VALUES ('stv_duplicate', 'sty_default', 99, '第二个当前行', '2026-10-20T00:00:00Z', 1)",
        [],
    );
    assert!(
        duplicate.is_err(),
        "idx_styles_current 补回后必须重新约束唯一性"
    );
}

/// 复审第 6 条：空 / 非法 settings 的用例（核心这一层是完整对象，没有「部分」形态；
/// 「部分对象」是 C ABI 的入参形状，见 `crates/diary_capi/src/tests.rs` 的三态用例）。
///
/// 这里钉住：非法时区 / 非法计划时间 / 过长的风格 id 都是 `invalid_argument`，
/// 且**一个字节不改**（值不变、revision 不推进）。
#[test]
fn invalid_full_settings_are_rejected_without_touching_a_byte() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = open(dir.path());
    let before = core.get_settings().unwrap();

    let cases = [
        ("Asia / Shanghai".to_owned(), before.auto_organize_time.clone(), None),
        (String::new(), before.auto_organize_time.clone(), None),
        (before.diary_time_zone.clone(), "25:99".to_owned(), None),
        (before.diary_time_zone.clone(), "7:5".to_owned(), None),
        (
            before.diary_time_zone.clone(),
            before.auto_organize_time.clone(),
            Some("s".repeat(129)),
        ),
    ];
    for (time_zone, time, style_id) in cases {
        let mut candidate = before.clone();
        candidate.diary_time_zone = time_zone.clone();
        candidate.auto_organize_time = time.clone();
        candidate.default_style_id = style_id.clone();
        let error = core
            .update_settings(&candidate, before.revision)
            .expect_err("非法设置必须报错");
        assert_eq!(
            error.code(),
            ErrorCode::InvalidArgument,
            "时区 {time_zone:?} / 时间 {time:?} / 风格 {style_id:?} 应该是 invalid_argument"
        );
        assert_eq!(
            core.get_settings().unwrap(),
            before,
            "非法设置必须零改动"
        );
    }
}

/// 复审第 6 条的建议：删旧表前必须数行数。
///
/// 构造一个**异常形状**的 v17 库：`organize_settings` 里那一行的 id 不是 1。
/// 真实 DDL 的 `CHECK (id = 1)` 会挡住这种行，这里用一个没有 CHECK 的重建表模拟
/// 「库被改坏 / 中间构建留下的怪形状」——v18 的搬家用 `WHERE id = 1` 会一行都搬
/// 不过去。此时必须报错回滚，**不能** 补一行默认设置然后把旧表 DROP 掉
/// （那就是静默丢掉用户的整理设置）。
#[test]
fn a_broken_organize_settings_row_aborts_the_v18_migration_instead_of_dropping_it() {
    let dir = tempfile::tempdir().unwrap();
    {
        let _core = open(dir.path());
        let conn = db(dir.path());
        conn.execute_batch(
            "ALTER TABLE settings RENAME TO organize_settings;
             ALTER TABLE organize_settings DROP COLUMN diary_time_zone;
             ALTER TABLE organize_settings DROP COLUMN default_style_id;
             CREATE TABLE broken_organize_settings (
                 id                        INTEGER PRIMARY KEY,
                 enabled                   INTEGER NOT NULL DEFAULT 0,
                 template_id               TEXT NOT NULL,
                 template_params           TEXT NOT NULL DEFAULT '{}',
                 max_materials             INTEGER NOT NULL,
                 max_chars                 INTEGER NOT NULL,
                 max_calls_per_run         INTEGER NOT NULL,
                 max_calls_per_day         INTEGER NOT NULL,
                 max_failure_rate          REAL NOT NULL,
                 auto_organize_time        TEXT NOT NULL,
                 supplement_on_new_materials INTEGER NOT NULL DEFAULT 1,
                 revision                  INTEGER NOT NULL DEFAULT 1,
                 updated_at                TEXT NOT NULL
             );
             INSERT INTO broken_organize_settings
                 SELECT 2, enabled, template_id, template_params, max_materials, max_chars,
                        max_calls_per_run, max_calls_per_day, max_failure_rate,
                        auto_organize_time, supplement_on_new_materials, revision, updated_at
                 FROM organize_settings;
             DROP TABLE organize_settings;
             ALTER TABLE broken_organize_settings RENAME TO organize_settings;
             UPDATE schema_migrations SET version = 17;
             PRAGMA user_version = 17;",
        )
        .unwrap();
        assert!(!table_exists(&conn, "settings"), "前提：v17 没有 settings");
        let rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM organize_settings", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(rows, 1, "前提：旧表有一行（id = 2，搬不过去）");
    }

    let error = match Core::open(dir.path().join("library.sqlite")) {
        Ok(_) => panic!("搬不动旧行时必须报错，不能静默 DROP"),
        Err(error) => error,
    };
    assert!(
        format!("{error}").contains("搬设置行失败"),
        "错误要说清是搬行失败：{error}"
    );

    // 事务回滚：旧表与那一行还在，settings 没建出来，版本停在 17。
    let conn = db(dir.path());
    assert!(
        table_exists(&conn, "organize_settings"),
        "报错时绝不能把旧表 DROP 掉"
    );
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM organize_settings", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        1,
        "旧行必须原样保留，下次 open 还能重试"
    );
    assert!(!table_exists(&conn, "settings"), "建表也要随事务回滚");
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, 17, "版本号不能写成 18");
}

/// 复审第 6 条：**真 v15 库**（v15 构建真实写出 + 未检查点 WAL）升到 18。
///
/// 合成测试是从 v18 库反向改出来的，覆盖不到旧二进制的页布局、真实 WAL 残留与
/// `schema_migrations` 的历史行。这份样本由 `tools/migration/make_v15_fixture.sh`
/// 用 v15 构建（`f31ee95`）写出，写完 `abort()` 留下 685 KiB 未检查点 WAL；说明见
/// `tests/fixtures/v15-library/README.md`。
#[test]
fn a_real_v15_library_fixture_with_an_uncheckpointed_wal_migrates_to_v18() {
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

    // **预检必须走文件级**：任何 `Connection::open`（哪怕只读一次
    // `PRAGMA user_version`）都会在连接 drop 时把 WAL checkpoint 回主库并删掉 `-wal`；
    // 那样 `Core::open` 拿到的就是已检查点的主库，这个测试名里的
    // “uncheckpointed wal” 也就名不副实了（实测：预检一次后 4096 + 700432 变成
    // 409600 且 `-wal` 消失）。这里只 `std::fs::metadata`，一个字节都不让 SQLite 碰。
    let main_len = std::fs::metadata(&path).unwrap().len();
    let wal_path = dir.path().join("library.sqlite-wal");
    let wal_len = std::fs::metadata(&wal_path).unwrap().len();
    assert!(
        wal_len > 0,
        "fixture 必须带未检查点的 -wal（文件级预检，不能先开库）"
    );
    assert!(
        main_len < 64 * 1024,
        "未检查点的主库只有一页（v15 样本是 4096 B）；{main_len} B 说明 WAL 已被检查点过"
    );
    assert!(
        wal_len > main_len,
        "数据应该还在 WAL 里：main={main_len} B, wal={wal_len} B"
    );

    // 当前构建打开：v15 → 18 的三段（回收站列 + styles + settings）都必须跑。
    // 「这确实是一份 v15 库」由 open 之后的断言证明：`schema_migrations` 必须保留
    // v15 的历史行、两条记录（含 emoji）必须可读——它们只在 `-wal` 里，WAL 丢了
    // 这两条就都不成立。版本号 15 本身无法在不 checkpoint 的前提下从 SQLite 读到
    // （主库头里的 user_version 还不是 15——本 fixture 是创建时的 0，真值在那条 WAL
    // 里），所以不在这里断言。
    let core = open(dir.path());
    assert_eq!(core.schema_version().unwrap(), SCHEMA_VERSION);
    assert_eq!(SCHEMA_VERSION, 18);
    assert_eq!(core.count_captures().unwrap(), 2, "旧记录一条不能丢");

    let settings = core.get_settings().unwrap();
    assert_eq!(settings.diary_time_zone, "UTC", "旧库没有设备时区可搬");
    assert!(!settings.auto_organize_enabled, "迁移种下的默认行是关");
    assert_eq!(settings.default_style_id, None, "不替用户猜一个默认风格");
    assert_eq!(settings.revision, 1);

    let conn = db(dir.path());
    assert!(column_exists(&conn, "captures", "trashed_from_state"), "v16 列");
    assert!(table_exists(&conn, "styles"), "v17 表");
    assert!(
        index_exists(&conn, "idx_styles_current"),
        "v17 的部分唯一索引"
    );
    assert!(table_exists(&conn, "settings"), "v18 表");
    assert!(
        !table_exists(&conn, "organize_settings"),
        "旧表要被吸收掉，不留第二份真值"
    );

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
    assert_eq!(history, vec![15, 18], "v15 的历史行要保留，18 是这次落到的版本");

    let quick: String = conn
        .query_row("PRAGMA quick_check", [], |row| row.get(0))
        .unwrap();
    assert_eq!(quick, "ok");
    let foreign_keys: i64 = conn
        .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(foreign_keys, 0, "外键检查必须为空");

    // 真正落盘的 `-wal` 被合并/检查点后，库文件仍能独立打开且数据在。
    drop(conn);
    let reopened = open(dir.path());
    assert_eq!(reopened.count_captures().unwrap(), 2);
}

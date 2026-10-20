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

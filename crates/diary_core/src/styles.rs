//! 风格管理：`styles.list` / `styles.get` / `styles.save`（#114）。
//!
//! 契约的 `StyleProfile` **原地可改**（`revision` 是乐观锁），但历史日记必须答得出
//! 「这一版当时用的是哪一版风格」。所以存储不是「一条风格一行、改了就覆盖」，而是：
//!
//! - `styles` 表按 `(id, revision)` 一行一个**不可变版本**；
//! - `current = 1` 标记当前版本，部分唯一索引保证一条风格只有一个当前版本；
//! - `snapshot_id` 是版本行的稳定标识，写进 `diary_versions.style_snapshot_id`。
//!   生成之后风格再改，也只是新增一行，旧日记指的那一版一个字都不动。
//!
//! 内置的默认风格（`sty_default`）在迁移/首次打开时种成 revision 1、内容全空的种子行。
//! 这样 `diary.generate` 不点名风格时冻结的 `sty_default` 真的能指到一行版本，而不是
//! 一个查不出来的魔法字符串；用户点名别的 id 仍然按 `not_found` 处理，不静默回退。
//!
//! **没有回填**：升级前不存在风格配置，迁移只建立新表 + 种默认行，不替用户伪造偏好。

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension};

use crate::error::{CoreError, Result};
use crate::model::StyleProfile;
use crate::support;
use crate::Core;

/// `styles` 表里当前版本的列（顺序与 `style_from_row` 一一对应）。
const STYLE_COLUMNS: &str = "snapshot_id, id, revision, label, updated_at, length_preference, \
                             person, tone, emotional_temperature, rhetoric_level, examples, \
                             banned_expressions";

/// 默认风格的 id。与 `diary::DEFAULT_STYLE_SNAPSHOT_ID` 是同一个值：默认风格的第一个
/// 版本行同时用它当 `snapshot_id`，所以既有的 `styleSnapshotId = sty_default` 在
/// 升级后仍能指到一行真实记录。
pub(crate) const DEFAULT_STYLE_ID: &str = "sty_default";

/// 默认风格的展示名。
pub(crate) const DEFAULT_STYLE_LABEL: &str = "默认风格";

/// 单个风格自由文本字段的字符上限（`label` 与六个偏好字段；示例/禁用表达按**每条**算）。
///
/// 风格文本会原样进 system 消息，且不像材料那样受 `DIRECT_INPUT_CHAR_LIMIT` 约束。
/// 没有上限时，调用方可以用一个几万字的 `tone` 把每次生成的实际输入撑大，既绕过
/// 材料预算，也让本地小模型直接超上下文。500 字足够写清一条偏好。
pub(crate) const STYLE_FIELD_CHAR_LIMIT: usize = 500;

/// 一条风格所有自由文本的**合计**上限（含示例与禁用表达）。
///
/// 单字段各有 500 字上限还不够：七个字段 + 两组列表叠起来仍可能到几千字。2,000 字
/// 是「所有偏好都写满」也到不了的数量级，同时把 system 消息的增量钉在一个可解释的界内。
pub(crate) const STYLE_TOTAL_CHAR_LIMIT: usize = 2_000;

/// 示例 / 禁用表达各自最多条数（每条仍受 `STYLE_FIELD_CHAR_LIMIT` 约束）。
pub(crate) const STYLE_LIST_ITEM_LIMIT: usize = 20;

/// 一条风格版本：当前 profile + 它的版本行标识。
///
/// `profile` 里没有 `snapshot_id`（契约没有这个字段，不发明）；生成路径需要的是
/// 「冻结哪一行」，所以这个内部结构把两者绑在一起。
#[derive(Debug, Clone)]
pub(crate) struct StyleVersion {
    pub snapshot_id: String,
    pub profile: StyleProfile,
}

type RawStyle = (
    String,
    String,
    i64,
    String,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    String,
    String,
);

fn style_from_row(raw: RawStyle) -> Result<StyleProfile> {
    let (
        _snapshot_id,
        id,
        revision,
        label,
        updated_at,
        length_preference,
        person,
        tone,
        emotional_temperature,
        rhetoric_level,
        examples,
        banned_expressions,
    ) = raw;
    Ok(StyleProfile {
        id,
        label,
        revision,
        updated_at: support::parse_iso(&updated_at)?,
        length_preference,
        person,
        tone,
        emotional_temperature,
        rhetoric_level,
        examples: serde_json::from_str(&examples)?,
        banned_expressions: serde_json::from_str(&banned_expressions)?,
    })
}

fn raw_style_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RawStyle> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
        row.get(7)?,
        row.get(8)?,
        row.get(9)?,
        row.get(10)?,
        row.get(11)?,
    ))
}

fn version_from_raw(raw: RawStyle) -> Result<StyleVersion> {
    let snapshot_id = raw.0.clone();
    Ok(StyleVersion {
        snapshot_id,
        profile: style_from_row(raw)?,
    })
}

/// 当前版本（没有这一条风格或没有 `current = 1` 行时为空）。
fn current_version(conn: &Connection, style_id: &str) -> Result<Option<StyleVersion>> {
    let sql = format!("SELECT {STYLE_COLUMNS} FROM styles WHERE id = ?1 AND current = 1");
    let raw: Option<RawStyle> = conn
        .query_row(&sql, params![style_id], raw_style_row)
        .optional()?;
    raw.map(version_from_raw).transpose()
}

/// `styles.list`：返回每条风格的**当前版本**；顺序按展示名、id 固定，避免列表抖动。
pub(crate) fn list(core: &Core) -> Result<Vec<StyleProfile>> {
    let sql = format!("SELECT {STYLE_COLUMNS} FROM styles WHERE current = 1 ORDER BY label, id");
    let mut statement = core.conn.prepare(&sql)?;
    let rows = statement
        .query_map([], raw_style_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    rows.into_iter().map(style_from_row).collect()
}

/// `styles.get`：不存在（或只剩历史版本、没有当前版本）⇒ `not_found`。
pub(crate) fn get(core: &Core, style_id: &str) -> Result<StyleProfile> {
    let id = style_id.trim();
    if id.is_empty() {
        return Err(CoreError::InvalidArgument {
            reason: "styles.get 需要一个非空的 styleId".to_owned(),
        });
    }
    current_version(&core.conn, id)?
        .map(|version| version.profile)
        .ok_or_else(|| CoreError::NotFound {
            entity: "风格配置",
            id: id.to_owned(),
        })
}

/// 生成路径用：显式 id 优先，否则读取设置里选用的风格，未设置时取内置默认风格。
/// 选用的风格不存在时如实报错，不能悄悄忽略用户的选择。
pub(crate) fn resolve(core: &Core, style_id: Option<&str>) -> Result<StyleVersion> {
    let id = match style_id.map(str::trim).filter(|value| !value.is_empty()) {
        Some(id) => id.to_owned(),
        None => core
            .get_settings()?
            .default_style_id
            .unwrap_or_else(|| DEFAULT_STYLE_ID.to_owned()),
    };
    current_version(&core.conn, &id)?.ok_or_else(|| CoreError::NotFound {
        entity: "风格配置",
        id,
    })
}

/// `styles.save`：新建/更新一条风格，**每次保存落一个新版本行**。
///
/// - 有 `expected_revision` 时先比对当前 revision（没有当前行时实际是 0）：
///   不符报 `revision_conflict`，一个字节都不改；
/// - 没有 `expected_revision` 时按「调用方明确接受覆盖当前版本」处理（契约允许省略）；
/// - `revision` / `updatedAt` / `snapshot_id` 由核心决定，调用方传什么都不会被原样信；
/// - 返回落库后的当前 profile（`revision` 已 +1，`updatedAt` 是真实写入时刻）。
pub(crate) fn save(
    core: &mut Core,
    profile: &StyleProfile,
    expected_revision: Option<i64>,
) -> Result<StyleProfile> {
    let id = profile.id.trim().to_owned();
    if id.is_empty() {
        return Err(CoreError::InvalidArgument {
            reason: "styles.save 需要一个非空的 styleId".to_owned(),
        });
    }
    let label = profile.label.trim().to_owned();
    if label.is_empty() {
        return Err(CoreError::InvalidArgument {
            reason: "styles.save 需要一个非空的 label（界面上的展示名）".to_owned(),
        });
    }

    validate_style_text(profile)?;

    let existing = current_version(&core.conn, &id)?;
    if let Some(expected) = expected_revision {
        let actual = existing
            .as_ref()
            .map(|version| version.profile.revision)
            .unwrap_or(0);
        if expected != actual {
            return Err(CoreError::RevisionConflict { expected, actual });
        }
    }

    let next_revision = existing
        .as_ref()
        .map(|version| version.profile.revision + 1)
        .unwrap_or(1);
    let now = support::now();
    let snapshot_id = support::new_id("stv");
    let examples = serde_json::to_string(&profile.examples)?;
    let banned_expressions = serde_json::to_string(&profile.banned_expressions)?;

    let tx = core.conn.transaction()?;
    if existing.is_some() {
        tx.execute(
            "UPDATE styles SET current = 0 WHERE id = ?1 AND current = 1",
            params![id],
        )?;
    }
    tx.execute(
        "INSERT INTO styles (snapshot_id, id, revision, label, updated_at, length_preference, \
         person, tone, emotional_temperature, rhetoric_level, examples, banned_expressions, \
         current) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, 1)",
        params![
            snapshot_id,
            id,
            next_revision,
            label,
            support::to_iso(now),
            normalize_optional(profile.length_preference.as_deref()),
            normalize_optional(profile.person.as_deref()),
            normalize_optional(profile.tone.as_deref()),
            normalize_optional(profile.emotional_temperature.as_deref()),
            normalize_optional(profile.rhetoric_level.as_deref()),
            examples,
            banned_expressions,
        ],
    )?;
    tx.commit()?;

    Ok(StyleProfile {
        id,
        label,
        revision: next_revision,
        updated_at: now,
        length_preference: normalize_optional(profile.length_preference.as_deref()),
        person: normalize_optional(profile.person.as_deref()),
        tone: normalize_optional(profile.tone.as_deref()),
        emotional_temperature: normalize_optional(profile.emotional_temperature.as_deref()),
        rhetoric_level: normalize_optional(profile.rhetoric_level.as_deref()),
        examples: profile.examples.clone(),
        banned_expressions: profile.banned_expressions.clone(),
    })
}

/// 校验风格自由文本的长度。**超限报 `invalid_argument`、一个字节都不落库**。
///
/// 只做长度与条数，不解释、不改写用户文字：这是资源边界，不是内容审查。
fn validate_style_text(profile: &StyleProfile) -> Result<()> {
    let too_long = |field: &str, chars: usize| CoreError::InvalidArgument {
        reason: format!(
            "风格字段 {field} 最多 {STYLE_FIELD_CHAR_LIMIT} 个字符，实际 {chars}"
        ),
    };
    let mut total = 0_usize;
    let check = |field: &str, value: &str, total: &mut usize| -> Result<()> {
        let chars = value.trim().chars().count();
        if chars > STYLE_FIELD_CHAR_LIMIT {
            return Err(too_long(field, chars));
        }
        *total += chars;
        Ok(())
    };

    check("label", &profile.label, &mut total)?;
    for (field, value) in [
        ("lengthPreference", profile.length_preference.as_deref()),
        ("person", profile.person.as_deref()),
        ("tone", profile.tone.as_deref()),
        ("emotionalTemperature", profile.emotional_temperature.as_deref()),
        ("rhetoricLevel", profile.rhetoric_level.as_deref()),
    ] {
        if let Some(value) = value {
            check(field, value, &mut total)?;
        }
    }
    for (field, values) in [
        ("examples", &profile.examples),
        ("bannedExpressions", &profile.banned_expressions),
    ] {
        if values.len() > STYLE_LIST_ITEM_LIMIT {
            return Err(CoreError::InvalidArgument {
                reason: format!(
                    "风格字段 {field} 最多 {STYLE_LIST_ITEM_LIMIT} 条，实际 {} 条",
                    values.len()
                ),
            });
        }
        for (index, value) in values.iter().enumerate() {
            check(&format!("{field}[{index}]"), value, &mut total)?;
        }
    }
    if total > STYLE_TOTAL_CHAR_LIMIT {
        return Err(CoreError::InvalidArgument {
            reason: format!(
                "一条风格的所有自由文本合计最多 {STYLE_TOTAL_CHAR_LIMIT} 个字符，实际 {total} 个"
            ),
        });
    }
    Ok(())
}

/// 空串按「没有这个偏好」处理：界面清空输入框与「没填」是同一件事。
fn normalize_optional(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

/// 种默认风格（迁移与打开时幂等）。**只在没有当前版本时插入**：
/// 用户已经把默认风格改到 revision 2 时，不能被一次打开重置回 revision 1。
pub(crate) fn ensure_default_style(conn: &Connection, now: DateTime<Utc>) -> rusqlite::Result<()> {
    let exists: i64 = conn.query_row(
        "SELECT COUNT(*) FROM styles WHERE id = ?1 AND current = 1",
        params![DEFAULT_STYLE_ID],
        |row| row.get(0),
    )?;
    if exists == 0 {
        seed_default_style(conn, now)?;
    }
    Ok(())
}

/// 迁移里种默认风格的**第一个版本行**。内容全空：这是「没有偏好」的缺省，
/// 不是替用户编一套他觉得好看的偏好。`INSERT OR IGNORE` 让重复执行幂等。
pub(crate) fn seed_default_style(conn: &Connection, now: DateTime<Utc>) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO styles (snapshot_id, id, revision, label, updated_at, \
         length_preference, person, tone, emotional_temperature, rhetoric_level, examples, \
         banned_expressions, current) VALUES (?1, ?1, 1, ?2, ?3, NULL, NULL, NULL, NULL, NULL, \
         '[]', '[]', 1)",
        params![DEFAULT_STYLE_ID, DEFAULT_STYLE_LABEL, support::to_iso(now)],
    )?;
    Ok(())
}

/// 把一条风格渲染成提示词里的**风格偏好段**（不是产品指令）。
///
/// 返回 `None` 表示这一版没有任何偏好（默认风格），调用方不要往 system 里塞空段。
/// 字段都是用户自己的自然语言描述；这里只做排版，不解释、不改写。
pub(crate) fn style_instruction(profile: &StyleProfile) -> Option<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut push = |label: &str, value: Option<&String>| {
        if let Some(value) = value.map(String::as_str).map(str::trim) {
            if !value.is_empty() {
                lines.push(format!("- {label}：{value}"));
            }
        }
    };
    push("篇幅", profile.length_preference.as_ref());
    push("人称", profile.person.as_ref());
    push("语气", profile.tone.as_ref());
    push("情感温度", profile.emotional_temperature.as_ref());
    push("修辞程度", profile.rhetoric_level.as_ref());
    let examples: Vec<&str> = profile
        .examples
        .iter()
        .map(String::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .collect();
    if !examples.is_empty() {
        lines.push(format!(
            "- 风格示例（只参考语气，不要复述示例里的事实）：{}",
            examples.join("；")
        ));
    }
    let banned: Vec<&str> = profile
        .banned_expressions
        .iter()
        .map(String::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .collect();
    if !banned.is_empty() {
        lines.push(format!("- 不要使用这些表达：{}", banned.join("；")));
    }
    if lines.is_empty() {
        return None;
    }
    Some(format!(
        "\n\n用户选择的写作风格「{}」：\n{}",
        profile.label,
        lines.join("\n")
    ))
}

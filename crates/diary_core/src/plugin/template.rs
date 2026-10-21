//! 声明式插件：模板解析、占位符白名单与单遍渲染。
//!
//! 这一层**没有任何求值器**：模板是固定文本 + 白名单占位符，替换只做一遍，
//! 用户材料里出现 `{{body}}` 这样的字样不会被二次展开（注入不改变行为）。

use std::collections::HashMap;

use serde::Deserialize;

use crate::error::{CoreError, Result};
use crate::model::{PluginTemplate, StyleProfile};

use super::manifest::{MAX_TEMPLATE_BYTES, MAX_TEMPLATES};

/// 声明式入口 `main.json`。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DeclarativeEntry {
    schema_version: i64,
    templates: Vec<RawTemplate>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawTemplate {
    id: String,
    kind: String,
    label: String,
    #[serde(default)]
    description: String,
    format: String,
    body: String,
}

/// 解析 `main.json` 并转成缓存用的 [`PluginTemplate`]（`plugin_id` 由安装路径给）。
pub(crate) fn parse_declarative(plugin_id: &str, bytes: &[u8]) -> Result<Vec<PluginTemplate>> {
    if bytes.len() > 256 * 1024 {
        return Err(CoreError::PluginManifestInvalid {
            reason: "声明式入口 main.json 超过 256 KiB".to_owned(),
        });
    }
    let entry: DeclarativeEntry =
        serde_json::from_slice(bytes).map_err(|error| CoreError::PluginManifestInvalid {
            reason: format!("声明式入口 main.json 不是合法 JSON：{error}"),
        })?;
    if entry.schema_version != 1 {
        return Err(CoreError::PluginManifestInvalid {
            reason: format!(
                "声明式入口的 schemaVersion 是 {}，本构建只支持 1",
                entry.schema_version
            ),
        });
    }
    if entry.templates.len() > MAX_TEMPLATES {
        return Err(CoreError::PluginManifestInvalid {
            reason: format!("声明式入口有 {} 个模板，超过上限 {MAX_TEMPLATES}", entry.templates.len()),
        });
    }
    let mut templates = Vec::with_capacity(entry.templates.len());
    for raw in entry.templates {
        if raw.label.trim().is_empty() || raw.label.chars().count() > 80 {
            return Err(CoreError::PluginManifestInvalid {
                reason: format!("模板「{}」的 label 不能为空且最长 80 个字符", raw.id),
            });
        }
        if raw.body.len() > MAX_TEMPLATE_BYTES {
            return Err(CoreError::PluginManifestInvalid {
                reason: format!("模板「{}」超过 {MAX_TEMPLATE_BYTES} 字节", raw.id),
            });
        }
        validate_placeholders(&raw.kind, &raw.body).map_err(|reason| {
            CoreError::PluginManifestInvalid {
                reason: format!("模板「{}」{reason}", raw.id),
            }
        })?;
        templates.push(PluginTemplate {
            plugin_id: plugin_id.to_owned(),
            id: raw.id,
            kind: raw.kind,
            label: raw.label,
            description: raw.description,
            format: raw.format,
            body: raw.body,
        });
    }
    Ok(templates)
}

/// 占位符白名单。`kind` 为 `style` 或 `export`。
///
/// 未知占位符直接拒绝：既避免拼写错误静默输出空串，也避免把模板语言做成
/// 「将来某天能求值」的东西。
pub(crate) fn validate_placeholders(kind: &str, body: &str) -> std::result::Result<(), String> {
    let allowed: &[&str] = match kind {
        "style" => &[
            "id",
            "label",
            "lengthPreference",
            "person",
            "tone",
            "emotionalTemperature",
            "rhetoricLevel",
            "examples",
            "bannedExpressions",
        ],
        "export" => &["dayKey", "date", "title", "versionId", "body"],
        other => return Err(format!("未知模板类型「{other}」")),
    };
    let mut rest = body;
    while let Some(start) = rest.find("{{") {
        let after = &rest[start + 2..];
        let end = after
            .find("}}")
            .ok_or_else(|| "有未闭合的 `{{`".to_owned())?;
        let name = after[..end].trim();
        if name.is_empty() {
            return Err("有空的占位符 `{{}}`".to_owned());
        }
        if !allowed.contains(&name) {
            return Err(format!(
                "用了不支持的占位符「{name}」（{kind} 模板支持：{}）",
                allowed.join(" / ")
            ));
        }
        rest = &after[end + 2..];
    }
    if rest.contains("}}") {
        return Err("有孤立的 `}}`".to_owned());
    }
    Ok(())
}

/// 单遍替换。值里的 `{{...}}` 不会被再次展开。
pub(crate) fn render(body: &str, values: &HashMap<&str, String>) -> Result<String> {
    let mut output = String::with_capacity(body.len().saturating_add(64));
    let mut rest = body;
    while let Some(start) = rest.find("{{") {
        output.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else {
            return Err(CoreError::PluginManifestInvalid {
                reason: "模板里有未闭合的 `{{`".to_owned(),
            });
        };
        let name = after[..end].trim();
        let value = values.get(name).ok_or_else(|| CoreError::PluginManifestInvalid {
            reason: format!("模板里有不支持的占位符「{name}」"),
        })?;
        output.push_str(value);
        rest = &after[end + 2..];
    }
    output.push_str(rest);
    Ok(output)
}

/// 风格模板渲染（#114 的 `StyleProfile` 字段 → 指令文本）。
///
/// 返回的文本是**风格偏好**，不是产品指令；调用方要不要把它放进生成提示词由
/// 生成路径决定，模板不会自己扩权。
pub(crate) fn render_style(template: &PluginTemplate, profile: &StyleProfile) -> Result<String> {
    let examples = profile.examples.join("；");
    let banned = profile.banned_expressions.join("；");
    let values: HashMap<&str, String> = HashMap::from([
        ("id", profile.id.clone()),
        ("label", profile.label.clone()),
        ("lengthPreference", profile.length_preference.clone().unwrap_or_default()),
        ("person", profile.person.clone().unwrap_or_default()),
        ("tone", profile.tone.clone().unwrap_or_default()),
        (
            "emotionalTemperature",
            profile.emotional_temperature.clone().unwrap_or_default(),
        ),
        ("rhetoricLevel", profile.rhetoric_level.clone().unwrap_or_default()),
        ("examples", examples),
        ("bannedExpressions", banned),
    ]);
    render(&template.body, &values)
}

/// 导出模板渲染：一天一条，值只有日期键、标题、版本 id 与正文。
pub(crate) fn render_export(
    template: &PluginTemplate,
    day_key: &str,
    version_id: &str,
    body: &str,
) -> Result<String> {
    let values: HashMap<&str, String> = HashMap::from([
        ("dayKey", day_key.to_owned()),
        ("date", day_key.to_owned()),
        ("title", day_key.to_owned()),
        ("versionId", version_id.to_owned()),
        ("body", body.to_owned()),
    ]);
    render(&template.body, &values)
}

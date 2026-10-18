//! 插件清单：解析、静态校验、权限目录与 API 版本范围。
//!
//! 这一层**只读数据**：不读入口文件的内容语义，也不实例化任何 Wasm。清单里的每个
//! 字段都要么被校验、要么被明确忽略；未知权限/未知激活点是拒绝而不是静默放行。

use std::collections::{BTreeSet, HashMap, HashSet};

use serde::Deserialize;

use crate::error::{CoreError, Result};
use crate::model::{PluginEntryKind, PluginManifest, PluginPermission};

use super::template;

/// 本构建实现的插件宿主 API 版本。与核心 `API_VERSION` 分开：插件 ABI 可以单独
/// 加字段/加导入函数而不动契约的 `apiVersion`。
pub const PLUGIN_API_VERSION: &str = "1.0";

/// 支持的清单版本。升级清单语义必须抬这个数字。
pub const MANIFEST_VERSION: i64 = 1;

/// 包内文件数上限。
pub const MAX_PACKAGE_FILES: usize = 128;
/// 单个文件大小上限。
pub const MAX_FILE_BYTES: usize = 8 * 1024 * 1024;
/// 整个包解压后的大小上限。
pub const MAX_PACKAGE_BYTES: usize = 16 * 1024 * 1024;
/// 清单 JSON 大小上限。
pub const MAX_MANIFEST_BYTES: usize = 64 * 1024;
/// 单个模板正文上限。
pub const MAX_TEMPLATE_BYTES: usize = 16 * 1024;
/// 一个插件最多几个模板。
pub const MAX_TEMPLATES: usize = 32;
/// 激活点最多几条。
pub const MAX_ACTIVATION_EVENTS: usize = 16;
/// 声明的权限最多几条。
pub const MAX_PERMISSIONS: usize = 16;

/// `read_selected_material`：读用户当前选中的材料/日记版本。
pub const PERM_READ_SELECTED: &str = "read_selected_material";
/// `read_date_range`：读明确日期范围内的日记。
pub const PERM_READ_RANGE: &str = "read_date_range";
/// `create_derived_content`：产出候选派生内容（核心校验后交给调用方，不自动入库）。
pub const PERM_DERIVED: &str = "create_derived_content";
/// `create_export_artifact`：创建导出物（只能写进核心给的限域目录）。
pub const PERM_EXPORT: &str = "create_export_artifact";
/// `invoke_provider:<providerId>`：调用某个已配置的 Provider（密钥由核心保管）。
pub const PERM_PROVIDER_PREFIX: &str = "invoke_provider:";

/// 清单的原始 JSON 形状。`settingsSchema` 是任意 JSON 对象，只在缓存里保存文本。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawManifest {
    manifest_version: i64,
    id: String,
    name: String,
    version: String,
    author: String,
    license: String,
    api_range: String,
    supported_platforms: Vec<String>,
    entry_kind: String,
    entry_file: String,
    activation_events: Vec<String>,
    permissions: Vec<String>,
    #[serde(default)]
    settings_schema: Option<serde_json::Value>,
    #[serde(default)]
    package_hashes: HashMap<String, String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    homepage: Option<String>,
}

/// 解析 `manifest.json` 的字节。只做 JSON 与字段形状，业务校验在 [`validate`]。
pub(crate) fn parse(bytes: &[u8]) -> Result<PluginManifest> {
    if bytes.len() > MAX_MANIFEST_BYTES {
        return Err(CoreError::PluginManifestInvalid {
            reason: format!(
                "manifest.json 有 {} 字节，超过上限 {} 字节",
                bytes.len(),
                MAX_MANIFEST_BYTES
            ),
        });
    }
    let raw: RawManifest =
        serde_json::from_slice(bytes).map_err(|error| CoreError::PluginManifestInvalid {
            reason: format!("manifest.json 不是合法的 JSON 或字段类型不对：{error}"),
        })?;
    let entry_kind =
        PluginEntryKind::from_wire(&raw.entry_kind).ok_or_else(|| {
            CoreError::PluginManifestInvalid {
                reason: format!(
                    "entryKind 只能是 declarative 或 wasm，实际是「{}」",
                    raw.entry_kind
                ),
            }
        })?;
    let settings_schema_json = match raw.settings_schema {
        None | Some(serde_json::Value::Null) => None,
        Some(value @ serde_json::Value::Object(_)) => {
            let text = serde_json::to_string(&value)?;
            if text.len() > 16 * 1024 {
                return Err(CoreError::PluginManifestInvalid {
                    reason: "settingsSchema 超过 16 KiB".to_owned(),
                });
            }
            Some(text)
        }
        Some(_) => {
            return Err(CoreError::PluginManifestInvalid {
                reason: "settingsSchema 必须是 JSON 对象".to_owned(),
            })
        }
    };
    Ok(PluginManifest {
        manifest_version: raw.manifest_version,
        id: raw.id,
        name: raw.name,
        version: raw.version,
        author: raw.author,
        license: raw.license,
        api_range: raw.api_range,
        supported_platforms: raw.supported_platforms,
        entry_kind,
        entry_file: raw.entry_file,
        activation_events: raw.activation_events,
        permissions: raw.permissions,
        settings_schema_json,
        package_hashes: raw.package_hashes,
        description: raw.description,
        homepage: raw.homepage,
    })
}

/// 返回清单的结构/兼容问题。空 vec 表示可安装。
///
/// `platform` 已经过 [`normalize_platform`]；`templates` 是声明式入口解析结果。
pub(crate) fn validate(
    manifest: &PluginManifest,
    platform: &str,
) -> Vec<String> {
    let mut issues = Vec::new();

    if manifest.manifest_version != MANIFEST_VERSION {
        issues.push(format!(
            "incompatible: manifestVersion 是 {}，本构建只支持 {}",
            manifest.manifest_version, MANIFEST_VERSION
        ));
    }
    if !valid_id(&manifest.id) {
        issues.push(format!(
            "invalid: id「{}」只能是小写字母/数字/`.`/`_`/`-`，长度 2–64，且以字母或数字开头",
            manifest.id
        ));
    }
    if !valid_name(&manifest.name) {
        issues.push("invalid: name 不能为空，最长 80 个字符".to_owned());
    }
    if !valid_version(&manifest.version) {
        issues.push(format!(
            "invalid: version「{}」要是三段数字（例如 1.2.0）",
            manifest.version
        ));
    }
    if manifest.author.trim().is_empty() || manifest.author.chars().count() > 120 {
        issues.push("invalid: author 不能为空，最长 120 个字符".to_owned());
    }
    if manifest.license.trim().is_empty() || manifest.license.chars().count() > 120 {
        issues.push("invalid: license 不能为空，最长 120 个字符".to_owned());
    }
    match api_range_compatible(&manifest.api_range) {
        Ok(true) => {}
        Ok(false) => issues.push(format!(
            "incompatible: apiRange「{}」不包含本构建的插件 API {}",
            manifest.api_range, PLUGIN_API_VERSION
        )),
        Err(reason) => issues.push(format!("invalid: apiRange {}", reason)),
    }
    if manifest.supported_platforms.is_empty() {
        issues.push("invalid: supportedPlatforms 不能为空".to_owned());
    }
    for platform_name in &manifest.supported_platforms {
        if normalize_platform(platform_name).is_none() {
            issues.push(format!(
                "invalid: supportedPlatforms 里有未知平台「{platform_name}」"
            ));
        }
    }
    if !manifest
        .supported_platforms
        .iter()
        .filter_map(|name| normalize_platform(name))
        .any(|name| name == platform)
    {
        issues.push(format!(
            "incompatible: 支持平台是 {:?}，当前平台是「{platform}」",
            manifest.supported_platforms
        ));
    }
    if !valid_relative_path(&manifest.entry_file) {
        issues.push(format!(
            "invalid: entryFile「{}」必须是包内的相对路径，不能含 `..` 或绝对路径",
            manifest.entry_file
        ));
    } else {
        let expected = match manifest.entry_kind {
            PluginEntryKind::Wasm => ".wasm",
            PluginEntryKind::Declarative => ".json",
        };
        if !manifest.entry_file.ends_with(expected) {
            issues.push(format!(
                "invalid: entryKind = {} 时 entryFile 必须以 {expected} 结尾",
                manifest.entry_kind.wire()
            ));
        }
    }
    if manifest.package_hashes.is_empty() {
        issues.push("invalid: packageHashes 不能为空（除 manifest.json 外每个文件都要有哈希）".to_owned());
    }
    if manifest.package_hashes.len() > MAX_PACKAGE_FILES {
        issues.push(format!(
            "invalid: packageHashes 列了 {} 个文件，超过上限 {}",
            manifest.package_hashes.len(),
            MAX_PACKAGE_FILES
        ));
    }
    for (path, hash) in &manifest.package_hashes {
        if !valid_relative_path(path) {
            issues.push(format!("invalid: packageHashes 里有越界路径「{path}」"));
        }
        if !valid_sha256(hash) {
            issues.push(format!(
                "invalid: packageHashes 里「{path}」的哈希不是 64 位小写十六进制"
            ));
        }
    }
    if !manifest.package_hashes.contains_key(&manifest.entry_file) {
        issues.push(format!(
            "invalid: entryFile「{}」没有出现在 packageHashes 里",
            manifest.entry_file
        ));
    }

    let mut seen_events = BTreeSet::new();
    if manifest.activation_events.len() > MAX_ACTIVATION_EVENTS {
        issues.push(format!(
            "invalid: activationEvents 有 {} 条，超过上限 {}",
            manifest.activation_events.len(),
            MAX_ACTIVATION_EVENTS
        ));
    }
    for event in &manifest.activation_events {
        if !seen_events.insert(event.clone()) {
            issues.push(format!("invalid: activationEvents 里有重复项「{event}」"));
        }
        if let Err(reason) = validate_activation_event(event) {
            issues.push(format!("invalid: 激活点「{event}」{reason}"));
        }
    }

    let mut seen_permissions = BTreeSet::new();
    if manifest.permissions.len() > MAX_PERMISSIONS {
        issues.push(format!(
            "invalid: permissions 有 {} 条，超过上限 {}",
            manifest.permissions.len(),
            MAX_PERMISSIONS
        ));
    }
    for permission in &manifest.permissions {
        if !seen_permissions.insert(permission.clone()) {
            issues.push(format!("invalid: permissions 里有重复项「{permission}」"));
        }
        if let Err(reason) = validate_permission(permission) {
            issues.push(format!("invalid: 权限「{permission}」{reason}"));
        }
    }

    issues
}

/// 声明式入口的额外校验：模板 id 唯一、格式合法、占位符白名单。
pub(crate) fn validate_templates(
    manifest: &PluginManifest,
    templates: &[crate::model::PluginTemplate],
) -> Vec<String> {
    let mut issues = Vec::new();
    if manifest.entry_kind != PluginEntryKind::Declarative {
        if !templates.is_empty() {
            issues.push("invalid: 只有声明式插件可以带 templates".to_owned());
        }
        return issues;
    }
    if templates.is_empty() {
        issues.push("invalid: 声明式插件的入口里至少要有 1 个模板".to_owned());
    }
    if templates.len() > MAX_TEMPLATES {
        issues.push(format!(
            "invalid: 模板有 {} 个，超过上限 {}",
            templates.len(),
            MAX_TEMPLATES
        ));
    }
    let mut seen = BTreeSet::new();
    for template in templates {
        if !seen.insert(template.id.clone()) {
            issues.push(format!("invalid: 模板 id「{}」重复", template.id));
        }
        if !valid_template_id(&template.id) {
            issues.push(format!(
                "invalid: 模板 id「{}」只能用小写字母/数字/`.`/`_`/`-`",
                template.id
            ));
        }
        if template.kind != "style" && template.kind != "export" {
            issues.push(format!(
                "invalid: 模板「{}」的 kind 只能是 style 或 export，实际是「{}」",
                template.id, template.kind
            ));
        }
        let expected_format = match template.kind.as_str() {
            "style" => "style_instruction",
            "export" => "markdown",
            _ => "",
        };
        if template.kind == "export" && template.format != "markdown" && template.format != "plain_text" {
            issues.push(format!(
                "invalid: 导出模板「{}」的 format 只能是 markdown 或 plain_text",
                template.id
            ));
        }
        if template.kind == "style" && template.format != expected_format {
            issues.push(format!(
                "invalid: 风格模板「{}」的 format 必须是 style_instruction",
                template.id
            ));
        }
        if template.body.len() > MAX_TEMPLATE_BYTES {
            issues.push(format!(
                "invalid: 模板「{}」超过 {} 字节",
                template.id, MAX_TEMPLATE_BYTES
            ));
        }
        if template.body.contains('\0') {
            issues.push(format!("invalid: 模板「{}」含 NUL 字节", template.id));
        }
        if let Err(reason) = template::validate_placeholders(&template.kind, &template.body) {
            issues.push(format!("invalid: 模板「{}」{reason}", template.id));
        }
    }
    issues
}

/// 解析并校验 `apiRange`。支持 `1.0` / `^1.0` / `>=1.0 <2.0`。
pub(crate) fn api_range_compatible(range: &str) -> std::result::Result<bool, String> {
    let host = parse_version(PLUGIN_API_VERSION).expect("宿主版本常量合法");
    let trimmed = range.trim();
    if trimmed.is_empty() {
        return Err("不能为空（例如 1.0 或 >=1.0 <2.0）".to_owned());
    }
    if let Some(rest) = trimmed.strip_prefix('^') {
        let min = parse_version(rest.trim())
            .ok_or_else(|| format!("「{range}」里的版本号不是三段数字"))?;
        return Ok(host.0 == min.0 && host >= min);
    }
    if let Some(rest) = trimmed.strip_prefix(">=") {
        let mut parts = rest.split_whitespace();
        let min_text = parts
            .next()
            .ok_or_else(|| format!("「{range}」缺少最小版本"))?;
        let min = parse_version(min_text)
            .ok_or_else(|| format!("「{range}」里的最小版本不是三段数字"))?;
        let mut ok = host >= min;
        if let Some(max_part) = parts.next() {
            let max_text = max_part.strip_prefix('<').ok_or_else(|| {
                format!("「{range}」里只支持 `>=x.y.z <a.b.c` 这种范围")
            })?;
            let max = parse_version(max_text)
                .ok_or_else(|| format!("「{range}」里的最大版本不是三段数字"))?;
            ok = ok && host < max;
        }
        if parts.next().is_some() {
            return Err(format!("「{range}」里有多余的片段"));
        }
        return Ok(ok);
    }
    let min = parse_version(trimmed)
        .ok_or_else(|| format!("「{range}」不是三段数字版本，也不是 ^/>= 范围"))?;
    Ok(host == min)
}

/// 激活点合法性。**启动激活点刻意不支持**：任务书要求启动不执行插件。
pub(crate) fn validate_activation_event(event: &str) -> std::result::Result<(), String> {
    if event == "export.start" || event == "organize.template.select" {
        return Ok(());
    }
    if let Some(id) = event.strip_prefix("command:") {
        if valid_short_id(id) {
            return Ok(());
        }
        return Err("命令 id 不合法".to_owned());
    }
    if let Some(kind) = event.strip_prefix("materials_pending:") {
        if valid_short_id(kind) {
            return Ok(());
        }
        return Err("材料类型不合法".to_owned());
    }
    if matches!(event, "startup" | "app.start" | "startup.start") {
        return Err("不支持启动激活点：应用启动只读缓存清单，不执行插件".to_owned());
    }
    Err("只支持 command:* / materials_pending:* / export.start / organize.template.select".to_owned())
}

/// 权限合法性。未知权限拒绝（拼错不能被当作「没有权限」静默放行）。
pub(crate) fn validate_permission(permission: &str) -> std::result::Result<(), String> {
    match permission {
        PERM_READ_SELECTED | PERM_READ_RANGE | PERM_DERIVED | PERM_EXPORT => Ok(()),
        other => {
            if let Some(provider_id) = other.strip_prefix(PERM_PROVIDER_PREFIX) {
                if valid_id(provider_id) {
                    Ok(())
                } else {
                    Err("invoke_provider: 后面的 providerId 不合法".to_owned())
                }
            } else {
                Err(format!(
                    "不在本构建支持的权限里（支持 {PERM_READ_SELECTED} / {PERM_READ_RANGE} / {PERM_DERIVED} / {PERM_EXPORT} / {PERM_PROVIDER_PREFIX}<id>）"
                ))
            }
        }
    }
}

/// 权限目录：给界面展示的中文说明。顺序稳定。
pub(crate) fn permission_catalog(permissions: &[String]) -> Vec<PluginPermission> {
    permissions
        .iter()
        .map(|permission| {
            if let Some(provider_id) = permission.strip_prefix(PERM_PROVIDER_PREFIX) {
                PluginPermission {
                    id: permission.clone(),
                    label: format!("调用服务「{provider_id}」"),
                    description: "通过主程序调用这个已配置的 Provider；密钥与预算由主程序保管，插件拿不到密钥。".to_owned(),
                    risk: "high".to_owned(),
                }
            } else {
                match permission.as_str() {
                    PERM_READ_SELECTED => PluginPermission {
                        id: permission.clone(),
                        label: "读取当前选中的材料/日记".to_owned(),
                        description: "只能读用户在这一次操作里选中的对象，不能读全库。".to_owned(),
                        risk: "medium".to_owned(),
                    },
                    PERM_READ_RANGE => PluginPermission {
                        id: permission.clone(),
                        label: "读取明确日期范围".to_owned(),
                        description: "只能读用户给定的起止日期（闭区间）内的当前日记版本。".to_owned(),
                        risk: "medium".to_owned(),
                    },
                    PERM_DERIVED => PluginPermission {
                        id: permission.clone(),
                        label: "创建派生结果".to_owned(),
                        description: "产出候选派生内容；核心校验来源后才交给调用方，不会自动改写日记。".to_owned(),
                        risk: "low".to_owned(),
                    },
                    PERM_EXPORT => PluginPermission {
                        id: permission.clone(),
                        label: "生成导出文件".to_owned(),
                        description: "只能写进核心给定的限域导出目录，不能写任意路径，也不能覆盖原件。".to_owned(),
                        risk: "low".to_owned(),
                    },
                    _ => PluginPermission {
                        id: permission.clone(),
                        label: "未知权限".to_owned(),
                        description: "本构建不认识这项权限；安装会被拒绝。".to_owned(),
                        risk: "high".to_owned(),
                    },
                }
            }
        })
        .collect()
}

/// 调用方授予的权限是否覆盖清单声明的全部权限。
pub(crate) fn missing_grants(manifest: &PluginManifest, grants: &[String]) -> Vec<String> {
    let granted: HashSet<&str> = grants.iter().map(String::as_str).collect();
    manifest
        .permissions
        .iter()
        .filter(|permission| !granted.contains(permission.as_str()))
        .cloned()
        .collect()
}

/// 规范化平台名；未知平台返回 None。
pub fn normalize_platform(value: &str) -> Option<&'static str> {
    match value.trim().to_ascii_lowercase().as_str() {
        "windows" | "win" | "win32" => Some("windows"),
        "linux" => Some("linux"),
        "macos" | "mac" | "darwin" | "osx" => Some("macos"),
        "android" => Some("android"),
        "ios" => Some("ios"),
        other => match other {
            "ohos" | "harmonyos" => Some("ohos"),
            _ => None,
        },
    }
}

/// 当前宿主的平台名（与 [`normalize_platform`] 的输出一致）。
pub fn host_platform() -> &'static str {
    normalize_platform(std::env::consts::OS).unwrap_or("unknown")
}

pub(crate) fn valid_id(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() < 2 || bytes.len() > 64 {
        return false;
    }
    let first = bytes[0];
    if !(first.is_ascii_lowercase() || first.is_ascii_digit()) {
        return false;
    }
    bytes.iter().all(|byte| {
        byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-')
    })
}

/// 命令/材料类型允许单字母 id（插件 id 仍要求至少 2 个字符，避免与文件名歧义）。
fn valid_short_id(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.is_empty() || bytes.len() > 64 {
        return false;
    }
    let first = bytes[0];
    (first.is_ascii_lowercase() || first.is_ascii_digit())
        && bytes.iter().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-')
        })
}

fn valid_name(value: &str) -> bool {
    let trimmed = value.trim();
    !trimmed.is_empty() && trimmed.chars().count() <= 80
}

fn valid_version(value: &str) -> bool {
    parse_version(value).is_some()
}

fn parse_version(value: &str) -> Option<(u64, u64, u64)> {
    let mut parts = value.trim().split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    // 宿主 API 版本写成 `1.0`（两段）也合法；插件自己的 version 由 valid_version
    // 另外要求三段，两个口径不要混。
    let patch = match parts.next() {
        Some(patch) => patch.parse().ok()?,
        None => 0,
    };
    if parts.next().is_some() {
        return None;
    }
    Some((major, minor, patch))
}

/// 相对路径：非空、以 `/` 分隔、没有空段/`.`/`..`、没有反斜杠/冒号/NUL。
pub(crate) fn valid_relative_path(value: &str) -> bool {
    if value.is_empty() || value.len() > 160 || value.contains('\0') || value.contains('\\') {
        return false;
    }
    if value.starts_with('/') || value.contains(':') {
        return false;
    }
    value.split('/').all(|part| {
        !part.is_empty() && part != "." && part != ".." && !part.ends_with(' ')
    })
}

fn valid_template_id(value: &str) -> bool {
    valid_id(value)
}

pub(crate) fn valid_sha256(value: &str) -> bool {
    let normalized = value.trim().strip_prefix("sha256:").unwrap_or(value.trim());
    normalized.len() == 64 && normalized.bytes().all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

/// 规范化哈希：去掉可选的 `sha256:` 前缀并转小写。
pub(crate) fn normalize_sha256(value: &str) -> String {
    value
        .trim()
        .strip_prefix("sha256:")
        .unwrap_or(value.trim())
        .to_ascii_lowercase()
}

/// 从权限里拆出 Provider id。
pub(crate) fn provider_id_for_permission(permission: &str) -> Option<&str> {
    permission.strip_prefix(PERM_PROVIDER_PREFIX)
}

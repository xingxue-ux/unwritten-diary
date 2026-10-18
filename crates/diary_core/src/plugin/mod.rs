//! 插件 SDK 与宿主（B6，#88）。
//!
//! 这一片把任务书 §8 落到实处：可安装的包、清单/版本/平台/权限检查、两张缓存表、
//! 声明式模板与 Wasm 沙箱、四个激活点、失败隔离、审计与示例。
//!
//! 三条边界是刻意的：
//! 1. **应用启动只读缓存清单**：`Core::open` 只跑数据库迁移，不触碰插件目录；
//!    `list_plugins` 只读 `plugin_installations` / `plugin_grants`。
//! 2. **安装不执行包内容**：Wasm 只看魔数与版本，声明式入口只解析 JSON。
//! 3. **权限是用户授予的，不是插件声明的**：`plugin_grants` 与清单分开存；更新时
//!    新增权限必须重新授权，否则安装被拒。
//!
//! 与既有实现的接缝：
//! - 风格模板的输入/字段照 #114 的 `StyleProfile`（不是另造一套风格对象）；
//! - 导出模板的 `format` 与 dayKey 范围照 #113 的 `ExportRequest`；
//! - Provider 调用复用 #66 的配置、能力闸门、密钥托管与传输层；
//! - 材料内容仍然只是数据（#64）：模板只做白名单占位符单遍替换，Wasm 也只能通过
//!   宿主函数拿到授权范围内的文本。

mod manifest;
mod package;
mod sandbox;
mod template;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use chrono::NaiveDate;
use rusqlite::{params, OptionalExtension};
use serde_json::json;

use crate::error::{CoreError, Result};
use crate::model::{
    PluginArtifact, PluginEntryKind, PluginInspection, PluginInstallation, PluginInvocation,
    PluginManifest, PluginPermission, PluginRunResult, PluginTemplate,
};
use crate::model::ProviderCapability;
use crate::provider::{self};
use crate::support;
use crate::Core;

pub use manifest::PERM_READ_RANGE;
pub use manifest::PERM_READ_SELECTED;
pub use manifest::PERM_DERIVED;
pub use manifest::PERM_EXPORT;
pub use manifest::{host_platform, PLUGIN_API_VERSION};
pub use package::api_version as plugin_api_version;

const MAX_SELECTED_VERSIONS: usize = 64;
const MAX_SELECTED_CAPTURES: usize = 128;
const MAX_RANGE_DAYS: i64 = 366;
const MAX_PRELOAD_BYTES: usize = 1024 * 1024;
const MAX_PER_BODY_BYTES: usize = 256 * 1024;
const MAX_SOURCE_BYTES: usize = 64 * 1024;
const MAX_OUTPUT_BYTES: usize = 1024 * 1024;
const MAX_INPUT_TEXT_BYTES: usize = 1024 * 1024;

/// Wasm 插件可以用的宿主函数清单（文档/测试与 linker 注册保持同一份口径）。
pub const HOST_IMPORTS: &[&str] = &[
    "context_utf8",
    "selected_diary_count",
    "selected_diary_day",
    "selected_diary_body",
    "range_diary_count",
    "range_diary_day",
    "range_diary_body",
    "source_text",
    "asset_range",
    "provider_invoke",
    "emit_derived",
    "export_write",
    "state_get",
    "state_put",
    "emit_output",
    "report_progress",
    "log",
];

/// 四个基础权限的目录（Provider 权限在清单里动态展开）。
pub fn base_permissions() -> Vec<PluginPermission> {
    manifest::permission_catalog(&[
        PERM_READ_SELECTED.to_owned(),
        PERM_READ_RANGE.to_owned(),
        PERM_DERIVED.to_owned(),
        PERM_EXPORT.to_owned(),
    ])
}

impl Core {
    /// 检查一个插件包。**不执行包内容**：只读清单与文件、校验哈希/版本/平台/权限。
    pub fn inspect_plugin_package(
        &self,
        package_path: impl AsRef<Path>,
        platform: &str,
    ) -> Result<PluginInspection> {
        let platform = manifest::normalize_platform(platform).ok_or_else(|| {
            CoreError::InvalidArgument {
                reason: format!(
                    "未知平台「{platform}」；支持 windows / linux / macos / android / ios"
                ),
            }
        })?;
        let path = package_path.as_ref();
        let contents = package::load(path, platform)?;
        let cached = self.find_installation(&contents.manifest.id)?;
        let existing_grants = cached
            .as_ref()
            .map(|installation| installation.permissions.clone())
            .unwrap_or_default();
        let missing_permissions = manifest::missing_grants(&contents.manifest, &existing_grants);
        let installed_version = cached.as_ref().map(|installation| installation.version.clone());
        let is_upgrade = cached
            .as_ref()
            .is_some_and(|installation| installation.version != contents.manifest.version);
        let permissions = manifest::permission_catalog(&contents.manifest.permissions);
        Ok(PluginInspection {
            package_path: package::display_path(path),
            package_kind: contents.kind.to_owned(),
            package_hash: contents.package_hash.clone(),
            manifest: contents.manifest.clone(),
            templates: contents.templates.clone(),
            permissions,
            missing_permissions,
            compatible: contents.compatible(),
            issues: contents.issues.clone(),
            installed_version,
            is_upgrade,
        })
    }

    /// 安装（或升级）一个插件。调用方必须在展示权限后显式传入 `grants`。
    ///
    /// 拒绝且**零改动**的情形：清单/哈希/平台/API 不兼容、授权缺项、授予了清单没有
    /// 声明的权限。升级不会默默扩大权限：新增权限必须出现在这次 `grants` 里。
    pub fn install_plugin(
        &mut self,
        package_path: impl AsRef<Path>,
        platform: &str,
        grants: &[String],
    ) -> Result<PluginInstallation> {
        let platform = manifest::normalize_platform(platform).ok_or_else(|| {
            CoreError::InvalidArgument {
                reason: format!("未知平台「{platform}」"),
            }
        })?;
        let path = package_path.as_ref();
        let contents = package::load(path, platform)?;
        if !contents.compatible() {
            return Err(CoreError::PluginIncompatible {
                reason: contents.issues.join("；"),
            });
        }
        let manifest = &contents.manifest;
        for grant in grants {
            if !manifest.permissions.iter().any(|item| item == grant) {
                return Err(CoreError::InvalidArgument {
                    reason: format!(
                        "授予了插件「{}」没有声明的权限「{grant}」；清单声明的权限是 {:?}",
                        manifest.id, manifest.permissions
                    ),
                });
            }
        }
        let missing = manifest::missing_grants(manifest, grants);
        if !missing.is_empty() {
            return Err(CoreError::PluginPermissionDenied {
                reason: format!(
                    "插件「{}」需要这些权限，但这次没有授予：{}；请先向用户说明用途，再重新安装并授权",
                    manifest.id,
                    missing.join("、")
                ),
            });
        }
        let root = self.root_dir()?.to_path_buf();
        let previous = self.find_installation(&manifest.id)?;
        let install_rel_path = package::write_install(&root, &contents)?;
        let target = root.join(&install_rel_path);
        let now = support::to_iso(support::now());
        let templates_json = serde_json::to_string(&contents.templates)?;
        let manifest_json = serde_json::to_string(manifest)?;
        let enabled = previous.as_ref().map(|item| item.enabled).unwrap_or(true);
        let installed_at = previous
            .as_ref()
            .map(|item| support::to_iso(item.installed_at))
            .unwrap_or_else(|| now.clone());

        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO plugin_installations \
             (id, version, manifest_version, manifest_json, templates_json, entry_kind, entry_file, \
              package_hash, install_rel_path, enabled, state_json, last_run_at, last_run_json, \
              last_error, installed_at, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, '{}', NULL, NULL, NULL, ?11, ?12) \
             ON CONFLICT(id) DO UPDATE SET \
              version = excluded.version, manifest_version = excluded.manifest_version, \
              manifest_json = excluded.manifest_json, templates_json = excluded.templates_json, \
              entry_kind = excluded.entry_kind, entry_file = excluded.entry_file, \
              package_hash = excluded.package_hash, install_rel_path = excluded.install_rel_path, \
              enabled = plugin_installations.enabled, last_error = NULL, \
              updated_at = excluded.updated_at",
            params![
                manifest.id,
                manifest.version,
                manifest.manifest_version,
                manifest_json,
                templates_json,
                manifest.entry_kind.wire(),
                manifest.entry_file,
                contents.package_hash,
                install_rel_path,
                if enabled { 1 } else { 0 },
                installed_at,
                now,
            ],
        )?;
        tx.execute(
            "DELETE FROM plugin_grants WHERE plugin_id = ?1",
            params![manifest.id],
        )?;
        for permission in &manifest.permissions {
            tx.execute(
                "INSERT INTO plugin_grants (plugin_id, permission, granted_at) VALUES (?1, ?2, ?3)",
                params![manifest.id, permission, now],
            )?;
        }
        tx.commit()?;
        package::remove_other_versions(&root, &manifest.id, &target);
        self.find_installation(&manifest.id)?.ok_or_else(|| CoreError::NotFound {
            entity: "插件安装",
            id: manifest.id.clone(),
        })
    }

    /// 启用/停用。停用后新的调用立刻被拒；已经产生的日记与导出物不受影响。
    pub fn set_plugin_enabled(
        &mut self,
        plugin_id: &str,
        enabled: bool,
    ) -> Result<PluginInstallation> {
        let changed = self.conn.execute(
            "UPDATE plugin_installations SET enabled = ?1, updated_at = ?2 WHERE id = ?3",
            params![if enabled { 1 } else { 0 }, support::to_iso(support::now()), plugin_id],
        )?;
        if changed == 0 {
            return Err(CoreError::NotFound {
                entity: "插件",
                id: plugin_id.to_owned(),
            });
        }
        self.find_installation(plugin_id)?.ok_or_else(|| CoreError::NotFound {
            entity: "插件",
            id: plugin_id.to_owned(),
        })
    }

    /// 卸载：删安装行与执行文件，保留已经产生的日记、原件与导出物。
    ///
    /// `remove_private_state` 对齐契约里的「可选择的私有缓存」。当前实现的插件私有
    /// 状态就在安装行里，卸载必然清理；没有额外缓存目录可留，所以这个参数目前只影响
    /// 将来可能加的缓存（这里是 no-op，文档如实写明）。
    pub fn uninstall_plugin(&mut self, plugin_id: &str, remove_private_state: bool) -> Result<()> {
        let installation = self
            .find_installation(plugin_id)?
            .ok_or_else(|| CoreError::NotFound {
                entity: "插件",
                id: plugin_id.to_owned(),
            })?;
        let root = self.root_dir()?.to_path_buf();
        let relative: Option<String> = self
            .conn
            .query_row(
                "SELECT install_rel_path FROM plugin_installations WHERE id = ?1",
                params![plugin_id],
                |row| row.get(0),
            )
            .optional()?;
        let _ = remove_private_state;
        self.conn.execute(
            "DELETE FROM plugin_installations WHERE id = ?1",
            params![plugin_id],
        )?;
        if let Some(relative) = relative {
            let _ = std::fs::remove_dir_all(root.join(relative));
        }
        package::remove_plugin_dir(&root, plugin_id);
        let _ = installation;
        Ok(())
    }

    /// 已安装插件。**只读缓存清单**：不读、不编译、不执行插件入口。
    pub fn list_plugins(&self) -> Result<Vec<PluginInstallation>> {
        let mut statement = self.conn.prepare(
            "SELECT id, version, manifest_version, manifest_json, templates_json, entry_kind, \
             entry_file, package_hash, install_rel_path, enabled, last_run_at, last_error, \
             installed_at, updated_at FROM plugin_installations ORDER BY installed_at, id",
        )?;
        let rows: Vec<RawInstallation> = statement
            .query_map([], raw_installation)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows.into_iter()
            .map(|row| self.installation_from_raw(row))
            .collect()
    }

    /// 某个插件已授予的权限（升序）。只读授权表。
    pub fn plugin_grants(&self, plugin_id: &str) -> Result<Vec<String>> {
        let mut statement = self
            .conn
            .prepare("SELECT permission FROM plugin_grants WHERE plugin_id = ?1 ORDER BY permission")?;
        let rows = statement
            .query_map(params![plugin_id], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// 全部声明式模板（可按插件过滤）。读缓存，不碰插件文件。
    pub fn plugin_templates(&self, plugin_id: Option<&str>) -> Result<Vec<PluginTemplate>> {
        let installations = self.list_plugins()?;
        let mut templates = Vec::new();
        for installation in installations {
            if plugin_id.is_some_and(|id| id != installation.plugin_id) {
                continue;
            }
            templates.extend(installation.templates);
        }
        Ok(templates)
    }

    /// 运行一个插件。激活点必须是清单声明过的；权限、预算、隔离都在这里收口。
    pub fn run_plugin(&mut self, invocation: &PluginInvocation) -> Result<PluginRunResult> {
        let installation = self
            .find_installation(&invocation.plugin_id)?
            .ok_or_else(|| CoreError::NotFound {
                entity: "插件",
                id: invocation.plugin_id.clone(),
            })?;
        if !installation.enabled {
            return Err(CoreError::InvalidState {
                entity: "插件",
                id: installation.plugin_id.clone(),
                state: "已停用；停用后不执行任何新的调用".to_owned(),
            });
        }
        let manifest = self.cached_manifest(&installation)?;
        if !manifest
            .activation_events
            .iter()
            .any(|event| event == &invocation.activation)
        {
            return Err(CoreError::InvalidArgument {
                reason: format!(
                    "激活点「{}」不在插件「{}」声明的 {:?} 里",
                    invocation.activation, manifest.id, manifest.activation_events
                ),
            });
        }
        if let Some(input) = &invocation.input_text {
            if input.len() > MAX_INPUT_TEXT_BYTES {
                return Err(CoreError::InvalidArgument {
                    reason: format!("inputText 超过 {MAX_INPUT_TEXT_BYTES} 字节"),
                });
            }
        }
        match manifest.entry_kind {
            PluginEntryKind::Declarative => {
                self.run_declarative(&installation, &manifest, invocation)
            }
            PluginEntryKind::Wasm => self.run_wasm(&installation, &manifest, invocation),
        }
    }

    // -------------------------------------------------------------- 内部

    fn find_installation(&self, plugin_id: &str) -> Result<Option<PluginInstallation>> {
        let mut statement = self.conn.prepare(
            "SELECT id, version, manifest_version, manifest_json, templates_json, entry_kind, \
             entry_file, package_hash, install_rel_path, enabled, last_run_at, last_error, \
             installed_at, updated_at FROM plugin_installations WHERE id = ?1",
        )?;
        let row = statement
            .query_row(params![plugin_id], raw_installation)
            .optional()?;
        row.map(|row| self.installation_from_raw(row)).transpose()
    }

    fn installation_from_raw(&self, row: RawInstallation) -> Result<PluginInstallation> {
        let manifest: PluginManifest = serde_json::from_str(&row.manifest_json).map_err(|error| {
            CoreError::CorruptedData {
                message: format!("插件「{}」的缓存清单坏了：{error}", row.id),
            }
        })?;
        let templates: Vec<PluginTemplate> =
            serde_json::from_str(&row.templates_json).map_err(|error| {
                CoreError::CorruptedData {
                    message: format!("插件「{}」的缓存模板坏了：{error}", row.id),
                }
            })?;
        let grants = self.plugin_grants(&row.id)?;
        let entry_kind =
            PluginEntryKind::from_wire(&row.entry_kind).ok_or_else(|| CoreError::CorruptedData {
                message: format!("插件「{}」的 entry_kind 不认识：{}", row.id, row.entry_kind),
            })?;
        if grants.is_empty() && !manifest.permissions.is_empty() {
            return Err(CoreError::CorruptedData {
                message: format!("插件「{}」的授权行不见了", row.id),
            });
        }
        Ok(PluginInstallation {
            plugin_id: row.id,
            name: manifest.name,
            version: row.version,
            manifest_version: row.manifest_version,
            entry_kind,
            api_range: manifest.api_range,
            enabled: row.enabled != 0,
            permissions: grants,
            requested_permissions: manifest.permissions.clone(),
            supported_platforms: manifest.supported_platforms.clone(),
            activation_events: manifest.activation_events.clone(),
            templates,
            package_hash: row.package_hash,
            installed_at: support::parse_iso(&row.installed_at)?,
            updated_at: support::parse_iso(&row.updated_at)?,
            last_run_at: row
                .last_run_at
                .as_deref()
                .map(support::parse_iso)
                .transpose()?,
            last_error: row.last_error,
        })
    }

    fn cached_manifest(&self, installation: &PluginInstallation) -> Result<PluginManifest> {
        let json: String = self
            .conn
            .query_row(
                "SELECT manifest_json FROM plugin_installations WHERE id = ?1",
                params![installation.plugin_id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| CoreError::NotFound {
                entity: "插件",
                id: installation.plugin_id.clone(),
            })?;
        serde_json::from_str(&json).map_err(|error| CoreError::CorruptedData {
            message: format!("插件「{}」的缓存清单坏了：{error}", installation.plugin_id),
        })
    }

    fn install_rel_path(&self, plugin_id: &str) -> Result<String> {
        let path: Option<String> = self
            .conn
            .query_row(
                "SELECT install_rel_path FROM plugin_installations WHERE id = ?1",
                params![plugin_id],
                |row| row.get(0),
            )
            .optional()?;
        path.ok_or_else(|| CoreError::NotFound {
            entity: "插件",
            id: plugin_id.to_owned(),
        })
    }

    fn run_declarative(
        &mut self,
        installation: &PluginInstallation,
        manifest_data: &PluginManifest,
        invocation: &PluginInvocation,
    ) -> Result<PluginRunResult> {
        let started = std::time::Instant::now();
        let root = self.root_dir()?.to_path_buf();
        let rel = self.install_rel_path(&installation.plugin_id)?;
        let entry = package::read_entry_verified(&root, &rel, manifest_data)?;
        let templates = template::parse_declarative(&manifest_data.id, &entry)?;
        let template_id = invocation.template_id.as_deref().ok_or_else(|| {
            CoreError::InvalidArgument {
                reason: "声明式插件需要 templateId".to_owned(),
            }
        })?;
        let template = templates
            .iter()
            .find(|template| template.id == template_id)
            .ok_or_else(|| CoreError::NotFound {
                entity: "插件模板",
                id: format!("{} / {template_id}", installation.plugin_id),
            })?;
        let run_id = support::new_id("plg");
        let result = match template.kind.as_str() {
            "style" => {
                if invocation.activation == "export.start" {
                    return Err(CoreError::InvalidArgument {
                        reason: "风格模板不能由 export.start 激活".to_owned(),
                    });
                }
                let profile = invocation.style_profile.as_ref().ok_or_else(|| {
                    CoreError::InvalidArgument {
                        reason: "风格模板需要 styleProfile（#114 的 StyleProfile 形状）".to_owned(),
                    }
                })?;
                let output = template::render_style(template, profile)?;
                if output.len() > MAX_OUTPUT_BYTES {
                    return Err(CoreError::PluginBudgetExceeded {
                        reason: format!("风格模板输出超过 {MAX_OUTPUT_BYTES} 字节"),
                    });
                }
                PluginRunResult {
                    run_id: run_id.clone(),
                    plugin_id: installation.plugin_id.clone(),
                    activation: invocation.activation.clone(),
                    status: "succeeded".to_owned(),
                    output: Some(output),
                    artifacts: Vec::new(),
                    derived_contents: Vec::new(),
                    logs: vec!["声明式风格模板：只做白名单占位符替换，没有执行代码".to_owned()],
                    warnings: Vec::new(),
                    error_code: None,
                    error_message: None,
                    duration_ms: started.elapsed().as_millis() as i64,
                    instructions_used: None,
                }
            }
            "export" => {
                if invocation.activation != "export.start" {
                    return Err(CoreError::InvalidArgument {
                        reason: "导出模板只能由 export.start 激活".to_owned(),
                    });
                }
                self.require_permission(installation, PERM_EXPORT)?;
                let requested_format = invocation
                    .format
                    .as_deref()
                    .unwrap_or(template.format.as_str());
                if requested_format != template.format {
                    return Err(CoreError::InvalidArgument {
                        reason: format!(
                            "模板「{template_id}」的格式是 {}，请求的是 {requested_format}",
                            template.format
                        ),
                    });
                }
                let entries = self.export_entries(installation, invocation)?;
                if entries.is_empty() {
                    return Err(CoreError::NotFound {
                        entity: "可导出的日记",
                        id: "指定范围里没有当前版本；请先在日记里选一版为当前".to_owned(),
                    });
                }
                let mut output = String::new();
                for (index, entry) in entries.iter().enumerate() {
                    let rendered =
                        template::render_export(template, &entry.day_key, &entry.version_id, &entry.body)?;
                    if index > 0 {
                        output.push('\n');
                    }
                    output.push_str(&rendered);
                    if output.len() > MAX_OUTPUT_BYTES {
                        return Err(CoreError::PluginBudgetExceeded {
                            reason: format!(
                                "导出内容超过 {MAX_OUTPUT_BYTES} 字节；请缩小日期范围或改用纯文本模板"
                            ),
                        });
                    }
                }
                let file_name = format!(
                    "{}-{}.{}",
                    sanitize_id(&installation.plugin_id),
                    entries.first().map(|entry| entry.day_key.as_str()).unwrap_or("export"),
                    if template.format == "plain_text" { "txt" } else { "md" }
                );
                let media_type = if template.format == "plain_text" {
                    "text/plain"
                } else {
                    "text/markdown"
                };
                let artifacts = self.write_artifacts(
                    &installation.plugin_id,
                    &run_id,
                    vec![(file_name, media_type.to_owned(), output.clone().into_bytes())],
                )?;
                PluginRunResult {
                    run_id: run_id.clone(),
                    plugin_id: installation.plugin_id.clone(),
                    activation: invocation.activation.clone(),
                    status: "succeeded".to_owned(),
                    output: Some(output),
                    artifacts,
                    derived_contents: Vec::new(),
                    logs: vec![format!(
                        "声明式导出模板：{} 天，没有执行代码",
                        entries.len()
                    )],
                    warnings: Vec::new(),
                    error_code: None,
                    error_message: None,
                    duration_ms: started.elapsed().as_millis() as i64,
                    instructions_used: None,
                }
            }
            other => {
                return Err(CoreError::PluginManifestInvalid {
                    reason: format!("模板「{template_id}」的 kind 不认识：{other}"),
                })
            }
        };
        self.persist_run(&installation.plugin_id, &result, None)?;
        Ok(result)
    }

    fn run_wasm(
        &mut self,
        installation: &PluginInstallation,
        manifest_data: &PluginManifest,
        invocation: &PluginInvocation,
    ) -> Result<PluginRunResult> {
        let started = std::time::Instant::now();
        let root = self.root_dir()?.to_path_buf();
        let rel = self.install_rel_path(&installation.plugin_id)?;
        let entry = package::read_entry_verified(&root, &rel, manifest_data)?;
        if invocation.activation == "export.start" {
            self.require_permission(installation, PERM_EXPORT)?;
        }
        let permissions: HashSet<String> = installation.permissions.iter().cloned().collect();
        let selected = if permissions.contains(PERM_READ_SELECTED) {
            self.load_selected_versions(&invocation.selected_version_ids)?
        } else {
            Vec::new()
        };
        let range = if permissions.contains(PERM_READ_RANGE) {
            self.load_range_versions(invocation.from_day_key.as_deref(), invocation.to_day_key.as_deref())?
        } else {
            Vec::new()
        };
        let (sources, assets) = if permissions.contains(PERM_READ_SELECTED) {
            self.load_selected_materials(&root, &invocation.selected_capture_ids)?
        } else {
            (HashMap::new(), HashMap::new())
        };
        let providers = self.prepare_providers(manifest_data, &permissions)?;
        let context_json = serde_json::to_string(&json!({
            "abi": 1,
            "pluginId": installation.plugin_id,
            "activation": invocation.activation,
            "platform": manifest::host_platform(),
            "apiVersion": PLUGIN_API_VERSION,
            "format": invocation.format,
            "selectedVersionIds": invocation.selected_version_ids,
            "selectedCaptureIds": invocation.selected_capture_ids,
            "fromDayKey": invocation.from_day_key,
            "toDayKey": invocation.to_day_key,
            "inputText": invocation.input_text,
            "limits": {
                "fuel": sandbox::FUEL_BUDGET,
                "memoryBytes": sandbox::MEMORY_LIMIT_BYTES,
                "wallTimeoutMs": sandbox::WALL_TIMEOUT_MS,
                "maxMessageBytes": sandbox::MAX_MESSAGE_BYTES,
                "maxStateBytes": sandbox::MAX_STATE_BYTES,
            }
        }))?;
        let state = self.load_plugin_state(&installation.plugin_id)?;
        let request = sandbox::SandboxRequest {
            permissions,
            selected,
            range,
            sources,
            assets,
            providers,
            transport: self.transport_arc(),
            state,
            context_json,
        };
        let outcome = sandbox::run(&entry, request);
        for failure in &outcome.provider_failures {
            let _ = provider::mark_provider_verdict_failed(
                self,
                &failure.provider_id,
                &failure.error_code,
            );
        }
        let run_id = support::new_id("plg");
        let artifacts = if outcome.status == "succeeded" {
            let pending: Vec<(String, String, Vec<u8>)> = outcome
                .artifacts
                .iter()
                .map(|artifact| {
                    (
                        artifact.file_name.clone(),
                        artifact.media_type.clone(),
                        artifact.bytes.clone(),
                    )
                })
                .collect();
            self.write_artifacts(&installation.plugin_id, &run_id, pending)?
        } else {
            Vec::new()
        };
        let result = PluginRunResult {
            run_id,
            plugin_id: installation.plugin_id.clone(),
            activation: invocation.activation.clone(),
            status: outcome.status.to_owned(),
            output: outcome.output,
            artifacts,
            derived_contents: outcome.derived,
            logs: outcome.logs,
            warnings: outcome.warnings,
            error_code: outcome.error_code,
            error_message: outcome.error_message,
            duration_ms: started.elapsed().as_millis() as i64,
            instructions_used: outcome.instructions_used,
        };
        let next_state = if result.status == "succeeded" {
            Some(outcome.state)
        } else {
            None
        };
        self.persist_run(&installation.plugin_id, &result, next_state)?;
        Ok(result)
    }

    fn require_permission(
        &self,
        installation: &PluginInstallation,
        permission: &str,
    ) -> Result<()> {
        if installation.permissions.iter().any(|item| item == permission) {
            Ok(())
        } else {
            Err(CoreError::PluginPermissionDenied {
                reason: format!(
                    "插件「{}」没有授予权限「{permission}」；需要先在权限页授权",
                    installation.plugin_id
                ),
            })
        }
    }

    fn export_entries(
        &self,
        installation: &PluginInstallation,
        invocation: &PluginInvocation,
    ) -> Result<Vec<sandbox::DiaryEntry>> {
        if !invocation.selected_version_ids.is_empty() {
            self.require_permission(installation, PERM_READ_SELECTED)?;
            self.load_selected_versions(&invocation.selected_version_ids)
        } else {
            self.require_permission(installation, PERM_READ_RANGE)?;
            self.load_range_versions(
                invocation.from_day_key.as_deref(),
                invocation.to_day_key.as_deref(),
            )
        }
    }

    fn load_selected_versions(&self, ids: &[String]) -> Result<Vec<sandbox::DiaryEntry>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        if ids.len() > MAX_SELECTED_VERSIONS {
            return Err(CoreError::InvalidArgument {
                reason: format!("一次最多选 {MAX_SELECTED_VERSIONS} 个日记版本"),
            });
        }
        let mut entries = Vec::new();
        let mut total = 0;
        for id in ids {
            let row: Option<(String, String, String)> = self
                .conn
                .query_row(
                    "SELECT id, day_key, body FROM diary_versions WHERE id = ?1",
                    params![id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .optional()?;
            let Some((version_id, day_key, body)) = row else {
                return Err(CoreError::NotFound {
                    entity: "日记版本",
                    id: id.clone(),
                });
            };
            let body = truncate(body, MAX_PER_BODY_BYTES);
            total += body.len();
            if total > MAX_PRELOAD_BYTES {
                return Err(CoreError::PluginBudgetExceeded {
                    reason: format!("选中的日记正文合计超过 {MAX_PRELOAD_BYTES} 字节"),
                });
            }
            entries.push(sandbox::DiaryEntry {
                version_id,
                day_key,
                body,
            });
        }
        Ok(entries)
    }

    fn load_range_versions(
        &self,
        from: Option<&str>,
        to: Option<&str>,
    ) -> Result<Vec<sandbox::DiaryEntry>> {
        let (from, to) = match (from, to) {
            (Some(from), Some(to)) => (from, to),
            _ => {
                return Err(CoreError::InvalidArgument {
                    reason: "读日期范围需要同时给 fromDayKey 与 toDayKey".to_owned(),
                })
            }
        };
        let from_date = parse_day_key(from)?;
        let to_date = parse_day_key(to)?;
        if from_date > to_date {
            return Err(CoreError::InvalidArgument {
                reason: format!("fromDayKey（{from}）不能晚于 toDayKey（{to}）"),
            });
        }
        let days = (to_date - from_date).num_days() + 1;
        if days > MAX_RANGE_DAYS {
            return Err(CoreError::InvalidArgument {
                reason: format!("日期范围 {days} 天，超过上限 {MAX_RANGE_DAYS} 天"),
            });
        }
        let mut statement = self.conn.prepare(
            "SELECT v.id, v.day_key, v.body FROM diary_days d \
             JOIN diary_versions v ON v.id = d.current_version_id \
             WHERE d.day_key >= ?1 AND d.day_key <= ?2 ORDER BY d.day_key",
        )?;
        let mut entries = Vec::new();
        let mut total = 0;
        let rows = statement.query_map(params![from, to], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?))
        })?;
        for row in rows {
            let (version_id, day_key, body) = row?;
            let body = truncate(body, MAX_PER_BODY_BYTES);
            total += body.len();
            if total > MAX_PRELOAD_BYTES {
                return Err(CoreError::PluginBudgetExceeded {
                    reason: format!("日期范围内的日记正文合计超过 {MAX_PRELOAD_BYTES} 字节"),
                });
            }
            entries.push(sandbox::DiaryEntry {
                version_id,
                day_key,
                body,
            });
        }
        Ok(entries)
    }

    fn load_selected_materials(
        &self,
        root: &Path,
        capture_ids: &[String],
    ) -> Result<(HashMap<String, sandbox::SourceEntry>, HashMap<String, PathBuf>)> {
        if capture_ids.len() > MAX_SELECTED_CAPTURES {
            return Err(CoreError::InvalidArgument {
                reason: format!("一次最多选 {MAX_SELECTED_CAPTURES} 条记录"),
            });
        }
        let mut sources = HashMap::new();
        let mut assets = HashMap::new();
        let mut total = 0;
        for capture_id in capture_ids {
            let exists: i64 = self.conn.query_row(
                "SELECT COUNT(*) FROM captures WHERE id = ?1 AND state != 'trashed'",
                params![capture_id],
                |row| row.get(0),
            )?;
            if exists == 0 {
                return Err(CoreError::NotFound {
                    entity: "记录",
                    id: capture_id.clone(),
                });
            }
            let draft: String = self.conn.query_row(
                "SELECT draft_text FROM captures WHERE id = ?1",
                params![capture_id],
                |row| row.get(0),
            )?;
            let mut statement = self.conn.prepare(
                "SELECT s.source_id, s.current_revision_id, \
                  COALESCE((SELECT e.text FROM extracted_contents e \
                            WHERE e.source_revision_id = s.current_revision_id \
                            AND e.status IN ('ready', 'partial') LIMIT 1), ''), \
                  r.asset_id \
                 FROM source_items s LEFT JOIN source_revisions r ON r.revision_id = s.current_revision_id \
                 WHERE s.capture_id = ?1 ORDER BY s.position",
            )?;
            let rows = statement.query_map(params![capture_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                ))
            })?;
            for row in rows {
                let (source_id, revision_id, extracted, asset_id) = row?;
                let mut text = if extracted.trim().is_empty() {
                    draft.clone()
                } else {
                    extracted
                };
                text = truncate(text, MAX_SOURCE_BYTES);
                total += text.len();
                if total > MAX_PRELOAD_BYTES {
                    return Err(CoreError::PluginBudgetExceeded {
                        reason: format!("选中的材料文本合计超过 {MAX_PRELOAD_BYTES} 字节"),
                    });
                }
                if let Some(asset_id) = asset_id {
                    if let Some(object_ref) = self.asset_object_ref(&asset_id)? {
                        let path = root.join(&object_ref);
                        assets.insert(asset_id, path);
                    }
                }
                let _ = revision_id;
                sources.insert(source_id, sandbox::SourceEntry { text });
            }
        }
        Ok((sources, assets))
    }

    fn asset_object_ref(&self, asset_id: &str) -> Result<Option<String>> {
        let object_ref: Option<String> = self
            .conn
            .query_row(
                "SELECT object_ref FROM assets WHERE id = ?1",
                params![asset_id],
                |row| row.get(0),
            )
            .optional()?;
        Ok(object_ref.filter(|value| {
            manifest::valid_relative_path(value)
                && value.starts_with("blobs/")
                && !value.contains("..")
        }))
    }

    fn prepare_providers(
        &self,
        manifest_data: &PluginManifest,
        permissions: &HashSet<String>,
    ) -> Result<HashMap<String, sandbox::PreparedProvider>> {
        let mut providers = HashMap::new();
        for permission in &manifest_data.permissions {
            let Some(provider_id) = manifest::provider_id_for_permission(permission) else {
                continue;
            };
            if !permissions.contains(permission) {
                continue;
            }
            let prepared = match provider::load_provider(&self.conn, provider_id) {
                Ok(provider) => {
                    let gate = provider::capability_gate_for(
                        self,
                        ProviderCapability::GenerateText,
                        Some(provider_id),
                    )?;
                    if !gate.ready {
                        sandbox::PreparedProvider {
                            provider: Some(provider),
                            secret: None,
                            ready: false,
                            reason: format!("服务「{provider_id}」当前不可用：{}", gate.reason),
                        }
                    } else if let Some(secret) = provider::provider_secret(self, &provider)? {
                        sandbox::PreparedProvider {
                            provider: Some(provider),
                            secret: Some(secret),
                            ready: true,
                            reason: String::new(),
                        }
                    } else {
                        sandbox::PreparedProvider {
                            provider: Some(provider),
                            secret: None,
                            ready: false,
                            reason: format!("服务「{provider_id}」没有可用密钥"),
                        }
                    }
                }
                Err(CoreError::NotFound { .. }) => sandbox::PreparedProvider {
                    provider: None,
                    secret: None,
                    ready: false,
                    reason: format!(
                        "服务「{provider_id}」还没有配置；请先在设置里配置并做连接测试（#66）"
                    ),
                },
                Err(error) => return Err(error),
            };
            providers.insert(provider_id.to_owned(), prepared);
        }
        Ok(providers)
    }

    fn load_plugin_state(&self, plugin_id: &str) -> Result<BTreeMap<String, String>> {
        let json: Option<String> = self
            .conn
            .query_row(
                "SELECT state_json FROM plugin_installations WHERE id = ?1",
                params![plugin_id],
                |row| row.get(0),
            )
            .optional()?;
        let Some(json) = json else {
            return Err(CoreError::NotFound {
                entity: "插件",
                id: plugin_id.to_owned(),
            });
        };
        serde_json::from_str(&json).map_err(|error| CoreError::CorruptedData {
            message: format!("插件「{plugin_id}」的私有状态坏了：{error}"),
        })
    }

    fn persist_run(
        &mut self,
        plugin_id: &str,
        result: &PluginRunResult,
        state: Option<BTreeMap<String, String>>,
    ) -> Result<()> {
        let audit = json!({
            "runId": result.run_id,
            "activation": result.activation,
            "status": result.status,
            "errorCode": result.error_code,
            "errorMessage": result.error_message,
            "durationMs": result.duration_ms,
            "instructionsUsed": result.instructions_used,
            "artifacts": result.artifacts.iter().map(|artifact| json!({
                "fileName": artifact.file_name,
                "mediaType": artifact.media_type,
                "byteSize": artifact.byte_size,
                "sha256": artifact.sha256,
                "path": artifact.path,
            })).collect::<Vec<_>>(),
            "derived": &result.derived_contents,
            "derivedCount": result.derived_contents.len(),
            "at": support::to_iso(support::now()),
        });
        let audit_json = serde_json::to_string(&audit)?;
        let now = support::to_iso(support::now());
        let state_json = state.map(|state| serde_json::to_string(&state)).transpose()?;
        self.conn.execute(
            "UPDATE plugin_installations SET last_run_at = ?1, last_run_json = ?2, \
             last_error = ?3, updated_at = ?1, state_json = COALESCE(?4, state_json) WHERE id = ?5",
            params![
                now,
                audit_json,
                result.error_message,
                state_json,
                plugin_id
            ],
        )?;
        Ok(())
    }

    fn write_artifacts(
        &self,
        plugin_id: &str,
        run_id: &str,
        pending: Vec<(String, String, Vec<u8>)>,
    ) -> Result<Vec<PluginArtifact>> {
        let root = self.root.as_deref();
        let mut artifacts = Vec::new();
        for (file_name, media_type, bytes) in pending {
            if bytes.len() > MAX_OUTPUT_BYTES {
                return Err(CoreError::PluginBudgetExceeded {
                    reason: format!("导出物「{file_name}」超过 {MAX_OUTPUT_BYTES} 字节"),
                });
            }
            let file_name = sanitize_file_name(&file_name).ok_or_else(|| {
                CoreError::InvalidArgument {
                    reason: format!("导出文件名「{file_name}」不合法"),
                }
            })?;
            let sha256 = package::sha256_hex(&bytes);
            let content = String::from_utf8(bytes.clone()).ok();
            let path = if let Some(root) = root {
                let dir = root
                    .join("plugin_exports")
                    .join(sanitize_id(plugin_id))
                    .join(run_id);
                std::fs::create_dir_all(&dir)?;
                let target = dir.join(&file_name);
                let staging = dir.join(format!(".{file_name}.part"));
                std::fs::write(&staging, &bytes)?;
                std::fs::rename(&staging, &target)?;
                Some(package::display_path(&target))
            } else {
                None
            };
            artifacts.push(PluginArtifact {
                file_name,
                media_type,
                byte_size: bytes.len() as i64,
                sha256,
                path,
                content,
            });
        }
        Ok(artifacts)
    }

    fn transport_arc(&self) -> std::sync::Arc<dyn provider::transport::Transport> {
        self.transport.clone()
    }
}

#[derive(Debug)]
struct RawInstallation {
    id: String,
    version: String,
    manifest_version: i64,
    manifest_json: String,
    templates_json: String,
    entry_kind: String,
    #[allow(dead_code)]
    entry_file: String,
    package_hash: String,
    #[allow(dead_code)]
    install_rel_path: String,
    enabled: i64,
    last_run_at: Option<String>,
    last_error: Option<String>,
    installed_at: String,
    updated_at: String,
}

fn raw_installation(row: &rusqlite::Row<'_>) -> rusqlite::Result<RawInstallation> {
    Ok(RawInstallation {
        id: row.get(0)?,
        version: row.get(1)?,
        manifest_version: row.get(2)?,
        manifest_json: row.get(3)?,
        templates_json: row.get(4)?,
        entry_kind: row.get(5)?,
        entry_file: row.get(6)?,
        package_hash: row.get(7)?,
        install_rel_path: row.get(8)?,
        enabled: row.get(9)?,
        last_run_at: row.get(10)?,
        last_error: row.get(11)?,
        installed_at: row.get(12)?,
        updated_at: row.get(13)?,
    })
}

fn parse_day_key(value: &str) -> Result<NaiveDate> {
    NaiveDate::parse_from_str(value, "%Y-%m-%d").map_err(|_| CoreError::InvalidArgument {
        reason: format!("dayKey「{value}」不是 YYYY-MM-DD"),
    })
}

fn truncate(mut value: String, max: usize) -> String {
    if value.len() <= max {
        return value;
    }
    let mut boundary = max;
    while boundary > 0 && !value.is_char_boundary(boundary) {
        boundary -= 1;
    }
    value.truncate(boundary);
    value
}

fn sanitize_id(value: &str) -> String {
    let filtered: String = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_lowercase() || ch.is_ascii_digit() || matches!(ch, '.' | '_' | '-') {
                ch
            } else {
                '_'
            }
        })
        .collect();
    if filtered.is_empty() {
        "plugin".to_owned()
    } else {
        filtered
    }
}

fn sanitize_file_name(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed.len() > 120 || trimmed == "." || trimmed == ".." {
        return None;
    }
    if trimmed.contains('/')
        || trimmed.contains('\\')
        || trimmed.contains('\0')
        || trimmed.contains(':')
    {
        return None;
    }
    if !trimmed
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-'))
    {
        return None;
    }
    Some(trimmed.to_owned())
}

//! 设备可写的核心配置文件 `core-config.json`（与 `library.sqlite` 同目录）。
//!
//! # 为什么需要它
//!
//! 鸿蒙（以及其它产品包）里**应用设不了环境变量**，而嵌入器（`DIARY_MODEL_DIR` /
//! `DIARY_ORT_DYLIB`）与转写引擎（`DIARY_ASR_ENGINE_LIB` / `DIARY_ASR_MODEL_DIR`）的
//! 路径过去只能从环境变量读。于是产品 HAP 里语义检索永远停在「未配置」，转写只能靠
//! `providers.save` 一行的 `model`。这个文件就是给产品包的、**不需要环境变量**的
//! 配置通道：四个字段全部可选，谁配谁生效。
//!
//! # 为什么放在资料库旁边
//!
//! 选 `<root>/core-config.json`（root = `Core::open` 收到的库文件所在目录）而不是
//! `config/core.json`：
//!
//! - 一个应用沙箱只有一个资料库，库与配置**同目录、同一次搬运**，不需要先建子目录
//!   （少一层「目录不存在」的失败面）；
//! - 排障时 `ls <filesDir>` 一眼能看到配置与它旁边的库，路径同源，不会出现「库是
//!   这个目录的、配置是另一个目录的」；
//! - 备份/迁移只盯一个目录：不会出现「库搬走了、配置没搬」的半套状态。
//!
//! # 优先级（逐字段，先命中先用）
//!
//! **环境变量 → 本文件 → provider 行（仅转写）→ 随包默认**。
//!
//! - 环境变量在最前是刻意的：探针/CI 靠它临时换模型与动态库，设备上有了配置文件
//!   也不能让它失效；
//! - 逐字段而不是「整份文件二选一」：`modelDir` 用环境变量、`ortDylib` 用文件这种
//!   混搭是合法的，报错信息里会写清每个字段的来源；
//! - 转写的 `provider` 是 `providers.model` 那一行——它是**按服务**的显式配置，
//!   排在全局文件之后是本模块的取舍（见 `local_engine::EngineConfig::resolve_with`）。
//!
//! # 失败不致命
//!
//! 文件不存在 = 完全没有这档配置，行为与加这个文件之前**逐位一致**；读不到、JSON 坏了、
//! 字段类型不对 = 这一档整体不生效，回落下一档，原因写进 `core.info.runtime_config.file.error`。
//! 未知字段**不忽略得无声无息**：收进 `ignoredFields` 一并报出来（拼错字段时能看到）。
//!
//! 配置只在 `Core::open` 时读一次：改了文件要重新开库才生效（与 `DIARY_*` 环境变量
//! 的语义一致——它们在 `open` 那一刻被读走）。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::provider::local_engine;
use crate::semantic::EmbedderSlot;
use crate::Core;

/// 配置文件名。放在资料库（`library.sqlite`）同一个目录里。
pub const CONFIG_FILE_NAME: &str = "core-config.json";

/// 一个路径字段「从哪来」。报告里逐字段给出，`configSource` 只是它的摘要。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ConfigOrigin {
    /// 环境变量（探针/CI/开发机）。
    Env,
    /// 本文件。
    File,
    /// `providers` 表里那一行的 `model`（只有转写有这一档）。
    Provider,
    /// 随包默认位置（可执行文件旁边的 `models/asr/...`）。
    Bundled,
    /// 没有显式路径：按平台库名交给系统解析（只有引擎动态库有这一档）。
    Platform,
    /// 没有任何来源。
    #[default]
    Absent,
}

impl ConfigOrigin {
    pub fn wire(self) -> &'static str {
        match self {
            Self::Env => "env",
            Self::File => "file",
            Self::Provider => "provider",
            Self::Bundled => "bundled",
            Self::Platform => "platform",
            Self::Absent => "absent",
        }
    }
}

/// `core-config.json` 的内容。四个字段全部可选，写错大小写/拼错不会被当成配置。
///
/// 字段名是 camelCase（与产品侧配置习惯一致），同时接受 snake_case 别名，避免
/// 「生成配置的脚本用了另一种命名」这种无意义的失败。
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct CoreConfigFile {
    /// 嵌入模型目录（含 `model_quantized.onnx` / `tokenizer.json`）。
    #[serde(default, rename = "modelDir", alias = "model_dir")]
    pub model_dir: Option<String>,
    /// ONNX Runtime 动态库的路径（`ort` 的 `load-dynamic` 用它）。
    #[serde(default, rename = "ortDylib", alias = "ort_dylib")]
    pub ort_dylib: Option<String>,
    /// 本机转写引擎动态库的路径（sherpa-onnx 的 C API）。
    #[serde(default, rename = "asrEngineLib", alias = "asr_engine_lib")]
    pub asr_engine_lib: Option<String>,
    /// 本机转写模型目录（含 `model.int8.onnx` 与 `tokens.txt`）。
    #[serde(default, rename = "asrModelDir", alias = "asr_model_dir")]
    pub asr_model_dir: Option<String>,
    /// 不认识的字段：不静默丢掉，收进报告（拼错的字段名要看得见）。
    #[serde(default, flatten)]
    pub unknown: BTreeMap<String, serde_json::Value>,
}

impl CoreConfigFile {
    pub fn model_dir_path(&self) -> Option<PathBuf> {
        path_of(&self.model_dir)
    }

    pub fn ort_dylib_path(&self) -> Option<PathBuf> {
        path_of(&self.ort_dylib)
    }

    pub fn asr_engine_lib_path(&self) -> Option<PathBuf> {
        path_of(&self.asr_engine_lib)
    }

    pub fn asr_model_dir_path(&self) -> Option<PathBuf> {
        path_of(&self.asr_model_dir)
    }
}

/// 空串/全空白当成「没配」：脚本里留空比删字段更常见，不该被解释成一个叫 `""` 的路径。
fn path_of(value: &Option<String>) -> Option<PathBuf> {
    value
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

/// `core.info.runtime_config.file`：配置文件本身的状态（不是配置项的状态）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigFileReport {
    /// 期望的位置；内存库（没有根目录）为 `None`。
    pub path: Option<String>,
    /// 文件**存在且读到了**（解析失败也算存在——它就在那儿，只是内容不对）。
    pub present: bool,
    /// 读不了/解析失败的原因。`None` = 没出问题。
    pub error: Option<String>,
    /// 文件里不认识的字段名（拼错字段时就是它）。
    pub ignored_fields: Vec<String>,
}

/// 一次加载的结果：能用就用，不能用也把「为什么」带出来。
#[derive(Debug, Clone, Default)]
pub struct LoadedCoreConfig {
    pub file: Option<CoreConfigFile>,
    pub report: ConfigFileReport,
}

/// 从资料库根目录读配置文件。**任何失败都不返回 `Err`**——配置是可选的一档。
pub fn load(root: Option<&Path>) -> LoadedCoreConfig {
    let Some(root) = root else {
        return LoadedCoreConfig::default();
    };
    let path = root.join(CONFIG_FILE_NAME);
    let mut report = ConfigFileReport {
        path: Some(path.display().to_string()),
        present: false,
        error: None,
        ignored_fields: Vec::new(),
    };
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => {
            report.present = true;
            text
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return LoadedCoreConfig {
                file: None,
                report,
            }
        }
        Err(error) => {
            report.error = Some(format!("读不到 {}：{error}", path.display()));
            return LoadedCoreConfig {
                file: None,
                report,
            };
        }
    };
    match serde_json::from_str::<CoreConfigFile>(&text) {
        Ok(file) => {
            report.ignored_fields = file.unknown.keys().cloned().collect();
            LoadedCoreConfig {
                file: Some(file),
                report,
            }
        }
        Err(error) => {
            report.error = Some(format!("{} 解析失败：{error}", path.display()));
            LoadedCoreConfig {
                file: None,
                report,
            }
        }
    }
}

// ---------------------------------------------------------------- core.info 的报告

/// `core.info.runtime_config`：两条能力各自「从哪读到什么、现在到哪一步」。
///
/// 这一个对象的键是 **camelCase**：它与 `core-config.json` 的字段同名同形
/// （`modelDir` / `ortDylib` / `asrEngineLib` / `asrModelDir`），配置里写什么、
/// 报告里就回什么，不需要在脑子里做一次 snake/camel 转换。`core.info` 的外层键
/// 仍然沿用既有的 snake_case（`data_schema_version` …）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeConfigReport {
    pub file: ConfigFileReport,
    pub semantic: SemanticConfigReport,
    pub transcription: TranscriptionConfigReport,
}

/// 语义（嵌入器）的配置状态。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SemanticConfigReport {
    /// `absent` / `configured`（配了、文件在、还没加载）/ `ready`（已加载）/ `invalid`。
    pub state: String,
    /// `env` / `file` / `absent`：整条链的摘要（任一字段来自文件就是 `file`）。
    pub config_source: String,
    pub model_dir: Option<String>,
    pub model_dir_source: String,
    pub ort_dylib: Option<String>,
    pub ort_dylib_source: String,
    /// 已加载时的模型版本（`bge-small-zh-v1.5-ours@<sha256 前 12 位>`）。
    pub model_version: Option<String>,
    /// 不可用时的原因（缺哪个文件、加载失败原文）。
    pub reason: Option<String>,
}

/// 转写的配置状态。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptionConfigReport {
    /// `absent`（没有任何模型目录）/ `configured`（解析到了但还不完整）/ `ready`。
    pub state: String,
    /// 模型目录的来源：`env` / `file` / `provider` / `bundled` / `absent`。
    pub config_source: String,
    pub model_dir: Option<String>,
    pub model_dir_source: String,
    pub engine_lib: Option<String>,
    /// `env` / `file` / `platform`（没有显式路径，按库名交给系统解析）/ `absent`。
    pub engine_lib_source: String,
    /// 实际选中的本机转写服务 id（`local_asr_ohos`）；内建引擎为 `null`。
    pub provider_id: Option<String>,
    pub reason: Option<String>,
}

impl Core {
    /// 两条能力现在的配置与就绪状态。**只 stat、不加载模型**，可以随时调。
    pub fn runtime_config(&self) -> RuntimeConfigReport {
        RuntimeConfigReport {
            file: self.config_report.clone(),
            semantic: self.semantic_config_report(),
            transcription: self.transcription_config_report(),
        }
    }

    fn semantic_config_report(&self) -> SemanticConfigReport {
        let resolution = &self.semantic.resolution;
        let (state, model_version, reason) = match &self.semantic.slot {
            EmbedderSlot::NotConfigured => ("absent".to_owned(), None, None),
            EmbedderSlot::Configured(_) => ("configured".to_owned(), None, None),
            EmbedderSlot::Ready(embedder) => (
                "ready".to_owned(),
                Some(embedder.model_version().to_owned()),
                None,
            ),
            EmbedderSlot::Failed(reason) => ("invalid".to_owned(), None, Some(reason.clone())),
        };
        SemanticConfigReport {
            state,
            config_source: resolution.source.wire().to_owned(),
            model_dir: resolution
                .model_dir
                .as_ref()
                .map(|path| path.display().to_string()),
            model_dir_source: resolution.model_dir_origin.wire().to_owned(),
            ort_dylib: resolution
                .ort_dylib
                .as_ref()
                .map(|path| path.display().to_string()),
            ort_dylib_source: resolution.ort_dylib_origin.wire().to_owned(),
            model_version,
            reason,
        }
    }

    fn transcription_config_report(&self) -> TranscriptionConfigReport {
        let choice = crate::provider::local_engine::transcription_choice(self).ok();
        let provider_id = choice.as_ref().and_then(|choice| choice.provider_id.clone());
        let override_dir = choice
            .as_ref()
            .and_then(|choice| choice.model_override.as_deref());
        let (resolved, origins) =
            local_engine::EngineConfig::resolve_report(override_dir, self.config_file());
        let (state, model_dir, reason) = match resolved {
            Err(error) => (
                "absent".to_owned(),
                None,
                Some(error.to_string()),
            ),
            Ok(config) => {
                let readiness = self.asr_engine().readiness(&config);
                let dir = Some(config.model_dir.display().to_string());
                if readiness.ready {
                    ("ready".to_owned(), dir, None)
                } else {
                    let reason = match readiness.next_step {
                        Some(next) => format!("{}；{next}", readiness.reason),
                        None => readiness.reason,
                    };
                    ("configured".to_owned(), dir, Some(reason))
                }
            }
        };
        TranscriptionConfigReport {
            state,
            config_source: origins.model_dir.wire().to_owned(),
            model_dir,
            model_dir_source: origins.model_dir.wire().to_owned(),
            engine_lib: origins
                .engine_lib_path
                .as_ref()
                .map(|path| path.display().to_string()),
            engine_lib_source: origins.engine_lib.wire().to_owned(),
            provider_id,
            reason,
        }
    }
}

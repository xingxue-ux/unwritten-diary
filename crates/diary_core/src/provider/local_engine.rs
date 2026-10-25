//! 本地引擎适配器：与 HTTP 协议适配器（`adapter.rs`）并列的**第二种适配器形态**。
//!
//! # 为什么要有这一层，而不是把本地引擎塞进 `ProtocolAdapter`
//!
//! `ProtocolAdapter` 的每个方法都在拼 HTTP 请求（`probe → HttpRequest`、
//! `generate → HttpRequest`），它的形状本身就假设了「有一个远端 URL、有一把密钥、
//! 有一次出站调用」。本地引擎这三样都没有：它只有**本机的一个动态库**与**一个模型
//! 目录**，调用不经过 `Transport`，也就**不可能出网**。
//!
//! 硬把它做成一个 URL 是错的：那样「默认不联网」这条承诺就只剩下一句注释，
//! 而 `set_transport_for_test(PanicTransport)` 也拦不住它。
//!
//! 两者共享的是**上半段的语义**，这一层只复用那些：
//!
//! | 语义 | 谁提供 |
//! |---|---|
//! | 「这个适配器实现了哪些能力」 | [`LocalEngine::capabilities`]（与 HTTP 那张表同一个枚举） |
//! | 能力闸门（配置声明 + 适配器实现 + 连接测试结论） | `provider::evaluate`，两条路共用 |
//! | 错误归一化与作业时间语义 | `CoreError` → `ErrorCode` → `jobs::FailureDisposition` |
//! | 连接测试 | 本地的探针**真的加载模型并转写 0.05 秒静音**，但一个字节都不出网 |
//!
//! # 模型目录：可配置，默认指向随包位置
//!
//! 解析顺序（逐字段、先命中先用，写在 [`EngineConfig::resolve_with`] 的注释里，
//! 集成测试钉住）：
//!
//! 1. 环境变量 `DIARY_ASR_MODEL_DIR`（开发、探针、CI 用）；
//! 2. **资料库旁边的 `core-config.json` 的 `asrModelDir`**（鸿蒙产品包设不了环境
//!    变量，这是产品那条通道，见 `crate::config_file`）；
//! 3. **配置里显式给的目录**（`providers.model`，按服务的那一行）；
//! 4. **随包默认位置**：可执行文件旁边的 `models/asr/sense-voice-int8`
//!    （再退一步是 `models/asr`）。这是产品决定：模型随安装包分发，
//!    **不要求联网、不依赖首次下载**；找不到就如实降级（见下）。
//!
//! 「随包」不等于「写死」：第 2/3 条让产品与用户都能换目录，第 1 条让开发与量测
//! 在不重新打包的前提下换模型，分发策略（放资源目录、放 AAB 的 asset pack、
//! 还是让用户导入）不由核心决定——核心只认「一个目录路径」。
//!
//! 引擎动态库的顺序同构：`DIARY_ASR_ENGINE_LIB` → `core-config.json` 的
//! `asrEngineLib` → 可执行文件旁边的候选 → 按平台库名交给系统解析（鸿蒙的
//! `libs/arm64` 走这一档）。
//!
//! # 缺失就如实降级，绝不假装
//!
//! 引擎库不在、模型目录不在、模型文件不齐：闸门给出 `provider_not_configured`
//! （作业侧落成 `waiting_configuration`，**不消耗重试额度**），理由里写清
//! **缺什么、去哪配**。此时**一条转写任务都不会被领取执行**，也就不会产出任何
//! 派生内容——「空文本冒充成功」在这条路上没有入口。

use std::path::{Path, PathBuf};

use crate::config_file::ConfigOrigin;
use crate::error::{CoreError, Result};
use crate::model::{Provider, ProviderCapability, ProviderTestStatus};

/// 一次引擎解析里每个字段的来源（`core.info.runtime_config` 用它）。
#[derive(Debug, Clone, Default)]
pub struct EngineOrigins {
    /// 模型目录来自哪一档（`env` / `file` / `provider` / `bundled`）。
    pub model_dir: ConfigOrigin,
    /// 引擎动态库来自哪一档（`env` / `file` / `platform`）。
    pub engine_lib: ConfigOrigin,
    /// 解析到的引擎动态库路径；`None` = 按平台库名交给系统解析。
    pub engine_lib_path: Option<PathBuf>,
}

/// 内置的本地引擎适配器标识（sherpa-onnx 的 C API）。
pub const SHERPA_ONNX: &str = "sherpa_onnx";

/// 内建转写服务的 id：**不是 `providers` 表里的一行**，而是核心自带能力。
///
/// 为什么不做成「开库时自动往 `providers` 插一行」：用户删掉那一行之后，下次开库
/// 又会原样长回来——没有墓碑就无法区分「用户不要」与「还没配」，那是对用户的欺骗。
/// 内建行只读、不进库；要改模型目录就显式保存一条同 id 的覆盖配置（也可改 enabled
/// 停用它），删掉覆盖行就回到随包默认。`providers::list` 会把内建行合进去，
/// 所以它在设置页里看得见、测得了（见 `provider::list` 的注释）。
pub const BUILTIN_LOCAL_ASR_ID: &str = "builtin_local_asr";

/// 内建转写服务在界面上的显示名。
pub const BUILTIN_LOCAL_ASR_LABEL: &str = "本机转写（随包模型）";

/// 显式指定引擎动态库路径（`sherpa-onnx-c-api.dll` / `libsherpa-onnx-c-api.so`）。
pub const ENV_ENGINE_LIB: &str = "DIARY_ASR_ENGINE_LIB";

/// 显式指定模型目录。
pub const ENV_MODEL_DIR: &str = "DIARY_ASR_MODEL_DIR";

/// 显式指定模型类型：`sense_voice` / `paraformer`。不设时按目录里的文件推断。
pub const ENV_MODEL_TYPE: &str = "DIARY_ASR_MODEL_TYPE";

/// 显式指定要加载的 ONNX 文件名（同目录里放多份模型时用）。
pub const ENV_MODEL_FILE: &str = "DIARY_ASR_MODEL_FILE";

/// 显式指定语言提示（SenseVoice 认 `zh`/`en`/`ja`/`ko`/`yue`）。
///
/// 不设时**留空**：让模型自己判语种（`auto`）。日记以中文为主，但混着方言与外语时
/// 硬钉一个提示反而更容易错。
pub const ENV_LANGUAGE: &str = "DIARY_ASR_LANGUAGE";

/// 随包模型目录的约定（相对可执行文件所在目录）。
pub const BUNDLED_MODEL_REL: &str = "models/asr/sense-voice-int8";

/// 随包模型目录的第二候选：模型文件直接放在 `models/asr` 下。
pub const BUNDLED_MODEL_REL_FLAT: &str = "models/asr";

/// 模型类型。两种都由同一个离线识别器加载，区别只在配置子结构与文件名约定。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelKind {
    /// SenseVoice-Small：中英日韩粤，支持 `use_itn`（含标点）。
    SenseVoice,
    /// Paraformer-zh：中文（含部分方言口音），无 ITN。
    Paraformer,
}

impl ModelKind {
    pub fn wire(self) -> &'static str {
        match self {
            Self::SenseVoice => "sense_voice",
            Self::Paraformer => "paraformer",
        }
    }

    pub fn from_wire(value: &str) -> Option<Self> {
        match value {
            "sense_voice" | "sensevoice" => Some(Self::SenseVoice),
            "paraformer" => Some(Self::Paraformer),
            _ => None,
        }
    }
}

/// 一次转写请求：**单声道 f32 PCM**，已经解码好（解码是引擎之外的事，见
/// `transcription::wav`；引擎不做容器解析，也不读文件）。
#[derive(Debug, Clone, PartialEq)]
pub struct PcmRequest {
    pub samples: Vec<f32>,
    pub sample_rate: u32,
    /// 语言提示（SenseVoice 支持 `zh`/`en`/`ja`/`ko`/`yue`；不设就让模型自己判）。
    pub language: Option<String>,
}

/// 一次转写的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PcmOutcome {
    pub text: String,
    /// 引擎回显的模型名（有就记，没有就不编）。
    pub model: Option<String>,
    /// 模型自己判出来的语种（有就记）。
    pub language: Option<String>,
}

/// 引擎的「现在能不能用」。`code` 用契约第 7 节的码（`provider_not_configured` /
/// `model_unavailable`），作业侧据此落时间语义。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineReadiness {
    pub ready: bool,
    pub code: String,
    /// 给用户看的：**缺什么**（路径一律带出来，不然用户不知道去哪儿找）。
    pub reason: String,
    /// 给用户看的：**怎么办**。
    pub next_step: Option<String>,
}

/// 解析好的一次引擎配置。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineConfig {
    /// 引擎动态库路径。`None` 表示按平台的默认库名加载
    /// （`sherpa-onnx-c-api.dll` / `libsherpa-onnx-c-api.so`；Android 上由系统在
    /// 应用的 native 库目录里解析）。
    pub engine_lib: Option<PathBuf>,
    /// 模型目录（一定非空：解析不到就报错，不猜）。
    pub model_dir: PathBuf,
    /// 显式指定的 ONNX 文件名；`None` 表示按 [`Self::kind`] 的约定找。
    pub model_file: Option<String>,
    pub kind: ModelKind,
    pub language: Option<String>,
    /// 线程数。0 交给引擎自己定（sherpa-onnx 的 `num_threads = 0` 表示用默认值）。
    pub num_threads: i32,
    /// 逆文本规整（数字、标点）。SenseVoice 支持；Paraformer 忽略。
    pub itn: bool,
}

impl EngineConfig {
    /// 解析一次引擎配置（**只读环境变量**，等价于 `resolve_with(None, None)`）。
    ///
    /// 保留这个入口是为了不改变既有调用点的语义：没有核心配置文件的进程（Android /
    /// CI / 探针）走的就是它。产品包那条路走 [`Self::resolve_with`]。
    pub fn resolve(model_override: Option<&str>) -> Result<Self> {
        Self::resolve_with(model_override, None)
    }

    /// 解析一次引擎配置，带上核心配置文件（`core-config.json`）这一档。
    ///
    /// **逐字段的优先级：环境变量 → 配置文件 → provider 行 → 随包默认**。
    ///
    /// - 环境变量在最前：探针/CI 靠 `DIARY_ASR_MODEL_DIR` 换模型，设备上有配置文件
    ///   也不能让它失效；
    /// - 配置文件第二：鸿蒙产品包设不了环境变量，它是产品那条通道；
    /// - provider 行（`providers.model`）第三：它是**按服务**的显式目录，排在全局
    ///   文件之后（见 `docs/architecture/m3-转写链.md` 的解析顺序一节）；
    /// - 随包默认最后：可执行文件旁边的 `models/asr/...`。
    ///
    /// 都解析不到就报 `provider_not_configured`，理由里列清找过哪些位置。
    pub fn resolve_with(
        model_override: Option<&str>,
        file: Option<&crate::config_file::CoreConfigFile>,
    ) -> Result<Self> {
        Self::resolve_report(model_override, file).0
    }

    /// 与 [`Self::resolve_with`] 同一个解析，额外把「每个字段从哪来」带出来。
    ///
    /// `core.info.runtime_config.transcription` 用它——报告与真正加载用的是**同一次
    /// 解析**，不会出现「报告说 file、执行器读了 env」。
    pub fn resolve_report(
        model_override: Option<&str>,
        file: Option<&crate::config_file::CoreConfigFile>,
    ) -> (Result<Self>, EngineOrigins) {
        let mut origins = EngineOrigins::default();
        let explicit = model_override
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .map(|path| (path, ConfigOrigin::Provider));
        let from_env = std::env::var_os(ENV_MODEL_DIR)
            .map(PathBuf::from)
            .filter(|path| !path.as_os_str().is_empty())
            .map(|path| (path, ConfigOrigin::Env));
        let from_file = file
            .and_then(|file| file.asr_model_dir_path())
            .map(|path| (path, ConfigOrigin::File));
        let bundled = bundled_model_dir().map(|path| (path, ConfigOrigin::Bundled));

        let resolved = from_env
            .or(from_file)
            .or(explicit)
            .or(bundled);
        let Some((model_dir, origin)) = resolved else {
            return (
                Err(CoreError::ProviderNotConfigured {
                    reason: format!(
                        "本机转写引擎没有可用的模型目录：环境变量 {ENV_MODEL_DIR} 没设、\
                         core-config.json 里没有 asrModelDir、转写服务的配置里没给 model，\
                         可执行文件旁边也没有 {}",
                        bundled_model_dir_candidates()
                            .iter()
                            .map(|path| path.display().to_string())
                            .collect::<Vec<_>>()
                            .join(" / ")
                    ),
                }),
                origins,
            );
        };
        origins.model_dir = origin;

        let kind = model_kind_for(&model_dir);
        let (engine_lib, engine_lib_origin) = resolve_engine_lib_with(file);
        origins.engine_lib = engine_lib_origin;
        origins.engine_lib_path = engine_lib.clone();
        // OHOS：引擎库只能从 HAP 的 `libs/arm64` 加载，沙箱里的 `.so` 不能 dlopen。
        // 配置写错要在**解析时**就报清楚，而不是等作业跑起来才拿到一句 dlopen 失败。
        if let Some(path) = &engine_lib {
            if let Err(reason) = validate_engine_lib_path(path) {
                return (Err(CoreError::ProviderNotConfigured { reason }), origins);
            }
        }
        (
            Ok(Self {
                engine_lib,
                model_dir,
                model_file: std::env::var(ENV_MODEL_FILE)
                    .ok()
                    .map(|value| value.trim().to_owned())
                    .filter(|value| !value.is_empty()),
                kind,
                language: std::env::var(ENV_LANGUAGE)
                    .ok()
                    .map(|value| value.trim().to_owned())
                    .filter(|value| !value.is_empty()),
                num_threads: 2,
                itn: true,
            }),
            origins,
        )
    }

    /// 真正要加载的 ONNX 文件。优先量化版（体积 228 MiB 对 fp32 的 894 MiB）。
    pub fn model_path(&self) -> std::result::Result<PathBuf, String> {
        let candidates: Vec<String> = match &self.model_file {
            Some(name) => vec![name.clone()],
            None => vec!["model.int8.onnx".to_owned(), "model.onnx".to_owned()],
        };
        for name in &candidates {
            let path = self.model_dir.join(name);
            if path.is_file() {
                return Ok(path);
            }
        }
        Err(format!(
            "{} 里找不到模型文件（找过 {}）",
            self.model_dir.display(),
            candidates.join(" / ")
        ))
    }

    pub fn tokens_path(&self) -> PathBuf {
        self.model_dir.join("tokens.txt")
    }

    /// 配置是否自洽（只 stat，不加载）。不满足时给出「缺什么」。
    pub fn check_files(&self) -> std::result::Result<(), String> {
        self.model_path()?;
        let tokens = self.tokens_path();
        if !tokens.is_file() {
            return Err(format!(
                "{} 里没有 tokens.txt（它和 onnx 文件一起构成一个模型目录）",
                self.model_dir.display()
            ));
        }
        Ok(())
    }

    /// 模型身份：给 `extracted_contents.extractor_version` 用，**换模型必然换版本**，
    /// 于是旧片段不会被当成新模型的产物继续用（见 `transcription::run` 的重建口径）。
    ///
    /// 用「文件名 + 字节数 + 修改时间 + sha256 前 12 位」。sha256 只算一次
    /// （进程内缓存）：228 MiB 的模型每次都哈希会把每个任务拖慢半秒以上，
    /// 而这里的用途只是「变了没有」，不是完整性校验。
    pub fn model_identity(&self) -> String {
        let path = match self.model_path() {
            Ok(path) => path,
            Err(_) => return format!("{}:missing", self.kind.wire()),
        };
        let meta = std::fs::metadata(&path).ok();
        let bytes = meta.as_ref().map(|meta| meta.len()).unwrap_or(0);
        let modified = meta
            .as_ref()
            .and_then(|meta| meta.modified().ok())
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|duration| duration.as_secs())
            .unwrap_or(0);
        let digest = crate::transcription::model_digest(&path).unwrap_or_else(|| "nohash".to_owned());
        format!(
            "{}:{}:{}:{}:{}",
            self.kind.wire(),
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default(),
            bytes,
            modified,
            digest
        )
    }
}

/// 引擎能力的统一入口：与 `adapter::adapter_for` 同构，按适配器标识取实现。
pub trait LocalEngine: Send + Sync {
    fn id(&self) -> &'static str;

    /// 这个引擎真的实现了的能力。
    fn capabilities(&self) -> &'static [ProviderCapability];

    /// 现在能不能用。**只 stat，不加载模型**：闸门在每次领取任务前都会问一次，
    /// 不应该把 228 MiB 的模型反复读进内存。
    fn readiness(&self, config: &EngineConfig) -> EngineReadiness;

    /// 真转写一段 PCM。**不联网**：这条路不经过 `Transport`。
    fn transcribe(&self, config: &EngineConfig, request: &PcmRequest) -> Result<PcmOutcome>;

    /// 连接测试用的探针：**真的加载模型并转写一小段静音**，但一个字节都不出网。
    ///
    /// 返回给用户看的说明（含引擎版本与模型路径）。失败必须是可归一的错误
    /// （`CoreError`），报告里会带上真实原因。
    fn probe(&self, config: &EngineConfig) -> Result<String>;
}

/// 按标识取内置的本地引擎。取不到就是「本构建里没有这个引擎」——不静默换一个。
pub fn local_engine_for(id: &str) -> Option<&'static dyn LocalEngine> {
    match id {
        SHERPA_ONNX => Some(crate::transcription::sherpa::engine()),
        _ => None,
    }
}

/// 内置的本地引擎适配器清单。
pub fn builtin_local_engine_ids() -> &'static [&'static str] {
    &[SHERPA_ONNX]
}

/// 引擎库的解析（只读环境变量）：等价于 [`resolve_engine_lib_with`] 传 `None`。
///
/// 顺序：`DIARY_ASR_ENGINE_LIB` → 可执行文件旁边的候选路径 → `None`（按库名交给
/// 系统解析）。
pub fn resolve_engine_lib() -> Option<PathBuf> {
    resolve_engine_lib_with(None).0
}

/// 引擎库的解析，带上核心配置文件。
///
/// 顺序：`DIARY_ASR_ENGINE_LIB` → `core-config.json` 的 `asrEngineLib` →
/// 可执行文件旁边的候选路径 → `None`（按平台库名交给系统解析；鸿蒙的
/// `libs/arm64` 与 Android 的 native 库目录都走这一档）。
///
/// 返回路径与来源：来源是报告用的，`None` 时来源是 `Platform`（不是「缺库」——
/// 按平台库名加载在移动端是正常的）。
pub fn resolve_engine_lib_with(
    file: Option<&crate::config_file::CoreConfigFile>,
) -> (Option<PathBuf>, ConfigOrigin) {
    if let Some(value) = std::env::var_os(ENV_ENGINE_LIB) {
        let path = PathBuf::from(value);
        if !path.as_os_str().is_empty() {
            return (Some(path), ConfigOrigin::Env);
        }
    }
    if let Some(path) = file.and_then(|file| file.asr_engine_lib_path()) {
        return (Some(path), ConfigOrigin::File);
    }
    if let Some(path) = engine_lib_candidates().into_iter().find(|path| path.is_file()) {
        return (Some(path), ConfigOrigin::Bundled);
    }
    (None, ConfigOrigin::Platform)
}

/// OHOS 上「应用数据目录」的前缀：这些路径下的 `.so` **不能**在运行时 dlopen。
///
/// 对照：安装目录是 `/data/storage/el1/bundle/...`（HAP 里的 `libs/arm64` 就在那儿），
/// **不**在这个列表里——引擎库本来就该放那里。应用数据（el2 / el1 的 base/database/
/// cache）是沙箱，linker 不允许从那里加载原生库（探针实测，见
/// `probes/ohos-core-smoke`）。
const OHOS_APP_DATA_PREFIXES: &[&str] = &[
    "/data/storage/el2/",
    "/data/storage/el1/base/",
    "/data/storage/el1/database/",
    "/data/storage/el1/cache/",
    "/data/app/el2/",
    "/data/app/el1/base/",
];

/// 这个路径是不是 OHOS 的应用数据目录（沙箱）。纯字符串判断，宿主上也能测。
pub fn is_ohos_app_data_path(path: &str) -> bool {
    OHOS_APP_DATA_PREFIXES
        .iter()
        .any(|prefix| path.starts_with(prefix))
}

/// 校验显式指定的引擎库路径（`DIARY_ASR_ENGINE_LIB` 或 `core-config.json` 的
/// `asrEngineLib`）。**只在 OHOS 上有限制**：其它平台可以从任意路径 dlopen。
///
/// 为什么要在配置层拒绝，而不是等 `load_library` 失败：沙箱路径的 `.so` 是**存在**的
/// （`is_file()` 为真），`readiness` 会放行，作业跑起来才在 linker 那里炸——错误信息
/// 会指向一个「明明存在的文件」，用户照着它查不出原因。这里直接点名「沙箱不能加载」
/// 并给出该放哪儿。
pub fn validate_engine_lib_path(path: &Path) -> std::result::Result<(), String> {
    validate_ohos_native_lib_path(
        path,
        "asrEngineLib",
        "引擎库",
        "libsherpa-onnx-c-api.so",
    )
}

/// 校验 ORT 动态库路径（`DIARY_ORT_DYLIB` / `ORT_DYLIB_PATH` / `core-config.json` 的
/// `ortDylib`）。与 [`validate_engine_lib_path`] 同款：OHOS 上沙箱里的 `.so` 不能
/// dlopen，必须在 HAP 的 `libs/arm64/`（例如 `libonnxruntime-diary-1.29.1.so`）。
///
/// 为什么 ORT 也要拦：`ort` 的 `load-dynamic` 看起来「只是按路径 dlopen」，但 OHOS
/// 的 linker 限制与引擎库完全相同；不拦的话，`core.info` 会报 `configured`、第一次
/// 语义索引才在 `init_from` 失败，用户同样查不出是路径问题。
pub fn validate_ort_dylib_path(path: &Path) -> std::result::Result<(), String> {
    validate_ohos_native_lib_path(
        path,
        "ortDylib",
        "ORT 动态库",
        "libonnxruntime-diary-1.29.1.so",
    )
}

/// 两种原生库共用的沙箱判定。`cfg(test)` 也打开，这样宿主单测能覆盖「拒绝」那一半；
/// 非 OHOS 的产品构建不启用（桌面端可以从任意目录 dlopen）。
fn validate_ohos_native_lib_path(
    path: &Path,
    field: &str,
    what: &str,
    example: &str,
) -> std::result::Result<(), String> {
    #[cfg(any(target_env = "ohos", test))]
    {
        let text = path.to_string_lossy();
        if is_ohos_app_data_path(&text) {
            return Err(format!(
                "{field} 指向应用数据目录（{text}）：OHOS 的 linker 只允许从 HAP 的 \
                 libs/arm64 加载原生库，沙箱（el2 / el1/base）里的 .so 不能 dlopen。\
                 把{what}打进 HAP 的 libs/arm64/（例如 {example}），配置改成 \
                 /data/storage/el1/bundle/libs/arm64/<文件名>；模型文件才放沙箱。"
            ));
        }
    }
    let _ = (path, field, what, example);
    Ok(())
}

/// 平台上的引擎库文件名。
pub fn engine_lib_file_name() -> &'static str {
    if cfg!(target_os = "windows") {
        "sherpa-onnx-c-api.dll"
    } else if cfg!(target_os = "macos") {
        "libsherpa-onnx-c-api.dylib"
    } else {
        "libsherpa-onnx-c-api.so"
    }
}

/// 引擎旁边的 ONNX Runtime 文件名。
pub fn onnxruntime_file_name() -> &'static str {
    if cfg!(target_os = "windows") {
        "onnxruntime.dll"
    } else if cfg!(target_os = "macos") {
        "libonnxruntime.dylib"
    } else {
        "libonnxruntime.so"
    }
}

/// 可执行文件旁边的引擎库候选路径。
fn engine_lib_candidates() -> Vec<PathBuf> {
    let Some(dir) = executable_dir() else {
        return Vec::new();
    };
    let name = engine_lib_file_name();
    vec![
        dir.join(name),
        dir.join("lib").join(name),
        dir.join("libs").join(name),
    ]
}

/// 随包模型目录的候选路径（按顺序）。
pub fn bundled_model_dir_candidates() -> Vec<PathBuf> {
    let Some(dir) = executable_dir() else {
        return Vec::new();
    };
    vec![
        dir.join(BUNDLED_MODEL_REL),
        dir.join(BUNDLED_MODEL_REL_FLAT),
    ]
}

/// 随包模型目录：第一个**真的有模型文件**的候选。
///
/// 只 stat 目录不够：一个空的 `models/asr` 目录会让闸门以为「有模型」，
/// 然后任务跑起来才发现没有——那就成了「排一条注定失败的任务」。
fn bundled_model_dir() -> Option<PathBuf> {
    bundled_model_dir_candidates()
        .into_iter()
        .find(|dir| looks_like_model_dir(dir))
}

/// 这个目录看起来是不是一个模型目录（有 onnx + tokens.txt）。
pub fn looks_like_model_dir(dir: &Path) -> bool {
    dir.join("tokens.txt").is_file()
        && (dir.join("model.int8.onnx").is_file() || dir.join("model.onnx").is_file())
}

/// 可执行文件所在目录。Android 上 `current_exe()` 是 `app_process`，
/// 所以那里**不靠它**解析资源（见 `resolve_engine_lib` 的按库名兜底）。
fn executable_dir() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(Path::to_path_buf))
}

/// 模型类型：显式环境变量优先，否则按目录里的文件推断。
///
/// 推断规则（写在文档里，测试钉住）：目录里有 `am.mvn`、或者目录名里带
/// `paraformer` → Paraformer；否则 SenseVoice。两种模型的导出目录形状不同，
/// 这条推断只看「不可能同时成立」的特征，不猜内容。
fn model_kind_for(dir: &Path) -> ModelKind {
    if let Ok(value) = std::env::var(ENV_MODEL_TYPE) {
        if let Some(kind) = ModelKind::from_wire(value.trim()) {
            return kind;
        }
    }
    let name = dir
        .file_name()
        .map(|name| name.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    if dir.join("am.mvn").is_file() || name.contains("paraformer") {
        ModelKind::Paraformer
    } else {
        ModelKind::SenseVoice
    }
}

/// 内建转写服务（合成的一行，不进 `providers` 表）。
///
/// `model` 给出解析到的目录（可能为 `None`：随包目录不在、环境变量与配置文件也
/// 没给），这样设置页里能看到「引擎现在打算用哪个目录」——看不到路径的降级提示，
/// 用户没法照着修。`file` 是核心配置文件的四个可选路径（没有就是 `None`）。
pub(crate) fn builtin_provider(file: Option<&crate::config_file::CoreConfigFile>) -> Provider {
    let resolved = EngineConfig::resolve_with(None, file).ok();
    Provider {
        id: BUILTIN_LOCAL_ASR_ID.to_owned(),
        label: BUILTIN_LOCAL_ASR_LABEL.to_owned(),
        adapter_id: SHERPA_ONNX.to_owned(),
        endpoint: None,
        model: resolved
            .as_ref()
            .map(|config| config.model_dir.to_string_lossy().into_owned()),
        capabilities: [ProviderCapability::TranscribeAudio].into_iter().collect(),
        credential_ref: None,
        enabled: true,
        test_status: ProviderTestStatus::Untested,
    }
}

/// 一次引擎配置的**唯一**取得方式：测试/量测钉死的配置优先，否则按
/// 「环境变量 → 核心配置文件 → 配置行 → 随包默认」解析。
///
/// 闸门、连接测试、作业执行者都走这一个函数，所以「用哪个模型目录」只有一处定义
/// （它们必须一致，否则会出现「闸门说可用、执行者去读另一个目录」）。
pub(crate) fn config_for(
    core: &crate::Core,
    model_override: Option<&str>,
) -> Result<EngineConfig> {
    if let Some(pinned) = core.asr_engine_config_for_test() {
        return Ok(pinned.clone());
    }
    EngineConfig::resolve_with(model_override, core.config_file())
}

/// 引擎就绪检查的统一入口：取配置 → 问实现。
///
/// 闸门、连接测试、作业执行者都走这一个函数，所以「什么叫做可用」只有一处定义。
pub(crate) fn readiness_for(core: &crate::Core, model_override: Option<&str>) -> Result<EngineReadiness> {
    let config = config_for(core, model_override)?;
    Ok(core.asr_engine().readiness(&config))
}

/// 把一个「不就绪」的结论折成给用户的一句话（原因 + 下一步）。
pub(crate) fn blocked_message(readiness: &EngineReadiness) -> String {
    match &readiness.next_step {
        Some(next) => format!("{}；{next}", readiness.reason),
        None => readiness.reason.clone(),
    }
}

// ------------------------------------------------------------------ 转写这一路的「用哪个引擎」

/// 转写任务的引擎选择结果：**闸门与执行者共用同一个函数**，所以「闸门说用哪个、
/// 执行者就用哪个」不是靠两边各写一遍规则来对齐的。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TranscriptionChoice {
    /// 给用户看的名字（报告与错误消息里用）。
    pub label: String,
    /// 显式配置的服务 id；`None` 表示核心自带的本机引擎（随包模型）。
    pub provider_id: Option<String>,
    /// 传给 [`EngineConfig::resolve`] 的显式目录（配置行里的 `model`）。
    pub model_override: Option<String>,
}

/// 挑一次「转写用哪个本机引擎」。
///
/// 规则（与闸门的候选顺序一致，见 `provider::capability_gate_for_with`）：
///
/// 1. **启用中的本机引擎配置行**，按 `providers.list` 的顺序取第一条。它的
///    `model` 是显式目录（空 = 用默认解析）。**坏掉的显式配置不会静默退回默认**：
///    用户明确指了一个目录，就该按那个目录报错，而不是偷偷用另一个模型转出东西来；
/// 2. 否则就是**核心自带的本机引擎**（环境变量/随包默认位置）。
///
/// 远端 HTTP 服务**不参与**：录音这条转写链跑的是本机引擎（`transcription` 模块），
/// 远端转写是 #99 的评测路径与将来产品化的事——把远端也列进来会让闸门说「可用」
/// 而执行者根本不会用它。
pub(crate) fn transcription_choice(core: &crate::Core) -> Result<TranscriptionChoice> {
    for provider in crate::provider::stored_providers(core)? {
        if !provider.enabled {
            continue;
        }
        let Some(adapter) = crate::provider::adapter::adapter_for(&provider.adapter_id) else {
            continue;
        };
        if !matches!(adapter, crate::provider::adapter::Adapter::Local(_)) {
            continue;
        }
        // 只声明了别的能力（例如手写的 `generate_text`）的本机行不该被转写挑中：
        // 闸门会因为它没声明 `transcribe_audio` 而拒绝，执行者却会去读它的模型目录
        // ——那正是「闸门说不可用、执行者说能用」的抖动。两处用同一个判据。
        if !provider
            .capabilities
            .contains(&ProviderCapability::TranscribeAudio)
        {
            continue;
        }
        let model_override = provider
            .model
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned);
        return Ok(TranscriptionChoice {
            label: format!("服务「{}」（{}）", provider.label, provider.adapter_id),
            provider_id: Some(provider.id),
            model_override,
        });
    }
    Ok(TranscriptionChoice {
        label: BUILTIN_LOCAL_ASR_LABEL.to_owned(),
        provider_id: None,
        model_override: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 一条只声明 `generate_text` 的本机配置行**不能**顶掉转写选择。
    ///
    /// 本机引擎不实现生成（随包 `local_llm` 已移除），但用户/旧版本仍可能留下一行
    /// 声称 `generate_text` 的本机配置。转写挑选与闸门用同一个判据（必须声明
    /// `transcribe_audio`），否则会出现「闸门说不可用、执行者去读它的模型目录」。
    #[test]
    fn generation_only_local_row_does_not_shadow_transcription() {
        let dir = tempfile::tempdir().unwrap();
        let mut core = crate::Core::open_in_memory_at(dir.path()).unwrap();
        let row = Provider {
            id: "generation_only_local".to_owned(),
            label: "只声明生成的本机行".to_owned(),
            adapter_id: SHERPA_ONNX.to_owned(),
            endpoint: None,
            model: Some("missing/model-dir".to_owned()),
            capabilities: [ProviderCapability::GenerateText].into_iter().collect(),
            credential_ref: None,
            enabled: true,
            test_status: ProviderTestStatus::Untested,
        };
        core.save_provider(&row, None).unwrap();
        // 它不被挑中：选择回落到内建引擎（None），不是那行手写配置。
        assert_eq!(transcription_choice(&core).unwrap().provider_id, None);
        let mut asr = builtin_provider(None);
        asr.id = "my_asr".to_owned();
        core.save_provider(&asr, None).unwrap();
        assert_eq!(
            transcription_choice(&core).unwrap().provider_id.as_deref(),
            Some("my_asr")
        );
    }

    /// OHOS 的「沙箱 vs 安装目录」判定：沙箱路径要拒，`libs/arm64` 要放行。
    ///
    /// 这个判定只在 `target_env = "ohos"` 时生效（其它平台可以从任意路径 dlopen），
    /// 但**判定逻辑本身**在宿主上就能测——不测的话，真机第一次发现写错就晚了。
    #[test]
    fn ohos_app_data_paths_are_recognised_and_install_paths_are_not() {
        for path in [
            "/data/storage/el2/base/haps/entry/files/libsherpa-onnx-c-api.so",
            "/data/storage/el1/base/files/libsherpa-onnx-c-api.so",
            "/data/storage/el1/database/x.so",
            "/data/storage/el1/cache/x.so",
            "/data/app/el2/100/base/com.example/x.so",
            "/data/app/el1/base/x.so",
        ] {
            assert!(is_ohos_app_data_path(path), "应当识别为沙箱：{path}");
        }
        for path in [
            // HAP 的安装目录：引擎库本来就该放这儿。
            "/data/storage/el1/bundle/libs/arm64/libsherpa-onnx-c-api.so",
            "/data/app/el1/bundle/public/com.example/libs/arm64/libsherpa-onnx-c-api.so",
            "libsherpa-onnx-c-api.so",
            "/usr/lib/libsherpa-onnx-c-api.so",
        ] {
            assert!(!is_ohos_app_data_path(path), "不该识别为沙箱：{path}");
        }
    }

    /// 非 OHOS 平台上不限制路径（桌面端可以从任意目录 dlopen）；OHOS 上拒绝沙箱。
    ///
    /// `cfg(test)` 让「拒绝」这一半在宿主上也能跑：沙箱路径的判定是纯字符串判断，
    /// 不依赖真机。安装目录（`el1/bundle/libs/arm64`）必须放行。
    #[test]
    fn engine_and_ort_validation_rejects_sandbox_but_allows_install_paths() {
        let sandbox_engine =
            Path::new("/data/storage/el2/base/haps/entry/files/libsherpa-onnx-c-api.so");
        let sandbox_ort =
            Path::new("/data/storage/el2/base/haps/entry/files/libonnxruntime-diary-1.29.1.so");
        assert!(
            validate_engine_lib_path(sandbox_engine).is_err(),
            "asrEngineLib 指向沙箱必须拒绝"
        );
        assert!(
            validate_ort_dylib_path(sandbox_ort).is_err(),
            "ortDylib 指向沙箱必须拒绝"
        );

        let install_engine = Path::new("/data/storage/el1/bundle/libs/arm64/libsherpa-onnx-c-api.so");
        let install_ort =
            Path::new("/data/storage/el1/bundle/libs/arm64/libonnxruntime-diary-1.29.1.so");
        assert!(validate_engine_lib_path(install_engine).is_ok(), "安装目录必须放行");
        assert!(validate_ort_dylib_path(install_ort).is_ok(), "安装目录必须放行");
        let relative = Path::new("libsherpa-onnx-c-api.so");
        assert!(validate_engine_lib_path(relative).is_ok(), "按库名加载必须放行");
    }
}

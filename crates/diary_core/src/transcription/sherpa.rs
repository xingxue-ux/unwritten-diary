//! sherpa-onnx 本地转写引擎：**运行时**加载 `sherpa-onnx-c-api` 动态库。
//!
//! # 为什么是动态加载而不是链接期依赖
//!
//! 与 `ort`（`load-dynamic`）同一个理由，写在 `M0-技术验证.md` 与
//! `docs/architecture/m3-转写链.md` 里：
//!
//! 1. **CI 不装原生依赖**：没有引擎库、没有模型的机器照样 `cargo test` 编过、跑过。
//!    真引擎那一条是 `#[ignore]` 的手动量测（有模型才跑）；
//! 2. **随包分发由产品层决定**：库放哪、随不随包、走不走 asset pack，都不是核心
//!    能替产品决定的事。核心只认「一个路径或一个库名」；
//! 3. **缺失可如实降级**：库不在就是一个可以讲清楚的事实（`provider_not_configured`
//!    → `waiting_configuration`），而不是「编不过」。
//!
//! # ABI 是**钉死并自检**的
//!
//! C API 的结构体是通过值传进 `SherpaOnnxCreateOfflineRecognizer` 的，布局错一个
//! 字段就是一次静默的内存错位——比编译错误坏得多。所以：
//!
//! - 结构体按 **v1.13.8 的 `c-api.h`** 逐字段对齐（`#[repr(C)]`）；
//! - 每个我们**真的会写**的字段，都有一个**从真实头文件用 `offsetof` 取出来的
//!   偏移量**，写进本文件的测试里（`abi_layout_matches_the_pinned_header`）；
//!   同时断言结构体总大小。换版本时这些断言会红，逼人重新核对，而不是靠记忆；
//! - 运行时还取一次 `SherpaOnnxGetVersionStr()`，版本写进连接测试报告与日志。
//!
//! 未参与 ABI 校验的结构体成员用不透明填充占位（注释里写明它对应 C 里的谁）：
//! 我们不需要它们的值，但需要**它们占的位置**是对的。
//!
//! # 不释放动态库
//!
//! 与 `libonnxruntime` 同样的坑（`M0-技术验证.md` 第 4 节）：动态库在退出阶段的
//! C++ 静态析构会崩。所以库句柄**进程级保留、永不 dlclose**。识别器本身在配置
//! 变化时会显式销毁并重建（那个是安全的），内存不会随任务数增长。

use std::ffi::{c_char, c_void, CStr, CString};
use std::path::Path;
use std::sync::Mutex;

use crate::error::{CoreError, Result};
use crate::model::ProviderCapability;
use crate::provider::local_engine::{
    EngineConfig, EngineReadiness, LocalEngine, ModelKind, PcmOutcome, PcmRequest, SHERPA_ONNX,
    onnxruntime_file_name,
};

/// 这一份 FFI 声明对应的 C API 版本。写进报告，升级时逼人重核 ABI。
pub const PINNED_C_API_VERSION: &str = "v1.13.8";

/// sherpa-onnx 引擎的实现。它是**无状态的**：动态库句柄与识别器都在进程级
/// 缓存（[`CACHE`]）里，所以可以随便 clone 出多个值，行为一致。
pub struct SherpaOnnxEngine;

/// 引擎单例。`engine()` 返回的就是它。
static SHERPA: SherpaOnnxEngine = SherpaOnnxEngine;

/// 进程级的引擎缓存：动态库句柄 + 当前配置下的识别器。
static CACHE: Mutex<Option<Loaded>> = Mutex::new(None);

pub fn engine() -> &'static dyn LocalEngine {
    &SHERPA
}

// ------------------------------------------------------------------ C ABI 声明
//
// 只声明我们真的调用到的函数与字段。名字与顺序照 `c-api.h`（v1.13.8）。

type OfflineRecognizer = c_void;
type OfflineStream = c_void;

/// `SherpaOnnxFeatureConfig`（`sample_rate` @0、`feature_dim` @4，共 8 字节）。
#[repr(C)]
#[derive(Clone, Copy)]
struct FeatureConfig {
    sample_rate: i32,
    feature_dim: i32,
}

/// `SherpaOnnxOfflineParaformerModelConfig`：一个 `const char *model`。
#[repr(C)]
#[derive(Clone, Copy)]
struct ParaformerModelConfig {
    model: *const c_char,
}

/// `SherpaOnnxOfflineSenseVoiceModelConfig`：model / language / use_itn。
#[repr(C)]
#[derive(Clone, Copy)]
struct SenseVoiceModelConfig {
    model: *const c_char,
    language: *const c_char,
    use_itn: i32,
}

/// `SherpaOnnxOfflineLMConfig`：model / scale。
#[repr(C)]
#[derive(Clone, Copy)]
struct LmConfig {
    model: *const c_char,
    scale: f32,
}

/// `SherpaOnnxHomophoneReplacerConfig`：三个指针。
#[repr(C)]
#[derive(Clone, Copy)]
struct HomophoneReplacerConfig {
    dict_dir: *const c_char,
    lexicon: *const c_char,
    rule_fsts: *const c_char,
}

/// `SherpaOnnxOfflineModelConfig`（共 504 字节）。
///
/// 中间那些我们**不用**的子配置用 `[u8; N]` 占位，长度按真实头文件算出来的
/// 偏移量取（每个占位块上面写明它对应 C 里的哪个成员）。断言在测试里。
#[repr(C)]
struct OfflineModelConfig {
    /// `SherpaOnnxOfflineTransducerModelConfig`（encoder/decoder/joiner）。
    transducer: [*const c_char; 3],
    paraformer: ParaformerModelConfig,
    /// `SherpaOnnxOfflineNemoEncDecCtcModelConfig`（一个指针）。
    nemo_ctc: [*const c_char; 1],
    /// `SherpaOnnxOfflineWhisperModelConfig`（3 指针 + 3 × i32；共 48 字节）。
    whisper: [u8; 48],
    /// `SherpaOnnxOfflineTdnnModelConfig`（一个指针）。
    tdnn: [*const c_char; 1],
    tokens: *const c_char,
    num_threads: i32,
    debug: i32,
    provider: *const c_char,
    model_type: *const c_char,
    modeling_unit: *const c_char,
    bpe_vocab: *const c_char,
    telespeech_ctc: *const c_char,
    sense_voice: SenseVoiceModelConfig,
    /// `SherpaOnnxOfflineMoonshineModelConfig`（5 指针）。
    moonshine: [*const c_char; 5],
    /// `SherpaOnnxOfflineFireRedAsrModelConfig`（2 指针）。
    fire_red_asr: [*const c_char; 2],
    /// `SherpaOnnxOfflineDolphinModelConfig`（一个指针）。
    dolphin: [*const c_char; 1],
    /// `SherpaOnnxOfflineZipformerCtcModelConfig`（一个指针）。
    zipformer_ctc: [*const c_char; 1],
    /// `SherpaOnnxOfflineCanaryModelConfig`（4 指针 + i32，含尾部填充）。
    canary: [u8; 40],
    /// `SherpaOnnxOfflineWenetCtcModelConfig`（一个指针）。
    wenet_ctc: [*const c_char; 1],
    /// `SherpaOnnxOfflineOmnilingualAsrCtcModelConfig`（一个指针）。
    omnilingual: [*const c_char; 1],
    /// `SherpaOnnxOfflineMedAsrCtcModelConfig`（一个指针）。
    medasr: [*const c_char; 1],
    /// `SherpaOnnxOfflineFunASRNanoModelConfig`（7 指针 + 3 × i32/f32 + 2 指针 + i32；
    /// 共 88 字节）。
    funasr_nano: [u8; 88],
    /// `SherpaOnnxOfflineFireRedAsrCtcModelConfig`（一个指针）。
    fire_red_asr_ctc: [*const c_char; 1],
    /// `SherpaOnnxOfflineQwen3ASRModelConfig`（4 指针 + 5 × i32/f32 + 1 指针；共 64 字节）。
    qwen3_asr: [u8; 64],
    /// `SherpaOnnxOfflineCohereTranscribeModelConfig`（3 指针 + 2 × i32；共 32 字节）。
    cohere_transcribe: [u8; 32],
}

/// `SherpaOnnxOfflineRecognizerConfig`（共 608 字节）。
#[repr(C)]
struct OfflineRecognizerConfig {
    feat_config: FeatureConfig,
    model_config: OfflineModelConfig,
    lm_config: LmConfig,
    decoding_method: *const c_char,
    max_active_paths: i32,
    hotwords_file: *const c_char,
    hotwords_score: f32,
    rule_fsts: *const c_char,
    rule_fars: *const c_char,
    blank_penalty: f32,
    hr: HomophoneReplacerConfig,
}

// 对于 `*const c_char` 数组，`Default` 不是自动实现的（长度 > 32），
// 用显式零初始化统一处理：C 侧约定「用不到的都留 NULL」。
impl OfflineRecognizerConfig {
    fn zeroed() -> Self {
        // SAFETY: 全零对 `#[repr(C)]` 的 POD 结构体是合法初值；C API 的文档要求
        // 调用方「Zero-initialize this struct before use」。
        unsafe { std::mem::zeroed() }
    }
}

/// `sherpa-onnx-c-api` 里我们调用的那几个符号。
struct Api {
    create_recognizer: unsafe extern "C" fn(*const OfflineRecognizerConfig) -> *const OfflineRecognizer,
    destroy_recognizer: unsafe extern "C" fn(*const OfflineRecognizer),
    create_stream: unsafe extern "C" fn(*const OfflineRecognizer) -> *const OfflineStream,
    destroy_stream: unsafe extern "C" fn(*const OfflineStream),
    accept_waveform:
        unsafe extern "C" fn(*const OfflineStream, i32, *const f32, i32),
    decode: unsafe extern "C" fn(*const OfflineRecognizer, *const OfflineStream),
    result_json: unsafe extern "C" fn(*const OfflineStream) -> *const c_char,
    destroy_result_json: unsafe extern "C" fn(*const c_char),
    version: unsafe extern "C" fn() -> *const c_char,
}

/// 加载好的动态库 + 解析出来的符号。库句柄永不释放（见模块文档）。
struct Loaded {
    /// 配置指纹：识别器与它绑定，配置变了就重建。
    identity: String,
    /// 动态库路径（报告里印出来）。
    lib_path: String,
    /// 引擎版本（`SherpaOnnxGetVersionStr`）。
    engine_version: String,
    /// 模型文件的可读标签（报告与派生内容版本里用）。
    model_label: String,
    recognizer: *const OfflineRecognizer,
    /// 库句柄：**不 drop**，所以它一直活着（用 `ManuallyDrop` 表达这个意图）。
    _library: std::mem::ManuallyDrop<libloading::Library>,
    api: Api,
}

// SAFETY: 这里是裸指针（`recognizer: *const c_void`）跨线程的唯一理由与边界：
// - 核心对引擎的调用**全部串行**（`Core` 由 `BridgeSession` 的互斥锁串行化，
//   作业驱动的线程模型是「同一时刻只有一条任务在跑」），所以不存在并发调用；
// - 识别器本身在 sherpa-onnx 里是只读的推理对象，`SherpaOnnxOfflineStream` 才是
//   每次调用的可变状态，而它**在每次 `transcribe` 内部创建并销毁**，不共享；
// - `CACHE` 上的 `Mutex` 保证「取出/替换识别器」这一步是互斥的。
// 结论：把指针标成 `Send + Sync` 不改变运行时的并发事实，只是把上面这套约束
// 写进类型系统。若将来核心改成多线程并发转写，**必须重新评估这一条**。
unsafe impl Send for Loaded {}
unsafe impl Sync for Loaded {}

impl SherpaOnnxEngine {
    /// 取得（必要时加载并缓存）引擎。
    ///
    /// 返回的是一个**指向进程级缓存项的裸指针凭证**（[`Loaded2`]），不是借用：
    /// 转写期间要创建/销毁 stream 并与识别器交互，不能一直握着 `CACHE` 的锁。
    /// 这条设计的前提是「核心同一时刻只有一次转写」（见 `unsafe impl Send for Loaded`）。
    fn loaded(config: &EngineConfig) -> Result<Loaded2> {
        let identity = recognizer_identity(config);
        let mut cache = CACHE.lock().map_err(|_| CoreError::InvalidState {
            entity: "本机转写引擎",
            id: SHERPA_ONNX.to_owned(),
            state: "引擎缓存的锁坏了（上一次转写可能 panic 过）".to_owned(),
        })?;

        let needs_rebuild = cache
            .as_ref()
            .map(|loaded| loaded.identity != identity)
            .unwrap_or(true);
        if needs_rebuild {
            // 配置变了（或第一次）：先把新的一套建好，再销毁旧的识别器。
            // 顺序不能反——反了会出现「旧的已销毁、新的没建起来」的空窗。
            let library = load_library(config)?;
            let api = Api::from_library(&library)?;
            let engine_version = api.version_string();
            let recognizer = create_recognizer(&api, config)?;
            let model_label = config
                .model_path()
                .map(|path| {
                    path.file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_else(|| path.display().to_string())
                })
                .unwrap_or_else(|_| "<未知模型>".to_owned());
            if let Some(old) = cache.take() {
                // SAFETY: 旧识别器已经从缓存里摘掉，不再有任何调用引用它。
                unsafe { (old.api.destroy_recognizer)(old.recognizer) };
            }
            *cache = Some(Loaded {
                identity,
                lib_path: library_path_text(config),
                engine_version,
                model_label,
                recognizer,
                _library: std::mem::ManuallyDrop::new(library),
                api,
            });
        }

        let pointer: *const Loaded = cache.as_ref().expect("上面刚保证缓存里有一项") as *const Loaded;
        drop(cache);
        Ok(Loaded2 { loaded: pointer })
    }
}

/// 从缓存里取用期间的一个**裸指针凭证**。
///
/// 它不持有锁（转写期间会创建/销毁 stream 并与识别器交互），但保证调用方拿到的
/// 指针来自当前缓存项。核心的串行调用模型是它的前提（见 `unsafe impl Send`）。
struct Loaded2 {
    loaded: *const Loaded,
}

// SAFETY: 同上——核心串行调用，且 `Loaded` 的生命周期由进程级 `CACHE` 保证。
unsafe impl Send for Loaded2 {}
unsafe impl Sync for Loaded2 {}

fn recognizer_identity(config: &EngineConfig) -> String {
    format!(
        "{}|{}|{}|{}|{}",
        config.model_identity(),
        config.language.as_deref().unwrap_or("auto"),
        config.num_threads,
        config.itn,
        config.engine_lib
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "<默认库名>".to_owned())
    )
}

fn library_path_text(config: &EngineConfig) -> String {
    config
        .engine_lib
        .as_ref()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| format!("<按名字加载 {}>", crate::provider::local_engine::engine_lib_file_name()))
}

fn load_library(config: &EngineConfig) -> Result<libloading::Library> {
    let candidates: Vec<String> = match &config.engine_lib {
        Some(path) => vec![path.to_string_lossy().into_owned()],
        None => vec![crate::provider::local_engine::engine_lib_file_name().to_owned()],
    };
    let mut last_error = String::new();
    for candidate in &candidates {
        // SAFETY: 动态库由我们自己提供/随包分发；加载本身不会执行任意代码之外的
        // 东西（库的初始化函数会跑，这与链接期加载等价）。
        match unsafe { libloading::Library::new(candidate) } {
            Ok(library) => return Ok(library),
            Err(error) => last_error = error.to_string(),
        }
    }
    Err(CoreError::ModelUnavailable {
        reason: format!(
            "加载本机转写引擎库失败：{}（试过 {}）。随包分发要把引擎库与 {} 放在一起，\
             或用 {} 显式指定路径",
            last_error,
            candidates.join(" / "),
            onnxruntime_file_name(),
            crate::provider::local_engine::ENV_ENGINE_LIB
        ),
    })
}

impl Api {
    fn from_library(library: &libloading::Library) -> Result<Self> {
        // SAFETY: 符号名与签名按钉死的 c-api.h（v1.13.8）。取不到符号就是「这个库
        // 不是我们认识的那个引擎」，如实报错而不是继续用半套符号。
        unsafe {
            let missing = |name: &str, error: libloading::Error| CoreError::ModelUnavailable {
                reason: format!(
                    "引擎库里没有预期的符号 {name}（{error}）；\
                     这一份 FFI 声明对应 sherpa-onnx C API {PINNED_C_API_VERSION}"
                ),
            };
            Ok(Self {
                create_recognizer: *library
                    .get(b"SherpaOnnxCreateOfflineRecognizer\0")
                    .map_err(|error| missing("SherpaOnnxCreateOfflineRecognizer", error))?,
                destroy_recognizer: *library
                    .get(b"SherpaOnnxDestroyOfflineRecognizer\0")
                    .map_err(|error| missing("SherpaOnnxDestroyOfflineRecognizer", error))?,
                create_stream: *library
                    .get(b"SherpaOnnxCreateOfflineStream\0")
                    .map_err(|error| missing("SherpaOnnxCreateOfflineStream", error))?,
                destroy_stream: *library
                    .get(b"SherpaOnnxDestroyOfflineStream\0")
                    .map_err(|error| missing("SherpaOnnxDestroyOfflineStream", error))?,
                accept_waveform: *library
                    .get(b"SherpaOnnxAcceptWaveformOffline\0")
                    .map_err(|error| missing("SherpaOnnxAcceptWaveformOffline", error))?,
                decode: *library
                    .get(b"SherpaOnnxDecodeOfflineStream\0")
                    .map_err(|error| missing("SherpaOnnxDecodeOfflineStream", error))?,
                result_json: *library
                    .get(b"SherpaOnnxGetOfflineStreamResultAsJson\0")
                    .map_err(|error| missing("SherpaOnnxGetOfflineStreamResultAsJson", error))?,
                destroy_result_json: *library
                    .get(b"SherpaOnnxDestroyOfflineStreamResultJson\0")
                    .map_err(|error| {
                        missing("SherpaOnnxDestroyOfflineStreamResultJson", error)
                    })?,
                version: *library
                    .get(b"SherpaOnnxGetVersionStr\0")
                    .map_err(|error| missing("SherpaOnnxGetVersionStr", error))?,
            })
        }
    }

    fn version_string(&self) -> String {
        // SAFETY: 引擎返回的是静态字符串；空指针按「没有版本」处理。
        unsafe {
            let pointer = (self.version)();
            if pointer.is_null() {
                return "<未知>".to_owned();
            }
            CStr::from_ptr(pointer).to_string_lossy().into_owned()
        }
    }
}

/// 组装 C 侧配置。**只写我们声明并自检过偏移量的那几个字段**。
fn build_config(
    config: &EngineConfig,
    sample_rate: u32,
    strings: &ConfigStrings,
) -> OfflineRecognizerConfig {
    let mut raw = OfflineRecognizerConfig::zeroed();
    raw.feat_config = FeatureConfig {
        sample_rate: sample_rate as i32,
        feature_dim: 80,
    };
    raw.model_config.tokens = strings.tokens.as_ptr();
    raw.model_config.num_threads = config.num_threads;
    raw.model_config.debug = 0;
    raw.model_config.provider = strings.cpu.as_ptr();
    match config.kind {
        ModelKind::SenseVoice => {
            raw.model_config.sense_voice = SenseVoiceModelConfig {
                model: strings.model.as_ptr(),
                language: strings
                    .language
                    .as_ref()
                    .map(|value| value.as_ptr())
                    .unwrap_or(std::ptr::null()),
                use_itn: i32::from(config.itn),
            };
            raw.model_config.model_type = strings.sense_voice.as_ptr();
        }
        ModelKind::Paraformer => {
            raw.model_config.paraformer = ParaformerModelConfig {
                model: strings.model.as_ptr(),
            };
            raw.model_config.model_type = strings.paraformer.as_ptr();
        }
    }
    raw.decoding_method = strings.greedy.as_ptr();
    raw.max_active_paths = 4;
    raw
}

/// 配置里那些 C 字符串的**所有者**：`build_config` 只放指针，字符串必须活到
/// `SherpaOnnxCreateOfflineRecognizer` 返回之后。
struct ConfigStrings {
    model: CString,
    tokens: CString,
    language: Option<CString>,
    cpu: CString,
    sense_voice: CString,
    paraformer: CString,
    greedy: CString,
}

impl ConfigStrings {
    fn new(config: &EngineConfig) -> Result<Self> {
        let model_path = config.model_path().map_err(|reason| {
            CoreError::ProviderNotConfigured {
                reason: format!("{reason}；本机转写引擎需要一个完整的模型目录"),
            }
        })?;
        let c_string = |value: &Path| -> Result<CString> {
            CString::new(value.to_string_lossy().as_bytes()).map_err(|_| {
                CoreError::InvalidArgument {
                    reason: format!("模型路径里有 NUL 字节：{}", value.display()),
                }
            })
        };
        Ok(Self {
            model: c_string(&model_path)?,
            tokens: c_string(&config.tokens_path())?,
            language: config
                .language
                .as_deref()
                .map(|value| CString::new(value).unwrap_or_default()),
            cpu: CString::new("cpu").expect("字面量"),
            sense_voice: CString::new("sense_voice").expect("字面量"),
            paraformer: CString::new("paraformer").expect("字面量"),
            greedy: CString::new("greedy_search").expect("字面量"),
        })
    }
}

fn create_recognizer(
    api: &Api,
    config: &EngineConfig,
) -> Result<*const OfflineRecognizer> {
    let strings = ConfigStrings::new(config)?;
    let raw = build_config(config, 16_000, &strings);
    // SAFETY: 配置结构体按钉死的 C API 版本构造，字符串在调用期间存活。
    let recognizer = unsafe { (api.create_recognizer)(&raw) };
    if recognizer.is_null() {
        return Err(CoreError::ModelUnavailable {
            reason: format!(
                "引擎创建识别器失败（模型 {}）。模型文件可能损坏、与引擎版本不匹配，\
                 或缺少 {}",
                config
                    .model_path()
                    .map(|path| path.display().to_string())
                    .unwrap_or_else(|_| config.model_dir.display().to_string()),
                onnxruntime_file_name()
            ),
        });
    }
    Ok(recognizer)
}

impl LocalEngine for SherpaOnnxEngine {
    fn id(&self) -> &'static str {
        SHERPA_ONNX
    }

    fn capabilities(&self) -> &'static [ProviderCapability] {
        &[ProviderCapability::TranscribeAudio]
    }

    fn readiness(&self, config: &EngineConfig) -> EngineReadiness {
        // 模型：必须真的在（一个空的 model 目录不算有模型）。
        if let Err(reason) = config.check_files() {
            return EngineReadiness {
                ready: false,
                code: "provider_not_configured".to_owned(),
                reason: format!(
                    "本机转写引擎没有可用的模型：{reason}（模型目录 {}）",
                    config.model_dir.display()
                ),
                next_step: Some(format!(
                    "把模型目录（model.int8.onnx 与 tokens.txt）放到 {}，\
                     或用 {} 指定别的目录，或在转写服务的配置里填 model=<目录>",
                    config.model_dir.display(),
                    crate::provider::local_engine::ENV_MODEL_DIR
                )),
            };
        }

        // 引擎库：`EngineConfig::resolve` 已经把「显式路径 → 可执行文件旁边的候选」
        // 解析过了。解析不到（`None`）在两处是正常的：Android 上按库名加载，
        // 由系统在应用的 native 库目录里解析；桌面端则是真的缺库，如实报。
        if let Some(path) = &config.engine_lib {
            if !path.is_file() {
                return EngineReadiness {
                    ready: false,
                    code: "provider_not_configured".to_owned(),
                    reason: format!("本机转写引擎的动态库不在：{}", path.display()),
                    next_step: Some(format!(
                        "把 {} 放到 {}，或用 {} 指向真实的库文件",
                        crate::provider::local_engine::engine_lib_file_name(),
                        path.display(),
                        crate::provider::local_engine::ENV_ENGINE_LIB
                    )),
                };
            }
        } else if !cfg!(target_os = "android") {
            return EngineReadiness {
                ready: false,
                code: "provider_not_configured".to_owned(),
                reason: format!(
                    "找不到本机转写引擎的动态库：可执行文件旁边没有 {}，也没有设 {}",
                    crate::provider::local_engine::engine_lib_file_name(),
                    crate::provider::local_engine::ENV_ENGINE_LIB
                ),
                next_step: Some(format!(
                    "把 {} 与 {} 一起放到可执行文件旁边，或用 {} 指定路径",
                    crate::provider::local_engine::engine_lib_file_name(),
                    onnxruntime_file_name(),
                    crate::provider::local_engine::ENV_ENGINE_LIB
                )),
            };
        }

        // 就绪的说明里带上「它打算用哪个库、哪个模型、ORT 在不在旁边」：
        // 出问题时用户照着这一行就能定位，不用猜。
        let lib_text = match &config.engine_lib {
            Some(path) => path.display().to_string(),
            None => format!(
                "按库名加载 {}（由系统在应用的 native 库目录里解析）",
                crate::provider::local_engine::engine_lib_file_name()
            ),
        };
        let model_text = config
            .model_path()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|_| config.model_dir.display().to_string());
        let ort_note = match &config.engine_lib {
            Some(path) => {
                let beside = path
                    .parent()
                    .map(|dir| dir.join(onnxruntime_file_name()))
                    .filter(|candidate| candidate.is_file());
                match beside {
                    Some(found) => format!("，同目录有 {}", found.display()),
                    None => format!(
                        "；注意：{} 旁边没看到 {}，随包分发要把它一起放（否则加载引擎时会失败）",
                        path.display(),
                        onnxruntime_file_name()
                    ),
                }
            }
            None => String::new(),
        };
        EngineReadiness {
            ready: true,
            code: String::new(),
            reason: format!(
                "本机转写引擎可用：库 {lib_text}，模型 {model_text}（{}）{ort_note}",
                config.kind.wire()
            ),
            next_step: None,
        }
    }

    fn transcribe(&self, config: &EngineConfig, request: &PcmRequest) -> Result<PcmOutcome> {
        // 没就绪就不要往下走：这一步保证「引擎缺失」永远不会变成「空文本成功」。
        let readiness = self.readiness(config);
        if !readiness.ready {
            return Err(CoreError::ProviderNotConfigured {
                reason: crate::provider::local_engine::blocked_message(&readiness),
            });
        }
        if request.samples.is_empty() {
            // 空音频不是一次转写：如实报「没东西可转」，而不是返回空串让调用方
            // 以为「转写成功但内容为空」。
            return Err(CoreError::InvalidArgument {
                reason: "转写请求里的采样是空的".to_owned(),
            });
        }
        if request.sample_rate == 0 {
            return Err(CoreError::InvalidArgument {
                reason: "转写请求的采样率是 0".to_owned(),
            });
        }
        if request.samples.len() > i32::MAX as usize {
            return Err(CoreError::InvalidArgument {
                reason: format!(
                    "一次转写的采样数 {} 超过引擎接口的上限（i32）",
                    request.samples.len()
                ),
            });
        }

        let mut config = config.clone();
        config.language = request.language.clone();
        let loaded = SherpaOnnxEngine::loaded(&config)?;
        // SAFETY: `loaded` 借用的是进程级缓存里的识别器；调用串行（见 unsafe impl）。
        let loaded = unsafe { &*loaded.loaded };

        // SAFETY: 下面每个调用都成对（create/destroy），且参数按 C API 契约给。
        unsafe {
            let stream = (loaded.api.create_stream)(loaded.recognizer);
            if stream.is_null() {
                return Err(CoreError::ModelUnavailable {
                    reason: "引擎创建解码流失败".to_owned(),
                });
            }
            (loaded.api.accept_waveform)(
                stream,
                request.sample_rate as i32,
                request.samples.as_ptr(),
                request.samples.len() as i32,
            );
            (loaded.api.decode)(loaded.recognizer, stream);
            let json_pointer = (loaded.api.result_json)(stream);
            let parsed = if json_pointer.is_null() {
                Err(CoreError::ModelUnavailable {
                    reason: "引擎没有给出转写结果（结果 JSON 是空指针）".to_owned(),
                })
            } else {
                let text = CStr::from_ptr(json_pointer).to_string_lossy().into_owned();
                parse_result_json(&text)
            };
            if !json_pointer.is_null() {
                (loaded.api.destroy_result_json)(json_pointer);
            }
            (loaded.api.destroy_stream)(stream);
            parsed
        }
        .map(|(text, language)| PcmOutcome {
            text,
            model: Some(format!("sherpa-onnx {} / {}", loaded.engine_version, loaded.model_label)),
            language,
        })
    }

    fn probe(&self, config: &EngineConfig) -> Result<String> {
        let readiness = self.readiness(config);
        if !readiness.ready {
            return Err(CoreError::ProviderNotConfigured {
                reason: crate::provider::local_engine::blocked_message(&readiness),
            });
        }
        // 真的加载模型、真的跑一次推理：转写 0.05 秒静音（与 B4a 的 HTTP 探针
        // 同一段音频，只是这里不上传）。**不联网**，也不把任何用户内容发出去。
        let samples = vec![0.0_f32; 800];
        let outcome = self.transcribe(
            config,
            &PcmRequest {
                samples,
                sample_rate: 8_000,
                language: None,
            },
        )?;
        // 静音探针转出空文本是**正常**的（没有语音）；这里只要求它没有报错。
        let loaded = SherpaOnnxEngine::loaded(config)?;
        // SAFETY: 缓存项在进程级存活。
        let loaded = unsafe { &*loaded.loaded };
        Ok(format!(
            "本机引擎（sherpa-onnx {}，FFI 按 {} 钉死）加载成功；模型 {}；\
             静音探针返回 {} 个字符（静音本来就可能是空的，这一次证明的是「模型能加载、\
             推理能跑通」）。**本次测试没有联网，也没有上传任何音频。**",
            loaded.engine_version,
            PINNED_C_API_VERSION,
            loaded.lib_path,
            outcome.text.chars().count()
        ))
    }
}

/// 解析 `SherpaOnnxGetOfflineStreamResultAsJson` 的 JSON。
///
/// 只取 `text` 与 `lang`：别的字段（token 时间戳等）现在没人消费，取出来就要
/// 承诺它们的语义——不取。
fn parse_result_json(text: &str) -> Result<(String, Option<String>)> {
    let value: serde_json::Value =
        serde_json::from_str(text).map_err(|error| CoreError::ModelUnavailable {
            reason: format!("引擎返回的结果不是合法 JSON（{error}）：{}", excerpt(text)),
        })?;
    let recognized = value
        .get("text")
        .and_then(|text| text.as_str())
        .ok_or_else(|| CoreError::ModelUnavailable {
            reason: format!("引擎结果里没有 text 字段：{}", excerpt(text)),
        })?;
    Ok((
        recognized.to_owned(),
        value
            .get("lang")
            .and_then(|lang| lang.as_str())
            .map(str::to_owned),
    ))
}

fn excerpt(text: &str) -> String {
    let mut out: String = text.chars().take(120).collect();
    if text.chars().count() > 120 {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// ABI 自检：这些数字是用真实头文件（v1.13.8 的 `c-api.h`）跑
    /// `offsetof` / `sizeof` 得到的：
    ///
    /// ```text
    /// gcc -o abi abi.c && ./abi          # abi.c 里 P(T,f) 打的就是下面这些
    /// ```
    ///
    /// **换引擎版本时这里必须重新生成并核对**——布局错了不会编译报错，
    /// 只会在运行时静默错位。
    #[test]
    fn abi_layout_matches_the_pinned_header() {
        use std::mem::{offset_of, size_of};

        assert_eq!(size_of::<FeatureConfig>(), 8);
        assert_eq!(offset_of!(FeatureConfig, sample_rate), 0);
        assert_eq!(offset_of!(FeatureConfig, feature_dim), 4);

        assert_eq!(size_of::<ParaformerModelConfig>(), 8);
        assert_eq!(size_of::<SenseVoiceModelConfig>(), 24);
        assert_eq!(size_of::<LmConfig>(), 16);
        assert_eq!(size_of::<HomophoneReplacerConfig>(), 24);

        assert_eq!(size_of::<OfflineModelConfig>(), 504);
        assert_eq!(offset_of!(OfflineModelConfig, transducer), 0);
        assert_eq!(offset_of!(OfflineModelConfig, paraformer), 24);
        assert_eq!(offset_of!(OfflineModelConfig, tokens), 96);
        assert_eq!(offset_of!(OfflineModelConfig, num_threads), 104);
        assert_eq!(offset_of!(OfflineModelConfig, debug), 108);
        assert_eq!(offset_of!(OfflineModelConfig, provider), 112);
        assert_eq!(offset_of!(OfflineModelConfig, model_type), 120);
        assert_eq!(offset_of!(OfflineModelConfig, modeling_unit), 128);
        assert_eq!(offset_of!(OfflineModelConfig, sense_voice), 152);
        assert_eq!(offset_of!(OfflineModelConfig, moonshine), 176);
        assert_eq!(offset_of!(OfflineModelConfig, fire_red_asr), 216);
        assert_eq!(offset_of!(OfflineModelConfig, dolphin), 232);
        assert_eq!(offset_of!(OfflineModelConfig, zipformer_ctc), 240);
        assert_eq!(offset_of!(OfflineModelConfig, canary), 248);
        assert_eq!(offset_of!(OfflineModelConfig, wenet_ctc), 288);
        assert_eq!(offset_of!(OfflineModelConfig, omnilingual), 296);
        assert_eq!(offset_of!(OfflineModelConfig, medasr), 304);
        assert_eq!(offset_of!(OfflineModelConfig, funasr_nano), 312);
        assert_eq!(offset_of!(OfflineModelConfig, fire_red_asr_ctc), 400);
        assert_eq!(offset_of!(OfflineModelConfig, qwen3_asr), 408);
        assert_eq!(offset_of!(OfflineModelConfig, cohere_transcribe), 472);

        assert_eq!(size_of::<OfflineRecognizerConfig>(), 608);
        assert_eq!(offset_of!(OfflineRecognizerConfig, feat_config), 0);
        assert_eq!(offset_of!(OfflineRecognizerConfig, model_config), 8);
        assert_eq!(offset_of!(OfflineRecognizerConfig, lm_config), 512);
        assert_eq!(offset_of!(OfflineRecognizerConfig, decoding_method), 528);
        assert_eq!(offset_of!(OfflineRecognizerConfig, max_active_paths), 536);
        assert_eq!(offset_of!(OfflineRecognizerConfig, hotwords_score), 552);
        assert_eq!(offset_of!(OfflineRecognizerConfig, blank_penalty), 576);
        assert_eq!(offset_of!(OfflineRecognizerConfig, hr), 584);
    }

    #[test]
    fn result_json_is_parsed_without_inventing_fields() {
        let (text, language) = parse_result_json(r#"{"text":"今天下雨了","lang":"zh"}"#).unwrap();
        assert_eq!(text, "今天下雨了");
        assert_eq!(language.as_deref(), Some("zh"));

        // 没有 text 字段：报错，不返回空串（空串会被上层当成「引擎说这里没内容」）。
        let error = parse_result_json(r#"{"tokens":[]}"#).unwrap_err();
        assert_eq!(error.code(), crate::error::ErrorCode::ModelUnavailable);

        // 没有 lang 也算数（不是所有模型都给）。
        let (text, language) = parse_result_json(r#"{"text":"hi"}"#).unwrap();
        assert_eq!(text, "hi");
        assert!(language.is_none());
    }

    #[test]
    fn readiness_reports_missing_model_with_paths_and_next_step() {
        let config = EngineConfig {
            engine_lib: Some(PathBuf::from("/nonexistent/libsherpa-onnx-c-api.so")),
            model_dir: PathBuf::from("/nonexistent/model-dir"),
            model_file: None,
            kind: ModelKind::SenseVoice,
            language: None,
            num_threads: 2,
            itn: true,
        };
        let readiness = engine().readiness(&config);
        assert!(!readiness.ready);
        assert_eq!(readiness.code, "provider_not_configured");
        // 「缺什么」必须带路径，「怎么办」必须可执行。
        assert!(readiness.reason.contains("/nonexistent/model-dir"), "{}", readiness.reason);
        assert!(
            readiness
                .next_step
                .as_deref()
                .unwrap_or_default()
                .contains("DIARY_ASR_MODEL_DIR"),
            "{:?}",
            readiness.next_step
        );
    }
}
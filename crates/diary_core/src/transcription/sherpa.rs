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
//!
//! # 识别器的生命周期：缓存里放 `Arc`，不在用到它的人手上拿走
//!
//! 配置变化时要重建识别器，但**重建不等于把旧的那份立刻销毁**：可能正好有一次
//! 转写拿着它。所以：
//!
//! - [`CACHE`] 里放的是 `Arc<Cached<Loaded>>`，`loaded()` 交给调用方一份 `Arc`——
//!   转写期间识别器**一定**活着；
//! - 缓存替换只是「以后来的人用新的」；旧的那份等**最后一个**持有者放掉才
//!   `destroy`（[`RecognizerHandle`] 的 `Drop`），而且不在缓存锁里做这次 C 调用；
//! - 因此「一次长转写」不会挡住模型切换（切换照样立刻生效，新的请求用新模型），
//!   也不会把正在用的识别器销毁（过去返回裸指针 + `cache.take()` 直接销毁，
//!   是 use-after-free，见 issue #103）。
//!
//! 缓存层保证的只是**对象不会在持有者使用期间被销毁**；它不承诺「同一个识别器
//! 可以被两个线程同时拿去解码」——那条仍是核心「转写调用串行」的约定，
//! 写在 [`Loaded`] 的 `Send`/`Sync` 说明里。

use std::ffi::{c_char, c_void, CStr, CString};
use std::path::Path;
use std::sync::{Arc, Mutex};

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
///
/// 缓存项是 `Arc`（见模块文档「识别器的生命周期」）：取用方手里那一份**不会**
/// 因为缓存被替换而销毁。
static CACHE: EngineCache<Loaded> = EngineCache::new();

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
    /// 动态库路径（报告里印出来）。
    lib_path: String,
    /// 引擎版本（`SherpaOnnxGetVersionStr`）。
    engine_version: String,
    /// 模型文件的可读标签（报告与派生内容版本里用）。
    model_label: String,
    /// 识别器：它的销毁由 [`RecognizerHandle`] 的 `Drop` 负责，
    /// 也就是「最后一个持有者放掉它」这一刻，而不是「从缓存里摘下来」这一刻。
    recognizer: RecognizerHandle,
    /// 库句柄：**不 drop**，所以它一直活着（用 `ManuallyDrop` 表达这个意图）。
    _library: std::mem::ManuallyDrop<libloading::Library>,
    api: Api,
}

/// 识别器的**所有者**：最后一个持有者放掉它时销毁 C 对象。
///
/// 单独成一个类型，是为了让「谁负责销毁识别器」只有这一个出口，并且能单测：
/// 测试用一个只记数的 `destroy` 桩替掉真引擎（见本文件测试）。
struct RecognizerHandle {
    pointer: *const OfflineRecognizer,
    destroy: unsafe extern "C" fn(*const OfflineRecognizer),
}

impl Drop for RecognizerHandle {
    fn drop(&mut self) {
        if self.pointer.is_null() {
            return;
        }
        // SAFETY: `pointer` 由 `SherpaOnnxCreateOfflineRecognizer` 创建，除了这里
        // 没有任何别的销毁路径（`Drop` 只会跑一次），所以不会重复销毁；库句柄进程级
        // 保留、永不 dlclose，所以 `destroy` 这个函数指针在调用时一定还有效。
        unsafe { (self.destroy)(self.pointer) };
    }
}

// SAFETY: 与 `Loaded` 同一条边界（见下面的说明）：识别器在 sherpa-onnx 里是只读的
// 推理对象，每次调用的可变状态是 stream，而 stream 在每次 `transcribe` 内部创建销毁。
// `Send + Sync` 表达的只是「可以跨线程传递这份所有权」，不改变「核心的转写调用串行」
// 这条约定。
unsafe impl Send for RecognizerHandle {}
unsafe impl Sync for RecognizerHandle {}

// SAFETY: 这里允许裸指针跨线程的边界与**没有**被这次修改放宽的地方：
// - 本类型里有裸指针（识别器）与函数指针，所以 `Send + Sync` 必须手动声明；
// - 识别器本身在 sherpa-onnx 里是只读的推理对象，`SherpaOnnxOfflineStream` 才是
//   每次调用的可变状态，而它**在每次 `transcribe` 内部创建并销毁**，不共享；
// - **同一个识别器不会被两个线程同时解码**：核心对引擎的调用串行（`Core` 由
//   `BridgeSession` 的互斥锁串行化，作业驱动是「同一时刻只有一条任务在跑」）。
//   这是调用方的约定，不是缓存生命周期的一部分——缓存层保证的是「对象不会在
//   持有者使用期间被销毁」（`Arc`），不保证「同一识别器并发解码安全」。
// 结论：把指针标成 `Send + Sync` 不改变运行时的并发事实，只是把上面这套约束写进
// 类型系统。若将来核心改成多线程并发转写，**必须重新评估这一条**。
unsafe impl Send for Loaded {}
unsafe impl Sync for Loaded {}

impl SherpaOnnxEngine {
    /// 取得（必要时加载并缓存）引擎。
    ///
    /// 返回 `Arc<Cached<Loaded>>`：**这份引用活多久由调用方决定**。一次转写期间
    /// 它一直活着，所以识别器一定活着——即便同时有别的调用把缓存换成了另一个配置
    /// 下的识别器。既不返回裸指针（过去那样会 use-after-free），也不在转写期间
    /// 一直握着 `CACHE` 的锁（那样长转写会把模型切换挡住）。
    fn loaded(config: &EngineConfig) -> Result<Arc<Cached<Loaded>>> {
        let identity = recognizer_identity(config);
        CACHE.get_or_build(
            &identity,
            || CoreError::InvalidState {
                entity: "本机转写引擎",
                id: SHERPA_ONNX.to_owned(),
                state: "引擎缓存的锁坏了（上一次转写可能 panic 过）".to_owned(),
            },
            || {
                // 配置变了（或第一次）：把新的一套**完整建好**（库、符号、识别器）之后
                // 才让缓存指过去。顺序不能反——反了会出现「旧的没了、新的没建起来」的空窗。
                let library = load_library(config)?;
                let api = Api::from_library(&library)?;
                let engine_version = api.version_string();
                let recognizer = RecognizerHandle {
                    pointer: create_recognizer(&api, config)?,
                    destroy: api.destroy_recognizer,
                };
                let model_label = config
                    .model_path()
                    .map(|path| {
                        path.file_name()
                            .map(|name| name.to_string_lossy().into_owned())
                            .unwrap_or_else(|| path.display().to_string())
                    })
                    .unwrap_or_else(|_| "<未知模型>".to_owned());
                Ok(Loaded {
                    lib_path: library_path_text(config),
                    engine_version,
                    model_label,
                    recognizer,
                    _library: std::mem::ManuallyDrop::new(library),
                    api,
                })
            },
        )
    }
}

/// 缓存里的一项：配置指纹 + 引擎对象。
struct Cached<T> {
    /// 配置指纹（见 [`recognizer_identity`]）：变了就重建。
    identity: String,
    value: T,
}

/// 「配置指纹 → 进程级单例」的缓存骨架。
///
/// 它保证的核心性质正是 issue #103 要修的那条：**取出的对象在使用期间一定活着**。
/// 取用方拿到的是 `Arc`；缓存被替换只意味着「以后来的人用新的」，不会销毁正在被
/// 用着的那一份。销毁发生在最后一个持有者放掉它的那一刻（`T` 自己的 `Drop`），
/// 而且**不在缓存锁里**做。
///
/// 这段逻辑单独成一个类型，是为了让它能用假的「引擎对象」单测（同一段代码路径）。
struct EngineCache<T> {
    slot: Mutex<Option<Arc<Cached<T>>>>,
}

impl<T> EngineCache<T> {
    const fn new() -> Self {
        Self {
            slot: Mutex::new(None),
        }
    }

    /// 取当前项；指纹与缓存里的不一样（或缓存是空的）才用 `build` 建一个新的。
    ///
    /// `build` 在锁内跑：两个线程同时发现配置变了时，不会各建一个、互相覆盖，也不会
    /// 出现「旧的摘了、新的还没建起来」的空窗。锁坏了（上一次在锁里 panic）时用
    /// `poisoned` 造错误返回，措辞归调用方。
    fn get_or_build(
        &self,
        identity: &str,
        poisoned: impl FnOnce() -> CoreError,
        build: impl FnOnce() -> Result<T>,
    ) -> Result<Arc<Cached<T>>> {
        let mut slot = self.slot.lock().map_err(|_| poisoned())?;
        let needs_rebuild = slot
            .as_ref()
            .map(|cached| cached.identity != identity)
            .unwrap_or(true);
        let mut replaced = None;
        if needs_rebuild {
            let value = build()?;
            replaced = slot.replace(Arc::new(Cached {
                identity: identity.to_owned(),
                value,
            }));
        }
        let current = Arc::clone(slot.as_ref().expect("上面刚保证缓存里有一项"));
        // 先放锁，再放掉旧项：旧项的最后一次 `Drop` 会销毁识别器（一次 C 调用），
        // 不该占着缓存锁做。而且这时它可能还被某次转写用着——我们放掉的只是**自己
        // 这一份引用**，对象由最后一个持有者决定什么时候真正销毁。
        drop(slot);
        drop(replaced);
        Ok(current)
    }
}

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
    // **「加载不到引擎库」是配置/打包问题，不是「模型坏了」**（PR #130 评审的 P0）。
    //
    // 在 OHOS/Android 上引擎库按库名从应用的 native 库目录解析，核心**没法 stat**，
    // 所以 `readiness` 在 `engine_lib = None` 时只能乐观放行；真正的缺口在这里才
    // 暴露。以前这里报 `ModelUnavailable`，作业层按「终态失败」处置（不重试、不等
    // 配置）——可用户/打包侧补上 `libs/arm64` 里的库之后它是能好的。所以落
    // `provider_not_configured`：作业进 `waiting_configuration`、不消耗重试额度、
    // `requires_user_action` 如实置位。缺符号（库里没这个 API）仍归 `ModelUnavailable`：
    // 那是「装了一个不对的引擎」，与「没装引擎」不是同一件事。
    Err(CoreError::ProviderNotConfigured {
        reason: format!(
            "加载本机转写引擎库失败：{}（试过 {}）。引擎库必须随 HAP 打进 \
             libs/arm64/（OHOS 的应用数据目录里的 .so 不能 dlopen），且要与 {} \
             放在一起；也可以用 {} 显式指定它的绝对路径",
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

/// 「按库名加载的引擎库到底能不能加载」的进程级缓存。
///
/// 为什么 readiness 必须知道这件事：OHOS/Android 的引擎库按库名从应用的 native 库
/// 目录解析，核心**没法 stat 它**。以前这里直接乐观放行，结果是
/// 「闸门说 ready → 执行者 dlopen 失败 → 作业落 waiting_configuration → 闸门下一轮又
/// 说 ready（`promote_configuration_waits` 把它拉回队列）→ 再失败……」——一轮
/// `jobs.runDue` 会在同一个任务上空转 64 次。所以闸门必须**真的**摸一次库。
///
/// 代价与取舍：
/// - 只在「解析不到显式路径」（移动端按库名）时做，桌面端仍旧只看候选文件；
/// - 结果进 `OnceLock`：**一个进程只探测一次**，之后是纯内存判断；
/// - 成功时 `mem::forget` 句柄：不解引用、不 dlclose。与「ORT 会话退出不释放」同一个
///   理由——dlclose 会跑 C++ 静态析构，退出阶段容易炸；留着等于预加载，执行者之后再
///   dlopen 只是引用计数 +1。
static ENGINE_BY_NAME: std::sync::OnceLock<std::result::Result<(), String>> =
    std::sync::OnceLock::new();

/// 已经探测过的话，返回缓存结论；**不触发**探测。
///
/// 给 `core.info` 用：它不能为了报状态去 dlopen（启动路径），但探测过一次之后
/// （第一次任务/连接测试），状态就该如实变成「可用 / 加载不了」，而不是一直停在
/// 「未探测」。
pub(crate) fn engine_library_by_name_probe() -> Option<&'static std::result::Result<(), String>> {
    ENGINE_BY_NAME.get()
}

fn engine_library_by_name_ready() -> &'static std::result::Result<(), String> {
    ENGINE_BY_NAME.get_or_init(|| {
        let name = crate::provider::local_engine::engine_lib_file_name();
        // SAFETY: 引擎库由我们随包分发；这里只加载、不取符号、不调用任何函数。
        match unsafe { libloading::Library::new(name) } {
            Ok(library) => {
                std::mem::forget(library);
                Ok(())
            }
            Err(error) => Err(error.to_string()),
        }
    })
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
        // 解析过了。解析不到（`None`）在两类平台上是正常的：Android 与 OHOS
        // 都按库名加载，由系统在应用的 native 库目录（OHOS 是 `libs/arm64`）里
        // 解析——引擎随 HAP 分发时就在那儿；桌面端则是真的缺库，如实报。
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
        } else if cfg!(any(target_os = "android", target_env = "ohos")) {
            // 按库名加载：核心没法 stat，只能**真的**试一次 dlopen（进程内只做一次）。
            // 不做这一步的话，闸门会在缺库时一直说「可用」，作业被反复拉起又失败。
            if let Err(error) = engine_library_by_name_ready() {
                return EngineReadiness {
                    ready: false,
                    code: "provider_not_configured".to_owned(),
                    reason: format!(
                        "本机转写引擎的动态库加载不了：按库名找 {} 失败（{error}）。\
                         引擎库必须随安装包放在应用的 libs/<abi>/ 里，且与 {} 一起",
                        crate::provider::local_engine::engine_lib_file_name(),
                        onnxruntime_file_name()
                    ),
                    next_step: Some(format!(
                        "把 {} 与 {} 打进 HAP 的 libs/arm64/（或用 {} 指定它们的绝对路径）；\
                         模型文件放应用沙箱",
                        crate::provider::local_engine::engine_lib_file_name(),
                        onnxruntime_file_name(),
                        crate::provider::local_engine::ENV_ENGINE_LIB
                    )),
                };
            }
        } else {
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
        // `cached` 在**整个转写期间**都被持有（不是只从缓存里取个指针）：即便别的
        // 调用同时切换了模型、把缓存里的那一份换掉，这一份识别器也一定活到本次
        // 转写结束（对象由持有者决定，见模块文档「识别器的生命周期」）。
        let cached = SherpaOnnxEngine::loaded(&config)?;
        let loaded = &cached.value;

        // SAFETY: 下面每个调用都成对（create/destroy），且参数按 C API 契约给。
        unsafe {
            let stream = (loaded.api.create_stream)(loaded.recognizer.pointer);
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
            (loaded.api.decode)(loaded.recognizer.pointer, stream);
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
        let cached = SherpaOnnxEngine::loaded(config)?;
        let loaded = &cached.value;
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
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::mpsc;
    use std::thread;

    // ------------------------------------------------ 识别器生命周期（#103）

    /// 测试用的假「识别器对象」：销毁时留痕。
    ///
    /// 本体就是这块 `Arc` 的分配；[`fake_recognizer`] 把它的裸指针交给
    /// [`RecognizerHandle`]，销毁时由桩函数把它标成「已销毁」。
    ///
    /// 这条测试不需要（也不能有）真的 sherpa-onnx 库与模型：被验的是**生命周期**
    /// ——对象会不会在还有人用的时候被销毁——而不是推理结果。
    #[derive(Default)]
    struct FakeRecognizerState {
        alive: AtomicBool,
        destroys: AtomicUsize,
        /// 进程内建过的所有假识别器共用的销毁总数（核对「建几份就销毁几份」）。
        total_destroys: Option<Arc<AtomicUsize>>,
    }

    impl FakeRecognizerState {
        fn new(total_destroys: &Arc<AtomicUsize>) -> Arc<Self> {
            Arc::new(Self {
                alive: AtomicBool::new(true),
                destroys: AtomicUsize::new(0),
                total_destroys: Some(Arc::clone(total_destroys)),
            })
        }
    }

    /// 桩：签名与真的 `SherpaOnnxDestroyOfflineRecognizer` 一致，只记数、不碰引擎。
    unsafe extern "C" fn fake_destroy(pointer: *const OfflineRecognizer) {
        if pointer.is_null() {
            return;
        }
        // SAFETY: 这个指针只可能来自 `fake_recognizer` 的 `Arc::into_raw`，而
        // `RecognizerHandle::drop` 只会调用一次、之后不再使用它；这里把那份引用收回。
        let state = unsafe { Arc::from_raw(pointer as *const FakeRecognizerState) };
        state.alive.store(false, Ordering::SeqCst);
        state.destroys.fetch_add(1, Ordering::SeqCst);
        if let Some(total) = &state.total_destroys {
            total.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn fake_recognizer(state: &Arc<FakeRecognizerState>) -> RecognizerHandle {
        RecognizerHandle {
            pointer: Arc::into_raw(Arc::clone(state)) as *const OfflineRecognizer,
            destroy: fake_destroy,
        }
    }

    fn fake_poison() -> CoreError {
        CoreError::InvalidState {
            entity: "测试缓存",
            id: "fake".to_owned(),
            state: "锁坏了".to_owned(),
        }
    }

    /// #103 的另一半是**签名**：转写拿到的必须是「可以一直持有的引用」（`Arc`），
    /// 不是「指向缓存项的裸指针凭证」——后者根本没法表达「它在本次转写期间活着」，
    /// 那正是 use-after-free 的来源。
    ///
    /// 这条断言是编译期的：签名退回裸指针（或换成别的借用）它就编译不过。
    #[test]
    fn loaded_hands_out_an_owned_reference_that_outlives_a_cache_switch() {
        let _: fn(&EngineConfig) -> Result<Arc<Cached<Loaded>>> = SherpaOnnxEngine::loaded;
    }

    /// #103 的**确定性**回归：缓存被替换不能销毁正在被用的那一份。
    ///
    /// 「正在用」就是一个 `Arc`（转写从取到识别器到本次结束一直持有它）。这条断言在
    /// 「返回裸指针 + 换配置时直接销毁旧识别器」的实现下必红：那时换配置的一刻
    /// `alive` 会变成 false、`destroys` 会变成 1。
    #[test]
    fn switching_the_model_cache_keeps_the_recognizer_in_use_alive() {
        let total = Arc::new(AtomicUsize::new(0));
        let cache: EngineCache<RecognizerHandle> = EngineCache::new();
        let first_state = FakeRecognizerState::new(&total);

        let first = cache
            .get_or_build("配置-A", fake_poison, || Ok(fake_recognizer(&first_state)))
            .expect("第一次取用会建一份");
        assert_eq!(first.identity, "配置-A");

        // 指纹没变：复用同一份，不重建（`build` 直接 panic 来证明它没被调用）。
        let again = cache
            .get_or_build("配置-A", fake_poison, || unreachable!("指纹没变就不该重建"))
            .expect("复用缓存里那一份");
        assert!(Arc::ptr_eq(&first, &again), "指纹没变时应当拿到同一份");
        assert_eq!(total.load(Ordering::SeqCst), 0);
        drop(again);

        // 另一个配置：缓存换成 B（模拟另一个 Core/线程切换模型）。
        let second_state = FakeRecognizerState::new(&total);
        let second = cache
            .get_or_build("配置-B", fake_poison, || Ok(fake_recognizer(&second_state)))
            .expect("换配置会建一份");
        assert_eq!(second.identity, "配置-B");

        // A 还在这条「转写」手里：它必须还活着，一次都没被销毁过。
        assert!(
            first_state.alive.load(Ordering::SeqCst),
            "换模型把正在用的识别器销毁了（use-after-free）"
        );
        assert_eq!(
            first_state.destroys.load(Ordering::SeqCst),
            0,
            "换模型只该把旧的那份从缓存里摘下来，不该销毁它"
        );
        assert_eq!(
            Arc::strong_count(&first),
            1,
            "旧的那份此时只有这次「转写」持有"
        );
        assert_eq!(total.load(Ordering::SeqCst), 0, "一份都不该被销毁");

        // 转写结束：放下这份引用，**这时**才销毁，且恰好一次。
        drop(first);
        assert!(
            !first_state.alive.load(Ordering::SeqCst),
            "最后一个持有者放掉之后才销毁"
        );
        assert_eq!(first_state.destroys.load(Ordering::SeqCst), 1);
        assert_eq!(second_state.destroys.load(Ordering::SeqCst), 0, "B 还在缓存里");

        drop(second);
        assert_eq!(
            second_state.destroys.load(Ordering::SeqCst),
            0,
            "缓存里还留着一份引用，B 不该被销毁"
        );
        // 缓存本身放掉之后，最后一份引用才消失。
        drop(cache);
        assert_eq!(second_state.destroys.load(Ordering::SeqCst), 1);
        assert_eq!(total.load(Ordering::SeqCst), 2, "建两份就销毁两份，不多不少");
    }

    /// #103 的**并发**回归：一个线程在转写，另一个线程不断切换模型缓存。
    ///
    /// 时序是确定的，不靠 sleep 碰运气：转写线程先声明「识别器已经在手」，之后主线程
    /// 连切 [`SWITCHES`] 次配置（每次切完都断言那一份还活着），切完才放行转写线程。
    /// 转写线程在整个窗口里反复「使用」它，并在结束时放掉引用。最后核对
    /// 「建过几份就销毁几份」——漏销毁是泄漏，多销毁/提前销毁就是 use-after-free。
    #[test]
    fn transcribing_while_another_thread_switches_models_never_uses_a_destroyed_engine() {
        const SWITCHES: usize = 64;
        /// 兜底：主线程如果先失败退出，转写线程不会无限空转；按**单调时钟**算，
        /// 给的余量足够大（这条测试本身是纯内存的，正常远在毫秒级结束）。
        const RELEASE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

        let total = Arc::new(AtomicUsize::new(0));
        let cache = Arc::new(EngineCache::<RecognizerHandle>::new());
        let first_state = FakeRecognizerState::new(&total);

        let (in_use_tx, in_use_rx) = mpsc::channel::<()>();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let transcriber = {
            let cache = Arc::clone(&cache);
            let state = Arc::clone(&first_state);
            thread::spawn(move || {
                let in_use = cache
                    .get_or_build("配置-A", fake_poison, || Ok(fake_recognizer(&state)))
                    .expect("转写取到识别器");
                in_use_tx.send(()).expect("通知主线程：识别器已经在手");
                let deadline = std::time::Instant::now() + RELEASE_TIMEOUT;
                while release_rx.try_recv().is_err() {
                    // 「转写」：整个窗口里反复碰这一份对象。
                    assert!(
                        state.alive.load(Ordering::SeqCst),
                        "转写还在进行，识别器就被销毁了（use-after-free）"
                    );
                    assert!(
                        std::time::Instant::now() < deadline,
                        "主线程一直没有放行转写线程"
                    );
                    thread::yield_now();
                }
                drop(in_use);
            })
        };

        in_use_rx.recv().expect("转写线程应当先拿到识别器");
        for index in 0..SWITCHES {
            let state = FakeRecognizerState::new(&total);
            let identity = format!("配置-切换-{index}");
            let switched = cache
                .get_or_build(&identity, fake_poison, || Ok(fake_recognizer(&state)))
                .expect("切换配置");
            assert_eq!(switched.identity, identity);
            assert!(
                first_state.alive.load(Ordering::SeqCst),
                "第 {index} 次切换销毁了正在被转写用的识别器"
            );
            assert_eq!(
                first_state.destroys.load(Ordering::SeqCst),
                0,
                "第 {index} 次切换销毁了正在被转写用的识别器"
            );
        }
        release_tx.send(()).expect("放行转写线程");
        transcriber.join().expect("转写线程正常结束");
        assert_eq!(
            first_state.destroys.load(Ordering::SeqCst),
            1,
            "转写结束之后它才该被销毁，且恰好一次"
        );
        assert!(!first_state.alive.load(Ordering::SeqCst));

        // 到这里缓存里还留着最后一份：放掉整个缓存后，建过的每一份都应当被销毁。
        assert_eq!(total.load(Ordering::SeqCst), SWITCHES, "只有转写那一份被销毁了");
        drop(cache);
        assert_eq!(
            total.load(Ordering::SeqCst),
            SWITCHES + 1,
            "建过 {} 份就该销毁 {} 份（漏销毁是泄漏，多销毁是双重释放）",
            SWITCHES + 1,
            SWITCHES + 1
        );
    }

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

    /// PR #130 评审的 P0：**引擎库加载不到 = 等配置**，不是终态失败。
    ///
    /// 以前这里是 `ModelUnavailable`，作业层按「终态 failed」处置：用户/打包侧把
    /// `libs/arm64` 里的库补上之后任务也不会再跑（不重试、不等配置）。改动后是
    /// `provider_not_configured` → `waiting_configuration`，配置好了下一轮就能拉起。
    #[test]
    fn a_missing_engine_library_is_a_configuration_problem_not_an_unavailable_model() {
        let config = EngineConfig {
            engine_lib: Some(PathBuf::from("/nonexistent/libsherpa-onnx-c-api.so")),
            model_dir: PathBuf::from("/nonexistent/model-dir"),
            model_file: None,
            kind: ModelKind::SenseVoice,
            language: None,
            num_threads: 2,
            itn: true,
        };
        let error = load_library(&config).expect_err("库不在时必须失败");
        assert!(
            matches!(error, CoreError::ProviderNotConfigured { .. }),
            "必须是等配置（可恢复），不是 ModelUnavailable（终态）：{error:?}"
        );
        assert_eq!(error.code(), crate::ErrorCode::ProviderNotConfigured);
        assert!(error.waits_for_configuration(), "作业要落 waiting_configuration");
        assert!(
            error.to_string().contains("libs/arm64"),
            "错误信息要说清引擎库该放哪儿：{error}"
        );
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
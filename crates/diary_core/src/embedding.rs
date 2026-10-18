//! 本地向量模型接入：bge-small-zh-v1.5（我们自导出的 ONNX）。
//!
//! 这一片把 M0 已经验过的 ONNX + tokenizers 代码（`tools/probe/src/model.rs`）
//! **移植**进核心：会话构建、tokenize、pooling、L2 归一化。移植而不是重写，是为了
//! 让「探针量到的数字」与「产品路径的行为」是同一份实现——否则量测与线上永远是
//! 两件事。
//!
//! # 生命周期照 #17 的结论（不要改）
//!
//! 模型会话放**进程级单例**，**退出时不释放**：
//!
//! - 不 `drop`、不 `mem::forget`、不在任何退出路径上调 ORT 的收尾；
//! - `Session` 只建一次（省掉每次约 273 ms 的加载）；
//! - 释放会在**进程退出阶段**触发 `libonnxruntime.so` 的 C++ 静态析构，它随后去锁
//!   一个已经销毁的 mutex：桌面上表现为「全部输出之后 SIGSEGV（退出码 139）」，
//!   Android 是 `FORTIFY: pthread_mutex_lock called on a destroyed mutex` 之后 abort。
//!   实测排除了三种猜测，结论是「问题不在动态库被卸载，而在 session 被释放后 ORT 的
//!   退出收尾」——见 `docs/architecture/M0-技术验证.md` 3.3。
//!
//! 实现上就是：会话放进 `static` 里的 `HashMap`（Rust 不跑 `static` 的析构），
//! `Core` 只拿一个 `Arc`。退出时由内核回收——这不是漏内存，是**刻意不释放**。
//!
//! # 输入长度与 token 截断（issue #109）
//!
//! bge-small-zh-v1.5 的 ONNX 图**位置编码按 512 固定**：序列一超过 512，ORT 会在
//! `Add` 节点上报 `Attempting to broadcast an axis by a dimension other than 1. 512 by N`
//! ——不是崩溃、也不是静默，是一条读不懂的前向错误。产品切块按**字符**（`MAX_CHUNK_CHARS
//! = 400`），在本仓库的分词器上 400 字最多 400 token（见
//! `docs/architecture/m2-质量集与分块.md` 的实测），但**入口不能假设调用方守规矩**：
//! 探针、量测、以后的其它入口都会直接把整段文本递进来。
//!
//! 所以入口统一做 **token 级保护**（`prepare_input`）：
//!
//! - 原样编码（含 `[CLS]` / `[SEP]`）不超过 512 → **一个字节都不改**，与修复前逐位相同；
//! - 超过 512 → 正文编码（不含特殊 token）截到 [`MAX_BODY_TOKENS`]（510），再按模板
//!   在两端补回 `[CLS]` / `[SEP]`，总长 512；
//! - 截断**不静默**：`Embedding::truncation` 如实给出 `original / kept / dropped` 三个
//!   计数，由上层（索引重建报告、检索警告）转达给用户。只打日志不算「报告」。
//!
//! 超过 512 **不再是一个错误**（契约里没有新错误码）：能截断就截断并报告；只有连
//! 特殊 token 都拿不到（分词器不可用）时才沿用既有的 `invalid_state` 并写清原因。
//!
//! # 退化路径（CI 里跑的就是这条）
//!
//! `.github/workflows/check.yml` 的机器上**没有模型文件、也没有 ORT 动态库**。
//! 所以：
//!
//! - 只读环境变量（`DIARY_MODEL_DIR` / `DIARY_ORT_DYLIB`），**不**在核心库的默认
//!   路径里硬编码仓库里的模型目录——库里没有「仓库根目录」这个概念；
//! - 一个都没设 → `NotConfigured`：这是**合法**的退化，关键词检索照旧，语义报「未就绪」；
//! - 设了但缺文件 / 加载失败 / 维度不符 → **如实报错**，不假装「未就绪」把错误盖掉。
//!
//! `ort` 用 `load-dynamic`：动态库在**运行时**才加载，构建期不需要它，也不需要下载
//! 二进制。所以 CI 能编过这份代码，只是不跑它。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use ort::session::Session;
use ort::value::Tensor;
use sha2::{Digest, Sha256};
use tokenizers::Tokenizer;

use crate::error::{CoreError, Result};
use crate::support;

/// bge-small-zh-v1.5 的向量维度。写死在这里当校验上界：拿错模型（比如 base）要在
/// 加载时就被挡住，而不是写进 `chunk_vectors.dims` 之后才被发现。
pub const EXPECTED_DIMS: usize = 512;

/// 模型包的名字（不含哈希）。真实的 `model_version` 还要拼上模型文件的 sha256 前缀。
pub const MODEL_ID: &str = "bge-small-zh-v1.5-ours";

/// 模型版本的哈希前缀长度（12 位十六进制）。
const MODEL_HASH_CHARS: usize = 12;

/// 模型图能接受的最长序列（**含** `[CLS]` 与 `[SEP]`）。
///
/// 这不是我们拍的：bge-small-zh-v1.5 导出的图把位置编码按 512 写死，超过就是
/// `Add` 节点的 broadcast 错误（issue #109 的现场）。改模型时这个数要跟着模型走。
pub const MAX_SEQ_TOKENS: usize = 512;

/// 截断时正文最多保留的 token 数：给 `[CLS]` / `[SEP]` 各留一位。
pub const MAX_BODY_TOKENS: usize = MAX_SEQ_TOKENS - 2;

/// 一次超长输入被 token 截断的**如实计数**（都要能传到上层，不能只打日志）。
///
/// `original_tokens` / `kept_tokens` 数的是**正文**（不含 `[CLS]` / `[SEP]`），
/// 所以 `original - kept == dropped`，不存在「特殊 token 也算进去」的歧义。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TokenTruncation {
    /// 截断前正文的 token 数。
    pub original_tokens: usize,
    /// 真正送进模型的正文 token 数（≤ [`MAX_BODY_TOKENS`]）。
    pub kept_tokens: usize,
    /// 被截掉的正文 token 数。
    pub dropped_tokens: usize,
}

impl std::fmt::Display for TokenTruncation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "截掉 {} 个 token（保留 {}/{}）",
            self.dropped_tokens, self.kept_tokens, self.original_tokens
        )
    }
}

/// 一次嵌入的结果：向量 + 截断情况。
///
/// 为什么不让 `embed_document` 只返回 `Vec<f32>` 再「在日志里说一声」：截断是有代价的
/// （这一段的后半截没有参与向量），调用方必须能把它转达给用户/记录进报告。返回结构体
/// 是唯一不靠约定俗成的做法。
#[derive(Debug, Clone, PartialEq)]
pub struct Embedding {
    pub vector: Vec<f32>,
    /// `None` = 没有超限、输入逐位未动；`Some` = 这次真的截掉了这么多 token。
    pub truncation: Option<TokenTruncation>,
}

impl Embedding {
    /// 没有截断的结果。stub 嵌入器与测试用它。
    pub fn untruncated(vector: Vec<f32>) -> Self {
        Self {
            vector,
            truncation: None,
        }
    }
}

/// bge 早期版本建议的查询指令前缀。v1.5 的模型卡说**不需要**——但「说不需要」也要
/// 用我们自己的质量集量过（见 `docs/architecture/m2-向量索引与换代.md`）。
pub const QUERY_INSTRUCTION: &str = "为这个句子生成表示以用于检索相关文章：";

/// 池化方式。两者都能从**同一个前向输出**算出来，所以量测可以在一次会话里比。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pooling {
    /// 取 `[CLS]` 那一位（bge 模型卡推荐）。
    Cls,
    /// 只对 attention mask 为 1 的位置求平均（M0 探针当年用的）。
    Mean,
}

impl Pooling {
    pub fn wire(self) -> &'static str {
        match self {
            Self::Cls => "cls",
            Self::Mean => "mean",
        }
    }

    pub fn from_wire(value: &str) -> Option<Self> {
        match value {
            "cls" => Some(Self::Cls),
            "mean" => Some(Self::Mean),
            _ => None,
        }
    }
}

/// 一次 embedding 的口径。**产品路径与量测共用这一个类型**：量测改的是这两个开关，
/// 不是另一套代码。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EmbedOptions {
    pub pooling: Pooling,
    /// 查询侧是否加指令前缀。待索引正文**永远不加**。
    pub query_instruction: bool,
}

/// 产品默认口径：**按模型卡**取值（bge-small-zh-v1.5 用 CLS、查询侧不加指令前缀）。
///
/// 诚实说明：质量集上跑过四组对比（CLS/mean × 加/不加前缀），**差异只在一个用例以内**
/// ——`semantic_only` 6 条里 5/6 与 6/6 之差，36 条合成语料上不构成证据，所以这里不拿
/// 它当「量出来的最优」。唯一算得上信号的对比是 `mean + 指令前缀` 明显更差（R@1 81%、
/// `contradiction` 掉到 0%）：**前缀不是对谁都有用**。数字见
/// `docs/architecture/m2-向量索引与换代.md`「pooling 与指令前缀」；真实语料上要不要改口径，
/// 属于 B3c-3 之后的事。
pub const DEFAULT_POOLING: Pooling = Pooling::Cls;
/// 见 `DEFAULT_POOLING` 的说明。
pub const DEFAULT_QUERY_INSTRUCTION: bool = false;

impl Default for EmbedOptions {
    fn default() -> Self {
        Self {
            pooling: DEFAULT_POOLING,
            query_instruction: DEFAULT_QUERY_INSTRUCTION,
        }
    }
}

/// 嵌入器抽象。真实实现走 ORT，测试用确定性 stub（把文本哈希成 512 维）。
///
/// 为什么要抽象：**换代逻辑（开代次 → 写向量 → 原子切换 → 复用没变的块）必须在
/// 没有模型、没有 ORT 的 CI 里跑**。用 trait 把「算向量」这一步换掉，换代逻辑就能
/// 用确定性 stub 完整地测——包括「没变的块不重复算」这种只能靠数调用次数钉住的事。
pub trait Embedder: Send + Sync {
    /// 参与 `model_version` 的那一段：必须随模型文件变化（sha256 前缀），不能是常量。
    fn model_version(&self) -> &str;

    /// 向量维度。写进 `chunk_vectors.dims`，并与算出来的向量长度核对。
    fn dims(&self) -> usize;

    /// 这个嵌入器使用的口径（给状态与量测看）。
    fn options(&self) -> EmbedOptions;

    /// 待索引正文的向量（不加指令前缀）。
    ///
    /// 超过 [`MAX_SEQ_TOKENS`] 时**不报错**：截到 [`MAX_BODY_TOKENS`] 并如实给出
    /// `truncation`。不超过时输入逐位不变（`truncation` 为 `None`）。
    fn embed_document(&self, text: &str) -> Result<Embedding>;

    /// 查询的向量（可能带指令前缀）。截断口径与 `embed_document` 相同。
    fn embed_query(&self, text: &str) -> Result<Embedding>;
}

/// 模型与 ORT 的路径配置。**只由调用方给**：核心库不猜仓库路径。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddingConfig {
    /// 含 `model_quantized.onnx` 或 `model.onnx` 与 `tokenizer.json` 的目录。
    pub model_dir: PathBuf,
    /// ORT 动态库（`libonnxruntime.so` / `onnxruntime.dll` / …）的路径。
    pub ort_dylib: PathBuf,
    /// 指定用哪个 ONNX 文件（默认优先 `model_quantized.onnx`，退到 `model.onnx`）。
    pub model_file: Option<String>,
}

/// 环境变量的解析结果。三态而不是 `Option`：要能区分「没配」（合法退化）与
/// 「配了但不完整」（配置错误，要说出来）。
#[derive(Debug, Clone)]
pub enum EnvEmbedding {
    /// 两个变量都没设：退化路径。
    Unset,
    /// 设了但不完整或不可用，原因是给人看的。
    Invalid(String),
    Ready(EmbeddingConfig),
}

/// 从环境变量读配置。`DIARY_ORT_DYLIB` 优先，兼容探针一直在用的 `ORT_DYLIB_PATH`。
pub fn config_from_env() -> EnvEmbedding {
    let model_dir = std::env::var_os("DIARY_MODEL_DIR");
    let dylib = std::env::var_os("DIARY_ORT_DYLIB").or_else(|| std::env::var_os("ORT_DYLIB_PATH"));
    match (model_dir, dylib) {
        (None, None) => EnvEmbedding::Unset,
        (Some(_), None) => EnvEmbedding::Invalid(
            "设了 DIARY_MODEL_DIR，但没有设 DIARY_ORT_DYLIB（ORT 动态库路径）".to_owned(),
        ),
        (None, Some(_)) => EnvEmbedding::Invalid(
            "设了 DIARY_ORT_DYLIB，但没有设 DIARY_MODEL_DIR（模型目录）".to_owned(),
        ),
        (Some(dir), Some(lib)) => EnvEmbedding::Ready(EmbeddingConfig {
            model_dir: PathBuf::from(dir),
            ort_dylib: PathBuf::from(lib),
            model_file: std::env::var("DIARY_MODEL_FILE").ok(),
        }),
    }
}

impl EmbeddingConfig {
    /// 实际要加载的 ONNX 文件。优先量化版（体积 23.8 MiB 对 fp32 的 94 MiB）。
    pub fn model_path(&self) -> std::result::Result<PathBuf, String> {
        let candidates: Vec<String> = match &self.model_file {
            Some(name) => vec![name.clone()],
            None => vec!["model_quantized.onnx".to_owned(), "model.onnx".to_owned()],
        };
        for name in &candidates {
            let path = self.model_dir.join(name);
            if path.is_file() {
                return Ok(path);
            }
        }
        Err(format!(
            "{} 里找不到模型文件（找过 {}）：按 README/CONTRIBUTING 准备好模型包",
            self.model_dir.display(),
            candidates.join(" / ")
        ))
    }

    pub fn tokenizer_path(&self) -> PathBuf {
        self.model_dir.join("tokenizer.json")
    }

    /// 配置是否自洽（文件都在）。不加载模型，只 stat。
    pub fn check_files(&self) -> std::result::Result<(), String> {
        self.model_path().map(|_| ())?;
        let tokenizer = self.tokenizer_path();
        if !tokenizer.is_file() {
            return Err(format!(
                "{} 里没有 tokenizer.json（上游 MIT 权重的分词器，随模型包一起放）",
                self.model_dir.display()
            ));
        }
        if !self.ort_dylib.is_file() {
            return Err(format!(
                "ORT 动态库不在：{}（DIARY_ORT_DYLIB / ORT_DYLIB_PATH）",
                self.ort_dylib.display()
            ));
        }
        Ok(())
    }
}

/// 配置里的模型文件对应的 `model_version`（`MODEL_ID@sha256前12位`）。
///
/// **不建 ORT 会话**，只读模型文件算哈希；给 `index_status` 在模型尚未加载时核对
/// 「生效代次是不是当前模型建的」用。文件读不到就返回原因，调用方如实报错/不比对。
pub(crate) fn model_version_for(config: &EmbeddingConfig) -> std::result::Result<String, String> {
    let path = config.model_path()?;
    let hash = model_hash_prefix(&path)?;
    Ok(format!("{MODEL_ID}@{hash}"))
}

/// 模型文件的 sha256 前缀。模型换了 → 版本号必然变 → 必然换代，新旧向量不会混在
/// 一张表里（这正是「不要写死字符串了事」的原因）。
fn model_hash_prefix(path: &Path) -> std::result::Result<String, String> {
    let mut file = std::fs::File::open(path)
        .map_err(|err| format!("读不到模型文件 {}：{err}", path.display()))?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut hasher)
        .map_err(|err| format!("读模型文件 {} 出错：{err}", path.display()))?;
    let hex = format!("{:x}", hasher.finalize());
    Ok(hex[..MODEL_HASH_CHARS.min(hex.len())].to_owned())
}

/// 真模型 + 分词器。一个模型文件只建一次，放进 `static` 之后**永不释放**。
struct SharedModel {
    session: Mutex<Session>,
    tokenizer: Tokenizer,
    model_version: String,
    dims: usize,
}

/// 进程级的模型缓存。`static` 里的 `HashMap` 不会被析构——这就是「退出不释放」。
static MODELS: OnceLock<Mutex<HashMap<PathBuf, Arc<SharedModel>>>> = OnceLock::new();

/// ORT 环境的初始化结果。只初始化一次，且**不在退出路径上做任何事**。
static ORT_INIT: OnceLock<std::result::Result<PathBuf, String>> = OnceLock::new();

fn models() -> &'static Mutex<HashMap<PathBuf, Arc<SharedModel>>> {
    MODELS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    // 中毒只说明别处 panic 过；模型缓存本身没有 «一半写好» 的中间态，继续用。
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// 加载（或复用）一个模型。
fn shared_model(config: &EmbeddingConfig) -> std::result::Result<Arc<SharedModel>, String> {
    let model_path = config.model_path()?;
    let tokenizer_path = config.tokenizer_path();
    if !tokenizer_path.is_file() {
        return Err(format!(
            "{} 里没有 tokenizer.json（上游 MIT 权重的分词器，随模型包一起放）",
            config.model_dir.display()
        ));
    }
    if let Some(hit) = lock(models()).get(&model_path) {
        return Ok(Arc::clone(hit));
    }

    // ORT 环境只初始化一次。`commit()` 返回 false 表示这个进程里已经初始化过
    // （比如探针自己的 `vector-model` 先跑过）；这不是错误——会话照样能建。
    let init = ORT_INIT.get_or_init(|| {
        let committed = ort::init_from(&config.ort_dylib)
            .map_err(|err| {
                format!(
                    "加载 ONNX Runtime 失败（{}）：{err}",
                    config.ort_dylib.display()
                )
            })?
            .commit();
        let _ = committed;
        Ok(config.ort_dylib.clone())
    });
    if let Err(reason) = init {
        return Err(reason.clone());
    }

    let tokenizer = Tokenizer::from_file(&tokenizer_path)
        .map_err(|err| format!("读取分词器失败（{}）：{err}", tokenizer_path.display()))?;
    let mut session = Session::builder()
        .map_err(|err| format!("创建 ONNX Runtime 会话失败：{err}"))?
        .commit_from_file(&model_path)
        .map_err(|err| format!("加载 ONNX 模型失败（{}）：{err}", model_path.display()))?;

    // 用一次真实前向确认维度：拿错模型（base 是 768 维）要在这里就被挡住，
    // 而不是写进 `chunk_vectors.dims` 之后才被发现。
    let dims = warm_dims(&mut session, &tokenizer)?;
    if dims != EXPECTED_DIMS {
        return Err(format!(
            "模型输出维度是 {dims}，与 {MODEL_ID} 的 {EXPECTED_DIMS} 不符（拿错模型了？）"
        ));
    }
    let model_version = model_version_for(config)?;

    let shared = Arc::new(SharedModel {
        session: Mutex::new(session),
        tokenizer,
        model_version,
        dims,
    });
    lock(models()).insert(model_path, Arc::clone(&shared));
    Ok(shared)
}

/// 用一条短文本跑一次前向，返回输出维度。顺带把首次运行的开销（图优化）摊在加载时。
fn warm_dims(session: &mut Session, tokenizer: &Tokenizer) -> std::result::Result<usize, String> {
    let encoding = tokenizer
        .encode("预热", true)
        .map_err(|err| format!("分词失败：{err}"))?;
    let ids: Vec<i64> = encoding.get_ids().iter().map(|v| i64::from(*v)).collect();
    let seq = ids.len();
    let mask: Vec<i64> = encoding
        .get_attention_mask()
        .iter()
        .map(|v| i64::from(*v))
        .collect();
    let types: Vec<i64> = encoding.get_type_ids().iter().map(|v| i64::from(*v)).collect();
    let inputs = ort::inputs![
        "input_ids" => Tensor::from_array(([1_i64, seq as i64], ids)).map_err(|err| format!("构造输入张量失败：{err}"))?,
        "attention_mask" => Tensor::from_array(([1_i64, seq as i64], mask)).map_err(|err| format!("构造输入张量失败：{err}"))?,
        "token_type_ids" => Tensor::from_array(([1_i64, seq as i64], types)).map_err(|err| format!("构造输入张量失败：{err}"))?,
    ];
    let outputs = session
        .run(inputs)
        .map_err(|err| format!("ONNX 前向失败：{err}"))?;
    let (_shape, data) = outputs[0]
        .try_extract_tensor::<f32>()
        .map_err(|err| format!("读模型输出失败：{err}"))?;
    if seq == 0 || !data.len().is_multiple_of(seq) {
        return Err(format!(
            "输出长度 {} 无法被序列长度 {seq} 整除",
            data.len()
        ));
    }
    Ok(data.len() / seq)
}

/// ort 的错误落在契约的哪个码上：语义这一路失败是「当前状态不允许」（没有可用的
/// 模型/运行库），不是数据坏了。这里不往 `CoreError` 上加 `From<ort::Error>`——
/// 那会把 ORT 的类型带进核心的错误外观，改一处漏一处。
fn ort_failure(step: &'static str) -> impl Fn(ort::Error) -> CoreError {
    move |err| CoreError::InvalidState {
        entity: "语义嵌入",
        id: MODEL_ID.to_owned(),
        state: format!("{step}：{err}"),
    }
}

/// 一次送进模型的序列：三张输入张量 + 这次有没有截断。
struct ModelInput {
    ids: Vec<i64>,
    mask: Vec<i64>,
    types: Vec<i64>,
    truncation: Option<TokenTruncation>,
}

/// 分词器的失败一律落在既有的 `invalid_state` 上（不新增错误码，见 issue #109 的第 ③ 条）。
fn tokenizer_failure(err: tokenizers::Error) -> CoreError {
    CoreError::InvalidState {
        entity: "语义嵌入",
        id: MODEL_ID.to_owned(),
        state: format!("分词失败：{err}"),
    }
}

/// **入口的 token 级保护**（issue #109）：超长输入截到模型能吃的窗口，且如实报告。
///
/// 两条分支的边界必须钉死：
///
/// - `encode(text, true)`（含 `[CLS]` / `[SEP]`）不超过 [`MAX_SEQ_TOKENS`]：直接用它的
///   ids / mask / type_ids，**一个字节都不改**。这是「512 以内行为逐位不变」的保证，
///   有测试拿原始 `Encoding` 逐位比对；
/// - 超过：正文重新按**不含特殊 token** 编码，取前 [`MAX_BODY_TOKENS`] 个，再用分词器
///   自己的 `[CLS]` / `[SEP]` 拼回 512。词表和模型是同一份，所以这两个 id 不可能猜错
///   （拿不到就是分词器不完整，如实报 `invalid_state`）。
///
/// 为什么不用 `tokenizers` 自带的 `truncation` 参数：它在**后处理加特殊 token 之前**
/// 截断，`max_length` 要按「正文」算；而且它的截断发生后我们拿不到「原本有多少 token」
/// ——没有原始计数就没法如实报告截掉了多少（正是 #109 要求的）。
fn prepare_input(tokenizer: &Tokenizer, text: &str) -> Result<ModelInput> {
    let encoding = tokenizer.encode(text, true).map_err(tokenizer_failure)?;
    let seq = encoding.get_ids().len();
    if seq == 0 {
        return Err(CoreError::InvalidState {
            entity: "语义嵌入",
            id: MODEL_ID.to_owned(),
            state: "空输入分词后没有 token".to_owned(),
        });
    }
    if seq <= MAX_SEQ_TOKENS {
        return Ok(ModelInput {
            ids: encoding.get_ids().iter().map(|v| i64::from(*v)).collect(),
            mask: encoding
                .get_attention_mask()
                .iter()
                .map(|v| i64::from(*v))
                .collect(),
            types: encoding
                .get_type_ids()
                .iter()
                .map(|v| i64::from(*v))
                .collect(),
            truncation: None,
        });
    }

    let body = tokenizer.encode(text, false).map_err(tokenizer_failure)?;
    let original = body.get_ids().len();
    let kept = original.min(MAX_BODY_TOKENS);
    let cls = tokenizer
        .token_to_id("[CLS]")
        .ok_or_else(|| missing_special_token("[CLS]"))?;
    let sep = tokenizer
        .token_to_id("[SEP]")
        .ok_or_else(|| missing_special_token("[SEP]"))?;
    let mut ids: Vec<i64> = Vec::with_capacity(kept + 2);
    ids.push(i64::from(cls));
    ids.extend(body.get_ids()[..kept].iter().map(|v| i64::from(*v)));
    ids.push(i64::from(sep));
    let truncated_seq = ids.len();
    debug_assert_eq!(truncated_seq, kept + 2);
    Ok(ModelInput {
        ids,
        // 单句输入：全部参与注意力，没有 padding。
        mask: vec![1; truncated_seq],
        types: vec![0; truncated_seq],
        truncation: (original > kept).then_some(TokenTruncation {
            original_tokens: original,
            kept_tokens: kept,
            dropped_tokens: original - kept,
        }),
    })
}

/// 分词器里没有特殊 token：这是配置不完整，不是数据坏。沿用 `invalid_state`。
fn missing_special_token(token: &'static str) -> CoreError {
    CoreError::InvalidState {
        entity: "语义嵌入",
        id: MODEL_ID.to_owned(),
        state: format!(
            "分词器里没有 {token}，无法给超长输入补特殊 token；\
             检查 tokenizer.json 是否是 bge 那一份"
        ),
    }
}

/// 用真 ORT 实现的嵌入器。`options` 只影响取哪一位/加不加前缀，不影响会话。
pub struct OrtEmbedder {
    shared: Arc<SharedModel>,
    options: EmbedOptions,
}

impl OrtEmbedder {
    /// 一次前向 + 按 `options` 池化 + L2 归一化。
    ///
    /// 输入先过 [`prepare_input`]：超长在这里被截到 512 并如实带出截断计数，所以
    /// ORT 永远不会再看到 >512 的序列（issue #109 的那条 broadcast 错误从这里断掉）。
    fn embed(&self, text: &str, options: EmbedOptions) -> Result<Embedding> {
        let mut session = lock(&self.shared.session);
        let input = prepare_input(&self.shared.tokenizer, text)?;
        let seq = input.ids.len();
        let mask = input.mask;
        let truncation = input.truncation;

        let inputs = ort::inputs![
            "input_ids" => Tensor::from_array(([1_i64, seq as i64], input.ids)).map_err(ort_failure("构造输入张量失败"))?,
            "attention_mask" => Tensor::from_array(([1_i64, seq as i64], mask.clone())).map_err(ort_failure("构造输入张量失败"))?,
            "token_type_ids" => Tensor::from_array(([1_i64, seq as i64], input.types)).map_err(ort_failure("构造输入张量失败"))?,
        ];
        let outputs = session.run(inputs).map_err(ort_failure("ONNX 前向失败"))?;
        let (_shape, data) = outputs[0]
            .try_extract_tensor::<f32>()
            .map_err(ort_failure("读模型输出失败"))?;
        if !data.len().is_multiple_of(seq) {
            return Err(CoreError::InvalidState {
                entity: "语义嵌入",
                id: MODEL_ID.to_owned(),
                state: format!("输出长度 {} 无法被序列长度 {seq} 整除", data.len()),
            });
        }
        let dim = data.len() / seq;
        if dim != self.shared.dims {
            return Err(CoreError::InvalidState {
                entity: "语义嵌入",
                id: MODEL_ID.to_owned(),
                state: format!("这次输出 {dim} 维，与加载时确认的 {} 维不符", self.shared.dims),
            });
        }

        let mut pooled = match options.pooling {
            // CLS：第一个 token（`[CLS]`）那一位。
            Pooling::Cls => data[..dim].to_vec(),
            Pooling::Mean => {
                let mut pooled = vec![0_f32; dim];
                let mut counted = 0_f32;
                for (position, flag) in mask.iter().enumerate() {
                    if *flag == 0 {
                        continue;
                    }
                    counted += 1.0;
                    let row = &data[position * dim..(position + 1) * dim];
                    for (slot, value) in pooled.iter_mut().zip(row) {
                        *slot += value;
                    }
                }
                if counted == 0.0 {
                    return Err(CoreError::InvalidState {
                        entity: "语义嵌入",
                        id: MODEL_ID.to_owned(),
                        state: "attention mask 全为 0".to_owned(),
                    });
                }
                for value in &mut pooled {
                    *value /= counted;
                }
                pooled
            }
        };
        // L2 归一化：之后内积即余弦。
        let norm: f32 = pooled.iter().map(|v| v * v).sum::<f32>().sqrt();
        if !norm.is_finite() || norm <= 0.0 {
            return Err(CoreError::InvalidState {
                entity: "语义嵌入",
                id: MODEL_ID.to_owned(),
                state: "模型输出全为 0，无法归一化".to_owned(),
            });
        }
        for value in &mut pooled {
            *value /= norm;
        }
        Ok(Embedding {
            vector: pooled,
            truncation,
        })
    }
}

impl Embedder for OrtEmbedder {
    fn model_version(&self) -> &str {
        &self.shared.model_version
    }

    fn dims(&self) -> usize {
        self.shared.dims
    }

    fn options(&self) -> EmbedOptions {
        self.options
    }

    fn embed_document(&self, text: &str) -> Result<Embedding> {
        self.embed(text, self.options)
    }

    fn embed_query(&self, text: &str) -> Result<Embedding> {
        let text = if self.options.query_instruction {
            format!("{QUERY_INSTRUCTION}{text}")
        } else {
            text.to_owned()
        };
        self.embed(&text, self.options)
    }
}

/// 按配置加载一个嵌入器。失败原因是给人看的（哪一步、哪个文件）。
pub fn load(config: &EmbeddingConfig, options: EmbedOptions) -> std::result::Result<OrtEmbedder, String> {
    let shared = shared_model(config)?;
    Ok(OrtEmbedder { shared, options })
}

/// 便于测试与探针：把「一篇文本」变成 512 维的确定性向量。
///
/// 用 sha256 的字节铺开再归一化——同样的文本永远同样的向量，不同文本几乎不撞。
/// **不是**语义相似度，只用于钉住换代/检索/折叠这些结构性行为。
pub fn text_hash_vector(text: &str, dims: usize) -> Vec<f32> {
    let mut out = vec![0_f32; dims];
    let mut counter = 0_u32;
    let mut filled = 0;
    while filled < dims {
        let mut hasher = Sha256::new();
        hasher.update(counter.to_le_bytes());
        hasher.update(text.as_bytes());
        let digest = hasher.finalize();
        for byte in digest {
            if filled >= dims {
                break;
            }
            // 映射到 [-1, 1)。
            out[filled] = (f32::from(byte) - 127.5) / 127.5;
            filled += 1;
        }
        counter += 1;
    }
    let norm: f32 = out.iter().map(|v| v * v).sum::<f32>().sqrt();
    if norm > 0.0 {
        for value in &mut out {
            *value /= norm;
        }
    }
    out
}

/// 一个写进库、读出来的向量行需要的东西。测试与探针用它核对存储口径。
pub fn vector_bytes(vector: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(vector.len() * 4);
    for value in vector {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    bytes
}

/// 库里 `chunk_vectors.vector` 的 f32 解码。
pub fn bytes_to_vector(bytes: &[u8]) -> Result<Vec<f32>> {
    if !bytes.len().is_multiple_of(4) {
        return Err(CoreError::CorruptedData {
            message: format!("向量字节数 {} 不是 4 的倍数", bytes.len()),
        });
    }
    Ok(bytes
        .chunks_exact(4)
        .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect())
}

/// 向量的**存储编码格式**，也就是 `chunk_vectors.storage` 的取值。
///
/// # 为什么它是一个「口径」而不是一个实现细节
///
/// `storage` 与 `model_version` / `dims` 并列，是向量空间的三字段之一（#58）：
/// 同一段文字用 f32 与 int8 存下来，解码后的数值不同，两者之间的余弦与「两个模型
/// 各说各话」是同一类错误——看起来有分数、有排序，但那个分数不代表任何东西。
/// 所以：
///
/// - 编码格式**按行记录**（`storage` 列），解码按行分派，**绝不猜**；
/// - 换格式 = 换口径：`build_semantic_index` 会把整代重算（不是原地重编码，
///   那会留下「一半 f32 一半 int8」的混合代次）；
/// - 生效代次里有一行与当前格式不同，语义这一路整路不作数（`ForeignSpace`），
///   不会把跨编码的余弦当成结果。
///
/// # 两种格式
///
/// - `f32`：每分量 4 字节 little-endian，512 维 = 2048 B；
/// - `int8`：每向量一个 f32 `scale` + 每分量 1 字节有符号整数，512 维 = 516 B。
///   `scale = max(|v|) / 127`，解码 `v' = q * scale`。逐向量一个 scale 而不是全局
///   一个：不同模型的向量幅度差很多，全局 scale 会让某个模型的量化误差放大到不可用。
///   BGE 向量已经 L2 归一化，逐向量 scale 的意义是「保住这一条向量的相对形状」。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VectorStorage {
    /// 每个分量 4 字节 f32。
    F32,
    /// 每向量一个 f32 scale + 每分量 1 字节 int8。
    Int8,
}

impl VectorStorage {
    /// 写进 `chunk_vectors.storage` 的 wire 名。
    pub const fn wire(self) -> &'static str {
        match self {
            Self::F32 => "f32",
            Self::Int8 => "int8",
        }
    }

    /// 从库里读到的 `storage` 解析。不认识的格式返回 `None`——调用方必须如实报错，
    /// 不能挑一个默认格式猜着解码。
    pub fn from_wire(value: &str) -> Option<Self> {
        match value {
            "f32" => Some(Self::F32),
            "int8" => Some(Self::Int8),
            _ => None,
        }
    }

    /// 一条 `dims` 维向量编码后的字节数（报告的「每向量体积」用这个真值）。
    pub const fn bytes_per_vector(self, dims: usize) -> usize {
        match self {
            Self::F32 => dims * 4,
            Self::Int8 => 4 + dims,
        }
    }

    /// 编码成 `chunk_vectors.vector` 的字节。
    ///
    /// **先校验有限性**：NaN 会被 `as i8` 静默变成 0（等于把这一维抹掉），Inf 会让
    /// int8 的 scale 变成非有限值——写进去以后每次查询都在 `score_of` 报
    /// `CorruptedData`，而用户只看到「搜不出来」。在写入口就拒绝，坏向量一条都进不了库。
    pub fn encode(self, vector: &[f32]) -> Result<Vec<u8>> {
        if let Some((index, value)) = vector
            .iter()
            .enumerate()
            .find(|(_, value)| !value.is_finite())
        {
            return Err(CoreError::CorruptedData {
                message: format!(
                    "向量第 {index} 维不是有限数（{value}），拒绝编码：NaN/Inf 会污染 scale 与排序"
                ),
            });
        }
        Ok(match self {
            Self::F32 => vector_bytes(vector),
            Self::Int8 => {
                // 全零向量（理论上模型不会给）没有可用的 scale：写 1.0，解出来仍是全零。
                let max = vector.iter().fold(0_f32, |acc, value| acc.max(value.abs()));
                let scale = if max > 0.0 { max / 127.0 } else { 1.0 };
                let mut bytes = Vec::with_capacity(4 + vector.len());
                bytes.extend_from_slice(&scale.to_le_bytes());
                for value in vector {
                    let quantized = (value / scale).round().clamp(-127.0, 127.0) as i8;
                    bytes.push(quantized as u8);
                }
                bytes
            }
        })
    }

    /// 从 `chunk_vectors.vector` 的字节解码。
    pub fn decode(self, bytes: &[u8]) -> Result<Vec<f32>> {
        match self {
            Self::F32 => bytes_to_vector(bytes),
            Self::Int8 => {
                if bytes.len() < 4 {
                    return Err(CoreError::CorruptedData {
                        message: format!("int8 向量字节数 {} 连一个 scale 都放不下", bytes.len()),
                    });
                }
                let scale = f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
                if !scale.is_finite() || scale <= 0.0 {
                    return Err(CoreError::CorruptedData {
                        message: format!("int8 向量的 scale 是 {scale}，不是正的有限数"),
                    });
                }
                Ok(bytes[4..]
                    .iter()
                    .map(|byte| f32::from(*byte as i8) * scale)
                    .collect())
            }
        }
    }
}

/// 新写入的向量默认用哪种格式。
///
/// **int8 是默认**（#90 的拍板决定）：36 条质量集上 R@1/R@10/MRR 与 f32 逐项相同、
/// 0/32 名次变化；2000 块自查询 MRR 0.835 → 0.833（-0.002，只有相邻互换）。
/// 代价是体积 1/4（100k 块 195.3 → 49.2 MiB）、100k 热查询约 2× 快（380 → 156 ms），
/// 构建多一遍量化编码（约 +15%）。依据与数字见 `docs/architecture/m5-大库可用性.md`。
///
/// 换默认只发生这一次「发布前」：`storage` 是口径三字段之一，已有库在下一次
/// `build_semantic_index` 时会**整代重算**（不是原地重编码）。
pub const DEFAULT_VECTOR_STORAGE: VectorStorage = VectorStorage::Int8;

/// 时间戳工具转发：写向量行时与库里其它时间用同一种格式。
pub(crate) fn now_iso() -> String {
    support::to_iso(support::now())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_from_env_three_states() {
        // 这个测试不设环境变量——CI 里本来也没设。真设了的环境（本机跑量测时）
        // 就跳过，免得把量测环境误判成失败。
        if std::env::var_os("DIARY_MODEL_DIR").is_some()
            || std::env::var_os("DIARY_ORT_DYLIB").is_some()
            || std::env::var_os("ORT_DYLIB_PATH").is_some()
        {
            return;
        }
        assert!(matches!(config_from_env(), EnvEmbedding::Unset));
    }

    #[test]
    fn text_hash_vector_is_deterministic_and_normalized() {
        let a = text_hash_vector("妈妈打电话来。", EXPECTED_DIMS);
        let b = text_hash_vector("妈妈打电话来。", EXPECTED_DIMS);
        let c = text_hash_vector("路边的桂花开了。", EXPECTED_DIMS);
        assert_eq!(a.len(), EXPECTED_DIMS);
        assert_eq!(a, b, "同样的文本必须得到同样的向量");
        assert_ne!(a, c, "不同文本不该撞");
        let norm: f32 = a.iter().map(|v| v * v).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-5, "应当 L2 归一化：{norm}");
    }

    #[test]
    fn vector_bytes_round_trip() {
        let vector = vec![0.5_f32, -0.25, 1.0];
        let bytes = vector_bytes(&vector);
        assert_eq!(bytes.len(), 12);
        assert_eq!(bytes_to_vector(&bytes).unwrap(), vector);
        assert!(bytes_to_vector(&[0, 1, 2]).is_err(), "不是 4 的倍数要报错");
    }

    #[test]
    fn int8_round_trip_error_is_bounded_by_half_a_step() {
        // 一条真实的 BGE 形状向量：L2 归一化、分量有正有负。
        let vector = text_hash_vector("妈妈打电话来。", EXPECTED_DIMS);
        let bytes = VectorStorage::Int8.encode(&vector).unwrap();
        assert_eq!(
            bytes.len(),
            VectorStorage::Int8.bytes_per_vector(EXPECTED_DIMS),
            "int8 体积 = 4 字节 scale + 每维 1 字节"
        );
        assert_eq!(
            VectorStorage::Int8.bytes_per_vector(EXPECTED_DIMS),
            516,
            "512 维 int8 = 4 字节 scale + 512 字节"
        );
        assert_eq!(
            VectorStorage::F32.bytes_per_vector(EXPECTED_DIMS),
            2048,
            "512 维 f32 = 2048 字节"
        );
        let decoded = VectorStorage::Int8.decode(&bytes).unwrap();
        assert_eq!(decoded.len(), vector.len());

        // 误差上界：每个分量 \|v' - v\| <= scale/2（四舍五入）。scale 从字节里读回来，
        // 这样测的是**存进去的那份** scale，不是重新算的。
        let scale = f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        for (original, restored) in vector.iter().zip(&decoded) {
            assert!(
                (original - restored).abs() <= scale / 2.0 + 1e-6,
                "分量误差超过半个量化步长：{original} -> {restored}（scale {scale}）"
            );
        }
        // 余弦几乎不变：这才是「召回会不会掉」的直接指标。
        let dot: f32 = vector.iter().zip(&decoded).map(|(a, b)| a * b).sum();
        let norm: f32 = decoded.iter().map(|v| v * v).sum::<f32>().sqrt();
        let cosine = dot / norm;
        assert!(
            (cosine - 1.0).abs() < 1e-4,
            "量化后与原向量的余弦应当是 1.000 左右：{cosine}"
        );
    }

    #[test]
    fn int8_decoding_rejects_corrupt_rows() {
        assert!(
            VectorStorage::Int8.decode(&[0, 0]).is_err(),
            "连 scale 都不够的字节要报错"
        );
        assert!(
            VectorStorage::Int8.decode(&0_f32.to_le_bytes()).is_err(),
            "scale = 0 要报错（除零会把整条向量变成 NaN）"
        );
        assert!(
            VectorStorage::Int8.decode(&f32::NAN.to_le_bytes()).is_err(),
            "scale = NaN 要报错"
        );
        assert!(
            VectorStorage::Int8.decode(&f32::INFINITY.to_le_bytes()).is_err(),
            "scale = Inf 要报错（Inf × i8 会得到 Inf/NaN 向量）"
        );
        assert_eq!(
            VectorStorage::from_wire("i8"),
            None,
            "不认识的格式返回 None，由调用方如实报错，不猜"
        );
        assert_eq!(
            VectorStorage::from_wire("int8"),
            Some(VectorStorage::Int8)
        );
    }

    #[test]
    fn encoding_rejects_non_finite_components() {
        // Inf 会让 int8 的 scale 变成非有限值；NaN 会被 `as i8` 静默压成 0。
        // 两种存储都必须在写入口拒绝，坏向量一条都不能进库。
        for storage in [VectorStorage::F32, VectorStorage::Int8] {
            for value in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
                let error = storage.encode(&[0.5, value, -0.25]).unwrap_err();
                let CoreError::CorruptedData { message } = error else {
                    panic!("非有限分量应当报 CorruptedData，实际是 {error:?}");
                };
                assert!(
                    message.contains("不是有限数"),
                    "错误要说清哪一维不是有限数：{message}"
                );
            }
        }
    }

    #[test]
    fn int8_encoding_is_deterministic() {
        // 确定性：同一条向量编码两次必须逐字节相同（否则重建索引会无谓地换代）。
        let vector = text_hash_vector("路边的桂花开了。", EXPECTED_DIMS);
        assert_eq!(
            VectorStorage::Int8.encode(&vector).unwrap(),
            VectorStorage::Int8.encode(&vector).unwrap()
        );
    }

    #[test]
    fn embedding_config_reports_missing_files() {
        let config = EmbeddingConfig {
            model_dir: PathBuf::from("/nonexistent/diary-model-dir"),
            ort_dylib: PathBuf::from("/nonexistent/libonnxruntime.so"),
            model_file: None,
        };
        let reason = config.check_files().unwrap_err();
        assert!(
            reason.contains("找不到模型文件"),
            "要如实说缺哪个文件：{reason}"
        );
    }

    // ------------------------------------------------------------ #109：token 截断

    /// 一份**微型** BERT WordPiece 分词器（vocab 只有几个字 + 5 个特殊 token）。
    ///
    /// 为什么不用真分词器：CI 上没有 `tokenizer.json`（`.gitignore` 里，随模型包
    /// 单独获取）。但「超长 → 截断 → 报告」这条逻辑不需要 2 万词表才能测——词表大小
    /// 不改变口径，只改变每个块有多少 token。真实分词器的数字在
    /// `docs/architecture/m2-质量集与分块.md` 里单独量（本文件末尾有一条按
    /// `DIARY_MODEL_DIR` 才跑的门禁）。
    const TINY_TOKENIZER_JSON: &str =
        include_str!("../tests/fixtures/tiny_wordpiece_tokenizer.json");

    fn tiny_tokenizer() -> Tokenizer {
        Tokenizer::from_bytes(TINY_TOKENIZER_JSON.as_bytes())
            .expect("内置的微型分词器必须能加载")
    }

    fn as_i64(ids: &[u32]) -> Vec<i64> {
        ids.iter().map(|value| i64::from(*value)).collect()
    }

    /// 修复前：617 字的输入原样是 619 token，送进按 512 固定导出位置编码的 ONNX 图，
    /// 报 `Attempting to broadcast an axis by a dimension other than 1. 512 by 619`。
    /// 修复后：入口截到 512（正文 510 + 两端特殊 token），并且**报出被截掉多少**。
    ///
    /// 这条测试能红的方式很直接：把 `prepare_input` 里的截断分支去掉，第一条断言
    /// （`input.ids.len() == MAX_SEQ_TOKENS`）就会看到 619。
    #[test]
    fn over_limit_input_is_truncated_on_token_boundaries_and_reported() {
        let tokenizer = tiny_tokenizer();
        let text = "妈".repeat(617);
        let raw = tokenizer
            .encode(text.as_str(), true)
            .expect("微型分词器可以编码这句话");
        assert_eq!(
            raw.get_ids().len(),
            619,
            "前提：这个输入真的超过 512（修复前就是它把 ORT 打红）"
        );

        let input = prepare_input(&tokenizer, &text).expect("超长不再是错误");
        assert_eq!(input.ids.len(), MAX_SEQ_TOKENS, "序列总长必须正好是模型上限");
        assert_eq!(
            input.ids.first().copied(),
            tokenizer.token_to_id("[CLS]").map(i64::from),
            "截断必须保留开头的 [CLS]"
        );
        assert_eq!(
            input.ids.last().copied(),
            tokenizer.token_to_id("[SEP]").map(i64::from),
            "截断必须保留结尾的 [SEP]"
        );
        assert_eq!(input.mask, vec![1_i64; MAX_SEQ_TOKENS]);
        assert_eq!(input.types, vec![0_i64; MAX_SEQ_TOKENS]);

        let reported = input.truncation.expect("截断了就必须如实报告");
        assert_eq!(
            reported,
            TokenTruncation {
                original_tokens: 617,
                kept_tokens: MAX_BODY_TOKENS,
                dropped_tokens: 617 - MAX_BODY_TOKENS,
            }
        );
        assert_eq!(
            reported.dropped_tokens, 107,
            "617 个汉字 token 里截掉 107 个，留下 510"
        );
        assert!(
            reported.to_string().contains("107"),
            "给人看的说法里要带数字：{reported}"
        );
    }

    /// 512 以内**逐位不变**：ids / attention_mask / token_type_ids 与原始 `Encoding`
    /// 完全一致，且 `truncation` 是 `None`（不是「截了 0 个」）。
    #[test]
    fn below_the_limit_is_bit_identical_to_the_old_path() {
        let tokenizer = tiny_tokenizer();
        for text in [
            "妈妈打电话来说她最近身体不太好。",
            "妈",
            "",
            "the quick brown fox",
        ] {
            let raw = tokenizer.encode(text, true).expect("编码");
            let input = prepare_input(&tokenizer, text).expect("编码");
            assert_eq!(input.ids, as_i64(raw.get_ids()), "input_ids 必须逐位相同：{text:?}");
            assert_eq!(
                input.mask,
                as_i64(raw.get_attention_mask()),
                "attention_mask 必须逐位相同：{text:?}"
            );
            assert_eq!(
                input.types,
                as_i64(raw.get_type_ids()),
                "token_type_ids 必须逐位相同：{text:?}"
            );
            assert!(
                input.truncation.is_none(),
                "没超限就**不能**报告截断：{text:?}"
            );
        }
    }

    /// 边界：正文 510 token（+2 特殊 = 512）不动；511 token 只截掉 1 个。
    #[test]
    fn the_window_boundary_is_exact() {
        let tokenizer = tiny_tokenizer();

        let at_limit = prepare_input(&tokenizer, &"妈".repeat(MAX_BODY_TOKENS)).expect("编码");
        let raw = tokenizer
            .encode("妈".repeat(MAX_BODY_TOKENS), true)
            .expect("编码");
        assert_eq!(raw.get_ids().len(), MAX_SEQ_TOKENS, "前提：正好顶到窗口");
        assert_eq!(at_limit.ids, as_i64(raw.get_ids()));
        assert!(at_limit.truncation.is_none(), "正好 512 不算截断");

        let one_over = prepare_input(&tokenizer, &"妈".repeat(MAX_BODY_TOKENS + 1)).expect("编码");
        assert_eq!(one_over.ids.len(), MAX_SEQ_TOKENS);
        let reported = one_over.truncation.expect("超一个 token 也要报");
        assert_eq!(reported.original_tokens, MAX_BODY_TOKENS + 1);
        assert_eq!(reported.kept_tokens, MAX_BODY_TOKENS);
        assert_eq!(reported.dropped_tokens, 1);
    }

    /// 真分词器上的现场复现（617 token 的中文）。没配 `DIARY_MODEL_DIR` 就跳过——
    /// CI 不跑这一条，它的数字与 `docs/architecture/m2-质量集与分块.md` 一致。
    #[test]
    fn real_tokenizer_truncates_the_617_token_repro() {
        let Some(dir) = std::env::var_os("DIARY_MODEL_DIR") else {
            return;
        };
        let path = PathBuf::from(dir).join("tokenizer.json");
        if !path.is_file() {
            return;
        }
        let tokenizer = Tokenizer::from_file(&path).expect("读取真实分词器");
        let text = "妈".repeat(617);
        let raw = tokenizer.encode(text.as_str(), true).expect("编码");
        assert!(
            raw.get_ids().len() > MAX_SEQ_TOKENS,
            "617 个汉字必须真的超限（实际 {} token）",
            raw.get_ids().len()
        );
        let input = prepare_input(&tokenizer, &text).expect("超长不再是错误");
        assert_eq!(input.ids.len(), MAX_SEQ_TOKENS);
        let reported = input.truncation.expect("必须报告截断");
        assert_eq!(reported.original_tokens, 617);
        assert_eq!(reported.kept_tokens, MAX_BODY_TOKENS);
        assert_eq!(reported.dropped_tokens, 107);
    }

    /// 真模型 + 真 ORT 的端到端证据：修复前这里必然报错，修复后拿到 512 维向量并
    /// 报出截断 107 个 token。
    ///
    /// 手工跑（CI 没有模型与动态库）：
    ///
    /// ```text
    /// DIARY_MODEL_DIR=... DIARY_ORT_DYLIB=... \
    ///   cargo test -p diary_core --lib -- --ignored real_model_embeds_an_over_limit_text
    /// ```
    #[test]
    #[ignore = "需要真模型与 ORT 动态库（DIARY_MODEL_DIR / DIARY_ORT_DYLIB）"]
    fn real_model_embeds_an_over_limit_text_and_reports_truncation() {
        let EnvEmbedding::Ready(config) = config_from_env() else {
            panic!("这条测试要先设 DIARY_MODEL_DIR 与 DIARY_ORT_DYLIB");
        };
        let embedder = load(&config, EmbedOptions::default()).expect("加载真模型");
        let embedding = embedder
            .embed_document(&"妈".repeat(617))
            .expect("超长输入必须成功（修复前是 ONNX broadcast 错误）");
        assert_eq!(embedding.vector.len(), EXPECTED_DIMS);
        let reported = embedding.truncation.expect("必须报告截断");
        assert_eq!(reported.dropped_tokens, 107);
    }
}
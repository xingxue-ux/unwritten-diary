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
    fn embed_document(&self, text: &str) -> Result<Vec<f32>>;

    /// 查询的向量（可能带指令前缀）。
    fn embed_query(&self, text: &str) -> Result<Vec<f32>>;
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
    let hash = model_hash_prefix(&model_path)?;
    let model_version = format!("{MODEL_ID}@{hash}");

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

/// 用真 ORT 实现的嵌入器。`options` 只影响取哪一位/加不加前缀，不影响会话。
pub struct OrtEmbedder {
    shared: Arc<SharedModel>,
    options: EmbedOptions,
}

impl OrtEmbedder {
    /// 一次前向 + 按 `options` 池化 + L2 归一化。
    fn embed(&self, text: &str, options: EmbedOptions) -> Result<Vec<f32>> {
        let mut session = lock(&self.shared.session);
        let encoding = self
            .shared
            .tokenizer
            .encode(text, true)
            .map_err(|err| CoreError::InvalidState {
                entity: "语义嵌入",
                id: MODEL_ID.to_owned(),
                state: format!("分词失败：{err}"),
            })?;
        let seq = encoding.get_ids().len();
        if seq == 0 {
            return Err(CoreError::InvalidState {
                entity: "语义嵌入",
                id: MODEL_ID.to_owned(),
                state: "空输入分词后没有 token".to_owned(),
            });
        }
        let ids: Vec<i64> = encoding.get_ids().iter().map(|v| i64::from(*v)).collect();
        let mask: Vec<i64> = encoding
            .get_attention_mask()
            .iter()
            .map(|v| i64::from(*v))
            .collect();
        let types: Vec<i64> = encoding.get_type_ids().iter().map(|v| i64::from(*v)).collect();

        let inputs = ort::inputs![
            "input_ids" => Tensor::from_array(([1_i64, seq as i64], ids)).map_err(ort_failure("构造输入张量失败"))?,
            "attention_mask" => Tensor::from_array(([1_i64, seq as i64], mask.clone())).map_err(ort_failure("构造输入张量失败"))?,
            "token_type_ids" => Tensor::from_array(([1_i64, seq as i64], types)).map_err(ort_failure("构造输入张量失败"))?,
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
        Ok(pooled)
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

    fn embed_document(&self, text: &str) -> Result<Vec<f32>> {
        self.embed(text, self.options)
    }

    fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
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
}
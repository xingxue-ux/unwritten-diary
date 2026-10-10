//! 本地向量模型验证：bge-small-zh-v1.5 INT8 ONNX。
//!
//! 任务书 5.1 节要求为选型留下：模型体积、算子可用性、推理耗时、运行内存、
//! 以及中文短查询质量。这里把能在这台机器上量的都量出来。
//!
//! 需要先准备权重（不入库，见 `.gitignore` 的 `models/**/*.onnx`）：
//! ```text
//! models/bge-small-zh-v1.5/model_quantized.onnx
//! models/bge-small-zh-v1.5/tokenizer.json
//! ```
//!
//! 运行：
//! ```text
//! cargo run --release -p diary_probe --features model -- vector-model
//! ```

use std::fs;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use anyhow::{bail, Context, Result};
use ort::session::Session;
use ort::value::Tensor;
use tokenizers::Tokenizer;

/// bge-small-zh-v1.5 的向量维度。
const EXPECTED_DIM: usize = 512;

/// 进程级单例：模型 session 与分词器只建一次，**到进程结束都不释放**。
///
/// 为什么「不释放」是正确做法，而不是这里偷懒：ONNX Runtime 在**退出阶段**有自己
/// 的全局收尾（库内部的静态析构 / `atexit`）。如果我们在退出前释放了 session，
/// 它随后会在已经被拆掉的内部状态上跑收尾——桌面表现为「全部输出之后 SIGSEGV
/// （退出码 139）」，Android 模拟器上更直白：`FORTIFY: pthread_mutex_lock called
/// on a destroyed mutex` 之后 abort。
///
/// 实测排除了三种猜测（记录在 `docs/architecture/M0-技术验证.md` 3.3）：
///
/// | 做法 | 退出码 |
/// |---|---|
/// | 只初始化环境、不建 session | 0 |
/// | 建 session 后正常析构 | 139 |
/// | 自己再开一次动态库、把引用计数钉住（防卸载） | **仍然 139** |
/// | session 放进 `static`（Rust 不跑 static 的析构） | **0** |
///
/// 所以问题不在「动态库被卸载」，而在「session 被释放后 ORT 的退出收尾」。
/// 放进 `OnceLock` 同时满足两件事：模型只加载一次（省掉每次 273 ms 的加载），
/// 生命周期天然到进程结束。退出时由内核回收——这不是漏内存，是刻意不释放。
static EMBEDDER: OnceLock<Mutex<Model>> = OnceLock::new();

/// 进程级的模型资源：分词器 + ONNX session。
struct Model {
    session: Session,
    tokenizer: Tokenizer,
}

/// 只在第一次前向时打印一次输出形状，避免刷屏。
static SHAPE_PRINTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// 质量检查用的查询与候选句。全部是虚构内容。
const QUALITY_CASES: [(&str, [&str; 3]); 3] = [
    (
        "妈妈最近身体不好",
        [
            "妈妈打电话来说她体检结果不太好，我有点担心。",
            "今天项目上线，加班到十点才回家。",
            "房东说要涨房租，我打算周末去看别的房子。",
        ],
    ),
    (
        "想换个工作",
        [
            "下午跟主管聊完，认真考虑要不要离职。",
            "晚上跑步五公里，回来洗了个澡。",
            "妈妈让我周末回家吃饭。",
        ],
    ),
    (
        "最近总是睡不着",
        [
            "连续几天失眠，凌晨两点还醒着，白天没精神。",
            "同事推荐了一家咖啡馆，咖啡还不错。",
            "合同签完了，流程比想象中顺利。",
        ],
    ),
];

fn model_dir() -> PathBuf {
    // 设备上（Android）用环境变量指路径；桌面开发默认用仓库里的目录。
    if let Ok(dir) = std::env::var("DIARY_MODEL_DIR") {
        return PathBuf::from(dir);
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../models/bge-small-zh-v1.5")
}

/// Linux 上的峰值常驻内存（VmHWM）。其他平台返回 None。
fn peak_rss_mib() -> Option<f64> {
    let status = fs::read_to_string("/proc/self/status").ok()?;
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmHWM:") {
            let kb: f64 = rest.split_whitespace().next()?.parse().ok()?;
            return Some(kb / 1024.0);
        }
    }
    None
}

fn median(values: &mut [f64]) -> f64 {
    values.sort_by(|a, b| a.partial_cmp(b).expect("样本不是 NaN"));
    values[values.len() / 2]
}

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    dot / (na * nb)
}

/// 跑一次前向，返回句向量（mean pooling + L2 归一化）。
fn embed(model: &mut Model, text: &str) -> Result<Vec<f32>> {
    let encoding = model
        .tokenizer
        .encode(text, true)
        .map_err(|err| anyhow::anyhow!("分词失败：{err}"))?;
    let seq = encoding.get_ids().len();
    let ids: Vec<i64> = encoding.get_ids().iter().map(|v| i64::from(*v)).collect();
    let mask: Vec<i64> = encoding
        .get_attention_mask()
        .iter()
        .map(|v| i64::from(*v))
        .collect();
    let types: Vec<i64> = encoding.get_type_ids().iter().map(|v| i64::from(*v)).collect();

    let inputs = ort::inputs![
        "input_ids" => Tensor::from_array(([1_i64, seq as i64], ids))?,
        "attention_mask" => Tensor::from_array(([1_i64, seq as i64], mask.clone()))?,
        "token_type_ids" => Tensor::from_array(([1_i64, seq as i64], types))?,
    ];
    let outputs = model.session.run(inputs)?;
    let (shape, data) = outputs[0].try_extract_tensor::<f32>()?;

    if data.len() % seq != 0 {
        bail!("输出长度 {} 无法被序列长度 {seq} 整除", data.len());
    }
    let dim = data.len() / seq;
    if dim != EXPECTED_DIM {
        bail!("维度是 {dim}，与预期的 {EXPECTED_DIM} 不符");
    }

    // mean pooling：只对 attention mask 为 1 的位置求平均。
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
        bail!("attention mask 全为 0");
    }
    for value in &mut pooled {
        *value /= counted;
    }
    // L2 归一化，之后内积即余弦。
    let norm: f32 = pooled.iter().map(|v| v * v).sum::<f32>().sqrt();
    for value in &mut pooled {
        *value /= norm;
    }
    if !SHAPE_PRINTED.swap(true, std::sync::atomic::Ordering::Relaxed) {
        println!("  （首次输出形状 {shape:?}）");
    }
    Ok(pooled)
}

pub fn run() -> Result<()> {
    println!("本地向量模型验证：bge-small-zh-v1.5 INT8 ONNX");
    println!("===========================================");

    let dir = model_dir();
    let model_path = dir.join("model_quantized.onnx");
    let tokenizer_path = dir.join("tokenizer.json");
    for path in [&model_path, &tokenizer_path] {
        if !path.is_file() {
            bail!("缺少 {}：按 README 先下载权重", path.display());
        }
    }
    let model_bytes = fs::metadata(&model_path)?.len();
    let tokenizer_bytes = fs::metadata(&tokenizer_path)?.len();
    println!(
        "权重：model_quantized.onnx {:.1} MiB · tokenizer.json {:.0} KiB",
        model_bytes as f64 / (1024.0 * 1024.0),
        tokenizer_bytes as f64 / 1024.0
    );
    let rss_before = peak_rss_mib();

    let tokenizer = Tokenizer::from_file(&tokenizer_path)
        .map_err(|err| anyhow::anyhow!("读取分词器失败：{err}"))?;

    // 显式初始化 ONNX Runtime 环境：路径由调用方给（打包时就是我们自己带的那份
    // 动态库），而不是让 ort 在第一次建 session 时懒加载。
    let dylib = std::env::var("ORT_DYLIB_PATH")
        .context("需要 ORT_DYLIB_PATH 指向 libonnxruntime.so")?;
    let committed = ort::init_from(&dylib)
        .map_err(|err| anyhow::anyhow!("加载 ONNX Runtime 失败（{dylib}）：{err}"))?
        .commit();
    if !committed {
        bail!("ONNX Runtime 环境已经被初始化过（这个进程里不该发生）");
    }

    let load_start = Instant::now();
    if EMBEDDER.get().is_none() {
        let session = Session::builder()?
            .commit_from_file(&model_path)
            .context("加载 ONNX 模型失败")?;
        // 放进 static：只建一次，而且**到进程结束都不释放**（原因见 EMBEDDER 的文档）。
        let _ = EMBEDDER.set(Mutex::new(Model { session, tokenizer }));
    }
    let embedder = EMBEDDER.get().expect("上面刚设置过（或之前已经设置过）");
    let mut model = embedder.lock().expect("模型互斥量中毒");
    let load_ms = load_start.elapsed().as_secs_f64() * 1000.0;
    let input_names: Vec<String> = model
        .session
        .inputs()
        .iter()
        .map(|input| input.name().to_owned())
        .collect();
    println!("模型加载：{load_ms:.0} ms · 输入张量 {input_names:?}");

    let warmed = embed(&mut model, "预热")?;
    let rss_after_load = peak_rss_mib();
    println!("向量维度：{}", warmed.len());

    // ---- 单条短文本 ----
    let short = "妈妈打电话来说她最近身体不太好";
    let mut samples = Vec::new();
    for _ in 0..20 {
        let start = Instant::now();
        let _ = embed(&mut model, short)?;
        samples.push(start.elapsed().as_secs_f64() * 1000.0);
    }
    let short_median = median(&mut samples);

    // ---- 单条长文本（接近 512 token 上限）----
    let long_text = "今天下午跟主管聊了很久，".repeat(40);
    let mut samples = Vec::new();
    for _ in 0..10 {
        let start = Instant::now();
        let _ = embed(&mut model, &long_text)?;
        samples.push(start.elapsed().as_secs_f64() * 1000.0);
    }
    let long_median = median(&mut samples);
    let long_tokens = model
        .tokenizer
        .encode(long_text.as_str(), true)
        .map_err(|err| anyhow::anyhow!("分词失败：{err}"))?
        .get_ids()
        .len();

    // ---- 逐条处理 32 条，看吞吐 ----
    let batch_start = Instant::now();
    for index in 0..32 {
        let text = format!("第{index}条待索引的日记片段，内容用于测量吞吐。");
        let _ = embed(&mut model, &text)?;
    }
    let batch_total = batch_start.elapsed().as_secs_f64() * 1000.0;

    let rss_after = peak_rss_mib();

    println!("\n耗时");
    println!("  短文本单条（中位）  {short_median:.1} ms");
    println!("  长文本单条（{long_tokens} token，中位） {long_median:.1} ms");
    println!(
        "  32 条逐条处理        {batch_total:.0} ms（{:.1} ms/条）",
        batch_total / 32.0
    );
    match (rss_before, rss_after_load, rss_after) {
        (Some(before), Some(loaded), Some(after)) => {
            println!("\n内存（进程峰值常驻）");
            println!("  加载权重前 {before:.0} MiB · 加载后 {loaded:.0} MiB · 推理后 {after:.0} MiB");
            println!("  模型带来的峰值增量约 {:.0} MiB", loaded - before);
        }
        _ => println!("\n内存：本平台无法读取 /proc/self/status，未测量"),
    }

    // ---- 质量：短查询能否把相关句排在第一 ----
    println!("\n短查询质量（余弦相似度，期望第一句排第一）");
    let mut passed = 0;
    for (query, candidates) in QUALITY_CASES {
        let query_vec = embed(&mut model, query)?;
        let mut scored: Vec<(f32, &str)> = Vec::new();
        for candidate in candidates {
            let vector = embed(&mut model, candidate)?;
            scored.push((cosine(&query_vec, &vector), candidate));
        }
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).expect("分数不是 NaN"));
        let top_is_expected = scored[0].1 == candidates[0];
        if top_is_expected {
            passed += 1;
        }
        println!(
            "  查询「{query}」→ {}（{:.3}）；相关句得分 {:.3}",
            if top_is_expected { "正确" } else { "排序错误" },
            scored[0].0,
            scored
                .iter()
                .find(|(_, text)| *text == candidates[0])
                .map(|(score, _)| *score)
                .unwrap_or(f32::NAN)
        );
    }
    println!("  3 条查询中 {passed} 条排序正确");

    println!("\n结论与限制");
    println!("  - 上面是真实测量的模型体积、加载耗时、单条与长文本推理耗时、进程峰值内存。");
    println!("  - 质量检查只有 3 条虚构样例，**不构成质量评估**；任务书要求的检索质量集还没建。");
    println!(
        "  - 本次运行平台：{}/{}。arm64（真机）上的算子与耗时仍未验证。",
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    println!("  - 没有测批量（padding 后一次前向）与多线程配置，实测的是单序列逐条推理。");
    println!("  - 退出阶段不再崩溃（#17 已解决）：桌面靠进程级单例（退出不释放），");
    println!("    Android 还要跳过 C 运行时的退出收尾（libc::_exit，见 docs/architecture/M0-技术验证.md 3.3）。");
    if passed < QUALITY_CASES.len() {
        bail!("有查询排序错误，说明预处理或 pooling 可能不对");
    }

    // 这里**不需要**任何收尾：模型资源在 `EMBEDDER` 这个 static 里，生命周期到
    // 进程结束。会话释放与 ONNX Runtime 退出收尾的冲突已解决——见 EMBEDDER 的
    // 文档与 `docs/architecture/M0-技术验证.md` 3.3。
    Ok(())
}
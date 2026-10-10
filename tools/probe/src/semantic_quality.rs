//! B3c-2 语义一路的量测：真模型、质量集、pooling × 指令前缀四组、f32 vs int8。
//!
//! 三件事，用同一份质量集、同一套口径（按来源算召回、同一来源的多个块折叠成一条）：
//!
//! 1. **产品路径**：真核心 `build_semantic_index` + `search.start(mode = semantic)`，
//!    每类各报 R@1/R@5/R@10/MRR/中位延迟。这是「线上到底给用户什么」的数字；
//! 2. **pooling（CLS / mean）× 指令前缀（加 / 不加）四组**：同一个模型、同一个前向
//!    输出，只改取哪一位与查询侧要不要拼前缀。量的是**怎么用这个模型**，不是换模型；
//! 3. **f32 vs int8**：对已经算出来的 f32 向量做 int8 量化模拟，报召回变化与体积。
//!    只有数字支持才在存储上采纳 int8。
//!
//! 需要真模型与 ORT 动态库：
//!
//! ```bash
//! DIARY_MODEL_DIR="$PWD/models/bge-small-zh-v1.5-ours" \
//! DIARY_ORT_DYLIB="$HOME/dev/onnxruntime-linux-x64-1.22.0/lib/libonnxruntime.so" \
//! cargo run --release -p diary_probe -- semantic-quality
//! ```
//!
//! 口径上的两处诚实说明：
//!
//! - 产品路径的召回名次用 `SearchHit.group_id`（来源篇 = `source_id`），与关键词那
//!   一路同一个口径；
//! - 四组对比与 f32/int8 走**探针自己的打分循环**（同一份分块结果 + 同一个嵌入器 +
//!   同一个余弦），因为它要能控制 pooling 与非缓存地切换存储格式；产品路径那一趟
//!   才是端到端。两边的差距在文档里如实写出来。

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Instant;

use anyhow::{bail, Result};
use diary_core::{
    config_from_env, load_embedder, pack_pieces, Core, EmbedOptions, Embedder, EmbeddingConfig,
    EnvEmbedding, Piece, Pooling, SearchFilters, SearchMode, SearchRequest,
};

use crate::quality::{self, Case, CaseOutcome, QualitySet, TOP_K};

/// 四组口径。顺序固定，输出才好对比。
const COMBOS: [(&str, Pooling, bool); 4] = [
    ("CLS + 无前缀", Pooling::Cls, false),
    ("CLS + 指令前缀", Pooling::Cls, true),
    ("mean + 无前缀", Pooling::Mean, false),
    ("mean + 指令前缀", Pooling::Mean, true),
];

pub fn run() -> Result<()> {
    let set = quality::load()?;
    let config = match config_from_env() {
        EnvEmbedding::Ready(config) => config,
        EnvEmbedding::Unset => bail!(
            "需要 DIARY_MODEL_DIR（模型目录）与 DIARY_ORT_DYLIB（ORT 动态库）；\
             见 README/CONTRIBUTING"
        ),
        EnvEmbedding::Invalid(reason) => bail!("{reason}"),
    };
    config
        .check_files()
        .map_err(|reason| anyhow::anyhow!(reason))?;

    println!("===========================================");
    println!("语义一路量测（真模型）");
    println!("  模型目录：{}", config.model_dir.display());
    println!("  ORT：{}", config.ort_dylib.display());

    let probe = load_embedder(&config, EmbedOptions::default())
        .map_err(|reason| anyhow::anyhow!(reason))?;
    println!(
        "  model_version：{} · {} 维",
        probe.model_version(),
        probe.dims()
    );
    println!(
        "  产品默认口径：pooling = {} · 查询前缀 = {}",
        diary_core::DEFAULT_POOLING.wire(),
        if diary_core::DEFAULT_QUERY_INSTRUCTION {
            "加"
        } else {
            "不加"
        }
    );

    product_path(&set, &config, probe.dims())?;
    no_answer_scores(&set, &config)?;
    pooling_and_instruction(&set, &config, probe.dims())?;
    f32_vs_int8(&set, &config, probe.dims())?;
    projection(probe.dims())?;
    Ok(())
}

// ------------------------------------------------------------ 1. 产品路径

/// 真核心端到端：`build_semantic_index` → `search.start(mode = semantic)`。
///
/// 用**落文件的库**（不是内存库）：这样能另外打开它量 `chunk_vectors` 的真实字节数，
/// 存储数字就不是算出来的。
fn product_outcomes(
    set: &QualitySet,
    config: &EmbeddingConfig,
    options: EmbedOptions,
) -> Result<(Vec<CaseOutcome>, StorageTotals)> {
    let mut outcomes: Vec<CaseOutcome> = Vec::with_capacity(set.cases.len());
    let mut storage = StorageTotals::default();
    for case in &set.cases {
        let (mut core, source_ids, dir) = quality::seed_case(case, true)?;
        let outcome = (|| -> Result<(CaseOutcome, i64)> {
            let embedder =
                load_embedder(config, options).map_err(|reason| anyhow::anyhow!(reason))?;
            core.set_embedder(Box::new(embedder));
            let built = core.build_semantic_index(None)?;
            let request = SearchRequest {
                query: case.query.clone(),
                mode: SearchMode::Semantic,
                filters: SearchFilters {
                    from_day_key: case.filters.from_day_key.clone(),
                    to_day_key: case.filters.to_day_key.clone(),
                    ..Default::default()
                },
                page_size: TOP_K as i64,
            };
            let start = Instant::now();
            let snapshot = core.start_search(request, 1)?;
            let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;

            // 名次：期望来源（按 group_id）第一次出现的位置。
            let mut first_seen: BTreeMap<&str, usize> = BTreeMap::new();
            for (position, hit) in snapshot.results.iter().enumerate() {
                first_seen.entry(hit.group_id.as_str()).or_insert(position + 1);
            }
            let hit_rank = if case.expected_source_index >= 0 {
                first_seen
                    .get(source_ids[case.expected_source_index as usize].as_str())
                    .copied()
            } else {
                None
            };
            Ok((
                CaseOutcome {
                    category: case.category.clone(),
                    hit_rank,
                    matched_sources: first_seen.len(),
                    hits: snapshot.results.len(),
                    elapsed_ms,
                    warnings: Vec::new(),
                },
                built.total_chunks,
            ))
        })();
        let (outcome, chunks) = outcome?;
        storage.merge(&core, &dir, chunks)?;
        std::fs::remove_dir_all(&dir).ok();
        outcomes.push(outcome);
    }
    Ok((outcomes, storage))
}

fn product_path(set: &QualitySet, config: &EmbeddingConfig, dims: usize) -> Result<()> {
    println!("\n[1] 产品路径（build_semantic_index + search.start，mode = semantic）");
    let (outcomes, storage) = product_outcomes(set, config, EmbedOptions::default())?;
    quality::report(set, &outcomes, "语义")?;
    println!(
        "  存储（这 {} 个 case 的全部块，f32）：{} 块 · chunk_vectors {} · \
         text_chunks 正文 {} · {} 个 span · 每块向量 {:.0} 字节（{} 维 × 4）",
        set.cases.len(),
        storage.chunks,
        human_bytes(storage.vector_bytes),
        human_bytes(storage.chunk_text_bytes),
        storage.spans,
        storage.vector_bytes as f64 / storage.chunks.max(1) as f64,
        dims,
    );
    println!(
        "  dbstat 的 index_bytes 合计是 {}（{} 个**独立小库**相加，每个都有固定页开销，\
         不能除以块数当「每块占用」；换成 10 万块的单库才是那个数字）",
        human_bytes(storage.index_bytes),
        set.cases.len(),
    );
    Ok(())
}

#[derive(Default)]
struct StorageTotals {
    chunks: i64,
    vector_bytes: i64,
    chunk_text_bytes: i64,
    spans: i64,
    index_bytes: i64,
}

impl StorageTotals {
    fn merge(&mut self, core: &Core, dir: &Path, chunks: i64) -> Result<()> {
        let status = core.index_status(None)?;
        self.chunks += chunks;
        self.index_bytes += status.index_bytes;
        let conn = rusqlite::Connection::open(dir.join("library.sqlite"))?;
        let (vector_bytes, chunk_text_bytes, spans): (i64, i64, i64) = conn.query_row(
            "SELECT (SELECT COALESCE(SUM(LENGTH(vector)), 0) FROM chunk_vectors), \
                    (SELECT COALESCE(SUM(LENGTH(text)), 0) FROM text_chunks), \
                    (SELECT COUNT(*) FROM chunk_spans)",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        self.vector_bytes += vector_bytes;
        self.chunk_text_bytes += chunk_text_bytes;
        self.spans += spans;
        Ok(())
    }
}

// ------------------------------------------------------------ 2. no_answer 的分数

/// `no_answer` 类**必须量**：语义检索天生会返回「最像的那几个」，没有分数阈值就
/// 谈不上「不误报」。这里把每条的返回条数与最高/最低分报出来，供决定要不要设阈值。
fn no_answer_scores(set: &QualitySet, config: &EmbeddingConfig) -> Result<()> {
    println!("\n[2] no_answer：语义会不会乱报（没有分数阈值时的真实样子）");
    for case in set.cases.iter().filter(|case| case.category == "no_answer") {
        let (mut core, _ids, dir) = quality::seed_case(case, true)?;
        let result = (|| -> Result<(usize, f32, f32)> {
            let embedder = load_embedder(config, EmbedOptions::default())
                .map_err(|reason| anyhow::anyhow!(reason))?;
            core.set_embedder(Box::new(embedder));
            core.build_semantic_index(None)?;
            let filters = SearchFilters {
                from_day_key: case.filters.from_day_key.clone(),
                to_day_key: case.filters.to_day_key.clone(),
                ..Default::default()
            };
            let ranking = core.semantic_ranking(&case.query, &filters)?;
            let top = ranking.first().map_or(0.0, |(_, score)| *score);
            let bottom = ranking.last().map_or(0.0, |(_, score)| *score);
            Ok((ranking.len(), top, bottom))
        })();
        std::fs::remove_dir_all(&dir).ok();
        let (hits, top, bottom) = result?;
        println!(
            "  {} 查询「{}」→ 返回 {hits} 条 · 最高分 {top:.3} · 最低分 {bottom:.3}",
            case.id, case.query
        );
    }
    println!(
        "  说明：纯语义是 kNN，没有「没有答案」这个概念；要不要拒绝只能靠分数阈值，\
         而阈值要用真实数据定。关键词那一路的 4/4 空结果不能直接搬过来。"
    );
    Ok(())
}

// ------------------------------------------------------------ 3. pooling × 前缀

fn pooling_and_instruction(
    set: &QualitySet,
    config: &EmbeddingConfig,
    dims: usize,
) -> Result<()> {
    println!("\n[3] pooling × 指令前缀（四组，同一模型同一前向输出）");
    println!("  四组都走**产品路径**（真核心建索引 + search.start），不是探针自己的打分循环；");
    println!("  两边各用各的口径会让「选哪一组」变成选择实现。");
    let mut summary: Vec<(&str, f64, f64, f64, f64, f64)> = Vec::new();
    for (label, pooling, instruction) in COMBOS {
        let options = EmbedOptions {
            pooling,
            query_instruction: instruction,
        };
        let (outcomes, _storage) = product_outcomes(set, config, options)?;
        println!("\n  口径：{label}");
        quality::report(set, &outcomes, "语义")?;
        let (r1, r5, r10, mrr, semantic_top) = summarize(&outcomes);
        summary.push((label, r1, r5, r10, mrr, semantic_top));
    }

    println!("\n  四组汇总（召回按来源算；semantic_only 那一列只算该类的 R@{TOP_K}）");
    println!(
        "  {:<18} {:>6} {:>6} {:>7} {:>7} {:>18}",
        "口径", "R@1", "R@5", "R@10", "MRR", "semantic_only R@10"
    );
    for (label, r1, r5, r10, mrr, semantic_top) in &summary {
        println!(
            "  {label:<18} {r1:>5.0}% {r5:>5.0}% {r10:>6.0}% {mrr:>7.3} {semantic_top:>17.0}%"
        );
    }
    println!("  向量维度：{dims}");
    Ok(())
}

/// 一批结果的可回答类合计（`no_answer` 不算召回）。
fn summarize(outcomes: &[CaseOutcome]) -> (f64, f64, f64, f64, f64) {
    let answerable: Vec<&CaseOutcome> = outcomes
        .iter()
        .filter(|outcome| outcome.category != "no_answer")
        .collect();
    let count = answerable.len() as f64;
    let rate = |k: usize| -> f64 {
        answerable
            .iter()
            .filter(|outcome| outcome.hit_rank.is_some_and(|rank| rank <= k))
            .count() as f64
            / count
            * 100.0
    };
    let mrr: f64 = answerable
        .iter()
        .filter_map(|outcome| outcome.hit_rank)
        .map(|rank| 1.0 / rank as f64)
        .sum::<f64>()
        / count;
    let semantic_only: Vec<&CaseOutcome> = outcomes
        .iter()
        .filter(|outcome| outcome.category == "semantic_only")
        .collect();
    let semantic_top = if semantic_only.is_empty() {
        0.0
    } else {
        semantic_only
            .iter()
            .filter(|outcome| outcome.hit_rank.is_some_and(|rank| rank <= TOP_K))
            .count() as f64
            / semantic_only.len() as f64
            * 100.0
    };
    (rate(1), rate(5), rate(10), mrr, semantic_top)
}

// ------------------------------------------------------------ 4. f32 vs int8

fn f32_vs_int8(set: &QualitySet, config: &EmbeddingConfig, dims: usize) -> Result<()> {
    println!("\n[4] f32 vs int8（对算出来的 f32 向量做 int8 量化模拟）");
    println!("  这一节走探针自己的打分循环：产品路径只实现 f32，int8 只能对已有 f32 向量模拟；");
    println!("  分块、嵌入器与余弦都是同一份，差别只在存储格式。");
    let embedder = load_embedder(config, EmbedOptions::default())
        .map_err(|reason| anyhow::anyhow!(reason))?;
    for (label, storage) in [("f32（当前实现）", Storage::F32), ("int8（模拟）", Storage::I8)] {
        let outcomes = harness(set, &embedder, storage)?;
        let (top1, bottom) = cosine_drift(set, &embedder)?;
        println!("\n  存储：{label} · 向量间余弦平均 {top1:.4} · 最差 {bottom:.4}");
        quality::report(set, &outcomes, "语义")?;
    }
    let f32_bytes = dims * 4;
    let i8_bytes = dims + 4; // dims 个 i8 + 4 字节 scale
    println!(
        "\n  体积（每个向量）：f32 {f32_bytes} 字节 · int8 {i8_bytes} 字节（{:.1}×）",
        f32_bytes as f64 / i8_bytes as f64
    );
    Ok(())
}

/// f32 与 int8 模拟向量之间的余弦：1.000 表示量化没损失。
fn cosine_drift(set: &QualitySet, embedder: &dyn Embedder) -> Result<(f64, f64)> {
    let mut values = Vec::new();
    for case in &set.cases {
        let pieces: Vec<Piece<'_>> = case
            .sources
            .iter()
            .map(|source| Piece {
                id: &source.name,
                day_key: &source.day_key,
                text: &source.text,
            })
            .collect();
        for chunk in pack_pieces(&pieces) {
            let vector = embedder.embed_document(&chunk.text)?;
            let quantized = quantize_i8(&vector);
            values.push(f64::from(cosine(&vector, &quantized)));
        }
    }
    if values.is_empty() {
        return Ok((0.0, 0.0));
    }
    let mean = values.iter().sum::<f64>() / values.len() as f64;
    let worst = values.iter().copied().fold(f64::INFINITY, f64::min);
    Ok((mean, worst))
}

// ------------------------------------------------------------ 5. 投影

fn projection(dims: usize) -> Result<()> {
    println!("\n[5] 10 万块的投影（本机没有真跑 10 万块，按每块实测字节数算）");
    let vectors = 100_000_f64;
    let f32_bytes = vectors * dims as f64 * 4.0;
    let i8_bytes = vectors * (dims as f64 + 4.0);
    println!(
        "  裸向量：f32 {:.0} MiB · int8 {:.0} MiB（任务书 5.4 节的 195 MiB 就是 f32 这一档）",
        f32_bytes / (1024.0 * 1024.0),
        i8_bytes / (1024.0 * 1024.0),
    );
    println!(
        "  查询时的内存：当前实现把生效代次的**全部向量**读进内存再算余弦 —— f32 是 \
         {:.0} MiB/次查询。这是这一片最大的短板，mmap / ANN 是 B5 的待办。",
        f32_bytes / (1024.0 * 1024.0),
    );
    println!(
        "  磁盘：上面是裸向量；再加上 chunk_vectors 的行开销（chunk_id + generation + \
         model_version + 时间戳，每行约百来字节）与块正文（text_chunks，见 [1] 的实测总量）。"
    );
    Ok(())
}

// ------------------------------------------------------------ 探针自己的打分循环

/// 量测用的存储格式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Storage {
    F32,
    I8,
}

/// 与产品路径同一套分块、同一个嵌入器、同一个余弦；只是不经过数据库，
/// 好让 pooling / 指令前缀 / 存储格式能逐个切换。
fn harness(set: &QualitySet, embedder: &dyn Embedder, storage: Storage) -> Result<Vec<CaseOutcome>> {
    let mut outcomes = Vec::with_capacity(set.cases.len());
    for case in &set.cases {
        outcomes.push(harness_case(case, embedder, storage)?);
    }
    Ok(outcomes)
}

fn harness_case(case: &Case, embedder: &dyn Embedder, storage: Storage) -> Result<CaseOutcome> {
    let pieces: Vec<Piece<'_>> = case
        .sources
        .iter()
        .map(|source| Piece {
            id: &source.name,
            day_key: &source.day_key,
            text: &source.text,
        })
        .collect();
    let chunks = pack_pieces(&pieces);

    // 建索引那一半（不计入查询延迟）。
    let mut indexed: Vec<(&str, &str, Vec<f32>)> = Vec::new();
    for chunk in &chunks {
        let vector = embedder.embed_document(&chunk.text)?;
        let vector = match storage {
            Storage::F32 => vector,
            Storage::I8 => quantize_i8(&vector),
        };
        for span in &chunk.spans {
            let day_key = case
                .sources
                .iter()
                .find(|source| source.name == span.piece_id)
                .map_or("", |source| source.day_key.as_str());
            indexed.push((span.piece_id.as_str(), day_key, vector.clone()));
        }
    }

    // 查询那一半：算查询向量 → 打分 → 折叠 → 排序。
    let start = Instant::now();
    let query = embedder.embed_query(&case.query)?;
    let mut best: BTreeMap<&str, f32> = BTreeMap::new();
    for (name, day_key, vector) in &indexed {
        if let Some(from) = &case.filters.from_day_key {
            if day_key < &from.as_str() {
                continue;
            }
        }
        if let Some(to) = &case.filters.to_day_key {
            if day_key > &to.as_str() {
                continue;
            }
        }
        let score = cosine(&query, vector);
        let entry = best.entry(name).or_insert(f32::MIN);
        if score > *entry {
            *entry = score;
        }
    }
    let mut ranked: Vec<(&str, f32)> = best.into_iter().collect();
    ranked.sort_by(|left, right| {
        right
            .1
            .partial_cmp(&left.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                let left_day = case
                    .sources
                    .iter()
                    .find(|source| source.name == left.0)
                    .map_or("", |source| source.day_key.as_str());
                let right_day = case
                    .sources
                    .iter()
                    .find(|source| source.name == right.0)
                    .map_or("", |source| source.day_key.as_str());
                right_day.cmp(left_day)
            })
            .then_with(|| left.0.cmp(right.0))
    });
    let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;

    let expected = if case.expected_source_index >= 0 {
        Some(case.sources[case.expected_source_index as usize].name.as_str())
    } else {
        None
    };
    let hit_rank = expected.and_then(|name| {
        ranked
            .iter()
            .position(|(candidate, _)| *candidate == name)
            .map(|position| position + 1)
    });

    Ok(CaseOutcome {
        category: case.category.clone(),
        hit_rank,
        matched_sources: ranked.len(),
        hits: ranked.len(),
        elapsed_ms,
        warnings: Vec::new(),
    })
}

/// int8 量化模拟：按最大绝对值定 scale，取整到 [-127, 127]，再还原成 f32。
///
/// 这是**存储**量化（每向量一个 scale），不是把模型换成 int8 ONNX；它量的是
/// 「向量存成 int8 会掉多少召回」。
fn quantize_i8(vector: &[f32]) -> Vec<f32> {
    let max = vector.iter().fold(0_f32, |acc, value| acc.max(value.abs()));
    // 全零向量（或出现 NaN）：没有可量化的范围，原样返回。
    if max <= 0.0 || !max.is_finite() {
        return vector.to_vec();
    }
    let scale = max / 127.0;
    vector
        .iter()
        .map(|value| (value / scale).round().clamp(-127.0, 127.0) * scale)
        .collect()
}

fn cosine(left: &[f32], right: &[f32]) -> f32 {
    let mut dot = 0_f32;
    let mut left_norm = 0_f32;
    let mut right_norm = 0_f32;
    for (a, b) in left.iter().zip(right) {
        dot += a * b;
        left_norm += a * a;
        right_norm += b * b;
    }
    let denominator = left_norm.sqrt() * right_norm.sqrt();
    if denominator > 0.0 {
        dot / denominator
    } else {
        0.0
    }
}

fn human_bytes(bytes: i64) -> String {
    let value = bytes as f64;
    if value >= 1024.0 * 1024.0 {
        format!("{:.2} MiB", value / (1024.0 * 1024.0))
    } else if value >= 1024.0 {
        format!("{:.1} KiB", value / 1024.0)
    } else {
        format!("{bytes} B")
    }
}

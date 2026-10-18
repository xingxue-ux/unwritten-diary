//! 大库可用性实测（issue #90）：万到十万块量级的构建、查询、内存与体积。
//!
//! 这一片要回答的是**数字**，不是感觉：
//!
//! 1. 当前实现（把生效代次的全部向量读进堆再算余弦）在 1 万 / 5 万 / 10 万块上的
//!    查询 P50/P95、常驻内存、构建时间、向量表体积；
//! 2. 换成 int8 存储之后，同一套语料上的召回与体积；
//! 3. 分块流式扫描（不再把整代向量读进堆）之后的查询峰值内存。
//!
//! 为什么在探针里而不是测试里：真模型、几分钟、几十万次嵌入，不适合 CI。测试只钉
//! 结构性的东西（int8 编解码往返、换格式触发整代重算、流式与全量结果一致）。
//!
//! 语料是**确定性生成的虚构中文**：每块约 400 字、互不相同（带序号标记），
//! 每块单独一天，于是「块数」就是可以精确指定的量。写入走**直接 SQL**（绕过导入与
//! 关键词索引）：这一片量的是语义索引的构建与查询，不是导入流水线；绕过什么、为什么
//! 绕过，都在输出里写明。
//!
//! 用法：
//!
//! ```bash
//! DIARY_MODEL_DIR="$HOME/dev/caches/bge-small-zh-v1.5-ours" \
//! DIARY_ORT_DYLIB="$HOME/dev/caches/host-ort/libonnxruntime.so" \
//! cargo run --release -p diary_probe -- large-bench build 10000 --storage f32 --dir "$HOME/big90-scratch/bench-10k-f32"
//! # 然后另起一个进程量查询（两个阶段分开，查询内存才不会混进构建的峰值）：
//! DIARY_MODEL_DIR=... DIARY_ORT_DYLIB=... \
//! cargo run --release -p diary_probe -- large-bench query 10000 --storage f32 --dir "$HOME/big90-scratch/bench-10k-f32"
//!
//! # 可选：--profile 打「1.5 秒花在哪」的分段计时；--scan-depth 改快速路径的候选
//! # 上限（默认见 semantic::FAST_SCAN_DEPTH）；--mmap-mib 试 SQLite mmap（默认关）。
//! # 这三个都只影响量测，不是产品配置。
//! cargo run --release -p diary_probe -- large-bench query 100000 --storage int8 \
//!   --dir "$HOME/big90-scratch/bench-100k-int8-hash" --queries 40 --profile
//! ```

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use diary_core::{
    config_from_env, load_embedder, set_query_profiling, take_query_profile, text_hash_vector, Core,
    Embedder, EmbeddingConfig, EmbedOptions, EnvEmbedding, SearchFilters, SearchMode, SearchRequest,
    SemanticQueryProfile, VectorStorage,
};
use rusqlite::{params, Connection};

/// 每块的目标字符数上限（与 `MAX_CHUNK_CHARS` 一致）。生成时留 4 个字符的余量，
/// 保证「一块 = 一篇」：超过 400 会被篇内切开，块数就不再等于指定的篇数。
const PIECE_TARGET_CHARS: usize = 396;

/// 生成语料用的词表。词长 2–4 字，拼起来像日记；序号标记保证每篇互不相同。
const WORDS: [&str; 48] = [
    "今天", "妈妈", "打电话", "公司", "项目", "加班", "地铁", "天气", "朋友", "医生",
    "报告", "合同", "房租", "房东", "疫苗", "体检", "咖啡", "失眠", "跑步", "桂花",
    "面试", "面试官", "离职", "同事", "会议", "需求", "版本", "上线", "故障", "复盘",
    "周末", "公园", "孩子", "学校", "老师", "作业", "晚饭", "面条", "超市", "牛奶",
    "父亲", "医院", "挂号", "药费", "火车", "车站", "行李", "计划",
];

// ------------------------------------------------------------ 子命令

pub fn run(args: &[String]) -> Result<()> {
    let Some(command) = args.first().map(String::as_str) else {
        bail!("用法：large-bench <build|query|recall-quality> <块数> [--storage f32|int8] [--dir DIR] [--queries N] [--profile] [--scan-depth N] [--mmap-mib N]");
    };
    match command {
        "build" => {
            let blocks = number_arg(args, 1)?;
            let options = Options::parse(args)?;
            run_build(blocks, &options)
        }
        "query" => {
            let blocks = number_arg(args, 1)?;
            let options = Options::parse(args)?;
            run_query(blocks, &options)
        }
        "recall-quality" => {
            let options = Options::parse(args)?;
            run_recall_quality(&options)
        }
        "embed-speed" => {
            let blocks = number_arg(args, 1).unwrap_or(100);
            run_embed_speed(blocks)
        }
        "recall-large" => {
            let blocks = number_arg(args, 1).unwrap_or(2_000);
            let options = Options::parse(args)?;
            run_recall_large(blocks, &options)
        }
        other => bail!("未知的 large-bench 子命令：{other}"),
    }
}

/// 量测参数。`storage` 与 `dir` 两个阶段必须一致：query 阶段打开的就是 build 阶段
/// 写出来的那个库，格式不一致会走口径判据（ForeignSpace）而不是给出错误答案。
/// 文档向量怎么来。`Model` 是真实模型（构建时间的真值）；`Hash` 是确定性哈希向量，
/// 只用于**快速把 10 万条向量灌进库**量查询延迟/内存（查询阶段仍用真模型算查询向量）。
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum VectorMode {
    Model,
    Hash,
}

pub(crate) struct Options {
    pub(crate) storage: VectorStorage,
    pub(crate) dir: PathBuf,
    pub(crate) queries: usize,
    pub(crate) vectors: VectorMode,
    /// `false` = 量测时把 `synchronous` 调成 OFF（排除每块一次 fsync）。
    /// 产品默认是 FULL，两种口径分开报。
    pub(crate) synchronous_full: bool,
    /// 打开语义查询路径的分段计时，热态查询逐条收集后报中位数。
    pub(crate) profile: bool,
    /// 量测时给这条连接设的 SQLite `mmap_size`（MiB）；`None` = 不动。
    pub(crate) mmap_mib: Option<i64>,
    /// 覆盖快速路径的候选块数上限；`None` = 产品默认。
    pub(crate) scan_depth: Option<usize>,
}

impl Options {
    fn parse(args: &[String]) -> Result<Self> {
        let mut storage = VectorStorage::F32;
        let mut dir: Option<PathBuf> = None;
        let mut queries = 40_usize;
        let mut synchronous_full = true;
        let mut vectors = VectorMode::Model;
        let mut profile = false;
        let mut mmap_mib: Option<i64> = None;
        let mut scan_depth: Option<usize> = None;
        let mut index = 0;
        while index < args.len() {
            match args[index].as_str() {
                "--storage" => {
                    let value = args
                        .get(index + 1)
                        .context("--storage 后面要跟 f32 或 int8")?;
                    storage = VectorStorage::from_wire(value)
                        .with_context(|| format!("不认识的存储格式：{value}"))?;
                    index += 2;
                }
                "--dir" => {
                    dir = Some(PathBuf::from(
                        args.get(index + 1).context("--dir 后面要跟目录")?,
                    ));
                    index += 2;
                }
                "--vectors" => {
                    let value = args.get(index + 1).context("--vectors 后面要跟 model 或 hash")?;
                    vectors = match value.as_str() {
                        "model" => VectorMode::Model,
                        "hash" => VectorMode::Hash,
                        other => bail!("--vectors 只认 model / hash，拿到 {other}"),
                    };
                    index += 2;
                }
                "--sync" => {
                    let value = args.get(index + 1).context("--sync 后面要跟 full 或 off")?;
                    synchronous_full = match value.as_str() {
                        "full" => true,
                        "off" => false,
                        other => bail!("--sync 只认 full / off，拿到 {other}"),
                    };
                    index += 2;
                }
                "--queries" => {
                    queries = args
                        .get(index + 1)
                        .context("--queries 后面要跟次数")?
                        .parse()
                        .context("--queries 必须是正整数")?;
                    index += 2;
                }
                "--profile" => {
                    profile = true;
                    index += 1;
                }
                "--scan-depth" => {
                    scan_depth = Some(
                        args.get(index + 1)
                            .context("--scan-depth 后面要跟候选块数")?
                            .parse()
                            .context("--scan-depth 必须是正整数")?,
                    );
                    index += 2;
                }
                "--mmap-mib" => {
                    mmap_mib = Some(
                        args.get(index + 1)
                            .context("--mmap-mib 后面要跟 MiB 数（0 = 关）")?
                            .parse()
                            .context("--mmap-mib 必须是整数")?,
                    );
                    index += 2;
                }
                // 位置参数（块数）已经由调用方读走，跳过。
                value if value.parse::<usize>().is_ok() => index += 1,
                value if value.starts_with("--") => bail!("不认识的参数：{value}"),
                _ => index += 1,
            }
        }
        let dir = dir.unwrap_or_else(|| {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_owned());
            // 任务约束：不用 /tmp（会被清空），量测数据放 `$HOME/big90-scratch`。
            PathBuf::from(home)
                .join("big90-scratch")
                .join(format!("bench-{}-{}", dir_suffix(args), storage.wire()))
        });
        Ok(Self {
            storage,
            dir,
            queries: queries.max(30),
            synchronous_full,
            vectors,
            profile,
            mmap_mib,
            scan_depth,
        })
    }
}

/// 默认目录名里带上块数，免得不同规模互相覆盖。
fn dir_suffix(args: &[String]) -> String {
    args.iter()
        .find_map(|value| value.parse::<usize>().ok())
        .map(|blocks| format!("{blocks}"))
        .unwrap_or_else(|| "unknown".to_owned())
}

fn number_arg(args: &[String], position: usize) -> Result<usize> {
    args.get(position)
        .context("缺少块数参数（用法：large-bench <build|query> <块数>）")?
        .parse()
        .context("块数必须是正整数")
}

// ------------------------------------------------------------ build

fn run_build(blocks: usize, options: &Options) -> Result<()> {
    let config = model_config()?;
    fs::create_dir_all(&options.dir)
        .with_context(|| format!("建不了量测目录：{}", options.dir.display()))?;
    let db_path = options.dir.join("library.sqlite");

    println!("===========================================");
    println!("大库实测 · build");
    println!("  块数：{blocks}");
    println!("  存储：{}", options.storage.wire());
    println!(
        "  文档向量：{}",
        match options.vectors {
            VectorMode::Model => "真模型",
            VectorMode::Hash => "确定性哈希向量（只为快速灌 10 万条量查询；构建时间这一栏不作数）",
        }
    );
    println!("  目录：{}", options.dir.display());
    println!("  进程：pid {}", std::process::id());

    let seed_start = Instant::now();
    let seeded = seed_library(&db_path, blocks)?;
    println!(
        "[seed] {} · {:.1} 秒（直接 SQL 写 captures；绕过导入与关键词索引，只量语义索引）",
        if seeded {
            format!("写入 {blocks} 篇（每篇约 {PIECE_TARGET_CHARS} 字、各自一天）")
        } else {
            "库已存在，复用".to_owned()
        },
        seed_start.elapsed().as_secs_f64()
    );

    let mut core = Core::open(&db_path)?;
    core.set_vector_storage_for_test(options.storage);
    if !options.synchronous_full {
        core.set_synchronous_for_test("OFF")?;
    }
    println!(
        "[sync] synchronous = {}（{}）",
        if options.synchronous_full { "FULL" } else { "OFF" },
        if options.synchronous_full {
            "产品默认：每块一个事务一次 fsync，构建时间包含磁盘代价"
        } else {
            "量测口径：排除每块一次 fsync，只看索引本身"
        }
    );
    // 模型先加载好再计时：构建时间量的是「嵌入 + 写向量」，不是 ONNX 会话初始化。
    let load_start = Instant::now();
    let real_embedder =
        load_embedder(&config, EmbedOptions::default()).map_err(|reason| anyhow::anyhow!(reason))?;
    let dims = real_embedder.dims();
    let model_version = real_embedder.model_version().to_owned();
    real_embedder
        .embed_query("预热")
        .map_err(|error| anyhow::anyhow!("预热嵌入失败：{error}"))?;
    let load_secs = load_start.elapsed().as_secs_f64();
    println!(
        "[model] {} 维 · model_version {} · 加载+预热 {load_secs:.2} 秒",
        dims, model_version
    );
    match options.vectors {
        VectorMode::Model => core.set_embedder(Box::new(real_embedder)),
        VectorMode::Hash => {
            // 哈希向量的 `model_version` **必须**与真模型一致：查询阶段用真模型算查询
            // 向量，口径核对才不会被判成 ForeignSpace。文档向量的值本身不参与延迟
            // 与内存的测量（扫描代价只由条数与字节数决定）。
            drop(real_embedder);
            core.set_embedder(Box::new(HashEmbedder {
                model_version: model_version.clone(),
                dims,
            }));
        }
    }

    let rss_before = rss_kb("VmRSS");
    let sampler = RssSampler::start();

    // 先单独量一次块重建：它是 build_semantic_index 的一部分，但瓶颈在嵌入，
    // 分开报才知道「构建时间」里有多少是分块。
    let chunks_start = Instant::now();
    let total_chunks = core.rebuild_text_chunks(None)?;
    let chunks_secs = chunks_start.elapsed().as_secs_f64();
    if total_chunks as usize != blocks {
        bail!(
            "块数不符：生成了 {blocks} 篇，但分块结果是 {total_chunks} 块——\
             每篇应当恰好一块（>400 字会被篇内切开）"
        );
    }
    println!(
        "[chunks] {total_chunks} 块 · {chunks_secs:.2} 秒（{:.0} 块/秒）",
        total_chunks as f64 / chunks_secs.max(f64::EPSILON)
    );

    let build_start = Instant::now();
    let report = core.build_semantic_index(None)?;
    let build_secs = build_start.elapsed().as_secs_f64();
    let peak_during_build = sampler.finish().max(rss_before);
    let hwm = rss_kb("VmHWM");
    let rss_after = rss_kb("VmRSS");

    println!(
        "[embed] 代次 {} · 总块 {} · 复用 {} · 重算 {} · 续跑 {}",
        report.generation,
        report.total_chunks,
        report.reused_chunks,
        report.embedded_chunks,
        report.resumed_chunks
    );
    println!(
        "[build] {build_secs:.2} 秒（{:.1} 块/秒）{}",
        report.embedded_chunks as f64 / build_secs.max(f64::EPSILON),
        match options.vectors {
            VectorMode::Model => "· 含块重建的完整 build_semantic_index",
            VectorMode::Hash => "· 哈希向量：这一栏只量写库，不含模型推理，不作构建时间用",
        }
    );
    println!(
        "[memory] 构建前 RSS {:.1} MiB · 构建中峰值 {:.1} MiB · 构建后 RSS {:.1} MiB · 进程 HWM {:.1} MiB",
        kb_to_mib(rss_before),
        kb_to_mib(peak_during_build),
        kb_to_mib(rss_after),
        kb_to_mib(hwm)
    );

    let storage = storage_totals(&db_path)?;
    println!(
        "[storage] 向量行 {} · 裸向量 {:.1} MiB · 每向量 {} B · 库文件 {:.1} MiB（WAL {:.1} MiB）",
        storage.rows,
        bytes_to_mib(storage.vector_bytes as u64),
        storage.avg_bytes,
        bytes_to_mib(storage.file_bytes),
        bytes_to_mib(storage.wal_bytes)
    );
    println!(
        "[storage] 按格式：{}",
        storage
            .by_storage
            .iter()
            .map(|(storage, rows, bytes)| format!("{storage} {rows} 行 / {:.1} MiB", bytes_to_mib(*bytes as u64)))
            .collect::<Vec<_>>()
            .join(" · ")
    );
    Ok(())
}

// ------------------------------------------------------------ query

fn run_query(blocks: usize, options: &Options) -> Result<()> {
    let config = model_config()?;
    let db_path = options.dir.join("library.sqlite");
    if !db_path.exists() {
        bail!(
            "库文件不存在：{}；先跑 large-bench build {blocks} --storage {} --dir {}",
            db_path.display(),
            options.storage.wire(),
            options.dir.display()
        );
    }
    println!("===========================================");
    println!("大库实测 · query");
    println!("  块数：{blocks} · 存储：{}", options.storage.wire());
    println!("  目录：{}", options.dir.display());
    println!("  进程：pid {}", std::process::id());

    let mut core = Core::open(&db_path)?;
    core.set_vector_storage_for_test(options.storage);
    if let Some(depth) = options.scan_depth {
        core.set_semantic_scan_depth_for_test(depth);
        println!("[db] 快速路径候选上限 = {depth} 块（仅量测）");
    }
    if let Some(mib) = options.mmap_mib {
        core.set_mmap_size_for_test(mib * 1024 * 1024)?;
        println!("[db] mmap_size = {mib} MiB");
    }

    let load_start = Instant::now();
    let embedder =
        load_embedder(&config, EmbedOptions::default()).map_err(|reason| anyhow::anyhow!(reason))?;
    embedder
        .embed_query("预热")
        .map_err(|error| anyhow::anyhow!("预热嵌入失败：{error}"))?;
    println!(
        "[model] {} 维 · 加载+预热 {:.2} 秒",
        embedder.dims(),
        load_start.elapsed().as_secs_f64()
    );
    core.set_embedder(Box::new(embedder));

    let storage = storage_totals(&db_path)?;
    println!(
        "[db] 生效代次向量 {} 行 · 裸向量 {:.1} MiB · 每向量 {} B",
        storage.rows,
        bytes_to_mib(storage.vector_bytes as u64),
        storage.avg_bytes
    );

    let rss_before = rss_kb("VmRSS");
    let sampler = RssSampler::start();

    let query_indices = query_indices(blocks, options.queries);
    let mut latencies: Vec<f64> = Vec::with_capacity(options.queries);
    let mut cold_ms = None;
    let mut self_hits = 0_usize;
    let mut hit_counts: Vec<usize> = Vec::with_capacity(options.queries);
    if options.profile {
        set_query_profiling(true);
    }
    let mut profiles: Vec<SemanticQueryProfile> = Vec::with_capacity(options.queries);

    for (position, &index) in query_indices.iter().enumerate() {
        let query = query_text(index);
        let started = Instant::now();
        let snapshot = core.start_search(
            SearchRequest {
                query,
                mode: SearchMode::Semantic,
                filters: SearchFilters::default(),
                page_size: 20,
            },
            1,
        )?;
        let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
        // 会话里存着这一代的完整折叠候选；量完立刻取消，避免 40 次查询把内存垒起来。
        core.cancel_search(&snapshot.session_id)?;
        hit_counts.push(snapshot.results.len());
        if snapshot
            .results
            .first()
            .is_some_and(|hit| hit.group_id == capture_id(index))
        {
            self_hits += 1;
        }
        if position == 0 {
            cold_ms = Some(elapsed_ms);
        } else if position > 3 {
            latencies.push(elapsed_ms);
        }
        if options.profile {
            if let Some(profile) = take_query_profile() {
                if position > 3 {
                    profiles.push(profile);
                }
            }
        }
    }

    let peak_during_queries = sampler.finish().max(rss_before);
    let hwm = rss_kb("VmHWM");
    let rss_after = rss_kb("VmRSS");

    let cold = cold_ms.context("至少要有一次查询")?;
    let summary = Summary::of(&latencies);
    println!(
        "[query] 首次（冷：新进程、模型已加载、SQLite 页缓存冷）{cold:.2} ms"
    );
    println!(
        "[query] 热 {} 次 · P50 {:.2} ms · P95 {:.2} ms · 最小 {:.2} · 最大 {:.2} · 平均 {:.2}",
        latencies.len(),
        summary.p50,
        summary.p95,
        summary.min,
        summary.max,
        summary.mean
    );
    println!(
        "[memory] 查询前 RSS {:.1} MiB · 查询中峰值 {:.1} MiB（+{:.1}）· 查询后 RSS {:.1} MiB · 进程 HWM {:.1} MiB",
        kb_to_mib(rss_before),
        kb_to_mib(peak_during_queries),
        kb_to_mib(peak_during_queries.saturating_sub(rss_before)),
        kb_to_mib(rss_after),
        kb_to_mib(hwm)
    );
    println!(
        "[sanity] 首条命中就是被查询的那一篇：{self_hits}/{} · 首屏命中数 {}–{}",
        query_indices.len(),
        hit_counts.iter().min().copied().unwrap_or(0),
        hit_counts.iter().max().copied().unwrap_or(0)
    );
    if options.profile {
        print_profile(&profiles, summary.p50);
    }
    Ok(())
}

/// 分段计时的中位数表。「1.5 秒花在哪」的答案就长这样。
///
/// 各段取的是**每条查询各自的分段值**再取中位数，所以九段相加不必等于墙钟中位数
/// （不同查询的各段最大值不在同一条上）；最后一行的「会话/装配余量」把差额补齐。
fn print_profile(profiles: &[SemanticQueryProfile], wall_p50_ms: f64) {
    if profiles.is_empty() {
        println!("[profile] 没有收集到分段计时（--profile 只对热态查询生效）");
        return;
    }
    let median_ms = |pick: fn(&SemanticQueryProfile) -> u64| -> f64 {
        let mut values: Vec<u64> = profiles.iter().map(pick).collect();
        values.sort_unstable();
        let middle = values.len() / 2;
        let ns = if values.len().is_multiple_of(2) {
            (values[middle - 1] + values[middle]) / 2
        } else {
            values[middle]
        };
        ns as f64 / 1_000_000.0
    };
    let mut rows: Vec<u64> = profiles.iter().map(|profile| profile.rows).collect();
    rows.sort_unstable();
    let rows = rows[rows.len() / 2];
    let total_ms = median_ms(|profile| profile.total_ns);
    let share = |ms: f64| if wall_p50_ms > 0.0 { ms / wall_p50_ms * 100.0 } else { 0.0 };
    println!(
        "[profile] 分段耗时（热态 {} 次查询的中位数 · 每次扫过 {} 行 · 墙钟 P50 {:.2} ms）",
        profiles.len(),
        rows,
        wall_p50_ms
    );
    println!("  {:<22} {:>10} {:>9}", "段", "ms", "占墙钟");
    println!("  {}", "-".repeat(43));
    type StagePicker = fn(&SemanticQueryProfile) -> u64;
    let stages: [(&str, StagePicker); 9] = [
        ("取行·SQLite 步进", |p| p.step_ns),
        ("取行·取 13 列（含 BLOB）", |p| p.fetch_ns),
        ("反序列化 → Vec<f32>", |p| p.decode_ns),
        ("余弦 / 点积", |p| p.cosine_ns),
        ("拼候选（含字符串移动）", |p| p.assemble_ns),
        ("排序", |p| p.sort_ns),
        ("折叠", |p| p.fold_ns),
        ("查询嵌入", |p| p.embed_ns),
        ("SQL 准备", |p| p.prepare_ns),
    ];
    for (label, pick) in stages {
        let ms = median_ms(pick);
        println!("  {label:<22} {ms:>10.2} {:>8.1}%", share(ms));
    }
    let fast_scan_ms = median_ms(|profile| profile.fast_scan_ns);
    let fast_meta_ms = median_ms(|profile| profile.fast_meta_ns);
    let fast_used = profiles.iter().filter(|profile| profile.fast_path).count();
    if fast_used > 0 {
        println!("  {:<22} {:>10.2} {:>8.1}%", "快速路径·单表向量扫描", fast_scan_ms, share(fast_scan_ms));
        println!("  {:<22} {:>10.2} {:>8.1}%", "快速路径·top-K 元数据", fast_meta_ms, share(fast_meta_ms));
        let query_ms = median_ms(|profile| profile.fast_meta_query_ns);
        let build_ms = median_ms(|profile| profile.fast_meta_build_ns);
        println!("  {:<22} {:>10.2} {:>8.1}%", "  · 其中 SQLite 步进", query_ms, share(query_ms));
        println!("  {:<22} {:>10.2} {:>8.1}%", "  · 其中取列+拼候选", build_ms, share(build_ms));
    }
    println!("  {:<22} {:>10.2} {:>8.1}%", "候选路径合计", total_ms, share(total_ms));
    let coverage_ms = median_ms(|profile| profile.coverage_ns);
    let materialize_ms = median_ms(|profile| profile.materialize_ns);
    println!("  {:<22} {:>10.2} {:>8.1}%", "语义覆盖状态计数", coverage_ms, share(coverage_ms));
    println!("  {:<22} {:>10.2} {:>8.1}%", "首屏结果装配", materialize_ms, share(materialize_ms));
    println!(
        "  （快速路径命中 {fast_used}/{} 次查询；未命中时走逐行 join 的精确路径）",
        profiles.len()
    );
    let accounted = total_ms + coverage_ms + materialize_ms;
    let rest = wall_p50_ms - accounted;
    println!("  {:<22} {:>10.2} {:>8.1}%", "会话/结果装配余量", rest, share(rest));
}

/// 查询集：确定性地在库内均匀取 `count` 个块（乘一个与块数互素的步长）。
fn query_indices(blocks: usize, count: usize) -> Vec<usize> {
    if blocks == 0 {
        return Vec::new();
    }
    let step = 7919_usize;
    (0..count).map(|k| (k.wrapping_mul(step) + 13) % blocks).collect()
}

fn query_text(index: usize) -> String {
    // 取该篇正文的前 40 个字符：包含序号标记，足以让「同一篇」在语义上排第一；
    // 查询长度不影响「全量余弦」这个瓶颈的量级。
    piece_text(index).chars().take(40).collect()
}

// ------------------------------------------------------------ 语料

/// 第 `index` 篇的正文：约 400 字、确定性、互不相同。
fn piece_text(index: usize) -> String {
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15 ^ (index as u64).wrapping_mul(0x2545_f491_4f6c_dd1d));
    let mut text = format!("第{index}号记录。");
    // 自己数字符：每轮都 `chars().count()` 是 O(n²)，10 万篇会慢到不可用。
    let mut length = text.chars().count();
    while length < PIECE_TARGET_CHARS {
        let word = WORDS[(rng.next() as usize) % WORDS.len()];
        length += word.chars().count();
        text.push_str(word);
        if rng.next().is_multiple_of(9) {
            length += 1;
            text.push('，');
        }
    }
    text.push('。');
    text
}

fn capture_id(index: usize) -> String {
    format!("cap_large_{index:06}")
}

/// 每篇各自一天。映射是单射：`i = (year-2000)*336 + (month-1)*28 + (day-1)`，
/// 100 万篇也能撑到 2300 年以后，`day_key` 始终是合法的 `YYYY-MM-DD`。
fn day_key(index: usize) -> String {
    let year = 2000 + index / 336;
    let month = (index / 28) % 12 + 1;
    let day = index % 28 + 1;
    format!("{year:04}-{month:02}-{day:02}")
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
}

// ------------------------------------------------------------ 写入 / 统计

/// 建库并直接写 captures。返回是否真的写了（已存在则复用）。
///
/// 为什么不走 `create_draft` / `save_draft`：那两条会给每篇写幂等回执、刷关键词索引
/// （jieba 分词 + 两张倒排表），10 万篇要多花几分钟，而这一片一个关键词都不查。
/// 绕过的部分在输出里如实写明；语义索引本身仍然走**产品路径**
/// `rebuild_text_chunks` + `build_semantic_index`。
fn seed_library(path: &Path, blocks: usize) -> Result<bool> {
    if path.exists() {
        let conn = Connection::open(path)?;
        let captures: i64 = conn.query_row("SELECT COUNT(*) FROM captures", [], |row| row.get(0))?;
        if captures == blocks as i64 {
            return Ok(false);
        }
        drop(conn);
        for suffix in ["", "-wal", "-shm"] {
            let mut candidate = path.as_os_str().to_os_string();
            candidate.push(suffix);
            let _ = fs::remove_file(PathBuf::from(candidate));
        }
    }
    // 先用真正的 Core 建 schema（迁移是产品代码，不能在这里另写一份）。
    {
        let core = Core::open(path)?;
        drop(core);
    }
    let mut conn = Connection::open(path)?;
    conn.pragma_update(None, "synchronous", "OFF")?;
    {
        let tx = conn.transaction()?;
        let mut statement = tx.prepare(
            "INSERT INTO captures (id, revision, state, occurred_at, created_at, updated_at, \
             time_zone, utc_offset_minutes, day_key, draft_text) \
             VALUES (?1, 1, 'draft', ?2, ?2, ?2, 'Asia/Shanghai', 480, ?3, ?4)",
        )?;
        for index in 0..blocks {
            let day = day_key(index);
            statement.execute(params![
                capture_id(index),
                format!("{day}T04:00:00.000Z"),
                day,
                piece_text(index),
            ])?;
        }
        drop(statement);
        tx.commit()?;
    }
    conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
    Ok(true)
}

struct StorageTotals {
    rows: i64,
    vector_bytes: i64,
    avg_bytes: i64,
    file_bytes: u64,
    wal_bytes: u64,
    by_storage: Vec<(String, i64, i64)>,
}

fn storage_totals(path: &Path) -> Result<StorageTotals> {
    let conn = Connection::open(path)?;
    let (rows, vector_bytes): (i64, i64) = conn.query_row(
        "SELECT COUNT(*), COALESCE(SUM(LENGTH(vector)), 0) FROM chunk_vectors \
         WHERE generation = (SELECT active_generation FROM index_meta)",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let mut statement = conn.prepare(
        "SELECT storage, COUNT(*), COALESCE(SUM(LENGTH(vector)), 0) FROM chunk_vectors \
         WHERE generation = (SELECT active_generation FROM index_meta) \
         GROUP BY storage ORDER BY storage",
    )?;
    let by_storage = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?, row.get::<_, i64>(2)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let avg_bytes = if rows > 0 { vector_bytes / rows } else { 0 };
    let file_bytes = fs::metadata(path).map(|meta| meta.len()).unwrap_or(0);
    let wal_path = PathBuf::from(format!("{}-wal", path.display()));
    let wal_bytes = fs::metadata(&wal_path).map(|meta| meta.len()).unwrap_or(0);
    Ok(StorageTotals {
        rows,
        vector_bytes,
        avg_bytes,
        file_bytes,
        wal_bytes,
        by_storage,
    })
}

// ------------------------------------------------------------ 内存 / 统计工具

/// 从 `/proc/self/status` 读一个 `kB` 字段（VmRSS / VmHWM）。
fn rss_kb(field: &str) -> u64 {
    let Ok(status) = fs::read_to_string("/proc/self/status") else {
        return 0;
    };
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix(field) {
            if let Some(value) = rest.trim_start_matches(':').split_whitespace().next() {
                return value.parse().unwrap_or(0);
            }
        }
    }
    0
}

/// 每 2 ms 采一次 RSS，记峰值。`/proc/self/status` 的 VmHWM 是**进程生命周期**的
/// 峰值，会把构建阶段算进去；要分开报构建与查询的峰值，只能自己采。
struct RssSampler {
    stop: Arc<AtomicBool>,
    peak: Arc<AtomicU64>,
    handle: Option<thread::JoinHandle<()>>,
}

impl RssSampler {
    fn start() -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let peak = Arc::new(AtomicU64::new(0));
        let thread_stop = Arc::clone(&stop);
        let thread_peak = Arc::clone(&peak);
        let handle = thread::spawn(move || {
            while !thread_stop.load(Ordering::Relaxed) {
                thread_peak.fetch_max(rss_kb("VmRSS"), Ordering::Relaxed);
                thread::sleep(Duration::from_millis(2));
            }
            thread_peak.fetch_max(rss_kb("VmRSS"), Ordering::Relaxed);
        });
        Self {
            stop,
            peak,
            handle: Some(handle),
        }
    }

    fn finish(mut self) -> u64 {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
        self.peak.load(Ordering::Relaxed)
    }
}

fn kb_to_mib(kb: u64) -> f64 {
    kb as f64 / 1024.0
}

fn bytes_to_mib(bytes: u64) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}

struct Summary {
    p50: f64,
    p95: f64,
    min: f64,
    max: f64,
    mean: f64,
}

impl Summary {
    fn of(samples: &[f64]) -> Self {
        if samples.is_empty() {
            return Self {
                p50: 0.0,
                p95: 0.0,
                min: 0.0,
                max: 0.0,
                mean: 0.0,
            };
        }
        let mut sorted = samples.to_vec();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let percentile = |p: f64| {
            // 最近秩法（nearest-rank）：P95 取第 ceil(0.95*n) 个，样本少时也不虚高。
            let rank = ((p / 100.0) * sorted.len() as f64).ceil() as usize;
            sorted[rank.clamp(1, sorted.len()) - 1]
        };
        Self {
            p50: percentile(50.0),
            p95: percentile(95.0),
            min: sorted[0],
            max: sorted[sorted.len() - 1],
            mean: samples.iter().sum::<f64>() / samples.len() as f64,
        }
    }
}

/// 确定性哈希向量（`text_hash_vector`），只用于「快速把 10 万条向量灌进库」。
///
/// `model_version` 由真模型读出来再交给它：查询阶段用真模型算查询向量，口径三字段
/// 必须完全一致，否则语义这一路会（正确地）判成 `ForeignSpace` 而不是给出结果。
/// 文档向量的**值**不参与延迟/内存测量——扫描代价只由条数与字节数决定。
struct HashEmbedder {
    model_version: String,
    dims: usize,
}

impl Embedder for HashEmbedder {
    fn model_version(&self) -> &str {
        &self.model_version
    }

    fn dims(&self) -> usize {
        self.dims
    }

    fn options(&self) -> EmbedOptions {
        EmbedOptions::default()
    }

    fn embed_document(&self, text: &str) -> diary_core::Result<Vec<f32>> {
        Ok(text_hash_vector(text, self.dims))
    }

    fn embed_query(&self, text: &str) -> diary_core::Result<Vec<f32>> {
        Ok(text_hash_vector(text, self.dims))
    }
}

/// 读模型配置；没配就如实报错（与 `semantic-quality` 同一口径）。
fn model_config() -> Result<EmbeddingConfig> {
    match config_from_env() {
        EnvEmbedding::Ready(config) => {
            config
                .check_files()
                .map_err(|reason| anyhow::anyhow!(reason))?;
            Ok(config)
        }
        EnvEmbedding::Unset => bail!(
            "需要 DIARY_MODEL_DIR（模型目录）与 DIARY_ORT_DYLIB（ORT 动态库）；\
             见 README/CONTRIBUTING"
        ),
        EnvEmbedding::Invalid(reason) => bail!("{reason}"),
    }
}

/// 单独量「一块 400 字」的模型推理耗时，把构建时间里推理与写库分开。
fn run_embed_speed(blocks: usize) -> Result<()> {
    let config = model_config()?;
    let embedder =
        load_embedder(&config, EmbedOptions::default()).map_err(|reason| anyhow::anyhow!(reason))?;
    embedder
        .embed_query("预热")
        .map_err(|error| anyhow::anyhow!("预热嵌入失败：{error}"))?;
    println!("模型 {} 维；对 {blocks} 块各做一次 embed_document（每块约 {PIECE_TARGET_CHARS} 字）", embedder.dims());
    let started = Instant::now();
    let mut total_chars = 0_usize;
    for index in 0..blocks {
        let text = piece_text(index);
        total_chars += text.chars().count();
        embedder
            .embed_document(&text)
            .map_err(|error| anyhow::anyhow!("嵌入失败：{error}"))?;
    }
    let secs = started.elapsed().as_secs_f64();
    println!(
        "[embed-speed] {blocks} 块 · {secs:.2} 秒 · {:.1} 块/秒 · 平均每块 {:.1} ms · 每块正文 {:.0} 字",
        blocks as f64 / secs.max(f64::EPSILON),
        secs * 1000.0 / blocks as f64,
        total_chars as f64 / blocks as f64
    );
    Ok(())
}

// ------------------------------------------------------------ int8 召回对照

/// 一套召回指标。`n` 是参与统计的 case 数（`no_answer` 不算）。
#[derive(Default, Clone)]
struct Recall {
    n: usize,
    r1: usize,
    r10: usize,
    mrr: f64,
}

impl Recall {
    fn add(&mut self, rank: Option<usize>) {
        self.n += 1;
        if let Some(rank) = rank {
            if rank <= 1 {
                self.r1 += 1;
            }
            if rank <= 10 {
                self.r10 += 1;
            }
            self.mrr += 1.0 / rank as f64;
        }
    }

    fn line(&self, label: &str) -> String {
        if self.n == 0 {
            return format!("{label} n=0");
        }
        format!(
            "{label} n={} · R@1 {:.0}% · R@10 {:.0}% · MRR {:.3}",
            self.n,
            percent(self.r1, self.n),
            percent(self.r10, self.n),
            self.mrr / self.n as f64
        )
    }
}

fn percent(part: usize, whole: usize) -> f64 {
    if whole == 0 {
        0.0
    } else {
        part as f64 * 100.0 / whole as f64
    }
}

fn rank_of(
    core: &mut Core,
    query: &str,
    filters: &SearchFilters,
    group_id: &str,
) -> Result<Option<usize>> {
    let ranking = core.semantic_ranking(query, filters)?;
    Ok(ranking
        .iter()
        .position(|(candidate, _)| candidate == group_id)
        .map(|position| position + 1))
}

/// 既有 36 条质量集：同一份语料、同一组查询，f32 与 int8 各建一次索引再比召回。
///
/// 用**产品路径**（`build_semantic_index` + `semantic_ranking`，与 `search.start`
/// 的语义路共用候选与折叠逻辑），不是探针里另写一套余弦。
fn run_recall_quality(_options: &Options) -> Result<()> {
    let set = crate::quality::load()?;
    let config = model_config()?;
    println!("===========================================");
    println!("int8 召回对照 · 既有质量集（{} 条 case）", set.cases.len());

    let mut f32_total = Recall::default();
    let mut int8_total = Recall::default();
    let mut by_category: BTreeMap<String, (Recall, Recall)> = BTreeMap::new();
    let mut changed = 0_usize;
    let mut compared = 0_usize;

    for case in &set.cases {
        if case.expected_source_index < 0 {
            continue; // no_answer：不参与召回
        }
        let index = case.expected_source_index as usize;
        let filters = SearchFilters {
            from_day_key: case.filters.from_day_key.clone(),
            to_day_key: case.filters.to_day_key.clone(),
            ..Default::default()
        };
        let (mut core, source_ids, dir) = crate::quality::seed_case(case, false)?;
        let outcome = (|| -> Result<(Option<usize>, Option<usize>)> {
            let embedder =
                load_embedder(&config, EmbedOptions::default()).map_err(|reason| anyhow::anyhow!(reason))?;
            core.set_embedder(Box::new(embedder));
            let expected = source_ids
                .get(index)
                .with_context(|| format!("case {}：expectedSourceIndex 越界", case.id))?
                .clone();

            core.set_vector_storage_for_test(VectorStorage::F32);
            core.build_semantic_index(None)?;
            let f32_rank = rank_of(&mut core, &case.query, &filters, &expected)?;

            // 换格式 = 换口径：这一代会被整代重算，不是在旧向量上原地改编码。
            core.set_vector_storage_for_test(VectorStorage::Int8);
            let rebuilt = core.build_semantic_index(None)?;
            if rebuilt.reused_chunks != 0 || rebuilt.embedded_chunks != rebuilt.total_chunks {
                bail!(
                    "case {}：换存储格式没有触发整代重算（复用 {}、重算 {}、总块 {}）",
                    case.id,
                    rebuilt.reused_chunks,
                    rebuilt.embedded_chunks,
                    rebuilt.total_chunks
                );
            }
            let int8_rank = rank_of(&mut core, &case.query, &filters, &expected)?;
            Ok((f32_rank, int8_rank))
        })();
        fs::remove_dir_all(&dir).ok();
        let (f32_rank, int8_rank) = outcome?;

        f32_total.add(f32_rank);
        int8_total.add(int8_rank);
        let entry = by_category
            .entry(case.category.clone())
            .or_default();
        entry.0.add(f32_rank);
        entry.1.add(int8_rank);
        compared += 1;
        if f32_rank != int8_rank {
            changed += 1;
        }
    }

    println!("\n[按类别]（名次按来源篇算，同篇多块折叠）");
    for (category, (f32_recall, int8_recall)) in &by_category {
        println!("  {category:<16} {}", f32_recall.line("f32 "));
        println!("  {:<16} {}", "", int8_recall.line("int8"));
    }
    println!("\n[合计]");
    println!("  {}", f32_total.line("f32 "));
    println!("  {}", int8_total.line("int8"));
    println!(
        "[变化] {changed}/{compared} 条查询的名次在 int8 下变了（0 表示完全一致）"
    );
    Ok(())
}

/// 自造大语料（≥2000 块）的召回对照：同一批块、同一组查询，f32 与 int8 各建一次。
///
/// 查询是**被查块自己的前 40 个字符**（含该篇唯一的序号标记）。这是「自查询」，
/// 只回答一个窄问题：**量化有没有改变排序**。它不证明 int8 在真实改写过的查询上
/// 不掉召回——那件事由上面的既有质量集回答，更大的真实语料留到有真实数据时再量。
fn run_recall_large(blocks: usize, options: &Options) -> Result<()> {
    let config = model_config()?;
    fs::create_dir_all(&options.dir)?;
    let db_path = options.dir.join("library.sqlite");
    println!("===========================================");
    println!("int8 召回对照 · 自造语料 {blocks} 块");
    println!("  目录：{}", options.dir.display());
    seed_library(&db_path, blocks)?;

    let mut core = Core::open(&db_path)?;
    let embedder =
        load_embedder(&config, EmbedOptions::default()).map_err(|reason| anyhow::anyhow!(reason))?;
    core.set_embedder(Box::new(embedder));

    let queries = query_indices(blocks, 100);
    let (f32_recall, f32_ranks, f32_build, f32_storage) =
        measure_recall(&mut core, &db_path, &queries, VectorStorage::F32)?;
    let (int8_recall, int8_ranks, int8_build, int8_storage) =
        measure_recall(&mut core, &db_path, &queries, VectorStorage::Int8)?;

    let changed = f32_ranks
        .iter()
        .zip(&int8_ranks)
        .filter(|(left, right)| left != right)
        .count();
    println!("\n[召回]");
    println!("  {}", f32_recall.line("f32 "));
    println!("  {}", int8_recall.line("int8"));
    println!("[变化] {changed}/{} 条查询名次变化", queries.len());
    println!(
        "[构建] f32 {f32_build:.2} 秒 · int8 {int8_build:.2} 秒（同一批块各整代重算一次）"
    );
    println!(
        "[体积] f32 {:.1} MiB / {} B 每向量 · int8 {:.1} MiB / {} B 每向量",
        bytes_to_mib(f32_storage.vector_bytes as u64),
        f32_storage.avg_bytes,
        bytes_to_mib(int8_storage.vector_bytes as u64),
        int8_storage.avg_bytes
    );
    Ok(())
}

/// 用指定存储格式整代重建，再跑完整查询集；返回 (召回, 每条查询的名次, 构建秒数, 体积)。
fn measure_recall(
    core: &mut Core,
    db_path: &Path,
    queries: &[usize],
    storage: VectorStorage,
) -> Result<(Recall, Vec<Option<usize>>, f64, StorageTotals)> {
    core.set_vector_storage_for_test(storage);
    let started = Instant::now();
    core.build_semantic_index(None)?;
    let build_secs = started.elapsed().as_secs_f64();
    let totals = storage_totals(db_path)?;
    let mut recall = Recall::default();
    let mut ranks = Vec::with_capacity(queries.len());
    for &index in queries {
        let ranking = core.semantic_ranking(&query_text(index), &SearchFilters::default())?;
        let expected = capture_id(index);
        let rank = ranking
            .iter()
            .position(|(candidate, _)| candidate == &expected)
            .map(|position| position + 1);
        recall.add(rank);
        ranks.push(rank);
    }
    Ok((recall, ranks, build_secs, totals))
}

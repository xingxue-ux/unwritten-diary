//! B3c-1 检索质量集：把 `tests/quality/search_quality_set.json` 跑一遍，
//! **分开**报每一路的召回与延迟（这一片只有关键词那一路）。
//!
//! 为什么要有这个入口，而不是写一次性脚本：
//! - 「混合检索是不是真的更好」只能用同一份质量集、同一套口径反复量；一次性脚本
//!   的数字对不上、也没法复查；
//! - 这一片的关键词数字是后面语义与 RRF 的**基线**，基线必须由同一条命令产出来。
//!
//! 口径（写在这里，避免以后各自解释）：
//! - 一条 case 建一个**独立的内存库**：不同 case 的语料不能互相污染，否则某个
//!   查询可能命中另一个 case 的来源，召回数字就掺了别的东西；
//! - 排名按 `start_search` 返回的顺序，命中来源第一次出现的位置就是它的名次；
//! - `recall@k` 按**来源**算（同一条来源的多个块只算一个），`MRR` 用名次的倒数；
//! - `no_answer` 类不参与召回，单独报「不误报率」；
//! - 延迟是每 case 查询耗时的中位数，**不含建库与提取**（那部分单独报）。
//!
//! 结构性错误（JSON 非法、`expectedSourceIndex` 越界、`expectedContains` 不在期望
//! 来源里）直接让命令失败；关键词纯度这类**数据质量**问题只打警告——它们是「标注
//! 需要修」的信号，不是程序错误。

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Instant;

use anyhow::{bail, Context, Result};
use chrono::{NaiveDate, TimeZone, Utc};
use diary_core::{
    pack_pieces, Core, CreateDraftInput, ImportManifest, ImportOrigin, ImportRequest, Piece,
    SearchFilters, SearchMode, SearchRequest,
};
use serde::Deserialize;
use sha2::{Digest, Sha256};

/// 取这么多条结果来算召回。
const TOP_K: usize = 10;

#[derive(Deserialize)]
struct QualitySet {
    version: i64,
    #[allow(dead_code)]
    note: String,
    cases: Vec<Case>,
}

#[derive(Deserialize)]
struct Case {
    id: String,
    category: String,
    query: String,
    #[serde(rename = "expectedSourceIndex")]
    expected_source_index: i64,
    #[serde(rename = "expectedContains")]
    expected_contains: String,
    sources: Vec<Source>,
    filters: Filters,
}

#[derive(Deserialize)]
struct Source {
    name: String,
    #[serde(rename = "dayKey")]
    day_key: String,
    text: String,
}

#[derive(Deserialize)]
struct Filters {
    #[serde(rename = "fromDayKey")]
    from_day_key: Option<String>,
    #[serde(rename = "toDayKey")]
    to_day_key: Option<String>,
}

/// 分块统计：字符数分布 + 块正文（`DIARY_DUMP_CHUNKS` 设了就写文件，
/// 交给真实分词器核 token 数——核心不为了切块把分词器拉进来）。
#[derive(Default)]
struct ChunkStats {
    chars: Vec<usize>,
    texts: Vec<String>,
    /// 每块由几篇组成：>1 就是打包（同一天的多个短篇）。
    pieces_per_chunk: Vec<usize>,
}

struct CaseOutcome {
    category: String,
    /// 期望来源第一次出现的位置（1 起）；没命中是 None。
    hit_rank: Option<usize>,
    /// 这次查询一共命中了几个不同来源——用来看「一份长材料是不是占满了结果」。
    matched_sources: usize,
    /// 返回的命中总数（含同一来源的多个块）。
    hits: usize,
    elapsed_ms: f64,
    warnings: Vec<String>,
}

pub fn run() -> Result<()> {
    let path = quality_set_path();
    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("读不到质量集：{}", path.display()))?;
    let set: QualitySet = serde_json::from_str(&raw)
        .with_context(|| format!("质量集不是合法 JSON：{}", path.display()))?;
    if set.version != 1 {
        bail!("质量集版本是 {}，这个探针只认 1", set.version);
    }

    println!("检索质量集：{}", path.display());
    println!("===========================================");
    println!("共 {} 条 case（关键词模式，取前 {TOP_K} 条算召回）\n", set.cases.len());

    let mut outcomes: Vec<CaseOutcome> = Vec::new();
    let mut chunk_stats = ChunkStats::default();
    for case in &set.cases {
        outcomes.push(run_case(case, &mut chunk_stats)?);
    }

    report(&set, &outcomes)?;
    report_chunking(&chunk_stats);
    dump_chunks(&chunk_stats)?;
    Ok(())
}

fn run_case(case: &Case, chunk_stats: &mut ChunkStats) -> Result<CaseOutcome> {
    let mut warnings = Vec::new();

    // 结构性检查：标注本身必须自洽，否则数字没有意义。
    let expected_index = case.expected_source_index;
    if expected_index >= 0 {
        let index = expected_index as usize;
        if index >= case.sources.len() {
            bail!("case {}：expectedSourceIndex 越界", case.id);
        }
        if !case.sources[index].text.contains(&case.expected_contains) {
            bail!(
                "case {}：expectedContains 不在期望来源的正文里（标注错了）",
                case.id
            );
        }
    }

    // 每个 case 一个独立的内存库（本 case 的语料不与其他 case 混在一起）。
    let dir = std::env::temp_dir().join(format!("diary_quality_{}_{}", std::process::id(), case.id));
    if dir.exists() {
        std::fs::remove_dir_all(&dir).ok();
    }
    std::fs::create_dir_all(&dir)?;
    let outcome = (|| -> Result<CaseOutcome> {
        let mut core = Core::open_in_memory_at(&dir)?;
        let build_start = Instant::now();
        let mut source_ids = Vec::new();
        for (index, source) in case.sources.iter().enumerate() {
            let day = NaiveDate::parse_from_str(&source.day_key, "%Y-%m-%d")
                .with_context(|| format!("case {}：dayKey 不合法（{}）", case.id, source.day_key))?;
            // 12:00（东八区）= 04:00 UTC，避免跨日把 dayKey 算到别的天。
            let occurred = Utc.from_utc_datetime(
                &day.and_hms_opt(4, 0, 0)
                    .context("构造时间失败（不该发生）")?,
            );
            let capture = core.create_draft(CreateDraftInput {
                occurred_at: Some(occurred),
                time_zone: "Asia/Shanghai",
                utc_offset_minutes: 480,
                operation_id: &format!("{}-create-{index}", case.id),
            })?;
            let bytes = source.text.as_bytes();
            let ticket = core.prepare_import(ImportRequest {
                capture_id: &capture.id,
                display_name: &source.name,
                mime_hint: Some("text/plain"),
                size_hint: Some(bytes.len() as i64),
                origin: ImportOrigin::Picker,
                operation_id: &format!("{}-prepare-{index}", case.id),
            })?;
            std::fs::write(&ticket.staging_ticket, bytes)?;
            core.finish_import(
                &ticket.import_id,
                &ticket.staging_ticket,
                ImportManifest {
                    copied_bytes: bytes.len() as i64,
                    sha256: sha256_hex(bytes),
                    detected_mime: "text/plain".to_owned(),
                    original_name: source.name.clone(),
                },
            )?;
            let source_id = core
                .get_capture(&capture.id)?
                .ordered_source_ids
                .first()
                .cloned()
                .with_context(|| format!("case {}：导入之后应当有来源", case.id))?;
            core.extract_source(&source_id)?;
            source_ids.push(source_id);
        }

        // 分块统计（B3c-1 冻结的规则）：**按打包路径**跑——同一 case 的短篇可能被
        // 合进同一块，所以统计要按整份 case 的篇来算，不能逐来源算。
        // token 数另用真实分词器核（核心不拉分词器进来）。
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
            chunk_stats.chars.push(chunk.text.chars().count());
            chunk_stats.pieces_per_chunk.push(chunk.spans.len());
            chunk_stats.texts.push(chunk.text);
        }
        let _build_ms = build_start.elapsed().as_secs_f64() * 1000.0;

        let request = SearchRequest {
            query: case.query.clone(),
            mode: SearchMode::Keyword,
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

        // 名次：期望来源在结果里第一次出现的位置；同时数一共命中几个来源。
        let mut first_seen: BTreeMap<&str, usize> = BTreeMap::new();
        for (position, hit) in snapshot.results.iter().enumerate() {
            if let Some(source_id) = hit.source_id.as_deref() {
                first_seen.entry(source_id).or_insert(position + 1);
            }
        }
        let hit_rank = if expected_index >= 0 {
            first_seen.get(source_ids[expected_index as usize].as_str()).copied()
        } else {
            None
        };

        // 数据质量：关键词类 case 里如果有多于一个来源命中，说明标注被污染了。
        if case.category == "keyword_only" && first_seen.len() > 1 {
            warnings.push(format!(
                "keyword_only 却有 {} 个来源命中（标注可能被污染）",
                first_seen.len()
            ));
        }
        if case.category == "semantic_only" && hit_rank.is_some() {
            warnings.push(
                "semantic_only 却被关键词命中（查询与来源撞了词，标注需要改）".to_owned(),
            );
        }
        if case.category == "no_answer" && !snapshot.results.is_empty() {
            warnings.push(format!("no_answer 却返回了 {} 条命中", snapshot.results.len()));
        }

        Ok(CaseOutcome {
            category: case.category.clone(),
            hit_rank,
            matched_sources: first_seen.len(),
            hits: snapshot.results.len(),
            elapsed_ms,
            warnings,
        })
    })();

    std::fs::remove_dir_all(&dir).ok();
    outcome
}

fn report(set: &QualitySet, outcomes: &[CaseOutcome]) -> Result<()> {
    // 按类别聚合：每类各报各的，避免「平均一下看不出问题」。
    let mut by_category: BTreeMap<&str, Vec<&CaseOutcome>> = BTreeMap::new();
    for outcome in outcomes {
        by_category
            .entry(outcome.category.as_str())
            .or_default()
            .push(outcome);
    }

    println!("按类别（召回按来源算，命中同一来源的多个块只算一次）");
    println!(
        "  {:<15} {:>5} {:>8} {:>8} {:>8} {:>7} {:>10}",
        "类别", "条数", "R@1", "R@5", "R@10", "MRR", "中位延迟"
    );
    for (category, group) in &by_category {
        let cases = group.len() as f64;
        let rate = |k: usize| -> f64 {
            let hit = group
                .iter()
                .filter(|outcome| outcome.hit_rank.is_some_and(|rank| rank <= k))
                .count() as f64;
            hit / cases * 100.0
        };
        let hits_found = group.iter().filter(|o| o.hit_rank.is_some()).count();
        let mrr = if hits_found == 0 {
            "—".to_owned()
        } else {
            let value: f64 = group
                .iter()
                .filter_map(|outcome| outcome.hit_rank)
                .map(|rank| 1.0 / rank as f64)
                .sum::<f64>()
                / cases;
            format!("{value:.3}")
        };
        let median = median_ms(group.iter().map(|outcome| outcome.elapsed_ms).collect());
        // 不误报类没有「召回」可言，印成 — 比印 0% 更不容易被误读。
        let (r1, r5, r10) = if *category == "no_answer" {
            ("—".to_owned(), "—".to_owned(), "—".to_owned())
        } else {
            (
                format!("{:.0}%", rate(1)),
                format!("{:.0}%", rate(5)),
                format!("{:.0}%", rate(10)),
            )
        };
        println!(
            "  {:<15} {:>5} {:>8} {:>8} {:>8} {:>7} {:>7.1} ms",
            category,
            group.len(),
            r1,
            r5,
            r10,
            mrr,
            median
        );
    }

    // 全部 case（no_answer 不算召回，它们只报不误报）。
    let answerable: Vec<&CaseOutcome> = outcomes
        .iter()
        .filter(|outcome| outcome.category != "no_answer")
        .collect();
    if !answerable.is_empty() {
        let count = answerable.len() as f64;
        let r1 = answerable
            .iter()
            .filter(|o| o.hit_rank.is_some_and(|rank| rank <= 1))
            .count() as f64
            / count;
        let r10 = answerable
            .iter()
            .filter(|o| o.hit_rank.is_some_and(|rank| rank <= TOP_K))
            .count() as f64
            / count;
        let mrr: f64 = answerable
            .iter()
            .filter_map(|o| o.hit_rank)
            .map(|rank| 1.0 / rank as f64)
            .sum::<f64>()
            / count;
        println!(
            "\n关键词一路合计：R@1 {:.0}% · R@{TOP_K} {:.0}% · MRR {:.3}（{} 条可回答 case）",
            r1 * 100.0,
            r10 * 100.0,
            mrr,
            answerable.len()
        );
    }

    let no_answer: Vec<&CaseOutcome> = outcomes
        .iter()
        .filter(|outcome| outcome.category == "no_answer")
        .collect();
    if !no_answer.is_empty() {
        let clean = no_answer.iter().filter(|o| o.hits == 0).count();
        println!(
            "不误报（no_answer 类）：{}/{} 条返回空结果",
            clean,
            no_answer.len()
        );
    }

    // 命中来源数：长材料是否占满结果，用这个看。
    let max_sources = outcomes.iter().map(|o| o.matched_sources).max().unwrap_or(0);
    let avg_sources: f64 =
        outcomes.iter().map(|o| o.matched_sources as f64).sum::<f64>() / outcomes.len() as f64;
    println!(
        "命中广度：平均每条 case 命中 {avg_sources:.1} 个来源，最多 {max_sources} 个（越接近 1 越像「一份材料占满结果」）"
    );

    // 数据质量警告：汇总，不逐条刷屏。
    let warnings: Vec<String> = outcomes
        .iter()
        .enumerate()
        .flat_map(|(index, outcome)| {
            outcome
                .warnings
                .iter()
                .map(move |warning| format!("  case {}（{}）：{warning}", set.cases[index].id, outcome.category))
        })
        .collect();
    if warnings.is_empty() {
        println!("\n数据自检：没有发现问题");
    } else {
        println!("\n数据自检（这些是标注要修，不是程序错误）：");
        for warning in warnings {
            println!("{warning}");
        }
    }
    Ok(())
}

fn report_chunking(stats: &ChunkStats) {
    if stats.chars.is_empty() {
        return;
    }
    let mut sorted = stats.chars.clone();
    sorted.sort_unstable();
    let total: usize = sorted.iter().sum();
    let max = *sorted.last().expect("非空");
    let p99 = sorted[((sorted.len() as f64 * 0.99) as usize).min(sorted.len() - 1)];
    println!(
        "\n分块（chunkerVersion {}）：{} 块 · 最长 {max} 字 · p99 {p99} 字 · 平均 {:.0} 字",
        diary_core::CHUNKER_VERSION,
        sorted.len(),
        total as f64 / sorted.len() as f64
    );
    let multi = stats.pieces_per_chunk.iter().filter(|count| **count > 1).count();
    let most = stats.pieces_per_chunk.iter().copied().max().unwrap_or(0);
    println!(
        "  合块（同一天的多个短篇进一块）：{multi} 块含多篇，最多 {most} 篇；其余是单篇"
    );
    println!("  token 数要用真实分词器核（见 docs/architecture/m2-质量集与分块.md）");
}

/// 把块正文写到 `DIARY_DUMP_CHUNKS` 指向的文件（JSON 数组），供真实分词器核对
/// token 数。不设这个变量就什么都不做。
fn dump_chunks(stats: &ChunkStats) -> Result<()> {
    let Ok(path) = std::env::var("DIARY_DUMP_CHUNKS") else {
        return Ok(());
    };
    std::fs::write(&path, serde_json::to_string(&stats.texts)?)
        .with_context(|| format!("写不出分块 dump：{path}"))?;
    println!("  已把 {} 块正文写到 {path}（供真实分词器核 token）", stats.texts.len());
    Ok(())
}

fn median_ms(mut values: Vec<f64>) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.sort_by(|a, b| a.partial_cmp(b).expect("耗时不是 NaN"));
    values[values.len() / 2]
}

fn quality_set_path() -> PathBuf {
    if let Ok(path) = std::env::var("DIARY_QUALITY_SET") {
        return PathBuf::from(path);
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/quality/search_quality_set.json")
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

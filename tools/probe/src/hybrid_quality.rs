//! B3c-3（#51）混合检索的量测：三类对照、`semantic_only` 逐条、`no_answer`、
//! `matchedBy` 分布、以及**语义证据门槛的扫描表**。
//!
//! 与 `semantic_quality` 一样走**产品路径**：真核心 `build_semantic_index` +
//! `search.start(mode = keyword / semantic)`、`search.start(mode = hybrid)` +
//! `Core::upgrade_search_to_hybrid`（混合是两步，量的是**融合后**的结果，#69），
//! 同一份质量集、同一套口径（按来源算召回、同一来源的多个块折叠成一条、
//! 延迟不含建库与提取）。
//!
//! 阈值扫描**不另写一套融合**：它在同一条产品路径上改
//! `Core::set_hybrid_min_score_for_test`，所以量到的曲线就是线上那条曲线。
//!
//! ```bash
//! DIARY_MODEL_DIR="$PWD/models/bge-small-zh-v1.5-ours" \
//! DIARY_ORT_DYLIB="$HOME/dev/onnxruntime-linux-x64-1.22.0/lib/libonnxruntime.so" \
//! cargo run --release -p diary_probe -- hybrid-quality
//! ```
//!
//! 一条 case 只建一次库、只建一次语义索引，然后在同一个库上跑三种模式与整条阈值
//! 扫描——否则「阈值 × 36 条」会把建索引的代价重复付 16 遍。

use std::collections::BTreeMap;
use std::time::Instant;

use anyhow::{bail, Result};
use diary_core::{
    config_from_env, load_embedder, Core, EmbedOptions, EnvEmbedding, SearchFilters, SearchMode,
    SearchRequest,
};

use crate::quality::{self, Case, CaseOutcome, QualitySet, TOP_K};

/// 阈值扫描：0.20–0.50，步进 0.02（16 个点）。
const SCAN_FROM: f32 = 0.20;
const SCAN_TO: f32 = 0.50;
const SCAN_STEP: f32 = 0.02;

/// 一次查询的结果：召回名次（按来源）+ `matchedBy` 分布 + 语义分数（定阈值用）。
struct Run {
    outcome: CaseOutcome,
    keyword: usize,
    semantic: usize,
    both: usize,
    /// 这一条查询语义一路的最高分（没有候选时为 None）。
    top_score: Option<f32>,
    /// 期望来源在语义一路的分数（期望来源没进候选时为 None）。
    expected_score: Option<f32>,
    /// 关键词首屏（`start_search`）耗时；#69 之后混合的第一阶段。
    start_ms: f64,
    /// 升级（`upgrade_search_to_hybrid`）耗时；非混合模式恒为 0。
    upgrade_ms: f64,
}

pub fn run() -> Result<()> {
    let set = quality::load()?;
    let config = match config_from_env() {
        EnvEmbedding::Ready(config) => config,
        EnvEmbedding::Unset => bail!(
            "需要 DIARY_MODEL_DIR（模型目录）与 DIARY_ORT_DYLIB（ORT 动态库）；见 README/CONTRIBUTING"
        ),
        EnvEmbedding::Invalid(reason) => bail!("{reason}"),
    };
    config
        .check_files()
        .map_err(|reason| anyhow::anyhow!(reason))?;

    println!("===========================================");
    println!("混合检索量测（真模型）");
    println!("  模型目录：{}", config.model_dir.display());
    println!(
        "  融合口径：{}（RRF k=60，两路各取前 50 篇，语义门槛可调）",
        diary_core::HYBRID_FUSION_VERSION
    );
    println!(
        "  产品默认语义门槛：{:.2}  候选深度：50  k：60",
        diary_core::HYBRID_SEMANTIC_MIN_SCORE
    );
    println!(
        "  阶段（#69）：start_search 先给 keyword_ready（只关键词那一路），\
         upgrade_search_to_hybrid 再给 hybrid_ready（融合 + 重排后，量的是这一份）"
    );

    let thresholds = scan_thresholds();
    let mut keyword_runs: Vec<Run> = Vec::new();
    let mut semantic_runs: Vec<Run> = Vec::new();
    let mut hybrid_runs: Vec<Run> = Vec::new();
    // 阈值 → 每类 outcome（只跑混合）。
    let mut scan_runs: Vec<(f32, Vec<Run>)> = thresholds.iter().map(|t| (*t, Vec::new())).collect();

    for case in &set.cases {
        check_case(case)?;
        let (mut core, source_ids, dir) = quality::seed_case(case, false)?;
        let outcome = (|| -> Result<()> {
            let embedder = load_embedder(&config, EmbedOptions::default())
                .map_err(|reason| anyhow::anyhow!(reason))?;
            core.set_embedder(Box::new(embedder));
            core.build_semantic_index(None)?;

            // 语义一路的分数分布：阈值扫描表的读法要用它（期望来源多少分、
            // 这一条查询最高多少分）。与产品路径共用 `semantic::candidates`。
            let ranking = core.semantic_ranking(&case.query, &filter_of(case))?;
            let scores = Scores::of(&ranking, case, &source_ids);

            keyword_runs.push(run_case(&mut core, case, &source_ids, SearchMode::Keyword, &scores)?);
            semantic_runs.push(run_case(&mut core, case, &source_ids, SearchMode::Semantic, &scores)?);
            hybrid_runs.push(run_case(&mut core, case, &source_ids, SearchMode::Hybrid, &scores)?);
            for (threshold, runs) in scan_runs.iter_mut() {
                core.set_hybrid_min_score_for_test(*threshold);
                runs.push(run_case(&mut core, case, &source_ids, SearchMode::Hybrid, &scores)?);
            }
            Ok(())
        })();
        std::fs::remove_dir_all(&dir).ok();
        outcome?;
    }

    println!("\n[1] 三类对照（同一份质量集、同一套口径；混合用产品默认门槛 {:.2}）", diary_core::HYBRID_SEMANTIC_MIN_SCORE);
    quality::report(&set, &outcomes_of(&keyword_runs), "关键词")?;
    quality::report(&set, &outcomes_of(&semantic_runs), "语义")?;
    quality::report(&set, &outcomes_of(&hybrid_runs), "混合")?;

    report_semantic_only(&set, &keyword_runs, &semantic_runs, &hybrid_runs);
    report_no_answer(&set, &keyword_runs, &semantic_runs, &hybrid_runs)?;
    report_matched_by(&set, &keyword_runs, &semantic_runs, &hybrid_runs);
    report_scan(&set, &scan_runs);
    report_latency(&hybrid_runs);
    Ok(())
}

/// 阈值扫描点：0.20 起、0.50 止、步进 0.02。
fn scan_thresholds() -> Vec<f32> {
    let mut values = Vec::new();
    let mut value = SCAN_FROM;
    while value <= SCAN_TO + 1e-6 {
        // 保留两位小数，免得 0.30000001 这种浮点尾巴进到表格里。
        values.push((value * 100.0).round() / 100.0);
        value += SCAN_STEP;
    }
    values
}

/// 结构性检查：`expectedSourceIndex` 不能越界（`expectedContains` 是私有的，
/// 那一条由 `quality-set` 那趟把关）。
fn check_case(case: &Case) -> Result<()> {
    if case.expected_source_index >= 0
        && case.expected_source_index as usize >= case.sources.len()
    {
        bail!("case {}：expectedSourceIndex 越界", case.id);
    }
    Ok(())
}

fn filter_of(case: &Case) -> SearchFilters {
    SearchFilters {
        from_day_key: case.filters.from_day_key.clone(),
        to_day_key: case.filters.to_day_key.clone(),
        ..Default::default()
    }
}

/// 一条 case 的语义分数：最高分与期望来源的分数。
struct Scores {
    top: Option<f32>,
    expected: Option<f32>,
}

impl Scores {
    fn of(ranking: &[(String, f32)], case: &Case, source_ids: &[String]) -> Self {
        let expected = if case.expected_source_index >= 0 {
            let wanted = &source_ids[case.expected_source_index as usize];
            ranking
                .iter()
                .find(|(group_id, _)| group_id == wanted)
                .map(|(_, score)| *score)
        } else {
            None
        };
        Self {
            top: ranking.first().map(|(_, score)| *score),
            expected,
        }
    }
}

/// 跑一条 case 的一次查询：名次按 `group_id`（与语义那一路同一个口径）。
///
/// 混合（#69）走**两步**：`start_search` 先给关键词首屏，`upgrade_search_to_hybrid`
/// 再融合；量测读的是**融合后**的那份快照（否则量的就不是混合了），同时把两步的
/// 耗时都记下来。
fn run_case(
    core: &mut Core,
    case: &Case,
    source_ids: &[String],
    mode: SearchMode,
    scores: &Scores,
) -> Result<Run> {
    let request = SearchRequest {
        query: case.query.clone(),
        mode,
        filters: filter_of(case),
        page_size: TOP_K as i64,
    };
    let start = Instant::now();
    let mut snapshot = core.start_search(request, 1)?;
    let start_ms = start.elapsed().as_secs_f64() * 1000.0;
    let mut upgrade_ms = 0.0;
    if mode == SearchMode::Hybrid {
        let upgrade = Instant::now();
        snapshot = core.upgrade_search_to_hybrid(&snapshot.session_id)?;
        upgrade_ms = upgrade.elapsed().as_secs_f64() * 1000.0;
    }

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

    let mut keyword = 0;
    let mut semantic = 0;
    let mut both = 0;
    for hit in &snapshot.results {
        match hit.matched_by.as_slice() {
            [diary_core::MatchedBy::Keyword] => keyword += 1,
            [diary_core::MatchedBy::Semantic] => semantic += 1,
            [diary_core::MatchedBy::Keyword, diary_core::MatchedBy::Semantic] => both += 1,
            _ => {}
        }
    }

    Ok(Run {
        outcome: CaseOutcome {
            category: case.category.clone(),
            hit_rank,
            matched_sources: first_seen.len(),
            hits: snapshot.results.len(),
            // 混合的「一条查询总延迟」= 关键词首屏 + 升级；其余模式就是 start 那一次。
            elapsed_ms: start_ms + upgrade_ms,
            warnings: snapshot.warnings.clone(),
        },
        keyword,
        semantic,
        both,
        top_score: scores.top,
        expected_score: scores.expected,
        start_ms,
        upgrade_ms,
    })
}

fn outcomes_of(runs: &[Run]) -> Vec<CaseOutcome> {
    runs.iter()
        .map(|run| CaseOutcome {
            category: run.outcome.category.clone(),
            hit_rank: run.outcome.hit_rank,
            matched_sources: run.outcome.matched_sources,
            hits: run.outcome.hits,
            elapsed_ms: run.outcome.elapsed_ms,
            warnings: run.outcome.warnings.clone(),
        })
        .collect()
}

/// `semantic_only` 6 条：关键词基线 / 语义 / 混合逐条对比。
fn report_semantic_only(set: &QualitySet, keyword: &[Run], semantic: &[Run], hybrid: &[Run]) {
    println!("\n[2] semantic_only 逐条（关键词基线 vs 语义 vs 混合，名次是期望来源的名次）");
    for (index, run) in hybrid.iter().enumerate() {
        let case = &set.cases[index];
        if case.category != "semantic_only" {
            continue;
        }
        let _ = run;
        println!(
            "  {} 查询「{}」 → 关键词 {} · 语义 {} · 混合 {} · matchedBy [keyword {} / semantic {} / 两者 {}]",
            case.id,
            case.query,
            rank_text(keyword[index].outcome.hit_rank),
            rank_text(semantic[index].outcome.hit_rank),
            rank_text(hybrid[index].outcome.hit_rank),
            hybrid[index].keyword,
            hybrid[index].semantic,
            hybrid[index].both,
        );
        println!(
            "      语义分数：期望来源 {} · 这一条最高 {}（阈值 {:.2}）",
            score_text(hybrid[index].expected_score),
            score_text(hybrid[index].top_score),
            diary_core::HYBRID_SEMANTIC_MIN_SCORE,
        );
    }
}

fn rank_text(rank: Option<usize>) -> String {
    match rank {
        Some(value) => format!("R@{value}"),
        None => "未命中".to_owned(),
    }
}

fn score_text(score: Option<f32>) -> String {
    match score {
        Some(value) => format!("{value:.3}"),
        None => "—".to_owned(),
    }
}

/// `no_answer` 4 条：三种模式各返回几条，混合必须 4/4 空。
fn report_no_answer(
    set: &QualitySet,
    keyword: &[Run],
    semantic: &[Run],
    hybrid: &[Run],
) -> Result<()> {
    println!("\n[3] no_answer：三种模式各返回几条（混合必须 4/4 空）");
    let mut empty = 0;
    let mut total = 0;
    for (index, case) in set.cases.iter().enumerate() {
        if case.category != "no_answer" {
            continue;
        }
        total += 1;
        if hybrid[index].outcome.hits == 0 {
            empty += 1;
        }
        println!(
            "  {} 查询「{}」 → 关键词 {} 条 · 语义 {} 条 · 混合 {} 条（阈值 {:.2}）；语义最高分 {}",
            case.id,
            case.query,
            keyword[index].outcome.hits,
            semantic[index].outcome.hits,
            hybrid[index].outcome.hits,
            diary_core::HYBRID_SEMANTIC_MIN_SCORE,
            score_text(hybrid[index].top_score),
        );
        if let Some(warning) = hybrid[index]
            .outcome
            .warnings
            .iter()
            .find(|warning| warning.contains("低于证据门槛"))
        {
            println!("      {warning}");
        }
    }
    println!("  合计：混合 {empty}/{total} 空");
    if empty != total {
        println!("  **不达标**：`no_answer` 是硬门槛，阈值要往上调（见 [4] 的扫描表）");
    }
    Ok(())
}

/// `matchedBy` 分布：哪几路真的贡献了这条命中。
fn report_matched_by(set: &QualitySet, keyword: &[Run], semantic: &[Run], hybrid: &[Run]) {
    println!("\n[4] matchedBy 分布（按类别，混合模式）");
    println!(
        "  {:<15} {:>8} {:>10} {:>10} {:>10}",
        "类别", "命中条数", "[keyword]", "[semantic]", "两者都有"
    );
    let mut by_category: BTreeMap<&str, (usize, usize, usize, usize)> = BTreeMap::new();
    for (index, case) in set.cases.iter().enumerate() {
        let entry = by_category
            .entry(case.category.as_str())
            .or_insert((0, 0, 0, 0));
        entry.0 += hybrid[index].outcome.hits;
        entry.1 += hybrid[index].keyword;
        entry.2 += hybrid[index].semantic;
        entry.3 += hybrid[index].both;
    }
    let mut total = (0, 0, 0, 0);
    for (category, (hits, keyword, semantic, both)) in &by_category {
        println!(
            "  {category:<15} {hits:>8} {keyword:>10} {semantic:>10} {both:>10}"
        );
        total.0 += hits;
        total.1 += keyword;
        total.2 += semantic;
        total.3 += both;
    }
    println!(
        "  {:<15} {:>8} {:>10} {:>10} {:>10}",
        "合计", total.0, total.1, total.2, total.3
    );

    // 关键词/语义两趟的 matchedBy 是写死的，顺带核一下产品路径没走偏。
    let keyword_only = keyword.iter().all(|run| run.semantic == 0 && run.both == 0);
    let semantic_only = semantic
        .iter()
        .all(|run| run.keyword == 0 && run.both == 0);
    println!(
        "  对照：关键词模式全是 [keyword]：{} · 语义模式全是 [semantic]：{}",
        if keyword_only { "是" } else { "否" },
        if semantic_only { "是" } else { "否" }
    );
}

/// 阈值扫描表：`阈值 × no_answer 空结果数 × semantic_only 命中数 × 关键词类是否变差`。
fn report_scan(set: &QualitySet, scan: &[(f32, Vec<Run>)]) {
    println!("\n[5] 阈值扫描（0.20–0.50 步进 0.02；每行 = 一个阈值下 36 条各跑一次混合）");
    println!(
        "  {:<7} {:>14} {:>16} {:>16} {:>13} {:>12} {:>15} {:>14}",
        "阈值", "no_answer 空", "semantic_only R@1", "semantic_only R@5", "keyword_only",
        "time_filter", "long_transcript", "contradiction"
    );
    for (threshold, runs) in scan {
        let no_answer = category_rate(runs, set, "no_answer", true);
        let semantic_r1 = category_rate(runs, set, "semantic_only", false);
        let semantic_r5 = category_rate_at(runs, set, "semantic_only", 5);
        println!(
            "  {threshold:<7.2} {:>14} {:>16} {:>16} {:>13} {:>12} {:>15} {:>14}",
            format!("{}/{}", no_answer.0, no_answer.1),
            format!("{}/{}", semantic_r1.0, semantic_r1.1),
            format!("{}/{}", semantic_r5.0, semantic_r5.1),
            r1_text(runs, set, "keyword_only"),
            r1_text(runs, set, "time_filter"),
            r1_text(runs, set, "long_transcript"),
            r1_text(runs, set, "contradiction"),
        );
    }
    println!(
        "  读法：`no_answer 空`这一列是硬门槛（必须 4/4），`semantic_only R@1` 是这一片要恢复的缺口；\n\
         关键词那几列是「不许变差」的底线。阈值往上调会同时压低两边——取舍写进文档。"
    );
}

/// 某一类里「空结果」或「R@1 命中」的条数。
fn category_rate(runs: &[Run], set: &QualitySet, category: &str, want_empty: bool) -> (usize, usize) {
    let mut hit = 0;
    let mut total = 0;
    for (index, case) in set.cases.iter().enumerate() {
        if case.category != category {
            continue;
        }
        total += 1;
        let ok = if want_empty {
            runs[index].outcome.hits == 0
        } else {
            runs[index]
                .outcome
                .hit_rank
                .is_some_and(|rank| rank <= 1)
        };
        if ok {
            hit += 1;
        }
    }
    (hit, total)
}

/// 某一类里期望来源落在前 `k` 名之内的条数。
fn category_rate_at(runs: &[Run], set: &QualitySet, category: &str, k: usize) -> (usize, usize) {
    let mut hit = 0;
    let mut total = 0;
    for (index, case) in set.cases.iter().enumerate() {
        if case.category != category {
            continue;
        }
        total += 1;
        if runs[index]
            .outcome
            .hit_rank
            .is_some_and(|rank| rank <= k)
        {
            hit += 1;
        }
    }
    (hit, total)
}

fn r1_text(runs: &[Run], set: &QualitySet, category: &str) -> String {
    let (hit, total) = category_rate(runs, set, category, false);
    format!("{hit}/{total}")
}

/// 渐进式（#69）的延迟：关键词首屏（`start_search`）与升级（`upgrade_search_to_hybrid`）
/// 各多少毫秒。36 条查询各跑一次，取中位 / 均值 / p95 / 最大。
fn report_latency(runs: &[Run]) {
    println!("\n[6] 渐进式延迟（36 条查询各一次；产品的两步：#69）");
    println!(
        "  {:<24} {:>10} {:>10} {:>10} {:>10}",
        "", "中位", "均值", "p95", "最大"
    );
    let start: Vec<f64> = runs.iter().map(|run| run.start_ms).collect();
    let upgrade: Vec<f64> = runs.iter().map(|run| run.upgrade_ms).collect();
    let total: Vec<f64> = runs
        .iter()
        .map(|run| run.start_ms + run.upgrade_ms)
        .collect();
    println!("  {:<24} {}", "关键词首屏 start_search", stats_text(&start));
    println!("  {:<24} {}", "升级 upgrade_search", stats_text(&upgrade));
    println!("  {:<24} {}", "合计（首屏 + 升级）", stats_text(&total));
    println!(
        "  读法：这里量的是**嵌入器已注入**（模型已加载）后的路径——升级那一步是\n\
         \x20        查询向量 + 全表余弦 + RRF 合并；产品里如果模型还没加载，\n\
         \x20        升级会额外付一次模型加载（首屏刻意不付这笔），探针量不到它。"
    );
}

fn stats_text(values: &[f64]) -> String {
    format!(
        "{:>10.2} {:>10.2} {:>10.2} {:>10.2}",
        percentile(values, 50.0),
        mean(values),
        percentile(values, 95.0),
        values.iter().copied().fold(0.0_f64, f64::max),
    )
}

fn mean(values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.iter().sum::<f64>() / values.len() as f64
}

/// 最近秩法取分位：`ceil(p/100 × n)` 名（1 起），够用且不插值。
fn percentile(values: &[f64], p: f64) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(|left, right| left.partial_cmp(right).unwrap_or(std::cmp::Ordering::Equal));
    let index = ((p / 100.0) * sorted.len() as f64).ceil() as usize;
    sorted[index.clamp(1, sorted.len()) - 1]
}

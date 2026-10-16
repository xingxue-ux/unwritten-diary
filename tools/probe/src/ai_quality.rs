//! B4b（issue #93 前半）AI 质量集的评测入口：`ai-quality` / `ai-quality-set`。
//!
//! # 两种模式，差别是**实质性的**，不是开关
//!
//! | | `--replay`（**默认**） | `--live`（手动） |
//! |---|---|---|
//! | 联网 | **不联网**（回放引擎不碰 Transport） | 真的发请求，可能计费 |
//! | 生成输入 | 质量集里人工写的**参考输出** | 模型真产出的文本 |
//! | 转写输入 | 质量集里注入的**假转写结果** | 模型对**占位静音音频**的产出 |
//! | 数字说明什么 | **评测本身可复现**（判定器认得出合格输出、也算得出不合格输出） | 这才是**质量**数字 |
//! | 进 CI | 是 | 否（要显式给 Provider 配置） |
//!
//! **回放不是质量基线**：它量的是「判定器 + 报表 + 门禁这条管线通不通」，
//! 不是「模型好不好」。报表与文档里都这么写，避免以后有人把回放的绿当成质量。
//!
//! # 用法
//!
//! ```bash
//! # 回放（默认；CI 就跑这条，不联网）
//! cargo run -p diary_probe -- ai-quality
//! cargo run -p diary_probe -- ai-quality --json tests/quality/ai-quality-baseline.json
//!
//! # 真模型（手动；需要显式给配置，缺一个就报错退出，绝不假装跑过）
//! DIARY_AI_ENDPOINT=http://127.0.0.1:11434/v1 DIARY_AI_MODEL=qwen2.5:7b \
//!   cargo run --release -p diary_probe -- ai-quality --live
//! ```
//!
//! # 待接的地方（B4b / #91 落地后改这里）
//!
//! 本文件里 `engine_for` 与 `transcript_engine_for` 两处决定「用哪个引擎」。
//! B4b（#64）落地后，把生成侧换成调用它核心方法的实现；#91 落地后同样换转写侧。
//! **判定器、质量集、报表、门禁都不用动**——判定只看输出文本。

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Instant;

use anyhow::{bail, Context, Result};
// 用 `core_quality` 别名：本 crate 里已经有一个同名的 `quality` 模块（检索质量集）。
// 直接写 `quality::` 也能编过，但读的人要停下来分清是哪一个。
use diary_core::quality as core_quality;
use diary_core::quality::{
    cer, judge_generation, judge_injection, GenerateEngine, GenerationCase, GenerationSet,
    GenerationVerdict, InjectionCase, InjectionSet, InjectionVerdict, Material, RuleOutcome,
    TemplateSpec, TranscriptCase, TranscriptEngine, TranscriptSet,
};

/// 命令行开关。
struct Options {
    /// 真模型模式（默认是回放）。
    live: bool,
    /// 只跑「生成 + 注入」，跳过转写（快速回归用）。
    generation_only: bool,
    /// 基线 JSON 输出路径（`None` = 只印报表）。
    json: Option<PathBuf>,
    /// 质量集目录覆盖（默认仓库里的 `tests/quality`）。
    quality_dir: Option<PathBuf>,
}

fn parse_options(args: &[String]) -> Result<Options> {
    let mut options = Options {
        live: false,
        generation_only: false,
        json: None,
        quality_dir: None,
    };
    let mut index = 0usize;
    while index < args.len() {
        match args[index].as_str() {
            "--live" => options.live = true,
            "--replay" => options.live = false,
            "--generation-only" => options.generation_only = true,
            "--json" => {
                index += 1;
                let path = args.get(index).context("--json 后面要给一个文件路径")?;
                options.json = Some(PathBuf::from(path));
            }
            "--quality-dir" => {
                index += 1;
                let path = args.get(index).context("--quality-dir 后面要给一个目录")?;
                options.quality_dir = Some(PathBuf::from(path));
            }
            other => bail!(
                "不认识这个参数：{other}\n用法：ai-quality [--replay|--live] [--generation-only] \
                 [--json 文件] [--quality-dir 目录]"
            ),
        }
        index += 1;
    }
    Ok(options)
}

pub fn run(args: &[String]) -> Result<()> {
    let options = parse_options(args)?;
    let dir = options
        .quality_dir
        .clone()
        .unwrap_or_else(core_quality::quality_dir);

    let started = Instant::now();
    println!("===========================================");
    println!("AI 质量集（issue #93 前半）");
    println!("  质量集目录：{}", dir.display());
    println!(
        "  模式：{}",
        if options.live {
            "**真模型**（会真的发请求，可能计费）"
        } else {
            "回放（默认；不联网。**回放不是质量基线**，它证明的是评测本身可复现）"
        }
    );

    let generation = GenerationSet::load(&dir.join("ai-quality-set.json"))?;
    let injection = InjectionSet::load(&dir.join("ai-injection-set.json"))?;
    let transcripts = if options.generation_only {
        None
    } else {
        Some(TranscriptSet::load(&dir.join("ai-transcript-set.json"))?)
    };

    // ---- 待接点 #1 / #2：这里决定用哪个引擎（回放 or 真模型 / B4b / #91） ----
    let generate_engine = engine_for(&options)?;
    let transcript_engine = transcript_engine_for(&options)?;
    println!("  生成引擎：{}", generate_engine.name());
    if let Some(engine) = &transcript_engine {
        println!("  转写引擎：{}", engine.name());
    }

    let mut results: Vec<CaseResult> = Vec::new();

    // ---------------- 生成质量 ----------------
    println!("\n【生成质量】{} 条", generation.cases.len());
    for case in &generation.cases {
        let output = generate_engine
            .generate(&build_request(&case.id, &case.material, &case.template))
            .with_context(|| format!("用例 {} 的生成调用失败", case.id))?;
        let verdict = judge_generation(case, &output.text);
        results.push(generation_result(case, &verdict));
    }

    // ---------------- 防提示注入 ----------------
    println!("\n【防提示注入】{} 条", injection.cases.len());
    for case in &injection.cases {
        let clean = run_generation(&*generate_engine, &format!("{}#clean", case.id), &case.clean, &case.template)?;
        let poisoned = run_generation(
            &*generate_engine,
            &format!("{}#poisoned", case.id),
            &case.poisoned,
            &case.template,
        )?;
        let verdict = judge_injection(case, &injection, &clean, &poisoned);
        results.push(injection_result(case, &verdict));
    }

    // ---------------- 转写 CER ----------------
    if let (Some(transcripts), Some(engine)) = (&transcripts, &transcript_engine) {
        println!("\n【转写 CER】{} 条", transcripts.cases.len());
        println!(
            "  音频：{}（**合成占位静音**，不是真实语音语料）",
            transcripts
                .cases
                .first()
                .map(|case| case.audio.kind.as_str())
                .unwrap_or("—")
        );
        for case in &transcripts.cases {
            let audio = core_quality::placeholder_audio(case);
            let outcome = engine
                .transcribe(&core_quality::TranscriptRequest {
                    case_id: case.id.clone(),
                    audio,
                    sample_rate: case.audio.sample_rate,
                    channels: case.audio.channels,
                    language: Some("zh".to_owned()),
                })
                .with_context(|| format!("用例 {} 的转写调用失败", case.id))?;
            results.push(transcript_result(case, &outcome.text, !engine.is_replay()));
        }
    }

    report(&results, &options)?;
    println!("\n总耗时 {:.0} ms", started.elapsed().as_secs_f64() * 1000.0);
    Ok(())
}

/// **待接点 #1**：生成引擎的选择。
///
/// - `--replay`（默认）：[`diary_core::quality::ScriptedGenerateEngine`]——回放质量集里人工写的
///   参考输出，**不联网**；
/// - `--live`：[`diary_core::quality::OpenAiCompatibleGenerateEngine`]——走 B4a 的通用 Provider
///   路径，**真的发请求**，但**不含 B4b 的提示词与上下文装配**。
///
/// B4b（#64）落地后：在这里加一个「调用 B4b 核心方法」的实现并在 `--live` 分支用它。
/// 调用点就是下面 `generate_engine.generate(...)` 那一处。
fn engine_for(options: &Options) -> Result<Box<dyn GenerateEngine>> {
    if options.live {
        let engine = core_quality::OpenAiCompatibleGenerateEngine::from_env()
            .context("真模型模式需要 DIARY_AI_ENDPOINT 与 DIARY_AI_MODEL")?;
        return Ok(Box::new(engine));
    }
    // 回放引擎需要质量集，这里按目录现取一次（与上面 load 的是同一份文件）。
    let dir = options
        .quality_dir
        .clone()
        .unwrap_or_else(core_quality::quality_dir);
    let generation = GenerationSet::load(&dir.join("ai-quality-set.json"))?;
    let injection = InjectionSet::load(&dir.join("ai-injection-set.json"))?;
    Ok(Box::new(core_quality::ScriptedGenerateEngine::from_sets(
        &generation,
        &injection,
    )))
}

/// **待接点 #2**：转写引擎的选择。`--replay` 时是回放（不联网），`--live` 走
/// B4a 的 Audio Transcriptions 路径。#91 落地后在这里换成调它核心方法的实现，
/// 调用点就是下面 `engine.transcribe(...)` 那一处。
fn transcript_engine_for(options: &Options) -> Result<Option<Box<dyn TranscriptEngine>>> {
    if options.live {
        let engine = core_quality::OpenAiCompatibleTranscriptEngine::from_env()
            .context("真模型转写模式需要 DIARY_AI_ENDPOINT 与 DIARY_AI_MODEL")?;
        return Ok(Some(Box::new(engine)));
    }
    let dir = options
        .quality_dir
        .clone()
        .unwrap_or_else(core_quality::quality_dir);
    let set = TranscriptSet::load(&dir.join("ai-transcript-set.json"))?;
    Ok(Some(Box::new(core_quality::ScriptedTranscriptEngine::from_set(
        &set,
    ))))
}

/// 模板字段的中文标签：提示词要给人看（也发给模型），别印 Rust 的 `Debug` 名。
fn person_label(person: core_quality::Person) -> &'static str {
    match person {
        core_quality::Person::First => "第一人称",
        core_quality::Person::Third => "第三人称",
    }
}

fn tense_label(tense: core_quality::Tense) -> &'static str {
    match tense {
        core_quality::Tense::Past => "过去时",
        core_quality::Tense::Present => "现在时",
    }
}

fn style_label(style: core_quality::Style) -> &'static str {
    match style {
        core_quality::Style::Written => "书面",
        core_quality::Style::Colloquial => "口语",
    }
}

fn run_generation(
    engine: &dyn GenerateEngine,
    case_id: &str,
    material: &[Material],
    template: &TemplateSpec,
) -> Result<String> {
    let output = engine
        .generate(&build_request(case_id, material, template))
        .with_context(|| format!("注入用例 {case_id} 的生成调用失败"))?;
    Ok(output.text)
}

/// 拼一次真模型请求：提示词 = 模板说明 + 材料正文。
///
/// **这不是 B4b 的提示词**：B4b 要负责风格快照、来源取舍、系统提示与模板约束的
/// 具体措辞。这里只是「够用来量测」的最小拼装，拼法在报表与文档里如实说明。
fn build_request(
    case_id: &str,
    material: &[Material],
    template: &TemplateSpec,
) -> core_quality::GenerateRequest {
    let material_text = core_quality::materials_text(material);
    let template_note = format!(
        "模板 {}：人称={}，时态={}，风格={}，长度 {}-{} 字。{}",
        template.id,
        person_label(template.person),
        tense_label(template.tense),
        style_label(template.style),
        template.length.min,
        template.length.max,
        template.note
    );
    let prompt = format!(
        "请把下面这些记录整理成一篇日记。\n\n要求：{template_note}\n\n记录：\n{material_text}"
    );
    core_quality::GenerateRequest {
        case_id: case_id.to_owned(),
        material_text,
        template_note,
        prompt,
        system: None,
    }
}

/// 每条用例的判定结果（报表与 json 都用它）。
struct CaseResult {
    id: String,
    category: String,
    kind: &'static str,
    passed: bool,
    /// 逐条规则的结论（`规则 → 通过/失败/未判 + 原因`）。
    features: Vec<(String, String)>,
    /// 软信号（只警告）。
    soft: Vec<String>,
    /// 转写专用：CER 明细。
    cer_text: Option<String>,
}

fn generation_result(case: &GenerationCase, verdict: &GenerationVerdict) -> CaseResult {
    CaseResult {
        id: case.id.clone(),
        category: case.category.clone(),
        kind: "generation",
        passed: verdict.passed(),
        features: feature_rows(verdict),
        soft: soft_rows(verdict, ""),
        cer_text: None,
    }
}

fn injection_result(case: &InjectionCase, verdict: &InjectionVerdict) -> CaseResult {
    let mut features: Vec<(String, String)> = Vec::new();
    features.push(("干净/投毒一致性".to_owned(), verdict.parity.summary()));
    for item in &verdict.cleanliness {
        features.push((item.rule.wire().to_owned(), item.outcome.summary()));
    }
    for (label, side) in [("干净", &verdict.clean), ("投毒", &verdict.poisoned)] {
        features.extend(
            feature_rows(side)
                .into_iter()
                .map(|(rule, summary)| (format!("{label}·{rule}"), summary)),
        );
    }
    let mut soft = soft_rows(&verdict.poisoned, "投毒·");
    soft.extend(soft_rows(&verdict.clean, "干净·"));
    CaseResult {
        id: case.id.clone(),
        category: case.category.clone(),
        kind: "injection",
        passed: verdict.passed(),
        features,
        soft,
        cer_text: None,
    }
}

fn feature_rows(verdict: &GenerationVerdict) -> Vec<(String, String)> {
    verdict
        .features
        .iter()
        .map(|feature| (feature.rule.clone(), feature.outcome.summary()))
        .collect()
}

fn soft_rows(verdict: &GenerationVerdict, prefix: &str) -> Vec<String> {
    verdict
        .soft
        .iter()
        .map(|signal| format!("{prefix}{}：{}", signal.rule, signal.detail))
        .collect()
}

/// 转写用例的判定：算 CER、比上限，回放模式下再核对回放基线。
fn transcript_result(case: &TranscriptCase, hypothesis: &str, live: bool) -> CaseResult {
    let report = cer(&case.reference, hypothesis);
    let mut features: Vec<(String, String)> = Vec::new();

    // 1. CER 是否在上限内。
    let cer_outcome = match report.cer {
        Some(value) if value <= case.expect.max_cer => RuleOutcome::Pass,
        Some(value) => RuleOutcome::Fail(vec![format!(
            "CER {:.4} > 上限 {:.2}（{}）",
            value, case.expect.max_cer, case.expect.note
        )]),
        None => RuleOutcome::Unjudged(
            "参考文本规范化之后是空的，CER 没有分母（不编一个数）".to_owned(),
        ),
    };
    features.push(("CER 上限".to_owned(), cer_outcome.summary()));

    // 2. 回放模式下核对**回放基线**：CER 算错了这里就对不上（计算回归的锚）。
    if !live {
        let baseline = case.replay_baseline;
        let same = report.reference_len == baseline.reference_chars
            && report.hypothesis_len == baseline.hypothesis_chars
            && report.errors() == baseline.errors
            && report
                .cer
                .is_some_and(|value| (value - baseline.cer).abs() < 1e-6);
        let outcome = if same {
            RuleOutcome::Pass
        } else {
            RuleOutcome::Fail(vec![format!(
                "与质量集里记的回放基线对不上：实算（参考 {} 字 / 假设 {} 字 / {} 错 / CER {:?}）\
                 vs 基线（{} / {} / {} / {}）——判定器或质量集改了，两边要一起改",
                report.reference_len,
                report.hypothesis_len,
                report.errors(),
                report.cer,
                baseline.reference_chars,
                baseline.hypothesis_chars,
                baseline.errors,
                baseline.cer
            )])
        };
        features.push(("回放基线一致".to_owned(), outcome.summary()));
    }

    let passed = features
        .iter()
        .all(|(_, summary)| summary == "通过");
    let cer_text = format!(
        "参考 {} 字 · 假设 {} 字 · 替换 {} / 删除 {} / 插入 {} · CER {}",
        report.reference_len,
        report.hypothesis_len,
        report.substitutions,
        report.deletions,
        report.insertions,
        report
            .cer
            .map(|value| format!("{value:.4}"))
            .unwrap_or_else(|| "无定义".to_owned())
    );

    CaseResult {
        id: case.id.clone(),
        category: case.category.clone(),
        kind: "transcript",
        passed,
        features,
        soft: Vec::new(),
        cer_text: Some(cer_text),
    }
}

/// 报表 + 可选的基线 JSON。
fn report(results: &[CaseResult], options: &Options) -> Result<()> {
    // 逐条明细：失败与未判的要能一眼看见「哪条规则、为什么」。
    println!("\n-------------------------------------------");
    println!("逐条明细（全通过的只印一行；失败与未判逐条列原因）");
    for result in results {
        let mark = if result.passed { "通过" } else { "**失败**" };
        println!("\n  {:<28} [{}] {}", result.id, result.kind, mark);
        if let Some(cer_text) = &result.cer_text {
            println!("    {cer_text}");
        }
        if result.passed {
            for signal in &result.soft {
                println!("    ~ 软信号 {signal}");
            }
            continue;
        }
        for (rule, summary) in &result.features {
            if summary == "通过" {
                continue;
            }
            println!("    - {rule}：{summary}");
        }
        for signal in &result.soft {
            println!("    ~ 软信号 {signal}");
        }
    }

    // 分类汇总。
    println!("\n-------------------------------------------");
    println!(
        "分类汇总（{}）",
        if options.live {
            "真模型产出；这才是质量数字（语料规模与局限见文档）"
        } else {
            "回放模式下这些**不是质量数字**，是「评测管线可复现」的证据"
        }
    );
    let mut by_kind: BTreeMap<(&str, &str), (usize, usize)> = BTreeMap::new();
    for result in results {
        let entry = by_kind
            .entry((result.kind, result.category.as_str()))
            .or_insert((0, 0));
        entry.0 += 1;
        if result.passed {
            entry.1 += 1;
        }
    }
    println!("  {:<12} {:<26} {:>6} {:>8}", "类别", "分类", "条数", "通过率");
    for ((kind, category), (total, passed)) in &by_kind {
        println!(
            "  {:<12} {:<26} {:>6} {:>7.0}%",
            kind,
            category,
            total,
            *passed as f64 / *total as f64 * 100.0
        );
    }

    // 分项通过率（生成 / 注入 / 转写）。
    println!();
    for kind in ["generation", "injection", "transcript"] {
        let group: Vec<&CaseResult> = results.iter().filter(|result| result.kind == kind).collect();
        if group.is_empty() {
            continue;
        }
        let ok = group.iter().filter(|result| result.passed).count();
        println!(
            "  {:<12} {ok}/{} 通过（{:.0}%）",
            kind,
            group.len(),
            ok as f64 / group.len() as f64 * 100.0
        );
    }
    let total = results.len();
    let passed = results.iter().filter(|result| result.passed).count();
    println!(
        "\n  合计：{passed}/{total} 通过（{:.0}%）",
        passed as f64 / total as f64 * 100.0
    );
    if !options.live {
        println!(
            "  提醒：这是**回放**数字——输入是人工写的参考输出与注入的假转写结果，\n\
             它证明「判定器 + 报表 + 门禁」这条管线可复现，**不说明模型质量**。"
        );
    }

    if let Some(path) = &options.json {
        write_baseline(path, results, options)?;
    }
    Ok(())
}

/// 写基线 JSON。**回放与真模型是同一个形状，但 `mode` 字段不同**——
/// 以后拿到一个 json 文件，第一眼就知道它是不是质量数字。
fn write_baseline(path: &PathBuf, results: &[CaseResult], options: &Options) -> Result<()> {
    let cases: Vec<serde_json::Value> = results
        .iter()
        .map(|result| {
            serde_json::json!({
                "id": result.id,
                "kind": result.kind,
                "category": result.category,
                "passed": result.passed,
                "cer": result.cer_text,
                "failedRules": result
                    .features
                    .iter()
                    .filter(|(_, summary)| summary != "通过")
                    .map(|(rule, summary)| serde_json::json!({"rule": rule, "detail": summary}))
                    .collect::<Vec<_>>(),
                "softSignals": result.soft,
            })
        })
        .collect();
    let total = results.len();
    let passed = results.iter().filter(|result| result.passed).count();
    let document = serde_json::json!({
        "version": 1,
        "mode": if options.live { "live" } else { "replay" },
        "modeNote": if options.live {
            "真模型产出的输出；这才是质量数字（语料规模与局限见 docs/architecture/m3-AI-质量集与评测.md）"
        } else {
            "回放：输入是质量集里人工写的参考输出与注入的假转写结果。**这不是质量基线**，它证明的是评测本身可复现"
        },
        "networked": options.live,
        "totals": { "cases": total, "passed": passed },
        "byKind": {
            "generation": kind_totals(results, "generation"),
            "injection": kind_totals(results, "injection"),
            "transcript": kind_totals(results, "transcript"),
        },
        "cases": cases,
    });
    let json = serde_json::to_string_pretty(&document)?;
    std::fs::write(path, format!("{json}\n"))
        .with_context(|| format!("写不出基线文件：{}", path.display()))?;
    println!(
        "\n基线已写到 {}（mode = {}）",
        path.display(),
        if options.live { "live" } else { "replay" }
    );
    Ok(())
}

fn kind_totals(results: &[CaseResult], kind: &str) -> serde_json::Value {
    let group: Vec<&CaseResult> = results.iter().filter(|result| result.kind == kind).collect();
    let passed = group.iter().filter(|result| result.passed).count();
    serde_json::json!({ "cases": group.len(), "passed": passed })
}
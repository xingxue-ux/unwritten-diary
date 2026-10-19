//! M0 技术验证脚手架。
//!
//! 这里放的是**验证代码，不是产品代码**：用来给契约 1.4 节里那几个未决实现选择
//! 拿到真实测量数字。结论记在 `docs/architecture/M0-技术验证.md`。
//!
//! 用法：
//! ```text
//! cargo run -p diary_probe -- short-word-search [片段数]
//! cargo run -p diary_probe -- plugin-runtime
//! cargo run -p diary_probe -- all [片段数]
//! ```

#[cfg(feature = "model")]
mod model;
#[cfg(feature = "plugin")]
mod plugin;
// 质量集量测跟着 search feature：关键词那一路只用核心的检索接口。
#[cfg(feature = "search")]
mod quality;
#[cfg(feature = "search")]
mod search;
// 语义量测要真模型（diary_core 里的 ort 实现），也走同一个 feature：
// 没有模型时这条命令会直接报错，不进 CI。
#[cfg(feature = "search")]
mod semantic_quality;
// 混合检索（RRF）的量测与阈值扫描，同样需要真模型。
#[cfg(feature = "search")]
mod hybrid_quality;
// 大库可用性实测（issue #90）：1 万/5 万/10 万块的构建、查询、内存、体积与 int8 召回。
#[cfg(feature = "search")]
mod large_bench;
// `Core::open` 的整代口径核对成本（评审 #128 第 6 条）：v15 覆盖索引前后的对比。
#[cfg(feature = "search")]
mod open_cost;
// AI 质量集的评测（issue #93 前半）：判定器 + CER + 回放/真模型两种模式。
// 判定器本身在 diary_core 里，由 `ai-quality` feature 打开（这一片加进 search 的依赖）。
#[cfg(feature = "search")]
mod ai_quality;

use anyhow::Result;

fn usage() -> &'static str {
    "用法：\n  diary_probe short-word-search [片段数，默认 20000]\n  diary_probe quality-set（读 tests/quality/search_quality_set.json，可用 DIARY_QUALITY_SET 覆盖）
  diary_probe semantic-quality [--storage f32|int8]（真模型：语义召回 + pooling×前缀四组 + f32/int8，需要 DIARY_MODEL_DIR 与 DIARY_ORT_DYLIB）
  diary_probe hybrid-quality [--storage f32|int8]（真模型：三类对照 + 阈值扫描 + matchedBy 分布，需要 DIARY_MODEL_DIR 与 DIARY_ORT_DYLIB）
  diary_probe ai-quality [--replay|--live] [--generation-only] [--json 文件] [--quality-dir 目录]（AI 质量集：生成特征 + 防提示注入 + 转写 CER。默认 --replay，不联网；--live 要 DIARY_AI_ENDPOINT/DIARY_AI_MODEL）
  diary_probe ai-quality-set（与 ai-quality 同义，风格对齐 quality-set / semantic-quality / hybrid-quality）
  diary_probe large-bench build <块数> [--storage f32|int8] [--dir 目录]（真模型：建合成语料 + 语义索引，报构建时间/内存/体积）
  diary_probe large-bench query <块数> [--storage f32|int8] [--dir 目录] [--queries 40]（另起进程量查询延迟与峰值内存）
  diary_probe large-bench recall-quality（既有 36 条质量集：f32 vs int8 召回）
  diary_probe large-bench recall-large [块数，默认 2000]（自造语料：f32 vs int8 召回）
  diary_probe open-cost <块数> --dir 目录（同一份库上有/无 v15 口径覆盖索引的 open 耗时对比）
  diary_probe plugin-runtime\n  diary_probe vector-model（需要 --features model 与本地权重）\n  diary_probe all [片段数]"
}

/// 只在需要检索的构建里解析片段数，否则 plugin-only 构建会报未使用变量。
#[cfg(feature = "search")]
fn segments_arg(args: &[String]) -> Result<usize> {
    Ok(args
        .get(1)
        .map(|value| value.parse::<usize>())
        .transpose()?
        .unwrap_or(20_000))
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let command = args.first().map(String::as_str).unwrap_or("all");

    match command {
        "short-word-search" => {
            #[cfg(feature = "search")]
            search::run(segments_arg(&args)?)?;
            #[cfg(not(feature = "search"))]
            anyhow::bail!("这个构建没有启用 search feature（尝试 cargo run -p diary_probe --features search）");
        }
        "vector-model" => {
            #[cfg(feature = "model")]
            model::run()?;
            #[cfg(not(feature = "model"))]
            anyhow::bail!("这个构建没有启用 model feature（尝试 cargo run -p diary_probe --features model）");
        }
        "quality-set" => {
            #[cfg(feature = "search")]
            quality::run()?;
            #[cfg(not(feature = "search"))]
            anyhow::bail!("这个构建没有启用 search feature（尝试 cargo run -p diary_probe --features search）");
        }
        "semantic-quality" => {
            #[cfg(feature = "search")]
            semantic_quality::run(&args[1..])?;
            #[cfg(not(feature = "search"))]
            anyhow::bail!("这个构建没有启用 search feature（尝试 cargo run -p diary_probe --features search）");
        }
        "hybrid-quality" => {
            #[cfg(feature = "search")]
            hybrid_quality::run(&args[1..])?;
            #[cfg(not(feature = "search"))]
            anyhow::bail!("这个构建没有启用 search feature（尝试 cargo run -p diary_probe --features search）");
        }
        // 大库可用性（issue #90）：build / query / recall-quality / recall-large。
        "large-bench" => {
            #[cfg(feature = "search")]
            large_bench::run(&args[1..])?;
            #[cfg(not(feature = "search"))]
            anyhow::bail!("这个构建没有启用 search feature（尝试 cargo run -p diary_probe --features search）");
        }
        "open-cost" => {
            #[cfg(feature = "search")]
            open_cost::run(&args[1..])?;
            #[cfg(not(feature = "search"))]
            anyhow::bail!("这个构建没有启用 search feature（尝试 cargo run -p diary_probe --features search）");
        }
        // `ai-quality-set` 与 `ai-quality` 同义：前者对齐 quality-set / semantic-quality /
        // hybrid-quality 的取名风格，后者短一点。两个都留，免得记错名字以为没这条命令。
        "ai-quality" | "ai-quality-set" => {
            #[cfg(feature = "search")]
            ai_quality::run(&args[1..])?;
            #[cfg(not(feature = "search"))]
            anyhow::bail!("这个构建没有启用 search feature（尝试 cargo run -p diary_probe --features search）");
        }
        "plugin-runtime" => {
            #[cfg(feature = "plugin")]
            plugin::run()?;
            #[cfg(not(feature = "plugin"))]
            anyhow::bail!("这个构建没有启用 plugin feature（尝试 cargo run -p diary_probe --features plugin）");
        }
        "all" => {
            #[cfg(feature = "search")]
            search::run(segments_arg(&args)?)?;
            #[cfg(feature = "search")]
            println!();
            #[cfg(feature = "plugin")]
            plugin::run()?;
        }
        "-h" | "--help" | "help" => println!("{}", usage()),
        other => {
            eprintln!("未知子命令：{other}\n\n{}", usage());
            std::process::exit(2);
        }
    }

    // Android 上**必须**跳过 C 运行时的退出收尾（`exit()` → `__cxa_finalize`）。
    //
    // 原因见 docs/architecture/M0-技术验证.md 3.3：libonnxruntime.so 自己注册的
    // 静态析构会在退出时去锁一个已经销毁的 mutex，于是 `FORTIFY: pthread_mutex_lock
    // called on a destroyed mutex` 之后 SIGABRT（退出码 134）。这条路径跟 Rust 侧
    // 释放不释放无关（tombstone 停在 `__cxa_finalize → libonnxruntime.so`），所以
    // 「资源放进 static 不释放」挡不住它——桌面那条路径一挡就好了，Android 这条
    // 只能不让收尾跑。`libc::_exit` 直接走系统调用退出，不跑 atexit / 静态析构；
    // 所以要**先**把标准输出刷干净，否则最后几行会丢。
    //
    // 产品那边不需要这个：Android 回收应用进程用 SIGKILL，本来就不跑这些析构；
    // 只有「会被当成 CLI 跑、要求退出码」的入口（探针、测试）需要这一步。
    #[cfg(target_os = "android")]
    {
        use std::io::Write;
        let _ = std::io::stdout().flush();
        let _ = std::io::stderr().flush();
        // SAFETY: `_exit` 是 async-signal-safe 的进程退出，不返回。
        unsafe { libc::_exit(0) }
    }

    #[cfg(not(target_os = "android"))]
    Ok(())
}
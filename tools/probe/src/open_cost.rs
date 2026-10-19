//! `open` 的整代口径核对成本：v15 的覆盖索引**前后**对比（评审 #128 第 6 条）。
//!
//! `Core::open` 在模型 `Configured` / `Ready` 时会核对生效代次的整代口径
//! （`semantic::active_caliber`：`SELECT DISTINCT model_version, dims, storage ...
//! WHERE generation = ?`）。v15 之前 `chunk_vectors` 没有按 `generation` 的索引，那是
//! 一次**整表扫描**；v15 加了覆盖索引 `idx_chunk_vectors_caliber`。
//!
//! 这个子命令在同一份库上量两次：**有索引**与 `DROP INDEX` 之后**没有索引**，并打印
//! 两次的 `EXPLAIN QUERY PLAN`。这样「改没改好」不靠猜，也不用准备两棵代码树。
//!
//! 语料是直接 SQL 灌进去的确定性数据（不导入、不跑模型）：这一条量的是 open 的
//! 口径核对，不是导入流水线。模型配置指向 `--dir` 下三个空文件——`Configured`
//! 只 stat、不加载，触发路径与真实机器上「配好模型、还没加载」完全一致。
//!
//! 用法（块数按需要改；50k 块 ≈ 26 MiB int8 向量）：
//!
//! ```text
//! cargo run --release -p diary_probe -- open-cost 50000 --dir "$HOME/dt-big90/open-cost"
//! ```
//!
//! 注：两次都跑在**热页缓存**上，量的是每次 open 的 CPU/内存扫描成本；冷启动时
//! 覆盖索引读的字节数远小于整表（索引只有几 MiB），差距只会更大。数字不是绝对
//! 承诺，同一台机器上的相对值才有意义。

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use diary_core::Core;
use rusqlite::{params, Connection};

pub fn run(args: &[String]) -> Result<()> {
    let blocks: usize = args
        .first()
        .context("用法：open-cost <块数> --dir <目录>")?
        .parse()
        .context("块数必须是正整数")?;
    let dir = dir_arg(args)?;
    prepare(&dir, blocks)?;
    // 让 open 走「模型 Configured」那条自动触发路径：只 stat 文件，不加载模型。
    set_fake_model_env(&dir);

    let path = dir.join("library.sqlite");
    let with_index = measure(&path)?;
    report("有索引（v15）", &with_index);
    report_plan(&path);
    assert_no_noise(&path)?;

    {
        let conn = Connection::open(&path)?;
        conn.execute_batch("DROP INDEX IF EXISTS idx_chunk_vectors_caliber;")?;
    }
    let without_index = measure(&path)?;
    report("无索引（v15 之前）", &without_index);
    report_plan(&path);

    let before = without_index.1.as_secs_f64() * 1000.0;
    let after = with_index.1.as_secs_f64() * 1000.0;
    println!(
        "OPEN_COST blocks={blocks} median_without_index={before:.2}ms \
         median_with_index={after:.2}ms speedup={:.2}x",
        before / after.max(f64::EPSILON)
    );
    if after >= before {
        bail!("覆盖索引没有变快（{after:.2}ms >= {before:.2}ms），先查查询计划");
    }
    Ok(())
}

fn dir_arg(args: &[String]) -> Result<PathBuf> {
    let mut index = 0;
    while index < args.len() {
        if args[index] == "--dir" {
            return Ok(PathBuf::from(
                args.get(index + 1).context("--dir 后面要跟目录")?,
            ));
        }
        index += 1;
    }
    bail!("必须给 --dir（例：--dir \"$HOME/dt-big90/open-cost\"）")
}

/// 建库 + 灌确定性语料。已有同名库时**直接失败**，不替调用方删除目录。
fn prepare(dir: &Path, blocks: usize) -> Result<()> {
    if dir.join("library.sqlite").exists() {
        bail!(
            "{} 里已经有 library.sqlite；换一个 --dir，或先手动删掉这次量测的库",
            dir.display()
        );
    }
    std::fs::create_dir_all(dir)?;
    let path = dir.join("library.sqlite");
    {
        // 先建 schema（此时环境变量还没设：open 的自动触发走 NotConfigured，什么都不排）。
        let core = Core::open(&path)?;
        drop(core);
    }
    let conn = Connection::open(&path)?;
    let tx = conn.unchecked_transaction()?;
    {
        let mut chunk = tx.prepare(
            "INSERT INTO text_chunks \
             (chunk_id, ordinal, text, kind, coverage, day_key, chunker_version, created_at) \
             VALUES (?1, ?2, ?3, 'text', 'complete', ?4, 'probe-chunker', ?5)",
        )?;
        let mut vector = tx.prepare(
            "INSERT INTO chunk_vectors \
             (chunk_id, generation, model_version, dims, storage, vector, created_at) \
             VALUES (?1, 1, 'probe-model@000000000000', 512, 'int8', ?2, ?3)",
        )?;
        // 512 维 int8：与产品默认一致（open 的口径核对只读列，不解码向量）。
        let blob = vec![7_u8; 512];
        for index in 0..blocks {
            let chunk_id = format!("chunk-{index:06}");
            let day = format!("2026-{:02}-{:02}", 1 + (index / 28) % 12, 1 + index % 28);
            chunk.execute(params![chunk_id, index as i64, "量测语料。", day, "2026-10-10T00:00:00.000Z"])?;
            vector.execute(params![chunk_id, blob, "2026-10-10T00:00:00.000Z"])?;
        }
    }
    tx.execute_batch("UPDATE index_meta SET active_generation = 1;")?;
    tx.commit()?;
    println!("SEED blocks={blocks} dir={}", dir.display());
    Ok(())
}

fn set_fake_model_env(dir: &Path) {
    let model_dir = dir.join("fake-model");
    std::fs::create_dir_all(&model_dir).unwrap();
    std::fs::write(model_dir.join("model.onnx"), b"not-a-real-model").unwrap();
    std::fs::write(model_dir.join("tokenizer.json"), b"{}").unwrap();
    let ort = dir.join("libonnxruntime.so");
    std::fs::write(&ort, b"not-a-real-ort").unwrap();
    std::env::set_var("DIARY_MODEL_DIR", &model_dir);
    std::env::set_var("DIARY_ORT_DYLIB", &ort);
}

/// 一次 warmup + 7 次测量，返回每次的耗时与中位数。
fn measure(path: &Path) -> Result<(Vec<Duration>, Duration)> {
    drop(Core::open(path)?);
    let mut samples = Vec::new();
    for _ in 0..7 {
        let started = Instant::now();
        let core = Core::open(path)?;
        samples.push(started.elapsed());
        drop(core);
    }
    let mut sorted = samples.clone();
    sorted.sort_unstable();
    Ok((samples, sorted[sorted.len() / 2]))
}

fn report(label: &str, (samples, median): &(Vec<Duration>, Duration)) {
    let mut sorted = samples.clone();
    sorted.sort_unstable();
    println!(
        "OPEN {label}: median={:.2}ms min={:.2}ms max={:.2}ms samples=[{}]",
        median.as_secs_f64() * 1000.0,
        sorted[0].as_secs_f64() * 1000.0,
        sorted[sorted.len() - 1].as_secs_f64() * 1000.0,
        samples
            .iter()
            .map(|sample| format!("{:.2}", sample.as_secs_f64() * 1000.0))
            .collect::<Vec<_>>()
            .join(", ")
    );
}

fn report_plan(path: &Path) {
    let conn = Connection::open(path).unwrap();
    let mut statement = conn
        .prepare(
            "EXPLAIN QUERY PLAN SELECT DISTINCT model_version, dims, storage \
             FROM chunk_vectors WHERE generation = ?1 \
             ORDER BY model_version, dims, storage",
        )
        .unwrap();
    let rows: Vec<String> = statement
        .query_map(params![1_i64], |row| row.get::<_, String>(3))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    println!("PLAN caliber: {}", rows.join(" | "));
}

/// 口径一致时自动触发不该排任何任务：量的是「正常库每次 open 的核对成本」。
fn assert_no_noise(path: &Path) -> Result<()> {
    let count: i64 = Connection::open(path)?.query_row(
        "SELECT COUNT(*) FROM jobs WHERE kind = 'semantic_index'",
        [],
        |row| row.get(0),
    )?;
    if count != 0 {
        bail!("口径一致的库不该自动排队，实际排了 {count} 条");
    }
    Ok(())
}

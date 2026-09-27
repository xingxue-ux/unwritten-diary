//! 中文短词检索实验：比较四条路径在真实 SQLite 上的表现。
//!
//! 关心的问题来自契约 1.4 节的未决项与任务书 5.3 节：
//! 「妈妈」「离职」这类一到二字查询，哪条路径能召回，代价是多少。
//!
//! 路径：
//! 1. FTS5 默认分词（unicode61）
//! 2. FTS5 trigram
//! 3. 自建 2-gram 倒排（n-gram 方案）
//! 4. jieba 分词索引 + 2-gram 补充（分词补充索引方案）
//!
//! 每条路径测两个指标：
//! - **全量计数**：`count(*)` 真正命中的条数，用来核对召回是否完整（最坏情况延迟）。
//! - **首屏 20 条**：真实查询要的是第一页，用 `LIMIT 20` 测，更接近实际体验。
//!
//! 所有数字都是本机实测；语料是确定性生成的虚构数据。

use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;
use std::time::Instant;

use anyhow::Result;
use rusqlite::{params, Connection};

/// 查询集：覆盖二字词、三字词，以及「短词是长词前缀」的情况。
const QUERIES: [&str; 5] = ["妈妈", "离职", "面试", "加班", "面试官"];

const WORDS: [&str; 20] = [
    "妈妈", "离职", "面试", "加班", "房东", "疫苗", "体检", "房租", "同事", "项目", "咖啡", "失眠",
    "跑步", "医生", "报告", "合同", "地铁", "天气", "朋友", "计划",
];

/// 分页大小，与契约第 1.2 节的默认每页条数一致。
const PAGE_SIZE: i64 = 20;

/// 确定性伪随机，保证语料可复现。
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

    /// 显式返回 `&str`：写成泛型 `&'a [T] -> &'a T` 会依赖自动解引用的推导，
    /// MSRV 1.94 上会推出 `T = str` 而编译失败（CI 抓到的就是这个）。
    fn pick<'a>(&mut self, items: &'a [&'a str]) -> &'a str {
        let index = (self.next() % items.len() as u64) as usize;
        items[index]
    }
}

/// 生成虚构语料。
///
/// 关键点：同一个子句里的词**不加分隔地连在一起**，只有子句之间才有标点。
/// 这才是真实日记的样子；如果每个词都用逗号隔开，unicode61 会因为标点切分而
/// 假性通过，测不出中文分词的真正问题（第一版就是这样误判的）。
fn build_corpus(segments: usize) -> Vec<String> {
    let mut rng = Rng(0x5eed_1234_abcd_0001);
    let mut corpus = Vec::with_capacity(segments);
    for i in 0..segments {
        let clauses = 2 + (rng.next() % 3) as usize;
        let mut text = String::new();
        for _ in 0..clauses {
            let words = 2 + (rng.next() % 4) as usize;
            for _ in 0..words {
                text.push_str(rng.pick(&WORDS));
            }
            text.push('，');
        }
        // 低频事件词嵌在长句里，不加分隔：这正是 unicode61 会漏掉的形态。
        if rng.next() % 100 < 4 {
            text.push_str("今天决定离职了，");
        }
        if rng.next().is_multiple_of(50) {
            text.push_str("面试官问了很多，");
        }
        corpus.push(format!("第{}天，{}。", i + 1, text));
    }
    corpus
}

fn ground_truth(corpus: &[String], query: &str) -> usize {
    corpus.iter().filter(|text| text.contains(query)).count()
}

fn open_db(name: &str) -> Result<(Connection, PathBuf)> {
    let path = std::env::temp_dir().join(format!("diary_probe_{name}.sqlite"));
    let _ = fs::remove_file(&path);
    let conn = Connection::open(&path)?;
    // 探针只关心索引结构与查询表现：关掉日志与同步，让数字更纯粹。
    conn.execute_batch("PRAGMA journal_mode=OFF; PRAGMA synchronous=OFF;")?;
    Ok((conn, path))
}

fn db_size(path: &PathBuf) -> u64 {
    fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

fn human(bytes: u64) -> String {
    if bytes >= 1024 * 1024 {
        format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0))
    } else {
        format!("{:.1} KiB", bytes as f64 / 1024.0)
    }
}

/// 预热后取多次运行的中位数。
fn median_ms<F>(runs: usize, mut query: F) -> f64
where
    F: FnMut() -> Result<usize>,
{
    for _ in 0..3 {
        let _ = query();
    }
    let mut samples = Vec::with_capacity(runs);
    for _ in 0..runs {
        let start = Instant::now();
        let _ = query();
        samples.push(start.elapsed().as_secs_f64() * 1000.0);
    }
    samples.sort_by(|a, b| a.partial_cmp(b).expect("样本不是 NaN"));
    samples[samples.len() / 2]
}

struct Row {
    query: &'static str,
    hits: usize,
    count_ms: f64,
    page_ms: f64,
    page_rows: usize,
}

struct Report {
    path: &'static str,
    size: u64,
    build_ms: f64,
    rows: Vec<Row>,
}

/// 同一查询测「全量计数」与「首屏一页」两个延迟。
fn measure<F, G>(mut count: F, mut page: G) -> Result<(usize, f64, usize, f64)>
where
    F: FnMut() -> Result<usize>,
    G: FnMut() -> Result<usize>,
{
    let hits = count()?;
    let count_ms = median_ms(50, &mut count);
    let page_rows = page()?;
    let page_ms = median_ms(50, &mut page);
    Ok((hits, count_ms, page_rows, page_ms))
}

fn print_report(report: &Report, truth: &[(&str, usize)]) {
    println!(
        "\n[{}]\n  索引体积 {} · 构建 {:.0} ms",
        report.path,
        human(report.size),
        report.build_ms
    );
    for row in &report.rows {
        let expected = truth
            .iter()
            .find(|(q, _)| *q == row.query)
            .map(|(_, n)| *n)
            .unwrap_or(0);
        let recall = if expected == 0 {
            "n/a".to_owned()
        } else if row.hits == expected {
            "全召回".to_owned()
        } else {
            format!("漏 {}", expected.saturating_sub(row.hits))
        };
        println!(
            "  「{}」 命中 {}/{}（{}）· 全量计数 {:.3} ms · 首屏 {} 条 {:.3} ms",
            row.query, row.hits, expected, recall, row.count_ms, row.page_rows, row.page_ms
        );
    }
}

/// 路径 1：FTS5 默认分词（unicode61）。
fn run_unicode61(corpus: &[String]) -> Result<Report> {
    let (mut conn, path) = open_db("unicode61")?;
    let start = Instant::now();
    conn.execute_batch("CREATE VIRTUAL TABLE seg USING fts5(text);")?;
    {
        let tx = conn.transaction()?;
        {
            let mut stmt = tx.prepare("INSERT INTO seg(text) VALUES (?1)")?;
            for text in corpus {
                stmt.execute(params![text])?;
            }
        }
        tx.commit()?;
    }
    let build_ms = start.elapsed().as_secs_f64() * 1000.0;

    let mut rows = Vec::new();
    for query in QUERIES {
        let (hits, count_ms, page_rows, page_ms) = measure(
            || -> Result<usize> {
                let n: i64 = conn.query_row(
                    "SELECT count(*) FROM seg WHERE seg MATCH ?1",
                    params![query],
                    |row| row.get(0),
                )?;
                Ok(n as usize)
            },
            || -> Result<usize> {
                let mut stmt = conn.prepare("SELECT rowid FROM seg WHERE seg MATCH ?1 LIMIT ?2")?;
                let mut count = 0;
                for row in stmt.query_map(params![query, PAGE_SIZE], |row| row.get::<_, i64>(0))? {
                    row?;
                    count += 1;
                }
                Ok(count)
            },
        )?;
        rows.push(Row {
            query,
            hits,
            count_ms,
            page_ms,
            page_rows,
        });
    }
    drop(conn);
    Ok(Report {
        path: "FTS5 unicode61",
        size: db_size(&path),
        build_ms,
        rows,
    })
}

/// 路径 2：FTS5 trigram。官方说明短于三个字符的查询无法匹配。
fn run_trigram(corpus: &[String]) -> Result<Report> {
    let (mut conn, path) = open_db("trigram")?;
    let start = Instant::now();
    conn.execute_batch("CREATE VIRTUAL TABLE seg USING fts5(text, tokenize='trigram');")?;
    {
        let tx = conn.transaction()?;
        {
            let mut stmt = tx.prepare("INSERT INTO seg(text) VALUES (?1)")?;
            for text in corpus {
                stmt.execute(params![text])?;
            }
        }
        tx.commit()?;
    }
    let build_ms = start.elapsed().as_secs_f64() * 1000.0;

    let mut rows = Vec::new();
    for query in QUERIES {
        let (hits, count_ms, page_rows, page_ms) = measure(
            || -> Result<usize> {
                let n: i64 = conn.query_row(
                    "SELECT count(*) FROM seg WHERE seg MATCH ?1",
                    params![query],
                    |row| row.get(0),
                )?;
                Ok(n as usize)
            },
            || -> Result<usize> {
                let mut stmt = conn.prepare("SELECT rowid FROM seg WHERE seg MATCH ?1 LIMIT ?2")?;
                let mut count = 0;
                for row in stmt.query_map(params![query, PAGE_SIZE], |row| row.get::<_, i64>(0))? {
                    row?;
                    count += 1;
                }
                Ok(count)
            },
        )?;
        rows.push(Row {
            query,
            hits,
            count_ms,
            page_ms,
            page_rows,
        });
    }
    drop(conn);
    Ok(Report {
        path: "FTS5 trigram",
        size: db_size(&path),
        build_ms,
        rows,
    })
}

/// 路径 3：自建 2-gram 倒排，另建 1-gram 表兜一字查询。
fn run_ngram(corpus: &[String]) -> Result<Report> {
    let (mut conn, path) = open_db("ngram")?;
    let start = Instant::now();
    conn.execute_batch(
        "CREATE TABLE seg(id INTEGER PRIMARY KEY, text TEXT NOT NULL);
         CREATE TABLE bigram(gram TEXT NOT NULL, seg_id INTEGER NOT NULL);
         CREATE TABLE unigram(gram TEXT NOT NULL, seg_id INTEGER NOT NULL);",
    )?;
    {
        let tx = conn.transaction()?;
        {
            let mut seg_stmt = tx.prepare("INSERT INTO seg(id, text) VALUES (?1, ?2)")?;
            let mut bi_stmt = tx.prepare("INSERT INTO bigram(gram, seg_id) VALUES (?1, ?2)")?;
            let mut uni_stmt = tx.prepare("INSERT INTO unigram(gram, seg_id) VALUES (?1, ?2)")?;
            for (index, text) in corpus.iter().enumerate() {
                let id = index as i64;
                seg_stmt.execute(params![id, text])?;
                let chars: Vec<char> = text.chars().collect();
                let mut seen_bi: HashSet<String> = HashSet::new();
                let mut seen_uni: HashSet<String> = HashSet::new();
                for window in chars.windows(2) {
                    let gram: String = window.iter().collect();
                    if seen_bi.insert(gram.clone()) {
                        bi_stmt.execute(params![gram, id])?;
                    }
                }
                for ch in &chars {
                    let gram = ch.to_string();
                    if seen_uni.insert(gram.clone()) {
                        uni_stmt.execute(params![gram, id])?;
                    }
                }
            }
        }
        tx.commit()?;
    }
    conn.execute_batch(
        "CREATE INDEX idx_bigram ON bigram(gram);
         CREATE INDEX idx_unigram ON unigram(gram);",
    )?;
    let build_ms = start.elapsed().as_secs_f64() * 1000.0;

    let mut rows = Vec::new();
    for query in QUERIES {
        let chars: Vec<char> = query.chars().collect();
        let grams: Vec<String> = chars.windows(2).map(|w| w.iter().collect()).collect();
        let single = chars.len() == 1;

        let ids_for = |gram: &str| -> Result<HashSet<i64>> {
            let sql = if single {
                "SELECT seg_id FROM unigram WHERE gram = ?1"
            } else {
                "SELECT seg_id FROM bigram WHERE gram = ?1"
            };
            let mut stmt = conn.prepare(sql)?;
            let set = stmt
                .query_map(params![gram], |row| row.get::<_, i64>(0))?
                .collect::<Result<HashSet<i64>, _>>()?;
            Ok(set)
        };
        // 三字以上：先按 gram 取候选，再回原文确认，去掉 gram 分离造成的假阳性。
        let verified_for = |gram: &str| -> Result<HashSet<i64>> {
            let mut stmt = conn.prepare(
                "SELECT s.id FROM bigram g JOIN seg s ON s.id = g.seg_id
                 WHERE g.gram = ?1 AND instr(s.text, ?2) > 0",
            )?;
            let set = stmt
                .query_map(params![gram, query], |row| row.get::<_, i64>(0))?
                .collect::<Result<HashSet<i64>, _>>()?;
            Ok(set)
        };

        let (hits, count_ms, page_rows, page_ms) = measure(
            || -> Result<usize> {
                if single {
                    return Ok(ids_for(query)?.len());
                }
                let mut total: Option<HashSet<i64>> = None;
                for gram in &grams {
                    let set = if chars.len() == 2 {
                        ids_for(gram)?
                    } else {
                        verified_for(gram)?
                    };
                    total = Some(match total {
                        None => set,
                        Some(previous) => previous.intersection(&set).copied().collect(),
                    });
                }
                Ok(total.map(|set| set.len()).unwrap_or(0))
            },
            || -> Result<usize> {
                // 首屏：取候选、排序、截断，模拟真实分页。
                let mut candidates: Vec<i64> = if single {
                    ids_for(query)?.into_iter().collect()
                } else if chars.len() == 2 {
                    ids_for(&grams[0])?.into_iter().collect()
                } else {
                    verified_for(&grams[0])?.into_iter().collect()
                };
                candidates.sort_unstable();
                candidates.truncate(PAGE_SIZE as usize);
                Ok(candidates.len())
            },
        )?;
        rows.push(Row {
            query,
            hits,
            count_ms,
            page_ms,
            page_rows,
        });
    }
    drop(conn);
    Ok(Report {
        path: "自建 n-gram（2-gram + 1-gram）",
        size: db_size(&path),
        build_ms,
        rows,
    })
}

/// 路径 4：jieba 分词索引 + 2-gram 补充，两者取并集。
fn run_jieba(corpus: &[String]) -> Result<Report> {
    let (mut conn, path) = open_db("jieba")?;
    let jieba = jieba_rs::Jieba::new();
    let start = Instant::now();
    conn.execute_batch(
        "CREATE TABLE seg(id INTEGER PRIMARY KEY, text TEXT NOT NULL);
         CREATE TABLE token(token TEXT NOT NULL, seg_id INTEGER NOT NULL);
         CREATE TABLE bigram(gram TEXT NOT NULL, seg_id INTEGER NOT NULL);",
    )?;
    {
        let tx = conn.transaction()?;
        {
            let mut seg_stmt = tx.prepare("INSERT INTO seg(id, text) VALUES (?1, ?2)")?;
            let mut tok_stmt = tx.prepare("INSERT INTO token(token, seg_id) VALUES (?1, ?2)")?;
            let mut bi_stmt = tx.prepare("INSERT INTO bigram(gram, seg_id) VALUES (?1, ?2)")?;
            for (index, text) in corpus.iter().enumerate() {
                let id = index as i64;
                seg_stmt.execute(params![id, text])?;
                let mut seen: HashSet<&str> = HashSet::new();
                for token in jieba.cut(text, false) {
                    let word = token.word.trim();
                    if word.is_empty() || !seen.insert(word) {
                        continue;
                    }
                    tok_stmt.execute(params![word, id])?;
                }
                let chars: Vec<char> = text.chars().collect();
                let mut seen_bi: HashSet<String> = HashSet::new();
                for window in chars.windows(2) {
                    let gram: String = window.iter().collect();
                    if seen_bi.insert(gram.clone()) {
                        bi_stmt.execute(params![gram, id])?;
                    }
                }
            }
        }
        tx.commit()?;
    }
    conn.execute_batch(
        "CREATE INDEX idx_token ON token(token);
         CREATE INDEX idx_bigram ON bigram(gram);",
    )?;
    let build_ms = start.elapsed().as_secs_f64() * 1000.0;

    let mut rows = Vec::new();
    for query in QUERIES {
        let chars: Vec<char> = query.chars().collect();
        let grams: Vec<String> = chars.windows(2).map(|w| w.iter().collect()).collect();

        // 分词索引与 2-gram 取并集。
        //
        // 不能做成「分词查不到才回退」：分词会把「面试官」切成一个词，查询「面试」
        // 在分词索引里查不到那部分记录，但它们在 2-gram 索引里是有的。只做回退会漏召回。
        // 这是本轮实测出来的设计要点。
        let token_ids = |limit: Option<i64>| -> Result<HashSet<i64>> {
            let sql = match limit {
                Some(_) => "SELECT DISTINCT seg_id FROM token WHERE token = ?1 LIMIT ?2",
                None => "SELECT DISTINCT seg_id FROM token WHERE token = ?1",
            };
            let mut stmt = conn.prepare(sql)?;
            let set = match limit {
                Some(bound) => stmt
                    .query_map(params![query, bound], |row| row.get::<_, i64>(0))?
                    .collect::<Result<HashSet<i64>, _>>()?,
                None => stmt
                    .query_map(params![query], |row| row.get::<_, i64>(0))?
                    .collect::<Result<HashSet<i64>, _>>()?,
            };
            Ok(set)
        };
        let gram_ids = |gram: &str, limit: Option<i64>| -> Result<HashSet<i64>> {
            let sql = match limit {
                Some(_) => {
                    "SELECT DISTINCT s.id FROM bigram g JOIN seg s ON s.id = g.seg_id
                     WHERE g.gram = ?1 AND instr(s.text, ?2) > 0 LIMIT ?3"
                }
                None => {
                    "SELECT DISTINCT s.id FROM bigram g JOIN seg s ON s.id = g.seg_id
                     WHERE g.gram = ?1 AND instr(s.text, ?2) > 0"
                }
            };
            let mut stmt = conn.prepare(sql)?;
            let set = match limit {
                Some(bound) => stmt
                    .query_map(params![gram, query, bound], |row| row.get::<_, i64>(0))?
                    .collect::<Result<HashSet<i64>, _>>()?,
                None => stmt
                    .query_map(params![gram, query], |row| row.get::<_, i64>(0))?
                    .collect::<Result<HashSet<i64>, _>>()?,
            };
            Ok(set)
        };

        let (hits, count_ms, page_rows, page_ms) = measure(
            || -> Result<usize> {
                let mut ids = token_ids(None)?;
                for gram in &grams {
                    ids.extend(gram_ids(gram, None)?);
                }
                Ok(ids.len())
            },
            || -> Result<usize> {
                let mut ids = token_ids(Some(PAGE_SIZE * 2))?;
                for gram in &grams {
                    ids.extend(gram_ids(gram, Some(PAGE_SIZE * 2))?);
                }
                let mut sorted: Vec<i64> = ids.into_iter().collect();
                sorted.sort_unstable();
                sorted.truncate(PAGE_SIZE as usize);
                Ok(sorted.len())
            },
        )?;
        rows.push(Row {
            query,
            hits,
            count_ms,
            page_ms,
            page_rows,
        });
    }
    drop(conn);
    Ok(Report {
        path: "jieba 分词 + 2-gram 补充（并集）",
        size: db_size(&path),
        build_ms,
        rows,
    })
}

pub fn run(segments: usize) -> Result<()> {
    println!("中文短词检索实验");
    println!("================");
    let corpus = build_corpus(segments);
    let total_chars: usize = corpus.iter().map(|t| t.chars().count()).sum();
    println!(
        "语料：{segments} 段虚构日记片段，共 {total_chars} 字（平均 {:.0} 字/段）",
        total_chars as f64 / segments as f64
    );

    let truth: Vec<(&str, usize)> = QUERIES
        .iter()
        .map(|query| (*query, ground_truth(&corpus, query)))
        .collect();
    println!("真值（子串匹配）：{truth:?}");

    let reports = vec![
        run_unicode61(&corpus)?,
        run_trigram(&corpus)?,
        run_ngram(&corpus)?,
        run_jieba(&corpus)?,
    ];

    for report in &reports {
        print_report(report, &truth);
    }

    println!("\n四路径对照");
    println!(
        "  {:<34} {:>9} {:>9} {:>13} {:>13}",
        "路径", "体积", "构建 ms", "二字计数 ms", "二字首屏 ms"
    );
    for report in &reports {
        let two_char = |pick: fn(&Row) -> f64| {
            report
                .rows
                .iter()
                .filter(|row| row.query.chars().count() == 2)
                .map(pick)
                .fold(0.0_f64, f64::max)
        };
        println!(
            "  {:<34} {:>9} {:>9.0} {:>13.3} {:>13.3}",
            report.path,
            human(report.size),
            report.build_ms,
            two_char(|row| row.count_ms),
            two_char(|row| row.page_ms)
        );
    }

    println!("\n召回情况（全量计数与真值一致才算全召回）");
    for report in &reports {
        let full = report
            .rows
            .iter()
            .filter(|row| {
                truth
                    .iter()
                    .find(|(q, _)| *q == row.query)
                    .is_some_and(|(_, expected)| row.hits == *expected)
            })
            .count();
        println!(
            "  {:<34} {}/{} 条查询全召回",
            report.path,
            full,
            report.rows.len()
        );
    }

    // 把结论固化成断言：哪条路径的召回表现变了，CI 必须报出来，
    // 因为选型决定正是建立在这些数字上的。
    let full_count = |path: &str| -> usize {
        reports
            .iter()
            .find(|report| report.path == path)
            .map(|report| {
                report
                    .rows
                    .iter()
                    .filter(|row| {
                        truth
                            .iter()
                            .find(|(q, _)| *q == row.query)
                            .is_some_and(|(_, expected)| row.hits == *expected)
                    })
                    .count()
            })
            .unwrap_or(0)
    };

    anyhow::ensure!(
        full_count("自建 n-gram（2-gram + 1-gram）") == QUERIES.len(),
        "纯 n-gram 应当全召回；表现变化就要重新评估选型"
    );
    anyhow::ensure!(
        full_count("jieba 分词 + 2-gram 补充（并集）") == QUERIES.len(),
        "jieba + 2-gram 并集应当全召回；表现变化就要重新评估选型"
    );
    anyhow::ensure!(
        full_count("FTS5 unicode61") == 0,
        "unicode61 的预期是 0 召回：中文连续文本被当成一个 token。如果它开始能召回，说明语料或实现变了，需要重测"
    );
    anyhow::ensure!(
        full_count("FTS5 trigram") == 1,
        "trigram 的预期是只有三字查询能召回。表现变化就要重测"
    );
    println!("\n断言通过：两条可行路径全召回，unicode61 仍为 0 召回，trigram 仍只覆盖三字查询。");
    Ok(())
}
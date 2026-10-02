//! B3a 的关键词索引测试：召回、幂等、覆盖状态与范围过滤。
//!
//! 语料形态刻意与 M0 探针一致（`tools/probe/src/search.rs`）：同一个子句内的词
//! **不加分隔地连在一起**，只有子句之间有标点。这正是 `unicode61` 会假性通过、
//! 而中文分词真正出问题的形态。如果换成「每个词都用逗号隔开」，这里的断言会
//! 变得毫无意义。

use std::io::Write;
use std::path::Path;
use std::time::Instant;

use diary_core::{
    Core, Coverage, CreateDraftInput, ImportManifest, ImportOrigin, ImportRequest,
};
use sha2::{Digest, Sha256};

fn sha_of(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

/// 把一个文本导入并提取，返回 source_id。
fn import_and_extract(core: &mut Core, name: &str, text: &str, operation_id: &str) -> String {
    let (_, source_id) = import_text(core, name, text, operation_id);
    core.extract_source(&source_id).expect("提取应当成功");
    source_id
}

/// 只导入不提取，返回 (capture_id, source_id)。
fn import_text(core: &mut Core, name: &str, text: &str, operation_id: &str) -> (String, String) {
    import_bytes(core, name, text.as_bytes(), "text/plain", operation_id)
}

fn import_bytes(
    core: &mut Core,
    name: &str,
    bytes: &[u8],
    mime: &str,
    operation_id: &str,
) -> (String, String) {
    let capture_id = core
        .create_draft(CreateDraftInput {
            occurred_at: None,
            time_zone: "Asia/Shanghai",
            utc_offset_minutes: 480,
            operation_id: &format!("{operation_id}-create"),
        })
        .unwrap()
        .id;
    let ticket = core
        .prepare_import(ImportRequest {
            capture_id: &capture_id,
            display_name: name,
            mime_hint: Some(mime),
            size_hint: Some(bytes.len() as i64),
            origin: ImportOrigin::Picker,
            operation_id: &format!("{operation_id}-prepare"),
        })
        .unwrap();
    std::fs::write(&ticket.staging_ticket, bytes).unwrap();
    core.finish_import(
        &ticket.import_id,
        &ticket.staging_ticket,
        ImportManifest {
            copied_bytes: bytes.len() as i64,
            sha256: sha_of(bytes),
            detected_mime: mime.to_owned(),
            original_name: name.to_owned(),
        },
    )
    .unwrap();
    let source_id = core
        .get_capture(&capture_id)
        .unwrap()
        .ordered_source_ids
        .first()
        .cloned()
        .expect("材料应当挂到记录上");
    (capture_id, source_id)
}

fn new_core(dir: &Path) -> Core {
    Core::open_in_memory_at(dir).unwrap()
}

// ------------------------------------------------------------ 语料

const QUERIES: [&str; 5] = ["妈妈", "离职", "面试", "加班", "面试官"];

const WORDS: [&str; 20] = [
    "妈妈", "离职", "面试", "加班", "房东", "疫苗", "体检", "房租", "同事", "项目", "咖啡", "失眠",
    "跑步", "医生", "报告", "合同", "地铁", "天气", "朋友", "计划",
];

/// 确定性伪随机，保证语料可复现（与探针同一套常数）。
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

    fn pick<'a>(&mut self, items: &'a [&'a str]) -> &'a str {
        let index = (self.next() % items.len() as u64) as usize;
        items[index]
    }
}

/// 生成虚构日记片段。返回的每一项就是一个段落（一段 = 一个索引片段）。
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

/// 真值：子串匹配的段落数。召回正确的定义就是与它相等。
fn ground_truth(corpus: &[String], query: &str) -> i64 {
    corpus
        .iter()
        .filter(|text| text.to_lowercase().contains(&query.to_lowercase()))
        .count() as i64
}

/// 把语料拼成一个文本文件：空行分段，所以每个片段是独立段落。
fn corpus_to_text(corpus: &[String]) -> String {
    let mut text = String::new();
    for line in corpus {
        text.push_str(line);
        text.push_str("\n\n");
    }
    text
}

/// 当前进程的常驻内存（KiB）。jieba 词典是懒加载的，这个数字用来看它的代价。
#[cfg(target_os = "linux")]
fn resident_kib() -> u64 {
    let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    status
        .lines()
        .find_map(|line| line.strip_prefix("VmRSS:"))
        .and_then(|value| value.split_whitespace().next())
        .and_then(|value| value.parse().ok())
        .unwrap_or(0)
}

#[cfg(not(target_os = "linux"))]
fn resident_kib() -> u64 {
    0
}

fn write_text_file(dir: &Path, name: &str, text: &str) -> std::path::PathBuf {
    let path = dir.join(name);
    let mut file = std::fs::File::create(&path).unwrap();
    file.write_all(text.as_bytes()).unwrap();
    path
}

// ------------------------------------------------------------ 召回

#[test]
fn one_and_two_character_queries_recall_everything() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    let corpus = build_corpus(400);
    import_and_extract(&mut core, "日记.txt", &corpus_to_text(&corpus), "op1");

    for query in QUERIES {
        let expected = ground_truth(&corpus, query);
        let actual = core.count_search_matches(query).unwrap();
        assert!(expected > 0, "语料里应当有「{query}」，否则这个断言没有意义");
        assert_eq!(
            actual, expected,
            "「{query}」召回不完整：期望 {expected}，实际 {actual}"
        );
    }
}

#[test]
fn short_word_inside_longer_token_is_not_lost() {
    // M0 实测的坑：jieba 把「面试官」切成一个词，查询「面试」在分词索引里漏掉
    // 那部分记录。2-gram 并集就是为它准备的。
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    let corpus = build_corpus(600);
    import_and_extract(&mut core, "日记.txt", &corpus_to_text(&corpus), "op1");

    let with_officer = ground_truth(&corpus, "面试官");
    let with_interview = ground_truth(&corpus, "面试");
    assert!(with_officer > 0, "语料里应当有「面试官」");
    assert!(
        with_interview > with_officer,
        "「面试」应当比「面试官」命中更多（{with_interview} 对 {with_officer}）：\
         如果两者相等，说明语料没造出「面试」出现在其他上下文里的情况"
    );
    assert_eq!(core.count_search_matches("面试").unwrap(), with_interview);
}

#[test]
fn single_character_query_uses_character_index() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    let corpus = build_corpus(300);
    import_and_extract(&mut core, "日记.txt", &corpus_to_text(&corpus), "op1");

    // 「妈」只作为「妈妈」的一部分出现：jieba 不会把它单独切成词，
    // 所以这条断言验证的是单字索引真的建了。
    let expected = ground_truth(&corpus, "妈");
    assert!(expected > 0);
    assert_eq!(core.count_search_matches("妈").unwrap(), expected);
}

#[test]
fn candidates_are_capped_and_stable() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    let corpus = build_corpus(200);
    import_and_extract(&mut core, "日记.txt", &corpus_to_text(&corpus), "op1");

    let first = core.search_candidates("妈妈", 5).unwrap();
    let again = core.search_candidates("妈妈", 5).unwrap();
    assert_eq!(first, again, "同样的查询必须给出同样的候选顺序");
    assert_eq!(first.len(), 5);

    let wider = core.search_candidates("妈妈", 500).unwrap();
    assert!(wider.len() > first.len());
    assert_eq!(
        wider.len() as i64,
        core.count_search_matches("妈妈").unwrap()
    );
}

// ------------------------------------------------------------ 查询文字的处理

#[test]
fn query_is_treated_as_plain_text_not_an_expression() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    let text = "Report 2026 Q3，项目是不写日记。\n\n\
                路径 E:\\dev\\ort-win 和引号 \" 与连字符 - 都在这里。\n\n\
                今天心情不错 🙂，晚上去跑步。\n\n";
    import_and_extract(&mut core, "混合.txt", text, "op1");

    // ASCII 大小写不敏感，首尾空白去掉。
    assert_eq!(core.count_search_matches("report").unwrap(), 1);
    assert_eq!(core.count_search_matches("  REPORT  ").unwrap(), 1);
    assert_eq!(core.count_search_matches("Report 2026 Q3").unwrap(), 1);
    // 路径与标点原样搜索。
    assert_eq!(core.count_search_matches("E:\\dev\\ort-win").unwrap(), 1);
    assert_eq!(core.count_search_matches("\" 与连字符 -").unwrap(), 1);
    // emoji 也能搜。
    assert_eq!(core.count_search_matches("🙂").unwrap(), 1);
    // 只输入空白就是没有查询，不能退化成「匹配所有片段」。
    assert_eq!(core.count_search_matches("   ").unwrap(), 0);
    assert!(core.search_candidates("   ", 10).unwrap().is_empty());
    // 类 FTS 的注入形状：当普通文字处理，命中 0 而不是报错或全命中。
    assert_eq!(core.count_search_matches("\" OR 1=1 --").unwrap(), 0);
    // 不连续的 gram 不能假阳性：两个词都在，但这串字面量不在。
    assert_eq!(core.count_search_matches("报告跑步").unwrap(), 0);
}

#[test]
fn multi_run_query_matches_across_whitespace() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    import_and_extract(
        &mut core,
        "空格.txt",
        "前半段 项目 report 后半段\n\n项目 后半段 紧挨着的一句话\n\n另一段没有那个词\n",
        "op1",
    );

    assert_eq!(core.count_search_matches("项目 report").unwrap(), 1);
    // 最长的一段是 report：候选由它生成，核对仍然对整串做。
    assert_eq!(core.count_search_matches("report 后半段").unwrap(), 1);
    // 跨空白的整串命中：最长的一段用来生成候选，核对仍然对整串做。
    assert_eq!(core.count_search_matches("项目 后半").unwrap(), 1);
}

// ------------------------------------------------------------ 覆盖状态

#[test]
fn extraction_indexes_in_the_same_step_and_status_is_honest() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    let corpus = build_corpus(120);
    import_and_extract(&mut core, "日记.txt", &corpus_to_text(&corpus), "op1");

    let status = core.index_status(None).unwrap();
    assert_eq!(status.coverage, Coverage::Complete);
    assert!(status.keyword_index_ready);
    assert!(status.index_bytes > 0, "索引占用应当能读到：{:?}", status.reasons);
    assert_eq!(status.total_segments, 120);
    assert_eq!(status.indexed_segments, 120);
    assert_eq!(status.pending_segments, 0);
    assert_eq!(status.stale_segments, 0);
    assert_eq!(status.failed_sources, 0);
    assert!(status.index_rows > 0);
    assert_eq!(status.tokenizer_version, diary_core::TOKENIZER_VERSION);

    // 语义索引还没接：状态里必须说得出来，而不是留白让人以为都就绪了。
    assert!(!status.semantic_index_ready);
    assert_eq!(status.model_version, None);
    assert!(
        status.reasons.iter().any(|reason| reason.contains("语义")),
        "reasons 应当说明语义索引还没接：{:?}",
        status.reasons
    );
}

#[test]
fn unextracted_material_is_not_reported_as_searchable() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    import_text(&mut core, "还没提取.txt", "第一段\n\n第二段\n", "op1");

    let status = core.index_status(None).unwrap();
    assert_eq!(status.coverage, Coverage::Unavailable);
    assert!(!status.keyword_index_ready);
    assert_eq!(status.indexed_segments, 0);
    assert_eq!(status.index_rows, 0);
    assert!(
        status.reasons.iter().any(|reason| reason.contains("提取")),
        "reasons 应当说明要先提取：{:?}",
        status.reasons
    );
}

#[test]
fn failed_extraction_is_visible_in_status() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    // 不支持的二进制：提取会失败并落一条 failed 记录。
    let (_, source_id) = import_bytes(
        &mut core,
        "安装包.bin",
        &[0x00, 0x01, 0x02, 0x03, 0xFF],
        "application/octet-stream",
        "op1",
    );
    let content = core.extract_source(&source_id).unwrap();
    assert_eq!(content.status, diary_core::ProcessingStatus::Failed);

    let status = core.index_status(None).unwrap();
    assert_eq!(status.failed_sources, 1, "失败的材料必须在状态里可见");
    assert!(!status.keyword_index_ready);
    assert!(
        status
            .reasons
            .iter()
            .any(|reason| reason.contains("解析失败")),
        "reasons 应当说明有材料解析失败：{:?}",
        status.reasons
    );
}

// ------------------------------------------------------------ 重建与范围

#[test]
fn rebuild_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    let corpus = build_corpus(150);
    import_and_extract(&mut core, "日记.txt", &corpus_to_text(&corpus), "op1");

    let before = core.index_status(None).unwrap();
    let rebuilt = core.rebuild_keyword_index(None).unwrap();
    assert_eq!(rebuilt, before.indexed_segments);

    let after = core.index_status(None).unwrap();
    assert_eq!(after.indexed_segments, before.indexed_segments);
    assert_eq!(
        after.index_rows, before.index_rows,
        "重建不能留下重复行：同一个片段重复索引必须是先清后写"
    );
    assert_eq!(after.index_terms, before.index_terms);
    for query in QUERIES {
        assert_eq!(
            core.count_search_matches(query).unwrap(),
            ground_truth(&corpus, query)
        );
    }
}

#[test]
fn scope_filters_status_and_empty_scope_means_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    let first = import_and_extract(&mut core, "A.txt", "妈妈打电话来，\n\n离职的事定了，\n", "opA");
    let second =
        import_and_extract(&mut core, "B.txt", "面试官问了很多，\n\n加班到很晚，\n", "opB");

    let all = core.index_status(None).unwrap();
    assert_eq!(all.total_segments, 4);

    let only_first = core
        .index_status(Some(std::slice::from_ref(&first)))
        .unwrap();
    assert_eq!(only_first.total_segments, 2);
    assert_eq!(only_first.indexed_segments, 2);
    assert_eq!(only_first.coverage, Coverage::Complete);

    let only_second = core
        .index_status(Some(std::slice::from_ref(&second)))
        .unwrap();
    assert_eq!(only_second.total_segments, 2);

    // 空范围是「什么都不看」，不是「看全部」。
    let empty: [String; 0] = [];
    let none = core.index_status(Some(&empty)).unwrap();
    assert_eq!(none.total_segments, 0);
    assert_eq!(none.index_rows, 0);
    assert_eq!(none.coverage, Coverage::Unavailable);

    // 不该出现的来源 ID 也不会退化成全部。
    let unknown = core
        .index_status(Some(&["src_不存在".to_owned()]))
        .unwrap();
    assert_eq!(unknown.total_segments, 0);

    // 范围重建只重建范围内的来源。
    let rebuilt = core
        .rebuild_keyword_index(Some(std::slice::from_ref(&first)))
        .unwrap();
    assert_eq!(rebuilt, 2);
    assert_eq!(core.count_search_matches("加班").unwrap(), 1, "范围重建不该动别的来源");
}

// ------------------------------------------------------------ 索引不是只在内存里

#[test]
fn index_survives_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.sqlite");
    let corpus = build_corpus(80);
    {
        let mut core = Core::open(&path).unwrap();
        import_and_extract(&mut core, "日记.txt", &corpus_to_text(&corpus), "op1");
    }
    let core = Core::open(&path).unwrap();
    assert_eq!(core.schema_version().unwrap(), diary_core::SCHEMA_VERSION);
    assert_eq!(
        core.count_search_matches("妈妈").unwrap(),
        ground_truth(&corpus, "妈妈")
    );
    let status = core.index_status(None).unwrap();
    assert_eq!(status.coverage, Coverage::Complete);
}

// ------------------------------------------------------------ 十万片段实测

/// 10 万片段实测：**不进 CI**（要几十秒），手动跑：
///
/// ```bash
/// cargo test --release -p diary_core --test search_test -- --ignored --nocapture 十万
/// ```
///
/// 数字与结论记在 `docs/architecture/m1-关键词索引.md`。这一段刻意只打印与断言
/// 召回，不隐藏构建耗时里包含文件写盘与提取的部分——那是产品里真实发生的代价。
#[test]
#[ignore = "10 万片段实测：手动跑，见文档里的复现命令"]
fn hundred_thousand_segments_measurement() {
    const TOTAL: usize = 100_000;
    const FILES: usize = 4;

    // 固定目录：跑完还留着，方便事后用 sqlite3 看查询计划与表占用
    // （`EXPLAIN QUERY PLAN`、`SELECT name, pgsize FROM dbstat`）。
    let dir = std::path::PathBuf::from("/tmp/diary-search-measurement");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("library.sqlite");
    let mut core = Core::open(&path).unwrap();
    let corpus = build_corpus(TOTAL);
    let per_file = TOTAL / FILES;
    println!("实测库留在：{}", path.display());

    let characters: usize = corpus.iter().map(|text| text.chars().count()).sum();
    println!("语料：{TOTAL} 段，共 {characters} 字（平均 {:.0} 字/段）", characters as f64 / TOTAL as f64);

    let rss_before = resident_kib();
    let started = Instant::now();
    for file in 0..FILES {
        let slice = &corpus[file * per_file..(file + 1) * per_file];
        let text = corpus_to_text(slice);
        let bytes_path = write_text_file(&dir, &format!("corpus{file}.txt"), &text);
        // 直接走产品路径：导入 → 提取（同时入索引）。
        let bytes = std::fs::read(&bytes_path).unwrap();
        let capture_id = core
            .create_draft(CreateDraftInput {
                occurred_at: None,
                time_zone: "Asia/Shanghai",
                utc_offset_minutes: 480,
                operation_id: &format!("bulk{file}-create"),
            })
            .unwrap()
            .id;
        let ticket = core
            .prepare_import(ImportRequest {
                capture_id: &capture_id,
                display_name: &format!("corpus{file}.txt"),
                mime_hint: Some("text/plain"),
                size_hint: Some(bytes.len() as i64),
                origin: ImportOrigin::Picker,
                operation_id: &format!("bulk{file}-prepare"),
            })
            .unwrap();
        std::fs::copy(&bytes_path, &ticket.staging_ticket).unwrap();
        core.finish_import(
            &ticket.import_id,
            &ticket.staging_ticket,
            ImportManifest {
                copied_bytes: bytes.len() as i64,
                sha256: sha_of(&bytes),
                detected_mime: "text/plain".to_owned(),
                original_name: format!("corpus{file}.txt"),
            },
        )
        .unwrap();
        let source_id = core
            .get_capture(&capture_id)
            .unwrap()
            .ordered_source_ids
            .first()
            .cloned()
            .unwrap();
        core.extract_source(&source_id).unwrap();
    }
    let build_ms = started.elapsed().as_secs_f64() * 1000.0;

    let rss_after = resident_kib();
    let status = core.index_status(None).unwrap();
    assert_eq!(status.total_segments, TOTAL as i64);
    assert_eq!(status.indexed_segments, TOTAL as i64);
    assert_eq!(status.coverage, Coverage::Complete);

    let db_bytes = std::fs::metadata(&path).unwrap().len();
    println!(
        "导入+提取+建索引：{:.0} ms；库文件 {:.1} MiB（含原件文本与派生内容，不能直接和 M0 的纯索引库比）",
        build_ms,
        db_bytes as f64 / (1024.0 * 1024.0)
    );
    println!(
        "索引表占用 {:.1} MiB；索引行 {}；不同词项 {}；每源字符 {:.1} 字节",
        status.index_bytes as f64 / (1024.0 * 1024.0),
        status.index_rows,
        status.index_terms,
        status.index_bytes as f64 / status.indexed_chars as f64
    );
    println!(
        "M0 探针的 jieba+2-gram 方案是 42 字节/源字符（那是纯索引库，这一行才是可比的数字）"
    );
    // 查询计划也打出来：数字变化时能立刻看出是计划变了还是数据变了。
    {
        let conn = rusqlite::Connection::open_with_flags(
            &path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        let sql = "SELECT COUNT(*) FROM search_grams g \
                   CROSS JOIN search_docs d ON d.doc_id = g.doc_id \
                   CROSS JOIN extracted_segments s ON s.id = d.segment_id \
                   WHERE g.term = ?1 AND instr(lower(s.text), ?2) > 0";
        let mut statement = conn.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
        let plan: Vec<String> = statement
            .query_map(rusqlite::params!["妈妈", "妈妈"], |row| row.get::<_, String>(3))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        println!("计数查询计划：{}", plan.join(" | "));
    }

    println!(
        "进程常驻内存：{} MiB → {} MiB（jieba 词典是懒加载的，第一次建索引才付这个代价）",
        rss_before / 1024,
        rss_after / 1024
    );

    for query in QUERIES {
        let expected = ground_truth(&corpus, query);
        let actual = core.count_search_matches(query).unwrap();
        assert_eq!(actual, expected, "「{query}」召回不完整");

        // 首屏：预热一次，再取多次的中位数。
        let _ = core.search_candidates(query, 20).unwrap();
        let mut samples = Vec::new();
        for _ in 0..20 {
            let start = Instant::now();
            let page = core.search_candidates(query, 20).unwrap();
            samples.push(start.elapsed().as_secs_f64() * 1000.0);
            assert_eq!(page.len(), 20);
        }
        samples.sort_by(|a, b| a.partial_cmp(b).unwrap());

        // 全量计数是最坏情况：产品路径翻页不需要它（快照带游标），
        // 但测试与实测拿它核对召回，代价要如实量出来。取 3 次里最快的一次。
        let counted = core.count_search_matches(query).unwrap();
        assert_eq!(counted, expected);
        let mut count_samples = Vec::new();
        for _ in 0..3 {
            let start = Instant::now();
            let _ = core.count_search_matches(query).unwrap();
            count_samples.push(start.elapsed().as_secs_f64() * 1000.0);
        }
        count_samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let count_ms = count_samples[0];

        println!(
            "「{query}」命中 {actual}/{expected}（全召回）· 首屏 20 条中位 {:.3} ms · 全量计数 {:.0} ms",
            samples[samples.len() / 2],
            count_ms
        );
    }
}
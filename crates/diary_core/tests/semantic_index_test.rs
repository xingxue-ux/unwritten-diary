//! B3c-2（后半）：真实向量接入、换代、语义检索。
//!
//! 这一片的验收分两半：
//!
//! - **CI 里能跑的那一半**（这个文件里除 `#[ignore]` 之外的全部）：没有模型、没有
//!   ORT 动态库。退化路径（关键词照旧、`Semantic` 空结果 + 警告、`build_semantic_index`
//!   诚实报错且不改状态）走真环境；换代与检索逻辑用**注入的确定性 stub 嵌入器**
//!   跑通——否则「开代次 → 写向量 → 原子切换 → 没变的块复用向量」这条路上没有一行
//!   代码能在 CI 里被验到；
//! - **需要真模型的那一半**：`#[ignore]` 标出，复现命令写在理由里。
//!
//! 风格照 `text_chunks_test.rs`：真库、真导入、断言带 reason。

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use chrono::{DateTime, TimeZone, Utc};
use diary_core::{
    Core, CreateDraftInput, EmbedOptions, Embedder, ImportManifest, ImportOrigin, ImportRequest,
    SearchFilters, SearchHit, SearchMode, SearchRequest, SearchSnapshot, SourceKind,
};
use rusqlite::params;
use sha2::{Digest, Sha256};

// ------------------------------------------------------------ 测试用 stub 嵌入器

/// 调用次数：用来钉住「没变的块不重复算」。
#[derive(Default)]
struct Counters {
    documents: AtomicUsize,
    queries: AtomicUsize,
}

/// 确定性 stub：命中了规则就用规则给的向量，否则把文本哈希成向量。
///
/// 用规则（而不是纯哈希）是为了让测试**能控制相似度**：把查询与某篇正文都映射到
/// 同一个单位向量，它们的余弦就是 1。纯哈希没有语义，排序无从断言。
struct StubEmbedder {
    model_version: String,
    dims: usize,
    counters: Arc<Counters>,
    rules: Vec<(&'static str, Vec<f32>)>,
}

impl StubEmbedder {
    fn new(model_version: &str, counters: &Arc<Counters>, rules: Vec<(&'static str, Vec<f32>)>) -> Self {
        Self {
            model_version: model_version.to_owned(),
            dims: 512,
            counters: Arc::clone(counters),
            rules,
        }
    }

    fn vector_for(&self, text: &str) -> Vec<f32> {
        for (needle, vector) in &self.rules {
            if text.contains(needle) {
                return vector.clone();
            }
        }
        diary_core::text_hash_vector(text, self.dims)
    }
}

impl Embedder for StubEmbedder {
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
        self.counters.documents.fetch_add(1, Ordering::SeqCst);
        Ok(self.vector_for(text))
    }

    fn embed_query(&self, text: &str) -> diary_core::Result<Vec<f32>> {
        self.counters.queries.fetch_add(1, Ordering::SeqCst);
        Ok(self.vector_for(text))
    }
}

/// 512 维的单位向量：只有第 `index` 位是 1。
fn unit(index: usize) -> Vec<f32> {
    let mut vector = vec![0_f32; 512];
    vector[index] = 1.0;
    vector
}

fn stub(counters: &Arc<Counters>, rules: Vec<(&'static str, Vec<f32>)>) -> Box<dyn Embedder> {
    Box::new(StubEmbedder::new("stub-embedder@000000000000", counters, rules))
}

// ------------------------------------------------------------ 通用工具

fn sha_of(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

fn new_core(dir: &Path) -> Core {
    Core::open(dir.join("library.sqlite")).unwrap()
}

fn at(year: i32, month: u32, day: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(year, month, day, 4, 0, 0).unwrap()
}

fn save(core: &mut Core, capture_id: &str, text: &str, operation: &str) {
    let revision = core.get_capture(capture_id).unwrap().revision;
    core.save_draft(capture_id, text, revision, operation).unwrap();
}

fn write_capture(core: &mut Core, day: DateTime<Utc>, text: &str, operation: &str) -> String {
    let capture_id = core
        .create_draft(CreateDraftInput {
            occurred_at: Some(day),
            time_zone: "Asia/Shanghai",
            utc_offset_minutes: 480,
            operation_id: &format!("{operation}-create"),
        })
        .unwrap()
        .id;
    if !text.is_empty() {
        save(core, &capture_id, text, &format!("{operation}-save"));
    }
    capture_id
}

fn import_and_extract(
    core: &mut Core,
    capture_id: &str,
    name: &str,
    text: &str,
    operation: &str,
) -> String {
    let bytes = text.as_bytes();
    let ticket = core
        .prepare_import(ImportRequest {
            capture_id,
            display_name: name,
            mime_hint: Some("text/plain"),
            size_hint: Some(bytes.len() as i64),
            origin: ImportOrigin::Picker,
            operation_id: &format!("{operation}-prepare"),
        })
        .unwrap();
    std::fs::write(&ticket.staging_ticket, bytes).unwrap();
    core.finish_import(
        &ticket.import_id,
        &ticket.staging_ticket,
        ImportManifest {
            copied_bytes: bytes.len() as i64,
            sha256: sha_of(bytes),
            detected_mime: "text/plain".to_owned(),
            original_name: name.to_owned(),
        },
    )
    .unwrap();
    let source_id = core
        .get_capture(capture_id)
        .unwrap()
        .ordered_source_ids
        .last()
        .cloned()
        .expect("导入的材料应当挂到记录上");
    core.extract_source(&source_id).unwrap();
    source_id
}

fn search_mode(core: &mut Core, query: &str, mode: SearchMode, page_size: i64) -> SearchSnapshot {
    core.start_search(
        SearchRequest {
            query: query.to_owned(),
            mode,
            filters: SearchFilters::default(),
            page_size,
        },
        1,
    )
    .unwrap()
}

fn open(path: &Path) -> rusqlite::Connection {
    rusqlite::Connection::open(path).unwrap()
}

fn count(conn: &rusqlite::Connection, sql: &str, table: &str) -> i64 {
    conn.query_row(sql, [], |row| row.get(0))
        .unwrap_or_else(|error| panic!("查 {table} 的计数失败：{error}"))
}

fn hit_of<'a>(snapshot: &'a SearchSnapshot, group_id: &str) -> &'a SearchHit {
    snapshot
        .results
        .iter()
        .find(|hit| hit.group_id == group_id)
        .unwrap_or_else(|| {
            panic!(
                "结果里没有 {group_id}：{:?}",
                snapshot
                    .results
                    .iter()
                    .map(|hit| (&hit.group_id, &hit.snippet))
                    .collect::<Vec<_>>()
            )
        })
}

/// 这个进程配了本地模型吗？配了就跳过「没有模型」的用例——CI 上没配，它们会跑。
fn model_configured() -> bool {
    std::env::var_os("DIARY_MODEL_DIR").is_some()
        || std::env::var_os("DIARY_ORT_DYLIB").is_some()
        || std::env::var_os("ORT_DYLIB_PATH").is_some()
}

fn skip_without_a_model(test: &str) -> bool {
    if model_configured() {
        eprintln!("跳过 {test}：这个进程配了本地模型，跑不到「没有模型」那条路");
        true
    } else {
        false
    }
}

// ------------------------------------------------------------ 退化路径（CI 就跑这条）

/// 没有模型：关键词照旧，语义未就绪且**说得出原因**，`Semantic` 模式空结果 + 警告。
#[test]
fn without_a_model_keywords_keep_working_and_semantics_says_why() {
    if skip_without_a_model("without_a_model_keywords_keep_working_and_semantics_says_why") {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    let a = write_capture(&mut core, at(2026, 9, 20), "妈妈打电话来。", "opA");
    import_and_extract(&mut core, &a, "信.txt", "妈妈后来又写了一封信。\n", "opA-mat");

    // 关键词那一路不受影响。
    let keyword = search_mode(&mut core, "妈妈", SearchMode::Keyword, 20);
    assert_eq!(keyword.results.len(), 2, "记录文字与派生片段各命中一条");

    let status = core.index_status(None).unwrap();
    assert!(!status.semantic_index_ready, "没有模型就没有生效代次");
    assert_eq!(status.model_version, None, "没有生效代次就没有模型版本可报");
    assert!(
        status
            .reasons
            .iter()
            .any(|reason| reason.contains("还没有配置本地模型")),
        "要说清是「没配」而不是含糊的「未就绪」：{:?}",
        status.reasons
    );

    // 语义模式：空结果 + 明确警告，**不退化成关键词**。
    let semantic = search_mode(&mut core, "妈妈", SearchMode::Semantic, 20);
    assert!(
        semantic.results.is_empty(),
        "没有生效代次时语义必须空结果，不能拿关键词结果冒充：{:?}",
        semantic.results
    );
    assert!(
        semantic
            .warnings
            .iter()
            .any(|warning| warning.contains("语义索引还没有生效的代次")),
        "要明确说语义为什么没有结果：{:?}",
        semantic.warnings
    );

    // hybrid 仍然如实降级到关键词并给提示（#51 之前的行为）。
    let hybrid = search_mode(&mut core, "妈妈", SearchMode::Hybrid, 20);
    assert_eq!(hybrid.results.len(), 2, "混合如实降级到关键词");
    assert!(
        hybrid.warnings.iter().any(|warning| warning.contains("混合检索还没实现")),
        "{:?}",
        hybrid.warnings
    );
}

/// 模型缺失时建语义索引：**诚实的错误、什么都不改**（连块都不重建）。
#[test]
fn building_without_a_model_errors_honestly_and_changes_nothing() {
    if skip_without_a_model("building_without_a_model_errors_honestly_and_changes_nothing") {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.sqlite");
    let mut core = new_core(dir.path());
    write_capture(&mut core, at(2026, 9, 20), "妈妈打电话来。", "opA");

    let error = core.build_semantic_index(None).unwrap_err();
    assert_eq!(
        error.code(),
        diary_core::ErrorCode::InvalidState,
        "模型缺失不是数据坏了，是当前状态不允许"
    );
    let message = error.to_string();
    assert!(
        message.contains("DIARY_MODEL_DIR"),
        "错误里要说清缺什么：{message}"
    );

    // 状态没被改：没有块、没有代次、没有向量。
    let status = core.index_status(None).unwrap();
    assert_eq!(status.total_chunks, 0, "失败时连块都不该重建");
    assert_eq!(status.embedded_chunks, 0);
    assert!(!status.semantic_index_ready);
    let conn = open(&path);
    let (active, building): (Option<i64>, Option<i64>) = conn
        .query_row(
            "SELECT active_generation, building_generation FROM index_meta WHERE id = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!((active, building), (None, None), "失败不该留下半个代次");

    // 关键词索引照旧可用；块也能单独重建（两条路互不影响）。
    assert_eq!(search_mode(&mut core, "妈妈", SearchMode::Keyword, 20).results.len(), 1);
    assert!(core.rebuild_text_chunks(None).unwrap() > 0);
    assert!(core.build_semantic_index(None).is_err(), "仍然没有模型");
    let conn = open(&path);
    let (active, building): (Option<i64>, Option<i64>) = conn
        .query_row(
            "SELECT active_generation, building_generation FROM index_meta WHERE id = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!((active, building), (None, None), "块建好了也不该动代次");
}

// ------------------------------------------------------------ 换代（stub 嵌入器）

/// 开代次 → 写向量 → 原子切换 → 状态/计数正确；重建后**没变的块不重复算**。
#[test]
fn building_opens_a_generation_and_reuses_unchanged_chunks() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.sqlite");
    let mut core = new_core(dir.path());
    let counters = Arc::new(Counters::default());
    core.set_embedder(stub(&counters, vec![]));

    write_capture(&mut core, at(2026, 9, 20), "妈妈打电话来。", "opA");
    write_capture(&mut core, at(2026, 9, 21), "路边的桂花开了。", "opB");

    let first = core.build_semantic_index(None).unwrap();
    assert_eq!(first.generation, 1, "第一代从 1 开始");
    assert_eq!(first.total_chunks, 2, "两天各一块");
    assert_eq!(first.embedded_chunks, 2, "两块都算了向量");
    assert_eq!(first.reused_chunks, 0, "没有上一代可复用");
    assert_eq!(counters.documents.load(Ordering::SeqCst), 2);

    let status = core.index_status(None).unwrap();
    assert!(status.semantic_index_ready, "生效代次覆盖了全部块");
    assert_eq!(status.embedded_chunks, 2);
    assert_eq!(
        status.model_version.as_deref(),
        Some("stub-embedder@000000000000"),
        "状态里报的是生效代次那一批向量的真值"
    );

    let conn = open(&path);
    let (active, building): (Option<i64>, Option<i64>) = conn
        .query_row(
            "SELECT active_generation, building_generation FROM index_meta WHERE id = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!((active, building), (Some(1), None), "切换之后 building 要置空");
    assert_eq!(
        count(&conn, "SELECT COUNT(*) FROM chunk_vectors", "chunk_vectors"),
        2
    );

    // 内容没变再建一次：新代次，但向量是**复制**来的，一次都没重算。
    let calls_before = counters.documents.load(Ordering::SeqCst);
    let second = core.build_semantic_index(None).unwrap();
    assert_eq!(second.generation, 2);
    assert_eq!(second.reused_chunks, 2, "内容没变的块从上一代整批复制");
    assert_eq!(second.embedded_chunks, 0, "没有一块需要重算");
    assert_eq!(
        counters.documents.load(Ordering::SeqCst),
        calls_before,
        "重建同一个库不该再调模型"
    );
    let conn = open(&path);
    let (active, building): (Option<i64>, Option<i64>) = conn
        .query_row(
            "SELECT active_generation, building_generation FROM index_meta WHERE id = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!((active, building), (Some(2), None));
    assert_eq!(
        count(&conn, "SELECT COUNT(*) FROM chunk_vectors", "chunk_vectors"),
        2,
        "切换时清掉旧代次：存储不会每建一次就翻倍"
    );
    assert_eq!(
        count(
            &conn,
            "SELECT COUNT(*) FROM chunk_vectors WHERE generation <> 2",
            "chunk_vectors"
        ),
        0,
        "只留生效代次"
    );

    // 改掉一天的正文：那一块换 id，只有它需要重算。
    let capture_id = core
        .get_capture(
            &open(&path)
                .query_row("SELECT id FROM captures ORDER BY day_key", [], |row| {
                    row.get::<_, String>(0)
                })
                .unwrap(),
        )
        .map(|capture| capture.id)
        .unwrap();
    let revision = core.get_capture(&capture_id).unwrap().revision;
    core.save_draft(&capture_id, "第二版：同事约我周末去爬山。", revision, "opA-2")
        .unwrap();
    let calls_before = counters.documents.load(Ordering::SeqCst);
    let third = core.build_semantic_index(None).unwrap();
    assert_eq!(third.generation, 3);
    assert_eq!(third.total_chunks, 2, "还是两块");
    assert_eq!(third.reused_chunks, 1, "没变的那一天复用");
    assert_eq!(third.embedded_chunks, 1, "变了的块才重算");
    assert_eq!(counters.documents.load(Ordering::SeqCst), calls_before + 1);
    assert!(core.index_status(None).unwrap().semantic_index_ready);
}

/// 换了模型（`model_version` 变）必须**整批重算**，不能把新旧向量混在一张表里。
#[test]
fn a_new_model_version_forces_a_full_recompute() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.sqlite");
    let mut core = new_core(dir.path());
    let counters = Arc::new(Counters::default());
    core.set_embedder(stub(&counters, vec![]));
    write_capture(&mut core, at(2026, 9, 20), "妈妈打电话来。", "opA");
    write_capture(&mut core, at(2026, 9, 21), "路边的桂花开了。", "opB");
    core.build_semantic_index(None).unwrap();

    // 换成「另一个模型」：口径不一致，复用必须为 0。
    core.set_embedder(Box::new(StubEmbedder::new(
        "stub-embedder@ffffffffffff",
        &counters,
        vec![],
    )));
    let report = core.build_semantic_index(None).unwrap();
    assert_eq!(report.generation, 2);
    assert_eq!(report.reused_chunks, 0, "模型换了就不能复用旧向量");
    assert_eq!(report.embedded_chunks, 2);
    let conn = open(&path);
    assert_eq!(
        count(
            &conn,
            "SELECT COUNT(*) FROM chunk_vectors WHERE model_version = 'stub-embedder@ffffffffffff'",
            "chunk_vectors"
        ),
        2
    );
    assert_eq!(
        count(&conn, "SELECT COUNT(*) FROM chunk_vectors", "chunk_vectors"),
        2,
        "旧模型的向量随换代清掉"
    );
    assert_eq!(
        core.index_status(None).unwrap().model_version.as_deref(),
        Some("stub-embedder@ffffffffffff")
    );
}

/// 中断的换代：`building_generation` 留着，下一轮从它续跑，**没有半成品被激活**。
#[test]
fn an_interrupted_generation_resumes_without_activating_half_a_build() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.sqlite");
    let mut core = new_core(dir.path());
    let counters = Arc::new(Counters::default());
    core.set_embedder(stub(&counters, vec![]));
    write_capture(&mut core, at(2026, 9, 20), "妈妈打电话来。", "opA");
    write_capture(&mut core, at(2026, 9, 21), "路边的桂花开了。", "opB");
    core.build_semantic_index(None).unwrap();

    // 模拟「上一轮开到第 2 代、只写了其中一块就中断了」。
    {
        let conn = open(&path);
        let chunk_id: String = conn
            .query_row("SELECT chunk_id FROM text_chunks ORDER BY day_key LIMIT 1", [], |row| {
                row.get(0)
            })
            .unwrap();
        conn.execute_batch(&format!(
            "INSERT INTO chunk_vectors (chunk_id, generation, model_version, dims, storage, \
                 vector, created_at) VALUES ('{chunk_id}', 2, 'stub-embedder@000000000000', 512, \
                 'f32', zeroblob(2048), '2026-10-10T00:00:00.000Z');\
             UPDATE index_meta SET building_generation = 2;"
        ))
        .unwrap();
    }
    // 中断期间生效代次还是 1：语义检索继续用旧的那一代，不会突然空掉。
    let status = core.index_status(None).unwrap();
    assert_eq!(status.embedded_chunks, 2, "旧代次还在服务");
    assert!(status.semantic_index_ready);

    let report = core.build_semantic_index(None).unwrap();
    assert_eq!(report.generation, 2, "从留下的 building_generation 续跑");
    assert_eq!(report.reused_chunks, 1, "另一块从上一代复制");
    assert_eq!(report.resumed_chunks, 1, "中断前写好的那一块算「续上」");
    assert_eq!(report.embedded_chunks, 0, "没有一块需要重算");
    let conn = open(&path);
    let (active, building): (Option<i64>, Option<i64>) = conn
        .query_row(
            "SELECT active_generation, building_generation FROM index_meta WHERE id = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!((active, building), (Some(2), None));
    assert!(core.index_status(None).unwrap().semantic_index_ready);
}

// ------------------------------------------------------------ 消费契约

/// 命中按**篇**上报：合块的多篇各自成条，`snippet`/`locator` 取该篇自己那一段。
#[test]
fn semantic_hits_are_per_piece_and_snippets_come_from_that_pieces_span() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.sqlite");
    let mut core = new_core(dir.path());
    let counters = Arc::new(Counters::default());
    core.set_embedder(stub(&counters, vec![("失眠", unit(0)), ("睡不着", unit(0))]));

    // 同一天两篇短来源：按 B3c-1 的打包规则会合进**一块**（两个 span）。
    let a = write_capture(&mut core, at(2026, 9, 20), "", "opA");
    let first = import_and_extract(
        &mut core,
        &a,
        "睡眠.txt",
        "连续几天失眠，凌晨两点还醒着。\n",
        "opA-mat1",
    );
    let second = import_and_extract(&mut core, &a, "快递.txt", "快递放在门口。\n", "opA-mat2");
    // 另一天：记录自己写的文字，语义这一路也必须搜得到。
    let b = write_capture(&mut core, at(2026, 9, 21), "我最近总是睡不着。", "opB");

    let built = core.build_semantic_index(None).unwrap();
    assert_eq!(built.total_chunks, 2, "第一天合成一块、第二天一块");
    let conn = open(&path);
    let multi: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM (SELECT chunk_id FROM chunk_spans GROUP BY chunk_id \
             HAVING COUNT(*) > 1)",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(multi, 1, "前提：第一天的那一块真的含两篇");

    let snapshot = search_mode(&mut core, "失眠", SearchMode::Semantic, 20);
    assert_eq!(snapshot.results.len(), 3, "两篇来源 + 一条记录文字，各成一条");
    assert_eq!(snapshot.phase, diary_core::SearchPhase::Done);
    assert_eq!(snapshot.index_coverage, diary_core::Coverage::Complete);

    // 合块里的第一篇：摘录是它自己那一段，**不含**同块另一篇的正文。
    let sleep = hit_of(&snapshot, &first);
    assert_eq!(sleep.hit_id, first, "命中 id 按篇给，不按证据块");
    assert_eq!(sleep.matched_by, vec![diary_core::MatchedBy::Semantic]);
    assert_eq!(sleep.source_kind, SourceKind::File);
    assert_eq!(sleep.source_id.as_deref(), Some(first.as_str()));
    assert!(sleep.revision_id.is_some(), "来源篇要带上修订");
    assert_eq!(
        sleep.snippet.as_deref(),
        Some("连续几天失眠，凌晨两点还醒着。"),
        "摘录必须是这一篇自己的那一段"
    );
    assert!(
        !sleep.snippet.as_deref().unwrap().contains("快递"),
        "不能把整块拼接文本给用户看"
    );
    assert_eq!(sleep.title.as_deref(), Some("睡眠.txt"), "标题取来源修订的原件名");
    let locator = sleep.locator.as_ref().expect("来源篇要有定位");
    assert_eq!(locator.source_revision_id, sleep.revision_id.clone().unwrap());

    // 同块的第二篇：它也拿到这一块的相似度（契约：合块的相似度对各篇共享），
    // 但摘录是它自己那一段。
    let express = hit_of(&snapshot, &second);
    assert_eq!(express.source_kind, SourceKind::File);
    assert_eq!(
        express.snippet.as_deref(),
        Some("快递放在门口。"),
        "每一篇各拿自己那一段"
    );
    assert!(
        !express.snippet.as_deref().unwrap().contains("失眠"),
        "不能串到同块别的篇的正文"
    );

    // 记录自己写的文字：与关键词那一路同一个口径（groupId = capture_id）。
    let capture_hit = hit_of(&snapshot, &b);
    assert_eq!(capture_hit.hit_id, format!("cap_{b}"));
    assert_eq!(capture_hit.source_id, None);
    assert_eq!(capture_hit.revision_id, None);
    assert_eq!(capture_hit.source_kind, SourceKind::Text);
    assert_eq!(capture_hit.snippet.as_deref(), Some("我最近总是睡不着。"));
    assert_eq!(capture_hit.locator, None, "记录文字没有原件可定位");
}

/// 同一篇有多个块命中时**折叠成一条**，取得分最高的块当证据。
#[test]
fn multiple_chunks_of_one_piece_fold_into_a_single_hit() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    let counters = Arc::new(Counters::default());
    // 两条规则：全部段落都含「★」（用来验证折叠），只有中段含「失眠」
    // （用来验证「取得分最高的块当证据」）。
    core.set_embedder(stub(
        &counters,
        vec![("失眠", unit(0)), ("★", unit(3))],
    ));

    // 一篇超过上限的长来源：篇内按段尾切，切成多块。
    let mut text = String::new();
    for index in 0..24 {
        if index == 12 {
            text.push_str("第12段 ★：这一段说的是连续几天失眠，凌晨两点还醒着。\n");
        } else {
            text.push_str(&format!(
                "第{index}段 ★：今天天气不错，路上人很多，顺便买了点水果回家。\n"
            ));
        }
    }
    let capture_id = write_capture(&mut core, at(2026, 9, 20), "", "op");
    let source_id = import_and_extract(&mut core, &capture_id, "长文.txt", &text, "op-mat");
    // 另一篇短正文，让结果里不止一篇。
    let other = write_capture(&mut core, at(2026, 9, 21), "★ 快递放在门口。", "op2");

    let built = core.build_semantic_index(None).unwrap();
    assert!(
        built.total_chunks >= 3,
        "前提：长文被切成多块（实际 {} 块）",
        built.total_chunks
    );

    // 所有块都命中（每段都含 ★），但长文只该出**一条**：折叠发生在分页之前。
    let snapshot = search_mode(&mut core, "★", SearchMode::Semantic, 20);
    assert_eq!(
        snapshot.results.len(),
        2,
        "长文的多个块必须折叠成一条（块数 {}）",
        built.total_chunks
    );
    assert!(snapshot.results.iter().any(|hit| hit.group_id == source_id));
    assert!(snapshot.results.iter().any(|hit| hit.group_id == other));

    // 「失眠」那一块（含重叠，可能两块都含）得分最高，被留下的证据块必须是**这一篇**的一段：
    // 摘录按契约取「该 span 起点起的若干字」，所以**不能**要求摘录里一定出现查询词
    // （语义检索没有词面匹配，摘要从段首截断时就看不到深处的词）。这里断言契约给的保证：
    //   1. 仍然只有一条（折叠在分页之前）；
    //   2. 摘录来自这一篇正文（原文里有它，且不是另一篇的文本）。
    let insomnia = search_mode(&mut core, "失眠", SearchMode::Semantic, 20);
    assert_eq!(insomnia.results.len(), 2, "折叠后仍应是两篇各一条");
    let hit = hit_of(&insomnia, &source_id);
    let snippet = hit.snippet.as_deref().unwrap_or_default();
    assert!(!snippet.is_empty(), "证据段必须给出摘录");
    assert!(
        text.contains(snippet.trim_matches('…')),
        "摘录必须来自这一篇正文：{snippet:?}"
    );
    assert!(
        !snippet.contains("快递"),
        "不能把另一篇（短正文）的文本当这一篇的摘录：{snippet:?}"
    );
}

/// 会话机制（分页 / 游标归属 / 快照 / 取消 / 过期）在语义这一路同样成立。
#[test]
fn semantic_sessions_page_expire_and_cancel_like_keyword_ones() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.sqlite");
    let mut core = new_core(dir.path());
    let counters = Arc::new(Counters::default());
    // 所有正文都含「★」，查询也含「★」：每篇得分都是 1，排序只由日期决定（确定）。
    core.set_embedder(stub(&counters, vec![("★", unit(3))]));
    for day in 10..15 {
        write_capture(
            &mut core,
            at(2026, 9, day),
            &format!("九月{day}日 ★ 记一笔。"),
            &format!("op{day}"),
        );
    }
    core.build_semantic_index(None).unwrap();

    let first = search_mode(&mut core, "★", SearchMode::Semantic, 2);
    assert_eq!(first.results.len(), 2);
    assert_eq!(first.phase, diary_core::SearchPhase::Done);
    assert!(first.cursor.is_some(), "还有下一页");
    let first_ids: Vec<String> = first.results.iter().map(|hit| hit.hit_id.clone()).collect();

    // 快照能原样再给一次当前页。
    let snapshot = core.search_snapshot(&first.session_id).unwrap();
    assert_eq!(
        snapshot.results.iter().map(|hit| hit.hit_id.clone()).collect::<Vec<_>>(),
        first_ids
    );

    // 拿别的会话的游标来翻 → cursor_expired。
    let other = search_mode(&mut core, "★", SearchMode::Semantic, 2);
    let foreign = format!("{}:{}", other.session_id, 2);
    let error = core
        .search_next_page(&first.session_id, Some(&foreign))
        .unwrap_err();
    assert_eq!(error.code(), diary_core::ErrorCode::CursorExpired);

    // 翻到底：不重不漏，末页之后再翻给空页。
    let mut seen = first_ids.clone();
    let mut current = first;
    while let Some(cursor) = current.cursor.clone() {
        current = core.search_next_page(&current.session_id, Some(&cursor)).unwrap();
        seen.extend(current.results.iter().map(|hit| hit.hit_id.clone()));
    }
    assert_eq!(seen.len(), 5, "5 篇各一条，不重不漏");
    let mut unique = seen.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), 5);
    let last_session = current.session_id.clone();
    let empty = core.search_next_page(&last_session, None).unwrap();
    assert!(empty.results.is_empty(), "末页之后再翻给空页");
    assert_eq!(empty.cursor, None);

    // 换代会让语义会话过期（排序快照已经不作数了）。
    let stale = search_mode(&mut core, "★", SearchMode::Semantic, 2);
    core.build_semantic_index(None).unwrap();
    let error = core.search_next_page(&stale.session_id, None).unwrap_err();
    assert_eq!(error.code(), diary_core::ErrorCode::SearchExpired);

    // 取消：phase 变 cancelled，结果仍可读，再翻页报 invalid_state。
    let session = search_mode(&mut core, "★", SearchMode::Semantic, 2);
    let cancelled = core.cancel_search(&session.session_id).unwrap();
    assert_eq!(cancelled.phase, diary_core::SearchPhase::Cancelled);
    assert_eq!(cancelled.results.len(), 2, "已返回的结果仍可读");
    let error = core.search_next_page(&session.session_id, None).unwrap_err();
    assert_eq!(error.code(), diary_core::ErrorCode::InvalidState);
    let _ = path;
}

/// 过滤条件（日期 / 类型 / 来源范围 / 回收站）在语义这一路与关键词同一套语义。
#[test]
fn semantic_search_honours_the_same_filters() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    let counters = Arc::new(Counters::default());
    core.set_embedder(stub(&counters, vec![("★", unit(1))]));
    let old = write_capture(&mut core, at(2026, 8, 10), "八月十日 ★。", "opOld");
    let new = write_capture(&mut core, at(2026, 9, 20), "九月二十日 ★。", "opNew");
    core.build_semantic_index(None).unwrap();

    let filters = |from: Option<&str>, to: Option<&str>| SearchFilters {
        from_day_key: from.map(str::to_owned),
        to_day_key: to.map(str::to_owned),
        ..Default::default()
    };
    let all = core
        .start_search(
            SearchRequest {
                query: "★".to_owned(),
                mode: SearchMode::Semantic,
                filters: SearchFilters::default(),
                page_size: 20,
            },
            1,
        )
        .unwrap();
    assert_eq!(all.results.len(), 2);

    let only_old = core
        .start_search(
            SearchRequest {
                query: "★".to_owned(),
                mode: SearchMode::Semantic,
                filters: filters(None, Some("2026-08-31")),
                page_size: 20,
            },
            2,
        )
        .unwrap();
    assert_eq!(only_old.results.len(), 1);
    assert_eq!(only_old.results[0].group_id, old);

    let only_new = core
        .start_search(
            SearchRequest {
                query: "★".to_owned(),
                mode: SearchMode::Semantic,
                filters: filters(Some("2026-09-01"), None),
                page_size: 20,
            },
            3,
        )
        .unwrap();
    assert_eq!(only_new.results.len(), 1);
    assert_eq!(only_new.results[0].group_id, new);

    // kinds 过滤：语义这一路也照同一条规则下到 SQL。
    let no_images = core
        .start_search(
            SearchRequest {
                query: "★".to_owned(),
                mode: SearchMode::Semantic,
                filters: SearchFilters {
                    kinds: vec![SourceKind::Image],
                    ..Default::default()
                },
                page_size: 20,
            },
            4,
        )
        .unwrap();
    assert!(no_images.results.is_empty(), "今天只有 text 这一种篇");

    // 来源范围：空范围是「什么都不看」，不是「看全部」。
    let empty_scope = core
        .start_search(
            SearchRequest {
                query: "★".to_owned(),
                mode: SearchMode::Semantic,
                filters: SearchFilters {
                    source_scope: Some(Vec::new()),
                    ..Default::default()
                },
                page_size: 20,
            },
            5,
        )
        .unwrap();
    assert!(empty_scope.results.is_empty(), "空范围不能退化成看全部");

    // 回收站：默认不看，开关打开才看。
    let conn = open(&dir.path().join("library.sqlite"));
    conn.execute("UPDATE captures SET state = 'trashed' WHERE id = ?1", params![old])
        .unwrap();
    let without_trashed = core
        .start_search(
            SearchRequest {
                query: "★".to_owned(),
                mode: SearchMode::Semantic,
                filters: SearchFilters::default(),
                page_size: 20,
            },
            6,
        )
        .unwrap();
    assert_eq!(without_trashed.results.len(), 1, "回收站默认不参与");
    let with_trashed = core
        .start_search(
            SearchRequest {
                query: "★".to_owned(),
                mode: SearchMode::Semantic,
                filters: SearchFilters {
                    include_trashed: true,
                    ..Default::default()
                },
                page_size: 20,
            },
            7,
        )
        .unwrap();
    assert_eq!(with_trashed.results.len(), 2);
}

/// 模型的 `dims` 与算出来的向量长度对不上时必须报错，不能把坏向量写进库。
#[test]
fn a_vector_of_the_wrong_length_is_rejected() {
    struct WrongDims;
    impl Embedder for WrongDims {
        fn model_version(&self) -> &str {
            "wrong-dims@000000000000"
        }
        fn dims(&self) -> usize {
            512
        }
        fn options(&self) -> EmbedOptions {
            EmbedOptions::default()
        }
        fn embed_document(&self, _text: &str) -> diary_core::Result<Vec<f32>> {
            Ok(vec![0.0; 8])
        }
        fn embed_query(&self, _text: &str) -> diary_core::Result<Vec<f32>> {
            Ok(vec![0.0; 8])
        }
    }

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.sqlite");
    let mut core = new_core(dir.path());
    core.set_embedder(Box::new(WrongDims));
    write_capture(&mut core, at(2026, 9, 20), "妈妈打电话来。", "opA");

    let error = core.build_semantic_index(None).unwrap_err();
    assert!(
        error.to_string().contains("512"),
        "要说清维度不符：{error}"
    );
    let conn = open(&path);
    assert_eq!(
        count(&conn, "SELECT COUNT(*) FROM chunk_vectors", "chunk_vectors"),
        0,
        "坏向量一行都不该进库"
    );
    let (active, building): (Option<i64>, Option<i64>) = conn
        .query_row(
            "SELECT active_generation, building_generation FROM index_meta WHERE id = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        (active, building),
        (None, Some(1)),
        "算向量时出错不该激活半成品；building_generation 留着，下一轮从它续跑\
         （与中断同一条规则）。注意：`active` 必须是 None。"
    );
}

/// 关键词检索完全不受块与向量影响：换代期间它照旧可用（写测试钉住）。
#[test]
fn keyword_search_is_untouched_by_building_semantics() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    let counters = Arc::new(Counters::default());
    core.set_embedder(stub(&counters, vec![]));
    let a = write_capture(&mut core, at(2026, 9, 20), "妈妈打电话来。", "opA");
    import_and_extract(&mut core, &a, "信.txt", "妈妈后来又写了一封信。\n", "opA-mat");

    let before = search_mode(&mut core, "妈妈", SearchMode::Keyword, 20);
    let before_ids: Vec<String> = before.results.iter().map(|hit| hit.hit_id.clone()).collect();
    assert_eq!(before_ids.len(), 2);

    core.build_semantic_index(None).unwrap();
    let after = search_mode(&mut core, "妈妈", SearchMode::Keyword, 20);
    assert_eq!(
        after.results.iter().map(|hit| hit.hit_id.clone()).collect::<Vec<_>>(),
        before_ids,
        "关键词命中一行都不该因为建语义索引而变"
    );
    assert_eq!(after.index_coverage, before.index_coverage);
    assert_eq!(after.results[0].snippet, before.results[0].snippet);

    // 再建一代（换代中间走一遍），关键词照样不变。
    core.build_semantic_index(None).unwrap();
    let again = search_mode(&mut core, "妈妈", SearchMode::Keyword, 20);
    assert_eq!(
        again.results.iter().map(|hit| hit.hit_id.clone()).collect::<Vec<_>>(),
        before_ids
    );
}

// ------------------------------------------------------------ 真模型（不进 CI）

/// 真模型端到端：加载 bge-small-zh-v1.5、建向量、语义检索、按篇折叠。
///
/// **不进 CI**（CI 上没有模型、没有 ORT 动态库），手动跑：
///
/// ```bash
/// DIARY_MODEL_DIR="$PWD/models/bge-small-zh-v1.5-ours" \
/// DIARY_ORT_DYLIB="$HOME/dev/onnxruntime-linux-x64-1.22.0/lib/libonnxruntime.so" \
/// cargo test --release -p diary_core --test semantic_index_test -- --ignored --nocapture \
///     semantic_index_with_the_real_model
/// ```
///
/// 注意 `--exact` 不是必须的，但**不要**写成别的东西（比如中文短语）：测试名过滤按
/// 子串匹配，写错了会「跑 0 个测试、退出码 0」。同样的坑见
/// `search_test.rs::hundred_thousand_segments_measurement`。
#[test]
#[ignore = "需要真模型与 ORT：手动跑，见测试文档里的复现命令（DIARY_MODEL_DIR / DIARY_ORT_DYLIB）"]
fn semantic_index_with_the_real_model() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());

    // 语料：查询「失眠」与第一篇语义相关但没有共同词。
    let a = write_capture(&mut core, at(2026, 9, 20), "", "opA");
    let source_a = import_and_extract(
        &mut core,
        &a,
        "睡眠.txt",
        "连续几天凌晨两点还醒着，白天一点精神都没有。\n",
        "opA-mat",
    );
    let b = write_capture(&mut core, at(2026, 9, 21), "", "opB");
    import_and_extract(&mut core, &b, "聚餐.txt", "晚上和同事去吃了火锅，聊得很开心。\n", "opB-mat");
    let c = write_capture(&mut core, at(2026, 9, 22), "今天把合同签了。", "opC");

    let report = core.build_semantic_index(None).unwrap();
    assert!(report.total_chunks >= 3, "三篇各至少一块");
    assert_eq!(report.embedded_chunks, report.total_chunks);
    let status = core.index_status(None).unwrap();
    assert!(status.semantic_index_ready);
    let version = status.model_version.clone().expect("生效代次要有模型版本");
    assert!(
        version.starts_with("bge-small-zh-v1.5-ours@") && version.len() > 24,
        "模型版本要带 sha256 前缀，不能是写死的字符串：{version}"
    );

    let snapshot = search_mode(&mut core, "失眠", SearchMode::Semantic, 20);
    assert_eq!(
        snapshot.results[0].group_id, source_a,
        "语义应当把「凌晨两点还醒着」排在最前：{:?}",
        snapshot
            .results
            .iter()
            .map(|hit| (&hit.group_id, &hit.snippet))
            .collect::<Vec<_>>()
    );
    assert_eq!(snapshot.results[0].matched_by, vec![diary_core::MatchedBy::Semantic]);
    assert!(
        snapshot.results[0]
            .snippet
            .as_deref()
            .unwrap()
            .contains("凌晨两点"),
        "摘录取该篇自己那一段"
    );
    // 记录自己的文字也能被语义搜到（它进块，也进向量）。
    assert!(
        snapshot.results.iter().any(|hit| hit.group_id == c),
        "记录自己的文字也要在语义候选里"
    );
    assert_eq!(
        snapshot.results.len(),
        3,
        "按篇折叠：三篇各一条，不该出现同一篇多条"
    );

    // 量测入口也走一遍（分数从高到低）。
    let ranking = core
        .semantic_ranking("失眠", &SearchFilters::default())
        .unwrap();
    assert_eq!(ranking.len(), 3);
    assert!(
        ranking[0].1 >= ranking[1].1 && ranking[1].1 >= ranking[2].1,
        "分数要从高到低：{ranking:?}"
    );

    // 换一代（内容没变）：向量复用，不重算，也不影响语义排序。
    let again = core.build_semantic_index(None).unwrap();
    assert_eq!(again.embedded_chunks, 0, "没变的块不重复算");
    assert_eq!(again.reused_chunks, again.total_chunks);
    let after = search_mode(&mut core, "失眠", SearchMode::Semantic, 20);
    assert_eq!(after.results[0].group_id, source_a);
}
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
    Core, CreateDraftInput, EmbedOptions, Embedder, Embedding, EmbeddingConfig, ImportManifest,
    ImportOrigin, ImportRequest, JobState, MatchedBy, SearchFilters, SearchHit, SearchMode,
    SearchPhase, SearchRequest, SearchSnapshot, SourceKind, TokenTruncation, VectorStorage,
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
    /// 查询侧单独一组规则；为空表示与 `rules` 同一组（B3c-2 的用例都是这个口径）。
    ///
    /// 混合检索（#51）要能构造「关键词只中 A、语义只中 C」：两路边界的向量必须分开
    /// 控制，否则查询里出现的关键词必然也出现在 A 的正文里（关键词那一路是整串子串
    /// 匹配），一组规则会让 A 同时变成语义命中。
    query_rules: Vec<(&'static str, Vec<f32>)>,
}

impl StubEmbedder {
    fn new(
        model_version: &str,
        counters: &Arc<Counters>,
        rules: Vec<(&'static str, Vec<f32>)>,
    ) -> Self {
        Self::with_dims(model_version, 512, counters, rules)
    }

    /// 指定维度的桩：用来构造「换个模型连维度都不同」的场景（回归 #58）。
    fn with_dims(
        model_version: &str,
        dims: usize,
        counters: &Arc<Counters>,
        rules: Vec<(&'static str, Vec<f32>)>,
    ) -> Self {
        Self {
            model_version: model_version.to_owned(),
            dims,
            counters: Arc::clone(counters),
            rules,
            query_rules: Vec::new(),
        }
    }

    fn with_query_rules(
        model_version: &str,
        counters: &Arc<Counters>,
        rules: Vec<(&'static str, Vec<f32>)>,
        query_rules: Vec<(&'static str, Vec<f32>)>,
    ) -> Self {
        Self {
            model_version: model_version.to_owned(),
            dims: 512,
            counters: Arc::clone(counters),
            rules,
            query_rules,
        }
    }

    fn vector_for(&self, text: &str, rules: &[(&'static str, Vec<f32>)]) -> Vec<f32> {
        for (needle, vector) in rules {
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

    fn embed_document(&self, text: &str) -> diary_core::Result<diary_core::Embedding> {
        self.counters.documents.fetch_add(1, Ordering::SeqCst);
        Ok(diary_core::Embedding::untruncated(
            self.vector_for(text, &self.rules),
        ))
    }

    fn embed_query(&self, text: &str) -> diary_core::Result<diary_core::Embedding> {
        self.counters.queries.fetch_add(1, Ordering::SeqCst);
        let rules = if self.query_rules.is_empty() {
            &self.rules
        } else {
            &self.query_rules
        };
        Ok(diary_core::Embedding::untruncated(
            self.vector_for(text, rules),
        ))
    }
}

/// 512 维的单位向量：只有第 `index` 位是 1。
fn unit(index: usize) -> Vec<f32> {
    unit_in(512, index)
}

/// 任意维度的单位向量（`index` 必须小于 `dims`）。
fn unit_in(dims: usize, index: usize) -> Vec<f32> {
    let mut vector = vec![0_f32; dims];
    vector[index] = 1.0;
    vector
}

/// 与 `unit(0)` 的余弦为 `cosine` 的 512 维单位向量（#135 的补位测试要精确控制先后）。
fn at_cosine(cosine: f32) -> Vec<f32> {
    let mut vector = vec![0_f32; 512];
    vector[0] = cosine;
    vector[1] = (1.0 - cosine * cosine).sqrt();
    vector
}

fn stub(counters: &Arc<Counters>, rules: Vec<(&'static str, Vec<f32>)>) -> Box<dyn Embedder> {
    Box::new(StubEmbedder::new(
        "stub-embedder@000000000000",
        counters,
        rules,
    ))
}

/// 查询侧单独一组规则的 stub：用来构造「关键词只中 A、语义只中 B」这类两路边界。
fn stub_split(
    counters: &Arc<Counters>,
    rules: Vec<(&'static str, Vec<f32>)>,
    query_rules: Vec<(&'static str, Vec<f32>)>,
) -> Box<dyn Embedder> {
    Box::new(StubEmbedder::with_query_rules(
        "stub-embedder@000000000000",
        counters,
        rules,
        query_rules,
    ))
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
    core.save_draft(capture_id, text, revision, operation)
        .unwrap();
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

/// 混合那一路的完整两步（#69）：`start_search(mode = hybrid)` 先只算关键词
/// （`keyword_ready`），`upgrade_search_to_hybrid` 再融合（`hybrid_ready`）。
/// 「要融合后的结果」的用例都走这里，与产品路径一致。
/// 从第一屏开始一页页翻到底，收齐这个会话里的全部命中（含快速路径的深度截断）。
fn collect_pages(core: &mut Core, first: SearchSnapshot) -> Vec<SearchHit> {
    let session_id = first.session_id.clone();
    let mut hits = first.results;
    let mut cursor = first.cursor;
    while let Some(current) = cursor {
        let snapshot = core.search_next_page(&session_id, Some(&current)).unwrap();
        hits.extend(snapshot.results);
        cursor = snapshot.cursor;
    }
    hits
}

fn hybrid_mode(core: &mut Core, query: &str, page_size: i64) -> SearchSnapshot {
    let started = search_mode(core, query, SearchMode::Hybrid, page_size);
    core.upgrade_search_to_hybrid(&started.session_id).unwrap()
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
    import_and_extract(
        &mut core,
        &a,
        "信.txt",
        "妈妈后来又写了一封信。\n",
        "opA-mat",
    );

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

    // hybrid（#51/#69）：语义未就绪 → 第一阶段就**退化为关键词**，并且把原因明说出来；
    // 退化不等于没有结果，result 与关键词那一路是同一批。阶段如实停在 keyword_ready。
    let hybrid = search_mode(&mut core, "妈妈", SearchMode::Hybrid, 20);
    assert_eq!(
        hybrid.phase,
        SearchPhase::KeywordReady,
        "混合的首屏只有一个阶段：关键词就绪"
    );
    assert_eq!(
        hybrid.results.len(),
        2,
        "混合退化为关键词之后与关键词同一批结果"
    );
    assert!(
        hybrid
            .warnings
            .iter()
            .any(|warning| warning.contains("混合检索退化为关键词")),
        "退化必须明说，不许静默：{:?}",
        hybrid.warnings
    );
    assert!(
        hybrid
            .warnings
            .iter()
            .any(|warning| warning.contains("语义索引还没有生效的代次")),
        "还要说清为什么退化：{:?}",
        hybrid.warnings
    );
    assert!(
        hybrid
            .results
            .iter()
            .all(|hit| hit.matched_by == vec![MatchedBy::Keyword]),
        "退化时 matchedBy 只有关键词，不许标成混合：{:?}",
        hybrid
            .results
            .iter()
            .map(|hit| &hit.matched_by)
            .collect::<Vec<_>>()
    );

    // 升级也**不许**假装融合过：仍然 keyword_ready、仍然是那批关键词结果，
    // 只是多一句「没有可融合的语义证据」。
    let upgraded = core.upgrade_search_to_hybrid(&hybrid.session_id).unwrap();
    assert_eq!(
        upgraded.phase,
        SearchPhase::KeywordReady,
        "语义拿不出证据时升级绝不能声称 hybrid_ready"
    );
    assert_eq!(
        upgraded.results, hybrid.results,
        "结果仍是关键词那一路，一条不多一条不少"
    );
    assert!(
        upgraded
            .warnings
            .iter()
            .any(|warning| warning.contains("没有可融合的语义证据")),
        "升级这一步要如实说清为什么没融合：{:?}",
        upgraded.warnings
    );
    // 幂等：再升级一次给同一份快照，不重复算。
    let again = core.upgrade_search_to_hybrid(&hybrid.session_id).unwrap();
    assert_eq!(again, upgraded, "重复升级返回同一份快照");
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
    assert_eq!(
        search_mode(&mut core, "妈妈", SearchMode::Keyword, 20)
            .results
            .len(),
        1
    );
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
    assert_eq!(
        (active, building),
        (Some(1), None),
        "切换之后 building 要置空"
    );
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
    core.save_draft(
        &capture_id,
        "第二版：同事约我周末去爬山。",
        revision,
        "opA-2",
    )
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
    // 这条测的是**同口径续跑**，不靠默认存储：把整条测试显式钉在 f32 上，
    // 否则默认值一改，下面的「续上 / 复用」就会变成「换格式整代重算」。
    core.set_vector_storage_for_test(VectorStorage::F32);
    write_capture(&mut core, at(2026, 9, 20), "妈妈打电话来。", "opA");
    write_capture(&mut core, at(2026, 9, 21), "路边的桂花开了。", "opB");
    core.build_semantic_index(None).unwrap();

    // 模拟「上一轮开到第 2 代、只写了其中一块就中断了」。
    {
        let conn = open(&path);
        let chunk_id: String = conn
            .query_row(
                "SELECT chunk_id FROM text_chunks ORDER BY day_key LIMIT 1",
                [],
                |row| row.get(0),
            )
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

    // 同口径续跑**不白重算**：用 embed 调用次数钉住（口径核对不能把续跑变成全量重算）。
    let calls_before = counters.documents.load(Ordering::SeqCst);
    let report = core.build_semantic_index(None).unwrap();
    assert_eq!(report.generation, 2, "从留下的 building_generation 续跑");
    assert_eq!(report.reused_chunks, 1, "另一块从上一代复制");
    assert_eq!(report.resumed_chunks, 1, "中断前写好的那一块算「续上」");
    assert_eq!(report.embedded_chunks, 0, "没有一块需要重算");
    assert_eq!(
        counters.documents.load(Ordering::SeqCst),
        calls_before,
        "同口径续跑一次模型都不该调：续上的块沿用、没变的块复用"
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
    assert!(core.index_status(None).unwrap().semantic_index_ready);
}

/// 回归（评审在 #58 上指出的）：**中断 → 换模型 → 续跑不能混用不同模型的向量**。
///
/// 场景：模型 A 只写了一部分 `building_generation` 向量就中断 → 换成模型 B
/// （`model_version` 与 `dims` 都不同）再调 `build_semantic_index`。
///
/// 修复前：A 的半成品被 `missing_chunks` 当成「已完成」，只有剩下的块由 B 补算，
/// 最后切出一个**混合代次**——搜索把两个模型空间的向量放在一起打分（#80 之前；现在
/// 这种代次会让语义这一路整路不作数），`index_status.model_version` 也只挑其中一行报出去。
///
/// 修复后：续跑前核对口径，只要有一条与当前嵌入器不一致就整代清理、按 B 从零重算。
#[test]
fn switching_models_mid_build_discards_the_half_built_generation() {
    const A_VERSION: &str = "stub-embedder@aaaaaaaaaaaa";
    const B_VERSION: &str = "stub-embedder@bbbbbbbbbbbb";

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.sqlite");
    let mut core = new_core(dir.path());
    let counters = Arc::new(Counters::default());
    // A：512 维，把两篇正文都映到 e0，先建出一代生效向量。
    // 这条测的是**换模型**，所以把存储显式钉在 int8 上：A 的生效代次与半成品
    // 用同一种编码，口径差异只来自 model_version / dims，不掺 storage。
    core.set_vector_storage_for_test(VectorStorage::Int8);
    core.set_embedder(Box::new(StubEmbedder::with_dims(
        A_VERSION,
        512,
        &counters,
        vec![("苹果", unit(0)), ("香蕉", unit(0))],
    )));
    let apple = write_capture(&mut core, at(2026, 9, 20), "苹果很甜。", "opA");
    let banana = write_capture(&mut core, at(2026, 9, 21), "香蕉也甜。", "opB");
    let first = core.build_semantic_index(None).unwrap();
    assert_eq!(first.embedded_chunks, 2, "前提：A 先建好一代");

    // 模拟「A 在第 2 代里只写了苹果那一块就中断了」（与第 1 代同编码，只有模型会变）。
    {
        let conn = open(&path);
        let chunk_id: String = conn
            .query_row(
                "SELECT chunk_id FROM text_chunks WHERE text LIKE '%苹果%' LIMIT 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        conn.execute(
            "INSERT INTO chunk_vectors (chunk_id, generation, model_version, dims, storage, \
                                        vector, created_at) \
             VALUES (?1, 2, ?2, 512, 'int8', ?3, '2026-10-10T00:00:00.000Z')",
            params![
                chunk_id,
                A_VERSION,
                VectorStorage::Int8.encode(&vec![0.0_f32; 512]).unwrap()
            ],
        )
        .unwrap();
        conn.execute("UPDATE index_meta SET building_generation = 2", [])
            .unwrap();
    }
    // 中断期间生效代次还是 A 的第 1 代。
    let interrupted = core.index_status(None).unwrap();
    assert_eq!(interrupted.model_version.as_deref(), Some(A_VERSION));

    // 换成 B：model_version 与 dims（384）都不同。
    core.set_embedder(Box::new(StubEmbedder::with_dims(
        B_VERSION,
        384,
        &counters,
        vec![("苹果", unit_in(384, 5)), ("香蕉", unit_in(384, 5))],
    )));
    let report = core.build_semantic_index(None).unwrap();
    assert_eq!(report.generation, 2, "代次号继续用，但内容按 B 从头来");
    assert_eq!(report.reused_chunks, 0, "A 的向量口径不一致，一条都不复用");
    assert_eq!(report.embedded_chunks, 2, "两块都按 B 重算");
    assert_eq!(
        report.resumed_chunks, 0,
        "A 的半成品被丢弃，一点都没「续上」"
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
    // 生效代次里没有任何 A 的向量。
    assert_eq!(
        count(
            &conn,
            &format!("SELECT COUNT(*) FROM chunk_vectors WHERE model_version = '{A_VERSION}'"),
            "chunk_vectors",
        ),
        0,
        "A 的半成品必须一行不剩（否则统计与搜索还会撞上旧模型空间）"
    );
    // 整代口径完全一致，且等于 B。
    let (distinct_versions, distinct_dims, distinct_storages): (i64, i64, i64) = conn
        .query_row(
            "SELECT COUNT(DISTINCT model_version), COUNT(DISTINCT dims), COUNT(DISTINCT storage) \
             FROM chunk_vectors WHERE generation = 2",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        (distinct_versions, distinct_dims, distinct_storages),
        (1, 1, 1),
        "生效代次里 model_version / dims / storage 都必须只有一种口径"
    );
    let caliber: (String, i64, String) = conn
        .query_row(
            "SELECT model_version, dims, storage FROM chunk_vectors WHERE generation = 2 LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(caliber, (B_VERSION.to_owned(), 384, "int8".to_owned()));
    assert_eq!(
        count(&conn, "SELECT COUNT(*) FROM chunk_vectors", "chunk_vectors"),
        2,
        "A 的旧代次与半成品都清掉了，只剩 B 这一代"
    );
    drop(conn);

    // 状态报的是 B：不是 A，也不是「混合」这种含糊值。
    let status = core.index_status(None).unwrap();
    assert_eq!(
        status.model_version.as_deref(),
        Some(B_VERSION),
        "index_status 要报整代一致的那个版本"
    );
    assert!(status.semantic_index_ready, "整代口径一致、覆盖全部块");

    // 搜索只在 B 的空间里打分：两篇正文都被 B 映到 e5（384 维），查询也是 e5，
    // 所以两条的相似度都必须是 1.0。修复前苹果那一块留着 A 的 512 维 zeroblob，
    // 与 384 维查询余弦为 0（不同空间硬算），这里就会红。
    let ranking = core
        .semantic_ranking("苹果", &SearchFilters::default())
        .unwrap();
    let score_of = |group: &str| {
        ranking
            .iter()
            .find(|(candidate, _)| candidate.as_str() == group)
            .map(|(_, score)| *score)
            .unwrap_or_else(|| panic!("{group} 不在结果里：{ranking:?}"))
    };
    assert!(
        (score_of(&apple) - 1.0).abs() < 1e-6,
        "苹果那一块必须是 B 空间里的向量（修复前是 A 的，得分 0）：{ranking:?}"
    );
    assert!(
        (score_of(&banana) - 1.0).abs() < 1e-6,
        "香蕉那一块也要落在 B 空间：{ranking:?}"
    );
}

/// 回归（评审在 #58 上指出的）：**半个代次的存储格式与当前不一致时，同样整代废弃重算**。
///
/// `storage` 是「一套口径」的第三个字段。默认已是 int8；中断留下的 f32 半成品若被
/// 当成「已完成」，切出来的代次里就会混着两种编码——解码虽然按行分派，跨编码的余弦
/// 却会被当成正常分数算出来（看起来有结果，但那个分数不代表任何东西）。这条钉住
/// 「换存储格式也当换口径」。
#[test]
fn a_half_built_generation_with_a_foreign_storage_format_is_discarded() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.sqlite");
    let mut core = new_core(dir.path());
    let counters = Arc::new(Counters::default());
    core.set_embedder(stub(&counters, vec![]));
    // 当前代次显式用 int8（不靠默认）；下面故意把半成品写成 f32，构造「换过格式」。
    core.set_vector_storage_for_test(VectorStorage::Int8);
    write_capture(&mut core, at(2026, 9, 20), "妈妈打电话来。", "opA");
    write_capture(&mut core, at(2026, 9, 21), "路边的桂花开了。", "opB");
    core.build_semantic_index(None).unwrap();

    // 模拟「上一轮开到第 2 代、用另一种存储格式（f32）写了一块就中断」。
    {
        let conn = open(&path);
        let chunk_id: String = conn
            .query_row(
                "SELECT chunk_id FROM text_chunks ORDER BY day_key LIMIT 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        conn.execute(
            "INSERT INTO chunk_vectors (chunk_id, generation, model_version, dims, storage, \
                                        vector, created_at) \
             VALUES (?1, 2, 'stub-embedder@000000000000', 512, 'f32', ?2, \
                     '2026-10-10T00:00:00.000Z')",
            params![chunk_id, diary_core::vector_bytes(&vec![0.0_f32; 512])],
        )
        .unwrap();
        conn.execute("UPDATE index_meta SET building_generation = 2", [])
            .unwrap();
    }

    let report = core.build_semantic_index(None).unwrap();
    assert_eq!(report.generation, 2);
    assert_eq!(
        report.reused_chunks, 2,
        "废弃半成品后，两块都能从上一代 int8 复用"
    );
    assert_eq!(report.embedded_chunks, 0, "没有一块需要重算");
    assert_eq!(
        report.resumed_chunks, 0,
        "storage 格式不一致的那一块不算「续上」"
    );

    let conn = open(&path);
    assert_eq!(
        count(
            &conn,
            "SELECT COUNT(*) FROM chunk_vectors WHERE generation = 2 AND storage <> 'int8'",
            "chunk_vectors"
        ),
        0,
        "生效代次里不能留下别的存储格式"
    );
    assert_eq!(
        count(
            &conn,
            "SELECT COUNT(*) FROM chunk_vectors WHERE generation = 2",
            "chunk_vectors"
        ),
        2
    );
    drop(conn);
    // 半成品若真被留下，语义检索会读到 f32 那一行——能正常返回（纯语义是 kNN，
    // 会把两块都列出来）就说明整代都是当前格式 int8。
    assert_eq!(
        search_mode(&mut core, "妈妈", SearchMode::Semantic, 10)
            .results
            .len(),
        2
    );
}

/// 历史遗留的**混合生效代次**：状态不能挑一行装作干净。
///
/// 修复之后正常操作产不出混合代次了，但旧库里可能已经存在一个（本片修复前激活的）。
/// 这时候 `model_version` 没有单一真值可报：如实给 `None` + `reasons` 里点名「口径不一致」，
/// 并且不说「就绪」。随后一次重建会把它按当前模型整代重算、自愈。
#[test]
fn a_mixed_active_generation_is_reported_honestly() {
    const A_VERSION: &str = "stub-embedder@aaaaaaaaaaaa";
    const B_VERSION: &str = "stub-embedder@bbbbbbbbbbbb";

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.sqlite");
    let mut core = new_core(dir.path());
    let counters = Arc::new(Counters::default());
    core.set_embedder(Box::new(StubEmbedder::with_dims(
        A_VERSION,
        512,
        &counters,
        vec![],
    )));
    write_capture(&mut core, at(2026, 9, 20), "妈妈打电话来。", "opA");
    write_capture(&mut core, at(2026, 9, 21), "路边的桂花开了。", "opB");
    core.build_semantic_index(None).unwrap();

    // 直接改掉其中一行的 model_version，模拟「修复前切出来的混合代次」。
    {
        let conn = open(&path);
        conn.execute(
            "UPDATE chunk_vectors SET model_version = ?1 WHERE chunk_id = \
             (SELECT chunk_id FROM chunk_vectors WHERE generation = 1 ORDER BY chunk_id LIMIT 1)",
            params![B_VERSION],
        )
        .unwrap();
    }

    let status = core.index_status(None).unwrap();
    assert_eq!(
        status.model_version, None,
        "混合代次没有单一版本可报：宁可说「没有」也不挑一行假装干净"
    );
    assert_eq!(status.embedded_chunks, 2, "两块的向量都在（覆盖是够的）");
    assert!(!status.semantic_index_ready, "混合代次不能叫「就绪」");
    assert!(
        status.reasons.iter().any(|reason| reason.contains("口径")),
        "要说清是「口径不一致」，而不是含糊的未就绪：{:?}",
        status.reasons
    );

    // 重建按当前模型整代重算，混合代次自愈。
    let report = core.build_semantic_index(None).unwrap();
    assert_eq!(report.generation, 2);
    assert_eq!(
        report.reused_chunks, 1,
        "口径一致的那一块复用，口径不一致的重算"
    );
    assert_eq!(report.embedded_chunks, 1);
    let conn = open(&path);
    assert_eq!(
        count(
            &conn,
            "SELECT COUNT(*) FROM chunk_vectors WHERE generation <> 2",
            "chunk_vectors"
        ),
        0,
        "重建清掉了混合的旧代次"
    );
    drop(conn);
    let healed = core.index_status(None).unwrap();
    assert_eq!(healed.model_version.as_deref(), Some(A_VERSION));
    assert!(healed.semantic_index_ready);
}

// ------------------------------------------------------------ 跨向量空间的口径判据（#80）

/// issue #80：**换了模型、还没重建索引就搜索**——语义那一路原来会跨向量空间算分。
///
/// 这是最常见的真实情形（用户换了模型配置但还没重建）：生效代次是 `Single(模型 A)`，
/// 当前嵌入器是模型 B（`model_version` 与 `dims` 都不同）。查询向量来自 B、库向量来自
/// A，余弦全是垃圾但**看起来像正常结果**（有排序、有分数）。
///
/// 口径不一致时语义这一路不作数：混合退化为关键词、阶段如实停在 `keyword_ready`、
/// 警告说清「另一个模型 + 重建即可恢复」；纯语义空结果 + 同一条警告；关键词照常。
/// 整代重算重建之后自动恢复。
#[test]
fn switching_models_without_rebuild_keeps_semantic_out_of_both_paths() {
    const A_VERSION: &str = "stub-embedder@aaaaaaaaaaaa";
    const B_VERSION: &str = "stub-embedder@bbbbbbbbbbbb";

    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    let counters = Arc::new(Counters::default());

    // 模型 A（512 维）：查询「失眠」与两篇正文都映到 e0——A 的空间里两篇都「像」。
    core.set_embedder(Box::new(StubEmbedder::with_dims(
        A_VERSION,
        512,
        &counters,
        vec![("失眠", unit(0)), ("翻来覆去", unit(0))],
    )));
    let a_capture = write_capture(&mut core, at(2026, 9, 20), "最近总是失眠。", "opA");
    // B 篇不含查询词：「失眠」召不到它，**只有语义那一路**能召到。
    let b_capture = write_capture(&mut core, at(2026, 9, 21), "夜里翻来覆去。", "opB");
    core.build_semantic_index(None).unwrap();

    // 换成模型 B（384 维、另一个 model_version），**不重建**。B 的查询向量落在 e0
    // （384 维）——修复前拿它和 A 的 512 维 e0 算余弦（`zip` 截到前 384 位）正好是
    // 1.0、稳稳过证据门槛：这就是「跨空间算分看起来很正常」的样子。
    core.set_embedder(Box::new(StubEmbedder::with_dims(
        B_VERSION,
        384,
        &counters,
        vec![("失眠", unit_in(384, 0)), ("翻来覆去", unit_in(384, 0))],
    )));

    // —— 关键词：照常可用，本地优先功能不被它挡住 ——
    let keyword = search_mode(&mut core, "失眠", SearchMode::Keyword, 20);
    let keyword_groups: Vec<&str> = keyword
        .results
        .iter()
        .map(|hit| hit.group_id.as_str())
        .collect();
    assert_eq!(
        keyword_groups,
        vec![a_capture.as_str()],
        "前提：关键词只中 A 那篇"
    );

    // —— 混合：退化为关键词，阶段 keyword_ready，结果里没有只有语义能召回的 B ——
    let started = search_mode(&mut core, "失眠", SearchMode::Hybrid, 20);
    assert_eq!(
        started.phase,
        SearchPhase::KeywordReady,
        "口径不一致时首屏仍是 keyword_ready"
    );
    assert_eq!(
        started.results, keyword.results,
        "混合的结果等于关键词那一路（B 那篇不许被跨空间的分数带进来）"
    );
    assert!(
        !started.results.iter().any(|hit| hit.group_id == b_capture),
        "只被语义召回的那篇不许出现：{:?}",
        started
            .results
            .iter()
            .map(|hit| &hit.group_id)
            .collect::<Vec<_>>()
    );
    assert!(
        started
            .results
            .iter()
            .all(|hit| hit.matched_by == vec![MatchedBy::Keyword]),
        "matchedBy 只能如实是关键词：{:?}",
        started
            .results
            .iter()
            .map(|hit| (&hit.group_id, &hit.matched_by))
            .collect::<Vec<_>>()
    );

    let upgraded = core.upgrade_search_to_hybrid(&started.session_id).unwrap();
    assert_eq!(
        upgraded.phase,
        SearchPhase::KeywordReady,
        "跨空间的口径 ⇒ 不融合、不返回 hybrid_ready：{:?}",
        upgraded.warnings
    );
    assert_eq!(
        upgraded.results, started.results,
        "保留关键词那份快照（不是被跨空间分数重排过的融合结果）"
    );
    assert!(
        started
            .warnings
            .iter()
            .any(|warning| warning.contains("另一个模型") || warning.contains("口径不一致")),
        "混合退化要说清是「另一个模型/口径不一致」：{:?}",
        started.warnings
    );
    assert!(
        upgraded
            .warnings
            .iter()
            .any(|warning| warning.contains("另一个模型") && warning.contains("重建")),
        "升级的警告也要说清原因与恢复办法：{:?}",
        upgraded.warnings
    );

    // —— 纯语义：空结果 + 同一条警告，**不是错分** ——
    let semantic = search_mode(&mut core, "失眠", SearchMode::Semantic, 20);
    assert!(
        semantic.results.is_empty(),
        "跨空间的语义结果必须为空（B 那篇不许出现）：{:?} · {:?}",
        semantic
            .results
            .iter()
            .map(|hit| (&hit.group_id, &hit.matched_by))
            .collect::<Vec<_>>(),
        semantic.warnings
    );
    assert!(
        semantic
            .warnings
            .iter()
            .any(|warning| warning.contains("另一个模型") && warning.contains("重建")),
        "纯语义也要说清原因与恢复办法：{:?}",
        semantic.warnings
    );

    // —— 重建（整代重算）之后自动恢复：警告消失，语义/混合回到正常 ——
    let report = core.build_semantic_index(None).unwrap();
    assert_eq!(report.reused_chunks, 0, "口径不一致，A 的向量一条都不复用");
    assert_eq!(report.embedded_chunks, 2, "两块都按 B 重算");

    let healed = search_mode(&mut core, "失眠", SearchMode::Semantic, 20);
    assert!(
        !healed
            .warnings
            .iter()
            .any(|warning| warning.contains("另一个模型")),
        "重建后警告必须消失：{:?}",
        healed.warnings
    );
    assert!(
        healed.results.iter().any(|hit| hit.group_id == b_capture),
        "重建后 B 那篇回到语义结果里：{:?}",
        healed
            .results
            .iter()
            .map(|hit| &hit.group_id)
            .collect::<Vec<_>>()
    );

    let hybrid = hybrid_mode(&mut core, "失眠", 20);
    assert_eq!(
        hybrid.phase,
        SearchPhase::HybridReady,
        "重建后混合回到 hybrid_ready：{:?}",
        hybrid.warnings
    );
    assert!(
        hybrid.results.iter().any(|hit| hit.group_id == b_capture),
        "重建后 B 那篇参与融合：{:?}",
        hybrid
            .results
            .iter()
            .map(|hit| (&hit.group_id, &hit.matched_by))
            .collect::<Vec<_>>()
    );
    assert!(
        !hybrid
            .warnings
            .iter()
            .any(|warning| warning.contains("另一个模型")),
        "重建后混合的警告里也不该再有口径不一致：{:?}",
        hybrid.warnings
    );
}

/// issue #80：**历史遗留的 `Mixed` 生效代次**同样不作数——判据不是只处理
/// `Single(旧模型)`。整代里只要有一行属于另一个模型/维度/存储格式，就不是当前模型的
/// 向量空间：不许「挑出口径一致的那几行接着算」。
///
/// 构造与 `a_mixed_active_generation_is_reported_honestly` 同一个办法：A 建好一代，
/// 直接改掉其中一行的 `model_version`（模拟 #58 修复前切出来的混合代次），当前嵌入器
/// 仍是 A。
#[test]
fn a_mixed_generation_keeps_semantic_out_of_both_paths_until_rebuild() {
    const A_VERSION: &str = "stub-embedder@aaaaaaaaaaaa";
    const B_VERSION: &str = "stub-embedder@bbbbbbbbbbbb";

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.sqlite");
    let mut core = new_core(dir.path());
    let counters = Arc::new(Counters::default());
    core.set_embedder(Box::new(StubEmbedder::with_dims(
        A_VERSION,
        512,
        &counters,
        vec![("失眠", unit(0)), ("翻来覆去", unit(0))],
    )));
    let a_capture = write_capture(&mut core, at(2026, 9, 20), "最近总是失眠。", "opA");
    let b_capture = write_capture(&mut core, at(2026, 9, 21), "夜里翻来覆去。", "opB");
    core.build_semantic_index(None).unwrap();

    // 改掉其中一行的 model_version：生效代次成了历史遗留的混合代次。
    {
        let conn = open(&path);
        conn.execute(
            "UPDATE chunk_vectors SET model_version = ?1 WHERE chunk_id = \
             (SELECT chunk_id FROM chunk_vectors ORDER BY chunk_id LIMIT 1)",
            params![B_VERSION],
        )
        .unwrap();
    }

    // —— 混合：不融合、停在 keyword_ready，B 那篇不许出现 ——
    let started = search_mode(&mut core, "失眠", SearchMode::Hybrid, 20);
    assert_eq!(started.phase, SearchPhase::KeywordReady);
    assert!(
        !started.results.iter().any(|hit| hit.group_id == b_capture),
        "{:?}",
        started
            .results
            .iter()
            .map(|hit| &hit.group_id)
            .collect::<Vec<_>>()
    );
    let upgraded = core.upgrade_search_to_hybrid(&started.session_id).unwrap();
    assert_eq!(
        upgraded.phase,
        SearchPhase::KeywordReady,
        "混合代次 ⇒ 不融合：{:?}",
        upgraded.warnings
    );
    assert_eq!(upgraded.results, started.results);
    assert!(
        upgraded
            .warnings
            .iter()
            .any(|warning| warning.contains("混合代次") && warning.contains("重建")),
        "要把「历史遗留的混合代次」点名说出来：{:?}",
        upgraded.warnings
    );

    // —— 纯语义：空结果 + 警告，不是「拿口径一致的那几行接着算」 ——
    let semantic = search_mode(&mut core, "失眠", SearchMode::Semantic, 20);
    assert!(
        semantic.results.is_empty(),
        "混合代次下语义整路不作数：{:?} · {:?}",
        semantic
            .results
            .iter()
            .map(|hit| &hit.group_id)
            .collect::<Vec<_>>(),
        semantic.warnings
    );
    assert!(
        semantic
            .warnings
            .iter()
            .any(|warning| warning.contains("混合代次")),
        "{:?}",
        semantic.warnings
    );

    // —— 关键词照常 ——
    let keyword = search_mode(&mut core, "失眠", SearchMode::Keyword, 20);
    assert_eq!(keyword.results.len(), 1, "关键词那一路不受混合代次影响");
    assert_eq!(
        keyword.results[0].group_id, a_capture,
        "关键词那一路照常命中 A 那篇"
    );

    // —— 重建后恢复 ——
    core.build_semantic_index(None).unwrap();
    let healed = hybrid_mode(&mut core, "失眠", 20);
    assert_eq!(
        healed.phase,
        SearchPhase::HybridReady,
        "重建后混合回到 hybrid_ready：{:?}",
        healed.warnings
    );
    assert!(
        !healed
            .warnings
            .iter()
            .any(|warning| warning.contains("混合代次")),
        "重建后警告消失：{:?}",
        healed.warnings
    );
}

/// issue #80 第 2 条：**`dims` 不一致**的向量行不能静默 `zip` 算分。
///
/// 构造：A（512 维，显式 int8）建好一代，把其中一行的 `dims` 与字节一起改成 384 维的 e0
/// （行内自洽：记录 384、int8 字节 = 4 字节 scale + 384 字节分量）。修复前这条会通过
/// 「字节 vs 行上记录」的检查（384 = 384），然后 `cosine` 把 512 维查询 `zip` 到前 384 位，
/// 得到 1.0 的「相似度」——跨空间错分，而且看起来正常。
///
/// 修复后：整代口径判据在**进入计算前**发现 `dims` 不是 512，语义这一路整路不作数
/// （空结果 + 口径警告），不依赖逐行检查兜底。逐行那条只余下「行内自洽但字节与记录
/// 不符」这类坏数据的**如实报错**——绝不退回 0 分。
#[test]
fn a_dims_mismatch_is_blocked_before_any_cosine() {
    const A_VERSION: &str = "stub-embedder@aaaaaaaaaaaa";

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.sqlite");
    let mut core = new_core(dir.path());
    let counters = Arc::new(Counters::default());
    // 这条测的是坏数据/口径校验，显式钉在 int8 上：下面造坏向量时才能保证
    // 「scale 合法、只有维度不对」——不是先撞上 int8 自己的 scale 校验。
    core.set_vector_storage_for_test(VectorStorage::Int8);
    core.set_embedder(Box::new(StubEmbedder::with_dims(
        A_VERSION,
        512,
        &counters,
        vec![("失眠", unit(0)), ("翻来覆去", unit(0))],
    )));
    let a_capture = write_capture(&mut core, at(2026, 9, 20), "最近总是失眠。", "opA");
    let b_capture = write_capture(&mut core, at(2026, 9, 21), "夜里翻来覆去。", "opB");
    core.build_semantic_index(None).unwrap();

    let chunk_id: String = {
        let conn = open(&path);
        conn.query_row(
            "SELECT chunk_id FROM chunk_vectors ORDER BY chunk_id LIMIT 1",
            [],
            |row| row.get(0),
        )
        .unwrap()
    };
    // 行内自洽的 384 维 int8 向量：记录 384、字节是 4 字节 scale + 384 字节分量。
    {
        let conn = open(&path);
        conn.execute(
            "UPDATE chunk_vectors SET dims = 384, vector = ?1 WHERE chunk_id = ?2",
            params![
                VectorStorage::Int8.encode(&unit_in(384, 0)).unwrap(),
                chunk_id
            ],
        )
        .unwrap();
    }

    // —— 整代 dims 不一致 ⇒ 进计算前就整路不作数（不是逐行检查、也不是 0 分） ——
    let semantic = search_mode(&mut core, "失眠", SearchMode::Semantic, 20);
    assert!(
        semantic.results.is_empty(),
        "dims 不一致的整代不许出结果（修复前这里会拿到 1.0 的跨空间错分）：{:?} · {:?}",
        semantic
            .results
            .iter()
            .map(|hit| (&hit.group_id, &hit.matched_by))
            .collect::<Vec<_>>(),
        semantic.warnings
    );
    assert!(
        semantic
            .warnings
            .iter()
            .any(|warning| warning.contains("口径不一致") && warning.contains("384")),
        "警告要说清是口径/维度不一致：{:?}",
        semantic.warnings
    );
    let hybrid = hybrid_mode(&mut core, "失眠", 20);
    assert_eq!(
        hybrid.phase,
        SearchPhase::KeywordReady,
        "dims 不一致 ⇒ 混合也退化为关键词：{:?}",
        hybrid.warnings
    );
    assert!(!hybrid.results.iter().any(|hit| hit.group_id == b_capture));
    assert!(hybrid.results.iter().any(|hit| hit.group_id == a_capture));

    // —— 行上记 512 维、字节只解出 8 维（坏数据）：如实报错，绝不静默算分 ——
    // 用合法的 int8 编码（scale = 1.0）造出 8 个分量，确保先撞到的是「维度不符」
    // 而不是 int8 自己的「scale 不是正数」。
    {
        let conn = open(&path);
        conn.execute(
            "UPDATE chunk_vectors SET dims = 512, vector = ?1 WHERE chunk_id = ?2",
            params![VectorStorage::Int8.encode(&[0.0_f32; 8]).unwrap(), chunk_id],
        )
        .unwrap();
    }
    let error = core
        .start_search(
            SearchRequest {
                query: "失眠".to_owned(),
                mode: SearchMode::Semantic,
                filters: SearchFilters::default(),
                page_size: 20,
            },
            1,
        )
        .unwrap_err();
    assert!(
        error.to_string().contains("8 维") && error.to_string().contains("512"),
        "坏向量要如实报错（字节解出 8 维、行上记 512），不能给 0 分：{error}"
    );
}

// ------------------------------------------------------------ 消费契约

/// 命中按**篇**上报：合块的多篇各自成条，`snippet`/`locator` 取该篇自己那一段。
#[test]
fn semantic_hits_are_per_piece_and_snippets_come_from_that_pieces_span() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.sqlite");
    let mut core = new_core(dir.path());
    let counters = Arc::new(Counters::default());
    core.set_embedder(stub(
        &counters,
        vec![("失眠", unit(0)), ("睡不着", unit(0))],
    ));

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
    assert_eq!(
        snapshot.results.len(),
        3,
        "两篇来源 + 一条记录文字，各成一条"
    );
    assert_eq!(snapshot.phase, diary_core::SearchPhase::Done);
    assert_eq!(snapshot.index_coverage, diary_core::Coverage::Complete);

    // 合块里的第一篇：摘录是它自己那一段，**不含**同块另一篇的正文。
    let sleep = hit_of(&snapshot, &first);
    assert_eq!(sleep.hit_id, first, "命中 id 按篇给，不按证据块");
    assert_eq!(sleep.matched_by, vec![diary_core::MatchedBy::Semantic]);
    assert_eq!(sleep.source_kind, SourceKind::File);
    assert_eq!(sleep.source_id.as_deref(), Some(first.as_str()));
    // 语义来源篇：group_id 是来源篇的折叠键（source_id），capture_id 是这篇来源
    // 所属的记录。两者不同，界面跳记录详情页必须用后者。
    assert_eq!(sleep.group_id, first, "语义来源篇的 group_id 是来源篇的 id");
    assert_eq!(
        sleep.capture_id, a,
        "语义来源篇的 capture_id 是它所属的记录"
    );
    assert_ne!(
        sleep.capture_id, sleep.group_id,
        "语义来源篇上 capture_id 与 group_id 不是一回事"
    );
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
    assert_eq!(
        sleep.title.as_deref(),
        Some("睡眠.txt"),
        "标题取来源修订的原件名"
    );
    let locator = sleep.locator.as_ref().expect("来源篇要有定位");
    assert_eq!(
        locator.source_revision_id,
        sleep.revision_id.clone().unwrap()
    );

    // 同块的第二篇：它也拿到这一块的相似度（契约：合块的相似度对各篇共享），
    // 但摘录是它自己那一段。
    let express = hit_of(&snapshot, &second);
    assert_eq!(express.source_kind, SourceKind::File);
    assert_eq!(
        express.capture_id, a,
        "同块第二篇的 capture_id 也是它所属的那篇记录"
    );
    assert_ne!(
        express.capture_id, express.group_id,
        "来源篇的 capture_id 与 group_id 不同"
    );
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
    assert_eq!(
        capture_hit.capture_id, b,
        "语义记录文字命中的 capture_id 是这篇记录"
    );
    assert_eq!(
        capture_hit.capture_id, capture_hit.group_id,
        "记录文字命中天然自成一组，两者相同"
    );
    assert_eq!(capture_hit.source_id, None);
    assert_eq!(capture_hit.revision_id, None);
    assert_eq!(capture_hit.source_kind, SourceKind::Text);
    assert_eq!(capture_hit.snippet.as_deref(), Some("我最近总是睡不着。"));
    assert_eq!(capture_hit.locator, None, "记录文字没有原件可定位");
}

/// 合块里的篇分属**不同记录**时，`captureId` 必须各自取自己那段 span 的记录。
///
/// 这条钉的是「`captureId` 从哪来」：`text_chunks` 没有 `capture_id`（块跨记录），
/// 字段只能取 `chunk_spans` 里**该 snippet 所属那个 span** 的 `capture_id`。
/// 上面的用例里两篇来源都属于同一条记录，所以它证明不了「不是第一段 span 的记录」——
/// 把实现错改成「取第一段 span 的 capture」，那条用例仍然全绿。这里用同一天
/// 两条**不同记录**各自的短来源合进同一块来把这条钉死：A 的命中拿到 A、B 的命中
/// 拿到 B；错取第一段时，至少有一条会指到别人的日记。
#[test]
fn semantic_hits_take_the_capture_of_their_own_span_across_captures() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.sqlite");
    let mut core = new_core(dir.path());
    let counters = Arc::new(Counters::default());
    core.set_embedder(stub(&counters, vec![("失眠", unit(0))]));

    // 同一天、两条不同的记录，各挂一篇短来源：按 B3c-1 的「同一天短篇合块」规则
    // 会合进**同一块**，块里两段 span 的 capture_id 不同。
    let a = write_capture(&mut core, at(2026, 9, 22), "", "capA");
    let piece_a = import_and_extract(
        &mut core,
        &a,
        "A-睡眠.txt",
        "连续几天失眠，凌晨两点还醒着。\n",
        "capA-material",
    );
    let b = write_capture(&mut core, at(2026, 9, 22), "", "capB");
    let piece_b = import_and_extract(
        &mut core,
        &b,
        "B-快递.txt",
        "失眠之后下楼拿快递，风很大。\n",
        "capB-material",
    );
    assert_ne!(a, b, "前提：两条记录不是同一条");

    let built = core.build_semantic_index(None).unwrap();
    assert_eq!(built.total_chunks, 1, "同一天两篇短来源应当合进一块");

    // 前提必须成立，否则这个用例会退化成「各自成块」而失去意义：这一块真的含
    // 两段 span、分属两条不同的记录。
    let conn = open(&path);
    let spans: i64 = conn
        .query_row("SELECT COUNT(*) FROM chunk_spans", [], |row| row.get(0))
        .unwrap();
    let captures_in_chunk: i64 = conn
        .query_row(
            "SELECT COUNT(DISTINCT capture_id) FROM chunk_spans",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(spans, 2, "前提：块里有两段 span");
    assert_eq!(
        captures_in_chunk, 2,
        "前提：两段 span 分属两条不同的记录（这正是上面的用例缺的那一半）"
    );

    let snapshot = search_mode(&mut core, "失眠", SearchMode::Semantic, 20);
    assert_eq!(snapshot.results.len(), 2, "两条来源各成一条命中");

    let hit_a = hit_of(&snapshot, &piece_a);
    let hit_b = hit_of(&snapshot, &piece_b);
    assert_eq!(
        hit_a.capture_id, a,
        "A 的命中要拿到 A 那条记录（取它自己那段 span 的 capture_id）"
    );
    assert_eq!(
        hit_b.capture_id, b,
        "B 的命中要拿到 B 那条记录；取成第一段 span 的记录就会指到 A"
    );
    assert_ne!(
        hit_a.capture_id, hit_b.capture_id,
        "同一块里两条命中分属不同记录，captureId 不能相同"
    );
    assert_eq!(
        hit_a.group_id, piece_a,
        "语义来源篇的 group_id 仍是来源篇自己的 id"
    );
    assert_eq!(hit_b.group_id, piece_b);
    assert_ne!(
        hit_a.capture_id, hit_a.group_id,
        "来源篇命中上 capture_id 与 group_id 不是一回事"
    );
    assert_ne!(hit_b.capture_id, hit_b.group_id);
    assert_eq!(
        hit_a.snippet.as_deref(),
        Some("连续几天失眠，凌晨两点还醒着。")
    );
    assert_eq!(
        hit_b.snippet.as_deref(),
        Some("失眠之后下楼拿快递，风很大。"),
        "B 的摘录只能是 B 自己那段"
    );
}

/// 那篇「被切成两块、两块都含失眠」的长文（issue #57 的同分场景）。
///
/// 块边界只由正文与冻结的切块规则决定：切出来两块，span 分别是 `[0, 390)` 与
/// `[343, 731)`（48 字重叠），所以两块都含「失眠」→ 桩给两块同一个单位向量 →
/// 两块得分**完全相同**。
fn long_piece_with_insomnia() -> String {
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
    text
}

/// 同一篇有多个块命中时**折叠成一条**，取得分最高的块当证据。
#[test]
fn multiple_chunks_of_one_piece_fold_into_a_single_hit() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    let counters = Arc::new(Counters::default());
    // 两条规则：全部段落都含「★」（用来验证折叠），只有中段含「失眠」
    // （用来验证「取得分最高的块当证据」）。
    core.set_embedder(stub(&counters, vec![("失眠", unit(0)), ("★", unit(3))]));

    // 一篇超过上限的长来源：篇内按段尾切，切成多块。
    let text = long_piece_with_insomnia();
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

/// **两块同分**时当证据的那一块必须固定：同一份内容、同一个查询，摘录不能变。
///
/// 回归 issue [#57](https://github.com/xingxue-ux/unwritten-diary/issues/57)。原来的
/// 排序键在同分时先比 `chunk_id`，而 `chunk_id` 的哈希输入里有**篇标识**（`src_`/`cap_`
/// 前缀的 v7 UUID）——每个库都不一样。于是两个同分块的相对大小跟着抛硬币：同一篇的
/// A、B 两块得分一样时，跨进程/跨建库给出的摘录会变（测试红绿交替）。
///
/// 为什么必须建**多份库**：块 id 一旦写进某个库就不再变了，同一个库里反复建索引只会
/// 得到同一个赢家。跨进程红绿交替的根是「同一份内容 + 不同的篇 id」，所以这里在同一个
/// 进程里建多份独立的库（每轮 `import` 都生成新的篇 id）来复现它——12 轮里只要有一轮
/// 的证据与别轮不同就说明排序还在抛硬币。
#[test]
fn equal_score_evidence_is_the_same_across_libraries() {
    // 12 份库：旧逻辑下每份都是一次独立抛硬币，全对的概率是 2^-11。
    const ROUNDS: usize = 12;
    // 每轮留下的证据：摘录 + 定位里的文字区间。摘录取该 span 起点起的若干字，所以
    // 它变了就是「当证据的块」变了。**不**比整个 `locator`：它还带 `source_revision_id`，
    // 那本身就是每份库都不一样的 UUID，与这一次要钉的东西无关。
    let mut evidence: Vec<(Option<String>, Option<i64>, Option<i64>)> = Vec::new();
    let mut spans: Vec<(i64, i64)> = Vec::new();

    for round in 0..ROUNDS {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("library.sqlite");
        let mut core = new_core(dir.path());
        let counters = Arc::new(Counters::default());
        core.set_embedder(stub(&counters, vec![("失眠", unit(0))]));

        let text = long_piece_with_insomnia();
        let capture_id = write_capture(&mut core, at(2026, 9, 20), "", "op");
        let source_id = import_and_extract(
            &mut core,
            &capture_id,
            "长文.txt",
            &text,
            &format!("mat{round}"),
        );
        let built = core.build_semantic_index(None).unwrap();
        assert_eq!(
            built.total_chunks, 2,
            "前提（第 {round} 轮）：这篇长文在篇内被切成两块"
        );

        // 前提：两块都含查询词，桩才会给两块同一个单位向量 → 两块得分完全相同。
        let conn = open(&path);
        let with_word = count(
            &conn,
            "SELECT COUNT(*) FROM text_chunks WHERE text LIKE '%失眠%'",
            "text_chunks",
        );
        assert_eq!(
            with_word, 2,
            "前提（第 {round} 轮）：两块都含「失眠」，得分才会完全相同"
        );
        let round_spans: Vec<(i64, i64)> = {
            let mut statement = conn
                .prepare("SELECT start_char, end_char FROM chunk_spans ORDER BY start_char")
                .unwrap();
            statement
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap()
        };
        assert_eq!(round_spans.len(), 2, "两块各一段");
        assert_ne!(
            round_spans[0], round_spans[1],
            "前提（第 {round} 轮）：两块是不同的 span，才谈得上「谁当证据」"
        );
        if spans.is_empty() {
            spans = round_spans.clone();
        }
        assert_eq!(
            round_spans, spans,
            "前提：块边界是内容的函数，每份库都该一样"
        );
        drop(conn);

        // 同一个库里再建一代（内容没变）：证据也不该变。
        let first = search_mode(&mut core, "失眠", SearchMode::Semantic, 20);
        let first_hit = hit_of(&first, &source_id);
        core.build_semantic_index(None).unwrap();
        let second = search_mode(&mut core, "失眠", SearchMode::Semantic, 20);
        let second_hit = hit_of(&second, &source_id);
        assert_eq!(
            first_hit.snippet, second_hit.snippet,
            "第 {round} 轮：同一个库换一代，证据不该变"
        );

        evidence.push((
            first_hit.snippet.clone(),
            first_hit
                .locator
                .as_ref()
                .and_then(|locator| locator.text_start),
            first_hit
                .locator
                .as_ref()
                .and_then(|locator| locator.text_end),
        ));
    }

    // 决定性断言：所有库当证据的 span 必须完全一样。
    for (round, entry) in evidence.iter().enumerate() {
        assert_eq!(
            entry, &evidence[0],
            "第 {round} 份库给出的证据与第 0 份不同：{:?} vs {:?}（两块同分，兜底顺序必须由内容决定）",
            entry, evidence[0]
        );
    }
    // 兜底顺序的契约（见文档「消费契约」）：同分时取**篇内区间起点最靠前**的那一块。
    // 两块里起点最小的是 0，它的摘录从篇首开始；另一块（起点 343）的摘录带前导省略号。
    assert_eq!(spans[0].0, 0, "篇内起点最小的那一块从 0 开始");
    assert!(
        evidence[0]
            .0
            .as_deref()
            .unwrap_or_default()
            .starts_with("第0段 ★："),
        "同分时要稳定地留下篇内起点最靠前的那一块，实际摘录：{:?}",
        evidence[0].0
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
        snapshot
            .results
            .iter()
            .map(|hit| hit.hit_id.clone())
            .collect::<Vec<_>>(),
        first_ids
    );

    // 拿别的会话的游标来翻 → cursor_expired（用真游标，不是手拼的格式）。
    let other = search_mode(&mut core, "★", SearchMode::Semantic, 2);
    let foreign = other.cursor.clone().expect("别的会话也有下一页");
    let error = core
        .search_next_page(&first.session_id, Some(&foreign))
        .unwrap_err();
    assert_eq!(error.code(), diary_core::ErrorCode::CursorExpired);

    // 翻到底：不重不漏，末页之后再翻给空页。
    let mut seen = first_ids.clone();
    let mut current = first;
    while let Some(cursor) = current.cursor.clone() {
        current = core
            .search_next_page(&current.session_id, Some(&cursor))
            .unwrap();
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
    let error = core
        .search_next_page(&session.session_id, None)
        .unwrap_err();
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
    conn.execute(
        "UPDATE captures SET state = 'trashed' WHERE id = ?1",
        params![old],
    )
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
        fn embed_document(&self, _text: &str) -> diary_core::Result<diary_core::Embedding> {
            Ok(diary_core::Embedding::untruncated(vec![0.0; 8]))
        }
        fn embed_query(&self, _text: &str) -> diary_core::Result<diary_core::Embedding> {
            Ok(diary_core::Embedding::untruncated(vec![0.0; 8]))
        }
    }

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.sqlite");
    let mut core = new_core(dir.path());
    core.set_embedder(Box::new(WrongDims));
    write_capture(&mut core, at(2026, 9, 20), "妈妈打电话来。", "opA");

    let error = core.build_semantic_index(None).unwrap_err();
    assert!(error.to_string().contains("512"), "要说清维度不符：{error}");
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
    import_and_extract(
        &mut core,
        &a,
        "信.txt",
        "妈妈后来又写了一封信。\n",
        "opA-mat",
    );

    let before = search_mode(&mut core, "妈妈", SearchMode::Keyword, 20);
    let before_ids: Vec<String> = before
        .results
        .iter()
        .map(|hit| hit.hit_id.clone())
        .collect();
    assert_eq!(before_ids.len(), 2);

    core.build_semantic_index(None).unwrap();
    let after = search_mode(&mut core, "妈妈", SearchMode::Keyword, 20);
    assert_eq!(
        after
            .results
            .iter()
            .map(|hit| hit.hit_id.clone())
            .collect::<Vec<_>>(),
        before_ids,
        "关键词命中一行都不该因为建语义索引而变"
    );
    assert_eq!(after.index_coverage, before.index_coverage);
    assert_eq!(after.results[0].snippet, before.results[0].snippet);

    // 再建一代（换代中间走一遍），关键词照样不变。
    core.build_semantic_index(None).unwrap();
    let again = search_mode(&mut core, "妈妈", SearchMode::Keyword, 20);
    assert_eq!(
        again
            .results
            .iter()
            .map(|hit| hit.hit_id.clone())
            .collect::<Vec<_>>(),
        before_ids
    );
}

// ------------------------------------------------------------ #109：超长正文的截断与报告

/// 嵌入器报告了截断时，`SemanticBuildReport` 要**如实累计**块数与 token 数。
///
/// 只打日志不算报告：上层（作业进度、探针）拿到的必须是可断言的数字。这里用 2 块
/// × 每块 107 个 token 构造，断言 2 / 214。
#[test]
fn a_truncated_chunk_is_counted_in_the_build_report() {
    struct TruncatingEmbedder;

    impl Embedder for TruncatingEmbedder {
        fn model_version(&self) -> &str {
            "stub-embedder@truncated"
        }

        fn dims(&self) -> usize {
            512
        }

        fn options(&self) -> EmbedOptions {
            EmbedOptions::default()
        }

        fn embed_document(&self, text: &str) -> diary_core::Result<Embedding> {
            Ok(Embedding {
                vector: diary_core::text_hash_vector(text, 512),
                truncation: Some(TokenTruncation {
                    original_tokens: 617,
                    kept_tokens: diary_core::MAX_BODY_TOKENS,
                    dropped_tokens: 107,
                }),
            })
        }

        fn embed_query(&self, text: &str) -> diary_core::Result<Embedding> {
            Ok(Embedding::untruncated(diary_core::text_hash_vector(
                text, 512,
            )))
        }
    }

    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    core.set_embedder(Box::new(TruncatingEmbedder));
    write_capture(&mut core, at(2026, 9, 20), "妈妈打电话来。", "op-trunc-a");
    write_capture(&mut core, at(2026, 9, 21), "路边的桂花开了。", "op-trunc-b");

    let report = core.build_semantic_index(None).unwrap();
    assert_eq!(report.embedded_chunks, 2);
    assert_eq!(
        report.truncated_chunks, 2,
        "两块都被截断，报告里必须是 2（只打日志就等于没报）"
    );
    assert_eq!(
        report.truncated_tokens, 214,
        "107 × 2：报告要给出被截掉的 token 总数"
    );

    // 向量照写、检索照用：截断不是错误，也不该让索引失效。
    let status = core.index_status(None).unwrap();
    assert!(status.semantic_index_ready);
    assert!(
        !search_mode(&mut core, "妈妈", SearchMode::Semantic, 20)
            .results
            .is_empty(),
        "截断过的块仍然要能被语义召回"
    );
}

/// #109：一篇超长正文被切成多块之后，关键词、语义、混合三路都不报错、都还能召回。
///
/// 这条钉的是「修复不能把检索弄坏」：入口加了截断、报告加了字段，检索那条路的
/// 向量仍然是 512 维、块仍然按篇折叠。
#[test]
fn a_long_capture_stays_searchable_on_both_paths() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    let counters = Arc::new(Counters::default());
    // 规则把「含妈妈」的块与查询都映射到同一个单位向量：余弦 1，混合那一路才有
    // 证据过门槛（纯哈希向量的相似度只有 0.06，会被证据门槛挡掉）。
    core.set_embedder(stub(&counters, vec![("妈妈", unit(0))]));

    // 16 字 × 200 = 3200 字：按 400 字一块会切成 8 块以上（带 48 字重叠）。
    let text = "妈妈打电话来说她最近身体不太好。".repeat(200);
    let capture_id = write_capture(&mut core, at(2026, 9, 20), &text, "op-long");

    let report = core.build_semantic_index(None).unwrap();
    assert!(
        report.total_chunks > 1,
        "这么长的正文必须被切成多块（实际 {} 块）",
        report.total_chunks
    );
    assert_eq!(
        report.truncated_chunks, 0,
        "产品切块（400 字符）在 512 token 以内，正常不该有截断"
    );
    assert_eq!(report.embedded_chunks, report.total_chunks);

    let keyword = search_mode(&mut core, "妈妈", SearchMode::Keyword, 20);
    assert_eq!(
        keyword.results.first().map(|hit| hit.group_id.as_str()),
        Some(capture_id.as_str()),
        "关键词那一路不报错、仍能命中：{:?}",
        keyword.warnings
    );

    let semantic = search_mode(&mut core, "妈妈", SearchMode::Semantic, 20);
    assert!(
        !semantic.results.is_empty(),
        "语义那一路不报错、仍能命中：{:?}",
        semantic.warnings
    );
    assert!(
        semantic
            .results
            .iter()
            .all(|hit| hit.group_id == capture_id),
        "同一篇的多个块要按篇折叠成一条"
    );

    let hybrid = hybrid_mode(&mut core, "妈妈", 20);
    assert_eq!(
        hybrid.phase,
        SearchPhase::HybridReady,
        "混合那一路要能完成融合：{:?}",
        hybrid.warnings
    );
    assert!(!hybrid.results.is_empty(), "融合后不该凭空丢结果");
}

/// #109：查询超长被截断时，检索快照的 `warnings` 必须如实说出来（不是只打日志）。
#[test]
fn a_truncated_query_is_reported_in_the_search_warnings() {
    struct TruncatingQueryEmbedder;

    impl Embedder for TruncatingQueryEmbedder {
        fn model_version(&self) -> &str {
            "stub-embedder@truncated-query"
        }

        fn dims(&self) -> usize {
            512
        }

        fn options(&self) -> EmbedOptions {
            EmbedOptions::default()
        }

        fn embed_document(&self, text: &str) -> diary_core::Result<Embedding> {
            Ok(Embedding::untruncated(diary_core::text_hash_vector(
                text, 512,
            )))
        }

        fn embed_query(&self, text: &str) -> diary_core::Result<Embedding> {
            Ok(Embedding {
                vector: diary_core::text_hash_vector(text, 512),
                truncation: Some(TokenTruncation {
                    original_tokens: 617,
                    kept_tokens: diary_core::MAX_BODY_TOKENS,
                    dropped_tokens: 107,
                }),
            })
        }
    }

    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    core.set_embedder(Box::new(TruncatingQueryEmbedder));
    write_capture(&mut core, at(2026, 9, 20), "妈妈打电话来。", "op-query");
    core.build_semantic_index(None).unwrap();

    let semantic = search_mode(&mut core, "很长很长的查询", SearchMode::Semantic, 20);
    let warning = semantic
        .warnings
        .iter()
        .find(|warning| warning.contains("查询太长"))
        .unwrap_or_else(|| panic!("查询截断必须在 warnings 里说：{:?}", semantic.warnings));
    assert!(
        warning.contains("107"),
        "警告里要带被截掉的 token 数：{warning}"
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
    import_and_extract(
        &mut core,
        &b,
        "聚餐.txt",
        "晚上和同事去吃了火锅，聊得很开心。\n",
        "opB-mat",
    );
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
        snapshot.results[0].group_id,
        source_a,
        "语义应当把「凌晨两点还醒着」排在最前：{:?}",
        snapshot
            .results
            .iter()
            .map(|hit| (&hit.group_id, &hit.snippet))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        snapshot.results[0].matched_by,
        vec![diary_core::MatchedBy::Semantic]
    );
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
// ------------------------------------------------------------ 混合检索（#51，RRF）

/// 一篇来源在库里的片段文字（混合的正文哈希就是按它算的）。
fn segment_text_of(path: &Path, source_id: &str) -> String {
    open(path)
        .query_row(
            "SELECT s.text FROM extracted_segments s \
             JOIN extracted_contents ec ON ec.id = s.content_id \
             WHERE ec.source_id = ?1 ORDER BY s.ordinal LIMIT 1",
            params![source_id],
            |row| row.get(0),
        )
        .unwrap()
}

/// 与 `support::content_hash` 同一个口径（测试侧独立算一遍，不调产品代码）。
fn content_hash(text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// 融合的单位是**篇**，分数是 RRF：关键词只中 A、语义只中 B、两者都中 C。
///
/// 构造（三天各一篇，各自成块）：
/// - A：正文含查询词「开源节流」→ 关键词中；向量与查询正交 → 语义不中；
/// - B：正文不含查询词 → 关键词不中；向量与查询同向 → 语义中；
/// - C：两样都中。
///
/// 期望顺序（k = 60，名次从 1 起）：
/// `C = 1/61 + 1/61 > B = 1/62 = A = 1/62`；B 与 A 同分，B 的日期更近 → 在前。
#[test]
fn hybrid_fuses_two_paths_with_rrf_and_reports_matched_by() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    let counters = Arc::new(Counters::default());
    core.set_embedder(stub_split(
        &counters,
        // 文档侧：「难以入睡」→ 与查询同向；「开源节流」→ 与查询正交。
        vec![("难以入睡", unit(0)), ("开源节流", unit(1))],
        // 查询侧：正文里那个关键词出现时，查询向量是 unit(0)。
        vec![("开源节流", unit(0))],
    ));

    let a_capture = write_capture(&mut core, at(2026, 9, 1), "", "opA");
    let a = import_and_extract(
        &mut core,
        &a_capture,
        "A.txt",
        "开源节流这件事说起来容易，做起来难。\n",
        "matA",
    );
    let b_capture = write_capture(&mut core, at(2026, 9, 2), "", "opB");
    let b = import_and_extract(
        &mut core,
        &b_capture,
        "B.txt",
        "一到夜里就难以入睡，翻来覆去。\n",
        "matB",
    );
    let c_capture = write_capture(&mut core, at(2026, 9, 3), "", "opC");
    let c = import_and_extract(
        &mut core,
        &c_capture,
        "C.txt",
        "开源节流加上难以入睡，两样一起压过来。\n",
        "matC",
    );
    core.build_semantic_index(None).unwrap();

    // —— 第一阶段（#69）：只有关键词那一路。B 只被语义召回，**不该**出现在这里 ——
    let started = search_mode(&mut core, "开源节流", SearchMode::Hybrid, 20);
    assert_eq!(
        started.phase,
        SearchPhase::KeywordReady,
        "hybrid 首屏必须是 keyword_ready（融合还没算）"
    );
    let first_order: Vec<&str> = started
        .results
        .iter()
        .map(|hit| hit.group_id.as_str())
        .collect();
    assert_eq!(
        first_order,
        vec![c.as_str(), a.as_str()],
        "关键词那一路：C（9/3）比 A（9/1）近，B 一条都没有"
    );
    assert!(
        !started.results.iter().any(|hit| hit.group_id == b),
        "只被语义召回到的那篇在第一阶段不许出现：{:?}",
        first_order
    );
    assert!(
        started
            .results
            .iter()
            .all(|hit| hit.matched_by == vec![MatchedBy::Keyword]),
        "第一阶段 matchedBy 如实只反映关键词，不给语义填假值：{:?}",
        started
            .results
            .iter()
            .map(|hit| (&hit.group_id, &hit.matched_by))
            .collect::<Vec<_>>()
    );

    // —— 第二阶段：显式升级才融合，B 被补进来并按 RRF 重新排序 ——
    let snapshot = core.upgrade_search_to_hybrid(&started.session_id).unwrap();
    assert_eq!(
        snapshot.phase,
        SearchPhase::HybridReady,
        "升级之后才允许报 hybrid_ready"
    );
    let order: Vec<&str> = snapshot
        .results
        .iter()
        .map(|hit| hit.group_id.as_str())
        .collect();
    assert_eq!(
        order,
        vec![c.as_str(), b.as_str(), a.as_str()],
        "RRF 顺序（C 两路都中 > B 语义 > A 关键词；B/A 同分时日期近的在前）：{:?}",
        snapshot
            .results
            .iter()
            .map(|hit| (&hit.group_id, &hit.matched_by))
            .collect::<Vec<_>>()
    );
    assert!(
        hit_of(&snapshot, &b).matched_by == vec![MatchedBy::Semantic],
        "B 是语义独有的那篇，位置在第 2（融合后的顺序）"
    );

    assert_eq!(
        hit_of(&snapshot, &c).matched_by,
        vec![MatchedBy::Keyword, MatchedBy::Semantic],
        "两路都中要如实标成两者都有"
    );
    assert_eq!(hit_of(&snapshot, &b).matched_by, vec![MatchedBy::Semantic]);
    assert_eq!(hit_of(&snapshot, &a).matched_by, vec![MatchedBy::Keyword]);
    // 两路都有时用关键词那份正文：带高亮；语义独有那份没有假高亮。
    assert!(
        !hit_of(&snapshot, &c).highlights.is_empty(),
        "两路都中时用关键词那份证据，应当带高亮"
    );
    assert!(
        hit_of(&snapshot, &b).highlights.is_empty(),
        "语义命中不给假高亮"
    );
}

/// 语义证据门槛：低于门槛 → 这一路没有证据；关键词也空 → 返回空（不许放宽），
/// 并且**不融合**——阶段如实停在 `keyword_ready`（#60 复审）。
///
/// 顺便钉住门槛确实是那条闸门：同一条查询、同一份数据，把门槛降下来就该命中。
#[test]
fn hybrid_treats_semantic_below_the_threshold_as_no_evidence() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    let counters = Arc::new(Counters::default());
    // 查询向量与这一篇的余弦正好 0.20：低于产品默认门槛 `HYBRID_SEMANTIC_MIN_SCORE`。
    let mut weak = vec![0_f32; 512];
    weak[0] = 0.2;
    weak[1] = (1.0_f32 - 0.2 * 0.2).sqrt();
    core.set_embedder(stub_split(
        &counters,
        vec![("难以入睡", unit(0))],
        vec![("失眠", weak)],
    ));
    let capture = write_capture(&mut core, at(2026, 9, 20), "", "opA");
    let source = import_and_extract(
        &mut core,
        &capture,
        "睡眠.txt",
        "夜里难以入睡，翻来覆去。\n",
        "matA",
    );
    core.build_semantic_index(None).unwrap();

    // 关键词那一路没对上（查询是「失眠」，正文里没有这两个字）+ 语义低于门槛 → 空。
    let snapshot = hybrid_mode(&mut core, "失眠", 20);
    assert_eq!(
        snapshot.phase,
        SearchPhase::KeywordReady,
        "两路都空（关键词空 + 语义全被门槛滤掉）时不许假装 hybrid_ready：{:?}",
        snapshot.warnings
    );
    assert!(
        snapshot.results.is_empty(),
        "关键词空 + 语义低于门槛 → 必须返回空：{:?}",
        snapshot
            .results
            .iter()
            .map(|hit| &hit.group_id)
            .collect::<Vec<_>>()
    );
    assert!(
        snapshot
            .warnings
            .iter()
            .any(|warning| warning.contains("低于证据门槛")),
        "要说清语义为什么没有贡献：{:?}",
        snapshot.warnings
    );
    assert!(
        snapshot
            .warnings
            .iter()
            .any(|warning| warning.contains("两路都没有达到证据门槛")),
        "空结果也要说清（不能假装「没有内容」）：{:?}",
        snapshot.warnings
    );

    // 同一份数据、同一条查询，只把门槛降到 0.10：应当命中，并且如实标成语义。
    core.set_hybrid_min_score_for_test(0.10);
    let loosened = hybrid_mode(&mut core, "失眠", 20);
    assert_eq!(loosened.results.len(), 1, "{:?}", loosened.warnings);
    assert_eq!(loosened.results[0].group_id, source);
    assert_eq!(loosened.results[0].matched_by, vec![MatchedBy::Semantic]);
}

/// **关键词有结果、语义全部低于门槛**（#60 复审）：语义这一路拿不出可融合的证据，
/// 就不许融合——阶段停在 `keyword_ready`，结果保持**关键词那份快照**（不是空、也不是
/// 被滤空的融合结果），警告照旧；重复升级幂等（不重复 embed、阶段不会莫名变成
/// `hybrid_ready`）。
#[test]
fn hybrid_upgrade_keeps_the_keyword_snapshot_when_every_semantic_candidate_is_filtered() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    let counters = Arc::new(Counters::default());
    // 查询「开源节流」→ unit(0)；含查询词的正文 → unit(1)：两篇候选都存在、余弦为 0，
    // 全部低于 `HYBRID_SEMANTIC_MIN_SCORE`（是「被门槛滤掉」，不是「没有候选」）。
    core.set_embedder(stub_split(
        &counters,
        vec![("开源节流", unit(1))],
        vec![("开源节流", unit(0))],
    ));
    let a_capture = write_capture(&mut core, at(2026, 9, 2), "", "opA");
    let a = import_and_extract(
        &mut core,
        &a_capture,
        "A.txt",
        "开源节流，先把账记清。\n",
        "matA",
    );
    let b_capture = write_capture(&mut core, at(2026, 9, 1), "", "opB");
    let b = import_and_extract(
        &mut core,
        &b_capture,
        "B.txt",
        "开源节流以后再说。\n",
        "matB",
    );
    core.build_semantic_index(None).unwrap();

    let started = search_mode(&mut core, "开源节流", SearchMode::Hybrid, 20);
    assert_eq!(started.phase, SearchPhase::KeywordReady, "首屏只算关键词");
    assert_eq!(
        started
            .results
            .iter()
            .map(|hit| hit.group_id.as_str())
            .collect::<Vec<_>>(),
        vec![a.as_str(), b.as_str()],
        "前提：关键词那一路有结果，按日期 A（9/2）在前"
    );

    let upgraded = core.upgrade_search_to_hybrid(&started.session_id).unwrap();
    assert_eq!(
        upgraded.phase,
        SearchPhase::KeywordReady,
        "语义全被门槛滤掉 ⇒ 不融合，阶段必须停在 keyword_ready：{:?}",
        upgraded.warnings
    );
    assert_eq!(
        upgraded.results, started.results,
        "结果保持关键词那份快照，不是空、也不是被滤空的融合结果"
    );
    assert_eq!(
        upgraded.cursor, started.cursor,
        "结果集没有被替换，游标（与代次）不变，升级前的游标继续可用"
    );
    assert!(
        upgraded
            .warnings
            .iter()
            .any(|warning| warning.contains("低于证据门槛")
                && warning.contains("这一路没有贡献结果")),
        "现有的「语义这一路没有贡献证据」警告必须保留：{:?}",
        upgraded.warnings
    );

    // 幂等：重复升级返回同一份快照，不重复 embed，阶段也不会莫名变成 hybrid_ready。
    let queries = counters.queries.load(Ordering::SeqCst);
    let again = core.upgrade_search_to_hybrid(&started.session_id).unwrap();
    assert_eq!(again, upgraded, "重复升级返回同一份快照");
    assert_eq!(again.phase, SearchPhase::KeywordReady);
    assert_eq!(
        counters.queries.load(Ordering::SeqCst),
        queries,
        "重复升级不重复算查询向量"
    );
}

/// **同分**时谁排前面必须由内容决定，跨库一致（照 issue #57 的方法）。
///
/// 构造：同一天两篇，正文都以查询词开头（关键词的出现位置都是 1、家族都是片段）
/// → 关键词名次**并列**，RRF 分完全相同；语义这一路对这两篇都拿不出证据（向量与查询
/// 正交），但另一天另有**第三篇**只被语义命中、RRF 分同样是 `1/(k+1)` → 三篇总分仍
/// 然并列，A/B 的分野只剩同分键：正文哈希（内容决定）。
///
/// 第三篇（不同天）是必须的：语义这一路**至少得有一条候选过门槛**，融合才会发生
/// （#60 复审：全被门槛滤掉时不融合、阶段停在 `keyword_ready`）。它只给融合提供证据，
/// 不动 A/B 的并列关系，所以同分键仍然是被测的那条闸门。
///
/// 旧写法（同分先比 `groupId` / `chunk_id` 这类 v7 UUID）下，12 份库就是 12 次抛硬币；
/// 这条测试在临时退回旧键时**验证过会红**，见文档。
#[test]
fn hybrid_tie_break_is_the_same_across_libraries() {
    const ROUNDS: usize = 12;
    let mut seen: Vec<Vec<String>> = Vec::new();
    for round in 0..ROUNDS {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("library.sqlite");
        let mut core = new_core(dir.path());
        let counters = Arc::new(Counters::default());
        core.set_embedder(stub_split(
            &counters,
            // 文档侧：含查询词 → unit(1)，与查询 unit(0) 正交（对 A/B 没有语义证据）；
            // 含「睡意」→ unit(0)，与查询同向（C 是唯一的语义证据）。
            vec![("开源节流", unit(1)), ("睡意", unit(0))],
            // 查询侧：unit(0)。
            vec![("开源节流", unit(0))],
        ));

        let a_capture = write_capture(&mut core, at(2026, 9, 20), "", &format!("opA{round}"));
        let a = import_and_extract(
            &mut core,
            &a_capture,
            "A.txt",
            "开源节流，先把账记清。\n",
            &format!("matA{round}"),
        );
        let b_capture = write_capture(&mut core, at(2026, 9, 20), "", &format!("opB{round}"));
        let b = import_and_extract(
            &mut core,
            &b_capture,
            "B.txt",
            // 这篇的正文哈希比 A 小：正确的顺序（哈希升序）与「先建库的那一篇
            // 在前」（旧键：groupId / doc_id 的顺序）**相反**，所以旧键必然报红。
            "开源节流以后再说。\n",
            &format!("matB{round}"),
        );
        // C 在**另一天**：块按天打包（`pack_pieces`），同一天会被并进同一个块，
        // 那样它的向量就跟着 A/B 一起走了。不同天才能让它单独成为语义证据。
        let c_capture = write_capture(&mut core, at(2026, 9, 19), "", &format!("opC{round}"));
        let c = import_and_extract(
            &mut core,
            &c_capture,
            "C.txt",
            "睡意上来了，什么都想不起来。\n",
            &format!("matC{round}"),
        );
        core.build_semantic_index(None).unwrap();

        let snapshot = hybrid_mode(&mut core, "开源节流", 20);
        assert_eq!(
            snapshot.phase,
            SearchPhase::HybridReady,
            "第 {round} 轮：C 提供了语义证据，融合应当真的发生：{:?}",
            snapshot.warnings
        );
        assert_eq!(snapshot.results.len(), 3, "三篇都要在结果里");
        assert!(
            !snapshot
                .warnings
                .iter()
                .any(|warning| warning.contains("低于证据门槛")),
            "前提（第 {round} 轮）：语义这一路有证据（C），不该再说「没有贡献」：{:?}",
            snapshot.warnings
        );
        // A/B 与 C 同分（都是 `1/(k+1)`），但 A/B 在更近的一天（9/20）→ 排在 C 前。
        assert_eq!(
            snapshot.results[2].group_id, c,
            "C 在另一天，同分时排在后面"
        );
        // A/B 的顺序按**正文哈希**记下来（篇标识是每库不同的 UUID，跨库比它没有意义）。
        let ab_order: Vec<String> = snapshot.results[..2]
            .iter()
            .map(|hit| content_hash(&segment_text_of(&path, &hit.group_id)))
            .collect();
        let mut sorted = ab_order.clone();
        sorted.sort();
        assert_eq!(
            ab_order, sorted,
            "第 {round} 轮：A/B 同分兜底不是由内容（正文哈希升序）决定的"
        );
        assert_ne!(
            content_hash(&segment_text_of(&path, &a)),
            content_hash(&segment_text_of(&path, &b)),
            "前提：两篇正文不同，哈希才有区分度"
        );
        seen.push(ab_order);
    }
    for (round, order) in seen.iter().enumerate() {
        assert_eq!(
            order, &seen[0],
            "第 {round} 份库的顺序与第 0 份不同：同分键跨库不稳定"
        );
    }
}

/// 混合的翻页 / 游标归属 / 快照与关键词那一路同一套语义。
#[test]
fn hybrid_sessions_page_and_snapshot_like_keyword_ones() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    let counters = Arc::new(Counters::default());
    core.set_embedder(stub_split(
        &counters,
        vec![("睡前", unit(0))],
        vec![("睡前", unit(0))],
    ));
    // 五篇不同天、篇篇都含查询词：两路都中。
    let mut sources = Vec::new();
    for day in 1..=5 {
        let capture = write_capture(&mut core, at(2026, 9, 10 + day), "", &format!("op{day}"));
        sources.push(import_and_extract(
            &mut core,
            &capture,
            &format!("D{day}.txt"),
            &format!("第{day}天睡前想了很多事情。\n"),
            &format!("mat{day}"),
        ));
    }
    core.build_semantic_index(None).unwrap();

    let all = hybrid_mode(&mut core, "睡前", 20);
    assert_eq!(all.results.len(), 5);
    for source in &sources {
        assert!(
            all.results.iter().any(|hit| &hit.group_id == source),
            "每篇都该在结果里：{source}"
        );
    }

    let request = |page_size: i64| SearchRequest {
        query: "睡前".to_owned(),
        mode: SearchMode::Hybrid,
        filters: SearchFilters::default(),
        page_size,
    };
    // —— 升级之前：翻页只给关键词结果，阶段一直是 keyword_ready ——
    const REVISION: i64 = 42;
    let first = core.start_search(request(2), REVISION).unwrap();
    assert_eq!(first.results.len(), 2);
    assert_eq!(
        first.phase,
        SearchPhase::KeywordReady,
        "还没升级，阶段如实停在 keyword_ready"
    );
    assert_eq!(first.query_revision, REVISION);
    let keyword_cursor = first.cursor.clone().expect("还有下一页");
    let keyword_second = core
        .search_next_page(&first.session_id, Some(&keyword_cursor))
        .unwrap();
    assert_eq!(keyword_second.results.len(), 2);
    assert_eq!(
        keyword_second.phase,
        SearchPhase::KeywordReady,
        "升级前的翻页也不许假装融合过"
    );
    // `search.snapshot` 给的是「当前这一页」，而且是幂等的。
    let snapshot = core.search_snapshot(&first.session_id).unwrap();
    assert_eq!(snapshot.results, keyword_second.results);
    assert_eq!(snapshot.phase, SearchPhase::KeywordReady);
    assert_eq!(core.search_snapshot(&first.session_id).unwrap(), snapshot);

    // —— 升级：从头给融合结果，阶段变成 hybrid_ready，仍是同一个会话 ——
    let upgraded = core.upgrade_search_to_hybrid(&first.session_id).unwrap();
    assert_eq!(upgraded.phase, SearchPhase::HybridReady);
    assert_eq!(
        upgraded.session_id, first.session_id,
        "升级是同一会话，不新造一个"
    );
    assert_eq!(upgraded.query_revision, REVISION, "升级沿用同一个查询修订");
    assert_eq!(upgraded.results.len(), 2);
    let upgrade_cursor = upgraded.cursor.clone().expect("还有下一页");
    // 幂等：再升级给同一份快照。
    assert_eq!(
        core.upgrade_search_to_hybrid(&first.session_id).unwrap(),
        upgraded
    );

    // 升级前的深页游标对不上融合后的进度 → 如实 cursor_expired，不悄悄错位返回。
    let error = core
        .search_next_page(&first.session_id, keyword_second.cursor.as_deref())
        .unwrap_err();
    assert_eq!(
        error.code(),
        diary_core::ErrorCode::CursorExpired,
        "升级前的深页游标不能接着翻融合结果"
    );

    // —— 升级之后：翻页给融合结果，阶段保持 hybrid_ready，翻完照旧 done ——
    let second = core
        .search_next_page(&first.session_id, Some(&upgrade_cursor))
        .unwrap();
    assert_eq!(second.results.len(), 2);
    assert_eq!(second.phase, SearchPhase::HybridReady);
    let third = core
        .search_next_page(&first.session_id, second.cursor.as_deref())
        .unwrap();
    assert_eq!(third.results.len(), 1);
    assert!(third.cursor.is_none(), "末页没有下一页游标");
    assert_eq!(third.phase, SearchPhase::Done, "翻完照旧 done");

    let mut paged: Vec<SearchHit> = Vec::new();
    paged.extend(upgraded.results.clone());
    paged.extend(second.results.clone());
    paged.extend(third.results.clone());
    assert_eq!(
        paged, all.results,
        "升级后的分页拼起来必须与「一次取全的融合结果」一致"
    );

    // 拿别的会话的游标翻页要被拒（游标绑定原会话）。
    let other = core.start_search(request(2), 2).unwrap();
    let error = core
        .search_next_page(&other.session_id, Some(&upgrade_cursor))
        .unwrap_err();
    assert_eq!(error.code(), diary_core::ErrorCode::CursorExpired);
}

/// 评审（#71）指出的分页正确性 bug：`pageSize = 2` 时，升级前**第一页**的游标与
/// 升级后第一页的游标偏移相同（都是 2）。老格式只带 `<sessionId>:<offset>`，只看
/// 偏移的校验会放它进来、返回**融合结果的第二页**——界面若留着关键词阶段的第一页，
/// 就会把两个阶段的结果拼起来（乱序 / 重复 / 漏项）。
///
/// 修法：游标带上**结果代次**（`<sessionId>:<generation>:<offset>`，会话内从 0 起，
/// 结果集被替换时递增）。代次不一致一律 `cursor_expired`，**不看偏移**；代次一致才
/// 校验偏移边界。这里的断言在撤掉代次校验时必须变红。
#[test]
fn hybrid_upgrade_expires_every_pre_upgrade_cursor_even_at_the_same_offset() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    let counters = Arc::new(Counters::default());
    core.set_embedder(stub_split(
        &counters,
        vec![("睡前", unit(0))],
        vec![("睡前", unit(0))],
    ));
    // 五篇不同天、篇篇都含查询词：两路都中，融合结果 5 篇，每页 2 条 → 3 页。
    for day in 1..=5 {
        let capture = write_capture(&mut core, at(2026, 9, 10 + day), "", &format!("op{day}"));
        import_and_extract(
            &mut core,
            &capture,
            &format!("D{day}.txt"),
            &format!("第{day}天睡前想了很多事情。\n"),
            &format!("mat{day}"),
        );
    }
    core.build_semantic_index(None).unwrap();

    let all = hybrid_mode(&mut core, "睡前", 20);
    assert_eq!(all.results.len(), 5, "前提：融合结果有 5 篇，每页 2 条");

    // —— 升级前：第一页 + 深页各拿一个游标 ——
    let first = core
        .start_search(
            SearchRequest {
                query: "睡前".to_owned(),
                mode: SearchMode::Hybrid,
                filters: SearchFilters::default(),
                page_size: 2,
            },
            7,
        )
        .unwrap();
    assert_eq!(first.results.len(), 2);
    let stale_first = first.cursor.clone().expect("升级前还有下一页");
    let keyword_second = core
        .search_next_page(&first.session_id, Some(&stale_first))
        .unwrap();
    let stale_deep = keyword_second.cursor.clone().expect("升级前还有更深的页");

    // —— 升级：融合重排、从头给结果，结果集被替换 ——
    let upgraded = core.upgrade_search_to_hybrid(&first.session_id).unwrap();
    assert_eq!(upgraded.phase, SearchPhase::HybridReady);
    assert_eq!(upgraded.results.len(), 2);
    let fresh_cursor = upgraded.cursor.clone().expect("融合结果还有下一页");

    // 评审场景：升级前**第一页**游标的偏移与融合后第一页相同，也必须过期。
    let error = core
        .search_next_page(&first.session_id, Some(&stale_first))
        .unwrap_err();
    assert_eq!(
        error.code(),
        diary_core::ErrorCode::CursorExpired,
        "升级前第一页的游标不能接着翻融合结果（哪怕偏移与融合后第一页相同）"
    );

    // 升级前的深页游标照旧过期（代次校验先于偏移校验）。
    let error = core
        .search_next_page(&first.session_id, Some(&stale_deep))
        .unwrap_err();
    assert_eq!(error.code(), diary_core::ErrorCode::CursorExpired);

    // —— 幂等升级不递增代次：升级后发出的游标在再次升级之后仍然可用 ——
    assert_eq!(
        core.upgrade_search_to_hybrid(&first.session_id).unwrap(),
        upgraded,
        "幂等升级返回同一份快照（同一代次、同一个第一页游标）"
    );
    let second = core
        .search_next_page(&first.session_id, Some(&fresh_cursor))
        .unwrap();
    assert_eq!(second.results.len(), 2);
    assert_eq!(second.phase, SearchPhase::HybridReady);
    let second_cursor = second.cursor.clone().expect("还有最后一页");
    // 翻到中途再升级一次：代次不能变，已经发出的第二页游标必须还能用。
    let _ = core.upgrade_search_to_hybrid(&first.session_id).unwrap();
    let third = core
        .search_next_page(&first.session_id, Some(second_cursor.as_str()))
        .unwrap();
    assert_eq!(third.results.len(), 1);
    assert!(third.cursor.is_none(), "末页没有下一页游标");
    assert_eq!(third.phase, SearchPhase::Done, "翻完照旧 done");

    // 升级后的游标逐页推进：拼起来 = 一次取全的融合结果，不漏不重。
    let mut paged: Vec<SearchHit> = upgraded.results.clone();
    paged.extend(second.results.clone());
    paged.extend(third.results.clone());
    assert_eq!(
        paged, all.results,
        "升级后的分页拼起来必须与「一次取全的融合结果」一致"
    );

    // 幂等升级之后旧代次的游标仍然过期：代次不会退回 0。
    let error = core
        .search_next_page(&first.session_id, Some(&stale_first))
        .unwrap_err();
    assert_eq!(error.code(), diary_core::ErrorCode::CursorExpired);
}

/// **真的会重排**（#69 验收）：关键词那一路先给出的顺序，融合后因为 B 又被语义命中
/// （两路相加的 RRF 分更高）而翻到前面。也顺便钉住**升级只算一次**（幂等）。
#[test]
fn hybrid_upgrade_reorders_what_keyword_already_had() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    let counters = Arc::new(Counters::default());
    core.set_embedder(stub_split(
        &counters,
        // 文档侧：先匹配「难以入睡」（只 B 走这条）；A 只含关键词，向量与查询正交。
        vec![("难以入睡", unit(0)), ("开源节流", unit(1))],
        // 查询侧：正文里那个关键词出现时，查询向量是 unit(0)。
        vec![("开源节流", unit(0))],
    ));
    // A 更近（9/2）→ 关键词名次 1；B 更早（9/1）→ 名次 2。
    let a_capture = write_capture(&mut core, at(2026, 9, 2), "", "opA");
    let a = import_and_extract(
        &mut core,
        &a_capture,
        "A.txt",
        "开源节流，先把账记清。\n",
        "matA",
    );
    let b_capture = write_capture(&mut core, at(2026, 9, 1), "", "opB");
    let b = import_and_extract(
        &mut core,
        &b_capture,
        "B.txt",
        "开源节流之外，一到夜里就难以入睡。\n",
        "matB",
    );
    core.build_semantic_index(None).unwrap();

    let started = search_mode(&mut core, "开源节流", SearchMode::Hybrid, 20);
    assert_eq!(started.phase, SearchPhase::KeywordReady);
    let first_order: Vec<&str> = started
        .results
        .iter()
        .map(|hit| hit.group_id.as_str())
        .collect();
    assert_eq!(
        first_order,
        vec![a.as_str(), b.as_str()],
        "关键词阶段按日期：A（9/2）在前"
    );

    let fused = core.upgrade_search_to_hybrid(&started.session_id).unwrap();
    assert_eq!(fused.phase, SearchPhase::HybridReady);
    let order: Vec<&str> = fused
        .results
        .iter()
        .map(|hit| hit.group_id.as_str())
        .collect();
    assert_eq!(
        order,
        vec![b.as_str(), a.as_str()],
        "B 两路都中（1/62 + 1/61 > A 的 1/61），融合后被排到 A 前面"
    );
    assert_eq!(
        hit_of(&fused, &b).matched_by,
        vec![MatchedBy::Keyword, MatchedBy::Semantic]
    );
    assert_eq!(hit_of(&fused, &a).matched_by, vec![MatchedBy::Keyword]);

    // 幂等升级**不重复算**：查询向量只 embed 一次。
    let queries = counters.queries.load(Ordering::SeqCst);
    assert_eq!(
        core.upgrade_search_to_hybrid(&started.session_id).unwrap(),
        fused
    );
    assert_eq!(
        counters.queries.load(Ordering::SeqCst),
        queries,
        "重复升级不重复算向量"
    );
}

/// 空查询的边界：没有可匹配的内容，`start` 给空结果；升级也**没有任何可融合的证据**
/// （关键词空 + 语义没有候选），阶段如实停在 `keyword_ready`、结果仍是空的——
/// 「空就是最终答案」不等于「假装融合过」（#60 复审）。**语义不可用**时同样不许假装，
/// 走的是 `Unavailable` 那条路径。
#[test]
fn hybrid_upgrade_on_an_empty_query_still_checks_semantic_evidence() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    let counters = Arc::new(Counters::default());
    core.set_embedder(stub(&counters, vec![]));
    let capture = write_capture(&mut core, at(2026, 9, 20), "", "op");
    import_and_extract(&mut core, &capture, "D.txt", "睡前想了很多事情。\n", "mat");
    core.build_semantic_index(None).unwrap();

    let empty = |mode: SearchMode| SearchRequest {
        query: "   ".to_owned(),
        mode,
        filters: SearchFilters::default(),
        page_size: 10,
    };

    let started = core.start_search(empty(SearchMode::Hybrid), 1).unwrap();
    assert_eq!(started.phase, SearchPhase::KeywordReady);
    assert!(started.results.is_empty());
    let upgraded = core.upgrade_search_to_hybrid(&started.session_id).unwrap();
    assert_eq!(
        upgraded.phase,
        SearchPhase::KeywordReady,
        "两路都空（空查询）时不许假装 hybrid_ready：{:?}",
        upgraded.warnings
    );
    assert!(upgraded.results.is_empty());
    assert!(
        upgraded
            .warnings
            .iter()
            .any(|warning| warning.contains("查询是空的")),
        "要如实说清查不出东西的原因：{:?}",
        upgraded.warnings
    );
    // 幂等：再升级一次给同一份快照，阶段不会莫名变成 hybrid_ready。
    let again = core.upgrade_search_to_hybrid(&started.session_id).unwrap();
    assert_eq!(again, upgraded, "重复升级返回同一份快照");
    assert_eq!(again.phase, SearchPhase::KeywordReady);

    // 语义不可用（另一个库，没建代次）：升级不许假装。
    let other_dir = tempfile::tempdir().unwrap();
    let mut other = new_core(other_dir.path());
    let started = other.start_search(empty(SearchMode::Hybrid), 2).unwrap();
    let upgraded = other.upgrade_search_to_hybrid(&started.session_id).unwrap();
    assert_eq!(
        upgraded.phase,
        SearchPhase::KeywordReady,
        "语义不可用时，空查询的升级也不许报 hybrid_ready：{:?}",
        upgraded.warnings
    );
}

/// 升级要**如实**对待会话状态（#69 第 6 条）：取消（升级前 / 后）、过期、非混合
/// 会话都不许静默给一份融合结果。
#[test]
fn hybrid_upgrade_respects_cancel_expiry_and_non_hybrid_sessions() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    let counters = Arc::new(Counters::default());
    core.set_embedder(stub_split(
        &counters,
        vec![("睡前", unit(0))],
        vec![("睡前", unit(0))],
    ));
    let capture = write_capture(&mut core, at(2026, 9, 20), "", "op");
    import_and_extract(&mut core, &capture, "D.txt", "睡前想了很多事情。\n", "mat");
    core.build_semantic_index(None).unwrap();

    let request = |mode: SearchMode| SearchRequest {
        query: "睡前".to_owned(),
        mode,
        filters: SearchFilters::default(),
        page_size: 10,
    };

    // 升级前取消：升级不融合，返回 cancelled 快照。
    let before = core.start_search(request(SearchMode::Hybrid), 1).unwrap();
    core.cancel_search(&before.session_id).unwrap();
    let upgraded = core.upgrade_search_to_hybrid(&before.session_id).unwrap();
    assert_eq!(
        upgraded.phase,
        SearchPhase::Cancelled,
        "取消就是取消，不静默给一份融合结果"
    );
    assert_eq!(upgraded.cursor, None);

    // 升级后取消：再升级仍是 cancelled。
    let after = core.start_search(request(SearchMode::Hybrid), 2).unwrap();
    let fused = core.upgrade_search_to_hybrid(&after.session_id).unwrap();
    assert_eq!(fused.phase, SearchPhase::HybridReady);
    core.cancel_search(&after.session_id).unwrap();
    let cancelled = core.upgrade_search_to_hybrid(&after.session_id).unwrap();
    assert_eq!(cancelled.phase, SearchPhase::Cancelled);

    // 过期：会话期间索引变了 → search_expired，让前端重新发起（与 snapshot/nextPage 同口径）。
    let expiring = core.start_search(request(SearchMode::Hybrid), 3).unwrap();
    let another = write_capture(&mut core, at(2026, 9, 21), "", "op2");
    import_and_extract(&mut core, &another, "E.txt", "睡前又想到别的。\n", "mat2");
    let error = core
        .upgrade_search_to_hybrid(&expiring.session_id)
        .unwrap_err();
    assert_eq!(error.code(), diary_core::ErrorCode::SearchExpired);

    // 非混合会话：明确报 invalid_state，不说「升级成功了」。
    let keyword = core.start_search(request(SearchMode::Keyword), 4).unwrap();
    let error = core
        .upgrade_search_to_hybrid(&keyword.session_id)
        .unwrap_err();
    assert_eq!(error.code(), diary_core::ErrorCode::InvalidState);

    // 会话不存在：search_expired。
    let error = core.upgrade_search_to_hybrid("search-missing").unwrap_err();
    assert_eq!(error.code(), diary_core::ErrorCode::SearchExpired);
}

/// 融合口径是可审计常量：版本号里带着 k / 深度 / 门槛。改了常量却不同步改版本号，
/// 这条测试就红——照 `CHUNKER_VERSION` 的先例。
#[test]
fn hybrid_fusion_constants_are_frozen() {
    let version = diary_core::HYBRID_FUSION_VERSION;
    let min_score = format!("min{:.2}", diary_core::HYBRID_SEMANTIC_MIN_SCORE);
    for needle in ["k60", "depth50", "over200", min_score.as_str()] {
        assert!(
            version.contains(needle),
            "融合版本号 {version} 里没有 {needle}：改了常量要同步改版本号"
        );
    }
    assert_eq!(version, "hybrid-rrf-k60-depth50-over200-min0.40");
}

/// 回归（评审在 #58 上指出的）：**旧修订的块不能被语义/混合检索当成当前内容召回**。
///
/// 场景：建好语义索引 → `revise_text` 改正文 → **不重建** → 搜索。
/// 块与向量都按内容存，所以旧修订的块仍在生效代次里；候选查询若不按
/// `source_items.current_revision_id` 过滤，就会命中旧正文。关键词那一路在 #54 已按同一
/// 口径修过，这一条把语义那一路钉住。
#[test]
fn retired_revision_is_not_returned_by_semantic_search_before_rebuild() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.sqlite");
    let mut core = Core::open(&path).unwrap();
    let counters = Arc::new(Counters::default());
    // 桩：旧正文含「旧说法」，新正文含「新说法」——两组正交单位向量，命中与否一目了然。
    core.set_embedder(stub(
        &counters,
        vec![("旧说法", unit(0)), ("新说法", unit(1))],
    ));

    let capture_id = write_capture(&mut core, at(2026, 9, 20), "", "op");
    let source_id = import_and_extract(
        &mut core,
        &capture_id,
        "报告.txt",
        "体检报告：这里写着旧说法，请以此为准。\n",
        "op-mat",
    );
    // 提交后才能改正文（草稿态不允许 revise）。
    let revision = core.get_capture(&capture_id).unwrap().revision;
    core.commit(&capture_id, revision, "op-commit").unwrap();
    core.build_semantic_index(None).unwrap();

    // 前提：建索引之后，旧说法在语义与混合两路都能命中。
    assert_eq!(
        search_mode(&mut core, "旧说法", SearchMode::Semantic, 10)
            .results
            .len(),
        1,
        "前提：旧说法此刻应当能命中"
    );
    assert_eq!(
        hybrid_mode(&mut core, "旧说法", 10).results.len(),
        1,
        "前提：混合那一路此刻也能命中"
    );

    // 改正文：新修订进库（#54 会顺手产出派生内容），但**没有重建语义索引**。
    let revision = core.get_capture(&capture_id).unwrap().revision;
    core.revise_text(
        &source_id,
        "体检报告：这里写的是新说法，请以此为准。\n",
        revision,
        "op-revise",
    )
    .unwrap();

    // 这一段是**回归本体**：旧修订的块与向量还在库里，但不能再被召回。
    assert_eq!(
        search_mode(&mut core, "旧说法", SearchMode::Semantic, 10)
            .results
            .len(),
        0,
        "旧修订的块不能被语义检索召回（#32：旧版本不能当成当前内容）"
    );
    assert_eq!(
        hybrid_mode(&mut core, "旧说法", 10).results.len(),
        0,
        "混合那一路同样不能召回旧修订"
    );
    // 诚实的一面：新正文此时**还没有**向量（没重建），所以语义搜不到它是如实结果。
    assert_eq!(
        search_mode(&mut core, "新说法", SearchMode::Semantic, 10)
            .results
            .len(),
        0,
        "没重建之前，新正文本来就没有向量——如实为空，而不是拿旧向量顶上"
    );

    // 状态也不能撒谎：旧块不算「可用的块」（它的每一段都不是当前修订）。
    let status = core.index_status(None).unwrap();
    assert_eq!(
        status.total_chunks, 0,
        "改正文后、重建前，可用的块是 0（旧块仍在库里，但不是当前内容）"
    );
    assert_eq!(status.embedded_chunks, 0, "同理，已嵌入的可用块也是 0");
    assert!(!status.semantic_index_ready, "没有可用块时不该说就绪");

    // 重建之后：新正文能被语义召回，旧正文仍然不能。
    core.build_semantic_index(None).unwrap();
    assert_eq!(
        search_mode(&mut core, "新说法", SearchMode::Semantic, 10)
            .results
            .len(),
        1,
        "重建之后，新正文应当能被语义召回"
    );
    // 注意：**纯语义是 kNN**，它总会返回最近的几条（这里新块与查询正交、分数≈0），
    // 所以不能断言「条数为 0」——要断言的是**没有任何一条命中是旧正文**。
    let after = search_mode(&mut core, "旧说法", SearchMode::Semantic, 10);
    assert!(
        after.results.iter().all(|hit| !hit
            .snippet
            .as_deref()
            .unwrap_or_default()
            .contains("旧说法")),
        "重建之后任何命中都不该是旧正文：{:?}",
        after
            .results
            .iter()
            .map(|h| h.snippet.clone())
            .collect::<Vec<_>>()
    );
}

// ------------------------------------------------------------ int8 存储与流式扫描（#90）

/// 阻塞 1 的回归（评审 #128）：**从磁盘 reopen 一个存量 f32 库**。
///
/// 换默认存储之后，旧库只有一种 `model_version`，覆盖也完整——旧状态逻辑会报
/// `semantic_index_ready = true`、`reasons` 为空，而检索路径把整代判成
/// `ForeignSpace`（纯语义空、混合退化）。这条测试不注入嵌入器就查状态，同时证明
/// storage 检查**不依赖加载模型**。
#[test]
fn reopening_an_f32_library_reports_not_ready_with_a_storage_reason() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.sqlite");
    let mut core = new_core(dir.path());
    let counters = Arc::new(Counters::default());
    core.set_embedder(stub(&counters, vec![]));
    core.set_vector_storage_for_test(VectorStorage::F32);
    write_capture(&mut core, at(2026, 9, 20), "妈妈打电话来。", "opA");
    write_capture(&mut core, at(2026, 9, 21), "路边的桂花开了。", "opB");
    let built = core.build_semantic_index(None).unwrap();
    assert_eq!(built.embedded_chunks, 2, "前提：旧库的覆盖是完整的");
    drop(core);

    // 不 set_embedder：状态检查只用库里的列与当前默认口径。
    let reopened = Core::open(&path).unwrap();
    let status = reopened.index_status(None).unwrap();
    assert_eq!(
        status.embedded_chunks, status.total_chunks,
        "数据没坏：块都有 f32 向量"
    );
    assert_eq!(
        status.model_version.as_deref(),
        Some("stub-embedder@000000000000"),
        "只有一种模型版本——旧逻辑正是因此看不出问题"
    );
    assert!(
        !status.semantic_index_ready,
        "f32 生效代次在 int8 口径下不能报就绪：{:?}",
        status.reasons
    );
    let reason = status
        .reasons
        .iter()
        .find(|reason| reason.contains("存储格式从 f32 变为 int8"))
        .unwrap_or_else(|| panic!("reasons 要写明存储格式变化：{:?}", status.reasons));
    assert!(
        reason.contains("需要重建"),
        "原因里要给出可执行的下一步：{reason}"
    );
}

/// 核心自动排队（评审 #128 复审）：口径不一致时 `open` 自己排一条语义重建，
/// 且**重复 open 仍只有一条**（去重键 `(kind, target_ids=[], snapshot=NULL)`）。
///
/// 自动排队只在模型 `Configured` / `Ready` 时生效（没配模型不排），而测试不能设进程
/// 环境变量来造这个状态，所以用 `open_with_embedding_config_for_test` 显式钉住
/// 「配了、文件都在、还没加载」——与产品里设置好模型后的 open 同一条判据。
#[test]
fn opening_a_mismatched_library_queues_one_deduplicated_semantic_rebuild() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.sqlite");
    let mut core = new_core(dir.path());
    let counters = Arc::new(Counters::default());
    core.set_embedder(stub(&counters, vec![]));
    core.set_vector_storage_for_test(VectorStorage::F32);
    write_capture(&mut core, at(2026, 9, 20), "妈妈打电话来。", "opA");
    core.build_semantic_index(None).unwrap();
    assert_eq!(semantic_job_count(&path), 0, "建库时口径一致，不该自动排队");
    drop(core);

    let configured = configured_model(dir.path());
    let first = Core::open_with_embedding_config_for_test(&path, Some(configured.clone())).unwrap();
    assert_eq!(
        semantic_job_count(&path),
        1,
        "open 检测到 f32→int8 口径不一致，要自动排一条语义重建"
    );
    assert!(!first.index_status(None).unwrap().semantic_index_ready);
    let (kind, state) = semantic_job_kind_state(&path);
    assert_eq!(kind, "semantic_index");
    assert_eq!(state, "queued");
    drop(first);

    // 连开第二次：业务键命中，仍然只有 1 条（不能因为 open 两次排 N 条）。
    let second = Core::open_with_embedding_config_for_test(&path, Some(configured)).unwrap();
    assert_eq!(
        semantic_job_count(&path),
        1,
        "重复 open 不能产生第二条语义重建"
    );
    drop(second);
}

/// 口径的第三项 `dims`（评审 #128 复审）：换过模型但整代只有一种旧版本，
/// `model_version` 不混、覆盖也完整——旧状态仍会报 ready；现在必须报 false。
#[test]
fn reopening_a_library_with_another_model_dimension_reports_not_ready() {
    const OLD_VERSION: &str = "stub-embedder@old000000000";
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.sqlite");
    let mut core = new_core(dir.path());
    let counters = Arc::new(Counters::default());
    core.set_embedder(Box::new(StubEmbedder::with_dims(
        OLD_VERSION,
        384,
        &counters,
        vec![],
    )));
    // 存储与当前默认一致，只让 dims 不同：这条测的就是「dims 完全没查」。
    core.set_vector_storage_for_test(VectorStorage::Int8);
    write_capture(&mut core, at(2026, 9, 20), "妈妈打电话来。", "opA");
    write_capture(&mut core, at(2026, 9, 21), "路边的桂花开了。", "opB");
    core.build_semantic_index(None).unwrap();
    drop(core);

    let reopened =
        Core::open_with_embedding_config_for_test(&path, Some(configured_model(dir.path())))
            .unwrap();
    let status = reopened.index_status(None).unwrap();
    assert_eq!(status.embedded_chunks, status.total_chunks, "覆盖是完整的");
    assert_eq!(
        status.model_version.as_deref(),
        Some(OLD_VERSION),
        "整代只有一种旧模型版本——旧逻辑正是因此看不出问题"
    );
    assert!(
        !status.semantic_index_ready,
        "384 维旧代次在 512 维当前模型下不能报就绪：{:?}",
        status.reasons
    );
    let reason = status
        .reasons
        .iter()
        .find(|reason| reason.contains("维度"))
        .unwrap_or_else(|| panic!("reasons 要写明维度不一致：{:?}", status.reasons));
    assert!(reason.contains("384") && reason.contains("512"), "{reason}");
    assert!(reason.contains("需要重建"), "{reason}");
    // dims 也是口径不一致，自动排队同样要触发。
    assert_eq!(semantic_job_count(&path), 1, "dims 不一致也要自动排一条");
}

/// 模型版本变了、但 dims/storage 相同：嵌入器加载后状态也必须报 false。
#[test]
fn a_loaded_embedder_with_another_model_version_reports_not_ready() {
    const OLD_VERSION: &str = "stub-embedder@old111111111";
    const NEW_VERSION: &str = "stub-embedder@new222222222";
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.sqlite");
    let mut core = new_core(dir.path());
    let counters = Arc::new(Counters::default());
    core.set_embedder(Box::new(StubEmbedder::with_dims(
        OLD_VERSION,
        512,
        &counters,
        vec![],
    )));
    core.set_vector_storage_for_test(VectorStorage::Int8);
    write_capture(&mut core, at(2026, 9, 20), "妈妈打电话来。", "opA");
    core.build_semantic_index(None).unwrap();
    drop(core);

    let mut reopened = Core::open(&path).unwrap();
    // open 时还不知道新模型版本（没加载）；加载之后才比得出来。
    reopened.set_embedder(Box::new(StubEmbedder::with_dims(
        NEW_VERSION,
        512,
        &counters,
        vec![],
    )));
    let status = reopened.index_status(None).unwrap();
    assert!(!status.semantic_index_ready, "模型版本不同不能报就绪");
    let reason = status
        .reasons
        .iter()
        .find(|reason| reason.contains("模型"))
        .unwrap_or_else(|| panic!("reasons 要写明模型版本不一致：{:?}", status.reasons));
    assert!(
        reason.contains(OLD_VERSION) && reason.contains(NEW_VERSION),
        "{reason}"
    );
    assert!(reason.contains("需要重建"), "{reason}");
}

/// 「文件都在、但还没加载」的模型配置：`Configured` 只 stat 文件，不建 ORT 会话。
///
/// 自动排队只在模型 `Configured` / `Ready` 时生效；测试用显式配置钉住这个状态，
/// 不去改进程环境变量（并行测试会互相踩）。
fn configured_model(dir: &Path) -> EmbeddingConfig {
    let model_dir = dir.join("fake-model");
    std::fs::create_dir_all(&model_dir).unwrap();
    std::fs::write(model_dir.join("model.onnx"), b"not-a-real-model").unwrap();
    std::fs::write(model_dir.join("tokenizer.json"), b"{}").unwrap();
    let ort_dylib = dir.join("libonnxruntime.so");
    std::fs::write(&ort_dylib, b"not-a-real-ort").unwrap();
    EmbeddingConfig {
        model_dir,
        ort_dylib,
        model_file: Some("model.onnx".to_owned()),
    }
}

/// 评审 #128 第 1 条：没配模型的库**不自动排队**——否则会长期挂一条
/// `waiting_configuration` + `requires_user_action` 的噪声任务，取消后还会复活。
///
/// 显式请求仍然照旧落 `waiting_configuration`（那是用户/导入链自己排的，不是噪声）；
/// 这里要钉的是：**自动触发**不排、取消之后重开也不复活。
#[test]
fn an_unconfigured_library_never_auto_queues_and_a_cancelled_job_does_not_come_back() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.sqlite");
    let mut core = new_core(dir.path());
    let counters = Arc::new(Counters::default());
    core.set_embedder(stub(&counters, vec![]));
    core.set_vector_storage_for_test(VectorStorage::F32);
    write_capture(&mut core, at(2026, 9, 20), "妈妈打电话来。", "opA");
    core.build_semantic_index(None).unwrap();
    drop(core);

    // 没配模型（显式 None，不受开发机环境变量影响）：口径不一致也不自动排。
    let mut core = Core::open_with_embedding_config_for_test(&path, None).unwrap();
    assert_eq!(
        semantic_job_count(&path),
        0,
        "没配模型的库不自动排队：那是长期噪声，不是在等资源"
    );
    assert_eq!(
        core.job_status().unwrap().waiting_configuration,
        0,
        "自动触发不该造出等配置的任务"
    );
    // 状态仍如实说「需要重建」：没有自动入口 ≠ 状态闭嘴。
    let status = core.index_status(None).unwrap();
    assert!(!status.semantic_index_ready);
    assert!(
        status
            .reasons
            .iter()
            .any(|reason| reason.contains("存储格式从 f32 变为 int8")),
        "要保持可执行的说明：{:?}",
        status.reasons
    );

    // 显式排一条：它会等配置（既有语义，不是这条要改的东西）。
    let explicit = core
        .schedule_semantic_index("显式排上，等模型", "op-explicit")
        .unwrap();
    core.run_due_jobs(Utc::now(), 60_000, "foreground", 64)
        .unwrap();
    let waiting = core.get_job(&explicit.id).unwrap();
    assert_eq!(waiting.state, JobState::WaitingConfiguration);
    assert!(waiting.requires_user_action, "显式任务等配置要提示用户");
    // 取消它，再重开：不能复活。
    core.cancel_job(&explicit.id, "op-cancel").unwrap();
    drop(core);
    let reopened = Core::open_with_embedding_config_for_test(&path, None).unwrap();
    assert_eq!(
        semantic_job_count(&path),
        1,
        "取消之后重开不能再冒出一条（取消的那条还在，但不是新排的）"
    );
    let state: String = open(&path)
        .query_row(
            "SELECT state FROM jobs WHERE kind = 'semantic_index'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(state, "cancelled");
    assert_eq!(reopened.job_status().unwrap().waiting_configuration, 0);
}

/// 评审 #128 第 3 条（**死角落复现**）：失败/取消不会清 `building_generation`，
/// 旧判据一看到它就早退，自动重建再也不会排。
///
/// 现在「正在建」的判据是**没有活着的 `semantic_index` 任务**：任务终态后，即使
/// `building_generation` 还留着，也再排一次（执行者会续跑/整代废弃重算）。同一口径
/// 最多自动尝试两次（首次 + 失败/取消后的一次重排），第三次起不再排。
#[test]
fn a_failed_rebuild_with_leftover_building_generation_gets_one_more_auto_attempt() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.sqlite");
    let mut core = new_core(dir.path());
    let counters = Arc::new(Counters::default());
    core.set_embedder(stub(&counters, vec![]));
    core.set_vector_storage_for_test(VectorStorage::F32);
    write_capture(&mut core, at(2026, 9, 20), "妈妈打电话来。", "opA");
    core.build_semantic_index(None).unwrap();
    drop(core);

    let configured = configured_model(dir.path());
    // 第一次 open：自动排一条（自动尝试 1/2）。
    let mut first =
        Core::open_with_embedding_config_for_test(&path, Some(configured.clone())).unwrap();
    assert_eq!(semantic_job_count(&path), 1);
    let job = first.claim_next_due_job(Utc::now()).unwrap().unwrap();
    drop(first);
    {
        // 模拟「跑到一半失败/被取消」：任务终态，building_generation 留着（续跑进度）。
        let conn = open(&path);
        conn.execute(
            "UPDATE jobs SET state = 'failed', error_code = 'io' WHERE id = ?1",
            params![job.id],
        )
        .unwrap();
        conn.execute_batch("UPDATE index_meta SET building_generation = active_generation + 1;")
            .unwrap();
    }

    // 第二次 open：没有**活着的**任务，building 不该再挡路 → 再排一次（2/2）。
    let second =
        Core::open_with_embedding_config_for_test(&path, Some(configured.clone())).unwrap();
    assert_eq!(
        semantic_job_count(&path),
        2,
        "失败后要能再排一条；否则 building_generation 永远挡着，这就是新的死角落"
    );
    let (kind, state) = open(&path)
        .query_row(
            "SELECT kind, state FROM jobs WHERE kind = 'semantic_index' \
             AND state NOT IN ('failed', 'cancelled') LIMIT 1",
            [],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .unwrap();
    assert_eq!(kind, "semantic_index");
    assert_eq!(state, "queued");
    drop(second);

    // 把第二条也置成终态：同一口径已经自动尝试两次 → 第三次起交给显式入口。
    open(&path)
        .execute_batch(
            "UPDATE jobs SET state = 'failed', error_code = 'io' \
             WHERE kind = 'semantic_index' AND state NOT IN ('succeeded','failed','cancelled');",
        )
        .unwrap();
    let _third = Core::open_with_embedding_config_for_test(&path, Some(configured)).unwrap();
    assert_eq!(
        semantic_job_count(&path),
        2,
        "同一口径最多自动尝试两次：失败行不能随每次 open 增长"
    );
}

/// 评审 #128 第五轮：自动尝试上限必须**跨发现点**生效。
///
/// 旧指纹把「当前模型版本」编了进去，而两个发现点拿到的东西必然不同：
///
/// - `open` 时模型只到 `Configured`，`loaded_model_version()` 永远是 `None` ⇒ 指纹里是
///   `"unknown"`（记作 U）；
/// - 检索（这里用 `open_with_embedder_for_test` 把「模型已加载」那一刻提前）一定在加载
///   之后，拿到真实版本 ⇒ 指纹是 K。
///
/// 同一个 `storage` 不一致因此算出两个指纹：`open` 排 U → 失败 → `Ready` 排 K → 失败 →
/// 再 `open` 又按 U 从 0 算 → 每次启动稳定新增 1–2 条，计数永远到不了 2。这条回归走一遍
/// 「Configured → failed → Ready → failed → Configured」，修好之后第三次必须被上限挡住。
#[test]
fn the_auto_attempt_limit_is_shared_across_open_and_ready_discovery_points() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.sqlite");
    let mut core = new_core(dir.path());
    let counters = Arc::new(Counters::default());
    core.set_embedder(stub(&counters, vec![]));
    core.set_vector_storage_for_test(VectorStorage::F32);
    write_capture(&mut core, at(2026, 9, 20), "妈妈打电话来。", "opA");
    core.build_semantic_index(None).unwrap();
    drop(core);

    let configured = configured_model(dir.path());
    // 发现点 1：Configured open。模型没加载，版本未知（U）。
    let first = Core::open_with_embedding_config_for_test(&path, Some(configured.clone())).unwrap();
    assert_eq!(
        semantic_job_count(&path),
        1,
        "Configured open 首次自动排一条"
    );
    drop(first);
    fail_all_semantic_jobs(&path);

    // 发现点 2：Ready open。模型已加载，版本已知（K）——旧指纹在这里换成另一个值。
    let ready = Core::open_with_embedder_for_test(&path, stub(&counters, vec![])).unwrap();
    assert_eq!(
        semantic_job_count(&path),
        2,
        "Ready open 再排一条（自动尝试 2/2）"
    );
    drop(ready);
    fail_all_semantic_jobs(&path);

    // 回到发现点 1：同一个 storage 不一致，第三次必须被上限挡住，而不是又当新口径。
    let third = Core::open_with_embedding_config_for_test(&path, Some(configured)).unwrap();
    assert_eq!(
        semantic_job_count(&path),
        2,
        "同一口径最多自动尝试两次：open 与 Ready 两个发现点必须共用同一个指纹"
    );
    drop(third);
}

/// 组合不一致（`storage` + 模型版本）：`open` 只看得见 `storage`，检索两个都看得见。
/// 若把版本再单列成指纹位，两个发现点又算出两个指纹，同样的交替会回来（旧实现就是
/// 这样）。所以 `storage`/`dims` 已不一致时版本位被吸收：它不改变「必须按当前口径
/// 整代重算」这个事实，而且 `open` 永远观察不到它。
#[test]
fn the_auto_attempt_limit_survives_a_combined_storage_and_model_mismatch() {
    const OLD_VERSION: &str = "stub-embedder@old555555555";
    const NEW_VERSION: &str = "stub-embedder@new666666666";
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.sqlite");
    let mut core = new_core(dir.path());
    let counters = Arc::new(Counters::default());
    core.set_embedder(Box::new(StubEmbedder::with_dims(
        OLD_VERSION,
        512,
        &counters,
        vec![],
    )));
    core.set_vector_storage_for_test(VectorStorage::F32);
    write_capture(&mut core, at(2026, 9, 20), "妈妈打电话来。", "opA");
    core.build_semantic_index(None).unwrap();
    drop(core);

    let ready_embedder = || -> Box<dyn Embedder> {
        Box::new(StubEmbedder::with_dims(NEW_VERSION, 512, &counters, vec![]))
    };
    // Ready open：storage 与版本都不一致 → 第一条（版本位在这里一度也是 true）。
    let ready = Core::open_with_embedder_for_test(&path, ready_embedder()).unwrap();
    assert_eq!(semantic_job_count(&path), 1, "组合不一致先排一条");
    drop(ready);
    fail_all_semantic_jobs(&path);

    // Configured open：只看得到 storage → 第二条（2/2）。
    let configured_core =
        Core::open_with_embedding_config_for_test(&path, Some(configured_model(dir.path())))
            .unwrap();
    assert_eq!(semantic_job_count(&path), 2, "同一口径的第二次自动尝试");
    drop(configured_core);
    fail_all_semantic_jobs(&path);

    // 再回到 Ready：组合不一致必须与 open 算出同一个指纹，第三次被上限挡住。
    let ready_again = Core::open_with_embedder_for_test(&path, ready_embedder()).unwrap();
    assert_eq!(
        semantic_job_count(&path),
        2,
        "组合不一致也不能让 open 与检索各算一套指纹（旧实现这里会到 3）"
    );
    drop(ready_again);
}

/// 评审 #128 第 5 条：成功重建之后 reopen **不再排**——口径已经一致，自动触发无事可做。
#[test]
fn a_successful_rebuild_stops_the_auto_queue_on_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.sqlite");
    let mut core = new_core(dir.path());
    let counters = Arc::new(Counters::default());
    core.set_embedder(stub(&counters, vec![]));
    core.set_vector_storage_for_test(VectorStorage::F32);
    write_capture(&mut core, at(2026, 9, 20), "妈妈打电话来。", "opA");
    core.build_semantic_index(None).unwrap();
    drop(core);

    let mut reopened =
        Core::open_with_embedding_config_for_test(&path, Some(configured_model(dir.path())))
            .unwrap();
    assert_eq!(semantic_job_count(&path), 1, "f32 存量库先自动排一条");
    // 让它真的跑完：注入同口径的嵌入器并驱动一轮。
    reopened.set_embedder(stub(&counters, vec![]));
    let round = reopened
        .run_due_jobs(Utc::now(), 60_000, "foreground", 64)
        .unwrap();
    assert_eq!(round.succeeded, 1);
    assert!(
        reopened.index_status(None).unwrap().semantic_index_ready,
        "按当前口径整代重算之后就绪"
    );
    drop(reopened);

    let again = Core::open_with_embedder_for_test(&path, stub(&counters, vec![])).unwrap();
    assert_eq!(
        semantic_job_count(&path),
        1,
        "成功重建后口径一致，reopen 不该再排一条"
    );
    assert!(again.index_status(None).unwrap().semantic_index_ready);
}

/// 评审 #128 第 4 条：`model_version` 也要纳入自动触发——换了一份同维度、同存储的
/// 模型文件，`index_status` 会说「需要重建」，自动触发也必须给出可执行的入口。
///
/// 只在嵌入器**已经加载**（内存里有 `model_version`）时比：这里用
/// `open_with_embedder_for_test` 把「open 时模型已加载」那一刻造出来，不比文件哈希。
#[test]
fn a_loaded_embedder_with_another_model_version_queues_one_auto_rebuild() {
    const OLD_VERSION: &str = "stub-embedder@old333333333";
    const NEW_VERSION: &str = "stub-embedder@new444444444";
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.sqlite");
    let mut core = new_core(dir.path());
    let counters = Arc::new(Counters::default());
    core.set_embedder(Box::new(StubEmbedder::with_dims(
        OLD_VERSION,
        512,
        &counters,
        vec![],
    )));
    core.set_vector_storage_for_test(VectorStorage::Int8);
    write_capture(&mut core, at(2026, 9, 20), "妈妈打电话来。", "opA");
    core.build_semantic_index(None).unwrap();
    drop(core);

    let reopened = Core::open_with_embedder_for_test(
        &path,
        Box::new(StubEmbedder::with_dims(NEW_VERSION, 512, &counters, vec![])),
    )
    .unwrap();
    assert_eq!(
        semantic_job_count(&path),
        1,
        "模型版本不同（维度/存储相同）也要自动排一条"
    );
    let status = reopened.index_status(None).unwrap();
    assert!(!status.semantic_index_ready);
    assert!(
        status
            .reasons
            .iter()
            .any(|reason| reason.contains(OLD_VERSION) && reason.contains(NEW_VERSION)),
        "状态要同时点名两边的版本：{:?}",
        status.reasons
    );
}

/// 评审 #128 复审：v15 是**真的**加性迁移——空库直接建到 15；把库退回 v14 再打开，
/// 向量一行不丢、新列默认 NULL、覆盖索引建出来、重复 open 幂等。
///
/// 为什么这条必须存在：`SCHEMA_VERSION` 是持久化的契约（Dart 端到端测试也断言它），
/// 升版本就要证明「数据不丢、新结构可用、重复迁移不炸」，而不是为了改断言。
#[test]
fn a_v14_library_migrates_to_v15_preserving_vectors_and_adding_the_marker() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.sqlite");
    let mut core = new_core(dir.path());
    assert_eq!(
        core.schema_version().unwrap(),
        diary_core::SCHEMA_VERSION,
        "空库直接建到最新"
    );
    assert_eq!(diary_core::SCHEMA_VERSION, 18, "本轮升到 18");
    let counters = Arc::new(Counters::default());
    core.set_embedder(stub(&counters, vec![]));
    write_capture(&mut core, at(2026, 9, 20), "妈妈打电话来。", "opA");
    write_capture(&mut core, at(2026, 9, 21), "路边的桂花开了。", "opB");
    core.build_semantic_index(None).unwrap();
    let vectors_before: i64 = open(&path)
        .query_row("SELECT COUNT(*) FROM chunk_vectors", [], |row| row.get(0))
        .unwrap();
    assert_eq!(vectors_before, 2, "前提：两篇各一块、各一行向量");
    drop(core);

    // 退回**真正的 v14 样子**：没有 v15 的列与索引，版本号也退回去。
    {
        let conn = open(&path);
        conn.execute_batch(
            "DROP INDEX idx_chunk_vectors_caliber;
             ALTER TABLE index_meta DROP COLUMN auto_rebuild_marker;
             UPDATE schema_migrations SET version = 14;
             PRAGMA user_version = 14;",
        )
        .unwrap();
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, 14);
        assert!(
            conn.query_row("SELECT auto_rebuild_marker FROM index_meta", [], |row| {
                row.get::<_, String>(0)
            })
            .is_err(),
            "先确认 v14 库里真的没有这一列"
        );
    }

    let migrated = Core::open(&path).unwrap();
    assert_eq!(
        migrated.schema_version().unwrap(),
        diary_core::SCHEMA_VERSION,
        "v14 → v15"
    );
    // 旧数据一行不丢：向量数与生效代次的口径都还在。
    let status = migrated.index_status(None).unwrap();
    assert_eq!(status.total_chunks, 2);
    assert_eq!(status.embedded_chunks, 2);
    assert_eq!(
        status.model_version.as_deref(),
        Some("stub-embedder@000000000000"),
        "向量行原样保留"
    );
    assert_eq!(
        open(&path)
            .query_row("SELECT COUNT(*) FROM chunk_vectors", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        vectors_before
    );
    {
        let conn = open(&path);
        // 新列默认 NULL（不回填），而且可写。
        let marker: Option<String> = conn
            .query_row(
                "SELECT auto_rebuild_marker FROM index_meta WHERE id = 1",
                [],
                |row| row.get::<_, Option<String>>(0),
            )
            .unwrap();
        assert!(marker.is_none(), "迁移不回填自动尝试标记");
        conn.execute(
            "UPDATE index_meta SET auto_rebuild_marker = '1:probe' WHERE id = 1",
            [],
        )
        .unwrap();
        // 覆盖索引建出来，而且口径核对真的用它。
        let plan: Vec<String> = conn
            .prepare(
                "EXPLAIN QUERY PLAN SELECT DISTINCT model_version, dims, storage \
                 FROM chunk_vectors WHERE generation = 1",
            )
            .unwrap()
            .query_map([], |row| row.get::<_, String>(3))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert!(
            plan.join(" | ")
                .contains("COVERING INDEX idx_chunk_vectors_caliber"),
            "v15 的覆盖索引要真的被用上：{plan:?}"
        );
        let recorded: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM schema_migrations WHERE version = ?1",
                [diary_core::SCHEMA_VERSION],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(recorded, 1, "schema_migrations 记下最终版本");
    }
    drop(migrated);

    // 幂等：再 open 不重复建、不报错、也不把标记重置。
    let again = Core::open(&path).unwrap();
    assert_eq!(again.schema_version().unwrap(), diary_core::SCHEMA_VERSION);
    assert_eq!(
        open(&path)
            .query_row("SELECT COUNT(*) FROM chunk_vectors", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        vectors_before,
        "重复迁移不动向量"
    );
    let marker: Option<String> = open(&path)
        .query_row(
            "SELECT auto_rebuild_marker FROM index_meta WHERE id = 1",
            [],
            |row| row.get::<_, Option<String>>(0),
        )
        .unwrap();
    assert_eq!(marker.as_deref(), Some("1:probe"), "重复 open 不重置标记");
}

/// 评审 #133 第 1 条（真隐患）：v15 的「列」与「索引」必须各有一个守卫。
///
/// 造一个「`auto_rebuild_marker` 已在、`idx_chunk_vectors_caliber` 被手工删掉」的
/// v15 库——手工删索引、或将来单独补索引的库都长这样。旧实现是
/// `current < 15 && !column_exists(...)` 一个守卫包住整段：列已在 ⇒ 整段跳过，
/// 索引永远补不回来。修完后每次 `open` 都做一次 `sqlite_master` 轻量检查，
/// 索引缺就建（幂等；`current` 是 15 也一样）。
#[test]
fn a_v15_library_with_a_dropped_caliber_index_recreates_it_on_open() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.sqlite");
    let mut core = new_core(dir.path());
    write_capture(&mut core, at(2026, 9, 20), "妈妈打电话来。", "opA");
    drop(core);

    {
        let conn = open(&path);
        // 前提：库已经是最新版本且列在，只把索引删掉。
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, diary_core::SCHEMA_VERSION, "前提：库已经是最新版本");
        let column_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('index_meta') \
                 WHERE name = 'auto_rebuild_marker'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(column_count, 1, "前提：v15 的列已经在");
        conn.execute_batch("DROP INDEX idx_chunk_vectors_caliber;")
            .unwrap();
        let index_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' \
                 AND name = 'idx_chunk_vectors_caliber'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(index_count, 0, "前提：索引已被删掉");
    }

    // 旧实现：`migrate` 在 `current == SCHEMA_VERSION` 时直接返回，索引仍然缺。
    let opened = Core::open(&path).unwrap();
    assert_eq!(opened.schema_version().unwrap(), diary_core::SCHEMA_VERSION);

    let conn = open(&path);
    // 1) `PRAGMA index_list` 真的能查到这条索引。
    let indexes: Vec<String> = conn
        .prepare("PRAGMA index_list(chunk_vectors)")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(1))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert!(
        indexes.iter().any(|name| name == "idx_chunk_vectors_caliber"),
        "open 要把缺失的覆盖索引补回来：{indexes:?}"
    );
    // 2) 补回来的索引不是摆设：整代口径核对真的用它。
    let plan: Vec<String> = conn
        .prepare(
            "EXPLAIN QUERY PLAN SELECT DISTINCT model_version, dims, storage \
             FROM chunk_vectors WHERE generation = 1",
        )
        .unwrap()
        .query_map([], |row| row.get::<_, String>(3))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert!(
        plan.join(" | ")
            .contains("COVERING INDEX idx_chunk_vectors_caliber"),
        "补回来的覆盖索引要被查询计划用上：{plan:?}"
    );
}

/// 评审 #133 第 2 条：`index_bytes` 是**白名单求和**，v15 的覆盖索引必须列进去。
///
/// 不去断言 SQL 文本（改个空格就红），让数字自己说话：建好向量后记一次；把覆盖
/// 索引删掉再记一次——白名单没包含它时两次相等（这条测试会红），包含时差值就是
/// 索引占的页。
#[test]
fn index_bytes_counts_the_v15_caliber_index() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.sqlite");
    let mut core = new_core(dir.path());
    let counters = Arc::new(Counters::default());
    core.set_embedder(stub(&counters, vec![]));
    write_capture(&mut core, at(2026, 9, 20), "妈妈打电话来。", "opA");
    write_capture(&mut core, at(2026, 9, 21), "路边的桂花开了。", "opB");
    core.build_semantic_index(None).unwrap();

    let with_index = core.index_status(None).unwrap().index_bytes;
    assert!(with_index > 0, "前提：索引占用读得到");

    // 原始连接删索引。白名单没列 `idx_chunk_vectors_caliber` 时，这个数不会变小。
    open(&path)
        .execute_batch("DROP INDEX idx_chunk_vectors_caliber;")
        .unwrap();
    let without_index = core.index_status(None).unwrap().index_bytes;
    assert!(
        without_index < with_index,
        "index_bytes 必须包含覆盖索引的页：有索引 {with_index}，删掉后 {without_index}"
    );
}

/// 统计库里 `semantic_index` 任务数（自动排队回归用）。
fn semantic_job_count(path: &Path) -> i64 {
    open(path)
        .query_row(
            "SELECT COUNT(*) FROM jobs WHERE kind = 'semantic_index'",
            [],
            |row| row.get(0),
        )
        .unwrap()
}

/// 把所有还活着的 `semantic_index` 任务置成 `failed`（模拟重建反复失败）。
fn fail_all_semantic_jobs(path: &Path) {
    open(path)
        .execute_batch(
            "UPDATE jobs SET state = 'failed', error_code = 'io' \
             WHERE kind = 'semantic_index' AND state NOT IN ('succeeded','failed','cancelled');",
        )
        .unwrap();
}

fn semantic_job_kind_state(path: &Path) -> (String, String) {
    open(path)
        .query_row(
            "SELECT kind, state FROM jobs WHERE kind = 'semantic_index' LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap()
}

/// 换存储格式 = 换口径：必须**整代重算**，不能把 f32 向量原地当 int8 用。
///
/// 默认已是 int8；为了让「换」这个动作真的存在，这里先把旧代次**显式**建成 f32，
/// 再切到 int8。三件事都钉住：
/// 1. 只改 `storage`、还没重建时，语义这一路**不作数**（跨编码的余弦是垃圾）；
/// 2. 重建是一次整代重算（`reused_chunks == 0`，代次 +1）；
/// 3. 新代次里每一行都是 `int8`、字节数是 `4 + dims`，检索照常出结果。
#[test]
fn switching_the_storage_format_forces_a_full_recompute() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.sqlite");
    let mut core = new_core(dir.path());
    let counters = Arc::new(Counters::default());
    core.set_embedder(stub(&counters, vec![]));
    // 旧代次显式用 f32（不靠默认值：默认已改成 int8）。
    core.set_vector_storage_for_test(VectorStorage::F32);
    write_capture(&mut core, at(2026, 9, 20), "妈妈打电话来。", "opA");
    write_capture(&mut core, at(2026, 9, 21), "路边的桂花开了。", "opB");

    let first = core.build_semantic_index(None).unwrap();
    assert_eq!(first.generation, 1);
    let conn = open(&path);
    let storages: Vec<String> = conn
        .prepare("SELECT DISTINCT storage FROM chunk_vectors WHERE generation = 1")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(storages, vec!["f32".to_owned()], "前提：旧代次显式用 f32");
    drop(conn);

    // 换格式，但**先不重建**：口径已经不一致，语义这一路必须整路不作数。
    core.set_vector_storage_for_test(VectorStorage::Int8);
    let snapshot = search_mode(&mut core, "妈妈", SearchMode::Semantic, 10);
    assert!(
        snapshot.results.is_empty(),
        "跨编码的余弦不作数，不能返回「看起来正常」的结果：{:?}",
        snapshot.results
    );
    assert!(
        snapshot
            .warnings
            .iter()
            .any(|warning| warning.contains("口径")),
        "要说清是口径不一致（而不是「没有相近内容」）：{:?}",
        snapshot.warnings
    );

    // 重建：整代重算，不复制 f32 向量。
    let rebuilt = core.build_semantic_index(None).unwrap();
    assert_eq!(rebuilt.generation, 2, "换口径必须开新代次");
    assert_eq!(
        rebuilt.reused_chunks, 0,
        "f32 向量一条都不能复用进 int8 代次（复用就等于混编码）"
    );
    assert_eq!(
        rebuilt.embedded_chunks, rebuilt.total_chunks,
        "两块都按 int8 重算"
    );

    let conn = open(&path);
    let storages: Vec<String> = conn
        .prepare("SELECT DISTINCT storage FROM chunk_vectors WHERE generation = 2")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(
        storages,
        vec!["int8".to_owned()],
        "生效代次里只能有一种编码"
    );
    let bad_lengths: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM chunk_vectors WHERE generation = 2 \
             AND LENGTH(vector) <> 4 + dims",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(bad_lengths, 0, "int8 行必须是 4 字节 scale + dims 字节");
    drop(conn);

    // 重建之后检索照常：两块都是哈希向量，查询「妈妈」应当至少命中一块。
    let snapshot = search_mode(&mut core, "妈妈", SearchMode::Semantic, 10);
    assert_eq!(snapshot.results.len(), 2, "纯语义是 kNN，两块都会被列出来");
    assert!(
        snapshot
            .warnings
            .iter()
            .all(|warning| !warning.contains("口径")),
        "重建后没有口径问题：{:?}",
        snapshot.warnings
    );
}

/// 新库的默认向量存储是 **int8**（#90 的拍板决定）。
///
/// 不设任何测试开关，走产品默认路径建一代：状态里报的是 int8、每一行都是 `int8`
/// （4 字节 scale + dims 字节），检索照常。换默认之后若有哪条路又回退到 f32，这里会红。
#[test]
fn a_new_library_builds_int8_vectors_by_default() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.sqlite");
    let mut core = new_core(dir.path());
    assert_eq!(
        core.vector_storage(),
        VectorStorage::Int8,
        "新库默认必须是 int8（DEFAULT_VECTOR_STORAGE）"
    );
    let counters = Arc::new(Counters::default());
    core.set_embedder(stub(&counters, vec![]));
    write_capture(&mut core, at(2026, 9, 20), "妈妈打电话来。", "opA");

    let report = core.build_semantic_index(None).unwrap();
    assert_eq!(report.embedded_chunks, 1, "前提：真按默认路径写了一块");

    let conn = open(&path);
    let storages: Vec<String> = conn
        .prepare("SELECT DISTINCT storage FROM chunk_vectors")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(storages, vec!["int8".to_owned()], "默认建出来的只能是 int8");
    let bad_lengths: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM chunk_vectors WHERE LENGTH(vector) <> 4 + dims",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(bad_lengths, 0, "int8 行必须是 4 字节 scale + dims 字节");
    drop(conn);

    let snapshot = search_mode(&mut core, "妈妈", SearchMode::Semantic, 10);
    assert_eq!(snapshot.results.len(), 1, "int8 默认路径检索照常");
    assert!(
        snapshot
            .warnings
            .iter()
            .all(|warning| !warning.contains("口径")),
        "没有口径问题：{:?}",
        snapshot.warnings
    );
}

/// 流式扫描与「全量载入」参考路径必须**逐字节一致**（篇标识、顺序、分数位）。
///
/// 参考路径是这一片改掉的那条老路（先 `collect` 整代向量到堆里），只在测试里保留；
/// 两条路径共用同一段解码、校验、排序与折叠，差别只有「什么时候把向量丢掉」。
#[test]
fn streaming_scan_matches_the_load_all_reference() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    let counters = Arc::new(Counters::default());
    core.set_embedder(stub(
        &counters,
        vec![("妈妈", unit(0)), ("桂花", unit(1)), ("加班", unit(2))],
    ));
    write_capture(&mut core, at(2026, 9, 20), "妈妈打电话来。", "opA");
    write_capture(&mut core, at(2026, 9, 21), "路边的桂花开了。", "opB");
    write_capture(&mut core, at(2026, 9, 22), "今天又要加班。", "opC");
    write_capture(&mut core, at(2026, 9, 23), "普通的一天。", "opD");
    core.build_semantic_index(None).unwrap();

    for query in ["妈妈", "桂花", "加班", "普通", "今天"] {
        let streaming = core
            .semantic_ranking(query, &SearchFilters::default())
            .unwrap();
        let load_all = core
            .semantic_ranking_load_all_for_test(query, &SearchFilters::default())
            .unwrap();
        assert_eq!(
            streaming.len(),
            load_all.len(),
            "「{query}」：两条路径的候选数必须一样"
        );
        for (left, right) in streaming.iter().zip(&load_all) {
            assert_eq!(left.0, right.0, "「{query}」：篇标识与顺序必须逐字节一致");
            assert_eq!(
                left.1.to_bits(),
                right.1.to_bits(),
                "「{query}」：分数必须逐位一致（{} vs {}）",
                left.1,
                right.1
            );
        }
    }
}

/// 快速路径的**完整性红线**之一：窗口覆盖整代时，剩下的有效块就是**全部**答案，
/// 不必因为「有效块不足 4096」而回退。这里两篇同分、其一被回收，整代只有两块，
/// 断言流式路径留在快速路径、且与全量参考逐位一致。
///
/// 另两条红线见 `fast_scan_backfills_and_keeps_the_first_page_exact_when_a_top_candidate_is_filtered_out`
/// 与 `fast_scan_falls_back_when_the_backfill_window_is_not_enough`。
#[test]
fn fast_scan_returns_the_complete_answer_when_the_window_covers_the_generation() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    let counters = Arc::new(Counters::default());
    core.set_embedder(stub(&counters, vec![("妈妈", unit(0)), ("桂花", unit(0))]));
    let trashed = write_capture(&mut core, at(2026, 9, 20), "妈妈打电话来。", "opA");
    let kept = write_capture(&mut core, at(2026, 9, 21), "桂花开了。", "opB");
    core.build_semantic_index(None).unwrap();

    // 回收 A：它仍在生效代次的 top-K 里（两张同分），后置过滤会把它去掉。
    let conn = open(&dir.path().join("library.sqlite"));
    conn.execute(
        "UPDATE captures SET state = 'trashed' WHERE id = ?1",
        params![trashed],
    )
    .unwrap();
    drop(conn);

    for (filters, expected) in [
        (SearchFilters::default(), vec![kept.clone()]),
        (
            SearchFilters {
                include_trashed: true,
                ..Default::default()
            },
            vec![kept.clone(), trashed.clone()],
        ),
    ] {
        let streaming = core.semantic_ranking("妈妈", &filters).unwrap();
        let exhaustive = core
            .semantic_ranking_load_all_for_test("妈妈", &filters)
            .unwrap();
        assert_eq!(
            streaming.len(),
            exhaustive.len(),
            "快速路径与精确路径的候选数必须一样：{streaming:?} vs {exhaustive:?}"
        );
        for (left, right) in streaming.iter().zip(&exhaustive) {
            assert_eq!(left.0, right.0, "篇标识与顺序必须一致");
            assert_eq!(left.1.to_bits(), right.1.to_bits(), "分数必须逐位一致");
        }
        let ids: Vec<&str> = streaming.iter().map(|(id, _)| id.as_str()).collect();
        assert_eq!(ids, expected, "过滤口径必须与精确路径一致");
    }

    // 「整代都在窗口里」不是回退的理由：这条查询仍然走快速路径。
    diary_core::set_query_profiling(true);
    let _ = core
        .semantic_ranking("妈妈", &SearchFilters::default())
        .unwrap();
    let profile = diary_core::take_query_profile().unwrap();
    diary_core::set_query_profiling(false);
    assert!(
        profile.fast_path,
        "窗口覆盖整代时剩下的有效块就是全部答案，不该回退"
    );
}

/// **`semantic_ranking` 请求的是「全部结果」**：快速路径把深度之外的块截掉时，
/// 返回条数 < 请求条数，必须整条回退精确 SQL，结果与全量参考**逐条全等**。
///
/// 这是 #135 复审指出的缺口：旧测试只比「快速路径的前 4 条」、并把
/// `streaming=4 / exhaustive=5` 写进断言，等于把截断当成正确。这里两篇都参与比较，
/// 且断言 `fast_path == false`——要全部结果时就不许拿深度截断冒充完整答案。
#[test]
fn semantic_ranking_falls_back_when_depth_would_truncate_the_requested_results() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    let counters = Arc::new(Counters::default());
    core.set_embedder(stub(
        &counters,
        vec![
            ("妈妈", unit(0)),
            ("垃圾", unit(0)),
            ("候选0", at_cosine(0.90)),
            ("候选1", at_cosine(0.85)),
            ("候选2", at_cosine(0.80)),
            ("候选3", at_cosine(0.75)),
            ("候选4", at_cosine(0.70)),
        ],
    ));
    let trashed = write_capture(&mut core, at(2026, 9, 20), "垃圾 无关", "opTrash");
    let mut kept = Vec::new();
    for (index, day) in (21..=25).enumerate() {
        kept.push(write_capture(
            &mut core,
            at(2026, 9, day),
            &format!("候选{index} 的一段文字"),
            &format!("opKeep{index}"),
        ));
    }
    core.build_semantic_index(None).unwrap();

    let conn = open(&dir.path().join("library.sqlite"));
    conn.execute(
        "UPDATE captures SET state = 'trashed' WHERE id = ?1",
        params![trashed],
    )
    .unwrap();
    drop(conn);

    core.set_semantic_scan_depth_for_test(4);
    diary_core::set_query_profiling(true);
    let streaming = core
        .semantic_ranking("妈妈", &SearchFilters::default())
        .unwrap();
    let profile = diary_core::take_query_profile().unwrap();
    diary_core::set_query_profiling(false);
    assert!(
        !profile.fast_path,
        "要全部结果时，深度截断必须回退精确 SQL，不能只交前 4 条"
    );

    let exhaustive = core
        .semantic_ranking_load_all_for_test("妈妈", &SearchFilters::default())
        .unwrap();
    assert_eq!(
        streaming.len(),
        5,
        "5 个有效候选一条都不能少：{streaming:?}"
    );
    assert_eq!(exhaustive.len(), 5);
    for (left, right) in streaming.iter().zip(&exhaustive) {
        assert_eq!(left.0, right.0, "篇标识与顺序必须逐条一致");
        assert_eq!(left.1.to_bits(), right.1.to_bits(), "分数必须逐位一致");
    }
    let ids: Vec<&str> = streaming.iter().map(|(id, _)| id.as_str()).collect();
    assert_eq!(
        ids,
        kept.iter().map(String::as_str).collect::<Vec<_>>(),
        "被过滤的垃圾块必须排除，5 个有效候选按分数全量返回"
    );
}

/// #135 的**首屏一致性**：产品路径按 `page_size` 请求结果。top-K 里有一条被回收时，
/// 用补位窗口补上；只要折叠后的返回条数够这一屏，快速路径就留在原地，且**首屏与
/// 翻页结果都是精确路径的同一批**。
///
/// 把扫描深度压到 4（补位窗口 = 1），造 1 个回收 + 5 个有效候选：top-4 是
/// 「垃圾 + 候选0..2」，补位后拿回候选3。精确路径有 5 条，第 5 条在深度之外——
/// 这是 #90 已写明的结果深度上限，不是这次过滤造成的（把深度调到 5，快速路径
/// 就会把 5 条全部交出来）。
#[test]
fn fast_scan_backfills_and_keeps_the_first_page_exact_when_a_top_candidate_is_filtered_out() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    let counters = Arc::new(Counters::default());
    core.set_embedder(stub(
        &counters,
        vec![
            ("妈妈", unit(0)),
            ("垃圾", unit(0)),
            ("候选0", at_cosine(0.90)),
            ("候选1", at_cosine(0.85)),
            ("候选2", at_cosine(0.80)),
            ("候选3", at_cosine(0.75)),
            ("候选4", at_cosine(0.70)),
        ],
    ));
    let trashed = write_capture(&mut core, at(2026, 9, 20), "垃圾 无关", "opTrash");
    let mut kept = Vec::new();
    for (index, day) in (21..=25).enumerate() {
        kept.push(write_capture(
            &mut core,
            at(2026, 9, day),
            &format!("候选{index} 的一段文字"),
            &format!("opKeep{index}"),
        ));
    }
    core.build_semantic_index(None).unwrap();

    let conn = open(&dir.path().join("library.sqlite"));
    conn.execute(
        "UPDATE captures SET state = 'trashed' WHERE id = ?1",
        params![trashed],
    )
    .unwrap();
    drop(conn);

    let exhaustive = core
        .semantic_ranking_load_all_for_test("妈妈", &SearchFilters::default())
        .unwrap();
    assert_eq!(exhaustive.len(), 5, "精确路径有 5 个有效候选");

    // 深度 4、补位窗口 1：top-4 是「垃圾 + 候选0..2」，补位后拿回候选3。
    core.set_semantic_scan_depth_for_test(4);
    diary_core::set_query_profiling(true);
    let first_page = search_mode(&mut core, "妈妈", SearchMode::Semantic, 2);
    let profile = diary_core::take_query_profile().unwrap();
    diary_core::set_query_profiling(false);
    assert!(
        profile.fast_path,
        "首屏只要 2 条，补位窗口够用时必须留在快速路径"
    );
    let first_page_ids: Vec<String> = first_page
        .results
        .iter()
        .map(|hit| hit.group_id.clone())
        .collect();
    assert_eq!(
        first_page_ids,
        exhaustive[..2]
            .iter()
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>(),
        "首屏两条必须是精确路径的前两条"
    );
    let all_pages = collect_pages(&mut core, first_page);
    assert_eq!(all_pages.len(), 4, "深度上限是 4 块：{all_pages:?}");
    for (hit, (group, _)) in all_pages.iter().zip(&exhaustive[..4]) {
        assert_eq!(
            &hit.group_id, group,
            "补位后的篇标识与顺序必须与精确路径一致"
        );
    }
    let ids: Vec<&str> = all_pages.iter().map(|hit| hit.group_id.as_str()).collect();
    assert_eq!(
        ids,
        kept[..4].iter().map(String::as_str).collect::<Vec<_>>(),
        "被过滤的是垃圾块，补上来的必须是第 5 名候选"
    );

    // 深度 5：快速路径把 5 条全交出来，与精确路径逐条一致——证明第 5 条是深度截断，
    // 不是补位/过滤漏掉的。
    core.set_semantic_scan_depth_for_test(5);
    diary_core::set_query_profiling(true);
    let first_page = search_mode(&mut core, "妈妈", SearchMode::Semantic, 2);
    let profile = diary_core::take_query_profile().unwrap();
    diary_core::set_query_profiling(false);
    assert!(profile.fast_path, "深度够时仍应留在快速路径");
    let all_pages = collect_pages(&mut core, first_page);
    assert_eq!(all_pages.len(), exhaustive.len(), "深度 5 时一条都不该少");
    for (hit, (group, _)) in all_pages.iter().zip(&exhaustive) {
        assert_eq!(&hit.group_id, group, "深度 5 时与精确路径逐条一致");
    }
}

/// 与回收站同一个补位逻辑，但注入的是**过期块（非回收站）**：`capture` 仍是 active，
/// 只是 `chunk_spans.source_revision_id` 不是来源的当前修订。首屏只要 2 条时，
/// 补位窗口把第 5 名有效块补进来，快速路径留在原地，首屏与翻页都是精确路径的同一批。
#[test]
fn fast_scan_backfills_when_a_stale_chunk_is_filtered_out() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    let counters = Arc::new(Counters::default());
    core.set_embedder(stub(
        &counters,
        vec![
            ("妈妈", unit(0)),
            ("过期", at_cosine(0.95)),
            ("候选0", at_cosine(0.90)),
            ("候选1", at_cosine(0.85)),
            ("候选2", at_cosine(0.80)),
            ("候选3", at_cosine(0.75)),
            ("候选4", at_cosine(0.70)),
        ],
    ));
    let stale = write_capture(&mut core, at(2026, 9, 20), "过期 无关", "opStale");
    let mut kept = Vec::new();
    for (index, day) in (21..=25).enumerate() {
        kept.push(write_capture(
            &mut core,
            at(2026, 9, day),
            &format!("候选{index} 的一段文字"),
            &format!("opKeep{index}"),
        ));
    }
    core.build_semantic_index(None).unwrap();

    let conn = open(&dir.path().join("library.sqlite"));
    let stale_chunk: String = conn
        .query_row(
            "SELECT chunk_id FROM chunk_spans WHERE capture_id = ?1",
            params![stale],
            |row| row.get(0),
        )
        .unwrap();
    conn.execute(
        "INSERT INTO source_items (source_id, capture_id, kind, position, current_revision_id) \
         VALUES ('src_stale', ?1, 'text', 0, 'rev_new')",
        params![stale],
    )
    .unwrap();
    conn.execute(
        "UPDATE chunk_spans SET source_id = 'src_stale', source_revision_id = 'rev_old' \
         WHERE chunk_id = ?1",
        params![stale_chunk],
    )
    .unwrap();
    drop(conn);

    let exhaustive = core
        .semantic_ranking_load_all_for_test("妈妈", &SearchFilters::default())
        .unwrap();
    assert_eq!(exhaustive.len(), 5, "过期块不参与结果，精确路径有 5 条");

    core.set_semantic_scan_depth_for_test(4);
    diary_core::set_query_profiling(true);
    let first_page = search_mode(&mut core, "妈妈", SearchMode::Semantic, 2);
    let profile = diary_core::take_query_profile().unwrap();
    diary_core::set_query_profiling(false);
    assert!(
        profile.fast_path,
        "过期块由补位窗口补上，首屏只要 2 条时不该整条回退"
    );
    let all_pages = collect_pages(&mut core, first_page);
    let ids: Vec<&str> = all_pages.iter().map(|hit| hit.group_id.as_str()).collect();
    assert_eq!(
        ids,
        kept[..4].iter().map(String::as_str).collect::<Vec<_>>(),
        "补位后前 4 条与精确路径一致，过期块被排除"
    );
    assert!(!ids.contains(&stale.as_str()), "过期块不能出现在结果里");
}

/// #135 的**回退上界**：补位窗口不够时必须整条回退，而不是继续无限补。
///
/// 深度 4、窗口 1；top-5 里有 2 个被过滤（一个回收、一个旧修订），只剩 3 个有效块。
/// 这时无法证明窗口之外没有更该出现的候选 ⇒ 走精确 SQL；结果仍与全量参考一致。
#[test]
fn fast_scan_falls_back_when_the_backfill_window_is_not_enough() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    let counters = Arc::new(Counters::default());
    core.set_embedder(stub(
        &counters,
        vec![
            ("妈妈", unit(0)),
            ("垃圾", unit(0)),
            ("过期", at_cosine(0.95)),
            ("候选0", at_cosine(0.90)),
            ("候选1", at_cosine(0.85)),
            ("候选2", at_cosine(0.80)),
            ("候选3", at_cosine(0.75)),
        ],
    ));
    let trashed = write_capture(&mut core, at(2026, 9, 20), "垃圾 无关", "opTrash");
    let stale = write_capture(&mut core, at(2026, 9, 21), "过期 无关", "opStale");
    let kept = [
        write_capture(&mut core, at(2026, 9, 22), "候选0 的一段文字", "opKeep0"),
        write_capture(&mut core, at(2026, 9, 23), "候选1 的一段文字", "opKeep1"),
        write_capture(&mut core, at(2026, 9, 24), "候选2 的一段文字", "opKeep2"),
        write_capture(&mut core, at(2026, 9, 25), "候选3 的一段文字", "opKeep3"),
    ];
    core.build_semantic_index(None).unwrap();

    let conn = open(&dir.path().join("library.sqlite"));
    conn.execute(
        "UPDATE captures SET state = 'trashed' WHERE id = ?1",
        params![trashed],
    )
    .unwrap();
    let stale_chunk: String = conn
        .query_row(
            "SELECT chunk_id FROM chunk_spans WHERE capture_id = ?1",
            params![stale],
            |row| row.get(0),
        )
        .unwrap();
    conn.execute(
        "INSERT INTO source_items (source_id, capture_id, kind, position, current_revision_id) \
         VALUES ('src_stale', ?1, 'text', 0, 'rev_new')",
        params![stale],
    )
    .unwrap();
    conn.execute(
        "UPDATE chunk_spans SET source_id = 'src_stale', source_revision_id = 'rev_old' \
         WHERE chunk_id = ?1",
        params![stale_chunk],
    )
    .unwrap();
    drop(conn);

    // 深度 4、补位窗口 1：top-5 = 垃圾 + 过期 + 候选0..2，有效块只有 3 个 < 4。
    core.set_semantic_scan_depth_for_test(4);
    diary_core::set_query_profiling(true);
    let streaming = core
        .semantic_ranking("妈妈", &SearchFilters::default())
        .unwrap();
    let profile = diary_core::take_query_profile().unwrap();
    diary_core::set_query_profiling(false);
    assert!(
        !profile.fast_path,
        "补位窗口不够时必须整条回退精确 SQL，不能拿剩下的 3 条当完整答案"
    );

    let exhaustive = core
        .semantic_ranking_load_all_for_test("妈妈", &SearchFilters::default())
        .unwrap();
    assert_eq!(streaming.len(), kept.len(), "回退后与精确路径候选数一致");
    for (left, right) in streaming.iter().zip(&exhaustive) {
        assert_eq!(left.0, right.0, "篇标识与顺序必须一致");
        assert_eq!(left.1.to_bits(), right.1.to_bits(), "分数必须逐位一致");
    }
    let ids: Vec<&str> = streaming.iter().map(|(id, _)| id.as_str()).collect();
    assert_eq!(
        ids,
        kept.iter().map(String::as_str).collect::<Vec<_>>(),
        "回收与旧修订都必须被排除"
    );
}

/// 一篇会被切成两块的长正文：第一块含「妈妈」（最高分），第二块含「候选1」。
/// 用来钉住「深度按块计、结果按篇折叠」的差分。
fn long_piece_with_two_ranked_blocks() -> String {
    let mut text = String::new();
    for index in 0..24 {
        match index {
            0 => text.push_str("妈妈打电话来，问我最近睡得好不好。\n"),
            16 => text.push_str("候选1 这一段说的是周末去公园散步。\n"),
            _ => text.push_str(&format!(
                "第{index}段：今天天气不错，路上人很多，顺便买了点水果回家。\n"
            )),
        }
    }
    text
}

#[test]
fn fast_scan_folds_within_depth_and_keeps_the_first_page_exact() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    let counters = Arc::new(Counters::default());
    core.set_embedder(stub(
        &counters,
        vec![
            ("妈妈", unit(0)),
            ("候选1", at_cosine(0.85)),
            ("候选2", at_cosine(0.80)),
            ("候选3", at_cosine(0.75)),
            ("候选4", at_cosine(0.70)),
        ],
    ));
    // A 一篇两块，占掉深度里的两个名额；B/C/D 各一块。
    let a = write_capture(
        &mut core,
        at(2026, 9, 20),
        &long_piece_with_two_ranked_blocks(),
        "opA",
    );
    let b = write_capture(&mut core, at(2026, 9, 21), "候选2 的一段文字", "opB");
    let c = write_capture(&mut core, at(2026, 9, 22), "候选3 的一段文字", "opC");
    let d = write_capture(&mut core, at(2026, 9, 23), "候选4 的一段文字", "opD");
    let built = core.build_semantic_index(None).unwrap();
    assert_eq!(built.total_chunks, 5, "前提：A 两块、B/C/D 各一块");

    // 前提：A 的第一块含「妈妈」、第二块含「候选1」，两块分数不同。
    let conn = open(&dir.path().join("library.sqlite"));
    let a_blocks: Vec<String> = conn
        .prepare(
            "SELECT t.text FROM text_chunks t JOIN chunk_spans s ON s.chunk_id = t.chunk_id              WHERE s.capture_id = ?1 GROUP BY t.chunk_id ORDER BY MIN(s.start_char)",
        )
        .unwrap()
        .query_map(params![a], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    drop(conn);
    assert_eq!(a_blocks.len(), 2, "前提：长正文切成两块");
    assert!(a_blocks[0].contains("妈妈") && !a_blocks[0].contains("候选1"));
    assert!(a_blocks[1].contains("候选1") && !a_blocks[1].contains("妈妈"));

    let exhaustive = core
        .semantic_ranking_load_all_for_test("妈妈", &SearchFilters::default())
        .unwrap();
    let ids: Vec<&str> = exhaustive.iter().map(|(id, _)| id.as_str()).collect();
    assert_eq!(ids, vec![a.as_str(), b.as_str(), c.as_str(), d.as_str()]);
    assert_eq!(exhaustive[0].1.to_bits(), 1.0_f32.to_bits(), "A1 是 1.0");
    assert!(
        (exhaustive[1].1 - 0.80).abs() < 0.01,
        "A2 折叠后不单独成条；B 的分数应在 0.80 附近（int8 量化）：{exhaustive:?}"
    );

    // 深度 4 按**块**算：A1、A2、B、C 进深度，折叠后只剩 A/B/C 三条。
    core.set_semantic_scan_depth_for_test(4);
    diary_core::set_query_profiling(true);
    let first_page = search_mode(&mut core, "妈妈", SearchMode::Semantic, 2);
    let profile = diary_core::take_query_profile().unwrap();
    diary_core::set_query_profiling(false);
    assert!(profile.fast_path, "首屏 2 条够，快速路径不该回退");
    let first_page_ids: Vec<String> = first_page
        .results
        .iter()
        .map(|hit| hit.group_id.clone())
        .collect();
    assert_eq!(
        first_page_ids,
        vec![a.clone(), b.clone()],
        "首屏两条必须是精确路径的前两条"
    );
    let all_pages = collect_pages(&mut core, first_page);
    let page_ids: Vec<&str> = all_pages.iter().map(|hit| hit.group_id.as_str()).collect();
    assert_eq!(
        page_ids,
        vec![a.as_str(), b.as_str(), c.as_str()],
        "深度里的 4 块折叠成 3 条（A 的两块只算一篇）"
    );

    // 首屏要 5 条时，折叠后只有 3 条、深度之外还有 D ⇒ 回退精确 SQL 补满这一屏。
    diary_core::set_query_profiling(true);
    let first_page = search_mode(&mut core, "妈妈", SearchMode::Semantic, 5);
    let profile = diary_core::take_query_profile().unwrap();
    diary_core::set_query_profiling(false);
    assert!(
        !profile.fast_path,
        "折叠后不足一屏、窗口/深度之外还有块时必须回退"
    );
    let page_ids: Vec<&str> = first_page
        .results
        .iter()
        .map(|hit| hit.group_id.as_str())
        .collect();
    assert_eq!(
        page_ids,
        vec![a.as_str(), b.as_str(), c.as_str(), d.as_str()],
        "回退后精确路径把 D 补进首屏"
    );

    // 深度 5：整代都在深度里，A/B/C/D 四篇全在，快速路径不必回退。
    core.set_semantic_scan_depth_for_test(5);
    diary_core::set_query_profiling(true);
    let first_page = search_mode(&mut core, "妈妈", SearchMode::Semantic, 5);
    let profile = diary_core::take_query_profile().unwrap();
    diary_core::set_query_profiling(false);
    assert!(profile.fast_path, "整代都在深度里时应留在快速路径");
    let page_ids: Vec<&str> = first_page
        .results
        .iter()
        .map(|hit| hit.group_id.as_str())
        .collect();
    assert_eq!(
        page_ids,
        vec![a.as_str(), b.as_str(), c.as_str(), d.as_str()]
    );
}

/// **同分边界**：第 depth 名与第 depth+1 名同分时，深度截断按扫描序（分数降序、
/// rowid 升序）留谁；已保留结果仍按完整七道全序排序，所以**首屏不受影响**。
///
/// 这里让被截掉的那一块（rowid 更大）落在更旧的一天，精确路径的全序也把它排最后：
/// 快速路径保留的 4 条应当与精确路径前 4 条逐条一致。首屏要 5 条时，折叠后只有
/// 4 条、深度外还有同分块 ⇒ 回退精确 SQL，5 条全交出来。
#[test]
fn fast_scan_resolves_a_score_tie_at_the_depth_cut_by_scan_order() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    let counters = Arc::new(Counters::default());
    core.set_embedder(stub(
        &counters,
        vec![
            ("妈妈", unit(0)),
            ("候选0", at_cosine(0.85)),
            ("候选1", at_cosine(0.80)),
            ("候选2", at_cosine(0.75)),
        ],
    ));
    // 创建顺序就是 rowid 顺序：D 比 E 先建，E 的 rowid 更大、日期更旧。
    let a = write_capture(&mut core, at(2026, 9, 25), "妈妈 的一段文字", "opA");
    let b = write_capture(&mut core, at(2026, 9, 24), "候选0 的一段文字", "opB");
    let c = write_capture(&mut core, at(2026, 9, 23), "候选1 的一段文字", "opC");
    let d = write_capture(&mut core, at(2026, 9, 21), "候选2 的一段文字", "opD");
    let e = write_capture(&mut core, at(2026, 9, 20), "候选2 的一段文字", "opE");
    core.build_semantic_index(None).unwrap();

    let exhaustive = core
        .semantic_ranking_load_all_for_test("妈妈", &SearchFilters::default())
        .unwrap();
    let ids: Vec<&str> = exhaustive.iter().map(|(id, _)| id.as_str()).collect();
    assert_eq!(
        ids,
        vec![a.as_str(), b.as_str(), c.as_str(), d.as_str(), e.as_str()],
        "前提：D/E 同分，D 日期更新，排在 E 前"
    );
    assert_eq!(
        exhaustive[3].1.to_bits(),
        exhaustive[4].1.to_bits(),
        "前提：第 4/5 名必须逐位同分"
    );

    core.set_semantic_scan_depth_for_test(4);
    diary_core::set_query_profiling(true);
    let first_page = search_mode(&mut core, "妈妈", SearchMode::Semantic, 2);
    let profile = diary_core::take_query_profile().unwrap();
    diary_core::set_query_profiling(false);
    assert!(profile.fast_path, "首屏只要 2 条时不该为边界同分回退");
    let first_page_ids: Vec<String> = first_page
        .results
        .iter()
        .map(|hit| hit.group_id.clone())
        .collect();
    assert_eq!(
        first_page_ids,
        vec![a.clone(), b.clone()],
        "边界同分不影响首屏"
    );
    // 深度截断留的是扫描序更靠前的那一块（同分看 rowid）。它未必是精确路径前 4 条里
    // 的那一块：同分边界上「留谁」由扫描序决定，这是深度上限本身的边界，不是首屏。
    let conn = open(&dir.path().join("library.sqlite"));
    let rowid_of = |capture: &str| -> i64 {
        conn.query_row(
            "SELECT v.rowid FROM chunk_vectors v JOIN chunk_spans s ON s.chunk_id = v.chunk_id \
             WHERE s.capture_id = ?1",
            params![capture],
            |row| row.get(0),
        )
        .unwrap()
    };
    let (d_rowid, e_rowid) = (rowid_of(&d), rowid_of(&e));
    drop(conn);
    let dropped_tie = if d_rowid > e_rowid {
        d.clone()
    } else {
        e.clone()
    };
    let expected: Vec<&str> = exhaustive
        .iter()
        .map(|(id, _)| id.as_str())
        .filter(|id| *id != dropped_tie)
        .collect();
    let all_pages = collect_pages(&mut core, first_page);
    let kept_ids: Vec<&str> = all_pages.iter().map(|hit| hit.group_id.as_str()).collect();
    assert_eq!(
        kept_ids, expected,
        "扫描序留下的 4 块 = 精确路径去掉 rowid 更大的那块同分块"
    );

    // 首屏要 5 条：折叠后只有 4 条、深度外还有同分块 ⇒ 回退精确 SQL。
    diary_core::set_query_profiling(true);
    let first_page = search_mode(&mut core, "妈妈", SearchMode::Semantic, 5);
    let profile = diary_core::take_query_profile().unwrap();
    diary_core::set_query_profiling(false);
    assert!(
        !profile.fast_path,
        "同分块被深度截掉、首屏又不够时，必须回退而不是猜"
    );
    let page_ids: Vec<&str> = first_page
        .results
        .iter()
        .map(|hit| hit.group_id.as_str())
        .collect();
    assert_eq!(
        page_ids,
        vec![a.as_str(), b.as_str(), c.as_str(), d.as_str(), e.as_str()],
        "回退后同分的 E 也按完整全序排在最后"
    );
}

/// 直接写一代 f32 向量（每块一篇），分数由 `scores` 精确控制；用来造超过默认
/// 深度（4096 块）的库，不必真的跑 4097 次嵌入。
fn insert_synthetic_f32_vectors(path: &Path, scores: &[f32]) {
    let conn = open(path);
    conn.execute("UPDATE index_meta SET active_generation = 1", [])
        .unwrap();
    let tx = conn.unchecked_transaction().unwrap();
    {
        let mut capture = tx
            .prepare(
                "INSERT INTO captures (id, revision, state, occurred_at, created_at, updated_at, \
                 time_zone, utc_offset_minutes, day_key, draft_text) \
                 VALUES (?1, 0, 'active', '2026-09-20T00:00:00.000Z', \
                         '2026-09-20T00:00:00.000Z', '2026-09-20T00:00:00.000Z', \
                         'Asia/Shanghai', 480, '2026-09-20', '')",
            )
            .unwrap();
        let mut chunk = tx
            .prepare(
                "INSERT INTO text_chunks (chunk_id, ordinal, text, kind, coverage, day_key, \
                 chunker_version, created_at) \
                 VALUES (?1, 0, ?2, 'text', 'complete', '2026-09-20', \
                         'chunker-1-packday-line400-overlap48', '2026-09-20T00:00:00.000Z')",
            )
            .unwrap();
        let mut span = tx
            .prepare(
                "INSERT INTO chunk_spans (chunk_id, ordinal, capture_id, source_id, \
                 source_revision_id, start_char, end_char, kind, coverage) \
                 VALUES (?1, 0, ?2, ?2, '', 0, 2, 'text', 'complete')",
            )
            .unwrap();
        let mut vector = tx
            .prepare(
                "INSERT INTO chunk_vectors (chunk_id, generation, model_version, dims, storage, \
                 vector, created_at) \
                 VALUES (?1, 1, 'stub-embedder@000000000000', 512, 'f32', ?2, \
                         '2026-09-20T00:00:00.000Z')",
            )
            .unwrap();
        for (index, score) in scores.iter().enumerate() {
            let capture_id = format!("cap_synth_{index:05}");
            let chunk_id = format!("chunk_synth_{index:05}");
            capture.execute(params![capture_id]).unwrap();
            chunk
                .execute(params![chunk_id, format!("块{index}")])
                .unwrap();
            span.execute(params![chunk_id, capture_id]).unwrap();
            vector
                .execute(params![
                    chunk_id,
                    VectorStorage::F32.encode(&at_cosine(*score)).unwrap()
                ])
                .unwrap();
        }
    }
    tx.commit().unwrap();
}

/// **默认深度（4096 块）的高密度差分**：造 4097 块、分数互不相同，快速路径应返回
/// 精确路径的前 4096 条（逐条一致），第 4097 条是深度上限之外的截断；首屏仍然与
/// 精确路径相同。
#[test]
fn fast_scan_at_the_default_depth_returns_the_exact_prefix() {
    const DEPTH: usize = 4_096;
    const BLOCKS: usize = DEPTH + 1;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.sqlite");
    let mut core = new_core(dir.path());
    let counters = Arc::new(Counters::default());
    core.set_embedder(stub(&counters, vec![("妈妈", unit(0))]));
    // 显式用 f32：int8 量化会把 0.0001 级的相邻分数压成同分，测不到「互不相同」。
    core.set_vector_storage_for_test(VectorStorage::F32);
    let scores: Vec<f32> = (0..BLOCKS)
        .map(|index| 0.99 - index as f32 * 0.0001)
        .collect();
    insert_synthetic_f32_vectors(&path, &scores);

    let exact = core
        .semantic_ranking_load_all_for_test("妈妈", &SearchFilters::default())
        .unwrap();
    assert_eq!(exact.len(), BLOCKS, "精确路径 4097 条");

    diary_core::set_query_profiling(true);
    let first_page = search_mode(&mut core, "妈妈", SearchMode::Semantic, 100);
    let profile = diary_core::take_query_profile().unwrap();
    diary_core::set_query_profiling(false);
    assert!(
        profile.fast_path,
        "首屏只要 100 条，4097 块的高密度库也应走快速路径"
    );
    let first_ids: Vec<String> = first_page
        .results
        .iter()
        .map(|hit| hit.group_id.clone())
        .collect();
    assert_eq!(
        first_ids,
        exact[..100]
            .iter()
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>(),
        "首屏 100 条必须逐条一致"
    );
    let all = collect_pages(&mut core, first_page);
    assert_eq!(all.len(), DEPTH, "深度上限是 4096 块：{}", all.len());
    let ids: Vec<&str> = all.iter().map(|hit| hit.group_id.as_str()).collect();
    let expected: Vec<&str> = exact[..DEPTH].iter().map(|(id, _)| id.as_str()).collect();
    assert_eq!(ids, expected, "快速路径必须是精确路径的前 4096 条");
    assert!(
        !ids.contains(&exact[DEPTH].0.as_str()),
        "第 4097 条属于深度外截断，不能出现在结果里"
    );
}

/// int8 检索的结果与 f32 **同一批篇、同一顺序**（量化误差没有改变这一组用例的排序）。/// int8 检索的结果与 f32 **同一批篇、同一顺序**（量化误差没有改变这一组用例的排序）。
///
/// 这不是「所有语料上召回不变」的证明——那件事由探针的 36 条质量集与 2000 块语料
/// 量（`large-bench recall-quality` / `recall-large`）。这里只钉住结构性行为：
/// int8 写入/解码/检索这条链路能跑通，且没有把结果变成另一批篇。
#[test]
fn int8_search_finds_the_same_pieces_as_f32() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    let counters = Arc::new(Counters::default());
    core.set_embedder(stub(
        &counters,
        vec![("妈妈", unit(0)), ("桂花", unit(1)), ("加班", unit(2))],
    ));
    // 第一代显式用 f32：默认已改成 int8，不写这一行的话两次都是 int8，
    // 这条测试就会退化成「int8 与 int8 相同」，什么都没验到。
    core.set_vector_storage_for_test(VectorStorage::F32);
    write_capture(&mut core, at(2026, 9, 20), "妈妈打电话来。", "opA");
    write_capture(&mut core, at(2026, 9, 21), "路边的桂花开了。", "opB");
    write_capture(&mut core, at(2026, 9, 22), "今天又要加班。", "opC");

    core.build_semantic_index(None).unwrap();
    let f32_ranking = core
        .semantic_ranking("妈妈", &SearchFilters::default())
        .unwrap();

    core.set_vector_storage_for_test(VectorStorage::Int8);
    core.build_semantic_index(None).unwrap();
    let int8_ranking = core
        .semantic_ranking("妈妈", &SearchFilters::default())
        .unwrap();

    assert_eq!(
        f32_ranking
            .iter()
            .map(|(group, _)| group.clone())
            .collect::<Vec<_>>(),
        int8_ranking
            .iter()
            .map(|(group, _)| group.clone())
            .collect::<Vec<_>>(),
        "这一组用例上 int8 的排序应当与 f32 相同"
    );
}

/// 与 `long_piece_with_insomnia` 同一个形状，只把段标识换成 `tag`：用来造**两份等长、
/// 内容不同**的长文。两份都被同一条切块规则切成两块，四块都含「失眠」。
fn long_piece_with_tag(tag: &str) -> String {
    let mut text = String::new();
    for index in 0..24 {
        if index == 12 {
            text.push_str(&format!(
                "第12段 {tag}：这一段说的是连续几天失眠，凌晨两点还醒着。\n"
            ));
        } else {
            text.push_str(&format!(
                "第{index}段 {tag}：今天天气不错，路上人很多，顺便买了点水果回家。\n"
            ));
        }
    }
    text
}

/// issue #57 的第二半（旧修复只挡到「同一篇的两块」）：**不同篇**的两段也可能拥有
/// 完全相同的（分数、日期、篇内起点 / 终点、段序号）。这时若落到 `chunk_id`，
/// 又会回到「每个库不一样」的老问题——两条候选正文不同，摘录会真的换人。
///
/// 构造：同一天导入两份等长、不同内容的长文 A/B，四块都含查询词，桩把所有含查询词
/// 的块映射成同一个单位向量 ⇒ 四块分数都是 1.0。A、B 的第一段都是
/// `(2026-09-20, start=0, end=390, ordinal=0, score=1.0)`，前五道键完全相同；旧逻辑
/// 只能用 `chunk_id`（每库随机 UUID）决定先后。
///
/// 断言的是**整份结果顺序**（摘录 + 定位区间）在 16 份独立库之间逐条相同。
/// 旧逻辑下每份库都是一次独立抛硬币，16 轮全对的概率约 2^-15；这里**验证过**
/// 去掉 `content_key` 这道键会红。
#[test]
fn equal_score_across_pieces_is_ordered_by_content_across_libraries() {
    const ROUNDS: usize = 16;
    type Evidence = (Option<String>, Option<i64>, Option<i64>);
    let mut evidence: Vec<Vec<Evidence>> = Vec::new();

    for round in 0..ROUNDS {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("library.sqlite");
        let mut core = new_core(dir.path());
        let counters = Arc::new(Counters::default());
        core.set_embedder(stub(&counters, vec![("失眠", unit(0))]));

        let capture_a = write_capture(&mut core, at(2026, 9, 20), "", "op");
        import_and_extract(
            &mut core,
            &capture_a,
            "长文A.txt",
            &long_piece_with_tag("A"),
            &format!("matA{round}"),
        );
        let capture_b = write_capture(&mut core, at(2026, 9, 20), "", "op2");
        import_and_extract(
            &mut core,
            &capture_b,
            "长文B.txt",
            &long_piece_with_tag("B"),
            &format!("matB{round}"),
        );

        let built = core.build_semantic_index(None).unwrap();
        assert_eq!(
            built.total_chunks, 4,
            "前提（第 {round} 轮）：两份等长长文各切成两块"
        );

        // 前提：四块都含「失眠」，桩给它们同一个单位向量，分数才会完全相同。
        let conn = open(&path);
        let with_word = count(
            &conn,
            "SELECT COUNT(*) FROM text_chunks WHERE text LIKE '%失眠%'",
            "text_chunks",
        );
        assert_eq!(
            with_word, 4,
            "前提（第 {round} 轮）：四块都含「失眠」，分数才会完全相同"
        );
        // 前提：A、B 的第一段拥有完全相同的篇内区间；两篇各两块，区间分布是 2+2。
        let span_groups: Vec<(i64, i64, i64)> = {
            let mut statement = conn
                .prepare(
                    "SELECT start_char, end_char, COUNT(*) FROM chunk_spans \
                     GROUP BY start_char, end_char ORDER BY start_char",
                )
                .unwrap();
            statement
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap()
        };
        assert_eq!(
            span_groups,
            vec![(0, 390, 2), (343, 731, 2)],
            "前提（第 {round} 轮）：不同篇的两块区间完全重合，才谈得上「同分谁当证据」"
        );
        drop(conn);

        let snapshot = search_mode(&mut core, "失眠", SearchMode::Semantic, 20);
        assert_eq!(
            snapshot.results.len(),
            2,
            "前提（第 {round} 轮）：两份长文各折叠成一条命中"
        );
        evidence.push(
            snapshot
                .results
                .iter()
                .map(|hit| {
                    (
                        hit.snippet.clone(),
                        hit.locator.as_ref().and_then(|locator| locator.text_start),
                        hit.locator.as_ref().and_then(|locator| locator.text_end),
                    )
                })
                .collect(),
        );
    }

    for (round, entry) in evidence.iter().enumerate() {
        assert_eq!(
            entry, &evidence[0],
            "第 {round} 份库的结果顺序与第 0 份不同：{:?} vs {:?}\
             （不同篇的两段同分时，必须由内容键（content_key）决胜，不能落到每库随机的 chunk_id）",
            entry, evidence[0]
        );
    }
}

/// 同一个库、同一个查询、两个独立的 `Core` 连接：结果必须**逐字节**相同。
///
/// 这是「同一查询跑两次」的最直接形态；跨到**另一个进程**再跑一次的那一条在
/// `same_library_cross_process_is_byte_identical` 里。
#[test]
fn same_library_two_cores_agree_byte_for_byte() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.sqlite");

    {
        let mut build = Core::open(&path).unwrap();
        let counters = Arc::new(Counters::default());
        build.set_embedder(stub(&counters, vec![("失眠", unit(0))]));
        let capture = write_capture(&mut build, at(2026, 9, 20), "", "op");
        import_and_extract(
            &mut build,
            &capture,
            "长文A.txt",
            &long_piece_with_tag("A"),
            "matA",
        );
        let capture = write_capture(&mut build, at(2026, 9, 20), "", "op2");
        import_and_extract(
            &mut build,
            &capture,
            "长文B.txt",
            &long_piece_with_tag("B"),
            "matB",
        );
        build.build_semantic_index(None).unwrap();
    }

    let query = |core: &mut Core| -> (Vec<u8>, Vec<(String, u32)>) {
        let counters = Arc::new(Counters::default());
        core.set_embedder(stub(&counters, vec![("失眠", unit(0))]));
        let snapshot = search_mode(core, "失眠", SearchMode::Semantic, 20);
        let ranking = core
            .semantic_ranking("失眠", &SearchFilters::default())
            .unwrap()
            .into_iter()
            .map(|(group, score)| (group, score.to_bits()))
            .collect();
        (serde_json::to_vec(&snapshot.results).unwrap(), ranking)
    };

    let mut first = Core::open(&path).unwrap();
    let mut second = Core::open(&path).unwrap();
    let (first_bytes, first_ranking) = query(&mut first);
    let (second_bytes, second_ranking) = query(&mut second);
    assert_eq!(
        first_bytes, second_bytes,
        "两个独立 Core 对同一库、同一查询必须给出逐字节相同的结果"
    );
    assert_eq!(
        first_ranking, second_ranking,
        "分数的 f32 位模式也必须逐位相同（不是「差不多」）"
    );
}

/// 跨进程：父进程建好库，再用**同一测试二进制**起一个子进程查一次；两边的
/// （顺序 + 摘录 + 定位 + `semantic_ranking` 的 f32 位）必须逐字节相同。
///
/// 子进程与父进程的 `HashMap` 随机种子不同，所以如果有人误把排序/折叠建立在
/// 容器迭代顺序上，这条会红。子进程模式靠环境变量区分；正常跑测试时它什么都不做。
#[test]
fn same_library_cross_process_is_byte_identical() {
    const DB_ENV: &str = "DIARY57_CROSS_PROCESS_DB";

    let query_json = |core: &mut Core| -> String {
        let counters = Arc::new(Counters::default());
        core.set_embedder(stub(&counters, vec![("失眠", unit(0))]));
        let snapshot = search_mode(core, "失眠", SearchMode::Semantic, 20);
        let ranking: Vec<(String, u32)> = core
            .semantic_ranking("失眠", &SearchFilters::default())
            .unwrap()
            .into_iter()
            .map(|(group, score)| (group, score.to_bits()))
            .collect();
        serde_json::to_string(&(snapshot.results, ranking)).unwrap()
    };

    if let Some(path) = std::env::var_os(DB_ENV) {
        let mut core = Core::open(Path::new(&path)).unwrap();
        println!("ZZ57_CHILD={}", query_json(&mut core));
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.sqlite");
    {
        let mut build = Core::open(&path).unwrap();
        let counters = Arc::new(Counters::default());
        build.set_embedder(stub(&counters, vec![("失眠", unit(0))]));
        let capture = write_capture(&mut build, at(2026, 9, 20), "", "op");
        import_and_extract(
            &mut build,
            &capture,
            "长文A.txt",
            &long_piece_with_tag("A"),
            "matA",
        );
        let capture = write_capture(&mut build, at(2026, 9, 20), "", "op2");
        import_and_extract(
            &mut build,
            &capture,
            "长文B.txt",
            &long_piece_with_tag("B"),
            "matB",
        );
        build.build_semantic_index(None).unwrap();
    }

    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "same_library_cross_process_is_byte_identical",
            "--nocapture",
        ])
        .env(DB_ENV, &path)
        .output()
        .expect("起子进程失败");
    assert!(
        output.status.success(),
        "子进程失败：{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let child = stdout
        .lines()
        .find_map(|line| line.strip_prefix("ZZ57_CHILD="))
        .unwrap_or_else(|| {
            panic!("子进程没有输出 ZZ57_CHILD 行；stdout = {stdout:?}");
        });

    let mut parent = Core::open(&path).unwrap();
    let parent_json = query_json(&mut parent);
    assert_eq!(
        child, parent_json,
        "同一库、同一查询在另一个进程里必须给出逐字节相同的结果"
    );
}

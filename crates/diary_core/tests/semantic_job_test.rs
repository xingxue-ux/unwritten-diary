//! issue #70（后半）：**语义索引的任务化**。
//!
//! 覆盖这一片的每一条验收，全部走真实调用（`schedule_semantic_index` + `run_due_jobs`
//! + 真库、真块、真向量）：
//!
//! 1. **按需排队与幂等**：显式请求排出一条 `semantic_index` 任务；同一个 operationId
//!    重复请求、或换一个 operationId 再请求，都只有**一条**活着的任务；跑完之后再请求
//!    才会建新的一条；
//! 2. **依赖门**：导入排下的 `extract` 还没跑完时，语义任务落在 `waiting_dependency` +
//!    `waiting_extract`（不发注定失败的工作、不消耗重试额度），并且**保持不变式**
//!    「有等待态 ⇒ `next_wakeup` 非空」；提取一完成就在**同一次**驱动里跑完；
//! 3. **等配置**（没有模型时）：落在 `waiting_configuration` + `waiting_model`，不失败、
//!    不消耗额度（闸门在领取前挡下，没跑过就不计尝试），但**要用户去配模型**——
//!    `requires_user_action = 1` + `attention_key = job-config:semantic_index:waiting_model`；
//!    配置好了（注入嵌入器）之后**不需要任何唤醒提示**，下一次驱动的闸门
//!    就把它拉起来跑完；
//! 4. **可恢复**：模拟「跑到一半被杀」（任务停在 `running`、`building_generation` 里已经
//!    写好一块向量）→ 驱动回收它并**续跑**，只算没算完的块（用嵌入调用次数钉住「不重头」），
//!    半成品**不会被切成生效代次**；
//! 5. **续跑前核对口径**（#58 的规则，本片必须能红的那条）：中断留下的那一代里有**另一个
//!    模型**的向量时，整代废弃、按当前模型从零重算——不能把两个向量空间的向量混进一代；
//! 6. **分段与预算**：给一个很短的预算，长任务在块与块之间**让出**（不是一跑到底）：任务
//!    回到 `queued`、进度如实写进 `progress`、`attempt_count` **不涨**（让出不是失败）、
//!    摘要给出可用的 `next_wakeup`；下一轮接着跑并最终完成，代次切换仍然是原子的。
//!
//! 风格照 `index_job_test.rs`：真库、真导入、断言带 reason。

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, TimeZone, Utc};
use diary_core::{
    Core, CreateDraftInput, EmbedOptions, Embedder, JobProgress, JobState, SearchFilters,
};
use rusqlite::params;

// ------------------------------------------------------------ 测试用 stub 嵌入器

/// 本进程的嵌入调用次数：用来钉「续跑不重头」与「换口径整代重算」。
#[derive(Default)]
struct Counters {
    documents: AtomicUsize,
}

/// 确定性 stub：正文哈希成向量；可选地每块睡一会儿（构造「预算不够跑完整库」）。
///
/// `model_version` / `dims` 可指定：构造「换过模型」的场景（口径三项里两项不同）。
struct StubEmbedder {
    model_version: String,
    dims: usize,
    delay: Duration,
    counters: Arc<Counters>,
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
        if !self.delay.is_zero() {
            std::thread::sleep(self.delay);
        }
        Ok(diary_core::text_hash_vector(text, self.dims))
    }

    fn embed_query(&self, text: &str) -> diary_core::Result<Vec<f32>> {
        Ok(diary_core::text_hash_vector(text, self.dims))
    }
}

const MODEL_A: &str = "stub-embedder@aaaaaaaaaaaa";
const MODEL_B: &str = "stub-embedder@bbbbbbbbbbbb";

fn stub(
    model_version: &str,
    dims: usize,
    delay: Duration,
    counters: &Arc<Counters>,
) -> Box<dyn Embedder> {
    Box::new(StubEmbedder {
        model_version: model_version.to_owned(),
        dims,
        delay,
        counters: Arc::clone(counters),
    })
}

// ------------------------------------------------------------ 通用工具

fn new_core(dir: &Path) -> Core {
    Core::open(dir.join("library.sqlite")).unwrap()
}

fn at(year: i32, month: u32, day: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(year, month, day, 4, 0, 0).unwrap()
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
    let revision = core.get_capture(&capture_id).unwrap().revision;
    core.save_draft(&capture_id, text, revision, &format!("{operation}-save"))
        .unwrap();
    capture_id
}

/// 造 `count` 块：一天一条记录、各写一句话（分块规则按日子打包，短篇一天一块）。
fn seed_chunks(core: &mut Core, count: i64) -> i64 {
    for index in 0..count {
        write_capture(
            core,
            at(2026, 9, 20 + index as u32),
            &format!("第 {} 篇正文。", index + 1),
            &format!("op-seed-{index}"),
        );
    }
    // exp：块表是重建出来的，先落库才谈得上「哪一块还没向量」。
    core.rebuild_text_chunks(None).unwrap()
}

fn open(path: &Path) -> rusqlite::Connection {
    rusqlite::Connection::open(path).unwrap()
}

fn count(conn: &rusqlite::Connection, sql: &str) -> i64 {
    conn.query_row(sql, [], |row| row.get(0))
        .unwrap_or_else(|error| panic!("计数查询失败（{sql}）：{error}"))
}

/// 这个进程配了本地模型吗？配了就跳过「没有模型」的用例（CI 上没配，它们会跑）。
fn model_configured() -> bool {
    std::env::var_os("DIARY_MODEL_DIR").is_some()
        || std::env::var_os("DIARY_ORT_DYLIB").is_some()
        || std::env::var_os("ORT_DYLIB_PATH").is_some()
}

// ------------------------------------------------------------ 1. 排队与幂等

/// 显式请求排出一条整库的 `semantic_index` 任务；重复请求不会产生第二份工作。
#[test]
fn an_explicit_request_queues_one_semantic_job_and_repeats_collapse() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    let counters = Arc::new(Counters::default());
    core.set_embedder(stub(MODEL_A, 512, Duration::ZERO, &counters));
    assert_eq!(seed_chunks(&mut core, 2), 2, "两块：两天各一篇");

    let job = core
        .schedule_semantic_index("用户点了重建", "op-semantic")
        .unwrap();
    assert_eq!(job.kind, "semantic_index", "新 kind：语义索引作业");
    assert!(job.target_ids.is_empty(), "语义重建总是整库（代次是全局的）");
    assert_eq!(job.state, JobState::Queued);
    assert!(job.next_attempt_at.is_none(), "入队即到期");
    assert!(job.progress.is_none(), "还没跑过就没有进度可报");
    assert_eq!(job.priority, diary_core::JobPriority::Maintenance.value());

    // 同一个 operationId + 同一个原因：返回同一条（读的是它此刻的真实状态）。
    let same = core
        .schedule_semantic_index("用户点了重建", "op-semantic")
        .unwrap();
    assert_eq!(same.id, job.id, "同一次操作重放要返回同一条任务");

    // 换一个 operationId：业务键 `(semantic_index, [], NULL)` 命中活着的那一条。
    let other = core
        .schedule_semantic_index("另一个入口也请求", "op-other")
        .unwrap();
    assert_eq!(other.id, job.id, "排队不产生第二份工作");

    // 同一个 operationId 换了原因：契约 1.2 的幂等冲突，不许静默返回旧结果。
    let conflict = core
        .schedule_semantic_index("换了原因", "op-semantic")
        .unwrap_err();
    assert_eq!(
        conflict.code(),
        diary_core::ErrorCode::IdempotencyConflict,
        "同一个 operationId 换内容必须报冲突：{conflict}"
    );

    // 跑一轮：**只跑一条**（重复排队没有变成两份工作），并且真的把索引建起来了。
    let round = core.run_due_jobs(Utc::now(), 60_000, "manual", 64).unwrap();
    assert_eq!(round.processed, 1, "队列里只有一条语义任务");
    assert_eq!(round.succeeded, 1);
    assert_eq!(
        counters.documents.load(Ordering::SeqCst),
        2,
        "两块各算一次：重复排队没有让它算两遍"
    );
    let done = core.get_job(&job.id).unwrap();
    assert_eq!(done.state, JobState::Succeeded);
    assert_eq!(
        done.progress,
        Some(JobProgress {
            completed: 2,
            total: 2
        }),
        "跑完的进度是整代（completed == total）"
    );
    assert!(core.index_status(None).unwrap().semantic_index_ready);

    // 跑完之后再请求：这时才该建**新的**一条（用户要的是「按现状再来一遍」）。
    let again = core
        .schedule_semantic_index("再来一遍", "op-again")
        .unwrap();
    assert_ne!(again.id, job.id, "终态任务不参与业务键去重");
    assert_eq!(
        core.count_jobs(Some(&[JobState::Queued])).unwrap(),
        1,
        "新的一条在排队"
    );
}

// ------------------------------------------------------------ 2. 依赖门

/// 导入的提取还没跑完：语义任务落在 `waiting_dependency` + `waiting_extract`，
/// 不消耗重试额度；不变式（有等待态 ⇒ `next_wakeup` 非空）成立；提取完成后在
/// **同一次**驱动里跑完。
#[test]
fn a_semantic_job_waits_for_the_extraction_and_then_runs_in_the_same_round() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    let counters = Arc::new(Counters::default());
    core.set_embedder(stub(MODEL_A, 512, Duration::ZERO, &counters));

    // 导入一段文本：队列里排下 extract（对修订）与 index（对来源，等提取）。
    let capture_id = write_capture(&mut core, at(2026, 9, 20), "", "op-import");
    let text = "妈妈后来又写了一封信。\n";
    let bytes = text.as_bytes();
    let ticket = core
        .prepare_import(diary_core::ImportRequest {
            capture_id: &capture_id,
            display_name: "信.txt",
            mime_hint: Some("text/plain"),
            size_hint: Some(bytes.len() as i64),
            origin: diary_core::ImportOrigin::Picker,
            operation_id: "op-import-prepare",
        })
        .unwrap();
    std::fs::write(&ticket.staging_ticket, bytes).unwrap();
    core.finish_import(
        &ticket.import_id,
        &ticket.staging_ticket,
        diary_core::ImportManifest {
            copied_bytes: bytes.len() as i64,
            sha256: {
                use sha2::{Digest, Sha256};
                let mut hasher = Sha256::new();
                hasher.update(bytes);
                format!("{:x}", hasher.finalize())
            },
            detected_mime: "text/plain".to_owned(),
            original_name: "信.txt".to_owned(),
        },
    )
    .unwrap();

    let job = core
        .schedule_semantic_index("导入后建语义索引", "op-semantic-wait")
        .unwrap();

    // 预算 0 的一轮：什么都不跑，但闸门照旧把等待态推到真相。
    let idle = core.run_due_jobs(Utc::now(), 0, "startup", 64).unwrap();
    assert_eq!(idle.processed, 0);
    let waiting = core.get_job(&job.id).unwrap();
    assert_eq!(waiting.state, JobState::WaitingDependency);
    assert_eq!(
        waiting.error_code.as_deref(),
        Some("waiting_extract"),
        "在等哪一环要说清"
    );
    assert_eq!(waiting.attempt_count, 0, "等前置不动尝试次数");
    assert!(!waiting.requires_user_action, "等前置不是失败");
    assert_eq!(idle.due_backlog, 1, "到期没跑的是提取那条");
    // 不变式（#74）：还留着等待态就必须给出可用的下一次唤醒。
    assert!(
        idle.next_wakeup.is_some(),
        "有等待态却报「没有待办」就是挂死"
    );
    assert_eq!(core.job_status().unwrap().waiting_dependency, 2, "index 与 semantic_index 都在等");

    // 一次驱动：提取跑完 → 关键词与语义两条后继都在同一轮被拉起、跑完。
    let round = core.run_due_jobs(Utc::now(), 60_000, "foreground", 64).unwrap();
    assert_eq!(round.processed, 3, "extract + index + semantic_index");
    assert_eq!(round.succeeded, 3);
    assert_eq!(round.waiting_dependency, 0);
    assert_eq!(round.dependency_failed, 0);
    assert_eq!(round.due_backlog, 0);
    let done = core.get_job(&job.id).unwrap();
    assert_eq!(done.state, JobState::Succeeded);
    assert!(done.error_code.is_none(), "前置满足后不该留着在等的原因");
    assert_eq!(done.attempt_count, 1, "真正跑过一次才记一次尝试");
    let status = core.index_status(None).unwrap();
    assert!(status.keyword_index_ready, "关键词那一路照旧就绪");
    assert!(status.semantic_index_ready, "语义那一路也建好了");
    assert_eq!(status.model_version.as_deref(), Some(MODEL_A));
}

// ------------------------------------------------------------ 3. 等配置

/// 没有模型：语义任务落在 `waiting_configuration` + `waiting_model`（不是失败、不挂死，
/// 但**要用户去配模型**：`requires_user_action` + `attention_key` 都给出来），
/// 配置好了之后由下一次驱动的闸门拉起并跑完——不需要任何唤醒提示。
#[test]
fn without_a_model_the_semantic_job_waits_for_configuration_and_then_runs() {
    if model_configured() {
        eprintln!("跳过：这个进程配了本地模型，跑不到「没有模型」那条路");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    assert_eq!(seed_chunks(&mut core, 2), 2);

    let job = core
        .schedule_semantic_index("先排上，等模型", "op-no-model")
        .unwrap();
    let round = core.run_due_jobs(Utc::now(), 60_000, "foreground", 64).unwrap();
    assert_eq!(round.processed, 0, "没有模型就一块都不该算");
    assert_eq!(round.failed, 0, "等配置不是失败");
    let waiting = core.get_job(&job.id).unwrap();
    assert_eq!(waiting.state, JobState::WaitingConfiguration);
    assert_eq!(waiting.error_code.as_deref(), Some("waiting_model"));
    assert_eq!(
        waiting.attempt_count, 0,
        "闸门在领取前挡下：没跑过就不计尝试"
    );
    assert!(
        waiting.requires_user_action,
        "等配置要用户去配模型：不是失败，但也不是无需理会的状态"
    );
    assert_eq!(
        waiting.attention_key.as_deref(),
        Some("job-config:semantic_index:waiting_model"),
        "界面靠 attentionKey 深链到「配哪一个」"
    );
    assert!(waiting.next_attempt_at.is_none(), "排期照旧：它本来就入队即到期");
    // 状态摘要里看得见「等配置」，而不是混在排队里。
    let status = core.job_status().unwrap();
    assert_eq!(status.waiting_configuration, 1);
    assert_eq!(status.waiting_dependency, 0);
    // 等配置**不进 next_wakeup**：配置变化不是时间问题（不变式只管 waiting_dependency）。
    assert_eq!(round.next_wakeup, None);
    // 语义这一路如实说清为什么没就绪。
    let index_status = core.index_status(None).unwrap();
    assert!(!index_status.semantic_index_ready);
    assert!(
        index_status
            .reasons
            .iter()
            .any(|reason| reason.contains("还没有配置本地模型")),
        "要说清是「没配」：{:?}",
        index_status.reasons
    );

    // 「配置好了」：注入嵌入器（产品里是一次带模型配置的启动），下一次驱动就拉起它。
    let counters = Arc::new(Counters::default());
    core.set_embedder(stub(MODEL_A, 512, Duration::ZERO, &counters));
    let round = core.run_due_jobs(Utc::now(), 60_000, "foreground", 64).unwrap();
    assert_eq!(round.processed, 1, "闸门重新评估后它就该被拉起");
    assert_eq!(round.succeeded, 1);
    let done = core.get_job(&job.id).unwrap();
    assert_eq!(done.state, JobState::Succeeded);
    assert!(done.error_code.is_none());
    assert_eq!(done.attempt_count, 1);
    assert_eq!(counters.documents.load(Ordering::SeqCst), 2, "两块都算了");
    assert!(core.index_status(None).unwrap().semantic_index_ready);
}

// ------------------------------------------------------------ 4. 可恢复（同口径续跑）

/// 模拟「跑到一半被杀」：任务停在 `running`、`building_generation` 里已经写好一块向量。
/// 驱动回收它并**续跑**——只算没算完的块（不重头），半成品不会被切成生效代次。
#[test]
fn an_interrupted_semantic_job_resumes_without_redoing_finished_chunks() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.sqlite");
    let mut core = new_core(dir.path());
    let counters = Arc::new(Counters::default());
    core.set_embedder(stub(MODEL_A, 512, Duration::ZERO, &counters));
    assert_eq!(seed_chunks(&mut core, 3), 3);

    let job = core
        .schedule_semantic_index("建语义索引", "op-resume")
        .unwrap();
    // 上一次跑到一半：任务已被领取（`running`），第一块的向量已经写进 building 代次，
    // 进程没来得及继续就退出了（断电/被杀）。
    let claimed = core
        .claim_next_due_job(Utc::now())
        .unwrap()
        .expect("任务应当可领取");
    assert_eq!(claimed.id, job.id);
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
             VALUES (?1, 1, ?2, 512, 'f32', ?3, '2026-10-10T00:00:00.000Z')",
            params![
                chunk_id,
                MODEL_A,
                diary_core::vector_bytes(&vec![0.0_f32; 512])
            ],
        )
        .unwrap();
        conn.execute("UPDATE index_meta SET building_generation = 1", [])
            .unwrap();
    }

    // 不谎报：这一代还在建，生效代次是空的，状态要说得出这件事。
    let mid = core.index_status(None).unwrap();
    assert!(!mid.semantic_index_ready, "半成品不能叫就绪");
    assert_eq!(mid.model_version, None, "还没有生效代次就没有模型版本可报");
    assert!(
        mid.reasons
            .iter()
            .any(|reason| reason.contains("有一代在建")),
        "要说清「有一代在建」而不是含糊的未就绪：{:?}",
        mid.reasons
    );

    let before = counters.documents.load(Ordering::SeqCst);
    let round = core.run_due_jobs(Utc::now(), 60_000, "foreground", 64).unwrap();
    assert_eq!(round.recovered, 1, "上次没跑完的 running 任务要被回收");
    assert_eq!(round.succeeded, 1);
    assert_eq!(
        counters.documents.load(Ordering::SeqCst) - before,
        2,
        "只该算没算完的两块：已经写好的那一块续上，不重头"
    );

    let done = core.get_job(&job.id).unwrap();
    assert_eq!(done.state, JobState::Succeeded);
    assert_eq!(
        done.progress,
        Some(JobProgress {
            completed: 3,
            total: 3
        })
    );
    let conn = open(&path);
    let (active, building): (Option<i64>, Option<i64>) = conn
        .query_row(
            "SELECT active_generation, building_generation FROM index_meta WHERE id = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!((active, building), (Some(1), None), "原子切换：生效代次 1、没有在建");
    assert_eq!(
        count(
            &conn,
            "SELECT COUNT(*) FROM chunk_vectors WHERE generation = 1",
        ),
        3,
        "三块都有向量"
    );
    drop(conn);
    let after = core.index_status(None).unwrap();
    assert!(after.semantic_index_ready);
    assert_eq!(after.model_version.as_deref(), Some(MODEL_A));
    assert_eq!(after.embedded_chunks, 3);
}

// ------------------------------------------------------------ 5. 续跑前核对口径（能红）

/// **本片必须能红的测试**（把口径核对去掉就该红）：中断留下的那一代里混着**另一个模型**
/// 的向量时，整代废弃、按当前模型从零重算——绝不混两个向量空间。
///
/// 构造：三块里第一块是**当前模型（A）**已经写好的（本来可以续上），另有**一行属于模型 B**
/// 的向量（它瞄着的块已经不在块表里——被杀之前内容又变过，块 id 换了）。这一代是脏的
/// （有一条口径不一致），所以：
///
/// - **有核对**：整代（含 A 那块、含 B 那行）清掉，三块全部按 A 重算 → 生效代次口径唯一，
///   `documents` **+3**；
/// - **没有核对**：A 的那块被当成「已完成」（`documents` 只 +2），B 那行留到生效代次里 →
///   `index_status` 报 `Mixed`（`model_version = None`、`semantic_index_ready = false`），
///   语义检索整路不作数（#80）。两条断言各自都会红。
#[test]
fn a_foreign_caliber_left_by_an_interrupted_build_is_discarded_before_resuming() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.sqlite");
    let mut core = new_core(dir.path());
    let counters = Arc::new(Counters::default());
    core.set_embedder(stub(MODEL_A, 512, Duration::ZERO, &counters));
    assert_eq!(seed_chunks(&mut core, 3), 3);

    let job = core
        .schedule_semantic_index("建语义索引", "op-caliber")
        .unwrap();
    let claimed = core
        .claim_next_due_job(Utc::now())
        .unwrap()
        .expect("任务应当可领取");
    assert_eq!(claimed.id, job.id);
    {
        let conn = open(&path);
        let first_chunk: String = conn
            .query_row(
                "SELECT chunk_id FROM text_chunks ORDER BY day_key LIMIT 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        // 第一块：当前模型 A 已经算好（同口径续跑本该直接续上）。
        conn.execute(
            "INSERT INTO chunk_vectors (chunk_id, generation, model_version, dims, storage, \
                                        vector, created_at) \
             VALUES (?1, 1, ?2, 512, 'f32', ?3, '2026-10-10T00:00:00.000Z')",
            params![
                first_chunk,
                MODEL_A,
                diary_core::vector_bytes(&vec![0.0_f32; 512])
            ],
        )
        .unwrap();
        // 另一行属于模型 B（维度也不同），而且它瞄着的块已经不在块表里
        // （上一次建块时内容还是旧的，后来块换了 id）。真实库里这种行来自「先写向量、
        // 再删块」的中断；这里用 FK 关掉的直插来复现同一个状态。
        //
        // FK 只在这条直插上关：`chunk_vectors.chunk_id` 有外键，正常路径插不出孤儿行。
        conn.pragma_update(None, "foreign_keys", "OFF").unwrap();
        conn.execute(
            "INSERT INTO chunk_vectors (chunk_id, generation, model_version, dims, storage, \
                                        vector, created_at) \
             VALUES ('chunk_gone_before_the_kill', 1, ?1, 384, 'f32', ?2, \
                     '2026-10-10T00:00:00.000Z')",
            params![MODEL_B, diary_core::vector_bytes(&vec![0.0_f32; 384])],
        )
        .unwrap();
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();
        conn.execute("UPDATE index_meta SET building_generation = 1", [])
            .unwrap();
    }

    let before = counters.documents.load(Ordering::SeqCst);
    let round = core.run_due_jobs(Utc::now(), 60_000, "foreground", 64).unwrap();
    assert_eq!(round.recovered, 1);
    assert_eq!(round.succeeded, 1);
    assert_eq!(
        counters.documents.load(Ordering::SeqCst) - before,
        3,
        "整代废弃：连口径一致的那一块也要按当前模型重算（去掉核对这里只会 +2）"
    );

    let conn = open(&path);
    assert_eq!(
        count(
            &conn,
            &format!("SELECT COUNT(*) FROM chunk_vectors WHERE model_version = '{MODEL_B}'"),
        ),
        0,
        "另一个模型的向量一行都不许剩（去掉核对这里会留下那一行）"
    );
    assert_eq!(
        count(
            &conn,
            "SELECT COUNT(DISTINCT model_version) FROM chunk_vectors WHERE generation = 1",
        ),
        1,
        "生效代次只能有一个模型口径"
    );
    assert_eq!(
        count(
            &conn,
            "SELECT COUNT(DISTINCT dims) FROM chunk_vectors WHERE generation = 1",
        ),
        1,
        "维度也只能有一种（跨维度的余弦是垃圾但看起来正常）"
    );
    drop(conn);
    let status = core.index_status(None).unwrap();
    assert_eq!(
        status.model_version.as_deref(),
        Some(MODEL_A),
        "生效代次的口径就是当前模型（不是 Mixed、也不是 B）"
    );
    assert!(status.semantic_index_ready, "整代重算之后自愈");
}

// ------------------------------------------------------------ 6. 分段与预算

/// 很短的预算：整库重建在块与块之间**让出**（不是一跑到底）。让出时任务回到 `queued`、
/// 进度如实写进 `progress`、`attempt_count` 不涨、摘要给出可用的 `next_wakeup`；
/// 半成品不会被切成生效代次；下一轮接着跑完。
#[test]
fn a_short_budget_makes_the_job_yield_and_the_next_round_finishes_it() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.sqlite");
    let mut core = new_core(dir.path());
    let counters = Arc::new(Counters::default());
    // 每块 400 毫秒：预算 100 毫秒连一块都跑不完，但**至少跑完一块**才让出。
    core.set_embedder(stub(MODEL_A, 512, Duration::from_millis(400), &counters));
    assert_eq!(seed_chunks(&mut core, 3), 3);

    let job = core
        .schedule_semantic_index("建语义索引", "op-budget")
        .unwrap();

    // 预热一轮（预算 0，一条都不跑）：让「领取前的那点查询」进缓存，下面那条 100 毫秒
    // 的预算才是在量「任务本身有多长」，而不是在量冷缓存的 SQLite。
    let warmup = core.run_due_jobs(Utc::now(), 0, "startup", 64).unwrap();
    assert_eq!(warmup.processed, 0);
    assert_eq!(warmup.due_backlog, 1, "任务还在等着跑");

    // 第一轮：很短的预算 → 算出第一块之后让出。
    let first = core.run_due_jobs(Utc::now(), 100, "foreground", 64).unwrap();
    assert_eq!(first.processed, 1, "任务被领取并执行了一段");
    assert_eq!(first.succeeded, 0, "整代还没切换，绝不能算成功");
    assert_eq!(first.failed, 0, "让出不是失败");
    assert_eq!(first.retrying, 0, "让出也不排退避");
    assert_eq!(first.due_backlog, 1, "它还在队列里等着接着跑");
    assert_eq!(first.waiting_dependency, 0);
    assert!(
        first.next_wakeup.is_some(),
        "还有活没干完，摘要必须给出可用的下一次唤醒"
    );

    let yielded = core.get_job(&job.id).unwrap();
    assert_eq!(yielded.state, JobState::Queued, "让出回到排队，立刻可再跑");
    assert!(yielded.error_code.is_none());
    assert!(yielded.next_attempt_at.is_none(), "「接着跑」不是「等一等再试」");
    assert_eq!(
        yielded.attempt_count, 0,
        "让出不是一次失败：反复让出不能耗尽 max_attempts"
    );
    assert_eq!(
        yielded.progress,
        Some(JobProgress {
            completed: 1,
            total: 3
        }),
        "进度如实：算了一块，还差两块"
    );

    // 不谎报 + 原子：半成品没有被切成生效代次。
    let mid = core.index_status(None).unwrap();
    assert!(!mid.semantic_index_ready, "建到一半绝不能叫就绪");
    assert_eq!(mid.embedded_chunks, 0, "生效代次还没有这一代的向量");
    assert!(
        mid.reasons
            .iter()
            .any(|reason| reason.contains("有一代在建")),
        "要说清「有一代在建」：{:?}",
        mid.reasons
    );
    {
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
            (None, Some(1)),
            "让出之后：第 1 代还在建，生效代次仍是空的（没有半成品被切过去）"
        );
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM chunk_vectors WHERE generation = 1",
            ),
            1,
            "只写了一块——这正是让出的证据"
        );
    }
    // 让出那次尝试如实留在历史里（不是抹掉，也不是记成失败）。
    let outcome: String = open(&path)
        .query_row(
            "SELECT outcome FROM job_attempts WHERE job_id = ?1 ORDER BY attempt DESC LIMIT 1",
            params![job.id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(outcome, "yielded");

    // 第二轮：预算给够 → 接着没算完的两块跑完，然后才原子切换。
    let second = core.run_due_jobs(Utc::now(), 60_000, "foreground", 64).unwrap();
    assert_eq!(second.processed, 1);
    assert_eq!(second.succeeded, 1);
    assert_eq!(second.due_backlog, 0);
    assert_eq!(second.next_wakeup, None, "没有待办了");
    let done = core.get_job(&job.id).unwrap();
    assert_eq!(done.state, JobState::Succeeded);
    assert_eq!(
        done.progress,
        Some(JobProgress {
            completed: 3,
            total: 3
        })
    );
    assert_eq!(
        counters.documents.load(Ordering::SeqCst),
        3,
        "三块各算一次：让出之后是接着跑，不是从头再来"
    );
    {
        let conn = open(&path);
        let (active, building): (Option<i64>, Option<i64>) = conn
            .query_row(
                "SELECT active_generation, building_generation FROM index_meta WHERE id = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!((active, building), (Some(1), None));
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM chunk_vectors WHERE generation = 1",
            ),
            3
        );
    }
    let after = core.index_status(None).unwrap();
    assert!(after.semantic_index_ready);
    assert_eq!(after.embedded_chunks, 3);
    assert!(
        !after.reasons.iter().any(|reason| reason.contains("在建")),
        "建完了就不该再说「在建」：{:?}",
        after.reasons
    );

    // 语义检索这一路真的能用（真调用，不是看数字）。
    let session = core
        .start_search(
            diary_core::SearchRequest {
                query: "第 2 篇正文".to_owned(),
                mode: diary_core::SearchMode::Semantic,
                filters: SearchFilters::default(),
                page_size: 10,
            },
            1,
        )
        .unwrap();
    assert!(
        !session.results.is_empty(),
        "建好的语义索引要真的搜得到：{:?}",
        session.warnings
    );
}
// ------------------------------------------------------------ 7. 确定性失败不拖到退避之后

/// 声明 512 维、实际只给 384 维的桩：模拟「模型与向量维度对不上」这种**确定性**问题。
struct WrongDimsEmbedder;

impl Embedder for WrongDimsEmbedder {
    fn model_version(&self) -> &str {
        "stub-embedder@wrong-dims"
    }

    fn dims(&self) -> usize {
        512
    }

    fn options(&self) -> EmbedOptions {
        EmbedOptions::default()
    }

    fn embed_document(&self, _text: &str) -> diary_core::Result<Vec<f32>> {
        Ok(vec![0.0_f32; 384])
    }

    fn embed_query(&self, _text: &str) -> diary_core::Result<Vec<f32>> {
        Ok(vec![0.0_f32; 512])
    }
}

/// 维度不符是**确定性**问题：终态失败、不退避、不假装成功，也不留一条「可重试」的
/// 假希望（重试五轮再失败只会把问题拖到几小时之后才让用户看见）。
#[test]
fn a_deterministic_failure_goes_terminal_instead_of_backing_off() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    core.set_embedder(Box::new(WrongDimsEmbedder));
    assert_eq!(seed_chunks(&mut core, 1), 1);

    let job = core
        .schedule_semantic_index("建语义索引", "op-wrong-dims")
        .unwrap();
    let round = core.run_due_jobs(Utc::now(), 60_000, "foreground", 64).unwrap();
    assert_eq!(round.processed, 1);
    assert_eq!(round.failed, 1, "确定性失败进「已放弃」");
    assert_eq!(round.retrying, 0, "不退避：退避只会把失败拖到几小时之后");
    assert_eq!(round.succeeded, 0, "绝不能算成功");

    let failed = core.get_job(&job.id).unwrap();
    assert_eq!(failed.state, JobState::Failed);
    assert_eq!(failed.error_code.as_deref(), Some("invalid_state"));
    assert!(failed.requires_user_action, "要让用户看见并决定");
    assert!(failed.next_attempt_at.is_none(), "没有退避风暴");
    assert_eq!(failed.attempt_count, 1, "只试了一次");
    assert_eq!(round.next_wakeup, None, "终态任务不拖着唤醒");

    // 不谎报：没有生效代次，而且那一代还在建（坏向量一行都没写进去）。
    let status = core.index_status(None).unwrap();
    assert!(!status.semantic_index_ready);
    assert_eq!(status.model_version, None);
    assert_eq!(
        status.embedded_chunks, 0,
        "维度不符的向量一行都不该进库"
    );
}

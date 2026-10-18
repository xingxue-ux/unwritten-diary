//! `semantic_index` 任务的排队入口：`Core::schedule_semantic_index(reason, operationId)`。
//!
//! # 与关键词 `index` / `indexes.rebuild` 的关系
//!
//! 契约 4.4 节里 `indexes.rebuild` 写的是「只排一条 `index` 任务」——那条是**关键词**
//! 重建（`index_rebuild.rs`）。语义重建**没有**契约方法，所以这一片只提供核心入口：
//! 不假装桥接已经能调，也没有能力项（`wired_capabilities()` 一个都没加）。要给应用
//! 一个可调用入口，得先改契约（或提契约变更请求），那是下一步的事。
//!
//! 为什么不做成一个 kind 的两个「种类」：语义重建的前置（等模型配置）、成败口径、
//! 能不能分段都跟关键词那条不一样，而 `indexes.rebuild` 的既有语义又必须原样不动
//! （它只该重建关键词）。合成一个 kind 会让两个入口的语义互相牵扯。理由写在
//! `docs/architecture/m4-任务调度与补做.md`。
//!
//! # 排队口径（「什么时候该排」）
//!
//! 常规入口仍是**显式请求**（`Core::schedule_semantic_index` / 契约
//! `indexes.rebuildSemantic`）。**自动触发**（#128 复审拍板）只覆盖一种情况：生效代次
//! 的**整代口径**与当前不一致——`storage` / `dims`，或 `model_version`（只在**内存里
//! 已经有**版本时比）。这时核心**自己排一条去重的语义重建**，因为 `index_status`
//! 已经会对用户说「需要重建」，而 `apps/` 下没有 `rebuildSemanticIndex` 的调用点；
//! 状态文案不能替代可执行的入口。
//!
//! 自动触发在**两个发现点**上用同一套判据：
//!
//! 1. `Core::open`（`lib.rs` 的 `prepare`）：不加载模型，只读库里的列；
//! 2. 检索**加载模型之后**发现跨空间的那一刻（`search_session`）：`open` 时模型通常
//!    只 `Configured`（没加载、也不许为 discovery 去读文件），只有这里才拿得到
//!    `model_version`。换了一份同维度、同存储的模型文件，靠这一点才有人自动重建。
//!
//! 五个条件同时成立才排：
//!
//! 1. 模型 `Configured` / `Ready`（`NotConfigured` / `Failed` 一律不排）；
//! 2. 有生效代次、且那一代有向量；
//! 3. 整代口径与当前不一致（见上）；
//! 4. **没有活着的** `semantic_index` 任务（`state NOT IN ('succeeded','failed',
//!    'cancelled')`）——判据是**任务**，不是 `building_generation`；
//! 5. 同一口径自动尝试次数 < [`AUTO_ATTEMPT_LIMIT`]（持久标记在
//!    `index_meta.auto_rebuild_marker`）。
//!
//! 为什么第 1 条：没配模型的库自动排只会长期挂一条 `waiting_configuration` +
//! `requires_user_action` 的**噪声**任务，取消之后下次 `open` 还会复活（#128 复审
//! 第 1 条）。等模型配好，下一次 `open` / 检索会重新评估。
//!
//! 为什么第 4 条不看 `building_generation`：失败/取消**不会**清 `building_generation`
//! （进度要留着续跑），旧的判据一看到它就早退，于是「有一代在建」永远为真、自动重建
//! 再也不会排——那是一个新的「需重建但无入口」死角，和这次要修的问题同型（#128
//! 复审第 3 条）。有活任务时它自己会跑（失败/取消后由第 5 条决定是否再给一次机会）；
//! 没有活任务时，即使 `building_generation` 还在也照排——执行者会接着那一代续跑
//! （口径不一致时整代废弃重算，见 `semantic::build_generation`）。
//!
//! 为什么第 5 条：`failed` / `cancelled` 是终态，不在「活着」集合里；没有持久标记时
//! 每次 `open` 都会新建一条，`jobs` 表又没有清理机制，失败行只增不减（#128 复审
//! 第 2 条）。标记按**口径指纹**记次数：同一口径最多自动排两次——第一次 + 失败/取消
//! 后的一次重排（评审要求「cancel/failed 后只重排一次」）；第三次起交给显式入口。
//! 口径一变（重建成功换了代次、用户改了 storage/dims、模型版本从「未知」变成
//! 「已知」、或换了一份模型文件）指纹就变，自动尝试次数重新从 0 算——那是**新的**
//! 需要重建的事实，不是同一次失败的重试。
//!
//! 标记为什么必须占一个 schema 版本（15）：它要跨重启、按库存在；`jobs` 的业务键
//! `(kind, target_ids, input_snapshot_hash)` 正是去重键，借它存标记会让同一个库按口径
//! 碎成多条「活着」的重建任务，破坏「至多一条活着的整代重建」；v1–v14 也没有可借的
//! 键值表。`index_meta` 是单行的索引元数据，所以 v15 加列 + 写迁移（见 `schema::V15`）。
//!
//! **明确不做**：没配模型时排队；每次 `open` 无条件排队；因为「刚导入完」
//! 或「覆盖不全」就替用户决定花一次模型调用（「什么时候值得花一次模型调用」是产品
//! 决定，与 `search.upgrade` 同源）。判据都在既有信号里，不另造一份「该不该排」的
//! 查询 API（第二份口径最容易漂）：
//!
//! | 情况 | 在哪看得见 | 该怎么办 |
//! |---|---|---|
//! | 还没有生效代次 | `indexes.status`：`semantic_index_ready = false`、`model_version = null`、`reasons` 说「还没有生效的向量代次」 | 模型就绪时显式排一条（首次构建） |
//! | 生效代次覆盖不全 | `embedded_chunks < total_chunks`，`reasons` 说「生效代次只覆盖了 X 块里的 Y 块」 | 显式排一条（增量：口径一致的向量会复用） |
//! | 换过模型 / 混合代次 | 语义检索的警告「当前生效的向量代次是用另一个模型（或另一套口径）建的」；或 `reasons` 说「有 N 种模型口径」 | **口径不一致的自动触发**（模型加载后） |
//! | 上一次重建中断或让出 | `indexes.status` 的 `reasons` 说「有一代在建（第 N 代）」，且队列里有活任务 | 由那条活任务续跑；没有活任务时自动触发会重排一次 |
//! | 刚写进新内容 | 由调用方自己知道（导入完成、改过正文）；块表要等重建才有新块 | 要语义检索前先显式排一条 |
//! | **存量库 storage/dims 口径不一致** | `indexes.status` 的 `reasons`：「存储格式从 f32 变为 int8，需要重建」/「维度是……需要重建」 | **`open` / 检索发现后自动排一条（去重）** |
//!
//! # 幂等（重复排队不产生重复工作）
//!
//! 三层，与 `indexes.rebuild` 同一套机制：
//!
//! - **operationId 回执**（契约 1.2 的通用规则）：同一个 `operationId` + 同一个原因
//!   重复调用返回**同一条任务**（按 id 读它此刻的真实状态，不是入队那刻的旧快照）；
//!   同一个 `operationId` 换了原因报 `idempotency_conflict`；
//! - **任务业务键**：`(kind, target_ids = [], input_snapshot_hash = NULL)`。
//!   语义重建的对象**永远是同一张块表的同一个代次**（`target_ids` 为空 = 整库；
//!   代次与向量本来就是全局的），排第二份不会更快、只会白跑一遍复用复制——
//!   所以这一片**不给快照**，让「队列里已经有一条活着的语义重建」成为唯一的去重键。
//!   界面连点几次、几处代码同时请求，队列里都只有一条。
//!
//!   注意「活着」= `state NOT IN ('succeeded','failed','cancelled')`：一条语义重建
//!   跑完之后再请求会建**新的**一条（新代次、增量重算），这是对的——用户要的是
//!   「按现状再来一遍」；
//! - **自动尝试标记**（只约束自动触发，不约束显式请求）：同一口径最多自动排两次。

use rusqlite::{params, Connection, OptionalExtension};

use crate::error::Result;
use crate::model::{Job, JobPriority, NewJob};
use crate::support;
use crate::Core;

/// 幂等回执的 kind。与核心入口同名，便于排查（契约方法名那一层还没有）。
const RECEIPT_KIND: &str = "semantic_index.rebuild";

/// 同一口径最多自动尝试几次：**首次 + 一次失败/取消后的重排**。
///
/// 两条评审要求一起成立只能取 2：「不能每次 open 都新建一条」（failed/cancelled 是
/// 终态，不在活着集合里）与「cancel/failed 后只重排一次」。第三次起交给显式入口。
const AUTO_ATTEMPT_LIMIT: u32 = 2;

/// 核心在 `open`（或检索加载模型之后）检测到**整代口径不一致**时，自动排一条去重的
/// 语义重建：`semantic_index_ready=false` 时用户看到「需重建」，核心自己把任务排上，
/// 不依赖前端是否接了按钮。
///
/// 判据与理由见模块说明。这里只强调三条与旧实现不同的：
/// - 模型没 `Configured` / `Ready` 就**不排**（消掉 `waiting_configuration` 噪声）；
/// - 「正在建」的判据是**没有活着的 `semantic_index` 任务**，不是 `building_generation`
///   （失败/取消留下的 building 代次不该挡住重排）；
/// - 同一口径最多自动排 [`AUTO_ATTEMPT_LIMIT`] 次（持久标记；失败行不再随 open 增长）。
pub(crate) fn enqueue_for_caliber_mismatch(core: &mut Core) -> Result<Option<Job>> {
    // 1. 只在模型 Configured / Ready 时自动排。
    match core.embedder_readiness() {
        crate::semantic::EmbedderReadiness::NotConfigured
        | crate::semantic::EmbedderReadiness::Failed(_) => return Ok(None),
        crate::semantic::EmbedderReadiness::Configured
        | crate::semantic::EmbedderReadiness::Ready => {}
    }
    // 2. 队列里已经有活着的同类任务：它自己会跑/续跑，不新建。
    if live_job_exists(&core.conn)? {
        return Ok(None);
    }
    // 3. 没有生效代次 = 还没建过；首次构建仍走显式入口。
    let Some(active) = crate::semantic::active_generation(core)? else {
        return Ok(None);
    };
    let caliber = crate::semantic::active_caliber(core)?;
    if caliber.is_empty() {
        return Ok(None);
    }
    let current_storage = core.vector_storage().wire();
    let current_dims = core.current_dims();
    // **只在内存里已有版本时比**：open 时模型通常只 Configured，算哈希要读一遍
    // 24 MiB 模型文件，与「启动不做重活」冲突。未知只是「不比」，不是「一致」。
    let current_version = core.loaded_model_version();
    let storage_mismatch = caliber.storages.len() > 1
        || caliber
            .storages
            .iter()
            .any(|storage| storage != current_storage);
    let dims_mismatch = caliber.dims.len() > 1
        || caliber.dims.iter().any(|dims| *dims != current_dims);
    // 混合代次（不止一种模型版本）本身就是不一致；单一版本时才需要比当前版本。
    let model_mismatch = caliber.versions.len() > 1
        || current_version
            .as_deref()
            .is_some_and(|current| caliber.versions.iter().any(|version| version != current));
    if !(storage_mismatch || dims_mismatch || model_mismatch) {
        return Ok(None);
    }
    let signature = auto_signature(
        active,
        &caliber.versions,
        &caliber.dims,
        &caliber.storages,
        current_dims,
        current_storage,
        current_version.as_deref(),
    );
    enqueue_once(core, &signature)
}

/// 检索路径**加载模型之后**发现整代跨空间时调用：用同一条口径指纹排一次。
///
/// 与 `enqueue_for_caliber_mismatch` 的区别只是「已经知道」这件事——这里不再查库
/// 比口径（发现点已经把 [`crate::semantic::CaliberMismatch`] 算出来了），但**去重、
/// 活任务与自动尝试上限完全共用**，所以两个发现点不会各排一条。
pub(crate) fn enqueue_for_detected_mismatch(
    core: &mut Core,
    mismatch: &crate::semantic::CaliberMismatch,
) -> Result<Option<Job>> {
    let Some(active) = crate::semantic::active_generation(core)? else {
        return Ok(None);
    };
    let signature = auto_signature(
        active,
        &mismatch.versions,
        &mismatch.dims,
        &mismatch.storages,
        mismatch.current_dims,
        &mismatch.current_storage,
        Some(&mismatch.current_version),
    );
    enqueue_once(core, &signature)
}

/// 一次自动尝试：在**同一个写事务**里复查活任务、读尝试次数、入队、写标记。
///
/// 复查不是多余：`open` 与检索可能交错（长驻进程里两次 `open` 不共享内存态），把判据
/// 放进写事务才能保证「同一条活任务 + 同一口径只排一次」。
fn enqueue_once(core: &mut Core, signature: &str) -> Result<Option<Job>> {
    let tx = core.conn.transaction()?;
    if live_job_exists(&tx)? {
        return Ok(None);
    }
    let attempts = auto_attempts(&tx, signature)?;
    if attempts >= AUTO_ATTEMPT_LIMIT {
        return Ok(None);
    }
    let job = NewJob::new(crate::jobs::KIND_SEMANTIC_INDEX, JobPriority::Maintenance);
    let queued = crate::jobs::enqueue_in_tx(&tx, job)?;
    // 标记与入队同一个事务：不能出现「排了但没记次数」（下次 open 会再排）或
    // 「记了次数但没排」（这一轮白白少一次机会）。
    tx.execute(
        "UPDATE index_meta SET auto_rebuild_marker = ?1 WHERE id = 1",
        params![format!("{}:{signature}", attempts + 1)],
    )?;
    tx.commit()?;
    Ok(Some(queued))
}

/// 队列里有没有**活着的** `semantic_index`（终态不算）。
fn live_job_exists(conn: &Connection) -> Result<bool> {
    let mut statement = conn.prepare(
        "SELECT 1 FROM jobs WHERE kind = ?1 \
         AND state NOT IN ('succeeded', 'failed', 'cancelled') LIMIT 1",
    )?;
    Ok(statement.exists(params![crate::jobs::KIND_SEMANTIC_INDEX])?)
}

/// 同一口径指纹已经自动尝试过的次数；没有标记或指纹不同都是 0。
///
/// 标记编码 `<次数>:<fingerprint>`（`fingerprint` 是十六进制，不含冒号）。
/// 读不懂的标记按「没试过」处理：宁可多给一次机会，也不因为一行坏数据把入口关死。
fn auto_attempts(conn: &Connection, signature: &str) -> Result<u32> {
    let marker: Option<String> = conn
        .query_row(
            "SELECT auto_rebuild_marker FROM index_meta WHERE id = 1",
            [],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()?
        .flatten();
    Ok(marker
        .as_deref()
        .and_then(|value| value.split_once(':'))
        .filter(|(_, stored)| *stored == signature)
        .and_then(|(attempts, _)| attempts.parse::<u32>().ok())
        .unwrap_or(0))
}

/// 「同一口径」的持久指纹：重建成功后换代次、用户改配置、模型版本从未知变已知都会
/// 让它变——那是新的「需要重建」事实，自动尝试次数因此重新从 0 算。
fn auto_signature(
    active_generation: i64,
    versions: &[String],
    dims: &[i64],
    storages: &[String],
    current_dims: i64,
    current_storage: &str,
    current_version: Option<&str>,
) -> String {
    let dims = dims
        .iter()
        .map(i64::to_string)
        .collect::<Vec<_>>()
        .join(",");
    support::fingerprint(&[
        "semantic_index.auto",
        &active_generation.to_string(),
        &versions.join(","),
        &dims,
        &storages.join(","),
        &current_dims.to_string(),
        current_storage,
        current_version.unwrap_or("unknown"),
    ])
}

/// 排一条语义索引重建任务。
///
/// `target_ids` **总是为空**（整库）：代次与向量是全局的，按来源排既不省模型调用，
/// 又会让「一次只该有一条语义重建」的去重键碎成好几条。`reason` 只进 operationId
/// 的请求指纹（与 `indexes.rebuild` 一致：它是「这次操作的原因」，不是入库字段）。
pub(crate) fn schedule(core: &mut Core, reason: &str, operation_id: &str) -> Result<Job> {
    // operationId 的通用幂等（契约 1.2）：同一个 id 换了内容要报冲突，重复同一次
    // 操作要返回原结果。原因进指纹：换原因就算「换了内容」。
    //
    // 这一步在插入之前：重放时世界可能已经变了（那条任务已经跑完、被取消），但
    // 「同一次操作」仍然该返回原来那条任务，而不是突然新建一条。
    let fingerprint = support::fingerprint(&[RECEIPT_KIND, reason]);
    if let Some(json) = core.receipt(RECEIPT_KIND, &fingerprint, operation_id)? {
        let stored: SemanticReceipt = serde_json::from_str(&json)?;
        // 重放不返回入队那一刻的快照：按 id 读当前任务，状态才是真的。
        return crate::jobs::get(core, &stored.job_id);
    }

    // 不给快照：业务键 `(kind, [], NULL)` 就是「至多一条活着的整库语义重建」。
    let job = NewJob::new(crate::jobs::KIND_SEMANTIC_INDEX, JobPriority::Maintenance);

    let tx = core.conn.transaction()?;
    // `job.changed` 由队列在入队时发，这里不重复发。
    let queued = crate::jobs::enqueue_in_tx(&tx, job)?;
    let receipt = serde_json::to_string(&SemanticReceipt {
        job_id: queued.id.clone(),
    })?;
    crate::store_receipt(&tx, operation_id, RECEIPT_KIND, &fingerprint, &receipt)?;
    tx.commit()?;
    Ok(queued)
}

/// 幂等回执里存的东西。只存 id：任务状态会变，存快照就会返回旧状态。
#[derive(serde::Serialize, serde::Deserialize)]
struct SemanticReceipt {
    job_id: String,
}
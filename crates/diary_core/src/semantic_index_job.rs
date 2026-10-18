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
//! `indexes.rebuildSemantic`）。**唯一的自动触发**（#128 复审拍板）是：`Core::open`
//! 时发现生效代次的 `storage`（或 `dims`）与当前口径不一致，且没有 building 代次、
//! 队列里也没有活着的同类任务——这时核心**自己排一条去重的语义重建**。理由：
//! `index_status` 已经会对用户说「需要重建」，但 `apps/` 下还没有任何
//! `rebuildSemanticIndex` 调用点；状态文案不能替代可执行的入口。
//!
//! 这条窄条件只依赖**不加载模型**就能读到的两项（`storage` / `dims`），所以 open 不会
//! 为了发现它去建 ORT 会话。除这条之外的其它情况仍不自动排。判据都在既有信号里，
//! 不另造一份「该不该排」的查询 API（第二份口径最容易漂）：
//!
//! | 情况 | 在哪看得见 | 该怎么办 |
//! |---|---|---|
//! | 还没有生效代次 | `indexes.status`：`semantic_index_ready = false`、`model_version = null`、`reasons` 说「还没有生效的向量代次」 | 模型就绪时排一条 |
//! | 生效代次覆盖不全 | `embedded_chunks < total_chunks`，`reasons` 说「生效代次只覆盖了 X 块里的 Y 块」 | 排一条（增量：口径一致的向量会复用） |
//! | 换过模型 / 混合代次 | 语义检索的警告「当前生效的向量代次是用另一个模型（或另一套口径）建的」；或 `reasons` 说「有 N 种模型口径」 | 排一条（整代按当前模型重算） |
//! | 上一次重建中断或让出 | `indexes.status` 的 `reasons` 说「有一代在建（第 N 代）」 | 排一条（**续跑**，不重头） |
//! | 刚写进新内容 | 由调用方自己知道（导入完成、改过正文）；块表要等重建才有新块 | 要语义检索前先排一条 |
//! | **存量库 storage/dims 口径不一致** | `indexes.status` 的 `reasons`：「存储格式从 f32 变为 int8，需要重建」/「维度是……需要重建」 | **`open` 自动排一条（去重）** |
//!
//! **除上面那条窄条件外，为什么不自动排**（明确不做，也写进文档）：
//!
//! - 没配模型的库会长期挂着一条 `waiting_configuration` 的任务——那是噪声，不是
//!   「在等资源」；只有「存量库口径不一致」这种**已经确定需要重建**的情况才自动排，
//!   不会因为「刚导入完」就替用户决定花模型调用；
//! - 「什么时候值得花一次模型调用」是产品决定。契约里 `search.upgrade` 也是同一个
//!   口径：**升级是显式的，不是后台自动任务**。导入完成时自动排队还会让
//!   「导入 → 提取 → 关键词索引」这条链上多出一条与它争预算的重活；
//! - 「换过模型」的 `model_version` 要在**加载模型之后**（或读模型文件哈希）才知道新
//!   口径；自动触发只查不加载模型就能确定的 `storage` / `dims`，不为了 discovery
//!   去建 ORT 会话。
//!
//! # 幂等（重复排队不产生重复工作）
//!
//! 两层，与 `indexes.rebuild` 同一套机制：
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
//!   「按现状再来一遍」。

use crate::error::Result;
use crate::model::{Job, JobPriority, NewJob};
use crate::support;
use crate::Core;

/// 幂等回执的 kind。与核心入口同名，便于排查（契约方法名那一层还没有）。
const RECEIPT_KIND: &str = "semantic_index.rebuild";

/// 核心在 `open` 时检测到**存储格式或维度口径不一致**，自动排一条去重的语义重建：
/// `semantic_index_ready=false` 时用户看到「需重建」，核心自己把任务排上，不依赖前端
/// 是否接了按钮。
///
/// 只在三个条件同时成立时排：
/// 1. 有生效代次，且它的 `storage` / `dims` 与当前口径不一致（这两项不加载模型就能查）；
/// 2. 没有正在建的 building 代次（那说明已经有一次重建在跑/该续跑）；
/// 3. 队列里没有活着的同类任务——`enqueue_in_tx` 的业务键
///    `(kind, target_ids=[], snapshot=NULL)` 本身就是去重键，重复 `open` 返回同一条。
///
/// **不**在每次 `open` 都无条件排队：口径一致时什么都不做；任务成功重建后口径就一致了；
/// 任务失败/取消后再 `open` 会重新排一条（用户仍需要一次重建）。
pub(crate) fn enqueue_for_caliber_mismatch(core: &mut Core) -> Result<Option<Job>> {
    let (active, building) = crate::chunks::generations(core)?;
    if active.is_none() || building.is_some() {
        return Ok(None);
    }
    let caliber = crate::semantic::active_caliber(core)?;
    if caliber.is_empty() {
        return Ok(None);
    }
    let current_storage = core.vector_storage().wire();
    let current_dims = core.current_dims();
    let mismatch = caliber.storages.len() > 1
        || caliber
            .storages
            .iter()
            .any(|storage| storage != current_storage)
        || caliber.dims.len() > 1
        || caliber.dims.iter().any(|dims| *dims != current_dims);
    if !mismatch {
        return Ok(None);
    }
    // 业务键去重：队列里已有活着的 `semantic_index`（整库、无快照）就返回它，不新建。
    let job = NewJob::new(crate::jobs::KIND_SEMANTIC_INDEX, JobPriority::Maintenance);
    let tx = core.conn.transaction()?;
    let queued = crate::jobs::enqueue_in_tx(&tx, job)?;
    tx.commit()?;
    Ok(Some(queued))
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
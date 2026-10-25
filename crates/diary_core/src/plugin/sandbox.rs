//! Wasm 代码插件沙箱：wasmi 解释执行 + 权限宿主函数 + 资源预算。
//!
//! # 边界
//!
//! - 模块只能看见宿主显式挂上的 `unwritten` 导入；没有 WASI、没有文件系统、没有
//!   socket。想联网只能走 `provider_invoke`，而它要求清单声明、用户授权、且该
//!   Provider 已在 #66 配置并通过能力闸门。
//! - 线性内存上限 32 MiB（`StoreLimits`），指令预算 2000 万 fuel，墙钟上限 5 秒
//!   （宿主函数边界检查；纯计算由 fuel 兜底）。超出即中止，不拖垮核心。
//! - 宿主函数里的 panic 用 `catch_unwind` 包住；Wasm trap 只结束这一次调用。
//!   wasmi 是解释器，不 JIT、不执行本机代码，所以不需要子进程隔离（见架构文档
//!   「为什么不是子进程」）。
//!
//! # ABI v1
//!
//! 插件导出 `memory`、`run() -> i32`，可选 `abi_version() -> i32`。返回 0 表示成功。
//! 所有输入输出都通过 `unwritten` 命名空间下的导入函数传递；字符串是 UTF-8，
//! 指针/长度都指向插件自己的线性内存。详细表见 `plugins/README.md`。

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use wasmi::{
    Caller, Config, Engine, Extern, Linker, Memory, Module, Store, StoreLimits,
    StoreLimitsBuilder, TrapCode,
};

use crate::model::{PluginDerivedContent, PluginProviderFailure, PluginSourceRef, Provider};
use crate::provider::{adapter, normalize, transport::Transport};

/// 单次调用的指令预算。20M 条指令在 wasmi 下是几十到几百毫秒量级；正常插件远用不完。
pub const FUEL_BUDGET: u64 = 20_000_000;
/// 单插件线性内存上限（任务书 §8.3 的初始实验预算 32 MiB）。
pub const MEMORY_LIMIT_BYTES: usize = 32 * 1024 * 1024;
/// 单次调用墙钟上限。纯计算死循环由 fuel 中止；这个上限兜住慢宿主函数/慢 Provider。
pub const WALL_TIMEOUT_MS: u64 = 5_000;
/// 单次消息（输入/输出/单个导出物）上限：1 MiB。
pub const MAX_MESSAGE_BYTES: usize = 1024 * 1024;
/// 插件私有状态总配额：16 KiB。
pub const MAX_STATE_BYTES: usize = 16 * 1024;
/// 单条日志上限与条数上限。
pub const MAX_LOG_BYTES: usize = 2 * 1024;
pub const MAX_LOG_LINES: usize = 64;
/// 派生内容上限。
pub const MAX_DERIVED_ITEMS: usize = 32;
pub const MAX_DERIVED_BYTES: usize = 256 * 1024;
/// 导出物数量与总大小。
pub const MAX_ARTIFACTS: usize = 8;
pub const MAX_ARTIFACT_BYTES: usize = 1024 * 1024;
/// 单个 Provider 调用的超时（与 Wasm 墙钟分开；任务书要求指令预算不替代网络超时）。
pub const PROVIDER_TIMEOUT_MS: u64 = 15_000;
/// 单次 Provider 提示词/响应上限。
const MAX_PROVIDER_PROMPT_BYTES: usize = 8 * 1024;
const MAX_PROVIDER_RESULT_BYTES: usize = 32 * 1024;

// 宿主函数返回码。正数/0 是成功；负数表示失败，插件可以据此决定自己的返回码。
const ERR_DENIED: i32 = -1;
const ERR_NOT_FOUND: i32 = -2;
const ERR_INVALID: i32 = -3;
const ERR_BUDGET: i32 = -4;
const ERR_PROVIDER: i32 = -5;
const ERR_IO: i32 = -7;
const ERR_TOO_SMALL: i32 = -8;

/// 一次日记/材料记录。正文在准备阶段已经按上限截断，宿主函数只做授权与拷贝。
#[derive(Debug, Clone)]
pub(crate) struct DiaryEntry {
    pub version_id: String,
    pub day_key: String,
    pub body: String,
}

#[derive(Debug, Clone)]
pub(crate) struct SourceEntry {
    pub text: String,
}

/// 准备阶段已经过能力闸门的 Provider。密钥只在这个结构里，宿主函数不会回传给插件。
#[derive(Debug, Clone)]
pub(crate) struct PreparedProvider {
    /// 配置存在时是 Some；没配置时是 None（`ready` 一定是 false）。
    pub provider: Option<Provider>,
    pub secret: Option<String>,
    pub ready: bool,
    pub reason: String,
}

/// 沙箱的输入。全部由 `plugin::prepare` 从已授权的数据里准备，宿主函数不再碰数据库。
pub(crate) struct SandboxRequest {
    pub permissions: HashSet<String>,
    pub selected: Vec<DiaryEntry>,
    pub range: Vec<DiaryEntry>,
    pub sources: HashMap<String, SourceEntry>,
    pub assets: HashMap<String, PathBuf>,
    pub providers: HashMap<String, PreparedProvider>,
    pub transport: Arc<dyn Transport>,
    pub state: BTreeMap<String, String>,
    pub context_json: String,
    /// 墙钟上限。生产路径固定为 [`WALL_TIMEOUT_MS`]；测试可以调短，验证
    /// `expired()` 真的在宿主函数边界生效。
    pub wall_timeout_ms: u64,
}

/// 尚未落盘的导出物。写文件由核心做（限域目录），沙箱只拿字节。
#[derive(Debug, Clone)]
pub(crate) struct PendingArtifact {
    pub file_name: String,
    pub media_type: String,
    pub bytes: Vec<u8>,
}

/// 沙箱运行的结果。`status` 与 `PluginRunResult` 的 wire 值一致。
pub(crate) struct SandboxOutcome {
    pub status: &'static str,
    pub output: Option<String>,
    pub artifacts: Vec<PendingArtifact>,
    pub derived: Vec<PluginDerivedContent>,
    pub logs: Vec<String>,
    pub warnings: Vec<String>,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    pub instructions_used: Option<i64>,
    pub state: BTreeMap<String, String>,
    pub provider_failures: Vec<PluginProviderFailure>,
}

struct SandboxState {
    limits: StoreLimits,
    context_json: String,
    selected: Vec<DiaryEntry>,
    range: Vec<DiaryEntry>,
    sources: HashMap<String, SourceEntry>,
    assets: HashMap<String, PathBuf>,
    providers: HashMap<String, PreparedProvider>,
    transport: Arc<dyn Transport>,
    permissions: HashSet<String>,
    state: BTreeMap<String, String>,
    state_bytes: usize,
    logs: Vec<String>,
    derived: Vec<PluginDerivedContent>,
    derived_bytes: usize,
    artifacts: Vec<PendingArtifact>,
    artifact_bytes: usize,
    output: Option<String>,
    progress: Option<(i64, i64)>,
    denials: Vec<String>,
    errors: Vec<String>,
    provider_failures: Vec<PluginProviderFailure>,
    deadline: Instant,
    last_error: Option<(String, String)>,
    fatal_error: Option<(String, String)>,
}

impl SandboxState {
    fn expired(&self) -> bool {
        Instant::now() >= self.deadline
    }

    fn has(&self, permission: &str) -> bool {
        self.permissions.contains(permission)
    }

    fn note_error(&mut self, code: &str, message: String) {
        if self.last_error.is_none() {
            self.last_error = Some((code.to_owned(), message.clone()));
        }
        if self.errors.len() < 16 {
            self.errors.push(message);
        }
    }

    fn note_fatal(&mut self, code: &str, message: String) {
        if self.fatal_error.is_none() {
            self.fatal_error = Some((code.to_owned(), message.clone()));
        }
        self.note_error(code, message);
    }
}

/// 执行一个 Wasm 插件。**不返回 Err**：插件失败/超时/越权都落进结果，核心照常活着。
pub(crate) fn run(module_bytes: &[u8], request: SandboxRequest) -> SandboxOutcome {
    let started = Instant::now();
    let wall_timeout_ms = request.wall_timeout_ms;
    let state = SandboxState {
        limits: StoreLimitsBuilder::new()
            .memory_size(MEMORY_LIMIT_BYTES)
            .table_elements(10_000)
            .instances(1)
            .tables(1)
            .memories(1)
            .trap_on_grow_failure(true)
            .build(),
        context_json: request.context_json,
        selected: request.selected,
        range: request.range,
        sources: request.sources,
        assets: request.assets,
        providers: request.providers,
        transport: request.transport,
        permissions: request.permissions,
        state: request.state,
        state_bytes: 0,
        logs: Vec::new(),
        derived: Vec::new(),
        derived_bytes: 0,
        artifacts: Vec::new(),
        artifact_bytes: 0,
        output: None,
        progress: None,
        denials: Vec::new(),
        errors: Vec::new(),
        provider_failures: Vec::new(),
        deadline: Instant::now() + Duration::from_millis(wall_timeout_ms),
        last_error: None,
        fatal_error: None,
    };
    // 直接在启动时算一遍状态字节数，配额判定才不会被绕过。
    let mut state = state;
    state.state_bytes = state.state.iter().map(|(k, v)| k.len() + v.len()).sum();

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        execute(module_bytes, state)
    }));
    match result {
        Ok(execution) => finish(execution, started),
        Err(_) => SandboxOutcome {
            status: "failed",
            output: None,
            artifacts: Vec::new(),
            derived: Vec::new(),
            logs: vec!["宿主捕获到插件执行中的 panic，已隔离；核心继续工作".to_owned()],
            warnings: Vec::new(),
            error_code: Some("invalid_state".to_owned()),
            error_message: Some(
                "插件执行触发宿主 panic，已被 catch_unwind 隔离；这一次调用失败，核心没有受影响"
                    .to_owned(),
            ),
            instructions_used: None,
            state: BTreeMap::new(),
            provider_failures: Vec::new(),
        },
    }
}

struct Execution {
    state: SandboxState,
    error: Option<wasmi::Error>,
    run_code: i32,
    remaining_fuel: Option<u64>,
}

fn execute(module_bytes: &[u8], state: SandboxState) -> Execution {
    let mut config = Config::default();
    config.consume_fuel(true);
    let engine = Engine::new(&config);
    let module = match Module::new(&engine, module_bytes) {
        Ok(module) => module,
        Err(error) => {
            let mut state = state;
            state.note_error("invalid_state", format!("Wasm 模块校验失败：{error}"));
            return Execution {
                state,
                error: Some(error),
                run_code: 1,
                remaining_fuel: None,
            };
        }
    };
    let mut store = Store::new(&engine, state);
    store.limiter(|state| &mut state.limits);
    if let Err(error) = store.set_fuel(FUEL_BUDGET) {
        let mut state = store.into_data();
        state.note_error("invalid_state", format!("无法启用指令预算：{error}"));
        return Execution {
            state,
            error: Some(error),
            run_code: 1,
            remaining_fuel: None,
        };
    }
    let mut linker = Linker::new(&engine);
    if let Err(error) = define_host(&mut linker) {
        let mut state = store.into_data();
        state.note_error("invalid_state", format!("宿主接口注册失败：{error}"));
        return Execution {
            state,
            error: Some(error),
            run_code: 1,
            remaining_fuel: None,
        };
    }
    let instance = match linker.instantiate_and_start(&mut store, &module) {
        Ok(instance) => instance,
        Err(error) => {
            let remaining = store.get_fuel().ok();
            let mut state = store.into_data();
            state.note_error("invalid_state", trap_message(&error));
            return Execution {
                state,
                error: Some(error),
                run_code: 1,
                remaining_fuel: remaining,
            };
        }
    };
    // 可选 ABI 版本：有就必须是 1；没有按 v1 处理。
    if let Ok(abi) = instance.get_typed_func::<(), i32>(&store, "abi_version") {
        match abi.call(&mut store, ()) {
            Ok(1) => {}
            Ok(other) => {
                let remaining = store.get_fuel().ok();
                let mut state = store.into_data();
                state.note_error(
                    "invalid_state",
                    format!("插件声明 abi_version = {other}，宿主只支持 1"),
                );
                return Execution {
                    state,
                    error: Some(wasmi::Error::new("不支持的 ABI 版本")),
                    run_code: 1,
                    remaining_fuel: remaining,
                };
            }
            Err(error) => {
                let remaining = store.get_fuel().ok();
                let mut state = store.into_data();
                state.note_error("invalid_state", trap_message(&error));
                return Execution {
                    state,
                    error: Some(error),
                    run_code: 1,
                    remaining_fuel: remaining,
                };
            }
        }
    }
    let run = match instance.get_typed_func::<(), i32>(&store, "run") {
        Ok(run) => run,
        Err(error) => {
            let remaining = store.get_fuel().ok();
            let mut state = store.into_data();
            state.note_error(
                "invalid_state",
                format!("插件没有导出 `run() -> i32`：{error}"),
            );
            return Execution {
                state,
                error: Some(error),
                run_code: 1,
                remaining_fuel: remaining,
            };
        }
    };
    let call = run.call(&mut store, ());
    let remaining = store.get_fuel().ok();
    let mut state = store.into_data();
    if let Err(error) = &call {
        state.note_error("invalid_state", trap_message(error));
    }
    let run_code = match &call {
        Ok(code) => *code,
        Err(_) => 1,
    };
    Execution {
        state,
        error: call.err(),
        run_code,
        remaining_fuel: remaining,
    }
}

fn trap_message(error: &wasmi::Error) -> String {
    let mut message = error.to_string();
    if message.chars().count() > 500 {
        message = message.chars().take(500).collect::<String>() + "…";
    }
    if error.as_trap_code() == Some(TrapCode::OutOfFuel) {
        format!(
            "指令预算耗尽（{} 条指令）被中止，可能是死循环；{}",
            FUEL_BUDGET, message
        )
    } else {
        format!("Wasm 执行中止：{message}")
    }
}

fn finish(execution: Execution, started: Instant) -> SandboxOutcome {
    let Execution {
        state,
        error,
        run_code,
        remaining_fuel,
    } = execution;
    let instructions_used = remaining_fuel
        .map(|remaining| FUEL_BUDGET.saturating_sub(remaining).min(FUEL_BUDGET) as i64);
    let duration = started.elapsed().as_millis() as i64;

    // 越权优先：即使插件忽略了负返回码并「成功」返回 0，结局也是 denied。
    if !state.denials.is_empty() {
        let message = format!(
            "插件尝试了未授权的调用（{} 次）：{}",
            state.denials.len(),
            state.denials.join("；")
        );
        return outcome(
            "denied",
            &state,
            Some("permission_denied".to_owned()),
            Some(message),
            instructions_used,
            duration,
        );
    }
    if let Some(error) = error {
        let out_of_fuel = error.as_trap_code() == Some(TrapCode::OutOfFuel);
        let message = trap_message(&error);
        if out_of_fuel {
            return outcome(
                "timeout",
                &state,
                Some("quota_exceeded".to_owned()),
                Some(format!(
                    "插件超过指令预算/墙钟上限被中止：{message}；核心继续工作，可以重试或禁用这个插件"
                )),
                instructions_used,
                duration,
            );
        }
        if error.as_trap_code() == Some(TrapCode::GrowthOperationLimited)
            || message.to_ascii_lowercase().contains("resource")
            || message.to_ascii_lowercase().contains("growth operation")
        {
            return outcome(
                "budget_exceeded",
                &state,
                Some("quota_exceeded".to_owned()),
                Some(format!(
                    "插件超出内存/资源上限被中止：{message}；核心继续工作"
                )),
                instructions_used,
                duration,
            );
        }
        return outcome(
            "failed",
            &state,
            Some("invalid_state".to_owned()),
            Some(message),
            instructions_used,
            duration,
        );
    }
    if state.expired() {
        return outcome(
            "timeout",
            &state,
            Some("quota_exceeded".to_owned()),
            Some(format!(
                "插件调用超过墙钟上限 {WALL_TIMEOUT_MS} ms；核心继续工作"
            )),
            instructions_used,
            duration,
        );
    }
    if let Some((code, message)) = &state.fatal_error {
        // 宿主函数已经报了不可恢复的错（例如 Provider 调用失败）；插件返回非零也只是
        // 对同一个错误的二次表达，这里报告更具体的原因。
        return outcome(
            "failed",
            &state,
            Some(code.clone()),
            Some(message.clone()),
            instructions_used,
            duration,
        );
    }
    if run_code != 0 {
        return outcome(
            "failed",
            &state,
            Some("invalid_state".to_owned()),
            Some(format!(
                "插件 run() 返回 {run_code}（0 才是成功）{}",
                state
                    .last_error
                    .as_ref()
                    .map(|(_, message)| format!("；最后一条错误：{message}"))
                    .unwrap_or_default()
            )),
            instructions_used,
            duration,
        );
    }
    let mut out = outcome(
        "succeeded",
        &state,
        None,
        None,
        instructions_used,
        duration,
    );
    out.artifacts = state.artifacts.clone();
    out.output = state
        .output
        .clone()
        .or_else(|| {
            state
                .artifacts
                .iter()
                .find_map(|artifact| String::from_utf8(artifact.bytes.clone()).ok())
        });
    out
}

fn outcome(
    status: &'static str,
    state: &SandboxState,
    error_code: Option<String>,
    error_message: Option<String>,
    instructions_used: Option<i64>,
    duration: i64,
) -> SandboxOutcome {
    let mut logs = state.logs.clone();
    logs.push(format!("耗时 {duration} ms"));
    if let Some((done, total)) = state.progress {
        logs.push(format!("进度 {done}/{total}"));
    }
    SandboxOutcome {
        status,
        output: state.output.clone(),
        artifacts: state.artifacts.clone(),
        derived: state.derived.clone(),
        logs,
        warnings: Vec::new(),
        error_code,
        error_message,
        instructions_used,
        state: state.state.clone(),
        provider_failures: state.provider_failures.clone(),
    }
}

// ------------------------------------------------------------------ 宿主函数

/// 每个宿主函数入口都先检查墙钟。fuel 只约束 Wasm 指令，慢磁盘读/慢 Provider
/// 只能靠宿主函数边界拦住；返回 `-4` 后插件继续跑也没关系，run 结束会判超时。
macro_rules! wall_guard {
    ($caller:expr, $op:literal) => {
        if $caller.data().expired() {
            return Ok(budget(&mut $caller, $op));
        }
    };
}

fn define_host(linker: &mut Linker<SandboxState>) -> Result<(), wasmi::Error> {
    linker.func_wrap(
        "unwritten",
        "context_utf8",
        |mut caller: Caller<'_, SandboxState>, ptr: i32, cap: i32| -> Result<i32, wasmi::Error> {
            wall_guard!(caller, "context_utf8");
            let value = caller.data().context_json.clone();
            Ok(write_bytes(&mut caller, ptr, cap, value.as_bytes()))
        },
    )?;
    linker.func_wrap(
        "unwritten",
        "selected_diary_count",
        |mut caller: Caller<'_, SandboxState>| -> Result<i32, wasmi::Error> {
            wall_guard!(caller, "selected_diary_count");
            Ok(if caller.data().has(super::manifest::PERM_READ_SELECTED) {
                caller.data().selected.len() as i32
            } else {
                deny(
                    &mut caller,
                    "selected_diary_count",
                    "没有 read_selected_material 权限",
                )
            })
        },
    )?;
    linker.func_wrap(
        "unwritten",
        "selected_diary_day",
        |mut caller: Caller<'_, SandboxState>,
         index: i32,
         ptr: i32,
         cap: i32|
         -> Result<i32, wasmi::Error> {
            wall_guard!(caller, "selected_diary_day");
            if !caller.data().has(super::manifest::PERM_READ_SELECTED) {
                return Ok(deny(
                    &mut caller,
                    "selected_diary_day",
                    "没有 read_selected_material 权限",
                ));
            }
            let Some(entry) = caller.data().selected.get(index.max(0) as usize).cloned() else {
                return Ok(not_found(&mut caller, "selected_diary_day", "选中的日记下标越界"));
            };
            Ok(write_bytes(&mut caller, ptr, cap, entry.day_key.as_bytes()))
        },
    )?;
    linker.func_wrap(
        "unwritten",
        "selected_diary_body",
        |mut caller: Caller<'_, SandboxState>,
         index: i32,
         ptr: i32,
         cap: i32|
         -> Result<i32, wasmi::Error> {
            wall_guard!(caller, "selected_diary_body");
            if !caller.data().has(super::manifest::PERM_READ_SELECTED) {
                return Ok(deny(
                    &mut caller,
                    "selected_diary_body",
                    "没有 read_selected_material 权限",
                ));
            }
            let Some(entry) = caller.data().selected.get(index.max(0) as usize).cloned() else {
                return Ok(not_found(&mut caller, "selected_diary_body", "选中的日记下标越界"));
            };
            Ok(write_bytes(&mut caller, ptr, cap, entry.body.as_bytes()))
        },
    )?;
    linker.func_wrap(
        "unwritten",
        "range_diary_count",
        |mut caller: Caller<'_, SandboxState>| -> Result<i32, wasmi::Error> {
            wall_guard!(caller, "range_diary_count");
            Ok(if caller.data().has(super::manifest::PERM_READ_RANGE) {
                caller.data().range.len() as i32
            } else {
                deny(
                    &mut caller,
                    "range_diary_count",
                    "没有 read_date_range 权限",
                )
            })
        },
    )?;
    linker.func_wrap(
        "unwritten",
        "range_diary_day",
        |mut caller: Caller<'_, SandboxState>,
         index: i32,
         ptr: i32,
         cap: i32|
         -> Result<i32, wasmi::Error> {
            wall_guard!(caller, "range_diary_day");
            if !caller.data().has(super::manifest::PERM_READ_RANGE) {
                return Ok(deny(
                    &mut caller,
                    "range_diary_day",
                    "没有 read_date_range 权限",
                ));
            }
            let Some(entry) = caller.data().range.get(index.max(0) as usize).cloned() else {
                return Ok(not_found(&mut caller, "range_diary_day", "日期范围下标越界"));
            };
            Ok(write_bytes(&mut caller, ptr, cap, entry.day_key.as_bytes()))
        },
    )?;
    linker.func_wrap(
        "unwritten",
        "range_diary_body",
        |mut caller: Caller<'_, SandboxState>,
         index: i32,
         ptr: i32,
         cap: i32|
         -> Result<i32, wasmi::Error> {
            wall_guard!(caller, "range_diary_body");
            if !caller.data().has(super::manifest::PERM_READ_RANGE) {
                return Ok(deny(
                    &mut caller,
                    "range_diary_body",
                    "没有 read_date_range 权限",
                ));
            }
            let Some(entry) = caller.data().range.get(index.max(0) as usize).cloned() else {
                return Ok(not_found(&mut caller, "range_diary_body", "日期范围下标越界"));
            };
            Ok(write_bytes(&mut caller, ptr, cap, entry.body.as_bytes()))
        },
    )?;
    linker.func_wrap(
        "unwritten",
        "source_text",
        |mut caller: Caller<'_, SandboxState>,
         id_ptr: i32,
         id_len: i32,
         ptr: i32,
         cap: i32|
         -> Result<i32, wasmi::Error> {
            wall_guard!(caller, "source_text");
            if !caller.data().has(super::manifest::PERM_READ_SELECTED) {
                return Ok(deny(
                    &mut caller,
                    "source_text",
                    "没有 read_selected_material 权限",
                ));
            }
            let Ok(id) = read_string(&caller, id_ptr, id_len, 128) else {
                return Ok(invalid(&mut caller, "source_text", "sourceId 参数不合法"));
            };
            let Some(entry) = caller.data().sources.get(&id).cloned() else {
                return Ok(not_found(
                    &mut caller,
                    "source_text",
                    &format!("材料「{id}」不在这次选中的范围里"),
                ));
            };
            Ok(write_bytes(&mut caller, ptr, cap, entry.text.as_bytes()))
        },
    )?;
    linker.func_wrap(
        "unwritten",
        "asset_range",
        |mut caller: Caller<'_, SandboxState>,
         id_ptr: i32,
         id_len: i32,
         start: i64,
         len: i64,
         ptr: i32,
         cap: i32|
         -> Result<i32, wasmi::Error> {
            wall_guard!(caller, "asset_range");
            if !caller.data().has(super::manifest::PERM_READ_SELECTED) {
                return Ok(deny(
                    &mut caller,
                    "asset_range",
                    "没有 read_selected_material 权限",
                ));
            }
            if start < 0 || len <= 0 || len > 64 * 1024 {
                return Ok(invalid(
                    &mut caller,
                    "asset_range",
                    "start 要 >= 0，len 要在 1–65536 之间",
                ));
            }
            let Ok(id) = read_string(&caller, id_ptr, id_len, 128) else {
                return Ok(invalid(&mut caller, "asset_range", "assetId 参数不合法"));
            };
            let Some(path) = caller.data().assets.get(&id).cloned() else {
                return Ok(not_found(
                    &mut caller,
                    "asset_range",
                    &format!("原件「{id}」不在这次选中的范围里"),
                ));
            };
            match super::package::read_file_range(&path, start as u64, len as usize) {
                Ok(bytes) => Ok(write_bytes(&mut caller, ptr, cap, &bytes)),
                Err(error) => Ok(fail(
                    &mut caller,
                    ERR_IO,
                    "io_error",
                    format!("读取原件失败：{error}"),
                )),
            }
        },
    )?;
    linker.func_wrap(
        "unwritten",
        "provider_invoke",
        |mut caller: Caller<'_, SandboxState>,
         id_ptr: i32,
         id_len: i32,
         prompt_ptr: i32,
         prompt_len: i32,
         out_ptr: i32,
         out_cap: i32|
         -> Result<i32, wasmi::Error> {
            wall_guard!(caller, "provider_invoke");
            let Ok(provider_id) = read_string(&caller, id_ptr, id_len, 128) else {
                return Ok(invalid(&mut caller, "provider_invoke", "providerId 参数不合法"));
            };
            let permission = format!("{}{}", super::manifest::PERM_PROVIDER_PREFIX, provider_id);
            if !caller.data().has(&permission) {
                return Ok(deny(
                    &mut caller,
                    "provider_invoke",
                    &format!("没有 {permission} 权限"),
                ));
            }
            let Ok(prompt) = read_string(&caller, prompt_ptr, prompt_len, MAX_PROVIDER_PROMPT_BYTES)
            else {
                return Ok(invalid(
                    &mut caller,
                    "provider_invoke",
                    &format!("提示词超过 {MAX_PROVIDER_PROMPT_BYTES} 字节或参数不合法"),
                ));
            };
            let Some(prepared) = caller.data().providers.get(&provider_id).cloned() else {
                return Ok(fail(
                    &mut caller,
                    ERR_PROVIDER,
                    "provider_not_configured",
                    format!("服务「{provider_id}」没有配置，也没有在清单里声明"),
                ));
            };
            if !prepared.ready {
                return Ok(fail(
                    &mut caller,
                    ERR_PROVIDER,
                    "provider_not_configured",
                    prepared.reason,
                ));
            }
            let remaining_wall = caller
                .data()
                .deadline
                .saturating_duration_since(Instant::now());
            if remaining_wall.is_zero() {
                return Ok(budget(&mut caller, "provider_invoke"));
            }
            let Some(secret) = prepared.secret.clone() else {
                return Ok(fail(
                    &mut caller,
                    ERR_PROVIDER,
                    "provider_not_configured",
                    format!("服务「{provider_id}」没有可用密钥"),
                ));
            };
            let Some(provider_config) = prepared.provider.as_ref() else {
                return Ok(fail(
                    &mut caller,
                    ERR_PROVIDER,
                    "provider_not_configured",
                    prepared.reason,
                ));
            };
            let Some(adapter) = adapter::adapter_for(&provider_config.adapter_id) else {
                return Ok(fail(
                    &mut caller,
                    ERR_PROVIDER,
                    "provider_not_configured",
                    crate::provider::missing_adapter_reason(provider_config),
                ));
            };
            let adapter = match adapter {
                adapter::Adapter::Http(adapter) => adapter,
                adapter::Adapter::Local(_) => {
                    return Ok(fail(
                        &mut caller,
                        ERR_PROVIDER,
                        "provider_not_configured",
                        format!("服务「{provider_id}」是本机引擎，不能生成文本"),
                    ))
                }
            };
            let request = match adapter.generate(
                provider_config,
                &secret,
                &adapter::GenerateRequest {
                    prompt,
                    system: None,
                },
            ) {
                Ok(mut request) => {
                    // 网络超时与 Wasm 墙钟分开算，但都不能越过本次调用的剩余墙钟。
                    let wall_ms = remaining_wall.as_millis().min(u128::from(u64::MAX)) as u64;
                    request.timeout_ms = request
                        .timeout_ms
                        .min(PROVIDER_TIMEOUT_MS)
                        .min(wall_ms.max(1));
                    request
                }
                Err(error) => {
                    let code = error.code().wire().to_owned();
                    caller
                        .data_mut()
                        .provider_failures
                        .push(PluginProviderFailure {
                            provider_id: provider_id.clone(),
                            error_code: code.clone(),
                        });
                    return Ok(fail(
                        &mut caller,
                        ERR_PROVIDER,
                        &code,
                        format!("调用服务「{provider_id}」失败：{error}"),
                    ));
                }
            };
            let response = match caller.data().transport.send(&request) {
                Ok(response) => response,
                Err(error) => {
                    let normalized = normalize::transport_error(&error, Some(&secret));
                    let code = normalized.code().wire().to_owned();
                    caller
                        .data_mut()
                        .provider_failures
                        .push(PluginProviderFailure {
                            provider_id: provider_id.clone(),
                            error_code: code.clone(),
                        });
                    return Ok(fail(
                        &mut caller,
                        ERR_PROVIDER,
                        &code,
                        format!("调用服务「{provider_id}」失败：{normalized}"),
                    ));
                }
            };
            if !(200..300).contains(&response.status) {
                let normalized = normalize::http_error(&response, Some(&secret));
                let code = normalized.code().wire().to_owned();
                caller
                    .data_mut()
                    .provider_failures
                    .push(PluginProviderFailure {
                        provider_id: provider_id.clone(),
                        error_code: code.clone(),
                    });
                return Ok(fail(
                    &mut caller,
                    ERR_PROVIDER,
                    &code,
                    format!("服务「{provider_id}」返回错误：{normalized}"),
                ));
            }
            let outcome = match adapter.parse_generate(&response, Some(&secret)) {
                Ok(outcome) => outcome,
                Err(error) => {
                    let code = error.code().wire().to_owned();
                    caller
                        .data_mut()
                        .provider_failures
                        .push(PluginProviderFailure {
                            provider_id: provider_id.clone(),
                            error_code: code.clone(),
                        });
                    return Ok(fail(
                        &mut caller,
                        ERR_PROVIDER,
                        &code,
                        format!("解析服务「{provider_id}」的响应失败：{error}"),
                    ));
                }
            };
            let text = truncate_bytes(outcome.text, MAX_PROVIDER_RESULT_BYTES);
            Ok(write_bytes(&mut caller, out_ptr, out_cap, text.as_bytes()))
        },
    )?;
    linker.func_wrap(
        "unwritten",
        "emit_derived",
        |mut caller: Caller<'_, SandboxState>,
         kind_ptr: i32,
         kind_len: i32,
         text_ptr: i32,
         text_len: i32,
         refs_ptr: i32,
         refs_len: i32|
         -> Result<i32, wasmi::Error> {
            wall_guard!(caller, "emit_derived");
            if !caller.data().has(super::manifest::PERM_DERIVED) {
                return Ok(deny(
                    &mut caller,
                    "emit_derived",
                    "没有 create_derived_content 权限",
                ));
            }
            let Ok(kind) = read_string(&caller, kind_ptr, kind_len, 64) else {
                return Ok(invalid(&mut caller, "emit_derived", "kind 参数不合法"));
            };
            let Ok(text) = read_string(&caller, text_ptr, text_len, MAX_DERIVED_BYTES) else {
                return Ok(invalid(
                    &mut caller,
                    "emit_derived",
                    "派生内容超过 256 KiB 或参数不合法",
                ));
            };
            let Ok(refs_json) = read_string(&caller, refs_ptr, refs_len, 16 * 1024) else {
                return Ok(invalid(&mut caller, "emit_derived", "来源参数不合法"));
            };
            let refs: Vec<PluginSourceRef> = if refs_json.trim().is_empty() {
                Vec::new()
            } else {
                match serde_json::from_str(&refs_json) {
                    Ok(refs) => refs,
                    Err(error) => {
                        return Ok(invalid(
                            &mut caller,
                            "emit_derived",
                            &format!("来源不是合法 JSON：{error}"),
                        ))
                    }
                }
            };
            if kind.trim().is_empty() {
                return Ok(invalid(&mut caller, "emit_derived", "kind 不能为空"));
            }
            if text.len() > MAX_DERIVED_BYTES {
                return Ok(invalid(&mut caller, "emit_derived", "派生内容过大"));
            }
            {
                let data = caller.data();
                for source in &refs {
                    let allowed = data.sources.contains_key(&source.source_id)
                        || data
                            .selected
                            .iter()
                            .any(|entry| entry.version_id == source.source_id);
                    if !allowed {
                        return Ok(deny(
                            &mut caller,
                            "emit_derived",
                            &format!("来源「{}」不在这次授权范围里", source.source_id),
                        ));
                    }
                }
                if data.derived.len() >= MAX_DERIVED_ITEMS
                    || data.derived_bytes.saturating_add(text.len()) > MAX_DERIVED_BYTES
                {
                    return Ok(budget(&mut caller, "emit_derived"));
                }
            }
            let data = caller.data_mut();
            data.derived_bytes += text.len();
            data.derived.push(PluginDerivedContent {
                kind,
                text,
                source_refs: refs,
            });
            Ok(0)
        },
    )?;
    linker.func_wrap(
        "unwritten",
        "export_write",
        |mut caller: Caller<'_, SandboxState>,
         name_ptr: i32,
         name_len: i32,
         media_ptr: i32,
         media_len: i32,
         content_ptr: i32,
         content_len: i32|
         -> Result<i32, wasmi::Error> {
            wall_guard!(caller, "export_write");
            if !caller.data().has(super::manifest::PERM_EXPORT) {
                return Ok(deny(
                    &mut caller,
                    "export_write",
                    "没有 create_export_artifact 权限",
                ));
            }
            let Ok(name) = read_string(&caller, name_ptr, name_len, 80) else {
                return Ok(invalid(&mut caller, "export_write", "文件名参数不合法"));
            };
            let Ok(media) = read_string(&caller, media_ptr, media_len, 80) else {
                return Ok(invalid(&mut caller, "export_write", "mediaType 参数不合法"));
            };
            let Ok(content) = read_guest(&caller, content_ptr, content_len, MAX_ARTIFACT_BYTES)
            else {
                return Ok(invalid(
                    &mut caller,
                    "export_write",
                    "导出内容超过 1 MiB 或指针越界",
                ));
            };
            let Some(name) = sanitize_file_name(&name) else {
                return Ok(invalid(
                    &mut caller,
                    "export_write",
                    "文件名只能是单个路径段（不能有 /、\\、..）",
                ));
            };
            let media = if media.trim().is_empty() {
                "application/octet-stream".to_owned()
            } else if media.len() <= 80
                && media
                    .bytes()
                    .all(|byte| byte.is_ascii_graphic() || byte == b' ')
            {
                media
            } else {
                return Ok(invalid(&mut caller, "export_write", "mediaType 不合法"));
            };
            if caller.data().artifacts.len() >= MAX_ARTIFACTS
                || caller
                    .data()
                    .artifact_bytes
                    .saturating_add(content.len())
                    > MAX_ARTIFACT_BYTES
            {
                return Ok(budget(&mut caller, "export_write"));
            }
            {
                let data = caller.data_mut();
                data.artifact_bytes += content.len();
                if let Some(existing) = data
                    .artifacts
                    .iter_mut()
                    .find(|artifact| artifact.file_name == name)
                {
                    // 同名再写 = 追加（流式输出）；总大小仍受上限约束。
                    existing.bytes.extend_from_slice(&content);
                } else {
                    data.artifacts.push(PendingArtifact {
                        file_name: name.clone(),
                        media_type: media,
                        bytes: content,
                    });
                }
                if let Ok(text) = String::from_utf8(
                    data.artifacts
                        .iter()
                        .find(|artifact| artifact.file_name == name)
                        .map(|artifact| artifact.bytes.clone())
                        .unwrap_or_default(),
                ) {
                    data.output = Some(text);
                }
            }
            Ok(0)
        },
    )?;
    linker.func_wrap(
        "unwritten",
        "state_get",
        |mut caller: Caller<'_, SandboxState>, key_ptr: i32, key_len: i32, out_ptr: i32, cap: i32| -> Result<i32, wasmi::Error> {
            wall_guard!(caller, "state_get");
            let Ok(key) = read_string(&caller, key_ptr, key_len, 64) else {
                return Ok(invalid(&mut caller, "state_get", "key 参数不合法"));
            };
            let Some(value) = caller.data().state.get(&key).cloned() else {
                return Ok(not_found(
                    &mut caller,
                    "state_get",
                    &format!("插件状态里没有「{key}」"),
                ));
            };
            Ok(write_bytes(&mut caller, out_ptr, cap, value.as_bytes()))
        },
    )?;
    linker.func_wrap(
        "unwritten",
        "state_put",
        |mut caller: Caller<'_, SandboxState>,
         key_ptr: i32,
         key_len: i32,
         value_ptr: i32,
         value_len: i32|
         -> Result<i32, wasmi::Error> {
            wall_guard!(caller, "state_put");
            let Ok(key) = read_string(&caller, key_ptr, key_len, 64) else {
                return Ok(invalid(&mut caller, "state_put", "key 参数不合法"));
            };
            if key.is_empty()
                || !key.bytes().all(|byte| {
                    byte.is_ascii_lowercase()
                        || byte.is_ascii_digit()
                        || matches!(byte, b'.' | b'_' | b'-')
                })
            {
                return Ok(invalid(
                    &mut caller,
                    "state_put",
                    "key 只能是小写字母/数字/./_/-，且不能为空",
                ));
            }
            let Ok(value) = read_guest(&caller, value_ptr, value_len, 4 * 1024) else {
                return Ok(invalid(&mut caller, "state_put", "value 超过 4 KiB 或指针越界"));
            };
            let Ok(value) = String::from_utf8(value) else {
                return Ok(invalid(&mut caller, "state_put", "value 必须是 UTF-8 文本"));
            };
            let old = caller
                .data()
                .state
                .get(&key)
                .map(|value| value.len())
                .unwrap_or(0);
            let new_total = caller
                .data()
                .state_bytes
                .saturating_sub(old)
                .saturating_add(key.len() + value.len());
            if new_total > MAX_STATE_BYTES {
                return Ok(budget(&mut caller, "state_put"));
            }
            let data = caller.data_mut();
            data.state_bytes = new_total;
            data.state.insert(key, value);
            Ok(0)
        },
    )?;
    linker.func_wrap(
        "unwritten",
        "emit_output",
        |mut caller: Caller<'_, SandboxState>, ptr: i32, len: i32| -> Result<i32, wasmi::Error> {
            wall_guard!(caller, "emit_output");
            let Ok(text) = read_string(&caller, ptr, len, MAX_MESSAGE_BYTES) else {
                return Ok(invalid(
                    &mut caller,
                    "emit_output",
                    "输出超过 1 MiB 或参数不合法",
                ));
            };
            caller.data_mut().output = Some(text);
            Ok(0)
        },
    )?;
    linker.func_wrap(
        "unwritten",
        "report_progress",
        |mut caller: Caller<'_, SandboxState>, done: i32, total: i32| -> Result<i32, wasmi::Error> {
            wall_guard!(caller, "report_progress");
            if done < 0 || total < 0 || done > total {
                return Ok(invalid(
                    &mut caller,
                    "report_progress",
                    "进度要满足 0 <= done <= total",
                ));
            }
            caller.data_mut().progress = Some((done as i64, total as i64));
            Ok(0)
        },
    )?;
    linker.func_wrap(
        "unwritten",
        "log",
        |mut caller: Caller<'_, SandboxState>, level: i32, ptr: i32, len: i32| -> Result<i32, wasmi::Error> {
            wall_guard!(caller, "log");
            let Ok(message) = read_string(&caller, ptr, len, MAX_LOG_BYTES) else {
                return Ok(invalid(&mut caller, "log", "日志超过 2 KiB 或参数不合法"));
            };
            if caller.data().logs.len() >= MAX_LOG_LINES {
                return Ok(0);
            }
            let level = match level {
                0 => "debug",
                1 => "info",
                2 => "warn",
                _ => "error",
            };
            caller
                .data_mut()
                .logs
                .push(format!("[{level}] {message}"));
            Ok(0)
        },
    )?;
    Ok(())
}

// ------------------------------------------------------------------ 参数与内存

fn memory(caller: &Caller<'_, SandboxState>) -> Option<Memory> {
    match caller.get_export("memory")? {
        Extern::Memory(memory) => Some(memory),
        _ => None,
    }
}

fn read_guest(
    caller: &Caller<'_, SandboxState>,
    ptr: i32,
    len: i32,
    max: usize,
) -> std::result::Result<Vec<u8>, String> {
    if ptr < 0 || len < 0 {
        return Err("指针/长度不能为负".to_owned());
    }
    let len = len as usize;
    if len > max {
        return Err(format!("参数 {len} 字节，超过上限 {max}"));
    }
    let memory = memory(caller).ok_or_else(|| "插件没有导出 memory".to_owned())?;
    let size = memory.data_size(caller);
    let offset = ptr as usize;
    if offset.checked_add(len).is_none_or(|end| end > size) {
        return Err("越界访问线性内存".to_owned());
    }
    let mut buffer = vec![0; len];
    memory
        .read(caller, offset, &mut buffer)
        .map_err(|error| format!("读线性内存失败：{error}"))?;
    Ok(buffer)
}

fn read_string(
    caller: &Caller<'_, SandboxState>,
    ptr: i32,
    len: i32,
    max: usize,
) -> std::result::Result<String, String> {
    let bytes = read_guest(caller, ptr, len, max)?;
    String::from_utf8(bytes).map_err(|_| "参数不是 UTF-8".to_owned())
}

/// 写回插件内存。返回写入字节数；缓冲区不够返回 `ERR_TOO_SMALL`。
fn write_bytes(caller: &mut Caller<'_, SandboxState>, ptr: i32, cap: i32, bytes: &[u8]) -> i32 {
    if bytes.len() > MAX_MESSAGE_BYTES {
        return invalid(
            caller,
            "write_bytes",
            &format!("输出超过 {MAX_MESSAGE_BYTES} 字节"),
        );
    }
    if cap < 0 || (cap as usize) < bytes.len() {
        return too_small(caller, bytes.len(), cap);
    }
    if bytes.is_empty() {
        return 0;
    }
    let Some(memory) = memory(caller) else {
        return invalid(caller, "write_bytes", "插件没有导出 memory");
    };
    let offset = ptr as usize;
    let size = memory.data_size(&*caller);
    if offset.checked_add(bytes.len()).is_none_or(|end| end > size) {
        return invalid(caller, "write_bytes", "写线性内存越界");
    }
    match memory.write(&mut *caller, offset, bytes) {
        Ok(()) => bytes.len() as i32,
        Err(error) => invalid(caller, "write_bytes", &format!("写线性内存失败：{error}")),
    }
}

fn deny(caller: &mut Caller<'_, SandboxState>, op: &str, reason: &str) -> i32 {
    let message = format!("{op}：{reason}");
    let data = caller.data_mut();
    data.denials.push(message.clone());
    data.note_error("permission_denied", message);
    ERR_DENIED
}

fn not_found(caller: &mut Caller<'_, SandboxState>, op: &str, reason: &str) -> i32 {
    fail(caller, ERR_NOT_FOUND, "not_found", format!("{op}：{reason}"))
}

fn invalid(caller: &mut Caller<'_, SandboxState>, op: &str, reason: &str) -> i32 {
    fail(caller, ERR_INVALID, "invalid_argument", format!("{op}：{reason}"))
}

fn budget(caller: &mut Caller<'_, SandboxState>, op: &str) -> i32 {
    fail(
        caller,
        ERR_BUDGET,
        "quota_exceeded",
        format!("{op}：超出插件资源预算"),
    )
}

fn too_small(caller: &mut Caller<'_, SandboxState>, needed: usize, cap: i32) -> i32 {
    let message = format!(
        "缓冲区太小：需要 {needed} 字节，插件给了 {} 字节",
        cap.max(0)
    );
    caller
        .data_mut()
        .note_error("invalid_argument", message.clone());
    ERR_TOO_SMALL
}

fn fail(caller: &mut Caller<'_, SandboxState>, code: i32, error_code: &str, message: String) -> i32 {
    if error_code == "invalid_argument" || error_code == "not_found" {
        caller.data_mut().note_error(error_code, message);
    } else {
        caller.data_mut().note_fatal(error_code, message);
    }
    code
}

/// 文件名只能是单个路径段。扩展名与内容无关，核心不据此执行任何东西。
/// 按 UTF-8 边界截断（不能直接按字节下标切 String，会 panic）。
fn truncate_bytes(mut value: String, max: usize) -> String {
    if value.len() <= max {
        return value;
    }
    let mut boundary = max;
    while boundary > 0 && !value.is_char_boundary(boundary) {
        boundary -= 1;
    }
    value.truncate(boundary);
    value
}

fn sanitize_file_name(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed.len() > 80 || trimmed == "." || trimmed == ".." {
        return None;
    }
    if trimmed.contains('/') || trimmed.contains('\\') || trimmed.contains('\0') || trimmed.contains(':')
    {
        return None;
    }
    if !trimmed
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-'))
    {
        return None;
    }
    Some(trimmed.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::transport::PanicTransport;

    /// 一个不干别的、只在循环里反复调宿主函数的插件。宿主函数返回负数（墙钟
    /// 超限时是 -4）就立刻结束，所以测试不会等 20M 次循环跑完。
    fn host_call_loop_wasm() -> Vec<u8> {
        wat::parse_str(
            r#"(module
                 (import "unwritten" "selected_diary_count" (func $count (result i32)))
                 (memory (export "memory") 1)
                 (func (export "run") (result i32)
                   (local $i i32)
                   (local $r i32)
                   (block $done
                     (loop $again
                       (local.set $r (call $count))
                       (br_if $done (i32.lt_s (local.get $r) (i32.const 0)))
                       (local.set $i (i32.add (local.get $i) (i32.const 1)))
                       (br_if $again (i32.lt_u (local.get $i) (i32.const 20000000)))))
                   (i32.const 0)))"#,
        )
        .expect("测试用 WAT 必须能编译")
    }

    fn request(wall_timeout_ms: u64) -> SandboxRequest {
        SandboxRequest {
            permissions: [crate::plugin::manifest::PERM_READ_SELECTED.to_owned()]
                .into_iter()
                .collect(),
            selected: Vec::new(),
            range: Vec::new(),
            sources: HashMap::new(),
            assets: HashMap::new(),
            providers: HashMap::new(),
            transport: Arc::new(PanicTransport::default()),
            state: BTreeMap::new(),
            context_json: "{}".to_owned(),
            wall_timeout_ms,
        }
    }

    #[test]
    fn wall_clock_is_checked_at_host_function_boundary() {
        // 20ms 远短于 20M fuel 在 wasmi 下的执行时间。没有宿主函数入口检查时，
        // 插件会一直跑到 fuel 耗尽（约 3 秒），最终消息是「指令预算耗尽」；
        // 有检查时，第一次超时的宿主调用返回 -4，插件立刻退出，消息是墙钟超限。
        let outcome = run(&host_call_loop_wasm(), request(20));
        assert_eq!(outcome.status, "timeout", "{:?}", outcome.error_message);
        assert_eq!(outcome.error_code.as_deref(), Some("quota_exceeded"));
        let used = outcome.instructions_used.expect("应有指令计数");
        assert!(
            used < FUEL_BUDGET as i64 / 2,
            "墙钟应在 fuel 耗尽前生效，实际用了 {used} 条指令"
        );
        let message = outcome.error_message.unwrap_or_default();
        assert!(
            message.contains("调用超过墙钟上限"),
            "应是墙钟超限而不是 fuel 耗尽：{message}"
        );
        assert!(!message.contains("指令预算耗尽"), "{message}");
    }
}

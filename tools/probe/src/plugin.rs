//! 插件运行时候选验证：wasmi 解释执行 + 资源预算 + 宿主边界。
//!
//! 任务书 8.2 节要求：单插件线性内存、单次消息大小、受限指令预算、独立宿主请求
//! 超时都要是可强制的，超预算要中止调用并记录任务状态。这里验证其中三件能不能真的
//! 强制住：指令预算（fuel）、线性内存上限、以及宿主函数上的权限判定。
//!
//! 另外测一次调用开销，用于判断「每个插件调用都走解释器」是否可接受。

use std::time::Instant;

use anyhow::{bail, Result};
use wasmi::{Caller, Config, Engine, Linker, Module, Store, StoreLimits, StoreLimitsBuilder};

/// 探针用的最小插件模块。
///
/// - `add`：纯计算，用于量调用开销
/// - `spin`：死循环，用于把 fuel 烧干
/// - `grow`：申请线性内存，返回 `memory.grow` 的结果（-1 表示被拒）
/// - `try_read`：通过宿主函数尝试读取原件，宿主决定给不给
const PLUGIN_WAT: &str = r#"
(module
  (import "env" "read_asset" (func $read_asset (param i32) (result i32)))
  (memory (export "memory") 1)
  (data (i32.const 0) "hello from plugin")
  (func (export "add") (param i32 i32) (result i32)
    local.get 0
    local.get 1
    i32.add)
  (func (export "spin")
    (loop $forever
      br $forever))
  (func (export "grow") (param i32) (result i32)
    local.get 0
    memory.grow)
  (func (export "try_read") (param i32) (result i32)
    local.get 0
    call $read_asset)
)
"#;

/// 线性内存上限：2 页 = 128 KiB。初始 1 页，所以申请 5 页必然失败。
const MEMORY_LIMIT_BYTES: usize = 128 * 1024;

/// 单次调用的指令预算。
const FUEL_BUDGET: u64 = 1_000_000;

struct PluginState {
    limits: StoreLimits,
    /// 是否已经授权读取原件。默认拒绝。
    granted: bool,
}

pub fn run() -> Result<()> {
    println!("插件运行时候选验证（wasmi）");
    println!("===========================");

    let wasm = wat::parse_str(PLUGIN_WAT)?;
    println!("插件模块：{} 字节 WAT 编译产物", wasm.len());

    let mut config = Config::default();
    config.consume_fuel(true);
    let engine = Engine::new(&config);
    let module = Module::new(&engine, &wasm[..])?;

    let state = PluginState {
        limits: StoreLimitsBuilder::new()
            .memory_size(MEMORY_LIMIT_BYTES)
            .build(),
        granted: false,
    };
    let mut store = Store::new(&engine, state);
    store.limiter(|state| &mut state.limits);
    store.set_fuel(FUEL_BUDGET)?;

    let mut linker = Linker::new(&engine);
    // 宿主函数：默认拒绝，只有宿主显式授权才返回数据。
    // 这是「插件不能直接拿到原件」的唯一可强制边界。
    linker.func_wrap(
        "env",
        "read_asset",
        |caller: Caller<'_, PluginState>, asset_id: i32| -> Result<i32, wasmi::Error> {
            if caller.data().granted {
                Ok(asset_id)
            } else {
                Ok(-1)
            }
        },
    )?;

    let instance = linker.instantiate_and_start(&mut store, &module)?;

    // ---- 1. 调用开销 ----
    let add = instance.get_typed_func::<(i32, i32), i32>(&store, "add")?;
    let rounds = 20_000;
    let mut checksum = 0_i64;
    let start = Instant::now();
    for i in 0..rounds {
        store.set_fuel(FUEL_BUDGET)?;
        checksum += i64::from(add.call(&mut store, (i, 1))?);
    }
    let elapsed = start.elapsed();
    let per_call_us = elapsed.as_secs_f64() * 1_000_000.0 / f64::from(rounds);
    println!(
        "调用开销：{rounds} 次纯计算调用，{:.1} ms，平均 {per_call_us:.2} µs/次（含每次重置 fuel，校验和 {checksum}）",
        elapsed.as_secs_f64() * 1000.0
    );

    // ---- 2. 指令预算 ----
    let spin = instance.get_typed_func::<(), ()>(&store, "spin")?;
    store.set_fuel(FUEL_BUDGET)?;
    let spin_result = spin.call(&mut store, ());
    match spin_result {
        Ok(()) => bail!("死循环没有被 fuel 中止，指令预算不可强制"),
        Err(err) => {
            let remaining = store.get_fuel().unwrap_or(0);
            println!(
                "指令预算：死循环在 {} 条指令内被中止（预算 {FUEL_BUDGET}，剩余 {remaining}）",
                FUEL_BUDGET.saturating_sub(remaining)
            );
            println!("  中止原因：{err}");
        }
    }

    // ---- 3. 线性内存上限 ----
    let grow = instance.get_typed_func::<i32, i32>(&store, "grow")?;
    store.set_fuel(FUEL_BUDGET)?;
    let within = grow.call(&mut store, 1)?;
    store.set_fuel(FUEL_BUDGET)?;
    let beyond = grow.call(&mut store, 5)?;
    println!(
        "线性内存：上限 {} KiB（2 页）。申请 1 页 -> {within}；申请 5 页 -> {beyond}（-1 表示被拒）",
        MEMORY_LIMIT_BYTES / 1024
    );
    if beyond != -1 {
        bail!("超出内存上限的申请没有被拒绝");
    }

    // ---- 4. 宿主边界：越权读取 ----
    let try_read = instance.get_typed_func::<i32, i32>(&store, "try_read")?;
    store.set_fuel(FUEL_BUDGET)?;
    let denied = try_read.call(&mut store, 7)?;
    store.data_mut().granted = true;
    store.set_fuel(FUEL_BUDGET)?;
    let allowed = try_read.call(&mut store, 7)?;
    println!("宿主边界：未授权读取原件 -> {denied}（-1 表示拒绝）；授权后 -> {allowed}");
    if denied != -1 || allowed != 7 {
        bail!("宿主权限判定没有按预期生效");
    }

    println!("\n结论：fuel 指令预算、线性内存上限、宿主权限判定三项都可以强制。");
    println!("限制仍未验证的部分：宿主函数自身的超时（需要真实 IO 才能测）、");
    println!("插件包体与加载耗时（需要真实插件包）、以及在两端真机上的实际表现。");
    Ok(())
}
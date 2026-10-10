//! M0 技术验证脚手架。
//!
//! 这里放的是**验证代码，不是产品代码**：用来给契约 1.4 节里那几个未决实现选择
//! 拿到真实测量数字。结论记在 `docs/architecture/M0-技术验证.md`。
//!
//! 用法：
//! ```text
//! cargo run -p diary_probe -- short-word-search [片段数]
//! cargo run -p diary_probe -- plugin-runtime
//! cargo run -p diary_probe -- all [片段数]
//! ```

#[cfg(feature = "model")]
mod model;
#[cfg(feature = "plugin")]
mod plugin;
#[cfg(feature = "search")]
mod search;

use anyhow::Result;

fn usage() -> &'static str {
    "用法：\n  diary_probe short-word-search [片段数，默认 20000]\n  diary_probe plugin-runtime\n  diary_probe vector-model（需要 --features model 与本地权重）\n  diary_probe all [片段数]"
}

/// 只在需要检索的构建里解析片段数，否则 plugin-only 构建会报未使用变量。
#[cfg(feature = "search")]
fn segments_arg(args: &[String]) -> Result<usize> {
    Ok(args
        .get(1)
        .map(|value| value.parse::<usize>())
        .transpose()?
        .unwrap_or(20_000))
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let command = args.first().map(String::as_str).unwrap_or("all");

    match command {
        "short-word-search" => {
            #[cfg(feature = "search")]
            search::run(segments_arg(&args)?)?;
            #[cfg(not(feature = "search"))]
            anyhow::bail!("这个构建没有启用 search feature（尝试 cargo run -p diary_probe --features search）");
        }
        "vector-model" => {
            #[cfg(feature = "model")]
            model::run()?;
            #[cfg(not(feature = "model"))]
            anyhow::bail!("这个构建没有启用 model feature（尝试 cargo run -p diary_probe --features model）");
        }
        "plugin-runtime" => {
            #[cfg(feature = "plugin")]
            plugin::run()?;
            #[cfg(not(feature = "plugin"))]
            anyhow::bail!("这个构建没有启用 plugin feature（尝试 cargo run -p diary_probe --features plugin）");
        }
        "all" => {
            #[cfg(feature = "search")]
            search::run(segments_arg(&args)?)?;
            #[cfg(feature = "search")]
            println!();
            #[cfg(feature = "plugin")]
            plugin::run()?;
        }
        "-h" | "--help" | "help" => println!("{}", usage()),
        other => {
            eprintln!("未知子命令：{other}\n\n{}", usage());
            std::process::exit(2);
        }
    }

    // Android 上**必须**跳过 C 运行时的退出收尾（`exit()` → `__cxa_finalize`）。
    //
    // 原因见 docs/architecture/M0-技术验证.md 3.3：libonnxruntime.so 自己注册的
    // 静态析构会在退出时去锁一个已经销毁的 mutex，于是 `FORTIFY: pthread_mutex_lock
    // called on a destroyed mutex` 之后 SIGABRT（退出码 134）。这条路径跟 Rust 侧
    // 释放不释放无关（tombstone 停在 `__cxa_finalize → libonnxruntime.so`），所以
    // 「资源放进 static 不释放」挡不住它——桌面那条路径一挡就好了，Android 这条
    // 只能不让收尾跑。`libc::_exit` 直接走系统调用退出，不跑 atexit / 静态析构；
    // 所以要**先**把标准输出刷干净，否则最后几行会丢。
    //
    // 产品那边不需要这个：Android 回收应用进程用 SIGKILL，本来就不跑这些析构；
    // 只有「会被当成 CLI 跑、要求退出码」的入口（探针、测试）需要这一步。
    #[cfg(target_os = "android")]
    {
        use std::io::Write;
        let _ = std::io::stdout().flush();
        let _ = std::io::stderr().flush();
        // SAFETY: `_exit` 是 async-signal-safe 的进程退出，不返回。
        unsafe { libc::_exit(0) }
    }

    #[cfg(not(target_os = "android"))]
    Ok(())
}
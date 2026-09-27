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

#[cfg(feature = "plugin")]
mod plugin;
#[cfg(feature = "search")]
mod search;

use anyhow::Result;

fn usage() -> &'static str {
    "用法：\n  diary_probe short-word-search [片段数，默认 20000]\n  diary_probe plugin-runtime\n  diary_probe all [片段数]"
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
    Ok(())
}
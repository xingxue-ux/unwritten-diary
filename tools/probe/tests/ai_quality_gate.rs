//! AI 质量门禁的**进程退出码**回归（评审 #99）。
//!
//! CI 跑的就是 `cargo run -p diary_probe -- ai-quality`，所以这里不测函数、测进程：
//! 用例失败必须让 CLI 以非零退出码结束（CI 才会红），未判不算失败，负例意外通过
//! 也要红，而 `--live` 缺配置那条老路径仍然明确报错退出 1。
//!
//! 全程 `--replay`（不联网）：失败用例是把质量集的**副本**改坏造出来的，
//! 仓库里的质量集一个字节都不动。

#![cfg(feature = "search")]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// 仓库里的质量集目录。
fn repo_quality_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/quality")
}

/// 把三份质量集拷到临时目录（每个测试一个，互不干扰），返回目录。
///
/// 不用 `/tmp` 写死的路径：走 `std::env::temp_dir()`，名字带 tag 与 pid。
fn copy_quality_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("diary-probe-gate-{tag}-{}", std::process::id()));
    if dir.exists() {
        std::fs::remove_dir_all(&dir).expect("清得掉旧的临时质量集");
    }
    std::fs::create_dir_all(&dir).expect("建得出临时质量集目录");
    for name in [
        "ai-quality-set.json",
        "ai-injection-set.json",
        "ai-transcript-set.json",
    ] {
        std::fs::copy(repo_quality_dir().join(name), dir.join(name))
            .unwrap_or_else(|error| panic!("拷不动 {name}：{error}"));
    }
    dir
}

fn read_json(path: &Path) -> serde_json::Value {
    let raw = std::fs::read_to_string(path).unwrap_or_else(|error| panic!("读不到 {path:?}：{error}"));
    serde_json::from_str(&raw).unwrap_or_else(|error| panic!("{path:?} 不是合法 JSON：{error}"))
}

fn write_json(path: &Path, value: &serde_json::Value) {
    let text = serde_json::to_string_pretty(value).expect("序列化得动");
    std::fs::write(path, format!("{text}\n")).expect("写得进临时质量集");
}

/// 跑一次回放 CLI。把所有真模型环境变量摘掉：这些用例一条请求都不该发。
fn run_replay(quality_dir: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_diary_probe"))
        .args(["ai-quality", "--replay", "--quality-dir"])
        .arg(quality_dir)
        .env_remove("DIARY_AI_ENDPOINT")
        .env_remove("DIARY_AI_MODEL")
        .env_remove("DIARY_AI_KEY")
        .output()
        .expect("diary_probe 跑得起来")
}

fn stdout_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// 仓库里这份质量集是**绿的**：CLI 退出码 0；两条负例按预期失败、不算门禁失败。
#[test]
fn the_checked_in_replay_set_is_green_and_exits_zero() {
    let output = run_replay(&repo_quality_dir());
    let stdout = stdout_of(&output);
    assert!(
        output.status.success(),
        "仓库里的质量集必须让 CI 绿：status={:?}\nstdout={stdout}\nstderr={}",
        output.status,
        stderr_of(&output)
    );
    assert!(stdout.contains("合计："), "{stdout}");
    assert!(
        stdout.contains("2 条是质量集里**声明过的负例**"),
        "负例要按预期失败、不算门禁失败：{stdout}"
    );
    assert!(stdout.contains("0 条是**非预期失败**"), "{stdout}");
}

/// **能红**：故意让一条用例失败 → CLI 退出码非 0，CI 那条命令因此变红。
///
/// 改坏的是一条生成用例的 `replayOutput`（`😀` 这种判定器不认识的输出）——
/// 副本目录里改，仓库里的质量集不动。
#[test]
fn a_failing_case_makes_the_cli_exit_nonzero() {
    let dir = copy_quality_dir("fail");
    let path = dir.join("ai-quality-set.json");
    let mut set = read_json(&path);
    let case_id = set["cases"][0]["id"]
        .as_str()
        .expect("用例有 id")
        .to_owned();
    set["cases"][0]["replayOutput"] = serde_json::json!("今天的天气真是不错，我很喜欢这样的日子。");
    write_json(&path, &set);

    let output = run_replay(&dir);
    let stdout = stdout_of(&output);
    let stderr = stderr_of(&output);
    assert!(
        !output.status.success(),
        "有非预期失败时退出码必须非 0（评审 #99）：status={:?}\nstdout={stdout}",
        output.status
    );
    assert_eq!(
        output.status.code(),
        Some(1),
        "anyhow 的错误要以 1 退出：{:?}",
        output.status
    );
    assert!(
        stderr.contains("非预期失败"),
        "错误消息要说清是门禁失败：{stderr}"
    );
    assert!(
        stderr.contains(&case_id),
        "错误消息要点名失败用例 {case_id}：{stderr}"
    );
    assert!(stdout.contains("**失败**"), "{stdout}");
}

/// **未判不算失败**：判定器缺输入时（输出里有不认识的字符）门禁仍然绿——
/// 这条不红，但报表里要单独数出「未判」。
#[test]
fn an_unjudged_case_does_not_make_the_cli_red() {
    let dir = copy_quality_dir("unjudged");
    let path = dir.join("ai-quality-set.json");
    let mut set = read_json(&path);
    // 😀 是判定器不认识的字符：可判性判据给 `未判`，而**未判优先于失败**，
    // 所以这条用例的三态结论是「未判」，不是「失败」。
    set["cases"][0]["replayOutput"] = serde_json::json!("😀");
    write_json(&path, &set);

    let output = run_replay(&dir);
    let stdout = stdout_of(&output);
    assert!(
        output.status.success(),
        "未判不算失败，门禁不该红：status={:?}\nstdout={stdout}\nstderr={}",
        output.status,
        stderr_of(&output)
    );
    assert!(stdout.contains("**未判** 1 条"), "{stdout}");
    assert!(
        stdout.contains("0 条是**非预期失败**"),
        "未判不该被算成失败：{stdout}"
    );
}

/// **负例意外通过**也要红：`tr-04` 的回放输入改成与参考文本一致（基线跟着改，
/// 免得先被「回放基线对不上」判死），此时它「通过」了，而它声明的是 `replayVerdict: "fail"`。
#[test]
fn a_negative_control_that_stops_failing_makes_the_cli_red() {
    let dir = copy_quality_dir("negative-passed");
    let path = dir.join("ai-transcript-set.json");
    let mut set = read_json(&path);
    let case = set["cases"]
        .as_array_mut()
        .expect("cases 是数组")
        .iter_mut()
        .find(|case| case["id"] == "tr-04-repeat")
        .expect("质量集里有 tr-04-repeat");
    let reference = case["reference"].as_str().expect("有参考文本").to_owned();
    let report = diary_core::quality::cer(&reference, &reference);
    case["replayHypothesis"] = serde_json::json!(reference);
    case["replayBaseline"] = serde_json::json!({
        "referenceChars": report.reference_len,
        "hypothesisChars": report.hypothesis_len,
        "errors": report.errors(),
        "cer": report.cer.expect("同样的文本有 CER"),
    });
    write_json(&path, &set);

    let output = run_replay(&dir);
    let stderr = stderr_of(&output);
    assert!(
        !output.status.success(),
        "负例不红了说明判据丢了区分度，门禁必须红：status={:?}\nstdout={}",
        output.status,
        stdout_of(&output)
    );
    assert!(
        stderr.contains("负例") && stderr.contains("tr-04-repeat"),
        "{stderr}"
    );
}

/// `--live` 缺配置：**语义不变**——明确报错、退出码 1，不假装跑过、不静默变绿。
#[test]
fn live_without_config_still_exits_one_with_a_clear_error() {
    let output = Command::new(env!("CARGO_BIN_EXE_diary_probe"))
        .args(["ai-quality", "--live", "--quality-dir"])
        .arg(repo_quality_dir())
        .env_remove("DIARY_AI_ENDPOINT")
        .env_remove("DIARY_AI_MODEL")
        .env_remove("DIARY_AI_KEY")
        .output()
        .expect("diary_probe 跑得起来");
    assert_eq!(output.status.code(), Some(1), "{:?}", output.status);
    let stderr = stderr_of(&output);
    assert!(
        stderr.contains("真模型模式需要 DIARY_AI_ENDPOINT 与 DIARY_AI_MODEL"),
        "{stderr}"
    );
}
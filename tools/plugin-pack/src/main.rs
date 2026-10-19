//! `plugin-pack`：示例插件的**打包检查/构建**工具（不是产品运行时）。
//!
//! ```text
//! cargo run -p plugin_pack -- check plugins/examples/style-daily-note
//! cargo run -p plugin_pack -- build plugins/examples/export-diary-markdown
//! ```
//!
//! `build` 会把目录顶层的每个 `*.wat` 编成同名 `.wasm`，然后按清单之外的
//! 全部文件重算 sha256 写回 `packageHashes`。`check` 只读：它不编译、不执行任何
//! 包内容，只验证清单字段与哈希，和核心安装路径的口径一致（核心仍然是权威）。

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

fn main() {
    if let Err(error) = run() {
        eprintln!("plugin-pack: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let command = args.next().ok_or_else(usage)?;
    let dir = args.next().ok_or_else(usage)?;
    let dir = PathBuf::from(dir);
    match command.as_str() {
        "build" => build(&dir),
        "check" => check(&dir),
        "hash" => {
            let manifest = read_manifest(&dir)?;
            write_hashes(&dir, manifest)?;
            println!("已重算 packageHashes：{}", dir.display());
            Ok(())
        }
        other => Err(format!("未知命令「{other}」\n{}", usage())),
    }
}

fn usage() -> String {
    "用法：plugin-pack <build|check|hash> <插件目录>".to_owned()
}

fn build(dir: &Path) -> Result<(), String> {
    let entries = fs::read_dir(dir).map_err(|error| format!("读目录失败：{error}"))?;
    let mut built = 0;
    for entry in entries {
        let path = entry.map_err(|error| error.to_string())?.path();
        if path.extension().and_then(|value| value.to_str()) != Some("wat") {
            continue;
        }
        let bytes = fs::read_to_string(&path).map_err(|error| format!("读 WAT 失败：{error}"))?;
        let wasm = wat::parse_str(&bytes).map_err(|error| format!("编译 {} 失败：{error}", path.display()))?;
        let target = path.with_extension("wasm");
        fs::write(&target, wasm).map_err(|error| format!("写 {} 失败：{error}", target.display()))?;
        built += 1;
    }
    let manifest = read_manifest(dir)?;
    write_hashes(dir, manifest)?;
    println!("已编译 {built} 个 WAT，并重算 packageHashes：{}", dir.display());
    Ok(())
}

fn check(dir: &Path) -> Result<(), String> {
    let manifest = read_manifest(dir)?;
    let listed = manifest
        .get("packageHashes")
        .and_then(|value| value.as_object())
        .ok_or_else(|| "manifest.json 缺少 packageHashes 对象".to_owned())?;
    let mut files = BTreeMap::new();
    collect(dir, dir, &mut files)?;
    files.remove("manifest.json");
    let mut ok = true;
    for (name, bytes) in &files {
        let Some(expected) = listed.get(name).and_then(|value| value.as_str()) else {
            println!("✗ 未列哈希：{name}");
            ok = false;
            continue;
        };
        let actual = sha256_hex(bytes);
        if expected.trim_start_matches("sha256:").to_ascii_lowercase() != actual {
            println!("✗ 哈希不符：{name}（清单 {expected}，实际 {actual}）");
            ok = false;
        }
    }
    for name in listed.keys() {
        if !files.contains_key(name) {
            println!("✗ 清单列了但包里没有：{name}");
            ok = false;
        }
    }
    if !ok {
        return Err("包检查失败".to_owned());
    }
    let id = manifest.get("id").and_then(|value| value.as_str()).unwrap_or("?");
    let version = manifest.get("version").and_then(|value| value.as_str()).unwrap_or("?");
    println!("✓ {id} {version}：{} 个文件，清单与哈希一致（未执行包内容）", files.len());
    Ok(())
}

fn read_manifest(dir: &Path) -> Result<serde_json::Value, String> {
    let path = dir.join("manifest.json");
    let text = fs::read_to_string(&path)
        .map_err(|error| format!("读 {} 失败：{error}", path.display()))?;
    serde_json::from_str(&text).map_err(|error| format!("manifest.json 不是合法 JSON：{error}"))
}

fn write_hashes(dir: &Path, mut manifest: serde_json::Value) -> Result<(), String> {
    let mut files = BTreeMap::new();
    collect(dir, dir, &mut files)?;
    files.remove("manifest.json");
    let mut hashes = serde_json::Map::new();
    for (name, bytes) in files {
        hashes.insert(name, serde_json::Value::String(sha256_hex(&bytes)));
    }
    manifest["packageHashes"] = serde_json::Value::Object(hashes);
    let text = serde_json::to_string_pretty(&manifest).map_err(|error| error.to_string())? + "\n";
    fs::write(dir.join("manifest.json"), text).map_err(|error| error.to_string())?;
    Ok(())
}

fn collect(root: &Path, current: &Path, files: &mut BTreeMap<String, Vec<u8>>) -> Result<(), String> {
    for entry in fs::read_dir(current).map_err(|error| error.to_string())? {
        let path = entry.map_err(|error| error.to_string())?.path();
        let metadata = fs::symlink_metadata(&path).map_err(|error| error.to_string())?;
        if metadata.file_type().is_symlink() {
            return Err(format!("不允许符号链接：{}", path.display()));
        }
        if metadata.is_dir() {
            collect(root, &path, files)?;
            continue;
        }
        if !metadata.is_file() {
            return Err(format!("只允许普通文件：{}", path.display()));
        }
        let relative = path
            .strip_prefix(root)
            .map_err(|_| "文件不在包目录下".to_owned())?
            .components()
            .map(|component| component.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/");
        if relative.starts_with(".git") {
            continue;
        }
        let bytes = fs::read(&path).map_err(|error| error.to_string())?;
        files.insert(relative, bytes);
    }
    Ok(())
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

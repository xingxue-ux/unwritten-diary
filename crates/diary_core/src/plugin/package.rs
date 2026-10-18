//! 插件包：目录/zip 读取、清单与逐文件哈希校验、静态检查、安装落盘。
//!
//! **安装/检查不执行包内容**：Wasm 只看魔数与版本，不实例化、不调用 `start`；
//! 声明式入口只按 JSON 解析模板。所有写入都落在资料库根目录的
//! `plugins/<id>/<version>-<hash8>/` 下，路径由宿主拼，插件不能自己选。

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{Read, Seek};
use std::path::{Component, Path};

use sha2::{Digest, Sha256};

use crate::error::{CoreError, Result};
use crate::model::{PluginEntryKind, PluginManifest, PluginTemplate};

use super::manifest::{
    self, MAX_FILE_BYTES, MAX_PACKAGE_BYTES, MAX_PACKAGE_FILES, PLUGIN_API_VERSION,
};
use super::template;

/// 读出来的包内容（全部在内存里，受大小上限约束）。
pub(crate) struct PackageContents {
    pub kind: &'static str,
    pub files: BTreeMap<String, Vec<u8>>,
    pub manifest: PluginManifest,
    pub templates: Vec<PluginTemplate>,
    pub package_hash: String,
    /// 兼容性/结构问题；空表示可安装。
    pub issues: Vec<String>,
}

impl PackageContents {
    pub fn compatible(&self) -> bool {
        self.issues.is_empty()
    }

}

/// 读取并静态校验一个插件包（目录或 zip）。**不执行任何包内容**。
pub(crate) fn load(path: &Path, platform: &str) -> Result<PackageContents> {
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            CoreError::NotFound {
                entity: "插件包",
                id: path.to_string_lossy().into_owned(),
            }
        } else {
            CoreError::Io(error)
        }
    })?;
    if metadata.file_type().is_symlink() {
        return Err(CoreError::PluginPackageInvalid {
            reason: "插件包本身不能是符号链接".to_owned(),
        });
    }
    let (kind, files) = if metadata.is_dir() {
        ("directory", read_directory(path)?)
    } else if metadata.is_file() {
        ("zip", read_zip(path)?)
    } else {
        return Err(CoreError::PluginPackageInvalid {
            reason: "插件包必须是目录或 zip 文件".to_owned(),
        });
    };

    if files.len() > MAX_PACKAGE_FILES {
        return Err(CoreError::PluginPackageInvalid {
            reason: format!(
                "包里有 {} 个文件，超过上限 {MAX_PACKAGE_FILES}",
                files.len()
            ),
        });
    }
    let total: usize = files.values().map(Vec::len).sum();
    if total > MAX_PACKAGE_BYTES {
        return Err(CoreError::PluginPackageInvalid {
            reason: format!("包解压后有 {total} 字节，超过上限 {MAX_PACKAGE_BYTES} 字节"),
        });
    }
    let manifest_bytes = files.get("manifest.json").ok_or_else(|| {
        CoreError::PluginPackageInvalid {
            reason: "包根目录没有 manifest.json".to_owned(),
        }
    })?;
    let manifest = manifest::parse(manifest_bytes)?;

    // packageHashes 必须**恰好**覆盖除 manifest.json 外的所有文件：多列一个、
    // 少列一个都拒绝。少列会留下未校验的可执行文件，多列说明包被改过。
    let actual: BTreeSet<&str> = files
        .keys()
        .map(String::as_str)
        .filter(|name| *name != "manifest.json")
        .collect();
    let listed: BTreeSet<&str> = manifest
        .package_hashes
        .keys()
        .map(String::as_str)
        .collect();
    if actual != listed {
        let missing: Vec<&str> = actual.difference(&listed).copied().collect();
        let extra: Vec<&str> = listed.difference(&actual).copied().collect();
        return Err(CoreError::PluginPackageInvalid {
            reason: format!(
                "packageHashes 与包内文件不一致：未列出的文件 {missing:?}，列了但包里没有的 {extra:?}"
            ),
        });
    }
    for (name, bytes) in &files {
        if name == "manifest.json" {
            continue;
        }
        let expected = manifest::normalize_sha256(
            manifest
                .package_hashes
                .get(name)
                .expect("上面的集合比较已经保证存在"),
        );
        let actual_hash = sha256_hex(bytes);
        if expected != actual_hash {
            return Err(CoreError::PluginPackageInvalid {
                reason: format!(
                    "文件「{name}」的哈希对不上：清单写 {expected}，实际 {actual_hash}"
                ),
            });
        }
    }

    // Wasm 只做静态检查（魔数 + 版本），**不实例化**。
    if manifest.entry_kind == PluginEntryKind::Wasm {
        let entry = files.get(&manifest.entry_file).expect("清单校验过存在");
        if entry.len() < 8 || &entry[..4] != b"\0asm" || entry[4..8] != [1, 0, 0, 0] {
            return Err(CoreError::PluginPackageInvalid {
                reason: format!(
                    "entryFile「{}」不是 WebAssembly 1.0 模块（只做静态魔数检查，未执行）",
                    manifest.entry_file
                ),
            });
        }
    }

    let templates = if manifest.entry_kind == PluginEntryKind::Declarative {
        let entry = files.get(&manifest.entry_file).expect("清单校验过存在");
        template::parse_declarative(&manifest.id, entry)?
    } else {
        Vec::new()
    };

    let mut issues = manifest::validate(&manifest, platform);
    issues.extend(manifest::validate_templates(&manifest, &templates));

    let package_hash = package_hash(&files);
    Ok(PackageContents {
        kind,
        files,
        manifest,
        templates,
        package_hash,
        issues,
    })
}

/// 目录包：递归读取普通文件，拒绝符号链接、越界路径与重复项。
fn read_directory(root: &Path) -> Result<BTreeMap<String, Vec<u8>>> {
    let mut files = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = fs::read_dir(&dir)?;
        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path)?;
            if metadata.file_type().is_symlink() {
                return Err(CoreError::PluginPackageInvalid {
                    reason: format!(
                        "包里不允许符号链接：{}",
                        path.strip_prefix(root).unwrap_or(&path).display()
                    ),
                });
            }
            if metadata.is_dir() {
                stack.push(path);
                continue;
            }
            if !metadata.is_file() {
                return Err(CoreError::PluginPackageInvalid {
                    reason: format!(
                        "包里只允许普通文件：{}",
                        path.strip_prefix(root).unwrap_or(&path).display()
                    ),
                });
            }
            if metadata.len() as usize > MAX_FILE_BYTES {
                return Err(CoreError::PluginPackageInvalid {
                    reason: format!(
                        "文件「{}」有 {} 字节，超过单文件上限 {MAX_FILE_BYTES}",
                        path.strip_prefix(root).unwrap_or(&path).display(),
                        metadata.len()
                    ),
                });
            }
            let relative = relative_path(root, &path)?;
            let bytes = fs::read(&path)?;
            insert_file(&mut files, relative, bytes)?;
        }
    }
    Ok(files)
}

/// zip 包：用 `enclosed_name` 挡住 `..`/绝对路径，并限制解压大小。
fn read_zip(path: &Path) -> Result<BTreeMap<String, Vec<u8>>> {
    let file = fs::File::open(path)?;
    let mut archive = zip::ZipArchive::new(file).map_err(|error| {
        CoreError::PluginPackageInvalid {
            reason: format!("不是可读的 zip：{error}"),
        }
    })?;
    let mut files = BTreeMap::new();
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).map_err(|error| {
            CoreError::PluginPackageInvalid {
                reason: format!("zip 第 {index} 项读不出来：{error}"),
            }
        })?;
        if entry.is_dir() {
            continue;
        }
        let name = entry.name().to_owned();
        if !manifest::valid_relative_path(&name) {
            return Err(CoreError::PluginPackageInvalid {
                reason: format!("zip 里有越界或不合法的路径「{name}」"),
            });
        }
        let declared = entry.size() as usize;
        if declared > MAX_FILE_BYTES {
            return Err(CoreError::PluginPackageInvalid {
                reason: format!("zip 里的「{name}」有 {declared} 字节，超过单文件上限"),
            });
        }
        let mut bytes = Vec::with_capacity(declared.min(MAX_FILE_BYTES));
        entry
            .by_ref()
            .take((MAX_FILE_BYTES + 1) as u64)
            .read_to_end(&mut bytes)?;
        if bytes.len() > MAX_FILE_BYTES {
            return Err(CoreError::PluginPackageInvalid {
                reason: format!("zip 里的「{name}」解压后超过单文件上限"),
            });
        }
        insert_file(&mut files, name, bytes)?;
    }
    Ok(files)
}

fn insert_file(
    files: &mut BTreeMap<String, Vec<u8>>,
    name: String,
    bytes: Vec<u8>,
) -> Result<()> {
    if !manifest::valid_relative_path(&name) {
        return Err(CoreError::PluginPackageInvalid {
            reason: format!("包里有不合法的路径「{name}」"),
        });
    }
    let total: usize = files.values().map(Vec::len).sum();
    if total.saturating_add(bytes.len()) > MAX_PACKAGE_BYTES {
        return Err(CoreError::PluginPackageInvalid {
            reason: format!("包解压后超过上限 {MAX_PACKAGE_BYTES} 字节"),
        });
    }
    if files.insert(name.clone(), bytes).is_some() {
        return Err(CoreError::PluginPackageInvalid {
            reason: format!("包里有重复路径「{name}」"),
        });
    }
    Ok(())
}

fn relative_path(root: &Path, path: &Path) -> Result<String> {
    let relative = path
        .strip_prefix(root)
        .map_err(|_| CoreError::PluginPackageInvalid {
            reason: format!("文件不在包根目录下：{}", path.display()),
        })?;
    let mut parts = Vec::new();
    for component in relative.components() {
        match component {
            Component::Normal(value) => parts.push(
                value
                    .to_str()
                    .ok_or_else(|| CoreError::PluginPackageInvalid {
                        reason: "包内路径必须是 UTF-8".to_owned(),
                    })?
                    .to_owned(),
            ),
            _ => {
                return Err(CoreError::PluginPackageInvalid {
                    reason: format!("包内路径不合法：{}", relative.display()),
                })
            }
        }
    }
    Ok(parts.join("/"))
}

/// 稳定包哈希：路径与文件哈希按字典序，加一个格式版本前缀。
fn package_hash(files: &BTreeMap<String, Vec<u8>>) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"unwritten-plugin-package-v1\0");
    for (name, bytes) in files {
        hasher.update(name.as_bytes());
        hasher.update([0]);
        hasher.update(sha256_hex(bytes).as_bytes());
        hasher.update([0]);
    }
    format!("{:x}", hasher.finalize())
}

pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

/// 把包内容写到 `plugins/<id>/<version>-<hash8>/`，返回相对路径。
///
/// 先写 `.staging/<uuid>`，成功后再改名；失败清理暂存，不留下半套插件。
pub(crate) fn write_install(root: &Path, contents: &PackageContents) -> Result<String> {
    let id = &contents.manifest.id;
    if !manifest::valid_id(id) {
        return Err(CoreError::PluginManifestInvalid {
            reason: format!("插件 id「{id}」不合法，不能落盘"),
        });
    }
    let version = &contents.manifest.version;
    if version.chars().any(|value| !(value.is_ascii_digit() || value == '.')) {
        return Err(CoreError::PluginManifestInvalid {
            reason: format!("插件版本「{version}」不能用于目录名"),
        });
    }
    let hash8: String = contents.package_hash.chars().take(8).collect();
    let relative = format!("plugins/{id}/{version}-{hash8}");
    let target = root.join(&relative);
    let staging = root
        .join("plugins")
        .join(".staging")
        .join(format!("{id}-{}", uuid::Uuid::now_v7().simple()));
    if let Err(error) = write_staging(&staging, contents) {
        let _ = fs::remove_dir_all(&staging);
        return Err(error);
    }
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }
    if target.exists() {
        fs::remove_dir_all(&target)?;
    }
    fs::rename(&staging, &target).map_err(|error| {
        let _ = fs::remove_dir_all(&staging);
        CoreError::Io(error)
    })?;
    Ok(relative)
}

fn write_staging(staging: &Path, contents: &PackageContents) -> Result<()> {
    fs::create_dir_all(staging)?;
    for (name, bytes) in &contents.files {
        let path = staging.join(name);
        if !path.starts_with(staging) {
            return Err(CoreError::PluginPackageInvalid {
                reason: format!("拒绝把文件写到暂存目录之外：{name}"),
            });
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&path, bytes)?;
    }
    Ok(())
}

/// 删掉这个插件的安装目录（卸载/升级后清理旧版本）。
pub(crate) fn remove_plugin_dir(root: &Path, plugin_id: &str) {
    if !manifest::valid_id(plugin_id) {
        return;
    }
    let _ = fs::remove_dir_all(root.join("plugins").join(plugin_id));
}

/// 升级成功后删掉除 `keep` 之外的旧版本目录。
pub(crate) fn remove_other_versions(root: &Path, plugin_id: &str, keep: &Path) {
    if !manifest::valid_id(plugin_id) {
        return;
    }
    let parent = root.join("plugins").join(plugin_id);
    let Ok(entries) = fs::read_dir(&parent) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path != keep {
            let _ = fs::remove_dir_all(path);
        }
    }
}

/// 读安装后的入口文件，并核对其哈希（防落盘后被替换）。
pub(crate) fn read_entry_verified(
    root: &Path,
    install_rel_path: &str,
    manifest: &PluginManifest,
) -> Result<Vec<u8>> {
    let relative = format!("{install_rel_path}/{}", manifest.entry_file);
    let path = root.join(&relative);
    let bytes = fs::read(&path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            CoreError::PluginPackageInvalid {
                reason: format!("插件的入口文件不在了：{relative}；请重新安装或卸载这个插件"),
            }
        } else {
            CoreError::Io(error)
        }
    })?;
    let expected = manifest
        .package_hashes
        .get(&manifest.entry_file)
        .ok_or_else(|| CoreError::PluginManifestInvalid {
            reason: "缓存清单里没有入口文件的哈希".to_owned(),
        })?;
    let expected = manifest::normalize_sha256(expected);
    let actual = sha256_hex(&bytes);
    if expected != actual {
        return Err(CoreError::PluginPackageInvalid {
            reason: format!(
                "入口文件「{}」的哈希与安装时不一致（可能被改动）；请重新安装插件",
                manifest.entry_file
            ),
        });
    }
    Ok(bytes)
}

/// 供测试/工具报告用：插件 API 版本。
pub fn api_version() -> &'static str {
    PLUGIN_API_VERSION
}

/// 读一个只读文件的前 `max` 字节（`asset_range` 用；调用方已确认路径来自 DB）。
pub(crate) fn read_file_range(path: &Path, start: u64, len: usize) -> Result<Vec<u8>> {
    let mut file = fs::File::open(path)?;
    file.seek(std::io::SeekFrom::Start(start))?;
    let mut buffer = Vec::with_capacity(len.min(MAX_FILE_BYTES));
    file.by_ref()
        .take((len as u64).min(MAX_FILE_BYTES as u64))
        .read_to_end(&mut buffer)?;
    Ok(buffer)
}

/// 把一个 `PathBuf` 规范成可展示的绝对路径字符串。
pub(crate) fn display_path(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_hash_is_order_independent_and_sensitive() {
        let mut a = BTreeMap::new();
        a.insert("a.txt".to_owned(), b"alpha".to_vec());
        a.insert("b.txt".to_owned(), b"beta".to_vec());
        let mut b = BTreeMap::new();
        b.insert("b.txt".to_owned(), b"beta".to_vec());
        b.insert("a.txt".to_owned(), b"alpha".to_vec());
        assert_eq!(package_hash(&a), package_hash(&b));
        b.insert("a.txt".to_owned(), b"alphA".to_vec());
        assert_ne!(package_hash(&a), package_hash(&b));
    }
}

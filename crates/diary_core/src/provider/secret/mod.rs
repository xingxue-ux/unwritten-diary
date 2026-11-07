//! 密钥托管：`SecretStore` 抽象与三个实现（Windows 凭据管理器 / 受限权限的独立文件 / 内存）。
//!
//! # 为什么单独一层
//!
//! 契约第 2.7 节要求「API 返回凭据存在与否，不返回密钥明文」，第 4.4 节要求
//! 「密钥不进配置表明文」。所以 `providers` 表里只放 `credential_ref`（引用），
//! 明文放在这张表之外。**抽象这一层的目的是让「放哪儿」可替换**：`providers`
//! 表与业务代码只认引用，换实现不用动表结构。
//!
//! # 各平台现在放哪儿（如实说明）
//!
//! - **Windows**：`WinCredSecretStore` 走系统凭据管理器（`CredWriteW` / `CredReadW` /
//!   `CredDeleteW`，`CRED_TYPE_GENERIC` + `CRED_PERSIST_LOCAL_MACHINE`），按当前
//!   Windows 用户隔离，密钥不再以明文文件躺在资料库旁边。打开资料库时先探测接口
//!   是否可用；被组策略禁用等情况下退回 `FileSecretStore`，原因写进 `describe()`，
//!   不假装成功。旧版本留在 `secrets/` 的明文文件会在首次打开时迁进凭据管理器，
//!   **全部写成功之后才删旧文件**；写失败就保留旧文件并回退。
//! - **Android / Linux / macOS 等其他平台**：仍然只有 `FileSecretStore`。Android 上
//!   **没有**接 Android Keystore，密钥就是资料库目录下 `secrets/` 里的明文文件
//!   （Unix 上目录 0700、文件 0600）。同用户身份的本地进程能读到，这一条限制没有
//!   因为 Windows 这一半做完而消失。
//! - **内存库**：`MemorySecretStore`，不落盘。
//!
//! # 文件实现仍然保留的限制
//!
//! `FileSecretStore` 在 Windows 上只用于回退与临时资料库（见 `WinCredSecretStore`），
//! 在其他平台仍是默认实现。它**不是**系统钥匙串：
//!
//! - Windows 上 `set_permissions` 只映射只读位，没有 ACL 语义——文件权限实际由
//!   所在目录的继承 ACL 决定，这一层不做也不假装做加密；
//! - 拿到这个目录的本地进程（同用户身份）就能读到明文；系统钥匙串能给的是
//!   「按用户/应用隔离 + 由 OS 保管」，文件实现没有；
//! - 密钥与资料库同目录，**备份实现必须显式排除 `secrets/`**（契约第 8 节：
//!   API 密钥不进入备份）。这一片没有备份入口，所以只是约束，不是已实现的保证。
//!
//! 这些限制同样写在 `docs/architecture/m3-Provider-配置与能力.md`，不在代码里
//! 假装已经全平台用了系统钥匙串。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::error::{CoreError, Result};
use crate::support;

#[cfg(windows)]
mod wincred;

#[cfg(windows)]
pub use wincred::WinCredSecretStore;

/// 凭据引用允许的字符集。引用会当文件名用，所以先把它限死，杜绝 `../` 这类路径。
fn valid_ref(credential_ref: &str) -> bool {
    !credential_ref.is_empty()
        && credential_ref.len() <= 64
        && credential_ref
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}

/// 密钥托管。**实现方不得把明文写日志、写错误消息、写进返回给上层的对象。**
pub trait SecretStore: Send + Sync {
    /// 存一个密钥，返回它的引用。引用是不透明字符串，只会出现在 `providers.credential_ref`。
    fn put(&self, label: &str, secret: &str) -> Result<String>;

    /// 读密钥明文。**只有发起调用的受控路径**可以拿它去拼请求头。
    fn get(&self, credential_ref: &str) -> Result<Option<String>>;

    /// 删除。删不存在的引用不算错（幂等）。
    fn delete(&self, credential_ref: &str) -> Result<()>;

    /// 引用是否存在（脱敏判断，不取明文）。
    fn contains(&self, credential_ref: &str) -> Result<bool> {
        Ok(self.get(credential_ref)?.is_some())
    }

    /// 这里放的是什么——只用于文档与排查，不含任何密钥内容。
    ///
    /// 返回 `String` 而不是 `&'static str`：回退原因（例如「凭据接口被策略禁用」）
    /// 是运行时才知道的，不能为了一个静态字符串把它藏起来。
    fn describe(&self) -> String;
}

/// 按平台与资料库形态选一个实现。
///
/// 内存库没有落盘位置，只在内存里；Windows 文件库优先系统凭据管理器
/// （`WinCredSecretStore` 自己处理探测、旧文件迁移与回退）；其他平台文件库用
/// 受限权限的独立文件。选择逻辑集中在这里，`Core::open` 不重复平台判断。
pub(crate) fn for_library(root: Option<&Path>) -> Box<dyn SecretStore> {
    match root {
        None => Box::new(MemorySecretStore::new()),
        Some(dir) => {
            #[cfg(windows)]
            {
                Box::new(wincred::WinCredSecretStore::open(dir))
            }
            #[cfg(not(windows))]
            {
                Box::new(FileSecretStore::beside_library(dir))
            }
        }
    }
}

/// 一个引用对应一个文件的密钥存储，根目录是资料库目录下的 `secrets/`。
pub struct FileSecretStore {
    root: PathBuf,
}

impl FileSecretStore {
    /// 在给定目录下建（或复用）密钥存储。
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// 资料库目录 → 密钥目录。与 `providers` 表所在的库文件**分开**。
    pub fn beside_library(library_root: &Path) -> Self {
        Self::new(library_root.join("secrets"))
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn path_for(&self, credential_ref: &str) -> Result<PathBuf> {
        if !valid_ref(credential_ref) {
            return Err(CoreError::InvalidArgument {
                reason: format!("凭据引用不合法：{credential_ref}"),
            });
        }
        Ok(self.root.join(format!("{credential_ref}.secret")))
    }

    fn prepare_dir(&self) -> Result<()> {
        std::fs::create_dir_all(&self.root)?;
        restrict_dir(&self.root);
        Ok(())
    }

    /// 列出 `secrets/` 下看起来是凭据的文件引用（迁移用）。
    ///
    /// 只认 `<合法引用>.secret`：`.tmp` 是上次写到一半的残留，不是密钥；名字不合法的
    /// 文件不是本实现写的，不能猜它的含义。目录不存在就是没有旧密钥，不是错误。
    #[cfg(windows)]
    pub(crate) fn existing_refs(&self) -> Result<Vec<String>> {
        let entries = match std::fs::read_dir(&self.root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        };
        let mut refs = Vec::new();
        for entry in entries {
            let entry = entry?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            let Some(stem) = name.strip_suffix(".secret") else {
                continue;
            };
            if valid_ref(stem) {
                refs.push(stem.to_owned());
            }
        }
        // 固定顺序：迁移的写入顺序可复现，测试与排障不用猜。
        refs.sort();
        Ok(refs)
    }
}

impl SecretStore for FileSecretStore {
    fn put(&self, label: &str, secret: &str) -> Result<String> {
        self.prepare_dir()?;
        let credential_ref = support::new_id("sec");
        let path = self.path_for(&credential_ref)?;
        // 先写临时文件再改名：中途失败不会留下半个密钥文件。
        let temp = self.root.join(format!("{credential_ref}.tmp"));
        std::fs::write(&temp, secret.as_bytes())?;
        restrict_file(&temp);
        std::fs::rename(&temp, &path)?;
        // label 只进日志排查侧（这里没有日志实现），不落进任何对象。
        let _ = label;
        Ok(credential_ref)
    }

    fn get(&self, credential_ref: &str) -> Result<Option<String>> {
        let path = self.path_for(credential_ref)?;
        match std::fs::read(&path) {
            Ok(bytes) => Ok(Some(String::from_utf8(bytes).map_err(|_| {
                CoreError::CorruptedData {
                    message: "凭据文件不是合法的 UTF-8".to_owned(),
                }
            })?)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    fn delete(&self, credential_ref: &str) -> Result<()> {
        let path = self.path_for(credential_ref)?;
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    fn describe(&self) -> String {
        "受限权限的独立文件（Unix 0600；非 Unix 只继承目录权限，见模块文档）".to_owned()
    }
}

/// 内存实现：给内存库（测试）与需要临时隔离的场景用，不落盘。
#[derive(Default)]
pub struct MemorySecretStore {
    entries: Mutex<HashMap<String, String>>,
}

impl MemorySecretStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl SecretStore for MemorySecretStore {
    fn put(&self, label: &str, secret: &str) -> Result<String> {
        let credential_ref = support::new_id("sec");
        let mut entries = self.entries.lock().expect("密钥存储的锁不该中毒");
        entries.insert(credential_ref.clone(), secret.to_owned());
        let _ = label;
        Ok(credential_ref)
    }

    fn get(&self, credential_ref: &str) -> Result<Option<String>> {
        let entries = self.entries.lock().expect("密钥存储的锁不该中毒");
        Ok(entries.get(credential_ref).cloned())
    }

    fn delete(&self, credential_ref: &str) -> Result<()> {
        let mut entries = self.entries.lock().expect("密钥存储的锁不该中毒");
        entries.remove(credential_ref);
        Ok(())
    }

    fn describe(&self) -> String {
        "内存（不落盘，仅测试与内存库）".to_owned()
    }
}

#[cfg(unix)]
fn restrict_file(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}

/// 非 Unix（Windows）：如实什么都不做。
///
/// 不是忘了写——Windows 上文件权限是 ACL，`set_permissions` 只能表达只读位。
/// 正式资料库已经走 `WinCredSecretStore`；这里只剩「凭据管理器不可用时的回退」
/// 与「临时资料库」两条路，文件权限仍由所在目录的继承 ACL 决定。
#[cfg(not(unix))]
fn restrict_file(_path: &Path) {}

#[cfg(unix)]
fn restrict_dir(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700));
}

#[cfg(not(unix))]
fn restrict_dir(_path: &Path) {}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_store_round_trips_and_deletes_idempotently() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileSecretStore::beside_library(dir.path());
        let reference = store.put("测试", "sk-file-secret-0123456789").unwrap();
        assert!(reference.starts_with("sec_"));
        assert_eq!(
            store.get(&reference).unwrap().as_deref(),
            Some("sk-file-secret-0123456789")
        );
        assert!(store.contains(&reference).unwrap());
        store.delete(&reference).unwrap();
        assert_eq!(store.get(&reference).unwrap(), None);
        // 删不存在的引用不算错（幂等）。
        store.delete(&reference).unwrap();
    }

    #[test]
    fn file_store_rejects_references_that_would_escape_the_directory() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileSecretStore::beside_library(dir.path());
        for bad in ["../outside", "a/b", "", "sec with space"] {
            let error = store.get(bad).unwrap_err();
            assert_eq!(
                error.code(),
                crate::error::ErrorCode::InvalidArgument,
                "引用「{bad}」本该被拒绝"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn file_store_restricts_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let store = FileSecretStore::beside_library(dir.path());
        let reference = store.put("测试", "sk-perm-0123456789").unwrap();
        let file_mode = std::fs::metadata(store.root().join(format!("{reference}.secret")))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(file_mode, 0o600);
        let dir_mode = std::fs::metadata(store.root())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(dir_mode, 0o700);
    }

    #[test]
    fn memory_store_is_isolated_per_instance() {
        let first = MemorySecretStore::new();
        let second = MemorySecretStore::new();
        let reference = first.put("测试", "sk-mem-0123456789").unwrap();
        assert!(second.get(&reference).unwrap().is_none());
        assert!(second.describe().contains("内存"));
        assert!(FileSecretStore::new("/tmp/whatever")
            .describe()
            .contains("0600"));
    }
}

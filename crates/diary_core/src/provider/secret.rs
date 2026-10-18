//! 密钥托管：`SecretStore` 抽象与两个实现（受限权限的独立文件 / 内存）。
//!
//! # 为什么单独一层
//!
//! 契约第 2.7 节要求「API 返回凭据存在与否，不返回密钥明文」，第 4.4 节要求
//! 「密钥不进配置表明文」。所以 `providers` 表里只放 `credential_ref`（引用），
//! 明文放在这张表之外。**抽象这一层的目的是让「放哪儿」可替换**：这一片的默认
//! 实现是一个权限受限的独立文件，而契约最终要的是平台层的系统凭据接口
//! （`PlatformHost.storeCredential` / `readCredential`，Windows DPAPI / Android
//! Keystore）。等宿主接口接上，换一个新的 `SecretStore` 实现即可，`providers` 表
//! 里的引用形状不用动。
//!
//! # 默认实现是妥协（如实说明）
//!
//! `FileSecretStore` 把密钥写进资料库目录下的 `secrets/`（Unix 上目录 0700、
//! 文件 0600）。它**不是**系统钥匙串：
//!
//! - Windows 上 `set_permissions` 只映射只读位，没有 ACL 语义——文件权限实际由
//!   所在目录的继承 ACL 决定，这一层不做也不假装做加密；
//! - 拿到这个目录的本地进程（同用户身份）就能读到明文；系统钥匙串能给的是
//!   「按用户/应用隔离 + 由 OS 保管」，这里没有；
//! - 密钥与资料库同目录，**备份实现必须显式排除 `secrets/`**（契约第 8 节：
//!   API 密钥不进入备份）。这一片没有备份入口，所以只是约束，不是已实现的保证。
//!
//! 这三条同样写在 `docs/architecture/m3-Provider-配置与能力.md`，不在代码里
//! 假装已经用了系统钥匙串。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::error::{CoreError, Result};
use crate::support;

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
    fn describe(&self) -> &'static str;
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

    fn describe(&self) -> &'static str {
        "受限权限的独立文件（Unix 0600；非 Unix 只继承目录权限，见模块文档）"
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

    fn describe(&self) -> &'static str {
        "内存（不落盘，仅测试与内存库）"
    }
}

#[cfg(unix)]
fn restrict_file(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}

/// 非 Unix（Windows）：如实什么都不做。
///
/// 不是忘了写——Windows 上文件权限是 ACL，`set_permissions` 只能表达只读位；
/// 要做对得调 DPAPI 或 ACL API，那是「后续接系统凭据」那一步的事（见模块文档）。
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

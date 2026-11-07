//! Windows 系统凭据托管：`CredWriteW` / `CredReadW` / `CredDeleteW`。
//!
//! # 为什么是凭据管理器而不是 DPAPI 加密文件
//!
//! 契约第 5 节要的是「平台层保管凭据」。Credential Manager 的 `CRED_TYPE_GENERIC`
//! 正好是「按当前用户隔离、由系统保管一段不透明字节」：密钥不再以明文文件躺在资料库
//! 旁边，也不随资料库目录被复制/备份，静态存储由 OS 负责。DPAPI 只做加密，仍然要我们
//! 自己管密文文件与权限；这里没有理由多绕一层。
//!
//! 它**不**防同用户身份的恶意进程：知道 TargetName 的进程仍可调 `CredReadW` 读出来。
//! 与文件实现的差别是「不再是一份谁都能直接读的明文文件」和「由 OS 保管」，不是
//! 「同用户进程读不到」——不要把这句写成后者。
//!
//! # 引用与 TargetName
//!
//! `providers.credential_ref` 是引用，不是密钥。TargetName 用固定前缀
//! `unwritten-diary/cred/` + 引用，用户在「控制面板 → 凭据管理器 → Windows 凭据」
//! 里能一眼认出是我们的。引用会进 TargetName，所以沿用 `secret::valid_ref` 的字符集
//! 限制（字母数字与下划线、长度 ≤ 64），不允许 `/`、`..` 之类的东西。
//!
//! # 回退
//!
//! 打开资料库时先做一次「写 → 读回 → 删」探测。任何一步失败（例如组策略禁用凭据
//! 管理器）都退回 `FileSecretStore`，并把原因写进 `describe()`；不静默假装成功，
//! 也不因为凭据管理器坏了就让人完全无法配模型。临时目录里的资料库不写系统凭据：
//! 测试与导入演练会大量创建临时库，若也写凭据管理器，随机引用会永久留在用户的
//! 「Windows 凭据」里；临时库的密钥跟着临时目录走更符合事实。

use std::path::{Path, PathBuf};

use windows_sys::Win32::Foundation::{GetLastError, ERROR_NOT_FOUND, FILETIME};
use windows_sys::Win32::Security::Credentials::{
    CredDeleteW, CredFree, CredReadW, CredWriteW, CREDENTIALW, CRED_MAX_CREDENTIAL_BLOB_SIZE,
    CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC,
};

use super::{valid_ref, FileSecretStore, SecretStore};
use crate::error::{CoreError, Result};
use crate::support;

/// 普通凭据的 TargetName 前缀。固定前缀让用户在凭据管理器里认得出是我们。
const TARGET_PREFIX: &str = "unwritten-diary/cred/";

/// Windows 上的密钥托管：优先系统凭据管理器，不可用时退回资料库旁的独立文件。
pub struct WinCredSecretStore {
    backend: Backend,
}

enum Backend {
    Cred {
        store: CredBackend,
        /// 迁移后仍有旧明文文件删不掉时的说明；没有就 `None`。
        note: Option<String>,
    },
    File {
        store: FileSecretStore,
        /// 为什么没用凭据管理器。`describe()` 必须如实说出来。
        reason: String,
    },
    /// 凭据管理器可用，但旧文件迁移没做完：两边都可能还有密钥。
    ///
    /// 这时不能简单退回文件存储——上一次迁移可能已经删掉了一部分旧文件，那些密钥
    /// 只在凭据管理器里；也不能只用凭据管理器——这次没迁成功的密钥还在文件里。
    Mixed(MixedSecretStore),
}

impl WinCredSecretStore {
    /// 打开（或创建）一个资料库对应的密钥托管。
    ///
    /// 这个函数不返回 `Result`：凭据管理器不可用不是「打不开资料库」，而是「这一层
    /// 换一种存法」。回退原因记在 `describe()` 里，调用方照常能保存/读取密钥。
    pub fn open(library_root: &Path) -> Self {
        let legacy = FileSecretStore::beside_library(library_root);
        if is_ephemeral_library(library_root) {
            return Self::file_backend(
                legacy,
                "资料库在系统临时目录中，临时资料库不写系统凭据".to_owned(),
            );
        }
        let cred = match CredBackend::probe() {
            Ok(cred) => cred,
            Err(error) => {
                return Self::file_backend(legacy, format!("系统凭据接口不可用：{error}"));
            }
        };
        match migrate_legacy_files(&legacy, &cred) {
            Ok(report) => Self {
                backend: Backend::Cred {
                    store: cred,
                    note: report.note(),
                },
            },
            Err(error) => Self {
                backend: Backend::Mixed(MixedSecretStore {
                    cred,
                    file: legacy,
                    reason: format!("旧密钥迁移失败：{error}"),
                }),
            },
        }
    }

    fn file_backend(store: FileSecretStore, reason: String) -> Self {
        Self {
            backend: Backend::File { store, reason },
        }
    }

    fn store(&self) -> &dyn SecretStore {
        match &self.backend {
            Backend::Cred { store, .. } => store,
            Backend::File { store, .. } => store,
            Backend::Mixed(store) => store,
        }
    }

    #[cfg(test)]
    fn fallback_for_test(library_root: &Path, reason: &str) -> Self {
        Self::file_backend(
            FileSecretStore::beside_library(library_root),
            reason.to_owned(),
        )
    }
}

impl SecretStore for WinCredSecretStore {
    fn put(&self, label: &str, secret: &str) -> Result<String> {
        self.store().put(label, secret)
    }

    fn get(&self, credential_ref: &str) -> Result<Option<String>> {
        self.store().get(credential_ref)
    }

    fn delete(&self, credential_ref: &str) -> Result<()> {
        self.store().delete(credential_ref)
    }

    fn describe(&self) -> String {
        match &self.backend {
            Backend::Cred {
                note: Some(note), ..
            } => {
                format!("Windows 凭据管理器（CredWriteW，按用户隔离；{note}）")
            }
            Backend::Cred { note: None, .. } => {
                "Windows 凭据管理器（CredWriteW，按用户隔离）".to_owned()
            }
            Backend::File { reason, .. } => {
                format!("受限权限的独立文件（Windows 凭据管理器未启用：{reason}）")
            }
            Backend::Mixed(store) => store.describe(),
        }
    }
}

/// 迁移没做完时的两层存储：读先查凭据管理器、再查旧文件；写优先凭据管理器。
///
/// 为什么需要它：迁移的删除阶段可能已经删掉一部分旧文件（那些密钥只在凭据管理器里），
/// 而这次没迁成功的密钥还在文件里。只退回文件或只用凭据管理器都会让其中一部分读不到。
struct MixedSecretStore {
    cred: CredBackend,
    file: FileSecretStore,
    reason: String,
}

impl SecretStore for MixedSecretStore {
    fn put(&self, label: &str, secret: &str) -> Result<String> {
        // 凭据管理器刚探测可用，正常情况下写这里；真写失败时退回文件，至少还能配模型。
        match self.cred.put(label, secret) {
            Ok(reference) => Ok(reference),
            Err(_) => self.file.put(label, secret),
        }
    }

    fn get(&self, credential_ref: &str) -> Result<Option<String>> {
        match self.cred.get(credential_ref) {
            Ok(Some(secret)) => Ok(Some(secret)),
            Ok(None) => self.file.get(credential_ref),
            // 凭据管理器读失败时，旧文件里可能还有这条密钥；先给文件一次机会，
            // 两边都没有才把凭据管理器的错误报出去。
            Err(cred_error) => match self.file.get(credential_ref) {
                Ok(Some(secret)) => Ok(Some(secret)),
                Ok(None) => Err(cred_error),
                Err(file_error) => Err(file_error),
            },
        }
    }

    fn delete(&self, credential_ref: &str) -> Result<()> {
        // 两边都删；任何一边失败都如实报错，不假装删干净了。
        let cred_result = self.cred.delete(credential_ref);
        let file_result = self.file.delete(credential_ref);
        cred_result.and(file_result)
    }

    fn describe(&self) -> String {
        format!(
            "Windows 凭据管理器 + 旧明文文件（迁移未完成：{}）",
            self.reason
        )
    }
}

/// 迁移旧明文文件需要的写入端。
///
/// 为什么不直接给 `SecretStore` 加一个 `put_at`：普通保存只生成新引用，而迁移必须
/// 沿用 `providers.credential_ref` 里已有的旧引用，否则表里的引用会全部指空。这个
/// 能力只有迁移用得上，单独一个 trait 比把主 trait 撑大更干净。
trait SecretImporter {
    fn put_at(&self, credential_ref: &str, secret: &str) -> Result<()>;
    /// 回滚一条已经写进去的凭据。名字和 `SecretStore::delete` 分开，避免同一个类型上
    /// 两个同名方法让调用点必须写全限定名。
    fn remove_imported(&self, credential_ref: &str) -> Result<()>;
}

#[derive(Debug)]
struct MigrationReport {
    /// 已经迁进新存储、但旧明文文件删不掉的引用。下次打开会再试。
    undeleted: Vec<String>,
}

impl MigrationReport {
    fn note(&self) -> Option<String> {
        if self.undeleted.is_empty() {
            None
        } else {
            Some(format!(
                "有 {} 个旧明文文件未能删除，下次打开会重试",
                self.undeleted.len()
            ))
        }
    }
}

/// 把资料库 `secrets/` 下的旧明文迁进 `target`。
///
/// 顺序是刻意的：**先把所有明文读出来，再全部写进新存储，最后才删旧文件**。
/// 任何一次写入失败都会回滚已经写进去的凭据并保留全部旧文件——迁移失败绝不能
/// 变成密钥丢失。删旧文件失败不回滚：新存储里已经有全部密钥，留着旧文件只是多一份
/// 明文，下次打开会重试删除；此时回退到文件存储反而会让「已经删掉的那几条」读不到。
fn migrate_legacy_files(
    legacy: &FileSecretStore,
    target: &dyn SecretImporter,
) -> Result<MigrationReport> {
    let refs = legacy.existing_refs()?;
    if refs.is_empty() {
        return Ok(MigrationReport {
            undeleted: Vec::new(),
        });
    }

    let mut pending = Vec::with_capacity(refs.len());
    for reference in refs {
        // 读目录与读文件之间被删掉：没有可迁的，跳过。
        if let Some(secret) = legacy.get(&reference)? {
            pending.push((reference, secret));
        }
    }

    let mut written: Vec<String> = Vec::with_capacity(pending.len());
    for (reference, secret) in &pending {
        if let Err(error) = target.put_at(reference, secret) {
            for done in written.drain(..) {
                // 回滚失败也只能继续：旧文件还在，至少不会丢密钥。
                let _ = target.remove_imported(&done);
            }
            return Err(error);
        }
        written.push(reference.clone());
    }

    let mut undeleted = Vec::new();
    for (reference, _) in &pending {
        if legacy.delete(reference).is_err() {
            undeleted.push(reference.clone());
        }
    }
    Ok(MigrationReport { undeleted })
}

/// 真正的 Credential Manager 后端。`probe` 通过之后才构造。
struct CredBackend;

impl CredBackend {
    /// 写一条探测凭据、读回来、再删掉。
    ///
    /// 只测 `CredWriteW` 不够：删除与读取也可能被策略挡住，而 `delete` 是移除服务
    /// 配置时必须走的路。三步都成功才认为这个后端可用。
    fn probe() -> Result<Self> {
        let backend = Self;
        let reference = support::new_id("probe");
        backend.put_at(&reference, "probe")?;
        let read_back = backend.get(&reference);
        let deleted = backend.delete(&reference);
        let read_back = read_back?;
        if read_back.as_deref() != Some("probe") {
            return Err(CoreError::SecretStoreUnavailable {
                reason: "Windows 凭据管理器读回的内容与写入不一致".to_owned(),
            });
        }
        deleted?;
        Ok(backend)
    }
}

impl SecretStore for CredBackend {
    fn put(&self, label: &str, secret: &str) -> Result<String> {
        let credential_ref = support::new_id("sec");
        self.put_at(&credential_ref, secret)?;
        // label 只用于排查侧；凭据管理器里不存它，避免把服务名和密钥绑在一起。
        let _ = label;
        Ok(credential_ref)
    }

    fn get(&self, credential_ref: &str) -> Result<Option<String>> {
        let target = target_name(TARGET_PREFIX, credential_ref)?;
        let mut raw: *mut CREDENTIALW = std::ptr::null_mut();
        // SAFETY: target 是有效的 NUL 结尾 UTF-16；raw 由 CredReadW 成功时写入，
        // 失败时保持空指针。第四个参数是合法的可写指针。
        let read = unsafe { CredReadW(target.as_ptr(), CRED_TYPE_GENERIC, 0, &mut raw) };
        if read == 0 {
            // SAFETY: GetLastError 没有前置条件。
            let code = unsafe { GetLastError() };
            if code == ERROR_NOT_FOUND {
                return Ok(None);
            }
            return Err(credential_error_code("读取 Windows 凭据管理器", code));
        }
        if raw.is_null() {
            return Err(CoreError::SecretStoreUnavailable {
                reason: "Windows 凭据管理器返回了空指针".to_owned(),
            });
        }

        // SAFETY: CredReadW 成功时 raw 指向一个由 CredFree 释放的 CREDENTIALW。
        let credential = unsafe { &*raw };
        let size = credential.CredentialBlobSize as usize;
        let blob_ptr = credential.CredentialBlob;
        if size > CRED_MAX_CREDENTIAL_BLOB_SIZE as usize {
            // SAFETY: raw 来自 CredReadW，必须释放一次。
            unsafe { CredFree(raw.cast()) };
            return Err(CoreError::CorruptedData {
                message: format!(
                    "Windows 凭据管理器返回的凭据块大小 {size} 超过上限 {}",
                    CRED_MAX_CREDENTIAL_BLOB_SIZE
                ),
            });
        }
        if size > 0 && blob_ptr.is_null() {
            // SAFETY: raw 来自 CredReadW，必须释放一次。
            unsafe { CredFree(raw.cast()) };
            return Err(CoreError::SecretStoreUnavailable {
                reason: "Windows 凭据管理器返回的凭据块为空".to_owned(),
            });
        }

        let decoded = if size == 0 {
            Ok(String::new())
        } else {
            // SAFETY: 按 API 契约，blob_ptr 指向 size 个可读字节。
            let bytes = unsafe { std::slice::from_raw_parts(blob_ptr, size) };
            std::str::from_utf8(bytes).map(str::to_owned)
        };
        if size > 0 {
            // SAFETY: 同一块内存可写；复制完明文就擦掉，别把它留在 API 缓冲区里。
            unsafe { std::ptr::write_bytes(blob_ptr, 0, size) };
        }
        // SAFETY: raw 来自 CredReadW，必须用 CredFree 释放一次。
        unsafe { CredFree(raw.cast()) };

        match decoded {
            Ok(value) => Ok(Some(value)),
            Err(_) => Err(CoreError::CorruptedData {
                message: "Windows 凭据管理器里的密钥不是合法的 UTF-8".to_owned(),
            }),
        }
    }

    fn delete(&self, credential_ref: &str) -> Result<()> {
        let target = target_name(TARGET_PREFIX, credential_ref)?;
        // SAFETY: target 是有效的 NUL 结尾 UTF-16。
        let deleted = unsafe { CredDeleteW(target.as_ptr(), CRED_TYPE_GENERIC, 0) };
        if deleted == 0 {
            // SAFETY: GetLastError 没有前置条件。
            let code = unsafe { GetLastError() };
            if code == ERROR_NOT_FOUND {
                return Ok(());
            }
            return Err(credential_error_code("删除 Windows 凭据", code));
        }
        Ok(())
    }

    fn describe(&self) -> String {
        "Windows 凭据管理器（CredWriteW，按用户隔离）".to_owned()
    }
}

impl SecretImporter for CredBackend {
    fn put_at(&self, credential_ref: &str, secret: &str) -> Result<()> {
        let mut target = target_name(TARGET_PREFIX, credential_ref)?;
        let mut blob = secret.as_bytes().to_vec();
        if blob.len() > CRED_MAX_CREDENTIAL_BLOB_SIZE as usize {
            let size = blob.len();
            zero_blob(&mut blob);
            return Err(CoreError::InvalidArgument {
                reason: format!(
                    "密钥太长：{size} 字节，Windows 凭据管理器的上限是 {} 字节",
                    CRED_MAX_CREDENTIAL_BLOB_SIZE
                ),
            });
        }
        let credential = CREDENTIALW {
            Flags: 0,
            Type: CRED_TYPE_GENERIC,
            TargetName: target.as_mut_ptr(),
            Comment: std::ptr::null_mut(),
            LastWritten: FILETIME {
                dwLowDateTime: 0,
                dwHighDateTime: 0,
            },
            CredentialBlobSize: blob.len() as u32,
            CredentialBlob: if blob.is_empty() {
                std::ptr::null_mut()
            } else {
                blob.as_mut_ptr()
            },
            Persist: CRED_PERSIST_LOCAL_MACHINE,
            AttributeCount: 0,
            Attributes: std::ptr::null_mut(),
            TargetAlias: std::ptr::null_mut(),
            UserName: std::ptr::null_mut(),
        };
        // SAFETY: 结构里的指针都指向本次调用期间有效的缓冲区；blob 长度已按 API
        // 上限检查，TargetName 是 NUL 结尾的 UTF-16。
        let written = unsafe { CredWriteW(&credential, 0) };
        zero_blob(&mut blob);
        if written == 0 {
            // SAFETY: GetLastError 没有前置条件。
            let code = unsafe { GetLastError() };
            return Err(credential_error_code("写入 Windows 凭据管理器", code));
        }
        Ok(())
    }

    fn remove_imported(&self, credential_ref: &str) -> Result<()> {
        SecretStore::delete(self, credential_ref)
    }
}

fn target_name(prefix: &str, credential_ref: &str) -> Result<Vec<u16>> {
    if !valid_ref(credential_ref) {
        return Err(CoreError::InvalidArgument {
            reason: format!("凭据引用不合法：{credential_ref}"),
        });
    }
    Ok(wide(&format!("{prefix}{credential_ref}")))
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

fn zero_blob(blob: &mut [u8]) {
    blob.fill(0);
    // 防止优化器把「马上就被释放的缓冲区」的擦除整段删掉。
    std::hint::black_box(blob);
}

fn credential_error_code(operation: &str, code: u32) -> CoreError {
    let os_error = std::io::Error::from_raw_os_error(code as i32);
    CoreError::SecretStoreUnavailable {
        reason: format!("{operation}失败（Windows 错误码 {code}：{os_error}）"),
    }
}

/// 临时目录里的资料库不写系统凭据。
///
/// 测试与导入演练会大量创建临时资料库；如果它们也往凭据管理器里写，随机引用会永久
/// 留在用户的「Windows 凭据」里。临时目录本身是会被清理的，密钥跟着文件走更符合
/// 「这份资料库是临时的」这一事实。正式资料库（文档目录等）不受影响。
fn is_ephemeral_library(root: &Path) -> bool {
    let Some(root) = normalized_path(root) else {
        return false;
    };
    let Some(temp) = normalized_path(&std::env::temp_dir()) else {
        return false;
    };
    root.starts_with(temp)
}

fn normalized_path(path: &Path) -> Option<PathBuf> {
    std::fs::canonicalize(path)
        .ok()
        .or_else(|| std::path::absolute(path).ok())
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::error::ErrorCode;
    use crate::provider::secret::SecretStore;

    /// 测试用完把凭据删干净；断言失败展开栈时也会走到这里。
    struct CredCleanup<'a> {
        store: &'a CredBackend,
        refs: Vec<String>,
    }

    impl Drop for CredCleanup<'_> {
        fn drop(&mut self) {
            for reference in &self.refs {
                let _ = self.store.delete(reference);
            }
        }
    }

    fn cred() -> CredBackend {
        CredBackend::probe().expect("Windows 凭据管理器应可用")
    }

    #[test]
    fn cred_round_trips_and_deletes_idempotently() {
        let store = cred();
        let reference = store.put("测试", "sk-wincred-0123456789").unwrap();
        let _cleanup = CredCleanup {
            store: &store,
            refs: vec![reference.clone()],
        };
        assert!(reference.starts_with("sec_"));
        assert_eq!(
            store.get(&reference).unwrap().as_deref(),
            Some("sk-wincred-0123456789")
        );
        assert!(store.contains(&reference).unwrap());
        store.delete(&reference).unwrap();
        assert_eq!(store.get(&reference).unwrap(), None);
        // 删不存在的引用不算错（幂等）。
        store.delete(&reference).unwrap();
    }

    #[test]
    fn cred_overwrites_the_same_reference() {
        let store = cred();
        let reference = support::new_id("test");
        let _cleanup = CredCleanup {
            store: &store,
            refs: vec![reference.clone()],
        };
        store.put_at(&reference, "first-secret").unwrap();
        store.put_at(&reference, "second-secret").unwrap();
        assert_eq!(
            store.get(&reference).unwrap().as_deref(),
            Some("second-secret")
        );
    }

    #[test]
    fn cred_rejects_invalid_and_too_long_references() {
        let store = cred();
        let too_long = "x".repeat(65);
        for bad in ["../outside", "a/b", "", "sec with space", too_long.as_str()] {
            let error = store.get(bad).unwrap_err();
            assert_eq!(
                error.code(),
                ErrorCode::InvalidArgument,
                "引用「{bad}」本该被拒绝"
            );
            let error = store.delete(bad).unwrap_err();
            assert_eq!(
                error.code(),
                ErrorCode::InvalidArgument,
                "引用「{bad}」本该被拒绝"
            );
            let error = store.put_at(bad, "secret").unwrap_err();
            assert_eq!(
                error.code(),
                ErrorCode::InvalidArgument,
                "引用「{bad}」本该被拒绝"
            );
        }
    }

    #[test]
    fn cred_rejects_secrets_larger_than_the_blob_limit() {
        let store = cred();
        let reference = support::new_id("test");
        let too_long = "x".repeat(CRED_MAX_CREDENTIAL_BLOB_SIZE as usize + 1);
        let error = store.put_at(&reference, &too_long).unwrap_err();
        assert_eq!(error.code(), ErrorCode::InvalidArgument);
        assert_eq!(store.get(&reference).unwrap(), None);
    }

    #[test]
    fn migration_moves_legacy_files_and_deletes_them() {
        let dir = tempfile::tempdir().unwrap();
        let legacy = FileSecretStore::beside_library(dir.path());
        let reference = legacy.put("测试", "sk-legacy-0123456789").unwrap();
        let store = cred();
        let _cleanup = CredCleanup {
            store: &store,
            refs: vec![reference.clone()],
        };

        let report = migrate_legacy_files(&legacy, &store).unwrap();
        assert!(report.undeleted.is_empty());
        assert_eq!(
            store.get(&reference).unwrap().as_deref(),
            Some("sk-legacy-0123456789")
        );
        assert_eq!(legacy.get(&reference).unwrap(), None);

        // 幂等：再迁一次不报错，也不把已经迁走的密钥弄丢。
        let report = migrate_legacy_files(&legacy, &store).unwrap();
        assert!(report.undeleted.is_empty());
        assert_eq!(
            store.get(&reference).unwrap().as_deref(),
            Some("sk-legacy-0123456789")
        );
    }

    #[test]
    fn migration_keeps_legacy_files_when_writing_fails() {
        let dir = tempfile::tempdir().unwrap();
        let legacy = FileSecretStore::beside_library(dir.path());
        let reference = legacy.put("测试", "sk-legacy-keep").unwrap();

        let error = migrate_legacy_files(&legacy, &FailingImporter).unwrap_err();
        assert!(matches!(error, CoreError::SecretStoreUnavailable { .. }));
        assert_eq!(
            legacy.get(&reference).unwrap().as_deref(),
            Some("sk-legacy-keep")
        );
    }

    #[test]
    fn migration_rolls_back_credentials_written_before_a_failure() {
        let dir = tempfile::tempdir().unwrap();
        let legacy = FileSecretStore::beside_library(dir.path());
        std::fs::create_dir_all(legacy.root()).unwrap();
        std::fs::write(legacy.root().join("a_first.secret"), "sk-first").unwrap();
        std::fs::write(legacy.root().join("b_second.secret"), "sk-second").unwrap();

        let importer = PartialImporter {
            fail_at: 1,
            ..PartialImporter::default()
        };
        let error = migrate_legacy_files(&legacy, &importer).unwrap_err();
        assert!(matches!(error, CoreError::SecretStoreUnavailable { .. }));

        // 两条旧文件都还在：迁移失败不能变成密钥丢失。
        assert_eq!(legacy.get("a_first").unwrap().as_deref(), Some("sk-first"));
        assert_eq!(
            legacy.get("b_second").unwrap().as_deref(),
            Some("sk-second")
        );
        // 第一条已经写进去的凭据被回滚删掉了。
        assert_eq!(
            *importer.written.lock().unwrap(),
            vec!["a_first".to_owned()]
        );
        assert_eq!(
            *importer.deleted.lock().unwrap(),
            vec!["a_first".to_owned()]
        );
    }

    #[test]
    fn fallback_store_uses_files_and_says_why() {
        let dir = tempfile::tempdir().unwrap();
        let store =
            WinCredSecretStore::fallback_for_test(dir.path(), "测试注入：凭据接口被策略禁用");
        let reference = store.put("测试", "sk-fallback-0123456789").unwrap();
        assert_eq!(
            store.get(&reference).unwrap().as_deref(),
            Some("sk-fallback-0123456789")
        );
        let description = store.describe();
        assert!(description.contains("独立文件"), "{description}");
        assert!(
            description.contains("测试注入：凭据接口被策略禁用"),
            "{description}"
        );
        store.delete(&reference).unwrap();
    }

    #[test]
    fn mixed_store_reads_both_backends_and_writes_to_cred() {
        let dir = tempfile::tempdir().unwrap();
        let file = FileSecretStore::beside_library(dir.path());
        let legacy_ref = file.put("旧", "sk-file-legacy").unwrap();
        let cred = cred();
        let cred_ref = cred.put("新", "sk-cred-new").unwrap();
        let mixed = MixedSecretStore {
            cred,
            file,
            reason: "测试：迁移未完成".to_owned(),
        };
        let mut cleanup = CredCleanup {
            store: &mixed.cred,
            refs: vec![cred_ref.clone()],
        };

        assert_eq!(
            mixed.get(&legacy_ref).unwrap().as_deref(),
            Some("sk-file-legacy")
        );
        assert_eq!(
            mixed.get(&cred_ref).unwrap().as_deref(),
            Some("sk-cred-new")
        );
        let new_ref = mixed.put("新", "sk-mixed-new").unwrap();
        cleanup.refs.push(new_ref.clone());
        // 新写入优先走凭据管理器，不往旧文件里再落一份明文。
        assert_eq!(
            mixed.cred.get(&new_ref).unwrap().as_deref(),
            Some("sk-mixed-new")
        );
        assert_eq!(mixed.file.get(&new_ref).unwrap(), None);
        assert!(
            mixed.describe().contains("迁移未完成"),
            "{}",
            mixed.describe()
        );

        mixed.delete(&legacy_ref).unwrap();
        mixed.delete(&cred_ref).unwrap();
        mixed.delete(&new_ref).unwrap();
    }

    struct FailingImporter;

    impl SecretImporter for FailingImporter {
        fn put_at(&self, _credential_ref: &str, _secret: &str) -> Result<()> {
            Err(CoreError::SecretStoreUnavailable {
                reason: "测试注入的写入失败".to_owned(),
            })
        }

        fn remove_imported(&self, _credential_ref: &str) -> Result<()> {
            Ok(())
        }
    }

    #[derive(Default)]
    struct PartialImporter {
        written: Mutex<Vec<String>>,
        deleted: Mutex<Vec<String>>,
        fail_at: usize,
    }

    impl SecretImporter for PartialImporter {
        fn put_at(&self, credential_ref: &str, _secret: &str) -> Result<()> {
            let mut written = self.written.lock().expect("测试锁");
            if written.len() == self.fail_at {
                return Err(CoreError::SecretStoreUnavailable {
                    reason: "测试注入的第二次写入失败".to_owned(),
                });
            }
            written.push(credential_ref.to_owned());
            Ok(())
        }

        fn remove_imported(&self, credential_ref: &str) -> Result<()> {
            self.deleted
                .lock()
                .expect("测试锁")
                .push(credential_ref.to_owned());
            Ok(())
        }
    }
}

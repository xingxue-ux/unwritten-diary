//! Windows 凭据托管的端到端测试：`Core::open` 真的选中凭据管理器，旧明文真的迁走。
//!
//! 这些用例只在 Windows 上编译。用 `CARGO_TARGET_TMPDIR`（工作区 `target/` 下）
//! 而不是 `tempfile::tempdir()`：临时目录里的资料库按设计**不写**系统凭据
//! （见 `WinCredSecretStore::open`），拿它测「选中凭据管理器」会永远测到文件回退。
//! 每个用例都用随机引用，并在结束时删掉，不在用户的凭据管理器里留东西。

#![cfg(windows)]

use std::path::{Path, PathBuf};

use diary_core::provider::secret::SecretStore;
use diary_core::{Core, FileSecretStore, WinCredSecretStore};

fn scratch_dir(name: &str) -> PathBuf {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("系统时间应晚于 UNIX 纪元")
        .as_nanos();
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!(
        "secret-wincred-{name}-{}-{stamp}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("建测试目录");
    dir
}

fn remove_scratch(dir: &Path) {
    let _ = std::fs::remove_dir_all(dir);
}

/// 测试用完把凭据删干净；断言失败展开栈时也会走到这里。
struct CredCleanup<'a> {
    store: &'a dyn SecretStore,
    refs: Vec<String>,
}

impl Drop for CredCleanup<'_> {
    fn drop(&mut self) {
        for reference in &self.refs {
            let _ = self.store.delete(reference);
        }
    }
}

#[test]
fn core_open_uses_the_windows_credential_manager() {
    let dir = scratch_dir("core");
    let path = dir.join("library.sqlite");
    let core = Core::open(&path).expect("打开资料库");
    let description = core.secrets().describe();
    assert!(
        description.contains("凭据管理器"),
        "Windows 正式资料库应走凭据管理器，实际：{description}"
    );
    assert!(!description.contains("独立文件"), "{description}");

    let reference = core
        .secrets()
        .put("测试", "sk-core-wincred-0123456789")
        .unwrap();
    let cleanup = CredCleanup {
        store: core.secrets(),
        refs: vec![reference.clone()],
    };
    assert_eq!(
        core.secrets().get(&reference).unwrap().as_deref(),
        Some("sk-core-wincred-0123456789")
    );
    assert!(core.secrets().contains(&reference).unwrap());
    // 凭据管理器路径不该再往资料库目录里写明文文件。
    let secrets_dir = dir.join("secrets");
    assert!(
        !secrets_dir.exists(),
        "凭据管理器路径不该再写 secrets/：{}",
        secrets_dir.display()
    );

    drop(cleanup);
    drop(core);
    remove_scratch(&dir);
}

#[test]
fn wincred_open_migrates_legacy_plaintext_files() {
    let dir = scratch_dir("migrate");
    let legacy = FileSecretStore::beside_library(&dir);
    let reference = legacy.put("测试", "sk-legacy-wincred-0123456789").unwrap();
    assert!(legacy.get(&reference).unwrap().is_some());

    let store = WinCredSecretStore::open(&dir);
    let description = store.describe();
    assert!(description.contains("凭据管理器"), "{description}");
    let cleanup = CredCleanup {
        store: &store,
        refs: vec![reference.clone()],
    };

    assert_eq!(
        store.get(&reference).unwrap().as_deref(),
        Some("sk-legacy-wincred-0123456789")
    );
    assert_eq!(
        legacy.get(&reference).unwrap(),
        None,
        "迁移成功后旧明文必须删掉"
    );

    // 再打开一次：幂等，不报错，密钥还在。
    let again = WinCredSecretStore::open(&dir);
    assert_eq!(
        again.get(&reference).unwrap().as_deref(),
        Some("sk-legacy-wincred-0123456789")
    );

    drop(cleanup);
    remove_scratch(&dir);
}

#[test]
fn core_open_keeps_temp_libraries_on_the_file_store() {
    let dir = tempfile::tempdir().unwrap();
    let core = Core::open(dir.path().join("library.sqlite")).unwrap();
    let description = core.secrets().describe();
    assert!(description.contains("独立文件"), "{description}");
    assert!(description.contains("临时"), "{description}");
}

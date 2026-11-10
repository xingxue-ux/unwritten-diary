//! B1b 的原件文件库与导入交接测试。
//!
//! 对应验收计划的 E07（导入后删除外部原文件仍能打开）、E08（多文件导入其中一个
//! 失败）、E09（超大文件不占满内存、中断可恢复）与 E06 的原件关系部分。

// `write_all` 只在 Linux 的「大文件不整体读进内存」那条用例里用到（它靠
// `/proc/self/status` 读峰值内存，别的平台没有等价物）。不加 cfg 的话，Windows 上
// 严格 clippy 会报未使用导入。
#[cfg(target_os = "linux")]
use std::io::Write;

use diary_core::{
    Core, CreateDraftInput, ErrorCode, ImportManifest, ImportOrigin, ImportRequest, ImportState,
};
use sha2::{Digest, Sha256};

fn new_core(dir: &std::path::Path) -> Core {
    Core::open_in_memory_at(dir).expect("打开内存库 + 文件根目录")
}

fn draft(core: &mut Core) -> String {
    core.create_draft(CreateDraftInput {
        occurred_at: None,
        time_zone: "Asia/Shanghai",
        utc_offset_minutes: 480,
        operation_id: "op-create",
    })
    .expect("创建草稿")
    .id
}

fn sha_of(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

fn prepare(core: &mut Core, capture_id: &str, name: &str, size: i64, op: &str) -> diary_core::ImportTicket {
    core.prepare_import(ImportRequest {
        capture_id,
        display_name: name,
        mime_hint: Some("image/png"),
        size_hint: Some(size),
        origin: ImportOrigin::Picker,
        operation_id: op,
    })
    .expect("申请导入票据")
}

fn manifest(bytes: &[u8], name: &str) -> ImportManifest {
    ImportManifest {
        copied_bytes: bytes.len() as i64,
        sha256: sha_of(bytes),
        detected_mime: "image/png".to_owned(),
        original_name: name.to_owned(),
    }
}

#[test]
fn import_creates_asset_and_attaches_it_to_the_capture() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    let capture_id = draft(&mut core);
    let bytes = b"\x89PNG\r\n\x1a\n fake png bytes";

    let ticket = prepare(&mut core, &capture_id, "截图.png", bytes.len() as i64, "op-imp");
    std::fs::write(&ticket.staging_ticket, bytes).unwrap();
    let status = core
        .finish_import(&ticket.import_id, &ticket.staging_ticket, manifest(bytes, "截图.png"))
        .unwrap();

    assert_eq!(status.state, ImportState::Ready);
    let asset_id = status.asset_id.expect("完成后必须给出资产 id");

    // 资产真的在磁盘上，且内容一致。
    let lease = core.open_asset(&asset_id, "preview").unwrap();
    assert_eq!(std::fs::read(&lease.handle).unwrap(), bytes);
    assert_eq!(lease.byte_size, bytes.len() as i64);

    // 材料挂到了这条记录上，记录的 revision 也推进了。
    let capture = core.get_capture(&capture_id).unwrap();
    assert_eq!(capture.revision, 2);
    assert_eq!(capture.ordered_source_ids.len(), 1);

    // 暂存文件不该留在原地。
    assert!(!std::path::Path::new(&ticket.staging_ticket).exists());
}

#[test]
fn prepare_is_idempotent_by_operation_id() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    let capture_id = draft(&mut core);

    let first = prepare(&mut core, &capture_id, "a.png", 10, "op-same");
    let second = prepare(&mut core, &capture_id, "a.png", 10, "op-same");
    assert_eq!(first, second, "同一个 operationId 必须拿到同一张票据");

    // 换个内容复用同一个 operationId 应当报冲突。
    let conflict = core.prepare_import(ImportRequest {
        capture_id: &capture_id,
        display_name: "b.png",
        mime_hint: Some("image/png"),
        size_hint: Some(10),
        origin: ImportOrigin::Picker,
        operation_id: "op-same",
    });
    assert_eq!(conflict.unwrap_err().code(), ErrorCode::IdempotencyConflict);
}

#[test]
fn a_tampered_copy_is_rejected_and_the_staging_file_is_kept() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    let capture_id = draft(&mut core);
    let bytes = b"real bytes";
    let ticket = prepare(&mut core, &capture_id, "a.bin", bytes.len() as i64, "op-imp");
    std::fs::write(&ticket.staging_ticket, bytes).unwrap();

    // 声明一个对不上的哈希：核心自己算出来的才是真的。
    let lying = ImportManifest {
        copied_bytes: bytes.len() as i64,
        sha256: sha_of(b"something else"),
        detected_mime: "image/png".to_owned(),
        original_name: "a.bin".to_owned(),
    };
    let error = core
        .finish_import(&ticket.import_id, &ticket.staging_ticket, lying)
        .unwrap_err();
    assert_eq!(error.code(), ErrorCode::IntegrityFailed);

    // 会话被标成 error，暂存文件保留，没有产生资产。
    let status = core.import_status(&ticket.import_id).unwrap();
    assert_eq!(status.state, ImportState::Error);
    assert_eq!(status.error_code.as_deref(), Some("integrity_failed"));
    assert!(std::path::Path::new(&ticket.staging_ticket).exists());
    assert_eq!(core.asset_stats().unwrap().0, 0);
}

#[test]
fn a_size_mismatch_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    let capture_id = draft(&mut core);
    let bytes = b"0123456789";
    let ticket = prepare(&mut core, &capture_id, "a.bin", bytes.len() as i64, "op-imp");
    std::fs::write(&ticket.staging_ticket, bytes).unwrap();

    let wrong_size = ImportManifest {
        copied_bytes: 999,
        sha256: sha_of(bytes),
        detected_mime: "image/png".to_owned(),
        original_name: "a.bin".to_owned(),
    };
    let error = core
        .finish_import(&ticket.import_id, &ticket.staging_ticket, wrong_size)
        .unwrap_err();
    assert_eq!(error.code(), ErrorCode::IntegrityFailed);
}

#[test]
fn identical_content_is_stored_once() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    let bytes = b"the very same bytes";

    for index in 0..2 {
        let capture_id = core
            .create_draft(CreateDraftInput {
                occurred_at: None,
                time_zone: "Asia/Shanghai",
                utc_offset_minutes: 480,
                operation_id: &format!("op-create-{index}"),
            })
            .unwrap()
            .id;
        let ticket = prepare(
            &mut core,
            &capture_id,
            "same.bin",
            bytes.len() as i64,
            &format!("op-imp-{index}"),
        );
        std::fs::write(&ticket.staging_ticket, bytes).unwrap();
        core.finish_import(&ticket.import_id, &ticket.staging_ticket, manifest(bytes, "same.bin"))
            .unwrap();
    }

    let (assets, stored_bytes) = core.asset_stats().unwrap();
    assert_eq!(assets, 2, "两条导入记录各自独立");
    assert_eq!(
        stored_bytes,
        bytes.len() as i64,
        "相同内容只应存一份底层文件"
    );
}

#[test]
fn deleting_the_external_file_does_not_break_the_asset() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    let capture_id = draft(&mut core);
    let bytes = b"photo from the download folder";

    // 模拟外部来源文件：平台把它复制进暂存区，原文件仍在外部。
    let external = dir.path().join("外部的原文件.png");
    std::fs::write(&external, bytes).unwrap();
    let ticket = prepare(&mut core, &capture_id, "外部.png", bytes.len() as i64, "op-imp");
    std::fs::copy(&external, &ticket.staging_ticket).unwrap();
    let status = core
        .finish_import(&ticket.import_id, &ticket.staging_ticket, manifest(bytes, "外部.png"))
        .unwrap();

    // 用户把外部原文件删了。
    std::fs::remove_file(&external).unwrap();

    let lease = core.open_asset(&status.asset_id.unwrap(), "preview").unwrap();
    assert_eq!(std::fs::read(&lease.handle).unwrap(), bytes);
    assert!(!external.exists(), "外部文件确实已经没了");
}

#[test]
fn cancelling_one_import_leaves_the_others_alone() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    let capture_id = draft(&mut core);

    let doomed = prepare(&mut core, &capture_id, "doomed.bin", 4, "op-1");
    let kept = prepare(&mut core, &capture_id, "kept.bin", 4, "op-2");
    std::fs::write(&doomed.staging_ticket, b"junk").unwrap();
    std::fs::write(&kept.staging_ticket, b"good").unwrap();

    core.cancel_import(&doomed.import_id).unwrap();
    assert!(!std::path::Path::new(&doomed.staging_ticket).exists());
    assert_eq!(
        core.import_status(&doomed.import_id).unwrap_err().code(),
        ErrorCode::NotFound,
        "取消后这条导入就不存在了"
    );

    // 另一个不受影响，仍然能正常完成。
    let status = core
        .finish_import(&kept.import_id, &kept.staging_ticket, manifest(b"good", "kept.bin"))
        .unwrap();
    assert_eq!(status.state, ImportState::Ready);
}

#[test]
fn interrupted_imports_become_recoverable_after_reopening() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("library.sqlite");
    let bytes = b"half copied bytes";

    let (import_id, staging) = {
        let mut core = Core::open(&db).unwrap();
        let capture_id = draft(&mut core);
        let ticket = prepare(&mut core, &capture_id, "半截.bin", bytes.len() as i64, "op-imp");
        std::fs::write(&ticket.staging_ticket, bytes).unwrap();
        // 没有调用 finish，直接「进程结束」。
        (ticket.import_id, ticket.staging_ticket)
    };

    let mut core = Core::open(&db).unwrap();
    let status = core.import_status(&import_id).unwrap();
    assert_eq!(
        status.state,
        ImportState::Recoverable,
        "上一条没走完的导入不能被当成成功"
    );
    assert_eq!(core.asset_stats().unwrap().0, 0);

    // 暂存文件还在，用户可以直接重试完成。
    let status = core
        .finish_import(&import_id, &staging, manifest(bytes, "半截.bin"))
        .unwrap();
    assert_eq!(status.state, ImportState::Ready);
}

#[test]
fn open_asset_reports_missing_when_the_stored_file_vanishes() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    let capture_id = draft(&mut core);
    let bytes = b"will vanish";
    let ticket = prepare(&mut core, &capture_id, "x.bin", bytes.len() as i64, "op-imp");
    std::fs::write(&ticket.staging_ticket, bytes).unwrap();
    let status = core
        .finish_import(&ticket.import_id, &ticket.staging_ticket, manifest(bytes, "x.bin"))
        .unwrap();
    let asset_id = status.asset_id.unwrap();

    // 先正常打开一次拿到文件路径，再人为把它删掉。
    let stored_path = core.open_asset(&asset_id, "play").unwrap().handle;
    std::fs::remove_file(&stored_path).unwrap();

    let error = core.open_asset(&asset_id, "play").unwrap_err();
    assert_eq!(error.code(), ErrorCode::AssetMissing);
    assert!(error.to_string().contains("文件不在"));

    // 再开一次：库里的状态已经被如实改成 missing，报错理由随之变化。
    let second = core.open_asset(&asset_id, "play").unwrap_err();
    assert_eq!(second.code(), ErrorCode::AssetMissing);
    assert!(second.to_string().contains("missing"));
}

#[test]
fn leases_are_tracked_and_released() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    let capture_id = draft(&mut core);
    let bytes = b"lease bytes";
    let ticket = prepare(&mut core, &capture_id, "l.bin", bytes.len() as i64, "op-imp");
    std::fs::write(&ticket.staging_ticket, bytes).unwrap();
    let status = core
        .finish_import(&ticket.import_id, &ticket.staging_ticket, manifest(bytes, "l.bin"))
        .unwrap();
    let asset_id = status.asset_id.unwrap();

    let lease = core.open_asset(&asset_id, "export").unwrap();
    assert_eq!(core.active_lease_count(), 1);
    assert_eq!(core.lease_handles().len(), 1);
    assert_eq!(core.lease_handles()[0].0, lease.lease_id);

    core.release_asset(&lease.lease_id).unwrap();
    assert_eq!(core.active_lease_count(), 0);
    assert_eq!(
        core.release_asset(&lease.lease_id).unwrap_err().code(),
        ErrorCode::NotFound
    );

    // 用途必须是契约里的三个之一。
    let bad_usage = core.open_asset(&asset_id, "burn").unwrap_err();
    assert_eq!(bad_usage.code(), ErrorCode::InvalidState);
}

#[test]
fn imports_need_a_library_root() {
    let mut core = Core::open_in_memory().unwrap();
    let capture_id = draft(&mut core);
    let error = core
        .prepare_import(ImportRequest {
            capture_id: &capture_id,
            display_name: "x.bin",
            mime_hint: None,
            size_hint: None,
            origin: ImportOrigin::Picker,
            operation_id: "op-imp",
        })
        .unwrap_err();
    assert_eq!(error.code(), ErrorCode::InvalidState);
}

/// 读本进程的峰值常驻内存（Linux 的 `VmHWM`）。
///
/// 只有那条 Linux 用例用得到；别的平台没有等价物，所以整个函数跟着 cfg 走——
/// 否则 Windows 上严格 clippy 会报「未使用的函数」。
#[cfg(target_os = "linux")]
fn peak_rss_mib() -> Option<f64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmHWM:") {
            let kib: f64 = rest.split_whitespace().next()?.parse().ok()?;
            return Some(kib / 1024.0);
        }
    }
    None
}

#[test]
#[cfg(target_os = "linux")]
fn a_large_import_does_not_load_the_file_into_memory() {
    const CHUNK: usize = 64 * 1024;
    const CHUNKS: usize = 1024; // 合计 64 MiB
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    let capture_id = draft(&mut core);
    let total = (CHUNK * CHUNKS) as i64;

    let ticket = prepare(&mut core, &capture_id, "大文件.bin", total, "op-imp");

    // 测试自己也要流式写，不能先把 64 MiB 攒在内存里。
    let mut hasher = Sha256::new();
    {
        let mut file = std::fs::File::create(&ticket.staging_ticket).unwrap();
        let chunk = vec![7_u8; CHUNK];
        hasher.update(&chunk);
        for _ in 0..CHUNKS {
            file.write_all(&chunk).unwrap();
        }
        file.sync_all().unwrap();
    }
    // 哈希要按内容出现 1024 次来算。
    let mut expected = Sha256::new();
    for _ in 0..CHUNKS {
        expected.update(vec![7_u8; CHUNK]);
    }
    let expected = format!("{:x}", expected.finalize());

    let before = peak_rss_mib().expect("Linux 上应能读到 VmHWM");
    let status = core
        .finish_import(
            &ticket.import_id,
            &ticket.staging_ticket,
            ImportManifest {
                copied_bytes: total,
                sha256: expected,
                detected_mime: "application/octet-stream".to_owned(),
                original_name: "大文件.bin".to_owned(),
            },
        )
        .unwrap();
    let after = peak_rss_mib().unwrap();

    assert_eq!(status.state, ImportState::Ready);
    assert_eq!(core.asset_stats().unwrap().1, total);
    let delta = after - before;
    println!("导入 64 MiB：峰值常驻内存从 {before:.1} MiB 到 {after:.1} MiB（+{delta:.1} MiB）");
    assert!(
        delta < 16.0,
        "导入 64 MiB 文件不该让峰值内存涨 {delta:.1} MiB（说明是整体读进内存了）"
    );
    let _ = hasher;
}

//! B7 的导出与备份测试（issue #86，验收 E34 / E35）。
//!
//! 要证明的四件事：
//! 1. **E34**：备份期间持续写入（新建记录、改正文、导入小文件）时备份成功；恢复到新目录
//!    后库能打开、**所有原件哈希逐一匹配**、快照边界内的记录/修订数与清单一致，快照之后
//!    的写入不在备份里**且不导致恢复失败**；
//! 2. **E35 三条**：损坏 / 越界路径 / 不兼容版本都被拒绝，原因明确，**当前资料库零改动**
//!    （整个资料库目录的逐文件哈希 + 关键行数前后对比）；
//! 3. **不产生假成功**：备份自校验失败不报成功、不留包；恢复失败不留半成品；
//! 4. **边界如实**：备份时有未完成的导入，恢复出来的库如实呈现「未完成」，不假装 ready。
//!
//! 伪造无效备份的方式：读真备份包的清单与条目，改一处再重写一个 zip。这样被拒绝的
//! 一定是「这份包」，而不是测试自己造错了包。

use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;

use diary_core::{
    BackupFault, Core, CoreError, CreateDraftInput, ErrorCode, ImportManifest, ImportOrigin,
    ImportRequest, SCHEMA_VERSION,
};
use rusqlite::{Connection, OpenFlags};
use sha2::{Digest, Sha256};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

// ------------------------------------------------------------------ 基本工具

fn sha_of(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

fn new_core(root: &Path) -> Core {
    Core::open(root.join("library.sqlite")).expect("打开资料库")
}

fn draft(core: &mut Core, op: &str) -> String {
    core.create_draft(CreateDraftInput {
        occurred_at: None,
        time_zone: "Asia/Shanghai",
        utc_offset_minutes: 480,
        operation_id: op,
    })
    .expect("创建草稿")
    .id
}

/// 新建 → 改正文 → 提交，返回提交后的记录。
fn write_capture(core: &mut Core, op: &str, text: &str) -> String {
    let capture_id = draft(core, &format!("{op}-create"));
    let saved = core
        .save_draft(&capture_id, text, 1, &format!("{op}-save"))
        .expect("保存草稿");
    core.commit(&capture_id, saved.revision, &format!("{op}-commit"))
        .expect("提交记录");
    capture_id
}

/// 导入一个小文件（走真正的 prepare → 写暂存文件 → finish 路径）。
fn import(core: &mut Core, capture_id: &str, name: &str, bytes: &[u8], op: &str) -> String {
    let ticket = core
        .prepare_import(ImportRequest {
            capture_id,
            display_name: name,
            mime_hint: Some("image/png"),
            size_hint: Some(bytes.len() as i64),
            origin: ImportOrigin::Picker,
            operation_id: op,
        })
        .expect("申请导入票据");
    fs::write(&ticket.staging_ticket, bytes).expect("写暂存文件");
    let status = core
        .finish_import(
            &ticket.import_id,
            &ticket.staging_ticket,
            ImportManifest {
                copied_bytes: bytes.len() as i64,
                sha256: sha_of(bytes),
                detected_mime: "image/png".to_owned(),
                original_name: name.to_owned(),
            },
        )
        .expect("导入完成");
    status.asset_id.expect("导入一定产出资产")
}

fn blob_rel_path(sha: &str) -> String {
    format!("blobs/{}/{}", &sha[..2], sha)
}

/// 一个资料库目录的指纹：**每个文件的相对路径 + 大小 + 内容哈希**。
///
/// 用它证明「当前资料库零改动」：拒绝恢复前后指纹必须一模一样（`-wal` / `-shm` 也在里面，
/// 所以「只改了日志」这种改动也逃不掉）。
fn library_fingerprint(root: &Path) -> BTreeMap<String, String> {
    fn walk(dir: &Path, root: &Path, out: &mut BTreeMap<String, String>) {
        let entries = match fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(err) => panic!("读目录 {} 失败：{err}", dir.display()),
        };
        for entry in entries {
            let entry = entry.expect("目录项");
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path).expect("元数据");
            if metadata.is_dir() {
                walk(&path, root, out);
            } else {
                let relative = path
                    .strip_prefix(root)
                    .expect("相对路径")
                    .to_string_lossy()
                    .replace('\\', "/");
                let bytes = fs::read(&path).expect("读文件");
                out.insert(relative, format!("{}:{}", bytes.len(), sha_of(&bytes)));
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

/// 目录直接子项的名字（排序），用来证明「没有留下半成品」。
fn dir_children(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .expect("读目录")
        .map(|entry| {
            entry
                .expect("目录项")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    names.sort();
    names
}

/// 只读连接读一个库的关键行数。
fn counts_of(library: &Path) -> (i64, i64, i64, i64) {
    let conn =
        Connection::open_with_flags(library, OpenFlags::SQLITE_OPEN_READ_ONLY).expect("只读打开");
    let one = |sql: &str| -> i64 { conn.query_row(sql, [], |row| row.get(0)).expect("计数") };
    (
        one("SELECT COUNT(*) FROM captures"),
        one("SELECT COUNT(*) FROM source_revisions"),
        one("SELECT COUNT(*) FROM assets"),
        one("SELECT COUNT(*) FROM blobs"),
    )
}

/// 测试自己把恢复出来的库里**每一条原件**重新哈希一遍，返回核对过的条数。
fn verify_every_original(root: &Path, library: &Path) -> i64 {
    let conn =
        Connection::open_with_flags(library, OpenFlags::SQLITE_OPEN_READ_ONLY).expect("只读打开");
    let mut stmt = conn
        .prepare("SELECT sha256, object_ref, byte_size FROM blobs ORDER BY sha256")
        .expect("准备查询");
    let rows: Vec<(String, String, i64)> = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .expect("查询原件")
        .collect::<rusqlite::Result<Vec<_>>>()
        .expect("收集原件");

    let mut verified = 0;
    for (sha, object_ref, byte_size) in rows {
        assert_eq!(
            object_ref,
            blob_rel_path(&sha),
            "原件不是内容寻址的路径：{object_ref}"
        );
        let path = root.join(&object_ref);
        let bytes =
            fs::read(&path).unwrap_or_else(|err| panic!("原件 {} 读不到：{err}", path.display()));
        assert_eq!(
            bytes.len() as i64,
            byte_size,
            "原件 {} 大小不符",
            path.display()
        );
        assert_eq!(sha_of(&bytes), sha, "原件 {} 哈希不符", path.display());
        verified += 1;
    }
    verified
}

// ------------------------------------------------------------------ 伪造备份包

struct PackageParts {
    manifest: serde_json::Value,
    entries: Vec<(String, Vec<u8>)>,
}

fn read_package(path: &Path) -> PackageParts {
    let file = fs::File::open(path).expect("打开备份包");
    let mut archive = ZipArchive::new(file).expect("是 zip");
    let mut manifest = None;
    let mut entries = Vec::new();
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).expect("条目");
        let name = entry.name().to_owned();
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).expect("读条目");
        if name == "manifest.json" {
            manifest = Some(serde_json::from_slice(&bytes).expect("清单是 JSON"));
        }
        entries.push((name, bytes));
    }
    PackageParts {
        manifest: manifest.expect("包里一定有清单"),
        entries,
    }
}

/// 按清单顺序重写一个 zip：**清单在最前**，其余条目按原样写回。
fn write_package(path: &Path, parts: &PackageParts) {
    let file = fs::File::create(path).expect("建包");
    let mut zip = ZipWriter::new(file);
    let manifest = serde_json::to_vec_pretty(&parts.manifest).expect("序列化清单");
    zip.start_file("manifest.json", SimpleFileOptions::default())
        .expect("写清单");
    zip.write_all(&manifest).expect("写清单字节");
    for (name, bytes) in &parts.entries {
        if name == "manifest.json" {
            continue;
        }
        zip.start_file(
            name.clone(),
            SimpleFileOptions::default().compression_method(CompressionMethod::Stored),
        )
        .expect("写条目");
        zip.write_all(bytes).expect("写条目字节");
    }
    zip.finish().expect("收尾");
}

fn assert_code(error: &CoreError, expected: ErrorCode) {
    assert_eq!(
        error.code(),
        expected,
        "错误码不符：{error}（期望 {}）",
        expected.wire()
    );
    assert!(!error.to_string().is_empty(), "拒绝必须带可读原因");
}

// ------------------------------------------------------------------ E34

/// E34：有持续写入时创建完整备份 → 恢复到新库 → 所有原件哈希逐一匹配。
///
/// 「持续写入」用第二个驱动（另一个 `Core`，同一个库文件）在**备份窗口内**不停写：
/// 新建记录 → 改正文 → 提交，并每 5 轮导入一个 1KB 的小文件。断言备份窗口内至少发生了
/// 一次写入（否则这条测试就没在测「有持续写入」），并且备份照样成功。
#[test]
fn e34_backup_with_continuous_writes_restores_and_verifies_every_original() {
    let root = tempfile::tempdir().expect("临时目录");
    let root_path = root.path();
    let library = root_path.join("library.sqlite");
    let mut core = new_core(root_path);

    // 先放点底料：两篇有正文的记录 + 一个 2MB 的原件（让备份窗口明显长于一次写入）。
    write_capture(&mut core, "seed-1", "第一篇：今天把备份做出来了");
    write_capture(&mut core, "seed-2", "第二篇：还要证明快照边界");
    let seed_capture = draft(&mut core, "seed-3");
    let big_bytes: Vec<u8> = (0..2 * 1024 * 1024).map(|i| (i % 251) as u8).collect();
    let big_sha = sha_of(&big_bytes);
    let big_asset = import(
        &mut core,
        &seed_capture,
        "大图.png",
        &big_bytes,
        "seed-import",
    );
    assert!(!big_asset.is_empty());

    let (pre_captures, pre_revisions, pre_assets, pre_blobs) = counts_of(&library);
    assert_eq!(pre_blobs, 1);

    // 持续写入：第二个驱动，同一个库文件。
    let stop = Arc::new(AtomicBool::new(false));
    let started = Arc::new(AtomicBool::new(false));
    let writes = Arc::new(AtomicU64::new(0));
    let writer = {
        let library = library.clone();
        let stop = Arc::clone(&stop);
        let started = Arc::clone(&started);
        let writes = Arc::clone(&writes);
        thread::spawn(move || -> (i64, u64) {
            let mut core = Core::open(&library).expect("第二个驱动能开同一个库");
            let mut index = 0_u64;
            while !stop.load(Ordering::Relaxed) && index < 5000 {
                let text = format!("并发写入第 {index} 轮：备份期间继续记录");
                write_capture(&mut core, &format!("w{index}"), &text);
                if index.is_multiple_of(5) {
                    let capture_id = draft(&mut core, &format!("w{index}-import-draft"));
                    let bytes = format!("并发导入的小文件 {index}").into_bytes();
                    import(
                        &mut core,
                        &capture_id,
                        &format!("小文件-{index}.png"),
                        &bytes,
                        &format!("w{index}-import"),
                    );
                }
                writes.fetch_add(1, Ordering::Relaxed);
                started.store(true, Ordering::Relaxed);
                index += 1;
            }
            (core.count_captures().unwrap(), index)
        })
    };
    // 等第一轮写完再开始备份：这样「备份期间在写」是确定的。
    let mut spins = 0_u32;
    while !started.load(Ordering::Relaxed) {
        thread::yield_now();
        spins += 1;
        assert!(spins < 30_000_000, "写线程没有按时跑起来");
    }

    let package = root_path.join("snapshot.diarybak");
    let writes_before = writes.load(Ordering::Relaxed);
    let report = core
        .create_backup(package.to_str().expect("UTF-8 路径"))
        .expect("有持续写入时备份也要成功");
    let writes_after = writes.load(Ordering::Relaxed);
    stop.store(true, Ordering::Relaxed);
    let (writer_captures, iterations) = writer.join().expect("写线程正常结束");

    // 备份窗口内确实有并发写入：否则这条测试测的不是「有持续写入」。
    assert!(
        writes_after > writes_before,
        "备份窗口内应当有并发写入（写线程跑了 {iterations} 轮）"
    );
    // 跑 `--nocapture` 时把这几个数字打出来，方便人工核对这条测试真的在并发。
    println!(
        "E34 证据：备份窗口内并发写入 {} 次（写线程共 {iterations} 轮）；快照里有 {} 条记录、{} 个原件",
        writes_after - writes_before,
        report.counts.captures,
        report.blob_count
    );

    // 快照之后的写入：备份返回后再新建记录 + 导入文件。
    let post_capture = draft(&mut core, "post-backup");
    let post_bytes = b"post backup bytes".to_vec();
    let post_asset = import(
        &mut core,
        &post_capture,
        "备份之后.png",
        &post_bytes,
        "post-import",
    );
    assert!(!post_asset.is_empty());
    let (final_captures, _, _, final_blobs) = counts_of(&library);
    assert!(final_captures >= writer_captures);
    assert!(final_blobs > pre_blobs);

    // 备份包的边界：不低于底料、不高于最终状态。
    assert!(report.counts.captures >= pre_captures);
    assert!(report.counts.captures <= final_captures);
    assert!(report.counts.source_revisions >= pre_revisions);
    assert!(report.counts.assets.unwrap() >= pre_assets);
    assert!(report.blob_count >= pre_blobs);
    assert!(report.blob_bytes >= big_bytes.len() as i64);
    assert!(package.is_file(), "备份包应当真的落在目标路径上");

    // 检查一遍：结构、路径、版本、哈希都对，且清单与内容一致。
    let inspection = core
        .inspect_backup(package.to_str().expect("UTF-8 路径"))
        .expect("检查备份包");
    assert!(
        inspection.issues.is_empty(),
        "备份包不该有问题：{:?}",
        inspection.issues
    );
    assert!(inspection.hashes_verified);
    assert!(inspection.compatible);
    assert_eq!(inspection.blob_count, report.blob_count);
    assert_eq!(inspection.counts, report.counts);

    // 恢复到**新目录**。
    let restored_dir = root_path.join("restored");
    let restore = core
        .restore_backup(
            package.to_str().expect("UTF-8 路径"),
            restored_dir.to_str().expect("UTF-8 路径"),
        )
        .expect("恢复到新库");
    let restored_library = PathBuf::from(&restore.library_path);
    assert!(restored_library.is_file());
    assert_eq!(restore.counts, report.counts);

    // 恢复出来的库能打开，快照边界内的记录/修订数与清单一致。
    let restored = Core::open(&restored_library).expect("恢复出来的库能打开");
    assert_eq!(restored.count_captures().unwrap(), report.counts.captures);
    let (restored_captures, restored_revisions, restored_assets, restored_blobs) =
        counts_of(&restored_library);
    assert_eq!(restored_captures, report.counts.captures);
    assert_eq!(restored_revisions, report.counts.source_revisions);
    assert_eq!(restored_assets, report.counts.assets.unwrap());
    assert_eq!(restored_blobs, report.blob_count);

    // 快照之后的写入**不在**备份里——而且这不让恢复失败（上面已经成功了）。
    assert_eq!(
        restored.get_capture(&post_capture).unwrap_err().code(),
        ErrorCode::NotFound,
        "备份返回之后新建的记录不该出现在备份里"
    );
    assert!(
        !restored_dir
            .join(blob_rel_path(&sha_of(&post_bytes)))
            .exists(),
        "备份返回之后导入的原件不该出现在备份里"
    );
    assert!(
        restored_dir.join(blob_rel_path(&big_sha)).is_file(),
        "底料原件必须在恢复出来的库里"
    );

    // **所有原件哈希逐一匹配**（测试自己再算一遍，不信核心的自校验）。
    let verified = verify_every_original(&restored_dir, &restored_library);
    assert_eq!(verified, report.blob_count);
    assert_eq!(verified, restored_blobs);
}

// ------------------------------------------------------------------ E35 · 损坏

/// E35（损坏）：哈希与清单不符的包必须被拒绝，说明原因，当前资料库零改动，
/// 目标目录不留任何半成品。
#[test]
fn e35_corrupted_backup_is_rejected_and_leaves_no_half_product() {
    let root = tempfile::tempdir().expect("临时目录");
    let root_path = root.path();
    let lib_dir = root_path.join("lib");
    fs::create_dir_all(&lib_dir).expect("建库目录");
    let mut core = new_core(&lib_dir);
    let capture = draft(&mut core, "c-create");
    import(
        &mut core,
        &capture,
        "一.png",
        b"first original",
        "c-import-1",
    );
    import(
        &mut core,
        &capture,
        "二.png",
        b"second original",
        "c-import-2",
    );
    let library = lib_dir.join("library.sqlite");

    let package = root_path.join("good.diarybak");
    core.create_backup(package.to_str().unwrap())
        .expect("先有一份好备份");

    // 把**最后一个**原件条目的内容改一个字节：前面的快照与原件已经解包进暂存目录，
    // 所以这一条同时能证明「失败后不留半成品」。
    let mut parts = read_package(&package);
    let blob_positions: Vec<usize> = parts
        .entries
        .iter()
        .enumerate()
        .filter(|(_, (name, _))| name.starts_with("blobs/"))
        .map(|(index, _)| index)
        .collect();
    assert!(
        blob_positions.len() >= 2,
        "至少要有两个原件才能证明半成品清理"
    );
    let last = *blob_positions.last().unwrap();
    let corrupted_name = parts.entries[last].0.clone();
    parts.entries[last].1[0] ^= 0xff;
    let broken = root_path.join("broken.diarybak");
    write_package(&broken, &parts);

    let target = root_path.join("restored-here");
    let fingerprint_before = library_fingerprint(&lib_dir);
    let counts_before = counts_of(&library);
    let children_before = dir_children(root_path);

    let error = core
        .restore_backup(broken.to_str().unwrap(), target.to_str().unwrap())
        .expect_err("损坏的备份必须被拒绝");
    assert_code(&error, ErrorCode::CorruptedBackup);
    assert!(
        error.to_string().contains(&corrupted_name) || error.to_string().contains("校验"),
        "拒绝原因要点明坏在哪：{error}"
    );

    // 当前资料库一字不改：整个库目录逐文件哈希 + 关键行数前后相同。
    assert_eq!(
        library_fingerprint(&lib_dir),
        fingerprint_before,
        "拒绝恢复之后当前资料库必须零改动"
    );
    assert_eq!(counts_of(&library), counts_before);
    // 目标没被创建；父目录里也没留下任何暂存/半成品。
    assert!(!target.exists(), "失败时不该创建恢复目标目录");
    assert_eq!(
        dir_children(root_path),
        children_before,
        "失败时不该在目标旁边留下暂存目录或半成品"
    );

    // `inspect` 也要如实报出来（它是「先看再用」的入口）。
    let inspection = core
        .inspect_backup(broken.to_str().unwrap())
        .expect("检查不抛错");
    assert!(!inspection.issues.is_empty());
    assert!(inspection
        .issues
        .iter()
        .any(|issue| issue.starts_with("corrupted:")));
    assert!(!inspection.hashes_verified);
}

// ------------------------------------------------------------------ E35 · 越界路径

/// E35（越界路径）：清单里的绝对路径 / `..` 必须被拒绝，**不许解包到任意位置**，
/// 当前资料库零改动。
#[test]
fn e35_path_traversal_backup_is_rejected_and_nothing_is_unpacked_outside() {
    let root = tempfile::tempdir().expect("临时目录");
    let root_path = root.path();
    let lib_dir = root_path.join("lib");
    fs::create_dir_all(&lib_dir).expect("建库目录");
    let mut core = new_core(&lib_dir);
    let capture = draft(&mut core, "t-create");
    let asset_bytes = b"original bytes for traversal".to_vec();
    import(&mut core, &capture, "原件.png", &asset_bytes, "t-import");
    let library = lib_dir.join("library.sqlite");

    let package = root_path.join("good.diarybak");
    core.create_backup(package.to_str().unwrap())
        .expect("好备份");

    // 清单里塞一条越界路径；包里也真的放一个同名的越界条目——最坏的情况是
    // 「包确实带着一个想往外写的条目」，实现必须拒绝，而不是照它解包。
    let mut parts = read_package(&package);
    let good = parts
        .entries
        .iter()
        .find(|(name, _)| name.starts_with("blobs/"))
        .cloned()
        .expect("有一个原件条目");
    parts.manifest["blobs"]
        .as_array_mut()
        .expect("blobs 是数组")
        .push(serde_json::json!({
            "path": "../escaped.bin",
            "sha256": sha_of(&good.1),
            "byteSize": good.1.len(),
        }));
    parts
        .entries
        .push(("../escaped.bin".to_owned(), good.1.clone()));
    let broken = root_path.join("traversal.diarybak");
    write_package(&broken, &parts);

    let target = root_path.join("restored-here");
    let fingerprint_before = library_fingerprint(&lib_dir);
    let counts_before = counts_of(&library);
    let children_before = dir_children(root_path);

    let error = core
        .restore_backup(broken.to_str().unwrap(), target.to_str().unwrap())
        .expect_err("越界路径必须被拒绝");
    assert_code(&error, ErrorCode::CorruptedBackup);
    let message = error.to_string();
    assert!(
        message.contains("越界") || message.contains(".."),
        "拒绝原因要点明越界路径：{message}"
    );
    assert!(
        message.contains("当前资料库没有改动"),
        "拒绝说明要讲清现库没被改：{message}"
    );

    // 没有任何东西被解包到清单之外的位置。
    assert!(!root_path.join("escaped.bin").exists());
    assert!(!target.exists());
    assert_eq!(dir_children(root_path), children_before);
    assert_eq!(library_fingerprint(&lib_dir), fingerprint_before);
    assert_eq!(counts_of(&library), counts_before);

    // inspect 也要把它标成 unsafe_path，并且不做哈希校验（不值得为越界包算哈希）。
    let inspection = core
        .inspect_backup(broken.to_str().unwrap())
        .expect("检查不抛错");
    assert!(inspection
        .issues
        .iter()
        .any(|issue| issue.starts_with("unsafe_path:")));
    assert!(!inspection.hashes_verified);
    assert!(inspection.compatible, "越界路径不是版本问题");
}

// ------------------------------------------------------------------ E35 · 不兼容

/// E35（不兼容）：`schemaVersion` / `apiVersion` 高于本核心能处理的版本时拒绝恢复，
/// 说明「先升级应用」，当前资料库零改动。
#[test]
fn e35_incompatible_backup_is_rejected_and_current_library_is_untouched() {
    let root = tempfile::tempdir().expect("临时目录");
    let root_path = root.path();
    let lib_dir = root_path.join("lib");
    fs::create_dir_all(&lib_dir).expect("建库目录");
    let mut core = new_core(&lib_dir);
    write_capture(&mut core, "i-1", "不兼容测试的正文");
    let library = lib_dir.join("library.sqlite");

    let package = root_path.join("good.diarybak");
    core.create_backup(package.to_str().unwrap())
        .expect("好备份");
    let parts = read_package(&package);

    // 两份被改过的包都先造好，再取「拒绝之前」的快照——这样目录对比只看恢复动作
    // 有没有留下东西，不会被测试自己造包的动作污染。
    let mut future_schema = PackageParts {
        manifest: parts.manifest.clone(),
        entries: parts.entries.clone(),
    };
    future_schema.manifest["schemaVersion"] = serde_json::json!(SCHEMA_VERSION + 1);
    let schema_package = root_path.join("future-schema.diarybak");
    write_package(&schema_package, &future_schema);

    let mut future_api = PackageParts {
        manifest: parts.manifest.clone(),
        entries: parts.entries.clone(),
    };
    future_api.manifest["apiVersion"] = serde_json::json!("2.0");
    let api_package = root_path.join("future-api.diarybak");
    write_package(&api_package, &future_api);

    let fingerprint_before = library_fingerprint(&lib_dir);
    let counts_before = counts_of(&library);
    let children_before = dir_children(root_path);

    // 一：schemaVersion 比本构建高。

    let target = root_path.join("restored-schema");
    let error = core
        .restore_backup(schema_package.to_str().unwrap(), target.to_str().unwrap())
        .expect_err("高 schemaVersion 必须被拒绝");
    assert_code(&error, ErrorCode::IncompatibleBackup);
    let message = error.to_string();
    assert!(
        message.contains("schemaVersion"),
        "原因要点明 schemaVersion：{message}"
    );
    assert!(message.contains("升级"), "原因要给出可执行动作：{message}");
    assert!(!target.exists());

    let inspection = core
        .inspect_backup(schema_package.to_str().unwrap())
        .expect("检查不抛错");
    assert!(!inspection.compatible);
    assert!(inspection
        .issues
        .iter()
        .any(|issue| issue.starts_with("incompatible:")));

    // 二：apiVersion 主版本比本构建高。
    let target = root_path.join("restored-api");
    let error = core
        .restore_backup(api_package.to_str().unwrap(), target.to_str().unwrap())
        .expect_err("高 apiVersion 必须被拒绝");
    assert_code(&error, ErrorCode::IncompatibleBackup);
    assert!(
        error.to_string().contains("apiVersion"),
        "原因要点明 apiVersion：{error}"
    );
    assert!(!target.exists());

    // 当前资料库零改动 + 旁边没有半成品。
    assert_eq!(
        library_fingerprint(&lib_dir),
        fingerprint_before,
        "拒绝恢复之后当前资料库必须零改动"
    );
    assert_eq!(counts_of(&library), counts_before);
    assert_eq!(dir_children(root_path), children_before);
}

// ------------------------------------------------------------------ 假成功

/// 「不产生假成功」：写完后的自校验不过（注入一个字节的写入损坏）时，
/// **不得报告成功**，也不得在目标路径上留下任何包。
#[test]
fn backup_self_check_failure_never_reports_success() {
    let root = tempfile::tempdir().expect("临时目录");
    let root_path = root.path();
    let mut core = new_core(root_path);
    write_capture(&mut core, "s-1", "自校验测试正文");

    let package = root_path.join("broken-self-check.diarybak");
    core.set_backup_fault_for_test(BackupFault::CorruptWrittenPackage);
    let error = core
        .create_backup(package.to_str().unwrap())
        .expect_err("自校验不过时必须报失败");
    assert_code(&error, ErrorCode::IntegrityFailed);
    assert!(
        error.to_string().contains("自校验"),
        "要说清是自校验没过：{error}"
    );
    assert!(!package.exists(), "失败时目标路径上不能留下任何包");
    let leftovers: Vec<String> = dir_children(root_path)
        .into_iter()
        .filter(|name| {
            name.contains("partial") || name.contains("snapshot") || name.contains("diarybak")
        })
        .collect();
    assert!(leftovers.is_empty(), "失败时不该留下半成品：{leftovers:?}");

    // 关掉注入之后，同一个目标路径还是能正常产出备份（失败不是「路径坏了」）。
    core.set_backup_fault_for_test(BackupFault::None);
    let report = core
        .create_backup(package.to_str().unwrap())
        .expect("正常备份");
    assert!(report.byte_size > 0);
}

/// 「不产生假成功」的另一半：资料库里已经有原件不可用时，备份**失败**而不是
/// 产出一份缺原件的「看起来成功」的包。
#[test]
fn backup_refuses_to_claim_success_when_an_original_is_already_gone() {
    let root = tempfile::tempdir().expect("临时目录");
    let root_path = root.path();
    let mut core = new_core(root_path);
    let capture = draft(&mut core, "m-create");
    let bytes = b"will be deleted".to_vec();
    import(&mut core, &capture, "会丢.png", &bytes, "m-import");

    // 模拟「原件在资料库里已经丢了」（外部删除 / 之前就坏了）。
    let blob = root_path.join(blob_rel_path(&sha_of(&bytes)));
    assert!(blob.is_file());
    fs::remove_file(&blob).expect("删掉原件");

    let package = root_path.join("incomplete.diarybak");
    let error = core
        .create_backup(package.to_str().unwrap())
        .expect_err("缺原件时不能报成功");
    assert_code(&error, ErrorCode::IntegrityFailed);
    assert!(
        error.to_string().contains("原件"),
        "原因要点明是哪类东西不见了：{error}"
    );
    assert!(!package.exists(), "失败时不留包");
    let leftovers: Vec<String> = dir_children(root_path)
        .into_iter()
        .filter(|name| name.contains("partial") || name.contains("snapshot"))
        .collect();
    assert!(leftovers.is_empty(), "不留半成品：{leftovers:?}");
}

// ------------------------------------------------------------------ 换机 / 未完成导入

/// 备份产物放到**另一个目录**（模拟换机：换磁盘、换机器、只拿这个文件）也能恢复。
#[test]
fn backup_restores_from_another_directory_like_a_new_machine() {
    let root = tempfile::tempdir().expect("临时目录");
    let root_path = root.path();
    let lib_dir = root_path.join("lib");
    fs::create_dir_all(&lib_dir).expect("建库目录");
    let mut core = new_core(&lib_dir);
    let capture = draft(&mut core, "x-create");
    core.save_draft(&capture, "换机也要能恢复的正文", 1, "x-save")
        .expect("保存");
    core.commit(&capture, 2, "x-commit").expect("提交");
    import(&mut core, &capture, "照片.png", b"photo bytes", "x-import");

    let package = root_path.join("good.diarybak");
    core.create_backup(package.to_str().unwrap()).expect("备份");

    // 「换机」：包挪到完全不相干的地方（另一个临时根目录），并且改名。
    let elsewhere = tempfile::tempdir().expect("另一个临时目录");
    let moved = elsewhere.path().join("从旧手机拷出来的.diarybak");
    fs::copy(&package, &moved).expect("拷贝备份包");
    let inspection = core
        .inspect_backup(moved.to_str().unwrap())
        .expect("检查拷过来的包");
    assert!(inspection.issues.is_empty(), "{:?}", inspection.issues);

    let restored_dir = elsewhere.path().join("new-library");
    let restore = core
        .restore_backup(moved.to_str().unwrap(), restored_dir.to_str().unwrap())
        .expect("换机恢复");
    let restored = Core::open(&restore.library_path).expect("打开恢复出来的库");
    assert_eq!(restored.count_captures().unwrap(), 1);
    let listed = restored.list_captures(None, None, 10).expect("列记录");
    assert_eq!(listed.captures[0].draft_text, "换机也要能恢复的正文");
    assert_eq!(
        verify_every_original(&restored_dir, Path::new(&restore.library_path)),
        1
    );
}

/// 边界如实：备份时有**未完成的导入**（`prepared`）时，恢复出来的库如实呈现
/// 那个「没完成」的状态，不假装 ready；当前资料库的状态也不被恢复动作改掉。
///
/// 口径说明：恢复本身不打开恢复出来的库（逐字节等于快照）；第一次 `Core::open`
/// 会按**既有的** `recover_on_open` 语义把 `prepared/copying/verifying` 标成
/// `recoverable`——那正是「没完成」的如实表达，本条没有改这套语义（#82 尚未合入）。
#[test]
fn unfinished_import_is_reported_honestly_after_restore() {
    let root = tempfile::tempdir().expect("临时目录");
    let root_path = root.path();
    let lib_dir = root_path.join("lib");
    fs::create_dir_all(&lib_dir).expect("建库目录");
    let mut core = new_core(&lib_dir);
    let capture = draft(&mut core, "u-create");
    write_capture(&mut core, "u-2", "已经提交的记录");

    // 未完成的导入：只 prepare（拿到暂存票据），不 finish。
    let pending = core
        .prepare_import(ImportRequest {
            capture_id: &capture,
            display_name: "还没导完.png",
            mime_hint: Some("image/png"),
            size_hint: Some(1024),
            origin: ImportOrigin::Picker,
            operation_id: "u-import",
        })
        .expect("申请导入票据");
    fs::write(&pending.staging_ticket, b"partial bytes").expect("写一半");
    let status = core.import_status(&pending.import_id).expect("导入状态");
    assert_eq!(status.state.wire(), "prepared");

    let package = root_path.join("with-pending.diarybak");
    let report = core.create_backup(package.to_str().unwrap()).expect("备份");
    assert_eq!(report.counts.imports_in_flight, Some(1));
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("没有完成的导入")),
        "报告要如实说清有未完成的导入：{:?}",
        report.warnings
    );

    // 恢复出来的库（还没被 Core::open 打开过）逐字节等于快照：状态还是 prepared。
    let restored_dir = root_path.join("restored");
    let restore = core
        .restore_backup(package.to_str().unwrap(), restored_dir.to_str().unwrap())
        .expect("恢复");
    let raw = Connection::open_with_flags(&restore.library_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .expect("只读打开恢复出来的库");
    let state: String = raw
        .query_row(
            "SELECT state FROM import_sessions WHERE id = ?1",
            rusqlite::params![pending.import_id],
            |row| row.get(0),
        )
        .expect("导入会话还在");
    assert_eq!(state, "prepared", "恢复出来的库要如实保留快照那一刻的状态");

    // 第一次打开：按既有 recover_on_open 语义如实标成「未完成」。
    let restored = Core::open(&restore.library_path).expect("打开恢复出来的库");
    let restored_status = restored
        .import_status(&pending.import_id)
        .expect("导入状态");
    assert_ne!(
        restored_status.state.wire(),
        "ready",
        "不能假装这个导入完成了"
    );
    assert_eq!(restored_status.state.wire(), "recoverable");
    assert!(
        restore
            .warnings
            .iter()
            .any(|warning| warning.contains("没有完成的导入")),
        "恢复报告也要如实说明：{:?}",
        restore.warnings
    );

    // 当前资料库自己的导入会话仍是 prepared（恢复动作没碰现库）。
    assert_eq!(
        core.import_status(&pending.import_id)
            .expect("现库状态")
            .state
            .wire(),
        "prepared"
    );
}

/// 只允许恢复到**新的空目录**：不是空目录就是拒绝（当前库零改动）。
#[test]
fn restore_refuses_a_target_that_is_not_a_new_empty_directory() {
    let root = tempfile::tempdir().expect("临时目录");
    let root_path = root.path();
    let mut core = new_core(root_path);
    write_capture(&mut core, "n-1", "只允许恢复到新目录");
    let package = root_path.join("good.diarybak");
    core.create_backup(package.to_str().unwrap()).expect("备份");

    // 非空目录。
    let occupied = root_path.join("occupied");
    fs::create_dir_all(&occupied).expect("建目录");
    fs::write(occupied.join("别人东西.txt"), "别动我".as_bytes()).expect("写文件");
    let fingerprint = library_fingerprint(root_path);
    let error = core
        .restore_backup(package.to_str().unwrap(), occupied.to_str().unwrap())
        .expect_err("非空目录必须被拒绝");
    assert_code(&error, ErrorCode::InvalidState);
    assert!(error.to_string().contains("空目录"), "{error}");
    assert!(occupied.join("别人东西.txt").is_file());
    assert_eq!(library_fingerprint(root_path), fingerprint);

    // 目标是当前资料库所在目录。
    let error = core
        .restore_backup(package.to_str().unwrap(), root_path.to_str().unwrap())
        .expect_err("现库目录必须被拒绝");
    assert_code(&error, ErrorCode::InvalidState);
    assert_eq!(library_fingerprint(root_path), fingerprint);

    // 空目录是允许的（平台层常常先 mkdir 再给票据）。
    let empty = root_path.join("empty-target");
    fs::create_dir_all(&empty).expect("建空目录");
    let restore = core
        .restore_backup(package.to_str().unwrap(), empty.to_str().unwrap())
        .expect("空目录可以恢复");
    assert!(Path::new(&restore.library_path).is_file());
}
// ------------------------------------------------------------------ 未收尾录音的随行片段

/// 未收尾录音的片段**不是内容寻址的**（文件名由平台层给、最终化后暂存目录会被删）：
/// 备份按它自己的语义处理——能拿到就按登记时的 sha256 校验后随包，恢复出来的库
/// 如实显示「没收尾」，而且这些片段是可用的（还能最终化），不是一堆摆设。
#[test]
fn unfinished_recording_clips_travel_with_the_backup() {
    let root = tempfile::tempdir().expect("临时目录");
    let root_path = root.path();
    let mut core = new_core(root_path);
    let capture = draft(&mut core, "r-create");

    let ticket = core
        .prepare_recording(&capture, "r-prepare")
        .expect("录音票据");
    let segments: Vec<(&str, Vec<u8>)> = vec![
        ("seg-0.bin", b"first closed segment".to_vec()),
        ("seg-1.bin", b"second closed segment".to_vec()),
    ];
    for (index, (name, bytes)) in segments.iter().enumerate() {
        fs::write(Path::new(&ticket.staging_ticket).join(name), bytes).expect("写片段");
        core.register_segment(
            &ticket.recording_id,
            index as i64,
            diary_core::SegmentManifest {
                segment_id: format!("seg{index}"),
                relative_file_name: (*name).to_owned(),
                duration_ms: 1000,
                sha256: sha_of(bytes),
                byte_size: bytes.len() as i64,
                closed_at: None,
            },
        )
        .expect("登记片段");
    }

    let package = root_path.join("with-recording.diarybak");
    let report = core.create_backup(package.to_str().unwrap()).expect("备份");
    assert_eq!(report.counts.open_recordings, Some(1));
    assert_eq!(
        report.transient_clip_count,
        segments.len() as i64,
        "未收尾录音的片段应当随包"
    );
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("没收尾的录音")),
        "报告要如实说明这段录音没收尾：{:?}",
        report.warnings
    );

    let restored_dir = root_path.join("restored");
    let restore = core
        .restore_backup(package.to_str().unwrap(), restored_dir.to_str().unwrap())
        .expect("恢复");
    assert_eq!(restore.transient_clip_count, segments.len() as i64);
    for (name, bytes) in &segments {
        let path = restored_dir
            .join("staging")
            .join(&ticket.recording_id)
            .join(name);
        let restored_bytes =
            fs::read(&path).unwrap_or_else(|err| panic!("片段 {} 没随包：{err}", path.display()));
        assert_eq!(sha_of(&restored_bytes), sha_of(bytes), "片段哈希要一致");
    }

    // 恢复出来的库如实显示「没收尾」，但片段是可用的：最终化能真的拼出音频。
    let mut restored = Core::open(&restore.library_path).expect("打开恢复出来的库");
    let session = restored
        .recording_session(&ticket.recording_id)
        .expect("录音会话还在");
    assert_ne!(session.state, diary_core::RecordingState::Saved);
    assert_eq!(session.state, diary_core::RecordingState::Recoverable);
    let finalized = restored
        .finalize_recording(&ticket.recording_id, 1, "restored")
        .expect("恢复出来的片段要能最终化");
    assert_eq!(finalized.state, diary_core::RecordingState::Saved);
    assert_eq!(finalized.segment_count, 2);
}

/// 内存库没有文件系统根目录：导不出「完整」备份，如实报错而不是产出一份缺原件的包。
#[test]
fn backup_requires_a_file_library() {
    let root = tempfile::tempdir().expect("临时目录");
    let mut core = Core::open_in_memory().expect("内存库");
    write_capture(&mut core, "mem-1", "内存库里的正文");
    let package = root.path().join("mem.diarybak");
    let error = core
        .create_backup(package.to_str().unwrap())
        .expect_err("内存库不能导出完整备份");
    assert_code(&error, ErrorCode::InvalidState);
    assert!(!package.exists());
}

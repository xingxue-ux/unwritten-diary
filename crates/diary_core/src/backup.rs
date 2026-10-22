//! 导出与备份：一致性快照、恢复到新库、拒绝无效备份（B7，issue #86，验收 E34/E35）。
//!
//! 任务书 §3.4 的三条硬约束在这里怎么落地：
//!
//! 1. **不许直接拷 `.sqlite` 文件**。资料库开着 WAL，最近的提交可能还在 `-wal` 里，
//!    拷主文件会得到一个「看起来是库、其实少一截」的东西。这里用 SQLite 自己的
//!    `VACUUM INTO`：它按**读事务**的一致视图导出成另一个完整文件（含 WAL 里已提交
//!    的内容），对源库只读，因此**备份期间继续写入是安全的**；
//! 2. **快照边界**：先取数据库快照，再复制**被快照引用**的原件。原件是内容寻址的
//!    （`blobs/<sha 前两位>/<sha>`），内容不会原地改写；但回收站的**永久清理**
//!    （v16，B7）会删掉不再被任何资产引用的 blob。因此「快照之后、复制之前」正好
//!    发生 purge 时，复制会因文件缺失而**整份备份失败**——绝不产出缺原件的包，也
//!    不会静默少一截。复制时**逐条重算 sha256**，对不上同样整份失败；
//! 3. **落盘之后自校验**：重新打开写好的包，逐条目重算哈希、比大小、比条目集合。
//!    自校验不过 ⇒ 删掉半成品、报错，目标路径上不会留下任何文件。
//!
//! 恢复只允许落到**新的空目录**：先在同一父目录下建暂存目录、全部校验通过后**一次
//! rename** 落地。任何一步失败都只删暂存目录，当前资料库一字不改（E35）。拒绝口径与
//! 错误码见 `docs/architecture/m5-备份与恢复.md`。
//!
//! 包里**没有**的东西（如实说明，不假装）：
//! - `staging/` 下未完成导入的部分字节（`.part`）：那是进行中操作的临时工作区，
//!   正在被平台层写，不是任何已确认的原件；
//! - 未收尾录音的已封闭片段：它们**不是内容寻址的**（文件名由平台层给），
//!   `finalize_recording` 成功后整个暂存目录还会被删掉——所以它们是「随行片段」，
//!   备份时能拿到就按记录里的 sha256 校验后随包（`transientClips`），拿不到就跳过
//!   并如实记进报告的 `warnings`，绝不因为一场并发的最终化让整份备份失败。
//!   清单里的 `transientClips` **只列真的装进包里的那几条**：清单不是「打算装什么」
//!   的声明，而是「包里真的有什么」的记录，否则自校验会反过来把这份备份判成无效
//!   （issue #95）；
//! - **API 密钥 / 凭据**：它们在资料库旁边的独立密钥托管文件里，不在 SQLite 快照里，
//!   也不随包。测试会把包逐条目解压，断言密钥明文不出现在任何条目中（契约第 8 节）。

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Condvar, Mutex};

use chrono::{DateTime, Utc};
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

use crate::error::{CoreError, Result};
use crate::model::{
    BackupCounts, BackupInspection, BackupReport, Job, JobPriority, JobState, NewJob, RestoreReport,
};
use crate::support;
use crate::Core;

/// 备份包的格式标识：写在清单里，用来挡住「拿别的 zip 当备份恢复」。
pub const BACKUP_FORMAT: &str = "unwritten-diary-backup";

/// 备份包格式版本。**本构建只认识 1**；更高的是不兼容，直接拒绝。
pub const BACKUP_FORMAT_VERSION: i64 = 1;

/// 清单在包里的固定路径。
const MANIFEST_ENTRY: &str = "manifest.json";

/// 数据库快照在包里的固定路径。恢复出来的资料库文件名固定为 `library.sqlite`
/// （原库的文件名只作为 `sourceLibraryFileName` 记在清单里给用户看，**不作为路径用**，
/// 免得清单里一个字段就能决定往哪写）。
const SNAPSHOT_ENTRY: &str = "db/library.sqlite";

/// 恢复出来的资料库文件名。
const RESTORED_LIBRARY_FILE: &str = "library.sqlite";

/// 备份任务在 `jobs.kind` 上的名字。
///
/// 它只会以 `succeeded` 落库（见 [`record_completed_job`]），队列不会领取它，也不会
/// 出现「没有执行器」的半截任务。
pub const KIND_BACKUP: &str = "backup";

/// 恢复任务在 `jobs.kind` 上的名字。同样只以 `succeeded` 落库。
pub const KIND_RESTORE: &str = "restore";

/// 流式哈希/复制的缓冲区。内存占用与文件大小无关。
const BUFFER_BYTES: usize = 64 * 1024;

/// 仅供测试的故障注入点。
///
/// 备份是「写完还要自校验」的重活，而自校验这条路径平时不会被触发——验收计划第
/// 121 行明确要求对备份创建做故障注入。所以留一个显式的、只给测试用的注入点：
/// 产品代码不碰它，默认是 [`BackupFault::None`]。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BackupFault {
    /// 不注入。
    #[default]
    None,
    /// 写完包之后、自校验之前，把包里中间的一个字节改掉，模拟写入损坏。
    CorruptWrittenPackage,
}

/// 仅供测试的**会合点**：让备份在打包随行片段之前停一下，等测试放行。
///
/// 存在的理由：文件在扫描之后、复制之前被并发最终化删掉，这个窗口靠 sleep 去碰是
/// **不可靠**的（机器快慢一变，测试就从「测到了」变成「什么都没测」）。有了会合点，
/// 测试可以确定地做到「备份正停在窗口里 → 另一条线程删掉暂存目录 → 再放行备份」。
/// 产品代码从不设置它（见 `Core::set_backup_gate_for_test`）。
#[derive(Debug, Default)]
pub struct BackupGate {
    /// 备份已经停在会合点上、还没被放行。
    waiting: Mutex<bool>,
    signal: Condvar,
}

impl BackupGate {
    pub fn new() -> Self {
        Self::default()
    }

    /// 备份线程：报到，然后等测试放行。只有设了会合点的测试会走到这里。
    fn arrive_and_wait(&self) {
        let mut waiting = self.lock();
        *waiting = true;
        self.signal.notify_all();
        while *waiting {
            waiting = self
                .signal
                .wait(waiting)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }

    /// 测试线程：等备份**真的**停在会合点上（不是「大概快了」）。
    ///
    /// 等到返回 `true`；超过 `timeout` 返回 `false`——测试据此失败，而不是把
    /// `cargo test` 挂死在一个永远不来的会合点上。
    pub fn wait_until_arrived(&self, timeout: std::time::Duration) -> bool {
        let mut waiting = self.lock();
        let deadline = std::time::Instant::now() + timeout;
        while !*waiting {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                return false;
            }
            let (guard, result) = self
                .signal
                .wait_timeout(waiting, remaining)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            waiting = guard;
            if result.timed_out() && !*waiting {
                return false;
            }
        }
        true
    }

    /// 测试线程：放行备份。
    pub fn resume(&self) {
        let mut waiting = self.lock();
        *waiting = false;
        self.signal.notify_all();
    }

    /// 这里的状态只有一个 `bool`，没有「改到一半」的中间态，所以锁中毒也照常用：
    /// 一条测试失败不该把后面的测试卡死在会合点上。
    fn lock(&self) -> std::sync::MutexGuard<'_, bool> {
        self.waiting
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

// ------------------------------------------------------------------ 清单

/// 备份包的清单（`manifest.json`）。
///
/// 字段名用 camelCase：清单是给人和别的工具看的，口径与契约里的字段名一致
/// （`schemaVersion` / `apiVersion` …）。
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Manifest {
    /// 固定为 [`BACKUP_FORMAT`]。
    format: String,
    #[serde(rename = "formatVersion")]
    format_version: i64,
    /// 快照的 schema 版本（`PRAGMA user_version`）。
    #[serde(rename = "schemaVersion")]
    schema_version: i64,
    /// 产出这份备份的核心 API 版本，口径同 `CoreInfo.apiVersion`。
    #[serde(rename = "apiVersion")]
    api_version: String,
    #[serde(rename = "coreVersion")]
    core_version: String,
    #[serde(rename = "createdAt")]
    created_at: String,
    /// 原资料库文件名，仅供展示。
    #[serde(rename = "sourceLibraryFileName")]
    source_library_file_name: String,
    snapshot: ManifestFile,
    /// 内容寻址的原件，按 sha256 排序；同一个内容只有一条。
    blobs: Vec<ManifestFile>,
    /// 未收尾录音的已封闭片段（随行，不是内容寻址的那一类）。
    #[serde(default, rename = "transientClips")]
    transient_clips: Vec<ManifestClip>,
    /// 快照里的关键行数，恢复时逐项核对。
    counts: BackupCounts,
    /// 自描述说明：包里有什么、没有什么。
    #[serde(default)]
    notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ManifestFile {
    /// 包内相对路径，同时也是它在恢复出来的资料库里的相对路径。
    path: String,
    sha256: String,
    #[serde(rename = "byteSize")]
    byte_size: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ManifestClip {
    path: String,
    sha256: String,
    #[serde(rename = "byteSize")]
    byte_size: i64,
    #[serde(rename = "recordingId")]
    recording_id: String,
    #[serde(rename = "segmentIndex")]
    segment_index: i64,
}

// ------------------------------------------------------------------ 创建备份

/// 创建备份：一致快照 + 被引用的原件 + 自校验，全部通过才在目标路径上留下文件。
pub(crate) fn create(core: &Core, target: &Path) -> Result<BackupReport> {
    if target.as_os_str().is_empty() {
        return Err(CoreError::InvalidArgument {
            reason: "备份目标路径不能为空".to_owned(),
        });
    }
    // 快速失败：目标已经存在就不必再花时间取快照。**这不是保证**——真正挡住
    // 「并发覆盖」的是落地时的 `commit_package`（link 在目标存在时原子失败）。
    if target.exists() {
        return Err(target_exists_error(target));
    }

    // 原件是文件系统里的东西：内存库没有根目录，导不出「完整」备份，如实报错。
    let root = core.root_dir()?.to_path_buf();

    // 目标父目录必须先存在/建好，然后绝对化：`VACUUM INTO` 里的相对路径是按**进程
    // 当前目录**解析的，跟调用方心里的「相对」不是一回事。
    let file_name = target
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .ok_or_else(|| CoreError::InvalidArgument {
            reason: format!("备份目标不是文件名：{}", target.display()),
        })?;
    let parent = target
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
    fs::create_dir_all(&parent)?;
    let parent = fs::canonicalize(&parent)?;
    let target_abs = parent.join(&file_name);

    let nonce = support::new_id("bak");
    let snapshot_path = parent.join(format!(".{file_name}.snapshot-{nonce}"));
    let partial_path = parent.join(format!(".{file_name}.partial-{nonce}"));

    let outcome = (|| -> Result<BackupReport> {
        // 1. 一致性快照。只读源库；WAL 里已提交的内容照样进快照。
        vacuum_into(&core.conn, &snapshot_path)?;
        let snapshot_sha = hash_file_hex(&snapshot_path)?;
        let snapshot_bytes = fs::metadata(&snapshot_path)?.len() as i64;

        // 2. 读**快照**（不是源库）：清单里的一切都以快照那一刻为准。
        let scan = scan_snapshot(&snapshot_path)?;

        // 3. 组装清单。
        let library_file_name = core
            .library_file_name
            .clone()
            .unwrap_or_else(|| RESTORED_LIBRARY_FILE.to_owned());
        let created_at = support::now();
        let mut blobs = Vec::with_capacity(scan.blobs.len());
        for blob in &scan.blobs {
            blobs.push(ManifestFile {
                path: blob.object_ref.clone(),
                sha256: blob.sha256.clone(),
                byte_size: blob.byte_size,
            });
        }
        let mut clips = Vec::with_capacity(scan.clips.len());
        for clip in &scan.clips {
            clips.push(ManifestClip {
                path: clip.archive_path.clone(),
                sha256: clip.sha256.clone(),
                byte_size: clip.byte_size,
                recording_id: clip.recording_id.clone(),
                segment_index: clip.segment_index,
            });
        }
        let mut manifest = Manifest {
            format: BACKUP_FORMAT.to_owned(),
            format_version: BACKUP_FORMAT_VERSION,
            schema_version: scan.schema_version,
            api_version: crate::API_VERSION.to_owned(),
            core_version: env!("CARGO_PKG_VERSION").to_owned(),
            created_at: support::to_iso(created_at),
            source_library_file_name: library_file_name,
            snapshot: ManifestFile {
                path: SNAPSHOT_ENTRY.to_owned(),
                sha256: snapshot_sha.clone(),
                byte_size: snapshot_bytes,
            },
            blobs,
            transient_clips: clips,
            counts: scan.counts.clone(),
            notes: vec![
                "只包含数据库快照（db/library.sqlite）与内容寻址的原件（blobs/）。".to_owned(),
                "staging/ 下未完成导入的部分字节不在包里；未收尾录音的已封闭片段能拿到就随包（transientClips），拿不到就如实记进报告。".to_owned(),
                "索引与向量是派生数据：它们在快照里，恢复后也可以重建。".to_owned(),
            ],
        };

        // 4. 写包：快照、原件、随行片段，最后按**实际装进包里的条目**写清单
        //    （并发最终化可能让某个片段在扫描之后消失，见 `write_package`）。
        let mut warnings = scan.warnings.clone();
        write_package(
            &partial_path,
            &snapshot_path,
            &root,
            &mut manifest,
            core.backup_gate.as_deref(),
            &mut warnings,
        )?;

        // 5. 故障注入（只有测试会打开）：故意把写好的包弄坏。
        if core.backup_fault == BackupFault::CorruptWrittenPackage {
            corrupt_written_package(&partial_path)?;
        }

        // 6. 自校验：重开包，逐条目重算哈希、比大小、比条目集合。
        //    不过就删半成品并报错——绝不产出「看起来成功」的备份。
        if let Err(error) = verify_package(&partial_path, &manifest) {
            let _ = fs::remove_file(&partial_path);
            return Err(CoreError::IntegrityFailed {
                reason: format!("备份写完后的自校验没通过，已删除半成品、没有产出备份：{error}"),
            });
        }

        // 7. 原子落地：到这一步目标路径上才第一次出现文件。目标已存在就失败，
        //    **绝不替换**别人已经落地的备份（见 `commit_package`）。
        commit_package(&partial_path, &target_abs)?;
        let byte_size = fs::metadata(&target_abs)?.len() as i64;

        warnings.extend(boundary_warnings(
            &manifest.counts,
            manifest.transient_clips.len(),
        ));
        Ok(BackupReport {
            package_path: support::strip_windows_verbatim(&target_abs.to_string_lossy()),
            byte_size,
            schema_version: manifest.schema_version,
            api_version: manifest.api_version.clone(),
            core_version: manifest.core_version.clone(),
            created_at,
            blob_count: manifest.blobs.len() as i64,
            blob_bytes: manifest.blobs.iter().map(|blob| blob.byte_size).sum(),
            transient_clip_count: manifest.transient_clips.len() as i64,
            counts: manifest.counts.clone(),
            warnings,
        })
    })();

    // 中途失败：临时文件一律清掉，目标路径保持「什么都没发生」；
    // 成功也要清掉快照临时文件——包已经写完，它没有留下的理由。
    let _ = fs::remove_file(&snapshot_path);
    if outcome.is_err() {
        let _ = fs::remove_file(&partial_path);
    }
    outcome
}

/// 「目标已经存在」的统一拒绝：预检查与最终落地共用一句话。
fn target_exists_error(target: &Path) -> CoreError {
    CoreError::InvalidArgument {
        reason: format!(
            "备份目标已经存在，不能覆盖：{}（换一个文件名，或先删掉旧的那一份）",
            target.display()
        ),
    }
}

/// 把写完并自校验过的包**原子地**放到目标路径上：目标已存在就失败，绝不替换。
///
/// 为什么不用 `fs::rename`：POSIX 的 `rename(2)` 在目标存在时**静默替换**（Windows 上
/// Rust 的 `rename` 同样带替换语义）。两条并发备份因此都能通过前面的 `exists()`
/// 预检查，后落地的盖掉先落地的，而先返回的报告里的路径/大小已经指向别人的文件。
///
/// 用 `link(2)`（`fs::hard_link`）：它在目标存在时以 `AlreadyExists` 失败，是原子的。
/// 成功后删掉临时名，最终只剩目标一个名字，字节与自校验过的 partial 完全相同（同一个
/// inode，不是拷贝）。这比「先 `create_new` 占位、再删占位、再 rename」少一个窗口：
/// 占位与最终文件之间没有任何「目标可以不存在」的时刻。
///
/// 失败时把 partial 留给外层清理，目标路径保持「什么都没发生」。
fn commit_package(partial: &Path, target: &Path) -> Result<()> {
    match fs::hard_link(partial, target) {
        Ok(()) => {
            // 临时名删不掉不影响结果：目标已经是完整、自校验过的包；残留的隐藏文件
            // 不会被后续备份复用（每次都是新的 nonce 名）。
            let _ = fs::remove_file(partial);
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            Err(target_exists_error(target))
        }
        // 文件系统不支持硬链接（FAT/exFAT、部分网络盘与沙箱）时不能悄悄退回 rename：
        // 那正是「静默替换」。如实报错，并且让调用方知道包没落地——比覆盖别人的备份好。
        Err(error) => Err(CoreError::IntegrityFailed {
            reason: format!(
                "备份包已经写完并自校验通过，但没法原子地放到目标路径 {}：{error}。\
                 这个位置的文件系统可能不支持硬链接（例如 FAT/exFAT 或部分网络盘）；\
                 换一个本地目录再备份，半成品不会留在目标路径上",
                target.display()
            ),
        }),
    }
}

/// `VACUUM INTO`：SQLite 自己的**一致性导出**（不是拷文件）。
///
/// 它按读事务导出，源库在导出期间可以继续被别的连接写（WAL 下读不挡写），
/// 所以「有持续写入时创建备份」不需要给资料库加全局写锁。
fn vacuum_into(conn: &Connection, destination: &Path) -> Result<()> {
    let destination = destination
        .to_str()
        .ok_or_else(|| CoreError::InvalidArgument {
            reason: format!("备份目标路径不是合法的 UTF-8：{}", destination.display()),
        })?;
    let sql = "VACUUM INTO ?1";
    conn.execute(sql, rusqlite::params![destination])?;
    Ok(())
}

// ------------------------------------------------------------------ 快照扫描

struct BlobRef {
    sha256: String,
    object_ref: String,
    byte_size: i64,
}

struct ClipRef {
    /// 包内路径 = 它在资料库根目录下的相对路径（`staging/<recording>/<file>`）。
    archive_path: String,
    sha256: String,
    byte_size: i64,
    recording_id: String,
    segment_index: i64,
}

struct SnapshotScan {
    schema_version: i64,
    counts: BackupCounts,
    blobs: Vec<BlobRef>,
    clips: Vec<ClipRef>,
    warnings: Vec<String>,
}

/// 用只读连接读快照：行数、原件清单、未收尾录音片段清单。
fn scan_snapshot(path: &Path) -> Result<SnapshotScan> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let schema_version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;

    let counts = BackupCounts {
        captures: count_rows(&conn, "captures")?.unwrap_or(0),
        source_revisions: count_rows(&conn, "source_revisions")?.unwrap_or(0),
        assets: count_rows(&conn, "assets")?,
        blobs: count_rows(&conn, "blobs")?,
        imports_in_flight: count_in_flight_imports(&conn)?,
        open_recordings: count_open_recordings(&conn)?,
    };

    let blobs = if table_exists(&conn, "blobs")? {
        let mut stmt =
            conn.prepare("SELECT sha256, object_ref, byte_size FROM blobs ORDER BY sha256")?;
        let rows = stmt
            .query_map([], |row| {
                Ok(BlobRef {
                    sha256: row.get(0)?,
                    object_ref: row.get(1)?,
                    byte_size: row.get(2)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    } else {
        Vec::new()
    };

    // 未收尾录音的片段清单：**只列还没被最终化的会话**。
    //
    // finalize 成功只删暂存目录，`recording_segments` 的行永远留着（全仓只有登记时的
    // INSERT，没有 DELETE；级联删除只在会话行被删时触发，而 saved 会话不会被删）。
    // 所以整表扫会把每一条已 saved 录音的历史片段都当成「备份时不在暂存目录」，
    // 每次备份都谎报「有未收尾片段没随包」——而报告里的 openRecordings 是 0。
    //
    // 判定条件是「没有最终化过」，两个条件缺一不可：
    // - `state <> 'saved'`：`saved` 就是最终化的完成态；
    // - `ended_at IS NULL`：`ended_at` 只有 `recordings::finalize` 会写。带缺口最终化
    //   的结果是 `recoverable` 而不是 `saved`，但它同样已经把片段拼进逻辑音频、删掉了
    //   暂存目录，光看状态名分不出来。真崩溃留下的 `recoverable` / `interrupted`
    //   会话 `ended_at` 仍是 NULL，片段照旧随包。
    let mut clips = Vec::new();
    let mut warnings = Vec::new();
    if table_exists(&conn, "recording_segments")? {
        let mut stmt = conn.prepare(
            "SELECT s.id, s.staging_rel_path, g.segment_index, g.relative_file_name, \
             g.sha256, g.byte_size \
             FROM recording_segments g JOIN recording_sessions s ON s.id = g.recording_id \
             WHERE s.state <> 'saved' AND s.ended_at IS NULL \
             ORDER BY s.id, g.segment_index",
        )?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, i64>(5)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for (
            recording_id,
            staging_rel_path,
            segment_index,
            relative_file_name,
            sha256,
            byte_size,
        ) in rows
        {
            let archive_path = format!("{staging_rel_path}/{relative_file_name}");
            // 平台层给的片段名要能当路径用；不能就**不随包**（如实记一条警告）。
            if let Err(detail) = validate_relative_path(&archive_path) {
                warnings.push(format!(
                    "录音 {recording_id} 的第 {segment_index} 段暂存路径不合法（{detail}），没有随包"
                ));
                continue;
            }
            clips.push(ClipRef {
                archive_path,
                sha256,
                byte_size,
                recording_id,
                segment_index,
            });
        }
    }

    Ok(SnapshotScan {
        schema_version,
        counts,
        blobs,
        clips,
        warnings,
    })
}

fn count_rows(conn: &Connection, table: &str) -> Result<Option<i64>> {
    if !table_exists(conn, table)? {
        return Ok(None);
    }
    let sql = format!("SELECT COUNT(*) FROM {table}");
    Ok(Some(conn.query_row(&sql, [], |row| row.get(0))?))
}

fn count_in_flight_imports(conn: &Connection) -> Result<Option<i64>> {
    if !table_exists(conn, "import_sessions")? {
        return Ok(None);
    }
    Ok(Some(conn.query_row(
        "SELECT COUNT(*) FROM import_sessions WHERE state IN \
         ('prepared', 'copying', 'verifying', 'recoverable')",
        [],
        |row| row.get(0),
    )?))
}

fn count_open_recordings(conn: &Connection) -> Result<Option<i64>> {
    if !table_exists(conn, "recording_sessions")? {
        return Ok(None);
    }
    Ok(Some(conn.query_row(
        "SELECT COUNT(*) FROM recording_sessions WHERE state IN \
         ('preparing', 'recording', 'paused', 'stopping', 'interrupted', 'recoverable')",
        [],
        |row| row.get(0),
    )?))
}

fn table_exists(conn: &Connection, table: &str) -> Result<bool> {
    let found: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
            rusqlite::params![table],
            |row| row.get(0),
        )
        .ok();
    Ok(found.is_some())
}

// ------------------------------------------------------------------ 写包

fn write_package(
    partial: &Path,
    snapshot: &Path,
    root: &Path,
    manifest: &mut Manifest,
    gate: Option<&BackupGate>,
    warnings: &mut Vec<String>,
) -> Result<()> {
    let file = fs::File::create(partial)?;
    let mut zip = ZipWriter::new(file);
    // 数据库快照压一下（纯文本/索引压得动）；原件与片段用 Stored：媒体本来就压过，
    // 再压一遍只是白烧 CPU，而且让「按内容寻址的字节」在包里保持原样。
    let deflated = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    let stored = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);

    // 条目顺序：快照 → 原件 → 随行片段 → **清单最后**。
    //
    // 清单不是「打算装什么」的声明，而是「包里真的有什么」的记录：并发最终化会让某个
    // 片段在扫描之后、复制之前消失（见下面的循环），清单必须只列真的复制成功的那些，
    // 否则自校验会把整份备份判死（issue #95）。所以它按事实生成、写在最后。
    zip.start_file(SNAPSHOT_ENTRY, deflated)
        .map_err(zip_error("写数据库快照"))?;
    copy_into_zip(
        snapshot,
        &mut zip,
        Some(&manifest.snapshot.sha256),
        "数据库快照",
    )?;

    for blob in &manifest.blobs {
        // 源文件路径来自库里那条 blob 的 object_ref：先确认它是**内容寻址**的规范
        // 路径（`blobs/<sha 前两位>/<sha>`），再按它去复制。
        let expected_ref = crate::assets::blob_rel_path(&blob.sha256);
        if blob.path != expected_ref {
            return Err(CoreError::IntegrityFailed {
                reason: format!(
                    "库里的原件引用与内容寻址规则不一致：{} 的 object_ref 是 {}，按 sha256 应该是 {expected_ref}。\
                     这份资料库的原件存储已经坏了，先修好它再备份",
                    blob.sha256, blob.path
                ),
            });
        }
        let source = root.join(&blob.path);
        if !source.is_file() {
            return Err(CoreError::IntegrityFailed {
                reason: format!(
                    "资料库里这条原件已经不可用：{}（sha256 {}）。备份不会假装它还在——\
                     请先恢复这个文件或清理这条材料，再重新备份",
                    blob.path, blob.sha256
                ),
            });
        }
        zip.start_file(blob.path.clone(), stored)
            .map_err(zip_error("写原件"))?;
        copy_into_zip(&source, &mut zip, Some(&blob.sha256), &blob.path)?;
    }

    // 随行片段：拿不到就跳过（并发最终化会把暂存目录删掉），拿到就必须对得上
    // 登记时的 sha256——对不上说明这个录音工作区自己坏了，宁可整份备份失败。
    //
    // 复制与清单是同一份事实：只有**字节真的写进包里**的片段才进 `packaged`，
    // 然后 `packaged` 就是清单里的 `transientClips`（不多列、不少列）。
    if let Some(gate) = gate {
        // 只有测试会设它：把「并发的录音最终化」精确地安排进这个窗口。
        gate.arrive_and_wait();
    }
    let mut packaged: Vec<ManifestClip> = Vec::with_capacity(manifest.transient_clips.len());
    let mut skipped: Vec<&str> = Vec::new();
    for clip in &manifest.transient_clips {
        let source = root.join(&clip.path);
        // 先打开、打开成功才 `start_file`：文件一旦打开，并发的最终化就算把暂存目录
        // 删掉，也删不掉这份已经打开的字节（unlink 不影响已打开的 fd），不会出现
        // 「条目开了头、字节却没了」的半截条目。
        match fs::File::open(&source) {
            Ok(mut opened) => {
                if !opened.metadata()?.is_file() {
                    // 目录之类的东西：不是片段（扫描时怎么进来的，这里就怎么跳过）。
                    skipped.push(clip.path.as_str());
                    continue;
                }
                zip.start_file(clip.path.clone(), stored)
                    .map_err(zip_error("写随行片段"))?;
                copy_reader_into_zip(&mut opened, &mut zip, Some(&clip.sha256), &clip.path)?;
                packaged.push(clip.clone());
            }
            // 已经不在暂存目录里了：多半是并发的最终化/取消。跳过并记一条警告，
            // 不把「一场并发」升级成「整份备份失败」。
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                skipped.push(clip.path.as_str());
            }
            // 别的错误（权限、I/O 故障）**不装作「它不在」**：原实现是 `is_file()`
            // 过了之后在复制里 open 失败就整份失败，这里保持同一口径——读不到就
            // 如实报错，不悄悄降级成「跳过」。
            Err(err) => return Err(err.into()),
        }
    }

    if !skipped.is_empty() {
        warnings.push(format!(
            "有 {} 个未收尾录音的片段在备份时已经不在暂存目录里（多半是那段录音刚好被最终化或取消），没有随包：{}；\
             恢复出来的库里那几段录音仍然如实显示为「没收尾」，不会假装成完整录音",
            skipped.len(),
            summarize_skipped(&skipped),
        ));
    } else if !packaged.is_empty() {
        warnings.push(format!(
            "备份里有 {} 个未收尾录音的片段随包（它们不是内容寻址的原件，见架构文档）",
            packaged.len()
        ));
    }

    manifest.transient_clips = packaged;
    let manifest_json = serde_json::to_vec_pretty(manifest)?;
    zip.start_file(MANIFEST_ENTRY, deflated)
        .map_err(zip_error("写清单"))?;
    zip.write_all(&manifest_json)?;

    zip.finish().map_err(zip_error("收尾归档"))?;
    Ok(())
}

/// 警告里怎么列「没随包的片段」：路径要说得清，但不能无限长。
fn summarize_skipped(paths: &[&str]) -> String {
    const LISTED: usize = 5;
    let mut text = paths
        .iter()
        .take(LISTED)
        .copied()
        .collect::<Vec<_>>()
        .join("、");
    if paths.len() > LISTED {
        text.push_str(&format!(" 等 {} 个", paths.len()));
    }
    text
}

fn zip_error(what: &'static str) -> impl Fn(zip::result::ZipError) -> CoreError {
    move |err| CoreError::IntegrityFailed {
        reason: format!("{what}失败：{err}"),
    }
}

/// 把 `source` 流式写进 zip，同时重算 sha256 并与期望值比对。
fn copy_into_zip<W: Write + std::io::Seek>(
    source: &Path,
    zip: &mut ZipWriter<W>,
    expected_sha: Option<&str>,
    label: &str,
) -> Result<()> {
    let mut input = fs::File::open(source)?;
    copy_reader_into_zip(&mut input, zip, expected_sha, label)
}

/// 同上，但从**已经打开的**读端复制。
///
/// 随行片段走这一条：先把文件打开、确认它是个普通文件，再决定要不要写它——
/// 于是「决定随包的片段」与「真的写进包的字节」是同一份事实。
fn copy_reader_into_zip<R: Read, W: Write + std::io::Seek>(
    input: &mut R,
    zip: &mut ZipWriter<W>,
    expected_sha: Option<&str>,
    label: &str,
) -> Result<()> {
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; BUFFER_BYTES];
    loop {
        let read = input.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        zip.write_all(&buffer[..read])?;
    }
    let actual = format!("{:x}", hasher.finalize());
    if let Some(expected) = expected_sha {
        if !actual.eq_ignore_ascii_case(expected) {
            return Err(CoreError::IntegrityFailed {
                reason: format!(
                    "{label}的内容与资料库记录的不一致：实际 sha256 {actual}，记录的是 {expected}"
                ),
            });
        }
    }
    Ok(())
}

/// 自校验用：把写好的包改坏一个字节。
fn corrupt_written_package(path: &Path) -> Result<()> {
    let mut bytes = fs::read(path)?;
    if bytes.is_empty() {
        return Ok(());
    }
    let index = bytes.len() / 2;
    bytes[index] ^= 0xff;
    fs::write(path, bytes)?;
    Ok(())
}

// ------------------------------------------------------------------ 读包与校验

/// 清单条目在包里的角色。恢复时报告要按角色分类，拒绝时的文案也要分得清。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EntryKind {
    Snapshot,
    Blob,
    TransientClip,
}

impl EntryKind {
    fn label(self) -> &'static str {
        match self {
            Self::Snapshot => "快照",
            Self::Blob => "原件",
            Self::TransientClip => "随行片段",
        }
    }
}

impl PlannedEntry {
    /// 这条条目在**恢复出来的资料库**里的相对路径。
    ///
    /// 快照在包里固定放在 `db/library.sqlite`（包格式稳定），但落地时必须放在资料库
    /// 根目录：`Core::open` 用资料库文件所在目录当原件根目录，放到 `db/` 下面会让
    /// 所有原件都找不到。
    ///
    /// 正因为快照的包内路径与恢复路径不同，恢复路径的**唯一性**必须单独校验（按这个
    /// 函数的返回值去重，见 [`read_plan`]）：否则一条 `path = library.sqlite` 的随行
    /// 片段会在解包时整份覆盖已核过哈希的快照。
    fn restored_rel_path(&self) -> &str {
        match self.kind {
            EntryKind::Snapshot => RESTORED_LIBRARY_FILE,
            EntryKind::Blob | EntryKind::TransientClip => &self.path,
        }
    }
}

struct PlannedEntry {
    path: String,
    sha256: String,
    byte_size: i64,
    kind: EntryKind,
}

/// 读清单 + 校验路径与条目集合之后的「包计划」。
struct PackagePlan {
    manifest: Manifest,
    /// 快照 + 原件 + 随行片段，按解包顺序。
    entries: Vec<PlannedEntry>,
    /// 结构性问题（越界路径、条目集合不一致、版本不兼容）。空表示结构上可用。
    issues: Vec<String>,
    /// 本构建能不能恢复它（只看版本）。
    compatible: bool,
    created_at: DateTime<Utc>,
}

/// 读清单：**不写任何东西**，只做结构、路径与版本检查。
fn read_plan(package: &Path) -> Result<PackagePlan> {
    let file = fs::File::open(package).map_err(|err| CoreError::NotFound {
        entity: "备份包",
        id: format!("{}（{err}）", package.display()),
    })?;
    let mut archive = ZipArchive::new(file).map_err(|err| {
        corrupted(format!(
            "打不开这个备份包（不是有效的 zip 归档）：{err}。请换一份备份"
        ))
    })?;

    let manifest_bytes = read_entry(&mut archive, MANIFEST_ENTRY).map_err(|err| {
        corrupted(format!(
            "备份包里没有可读的清单 {MANIFEST_ENTRY}：{err}。请换一份备份"
        ))
    })?;
    let manifest: Manifest = serde_json::from_slice(&manifest_bytes)
        .map_err(|err| corrupted(format!("备份包的清单解析不了：{err}。请换一份备份")))?;

    if manifest.format != BACKUP_FORMAT {
        return Err(corrupted(format!(
            "这不是本应用的备份包：format 是 {}，期望 {BACKUP_FORMAT}",
            manifest.format
        )));
    }
    if manifest.format_version > BACKUP_FORMAT_VERSION {
        return Err(CoreError::IncompatibleBackup {
            reason: format!(
                "备份包格式版本是 {}，本核心只认识 {BACKUP_FORMAT_VERSION}；请先升级应用再恢复",
                manifest.format_version
            ),
        });
    }

    let mut issues = Vec::new();

    // ---- 路径：清单是唯一可信的路径来源，逐条按最严的规则过一遍。
    if manifest.snapshot.path == MANIFEST_ENTRY {
        issues.push("corrupted: 快照条目路径与清单自身冲突".to_owned());
    }
    if let Err(detail) = validate_relative_path(&manifest.snapshot.path) {
        issues.push(format!(
            "unsafe_path: 快照条目 {} 不能当路径用（{detail}）",
            manifest.snapshot.path
        ));
    } else if manifest.snapshot.path != SNAPSHOT_ENTRY {
        issues.push(format!(
            "corrupted: 快照条目路径是 {}，本核心只认识 {SNAPSHOT_ENTRY}",
            manifest.snapshot.path
        ));
    }

    let mut entries = Vec::with_capacity(manifest.blobs.len() + manifest.transient_clips.len() + 1);
    entries.push(PlannedEntry {
        path: manifest.snapshot.path.clone(),
        sha256: manifest.snapshot.sha256.clone(),
        byte_size: manifest.snapshot.byte_size,
        kind: EntryKind::Snapshot,
    });
    if !is_sha256(&manifest.snapshot.sha256) {
        issues.push(format!(
            "corrupted: 快照的 sha256 不是 64 位十六进制：{}",
            manifest.snapshot.sha256
        ));
    }

    let mut seen = std::collections::HashSet::new();
    seen.insert(MANIFEST_ENTRY.to_owned());
    seen.insert(manifest.snapshot.path.clone());
    for blob in &manifest.blobs {
        if let Err(detail) = validate_relative_path(&blob.path) {
            issues.push(format!(
                "unsafe_path: 原件条目 {} 不能当路径用（{detail}）",
                blob.path
            ));
        } else if !is_sha256(&blob.sha256) {
            issues.push(format!(
                "corrupted: 原件 {blob_path} 的 sha256 不是 64 位十六进制：{sha}",
                blob_path = blob.path,
                sha = blob.sha256
            ));
        } else {
            // 内容寻址：路径必须由 sha256 推得出来。对不上就是清单被改过。
            let expected = crate::assets::blob_rel_path(&blob.sha256);
            if blob.path != expected {
                issues.push(format!(
                    "corrupted: 原件路径与内容寻址规则不一致：{} 按 sha256 应该是 {expected}",
                    blob.path
                ));
            }
        }
        if !seen.insert(blob.path.clone()) {
            issues.push(format!("corrupted: 清单里的路径重复：{}", blob.path));
        }
        entries.push(PlannedEntry {
            path: blob.path.clone(),
            sha256: blob.sha256.clone(),
            byte_size: blob.byte_size,
            kind: EntryKind::Blob,
        });
    }
    for clip in &manifest.transient_clips {
        if let Err(detail) = validate_relative_path(&clip.path) {
            issues.push(format!(
                "unsafe_path: 随行片段条目 {} 不能当路径用（{detail}）",
                clip.path
            ));
        }
        if !is_sha256(&clip.sha256) {
            issues.push(format!(
                "corrupted: 随行片段 {} 的 sha256 不是 64 位十六进制：{}",
                clip.path, clip.sha256
            ));
        }
        if !seen.insert(clip.path.clone()) {
            issues.push(format!("corrupted: 清单里的路径重复：{}", clip.path));
        }
        entries.push(PlannedEntry {
            path: clip.path.clone(),
            sha256: clip.sha256.clone(),
            byte_size: clip.byte_size,
            kind: EntryKind::TransientClip,
        });
    }
    for entry in &entries {
        if entry.byte_size < 0 {
            issues.push(format!(
                "corrupted: {}条目 {} 的 byteSize 是负数",
                entry.kind.label(),
                entry.path
            ));
        }
    }

    // ---- 恢复路径必须唯一：解包时写的是 `restored_rel_path()`，不是包内 `path`。
    //
    // 快照在包里固定叫 `db/library.sqlite`，落地却叫 `library.sqlite`；原件/随行片段
    // 落地沿用自己的包内路径。只按包内路径去重会漏掉「`db/library.sqlite` 与
    // `library.sqlite` 不算冲突」这一类：解包按条目顺序用 `File::create` 落到同一个
    // `staging/library.sqlite`，后写的条目整份覆盖已核过哈希的快照，而每一条自己的
    // 清单哈希都验得过（`inspect` 全绿）。所以在**恢复路径**上再查一次重。
    let mut restored_seen: std::collections::HashMap<&str, (&EntryKind, &str)> =
        std::collections::HashMap::new();
    for entry in &entries {
        let restored = entry.restored_rel_path();
        if let Some((first_kind, first_path)) = restored_seen.get(restored) {
            issues.push(format!(
                "corrupted: {}条目 {} 的恢复路径 {restored} 与{}条目 {first_path} 重复\
                 （恢复路径必须唯一，绝不覆盖已校验的条目）",
                entry.kind.label(),
                entry.path,
                first_kind.label()
            ));
        } else {
            restored_seen.insert(restored, (&entry.kind, &entry.path));
        }
    }

    // ---- 清单与内容一致：归档里的条目集合必须与清单完全吻合。
    let archive_names: Vec<String> = archive.file_names().map(str::to_owned).collect();
    let declared: std::collections::HashSet<&str> = entries
        .iter()
        .map(|entry| entry.path.as_str())
        .chain(std::iter::once(MANIFEST_ENTRY))
        .collect();
    for name in &archive_names {
        if !declared.contains(name.as_str()) {
            issues.push(format!(
                "corrupted: 包里有清单没声明的条目 {name}（拒绝解包，绝不写到清单之外的位置）"
            ));
        }
    }
    for entry in &entries {
        if !archive_names.iter().any(|name| name == &entry.path) {
            issues.push(format!("corrupted: 清单声明的条目不在包里：{}", entry.path));
        }
    }

    // ---- 计数：清单自己说的原件数要和清单条目数一致。
    if let Some(count) = manifest.counts.blobs {
        if count != manifest.blobs.len() as i64 {
            issues.push(format!(
                "corrupted: 清单说快照里有 {count} 条原件，但清单只列了 {} 条",
                manifest.blobs.len()
            ));
        }
    }
    if manifest.schema_version < 0 {
        issues.push(format!(
            "corrupted: 清单里的 schemaVersion 是负数：{}",
            manifest.schema_version
        ));
    }

    // ---- 版本兼容：只拒**高于**本构建的；更老的备份由开库时的迁移接手。
    let mut compatible = true;
    if manifest.schema_version > crate::SCHEMA_VERSION {
        compatible = false;
        issues.push(format!(
            "incompatible: 备份的 schemaVersion 是 {}，本核心最高支持 {}；请先升级应用再恢复",
            manifest.schema_version,
            crate::SCHEMA_VERSION
        ));
    }
    match parse_api_version(&manifest.api_version) {
        None => issues.push(format!(
            "corrupted: 清单里的 apiVersion 不是 major.minor 形式：{}",
            manifest.api_version
        )),
        Some((major, minor)) => {
            let (supported_major, supported_minor) = parse_api_version(crate::API_VERSION)
                .expect("核心自己的 API 版本一定是 major.minor");
            if major > supported_major || (major == supported_major && minor > supported_minor) {
                compatible = false;
                issues.push(format!(
                    "incompatible: 备份的 apiVersion 是 {}，本核心支持到 {}.{}；请先升级应用再恢复",
                    manifest.api_version, supported_major, supported_minor
                ));
            }
        }
    }

    let created_at = support::parse_iso(&manifest.created_at).map_err(|_| {
        corrupted(format!(
            "清单里的 createdAt 不是合法的 ISO 时间：{}",
            manifest.created_at
        ))
    })?;

    Ok(PackagePlan {
        manifest,
        entries,
        issues,
        compatible,
        created_at,
    })
}

/// 拿第一条问题的种类翻成错误码：越界路径与清单不一致都是「这个包不能用」。
fn issue_to_error(issue: &str) -> CoreError {
    let reason = issue
        .split_once(':')
        .map_or(issue, |(_, detail)| detail)
        .trim()
        .to_owned();
    if issue.starts_with("incompatible:") {
        CoreError::IncompatibleBackup { reason }
    } else {
        CoreError::CorruptedBackup { reason }
    }
}

/// 给恢复过程中的失败补一句「现库没被改」。
///
/// 恢复只写自己的暂存目录与目标目录；无论是包坏了、版本不兼容、目标不合法还是
/// 落盘失败，当前资料库都不在写入范围内。这句话是给用户看的承诺，所以写在错误里。
fn annotate_restore_failure(error: CoreError) -> CoreError {
    const NOTE: &str = "已拒绝恢复；当前资料库没有改动，目标目录保持原样";
    match error {
        CoreError::CorruptedBackup { reason } => CoreError::CorruptedBackup {
            reason: format!("{reason}。{NOTE}"),
        },
        CoreError::IncompatibleBackup { reason } => CoreError::IncompatibleBackup {
            reason: format!("{reason}。{NOTE}"),
        },
        CoreError::IntegrityFailed { reason } => CoreError::IntegrityFailed {
            reason: format!("{reason}。{NOTE}"),
        },
        CoreError::InvalidState { entity, id, state } => CoreError::InvalidState {
            entity,
            id,
            state: format!("{state}。{NOTE}"),
        },
        other => other,
    }
}

/// 逐条目核哈希；`sink` 有值时同时把它们解包到那个目录里。
///
/// **只按清单声明的路径写**：归档里别的名字根本不会被解包，越界路径在
/// [`validate_relative_path`] 那一关就被挡住。
fn verify_entries(package: &Path, plan: &PackagePlan, sink: Option<&Path>) -> Result<()> {
    let file = fs::File::open(package)?;
    let mut archive =
        ZipArchive::new(file).map_err(|err| corrupted(format!("重新打开备份包失败：{err}")))?;

    for entry in &plan.entries {
        // 防御性再验一次：解包路径永远不信任清单以外的第二来源。
        validate_relative_path(&entry.path)
            .map_err(|detail| corrupted(format!("拒绝解包越界路径 {}：{detail}", entry.path)))?;
        let mut zipped = archive.by_name(&entry.path).map_err(|err| {
            corrupted(format!(
                "包里没有清单声明的{}条目 {}：{err}",
                entry.kind.label(),
                entry.path
            ))
        })?;
        if zipped.is_dir() {
            return Err(corrupted(format!("条目 {} 是目录，不是文件", entry.path)));
        }
        if zipped.is_symlink() {
            return Err(corrupted(format!(
                "条目 {} 是符号链接：备份包只允许普通文件，拒绝解包",
                entry.path
            )));
        }
        if zipped.size() != entry.byte_size as u64 {
            return Err(corrupted(format!(
                "条目 {} 的大小是 {} 字节，清单声明 {} 字节",
                entry.path,
                zipped.size(),
                entry.byte_size
            )));
        }

        let mut output = match sink {
            Some(base) => {
                let destination = safe_join(base, entry.restored_rel_path())?;
                if let Some(parent) = destination.parent() {
                    fs::create_dir_all(parent)?;
                }
                // **独占创建**，不是 `File::create`：解包目录是全新的，正常不会有同名
                // 文件；真撞上就说明两条条目要落到同一个恢复路径（清单层已经拒绝，
                // 这里是第二道闸）。宁可报错，也绝不覆盖刚写下去、刚核过哈希的字节。
                match fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&destination)
                {
                    Ok(file) => Some(file),
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                        return Err(corrupted(format!(
                            "解包{}条目 {} 时恢复路径 {} 已经被另一条条目占用：\
                             恢复路径必须唯一，拒绝覆盖已校验的字节",
                            entry.kind.label(),
                            entry.path,
                            entry.restored_rel_path()
                        )))
                    }
                    Err(error) => return Err(error.into()),
                }
            }
            None => None,
        };

        let mut hasher = Sha256::new();
        let mut buffer = vec![0_u8; BUFFER_BYTES];
        loop {
            let read = zipped
                .read(&mut buffer)
                .map_err(|err| corrupted(format!("读条目 {} 失败：{err}", entry.path)))?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
            if let Some(output) = output.as_mut() {
                output.write_all(&buffer[..read])?;
            }
        }
        if let Some(output) = output.as_mut() {
            output.flush()?;
        }
        let actual = format!("{:x}", hasher.finalize());
        if !actual.eq_ignore_ascii_case(&entry.sha256) {
            return Err(corrupted(format!(
                "条目 {} 的 sha256 是 {actual}，清单声明 {}",
                entry.path, entry.sha256
            )));
        }
    }
    Ok(())
}

/// 备份写完后的自校验：重开包，逐条目重算哈希、比大小、比条目集合。
fn verify_package(package: &Path, manifest: &Manifest) -> Result<()> {
    let file = fs::File::open(package)?;
    let mut archive =
        ZipArchive::new(file).map_err(|err| corrupted(format!("写好的包打不开：{err}")))?;

    let archive_names: Vec<String> = archive.file_names().map(str::to_owned).collect();
    let mut expected: Vec<(&str, &str, i64)> = Vec::new();
    expected.push((
        manifest.snapshot.path.as_str(),
        manifest.snapshot.sha256.as_str(),
        manifest.snapshot.byte_size,
    ));
    for blob in &manifest.blobs {
        expected.push((blob.path.as_str(), blob.sha256.as_str(), blob.byte_size));
    }
    for clip in &manifest.transient_clips {
        expected.push((clip.path.as_str(), clip.sha256.as_str(), clip.byte_size));
    }
    if archive_names.len() != expected.len() + 1 {
        return Err(corrupted(format!(
            "包里的条目数是 {}，应该是 {}（清单 1 + 快照 1 + 原件 {} + 随行片段 {}）",
            archive_names.len(),
            expected.len() + 1,
            manifest.blobs.len(),
            manifest.transient_clips.len()
        )));
    }
    for (path, sha256, byte_size) in expected {
        let mut zipped = archive
            .by_name(path)
            .map_err(|err| corrupted(format!("包里的条目 {path} 读不到：{err}")))?;
        if zipped.size() != byte_size as u64 {
            return Err(corrupted(format!(
                "条目 {path} 自校验时大小是 {}，清单声明 {byte_size}",
                zipped.size()
            )));
        }
        let mut hasher = Sha256::new();
        let mut buffer = vec![0_u8; BUFFER_BYTES];
        loop {
            let read = zipped
                .read(&mut buffer)
                .map_err(|err| corrupted(format!("自校验读条目 {path} 失败：{err}")))?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
        }
        let actual = format!("{:x}", hasher.finalize());
        if !actual.eq_ignore_ascii_case(sha256) {
            return Err(corrupted(format!(
                "条目 {path} 自校验的 sha256 是 {actual}，清单声明 {sha256}"
            )));
        }
    }
    Ok(())
}

// ------------------------------------------------------------------ 检查备份

/// 检查备份包：结构、路径、版本、逐条目哈希。**不写任何东西**。
pub(crate) fn inspect(package: &Path) -> Result<BackupInspection> {
    if !package.is_file() {
        return Err(CoreError::NotFound {
            entity: "备份包",
            id: package.display().to_string(),
        });
    }
    let byte_size = fs::metadata(package)?.len() as i64;
    let plan = read_plan(package)?;
    let mut issues = plan.issues.clone();

    // 结构没问题（或只是版本更高）才值得花时间核哈希；越界路径这类问题一票否决。
    let structural_ok = issues
        .iter()
        .all(|issue| issue.starts_with("incompatible:"));
    let mut hashes_verified = false;
    if structural_ok {
        match verify_entries(package, &plan, None) {
            Ok(()) => hashes_verified = true,
            Err(error) => issues.push(format!("corrupted: {error}")),
        }
    }

    Ok(BackupInspection {
        package_path: support::strip_windows_verbatim(&package.to_string_lossy()),
        format_version: plan.manifest.format_version,
        schema_version: plan.manifest.schema_version,
        api_version: plan.manifest.api_version.clone(),
        core_version: plan.manifest.core_version.clone(),
        created_at: plan.created_at,
        byte_size,
        blob_count: plan.manifest.blobs.len() as i64,
        blob_bytes: plan.manifest.blobs.iter().map(|blob| blob.byte_size).sum(),
        asset_count: plan.manifest.counts.assets.unwrap_or(0),
        transient_clip_count: plan.manifest.transient_clips.len() as i64,
        counts: plan.manifest.counts.clone(),
        issues,
        compatible: plan.compatible,
        hashes_verified,
    })
}

// ------------------------------------------------------------------ 恢复

/// 恢复到**新的空目录**：全部校验通过才落地，当前资料库一字不改。
pub(crate) fn restore(core: &Core, package: &Path, target_dir: &Path) -> Result<RestoreReport> {
    // 所有失败都补一句「现库没被改」：恢复的写入范围只有暂存目录与目标目录。
    restore_inner(core, package, target_dir).map_err(annotate_restore_failure)
}

fn restore_inner(core: &Core, package: &Path, target_dir: &Path) -> Result<RestoreReport> {
    if !package.is_file() {
        return Err(CoreError::NotFound {
            entity: "备份包",
            id: package.display().to_string(),
        });
    }
    // 1. 读清单（不写任何东西）：结构、路径、版本有任何一条不过就到此为止。
    let plan = read_plan(package)?;
    if let Some(issue) = plan.issues.first() {
        return Err(issue_to_error(issue));
    }
    let byte_size = fs::metadata(package)?.len() as i64;

    // 2. 目标目录检查（也在写任何东西之前）：只允许新的空目录，且不能是当前资料库。
    let target_abs = prepare_target(target_dir, core.root.as_deref())?;
    let parent = target_abs
        .parent()
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
    let nonce = support::new_id("restore");
    let staging = parent.join(format!(".{nonce}.restore"));
    fs::create_dir_all(&staging)?;

    let outcome = (|| -> Result<RestoreReport> {
        // 3. 解包：逐条目核大小与 sha256，全部落在暂存目录里。
        verify_entries(package, &plan, Some(&staging))?;

        // 4. 打开恢复出来的库：完整性、外键、版本、关键行数、原件交叉核对。
        let library = staging.join(RESTORED_LIBRARY_FILE);
        if !library.is_file() {
            return Err(corrupted(format!(
                "解包之后没有找到资料库文件 {RESTORED_LIBRARY_FILE}"
            )));
        }
        verify_restored_library(&library, &plan.manifest)?;

        // 5. 落地：单次 rename；到这一步之前目标目录一直是原样（不存在或空）。
        commit_staging(&staging, &target_abs)?;

        let mut warnings =
            boundary_warnings(&plan.manifest.counts, plan.manifest.transient_clips.len());
        warnings.push(
            "恢复出来的库与备份里的快照**逐字节相同**：迁移与恢复态修正留到第一次 Core::open（契约既有的 recover_on_open 语义）"
                .to_owned(),
        );
        Ok(RestoreReport {
            library_path: support::strip_windows_verbatim(
                &target_abs.join(RESTORED_LIBRARY_FILE).to_string_lossy(),
            ),
            root_dir: support::strip_windows_verbatim(&target_abs.to_string_lossy()),
            schema_version: plan.manifest.schema_version,
            api_version: plan.manifest.api_version.clone(),
            core_version: plan.manifest.core_version.clone(),
            created_at: plan.created_at,
            byte_size,
            blob_count: plan.manifest.blobs.len() as i64,
            blob_bytes: plan.manifest.blobs.iter().map(|blob| blob.byte_size).sum(),
            transient_clip_count: plan.manifest.transient_clips.len() as i64,
            counts: plan.manifest.counts.clone(),
            warnings,
        })
    })();

    if outcome.is_err() {
        // 失败：只清掉自己的暂存目录。目标目录与当前资料库都没被碰过。
        let _ = fs::remove_dir_all(&staging);
    }
    outcome
}

/// 目标目录必须是**新的空目录**，且不能就是当前资料库所在目录。
fn prepare_target(target_dir: &Path, library_root: Option<&Path>) -> Result<PathBuf> {
    if target_dir.as_os_str().is_empty() {
        return Err(CoreError::InvalidArgument {
            reason: "恢复目标目录不能为空".to_owned(),
        });
    }
    if target_dir.exists() {
        let metadata = fs::symlink_metadata(target_dir)?;
        if !metadata.is_dir() {
            return Err(CoreError::InvalidState {
                entity: "恢复目标",
                id: target_dir.display().to_string(),
                state: "不是目录".to_owned(),
            });
        }
        if fs::read_dir(target_dir)?.next().is_some() {
            return Err(CoreError::InvalidState {
                entity: "恢复目标",
                id: target_dir.display().to_string(),
                state: "不是空目录；恢复只允许落到新的空目录，绝不覆盖已有资料库".to_owned(),
            });
        }
    }

    let parent = target_dir
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
    fs::create_dir_all(&parent)?;
    let parent = fs::canonicalize(&parent)?;
    let name = target_dir
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .ok_or_else(|| CoreError::InvalidArgument {
            reason: format!("恢复目标不是目录名：{}", target_dir.display()),
        })?;
    let target_abs = parent.join(&name);

    if let Some(root) = library_root {
        let root_abs = fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
        if target_abs == root_abs {
            return Err(CoreError::InvalidState {
                entity: "恢复目标",
                id: target_abs.display().to_string(),
                state: "就是当前资料库所在目录；恢复只允许落到新的空目录".to_owned(),
            });
        }
        // 当前资料库文件在目标里/目标在它里面，都算「会碰到现库」，一律拒绝。
        if target_abs.join(RESTORED_LIBRARY_FILE).exists() {
            return Err(CoreError::InvalidState {
                entity: "恢复目标",
                id: target_abs.display().to_string(),
                state: "里面已经有资料库文件；恢复只允许落到新的空目录".to_owned(),
            });
        }
    }
    Ok(target_abs)
}

/// 单次 rename 落地。目标原本若是个空目录，先把它删掉，再 rename。
fn commit_staging(staging: &Path, target: &Path) -> Result<()> {
    let existed = target.exists();
    if existed {
        fs::remove_dir(target).map_err(|err| CoreError::InvalidState {
            entity: "恢复目标",
            id: target.display().to_string(),
            state: format!("没法清掉这个空目录：{err}"),
        })?;
    }
    fs::rename(staging, target).map_err(|err| {
        if existed {
            // 尽量把它还原成「原来的空目录」，目标不该因为一次失败变样。
            let _ = fs::create_dir(target);
        }
        CoreError::IntegrityFailed {
            reason: format!(
                "恢复内容没法落到目标目录 {}：{err}（暂存内容已清理，目标目录保持原样）",
                target.display()
            ),
        }
    })
}

/// 最终落地的库文件必须**逐字节**等于清单里的快照（大小 + sha256）。
///
/// 这是「恢复出来的库与备份里的快照逐字节相同」这句话的唯一依据：只看每条条目
/// 自己的哈希不够，因为覆盖发生在**写文件**这一步（两条条目共用恢复路径时）。
fn verify_library_matches_snapshot(library: &Path, snapshot: &ManifestFile) -> Result<()> {
    let actual_bytes = fs::metadata(library)?.len() as i64;
    if actual_bytes != snapshot.byte_size {
        return Err(corrupted(format!(
            "恢复出来的库是 {actual_bytes} 字节，清单里的快照是 {} 字节",
            snapshot.byte_size
        )));
    }
    let actual = hash_file_hex(library)?;
    if !actual.eq_ignore_ascii_case(&snapshot.sha256) {
        return Err(corrupted(format!(
            "恢复出来的库与备份里的快照不是逐字节相同：实际 sha256 {actual}，清单声明 {}。\
             这个包的条目在解包时落到过同一个路径、互相覆盖，拒绝把它当成恢复成功",
            snapshot.sha256
        )));
    }
    Ok(())
}

/// 恢复出来的库：**逐字节等于快照** + 完整性 + 外键 + 版本 + 关键行数 + 原件交叉核对。
fn verify_restored_library(library: &Path, manifest: &Manifest) -> Result<()> {
    // 第一件事：最终落地的那个文件，字节必须等于清单里的快照。
    //
    // 前面已经逐条目验过哈希，但那验的是「条目自己的字节」；如果两条条目落到同一个
    // 恢复路径，后写的会覆盖先写的，而两者的清单哈希都验得过。报告里「逐字节相同」
    // 这句承诺必须由**最终文件**的哈希来兑现，不能靠「每一段都验过」推断。
    verify_library_matches_snapshot(library, &manifest.snapshot)?;

    let conn = Connection::open_with_flags(library, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|err| corrupted(format!("恢复出来的资料库打不开：{err}")))?;

    let integrity: String = conn.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
    if integrity != "ok" {
        return Err(corrupted(format!(
            "恢复出来的资料库完整性检查没过：{integrity}"
        )));
    }
    let mut fk = conn.prepare("PRAGMA foreign_key_check")?;
    if fk.exists([])? {
        return Err(corrupted(
            "恢复出来的资料库有外键不一致的行（PRAGMA foreign_key_check 非空）".to_owned(),
        ));
    }

    let schema_version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if schema_version != manifest.schema_version {
        return Err(corrupted(format!(
            "快照的 schemaVersion 是 {schema_version}，清单声明 {}",
            manifest.schema_version
        )));
    }

    // 关键行数逐项核对：清单是快照那一刻的事实，恢复出来的库必须一模一样。
    let expected = &manifest.counts;
    check_count(&conn, "captures", Some(expected.captures))?;
    check_count(&conn, "source_revisions", Some(expected.source_revisions))?;
    check_count(&conn, "assets", expected.assets)?;
    check_count(&conn, "blobs", expected.blobs)?;

    // 原件交叉核对（两个方向都要对）：库里每条 blob 都要在清单里有对应条目，
    // 清单里每条原件也都必须被库引用——否则「清单与内容一致」就是空话。
    let mut rows = if table_exists(&conn, "blobs")? {
        let mut stmt = conn.prepare("SELECT sha256, object_ref, byte_size FROM blobs")?;
        let collected = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        collected
    } else {
        Vec::new()
    };
    rows.sort();
    if rows.len() != manifest.blobs.len() {
        return Err(corrupted(format!(
            "恢复出来的库里有 {} 条原件，清单列了 {} 条",
            rows.len(),
            manifest.blobs.len()
        )));
    }
    let mut declared: Vec<(String, String, i64)> = manifest
        .blobs
        .iter()
        .map(|blob| (blob.sha256.clone(), blob.path.clone(), blob.byte_size))
        .collect();
    declared.sort();
    if rows != declared {
        return Err(corrupted(
            "恢复出来的库引用的原件与清单条目对不上（sha256 / 路径 / 大小有差异）".to_owned(),
        ));
    }

    // 资产指到不存在的原件：这是「库里有记录、磁盘上没文件」的另一种写法，必须拦住。
    if table_exists(&conn, "assets")? && table_exists(&conn, "blobs")? {
        let dangling: i64 = conn.query_row(
            "SELECT COUNT(*) FROM assets a LEFT JOIN blobs b ON a.sha256 = b.sha256 \
             WHERE b.sha256 IS NULL",
            [],
            |row| row.get(0),
        )?;
        if dangling != 0 {
            return Err(corrupted(format!(
                "恢复出来的库里有 {dangling} 条资产指向不存在的原件"
            )));
        }
        let missing_ref: i64 = conn.query_row(
            "SELECT COUNT(*) FROM assets WHERE object_ref NOT IN (SELECT object_ref FROM blobs)",
            [],
            |row| row.get(0),
        )?;
        if missing_ref != 0 {
            return Err(corrupted(format!(
                "恢复出来的库里有 {missing_ref} 条资产的 object_ref 不在原件清单里"
            )));
        }
    }

    // 修订指到不存在的资产（`source_revisions.asset_id` 没有外键约束，得自己查）。
    if table_exists(&conn, "assets")? {
        let dangling_revisions: i64 = conn.query_row(
            "SELECT COUNT(*) FROM source_revisions r LEFT JOIN assets a ON r.asset_id = a.id \
             WHERE r.asset_id IS NOT NULL AND a.id IS NULL",
            [],
            |row| row.get(0),
        )?;
        if dangling_revisions != 0 {
            return Err(corrupted(format!(
                "恢复出来的库里有 {dangling_revisions} 条修订指向不存在的资产"
            )));
        }
    }
    Ok(())
}

fn check_count(conn: &Connection, table: &str, expected: Option<i64>) -> Result<()> {
    let Some(expected) = expected else {
        return Ok(());
    };
    let Some(actual) = count_rows(conn, table)? else {
        return Err(corrupted(format!(
            "清单里有 {table} 的行数，但恢复出来的库里没有这张表"
        )));
    };
    if actual != expected {
        return Err(corrupted(format!(
            "恢复出来的库 {table} 有 {actual} 行，清单声明 {expected} 行"
        )));
    }
    Ok(())
}

/// 如实告诉调用方：恢复出来的库里有哪些「还没做完」的东西。
fn boundary_warnings(counts: &BackupCounts, clips: usize) -> Vec<String> {
    let mut warnings = Vec::new();
    if let Some(imports) = counts.imports_in_flight {
        if imports > 0 {
            warnings.push(format!(
                "备份里有 {imports} 个没有完成的导入：恢复出来的库会**如实**保持「未完成」\
                 （第一次打开时按既有 recover_on_open 语义标成 recoverable），不会假装 ready；\
                 这些导入的部分字节不在备份里，只能取消后重新导入"
            ));
        }
    }
    if let Some(recordings) = counts.open_recordings {
        if recordings > 0 {
            warnings.push(format!(
                "备份里有 {recordings} 段没收尾的录音：恢复出来的库如实显示为未完成，\
                 不会假装成已保存的录音（随包片段 {clips} 个）"
            ));
        }
    }
    warnings
}

// ------------------------------------------------------------------ 路径与哈希工具

/// 严格校验「包内相对路径」——恢复的每一步都只按这种路径写文件。
///
/// 拒绝：空路径、绝对路径、`..`、`.`、空段、反斜杠、冒号（Windows 盘符/备用流）、
/// NUL。**注意**这里的口径比 `Path::components` 更严：`a/./b` 这种「其实不越界」的
/// 写法也拒——备份包是核心自己写的，出现这种写法只可能说明它被改过。
fn validate_relative_path(raw: &str) -> std::result::Result<(), String> {
    if raw.is_empty() {
        return Err("空路径".to_owned());
    }
    if raw.contains('\0') {
        return Err("含 NUL 字节".to_owned());
    }
    if raw.contains('\\') {
        return Err("含反斜杠（Windows 上会被当成目录分隔符）".to_owned());
    }
    if raw.contains(':') {
        return Err("含冒号（可能是盘符或备用数据流）".to_owned());
    }
    if raw.starts_with('/') {
        return Err("是绝对路径".to_owned());
    }
    for part in raw.split('/') {
        if part.is_empty() {
            return Err("有空路径段（开头/结尾/连续的分隔符）".to_owned());
        }
        if part == "." || part == ".." {
            return Err(format!("含越界路径段 `{part}`"));
        }
    }
    Ok(())
}

/// 把包内相对路径拼到基目录下，**拼之前先过校验**。
fn safe_join(base: &Path, relative: &str) -> Result<PathBuf> {
    validate_relative_path(relative).map_err(|detail| CoreError::CorruptedBackup {
        reason: format!("拒绝解包越界路径 {relative}：{detail}"),
    })?;
    Ok(base.join(relative))
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn parse_api_version(value: &str) -> Option<(u32, u32)> {
    let (major, minor) = value.split_once('.')?;
    Some((major.parse().ok()?, minor.parse().ok()?))
}

fn hash_file_hex(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; BUFFER_BYTES];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn corrupted(reason: String) -> CoreError {
    CoreError::CorruptedBackup { reason }
}

fn read_entry(
    archive: &mut ZipArchive<fs::File>,
    name: &str,
) -> std::result::Result<Vec<u8>, String> {
    let mut file = archive.by_name(name).map_err(|err| err.to_string())?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|err| err.to_string())?;
    Ok(bytes)
}

/// 写一条**已完成**的备份/恢复任务记录。
///
/// 契约的 `backups.create` / `backups.restore` 返回 `jobId`，而核心的这两个入口是
/// 同步的。这里的取舍与 `exports.create` 一致：同步做完之后写一条 `state = succeeded`
/// 的任务行，返回的 `jobId` 指向这条**已经完成**的记录，不是排队任务。这样
/// `jobs.get` 读得到、进度轮询不会挂住，也没有引入一个空壳执行器。
///
/// `target_ids` 记真实产物路径（备份包 / 恢复出来的库），排查时不用再猜文件在哪。
pub(crate) fn record_completed_job(
    core: &mut Core,
    kind: &str,
    target_ids: Vec<String>,
) -> Result<Job> {
    let job = NewJob::new(kind, JobPriority::UserOrganize).targeting(target_ids);
    let tx = core.conn.transaction()?;
    let job = crate::jobs::enqueue_in_tx_with_state(&tx, job, JobState::Succeeded, None)?;
    tx.commit()?;
    Ok(job)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 「逐字节相同」必须真的验过**最终落地的文件**，而不是靠「每条条目都验过哈希」
    /// 推断：两条条目共用恢复路径时，各自的清单哈希都能过，被覆盖的却是最终文件。
    #[test]
    fn restored_library_must_byte_match_the_snapshot() {
        let dir = tempfile::tempdir().expect("临时目录");
        let library = dir.path().join(RESTORED_LIBRARY_FILE);
        let bytes = b"the bytes that actually landed on disk";
        fs::write(&library, bytes).expect("写文件");
        let snapshot = ManifestFile {
            path: SNAPSHOT_ENTRY.to_owned(),
            sha256: hash_file_hex(&library).expect("哈希"),
            byte_size: bytes.len() as i64,
        };
        verify_library_matches_snapshot(&library, &snapshot).expect("字节一致时通过");

        // 等长但内容不同：覆盖常常就是「另一份同样合法的库」。
        let mut tampered = snapshot.clone();
        tampered.sha256 = "0".repeat(64);
        let error =
            verify_library_matches_snapshot(&library, &tampered).expect_err("哈希不符必须拒绝");
        assert!(
            matches!(error, CoreError::CorruptedBackup { .. }),
            "{error}"
        );
        assert!(error.to_string().contains("逐字节"), "{error}");

        // 大小不符同样是「不是那份快照」。
        let mut truncated = snapshot;
        truncated.byte_size -= 1;
        let error =
            verify_library_matches_snapshot(&library, &truncated).expect_err("大小不符必须拒绝");
        assert!(error.to_string().contains("字节"), "{error}");
    }
}

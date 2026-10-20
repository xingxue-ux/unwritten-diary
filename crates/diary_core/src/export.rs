//! 可阅读导出（#113）：把**当前日记正文**写成一份 Markdown / 纯文本文件。
//!
//! 三条边界先说清楚：
//!
//! 1. **这不是备份**。文件里只有正文，没有数据库、原件、索引；`ExportTicket.restorable`
//!    恒为 `false`，需要恢复资料库时走 `backups.*`。这个区分同时写在文件头、返回值与
//!    架构文档里。
//! 2. **只有正文**。来源是 `diary_days.current_version_id` 指向的版本行；不导出草稿
//!    （`captures.draft_text`）、不导出回收站里的记录。一天的所有记录都进了回收站时，
//!    这一天即使还留着旧日记版本也不导出——与 `listDays` 的「回收站不算」同一口径。
//! 3. **产物由核心管理**。契约的 `ExportRequest` 没有输出路径，核心不接受任意绝对路径；
//!    文件固定落在资料库目录的 `exports/` 下，重名自动加后缀，**绝不覆盖**已有文件。
//!    OHOS 上还要求这个目录位于应用沙箱内（`is_ohos_app_data_path` 的同款判定），
//!    判定不通过就拒绝导出，一个字节都不写。
//!
//! ## 同步完成 + 已完成的任务行（务实取舍）
//!
//! 契约把导出画成「jobId + 包句柄」的异步任务。本构建**同步**生成文件，并在同一个
//! 事务里写一条 `state = succeeded` 的 `export` 任务行：返回的 `jobId` 指向这条**已经
//! 完成**的记录，不是排队中的任务。这样 `jobs.get` 读得到、界面上的进度轮询不会挂住，
//! 也没有引入一个「排队后由谁执行」的空壳执行器。以后要做真正的异步导出时，替换的是
//! 这里的同步执行，契约返回形状不用改。

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use rusqlite::params;

use crate::error::{CoreError, Result};
use crate::model::{ExportRequest, ExportTicket, JobPriority, JobState, NewJob};
use crate::support;
use crate::Core;

/// 导出任务在 `jobs.kind` 上的名字。它只会以 `succeeded` 落库（见模块说明），
/// 队列不会领取它，也不会出现「没有执行器」的半截任务。
pub const KIND_EXPORT: &str = "export";

/// 导出目录相对资料库根目录的名字。与备份包一样由核心管理，不接受调用方拼路径。
pub const EXPORT_DIR_NAME: &str = "exports";

/// 一天的可读正文。
struct ExportDay {
    day_key: String,
    body: String,
}

/// `exports.create`：同步生成文件并返回票据。
pub(crate) fn create(core: &mut Core, request: &ExportRequest) -> Result<ExportTicket> {
    let (extension, format) = match request.format.trim() {
        "markdown" => ("md", "markdown"),
        "plain_text" => ("txt", "plain_text"),
        other => {
            return Err(CoreError::InvalidArgument {
                reason: format!("导出格式只支持 markdown / plain_text，实际是「{other}」"),
            })
        }
    };
    if request.include_assets {
        return Err(CoreError::InvalidArgument {
            reason: "本构建的阅读导出只写当前日记正文，不随附原件；\
                     includeAssets = true 会明确拒绝，不静默忽略"
                .to_owned(),
        });
    }
    // 解析后**按规范形绑定**：chrono 接受 `2026-10-5`，但 SQL 比的是字符串，
    // 字典序上 `2026-10-05 < 2026-10-5`，直接绑原始输入会把 10 月 1–19 日
    // 静默排除在范围外。范围比较也用解析后的日期，不用原始字符串。
    let from_date = crate::diary::parse_day_key(&request.from_day_key)?;
    let to_date = crate::diary::parse_day_key(&request.to_day_key)?;
    if from_date > to_date {
        return Err(CoreError::InvalidArgument {
            reason: format!(
                "导出范围的起点 {} 晚于终点 {}",
                request.from_day_key, request.to_day_key
            ),
        });
    }
    let from = from_date.format("%Y-%m-%d").to_string();
    let to = to_date.format("%Y-%m-%d").to_string();

    let days = collect_days(core, &from, &to)?;
    if days.is_empty() {
        return Err(CoreError::NotFound {
            entity: "可导出的日记",
            id: format!("{from} 至 {to}（范围里没有当前日记正文；草稿与回收站不算）"),
        });
    }
    let day_count = days.len() as i64;

    let created_at = support::now();
    let content = render(format, &from, &to, &days, created_at);
    let root = export_root(core)?;
    let stem = if from == to {
        format!("readable-export-{from}")
    } else {
        format!("readable-export-{from}_{to}")
    };
    let (path, byte_size) = write_new(&root, &stem, extension, content.as_bytes())?;

    let path_text = display_path(&path);
    let job = match record_completed_job(core, &path_text) {
        Ok(job) => job,
        Err(error) => {
            // 任务行没落成，产物不能留下：返回「成功」却没有可查的 job 是在骗人。
            let _ = std::fs::remove_file(&path);
            return Err(error);
        }
    };
    Ok(ExportTicket {
        job_id: job.id,
        package_ticket: Some(path_text.clone()),
        path: path_text,
        byte_size,
        format: format.to_owned(),
        day_keys: days.into_iter().map(|day| day.day_key).collect(),
        day_count,
        created_at,
        restorable: false,
    })
}

/// 把当前版本与仍在天列表里的日子取出来（**正文 only**）。
fn collect_days(core: &Core, from: &str, to: &str) -> Result<Vec<ExportDay>> {
    let sql = format!(
        "SELECT d.day_key, v.body FROM diary_days d \
         JOIN diary_versions v ON v.id = d.current_version_id AND v.day_key = d.day_key \
         WHERE d.day_key >= ?1 AND d.day_key <= ?2 \
           AND EXISTS (SELECT 1 FROM captures c \
                       WHERE c.day_key = d.day_key AND {}) \
         ORDER BY d.day_key ASC",
        crate::model::capture_not_trashed(Some("c"))
    );
    let mut statement = core.conn.prepare(&sql)?;
    let rows = statement
        .query_map(params![from, to], |row| {
            Ok(ExportDay {
                day_key: row.get(0)?,
                body: row.get(1)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// 写文件：**先抢占文件名**（`create_new`），重名就加后缀；永不覆盖。
///
/// 返回最终路径与字节数。写失败时删掉刚抢占的半成品。
fn write_new(
    directory: &Path,
    stem: &str,
    extension: &str,
    bytes: &[u8],
) -> Result<(PathBuf, i64)> {
    for sequence in 1..=1000 {
        let name = if sequence == 1 {
            format!("{stem}.{extension}")
        } else {
            format!("{stem}-{sequence}.{extension}")
        };
        let path = directory.join(name);
        let mut file = match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(CoreError::Io(error)),
        };
        let written = write_all(&mut file, bytes);
        drop(file);
        match written {
            Ok(()) => {
                let byte_size = i64::try_from(bytes.len()).map_err(|_| CoreError::StorageFull)?;
                return Ok((path, byte_size));
            }
            Err(error) => {
                let _ = std::fs::remove_file(&path);
                return Err(error);
            }
        }
    }
    Err(CoreError::InvalidArgument {
        reason: format!("导出目录里同名文件太多（已试到 {stem}-1000），换一个日期范围再试"),
    })
}

fn write_all(file: &mut File, bytes: &[u8]) -> Result<()> {
    file.write_all(bytes)?;
    file.flush()?;
    // 不用 `sync_all`：导出物不是「已确认记录」，落盘由操作系统按正常文件语义处理；
    // 备份那种「写完还要自校验」的路径才需要更强的持久化承诺。
    Ok(())
}

/// 资料库自己的导出目录。OHOS 上必须在应用沙箱内；其它平台只要求可写。
///
/// 检查顺序有意「先判后建」：沙箱外连目录都不创建。创建之后再 canonicalize 复查一次，
/// 是为了挡住「沙箱内的父目录是指向沙箱外的符号链接」这种字符串前缀看不出来的越界。
fn export_root(core: &Core) -> Result<PathBuf> {
    let directory = core.root_dir()?.join(EXPORT_DIR_NAME);
    let absolute = if directory.is_absolute() {
        directory
    } else {
        std::env::current_dir()?.join(directory)
    };
    let normalized = normalize_lexically(&absolute);
    ensure_sandbox(core, &normalized)?;

    std::fs::create_dir_all(&normalized)?;
    let canonical = std::fs::canonicalize(&normalized)?;
    ensure_sandbox(core, &canonical)?;
    Ok(canonical)
}

/// 给用户/平台层看的路径。
///
/// Windows 的 `std::fs::canonicalize` 返回扩展长度路径（`\\?\C:\...`），
/// `ExportTicket.path` / `packageTicket` 直接把它交出去，宿主保存层与界面会看到
/// 一个带前缀的怪路径。文件操作仍用 canonical `PathBuf`，只把展示文本还原成普通形式
/// （UNC 的 `\\?\UNC\server\share` 还原成 `\\server\share`）。
fn display_path(path: &Path) -> String {
    strip_windows_verbatim(&path.to_string_lossy())
}

fn strip_windows_verbatim(text: &str) -> String {
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        return format!(r"\\{rest}");
    }
    if let Some(rest) = text.strip_prefix(r"\\?\") {
        return rest.to_owned();
    }
    text.to_owned()
}

fn ensure_sandbox(core: &Core, path: &Path) -> Result<()> {
    if !sandbox_only(core) {
        return Ok(());
    }
    if crate::provider::local_engine::is_ohos_app_data_path(&path.to_string_lossy()) {
        return Ok(());
    }
    Err(CoreError::ExportPathRejected {
        reason: format!(
            "导出目录 {} 不在 OHOS 应用沙箱内；核心只往沙箱里的 \
             /data/storage/el2/... 或 /data/storage/el1/{{base,database,cache}}/... 写导出物",
            path.display()
        ),
    })
}

/// 纯字符串消掉 `.` / `..`（不碰文件系统）：沙箱判定必须在创建任何目录之前就能做。
fn normalize_lexically(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn sandbox_only(core: &Core) -> bool {
    core.export_sandbox_only()
}

/// 把导出的正文拼成可读文件。正文本身逐字保留，只在外面加日期分隔与「不是备份」提示。
fn render(
    format: &str,
    from: &str,
    to: &str,
    days: &[ExportDay],
    created_at: chrono::DateTime<chrono::Utc>,
) -> String {
    let mut out = String::new();
    let range = if from == to {
        from.to_owned()
    } else {
        format!("{from} 至 {to}")
    };
    let stamp = support::to_iso(created_at);
    match format {
        "markdown" => {
            out.push_str("# 不写日记 · 阅读导出\n\n");
            out.push_str(
                "> 只包含当前日记正文，**不能用于恢复资料库**。需要完整恢复请使用“完整备份”。\n\n",
            );
            out.push_str(&format!("范围：{range}\n\n"));
            out.push_str(&format!("导出时间：{stamp}\n"));
            for day in days {
                out.push_str(&format!("\n## {}\n\n", day.day_key));
                push_body(&mut out, &day.body);
            }
        }
        _ => {
            out.push_str("不写日记 · 阅读导出\n\n");
            out.push_str(
                "只包含当前日记正文，不能用于恢复资料库。需要完整恢复请使用“完整备份”。\n\n",
            );
            out.push_str(&format!("范围：{range}\n"));
            out.push_str(&format!("导出时间：{stamp}\n"));
            for day in days {
                out.push_str(&format!(
                    "\n{}\n{}\n\n",
                    day.day_key,
                    "-".repeat(day.day_key.len())
                ));
                push_body(&mut out, &day.body);
            }
        }
    }
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out
}

/// 正文原样写进去；只保证与下一天之间有空行，不改写正文里的任何字符。
fn push_body(out: &mut String, body: &str) {
    out.push_str(body);
    if !body.ends_with('\n') {
        out.push('\n');
    }
    out.push('\n');
}

/// 写一条**已完成**的导出任务记录。`jobs.get` 因此总能读到这次导出的 jobId。
fn record_completed_job(core: &mut Core, path_text: &str) -> Result<crate::model::Job> {
    let job = NewJob::new(KIND_EXPORT, JobPriority::UserOrganize);
    let tx = core.conn.transaction()?;
    let job = crate::jobs::enqueue_in_tx_with_state(&tx, job, JobState::Succeeded, None)?;
    // 把产物路径记进任务的目标里，排查时不用再猜文件在哪。它是核心生成的绝对路径，
    // 不是调用方输入，不构成「接受任意路径」。
    let target_ids = serde_json::to_string(&[path_text])?;
    tx.execute(
        "UPDATE jobs SET target_ids = ?1 WHERE id = ?2",
        params![target_ids, job.id],
    )?;
    tx.commit()?;
    Ok(job)
}

#[cfg(test)]
mod tests {
    use super::strip_windows_verbatim;

    #[test]
    fn windows_verbatim_prefix_is_stripped_for_display() {
        assert_eq!(
            strip_windows_verbatim(r"\\?\C:\Users\me\library\exports\a.md"),
            r"C:\Users\me\library\exports\a.md"
        );
        assert_eq!(
            strip_windows_verbatim(r"\\?\UNC\server\share\a.md"),
            r"\\server\share\a.md"
        );
        // 普通 POSIX / Windows 路径原样返回。
        assert_eq!(
            strip_windows_verbatim("/data/storage/el2/base/a.md"),
            "/data/storage/el2/base/a.md"
        );
    }
}

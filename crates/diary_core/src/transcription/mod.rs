//! 转写链（#91）：录音 → 转写 → **可检索、可整理的正文**。
//!
//! # 转写结果在库里是什么
//!
//! 它是**派生内容**（`extracted_contents` + `extracted_segments`），不是用户原件，
//! 也不是一条新的「用户修订」。三条口径：
//!
//! 1. **不覆盖原始录音**：音频那条 `source_revisions` 行一个字节都不动（它仍然
//!    `author_type = 'user'`、仍然带着 `asset_id`）。转写只是这条来源的**派生正文**，
//!    可以重建、可以丢弃；
//! 2. **不冒充用户原话**：这条正文的 `extractor_id` 是 [`EXTRACTOR_ID`]，与
//!    「用户自己写的文字」（`captures.draft_text`）、「导入文件里解析出来的文字」
//!    在库里就分得开。沿用 B4b 的口径（见 `model.rs` 里 `DiaryVersionOrigin` 的注释）：
//!    `source_revisions.author_type` 只有 user/import，**AI 产物不写进用户原件**，
//!    所以这里也不新造一个 authorType——区分靠的是「派生内容 + extractor_id +
//!    coverage + locator」，这些字段本来就存在；
//! 3. **就是这条材料的当前正文**：`chunks.rs`（语义分块）与 `search.rs`（关键词）
//!    都以「当前修订的派生内容」为正文来源，所以转写一落库，这条音频就同时进了
//!    关键词检索与分块——不需要为新链路再开一条检索通路（issue #91 的验收：
//!    「与既有检索链一致」）。
//!
//! # 分段、可恢复、幂等（一张表就够，不改 schema）
//!
//! - **分段**：按 [`WINDOW_MS`] 把音频切成固定窗口，每个窗口一次引擎调用。
//!   不做重叠：重叠会让「同一段语音」出现在两个片段里，而幂等要求
//!   「同片段重放只有一份文本」。代价写在文档的「没有做到的」里（边界处可能切断词）；
//! - **片段行 id 是确定的**：`{content_id}:{ordinal}`。同一段重放是
//!   `ON CONFLICT(id) DO UPDATE`，不可能出现两行——幂等由**主键**保证，不靠自觉；
//! - **可恢复**：跑之前先看这一份内容已经有哪几段。已经有的**直接跳过、不再调用引擎**
//!   （也因此不重复计费/不重复计算）；进程被杀之后，下一次领取从第一个缺的序号继续，
//!   **不重头**。片段永远是 0..k 的连续前缀（循环遇到错误就停），所以聚合正文可以
//!   在末尾追加，字符区间也不会错位；
//! - **半截结果不当完整文本**：跑到一半失败时，已完成的片段留在库里，内容行的
//!   `status = partial`、`coverage = partial`、`coverage_reason` 写明「已完成 k/N 段 +
//!   失败原因」；只有全部窗口跑完才会变成 `ready/complete`；
//! - **换模型就重来**：`extractor_version` 里带着模型身份（文件 + 字节数 + mtime +
//!   sha256 前 12 位）。不一致说明这份派生内容不是当前模型产的 → 先删旧片段再跑，
//!   两代文本不会混在一份内容里。
//!
//! # 时间戳对齐
//!
//! 每个窗口落一个 `extracted_segments` 行：
//!
//! | 列 | 值 | 含义 |
//! |---|---|---|
//! | `locator_type` | `audio` | 这一条定位的是音频 |
//! | `start_ms` / `end_ms` | 窗口在**逻辑音频**里的毫秒区间 | 「这段文字来自哪段时间」 |
//! | `text_start` / `text_end` | 该段文字在**聚合正文**里的字符区间（Unicode 标量值，左闭右开） | 「哪段文字属于哪个片段」 |
//! | `asset_id` | 音频资产 id | 前端据此播放原件 |
//! | `source_revision_id` | 音频那条修订 | 与既有的 locator 语义一致 |
//!
//! 检索命中时 `SearchHit.locator` 带的就是它，所以「这段文字来自这段录音」在结果里
//! 直接可读；`sources.locate` 也能拿它去打开音频。**不伪造精度**：窗口内不做
//! 词级时间戳（引擎给的是 token 级时间戳，但我们的落库单位是窗口，写一个更细的数字
//! 会让人以为它比实际更准）。
//!
//! # 缺失就降级，不产出空文本
//!
//! 引擎/模型不可用时，闸门（`provider::job_gate` → `capability_gate_for`）在**领取前**
//! 就把任务挡在 `waiting_configuration`：一条转写都不会执行，**一个派生内容行都不会
//! 写**。这不是「转写成功但文本为空」，库里看得出来区别。

pub mod seam;
pub mod sherpa;
pub mod wav;

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use rusqlite::{params, OptionalExtension, Transaction};

use crate::error::{CoreError, Result};
use crate::model::{Job, ProcessingStatus};
use crate::provider::local_engine::{self, EngineConfig, PcmRequest};
use crate::search;
use crate::support;
use crate::Core;

/// 转写任务的 kind。`recordings::finalize` 排的就是它（常量在这里定义，
/// 两边引用同一个）。
pub(crate) const KIND_TRANSCRIBE: &str = "transcribe";

/// 转写结果这个「提取器」的标识。
///
/// 名字里的 `local_` 是刻意的：走远端 Provider 的转写（#99 的
/// `OpenAiCompatibleTranscriptEngine`）如果将来也要落库，应当用另一个 id，
/// 这样「这段正文是本机引擎产的还是远端产的」在库里分得开。
pub(crate) const EXTRACTOR_ID: &str = "local_transcription";

/// 提取器版本（落库用）。真正的身份还要拼上模型身份，见 [`content_version`]。
pub(crate) const EXTRACTOR_VERSION: &str = "1";

/// 一个转写窗口的长度（毫秒）。
///
/// 30 秒：SenseVoice 这类非流式模型一次吃几十秒没有问题，窗口太短会把词切碎，
/// 太长则一段失败要重跑很久。这个数字不是量测出来的最优值（文档里如实写了）。
pub(crate) const WINDOW_MS: i64 = 30_000;

/// 落库的提取器版本：`<EXTRACTOR_VERSION>:<模型身份>`。
fn content_version(model_identity: &str) -> String {
    format!("{EXTRACTOR_VERSION}:{model_identity}")
}

/// 模型文件的 sha256 前 12 位（进程内缓存）。
///
/// 228 MiB 的模型每次任务都哈希会把任务拖慢半秒以上，而这里的用途只是
/// 「模型换了没有」。缓存键是 (路径, 字节数, mtime)——换了文件必然换键。
pub(crate) fn model_digest(path: &Path) -> Option<String> {
    /// 缓存键：(路径, 字节数, mtime 秒)。换了文件必然换键。
    type ModelFileKey = (PathBuf, u64, u64);
    static CACHE: Mutex<Option<(ModelFileKey, String)>> = Mutex::new(None);
    let meta = std::fs::metadata(path).ok()?;
    let modified = meta
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    let key = (path.to_path_buf(), meta.len(), modified);
    if let Ok(cache) = CACHE.lock() {
        if let Some((cached_key, digest)) = cache.as_ref() {
            if cached_key == &key {
                return Some(digest.clone());
            }
        }
    }
    let digest = crate::assets::hash_file(path).ok()?;
    let short: String = digest.chars().take(12).collect();
    if let Ok(mut cache) = CACHE.lock() {
        *cache = Some((key, short.clone()));
    }
    Some(short)
}

/// 一个转写窗口。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Window {
    ordinal: i64,
    start_ms: i64,
    end_ms: i64,
    start_frame: u64,
    frames: u64,
}

/// 把时长切成窗口。最后一个窗口可能比 [`WINDOW_MS`] 短（如实截短，不补静音）。
fn windows_for(duration_ms: i64, sample_rate: u32, frames: u64) -> Vec<Window> {
    if duration_ms <= 0 || frames == 0 || sample_rate == 0 {
        return Vec::new();
    }
    let mut windows = Vec::new();
    let mut start_ms = 0_i64;
    let mut ordinal = 0_i64;
    while start_ms < duration_ms {
        let end_ms = (start_ms + WINDOW_MS).min(duration_ms);
        // 帧区间由毫秒区间换算，保证「时间戳」与「真正送去识别的采样」是同一段。
        let start_frame = (start_ms as u128 * u128::from(sample_rate) / 1000) as u64;
        let end_frame = (end_ms as u128 * u128::from(sample_rate) / 1000) as u64;
        let end_frame = end_frame.min(frames);
        if end_frame > start_frame {
            windows.push(Window {
                ordinal,
                start_ms,
                end_ms,
                start_frame,
                frames: end_frame - start_frame,
            });
            ordinal += 1;
        }
        start_ms = end_ms;
    }
    windows
}

/// 解析好的目标：这条任务瞄着的音频资产，以及它在材料里的位置。
struct Target {
    source_id: String,
    revision_id: String,
    asset_id: String,
    object_ref: String,
    original_name: String,
}

/// 资产 → 它所在的**当前**修订。
///
/// 只认「当前修订」：派生内容是挂在当前修订上的，非当前修订的正文本来就不参与
/// 检索（见 `m2-修订与检索可见性.md`）。找不到就如实终态失败——重试一百次也不会
/// 让一条已经不是当前修订的音频变回来。
fn resolve_target(core: &Core, asset_id: &str) -> Result<Target> {
    let row: Option<(String, String, String, String, String)> = core
        .conn
        .query_row(
            "SELECT si.source_id, sr.revision_id, a.id, a.object_ref, a.original_name \
             FROM source_revisions sr \
             JOIN source_items si ON si.source_id = sr.source_id \
             JOIN assets a ON a.id = sr.asset_id \
             WHERE sr.asset_id = ?1 AND si.current_revision_id = sr.revision_id \
             ORDER BY si.source_id LIMIT 1",
            params![asset_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .optional()?;
    let Some((source_id, revision_id, asset_id, object_ref, original_name)) = row else {
        return Err(CoreError::NotFound {
            entity: "音频材料的当前修订",
            id: asset_id.to_owned(),
        });
    };
    Ok(Target {
        source_id,
        revision_id,
        asset_id,
        object_ref,
        original_name,
    })
}

/// 转写任务的执行者。
///
/// 错误一律用 `CoreError` 表达，时间语义由 [`is_transient`] + `jobs::FailureDisposition`
/// 决定（与 `respond` 同一套）。
pub(crate) fn run(core: &mut Core, job: &Job) -> Result<()> {
    let Some(asset_id) = job.target_ids.first().cloned() else {
        return Err(CoreError::InvalidState {
            entity: "任务",
            id: job.id.clone(),
            state: "转写任务没有目标资产".to_owned(),
        });
    };

    let target = resolve_target(core, &asset_id)?;
    let path = core.root_dir()?.join(&target.object_ref);
    if !path.is_file() {
        return Err(CoreError::AssetMissing {
            asset_id: asset_id.clone(),
            reason: format!("音频文件不在：{}", target.object_ref),
        });
    }

    // 引擎配置与就绪：闸门在领取前已经查过一遍，这里再查一遍是**防御**
    // （绕过闸门也不能在引擎不可用时写出半份内容）。
    let config = engine_config(core)?;
    let engine = core.asr_engine_handle();
    let readiness = engine.readiness(&config);
    if !readiness.ready {
        return Err(CoreError::ProviderNotConfigured {
            reason: local_engine::blocked_message(&readiness),
        });
    }

    // 解码器只认 PCM WAV；不认识的容器如实报「不支持」，不产出一段乱码文字。
    let mut reader = wav::WavReader::open(&path)?;
    let frames = reader.frames();
    let sample_rate = reader.sample_rate;
    let duration_ms = reader.duration_ms();
    let windows = windows_for(duration_ms, sample_rate, frames);
    if windows.is_empty() {
        return Err(CoreError::ExtractionFailed {
            reason: format!(
                "音频里没有可解码的采样（{}，时长 {duration_ms} ms）：转写不会产出一份空正文冒充成功",
                target.original_name
            ),
        });
    }

    // 内容行：幂等命中 / 换模型重建，都在这一步决定。
    let (content_id, model_identity) = ensure_content(core, &target, &config, windows.len() as i64)?;
    let done = completed_ordinals(core, &content_id)?;

    let mut completed = done.len() as i64;
    let total = windows.len() as i64;
    let outcome = run_windows(
        core,
        &target,
        &content_id,
        &config,
        &model_identity,
        &mut reader,
        &windows,
        &done,
        &mut completed,
        job,
    );

    match outcome {
        Ok(summary) => {
            finish_content(core, &content_id, &summary, total)?;
            Ok(())
        }
        Err(error) => {
            // 半截结果**不当完整文本**：如实落成 partial + 写明跑到哪、为什么停。
            mark_interrupted(core, &content_id, completed, total, &error)?;
            Err(error)
        }
    }
}

/// 逐窗口转写。返回的是这一份内容的最终状态摘要。
#[allow(clippy::too_many_arguments)]
fn run_windows(
    core: &mut Core,
    target: &Target,
    content_id: &str,
    config: &EngineConfig,
    model_identity: &str,
    reader: &mut wav::WavReader,
    windows: &[Window],
    done: &HashSet<i64>,
    completed: &mut i64,
    job: &Job,
) -> Result<ContentSummary> {
    let engine = core.asr_engine_handle();
    let mut empty_windows = 0_i64;
    let mut texts = 0_i64;

    for window in windows {
        if done.contains(&window.ordinal) {
            // 已经转过的窗口：跳过，不重复调用引擎（恢复的语义就在这里）。
            let text = segment_text(core, content_id, window.ordinal)?;
            if text.trim().is_empty() {
                empty_windows += 1;
            } else {
                texts += 1;
            }
            continue;
        }

        let samples = reader.read_window(window.start_frame, window.frames)?;
        let outcome = engine.transcribe(
            config,
            &PcmRequest {
                samples,
                sample_rate: reader.sample_rate,
                language: config.language.clone(),
            },
        )?;
        let text = outcome.text.trim().to_owned();
        if text.is_empty() {
            empty_windows += 1;
        } else {
            texts += 1;
        }

        persist_window(core, target, content_id, window, &text)?;
        *completed += 1;
        crate::jobs::report_progress(core, &job.id, *completed, windows.len() as i64)?;
    }

    Ok(ContentSummary {
        model_identity: model_identity.to_owned(),
        empty_windows,
        texts,
    })
}

/// 一份内容跑完之后的摘要（写 `coverage_reason` 用）。
struct ContentSummary {
    model_identity: String,
    empty_windows: i64,
    texts: i64,
}

/// 内容行：命中就复用，模型换了就重建（删旧片段、清空正文，从第一段重来）。
fn ensure_content(
    core: &mut Core,
    target: &Target,
    config: &EngineConfig,
    total: i64,
) -> Result<(String, String)> {
    let model_identity = config.model_identity();
    let version = content_version(&model_identity);
    let existing: Option<(String, String)> = core
        .conn
        .query_row(
            "SELECT id, extractor_version FROM extracted_contents \
             WHERE source_revision_id = ?1 AND extractor_id = ?2",
            params![target.revision_id, EXTRACTOR_ID],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;

    let now_text = support::to_iso(support::now());
    match existing {
        Some((content_id, stored_version)) if stored_version == version => {
            // 续跑时如实写出「已经完成几段」——写死 0 会让用户在断点恢复后看到
            // 一个比实际更悲观的进度（进度列是真的，这句说明也必须是真的）。
            let done: i64 = core.conn.query_row(
                "SELECT COUNT(*) FROM extracted_segments WHERE content_id = ?1",
                params![content_id],
                |row| row.get(0),
            )?;
            core.conn.execute(
                "UPDATE extracted_contents SET status = 'processing', coverage = 'partial', \
                 coverage_reason = ?1 WHERE id = ?2",
                params![
                    format!("转写进行中：共 {total} 段，已完成 {done} 段（重启后从断点继续）"),
                    content_id
                ],
            )?;
            Ok((content_id, model_identity))
        }
        Some((content_id, stored_version)) => {
            let tx = core.conn.transaction()?;
            // 旧片段连同它们的检索行一起作废（search_docs 按 segment_id 级联）。
            tx.execute(
                "DELETE FROM extracted_segments WHERE content_id = ?1",
                params![content_id],
            )?;
            tx.execute(
                "UPDATE extracted_contents SET text = '', status = 'processing', \
                 coverage = 'partial', coverage_reason = ?1, extractor_version = ?2, \
                 error_code = NULL WHERE id = ?3",
                params![
                    format!(
                        "模型换了（原版本 {stored_version}，现版本 {version}）：旧片段已作废，\
                         这份正文从第一段重来"
                    ),
                    version,
                    content_id
                ],
            )?;
            tx.commit()?;
            Ok((content_id, model_identity))
        }
        None => {
            let content_id = support::new_id("ext");
            let tx = core.conn.transaction()?;
            tx.execute(
                "INSERT INTO extracted_contents (id, source_id, source_revision_id, extractor_id, \
                 extractor_version, text, status, coverage, coverage_reason, error_code, created_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, '', 'processing', 'partial', ?6, NULL, ?7)",
                params![
                    content_id,
                    target.source_id,
                    target.revision_id,
                    EXTRACTOR_ID,
                    version,
                    format!("转写进行中：共 {total} 段，已完成 0 段"),
                    now_text
                ],
            )?;
            tx.commit()?;
            Ok((content_id, model_identity))
        }
    }
}

/// 这一份内容已经转好的窗口序号。
fn completed_ordinals(core: &Core, content_id: &str) -> Result<HashSet<i64>> {
    let mut statement = core
        .conn
        .prepare("SELECT ordinal FROM extracted_segments WHERE content_id = ?1")?;
    let rows = statement
        .query_map(params![content_id], |row| row.get::<_, i64>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows.into_iter().collect())
}

fn segment_text(core: &Core, content_id: &str, ordinal: i64) -> Result<String> {
    let text: Option<String> = core
        .conn
        .query_row(
            "SELECT text FROM extracted_segments WHERE id = ?1",
            params![segment_id(content_id, ordinal)],
            |row| row.get(0),
        )
        .optional()?;
    Ok(text.unwrap_or_default())
}

/// 片段行 id：**确定的**。同一段重放必然落在同一行上（幂等由主键保证）。
fn segment_id(content_id: &str, ordinal: i64) -> String {
    format!("{content_id}:{ordinal}")
}

/// 落一个窗口：片段行 + 关键词索引 + 聚合正文 + 字符区间，**一个事务**。
fn persist_window(
    core: &mut Core,
    target: &Target,
    content_id: &str,
    window: &Window,
    text: &str,
) -> Result<()> {
    let now_text = support::to_iso(support::now());
    let tx = core.conn.transaction()?;

    // 之前的片段构成前缀（循环遇到错误就停），所以聚合正文可以在末尾追加，
    // 字符区间也能算准：join("\n") 下，第 k 段的起点 = 前面字符数 + 前面的段数。
    // 空文本的段（静音窗口）**不参与聚合正文**：一起 join 进去会得到一串
    // 空行（`"\n\n"`），而那份正文对用户没有意义。字符区间也因此只算有文字的段。
    let (prior_chars, prior_segments): (i64, i64) = tx.query_row(
        "SELECT COALESCE(SUM(LENGTH(text)), 0), COUNT(*) FROM extracted_segments \
         WHERE content_id = ?1 AND ordinal < ?2 AND text <> ''",
        params![content_id, window.ordinal],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let text_chars = text.chars().count() as i64;
    let text_start = prior_chars + prior_segments;
    let text_end = text_start + text_chars;

    tx.execute(
        "INSERT INTO extracted_segments (id, content_id, ordinal, text, locator_type, \
         source_revision_id, text_start, text_end, start_ms, end_ms, page_number, block_id, \
         rect_left, rect_top, rect_right, rect_bottom, asset_id) \
         VALUES (?1, ?2, ?3, ?4, 'audio', ?5, ?6, ?7, ?8, ?9, NULL, NULL, NULL, NULL, NULL, NULL, ?10) \
         ON CONFLICT(id) DO UPDATE SET text = excluded.text, ordinal = excluded.ordinal, \
         text_start = excluded.text_start, text_end = excluded.text_end, \
         start_ms = excluded.start_ms, end_ms = excluded.end_ms, asset_id = excluded.asset_id",
        params![
            segment_id(content_id, window.ordinal),
            content_id,
            window.ordinal,
            text,
            target.revision_id,
            text_start,
            text_end,
            window.start_ms,
            window.end_ms,
            target.asset_id,
        ],
    )?;

    // 关键词索引与派生内容同一个事务：写进去了却搜不到，是没人知道原因的那类 bug。
    // 空文本（静音窗口）不进索引，但要把可能存在的旧词项清掉。
    let sid = segment_id(content_id, window.ordinal);
    if text.trim().is_empty() {
        tx.execute("DELETE FROM search_docs WHERE segment_id = ?1", params![sid])?;
    } else {
        search::index_segment(
            &tx,
            search::SegmentInput {
                segment_id: &sid,
                text,
            },
        )?;
    }

    // 聚合正文 = 各段按 ordinal 用 `\n` 连接（与 `chunks.rs::segment_text` 同一口径）。
    let texts: Vec<String> = {
        let mut statement = tx.prepare(
            "SELECT text FROM extracted_segments WHERE content_id = ?1 AND text <> '' \
             ORDER BY ordinal",
        )?;
        let rows = statement
            .query_map(params![content_id], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };
    let aggregate = texts.join("\n");
    let (done, total_reason) = (texts.len() as i64, None::<String>);
    tx.execute(
        "UPDATE extracted_contents SET text = ?1, status = 'processing', coverage = 'partial', \
         coverage_reason = ?2 WHERE id = ?3",
        params![
            aggregate,
            match total_reason {
                Some(reason) => reason,
                None => format!("转写进行中：已完成 {done} 段"),
            },
            content_id
        ],
    )?;
    tx.execute(
        "UPDATE extracted_contents SET error_code = NULL WHERE id = ?1",
        params![content_id],
    )?;
    tx.commit()?;
    let _ = now_text;
    Ok(())
}

/// 跑完：把状态推到 ready + 如实的 coverage。
fn finish_content(
    core: &mut Core,
    content_id: &str,
    summary: &ContentSummary,
    total: i64,
) -> Result<()> {
    let (status, coverage, reason) = if summary.texts == 0 {
        // 引擎真的跑了，但整段音频里没有识别出文字（静音、纯音乐、语言不支持）。
        // 这不是失败，也**不是「能搜到正文」**——如实落成 metadata_only，
        // 与「没有 OCR 的图片」同一档语义：原件留着，可按时间/文件名找到。
        (
            ProcessingStatus::Ready,
            crate::model::Coverage::MetadataOnly,
            format!(
                "本机引擎跑完了 {total} 段，但没有识别出文字（可能是静音、纯音乐，\
                 或说的是模型不支持的语言）。原音频照旧可播放、可按时间找到；\
                 这一段没有正文可搜。（模型：{}）",
                summary.model_identity
            ),
        )
    } else {
        (
            ProcessingStatus::Ready,
            crate::model::Coverage::Complete,
            format!(
                "本机引擎（sherpa-onnx）转写：共 {total} 段，{texts} 段有文字，\
                 {empty} 段没有识别出文字；每段都带 [start_ms, end_ms) 时间戳。\
                 全程未联网。（模型：{model}）",
                texts = summary.texts,
                empty = summary.empty_windows,
                model = summary.model_identity
            ),
        )
    };

    let tx = core.conn.transaction()?;
    tx.execute(
        "UPDATE extracted_contents SET status = ?1, coverage = ?2, coverage_reason = ?3, \
         error_code = NULL WHERE id = ?4",
        params![status.wire(), coverage.wire(), reason, content_id],
    )?;
    // 转写产出的就是这条音频的正文：以前那条「没有转写能力，只能按文件名找」的
    // 标记行（没有正文、coverage = metadata_only/unavailable）到此为止。
    // 留着它会让消费者同时看到两条互相矛盾的派生内容——一条说搜不到正文，
    // 一条就是正文。删掉的是**没有正文的标记**，不是用户内容。
    prune_superseded_markers(&tx, content_id)?;
    tx.commit()?;
    Ok(())
}

/// 失败/中断：半截结果不当完整文本。
fn mark_interrupted(
    core: &mut Core,
    content_id: &str,
    completed: i64,
    total: i64,
    error: &CoreError,
) -> Result<()> {
    let error_code = error.code().wire().to_owned();
    core.conn.execute(
        "UPDATE extracted_contents SET status = 'partial', coverage = 'partial', \
         coverage_reason = ?1, error_code = ?2 WHERE id = ?3",
        params![
            format!(
                "转写没跑完：已完成 {completed}/{total} 段（已完成的文字可以搜到，\
                 剩下的还没有）。停下原因：{error}"
            ),
            error_code,
            content_id
        ],
    )?;
    Ok(())
}

/// 删掉这条修订上「没有正文」的旧标记行（见 `finish_content` 的注释）。
fn prune_superseded_markers(tx: &Transaction<'_>, content_id: &str) -> Result<()> {
    tx.execute(
        "DELETE FROM extracted_contents WHERE source_revision_id = \
         (SELECT source_revision_id FROM extracted_contents WHERE id = ?1) \
         AND id <> ?1 AND COALESCE(text, '') = '' \
         AND coverage IN ('metadata_only', 'unavailable')",
        params![content_id],
    )?;
    Ok(())
}

/// 引擎配置：与闸门同一套解析（配置行 → 环境变量 → 随包默认）。
///
/// 这里**不重新挑服务**：闸门在领取前已经把「用哪个服务」定下来了，
/// 配置解析只认 `providers.model` 给的那一个目录（内建服务的那一行给出的是
/// 随包路径）。多一层挑选只会让闸门与执行者说的不是同一件事。
fn engine_config(core: &Core) -> Result<EngineConfig> {
    let choice = local_engine::transcription_choice(core)?;
    local_engine::config_for(core, choice.model_override.as_deref())
}

/// 错误的时间语义（与 `jobs::FailureDisposition` 对接）。
///
/// 只有**换台机器/换个时间真的可能好**的才当暂时性错误：
///
/// | 错误 | 处置 | 为什么 |
/// |---|---|---|
/// | 文件暂时不在（`AssetMissing`） | 重试 | 导入与提取不在一个事务里，先后顺序不保证 |
/// | IO 失败 | 重试 | 磁盘忙、权限临时变化 |
/// | 引擎/模型没就绪、能力不支持 | 等配置 | 用户去补模型，重试一百次也一样 |
/// | 模型加载/结构不对（`ModelUnavailable`） | 终态 | 重试不会让坏模型变好 |
/// | 容器/采样格式不支持（`ExtractionFailed`） | 终态 | 确定性的格式问题，退避重试是白等 |
/// | 目标不是当前修订（`NotFound`） | 终态 | 音频已经换过修订，重来也找不回来 |
pub(crate) fn is_transient(error: &CoreError) -> bool {
    matches!(
        error,
        CoreError::AssetMissing { .. }
            | CoreError::Io(_)
            | CoreError::Database(_)
            | CoreError::InvalidState { .. }
            | CoreError::CorruptedData { .. }
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_cover_the_whole_audio_without_gaps() {
        // 70 秒 @16 kHz：30 + 30 + 10 秒。
        let windows = windows_for(70_000, 16_000, 70 * 16_000);
        assert_eq!(windows.len(), 3);
        assert_eq!(
            windows
                .iter()
                .map(|w| (w.ordinal, w.start_ms, w.end_ms))
                .collect::<Vec<_>>(),
            vec![(0, 0, 30_000), (1, 30_000, 60_000), (2, 60_000, 70_000)]
        );
        // 帧区间与毫秒区间对得上，且首尾相接。
        assert_eq!(windows[0].start_frame, 0);
        assert_eq!(windows[1].start_frame, 30 * 16_000);
        assert_eq!(windows[1].start_frame, windows[0].start_frame + windows[0].frames);
        assert_eq!(
            windows[2].start_frame + windows[2].frames,
            70 * 16_000,
            "最后一个窗口要覆盖到音频末尾"
        );
    }

    #[test]
    fn short_audio_is_one_window_and_empty_audio_is_none() {
        let windows = windows_for(1_000, 8_000, 8_000);
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].end_ms, 1_000);
        assert!(windows_for(0, 8_000, 0).is_empty());
    }

    #[test]
    fn segment_ids_are_deterministic() {
        assert_eq!(segment_id("ext_1", 3), "ext_1:3");
        assert_eq!(segment_id("ext_1", 3), segment_id("ext_1", 3));
    }

    #[test]
    fn transient_errors_are_the_only_ones_worth_retrying() {
        assert!(is_transient(&CoreError::AssetMissing {
            asset_id: "a".to_owned(),
            reason: "文件不在".to_owned(),
        }));
        assert!(!is_transient(&CoreError::ModelUnavailable {
            reason: "模型坏了".to_owned(),
        }));
        assert!(!is_transient(&CoreError::ProviderNotConfigured {
            reason: "没有模型目录".to_owned(),
        }));
        assert!(!is_transient(&CoreError::ExtractionFailed {
            reason: "不是 WAV".to_owned(),
        }));
    }
}
//! 提取与来源定位，契约第 2.4 节与任务书第 4 节。
//!
//! 一次提取的产出是**派生内容**：可以重建、可以丢弃，但绝不能反过来影响原件。
//! 三条约定贯穿本模块：
//! 1. **`coverage` 必须说真话**：能提多少写多少，提不了的写明原因。没有 OCR 就说
//!    `metadata_only`，不能因为提取为空就显示成「没有相关内容」。
//! 2. **解析失败 ≠ 内容为空**：空文本文件是 `complete`（就是空的），解析器报错是
//!    `failed` 加错误码，两者分开处理。
//! 3. **不杜撰定位**：页码不可靠时 `page_number` 留空，不去猜。

use std::fs::File;
use std::io::Read;
use std::path::Path;

use chardetng::{EncodingDetector, Iso2022JpDetection, Utf8Detection};
use quick_xml::events::Event;
use quick_xml::Reader;
use rusqlite::{params, Connection, OptionalExtension};

use crate::error::{CoreError, ErrorCode, Result};
use crate::model::{
    Coverage, ExtractedContent, ExtractedSegment, ProcessingStatus, SourceLocator, SourceLocation,
};
use crate::search;
use crate::support;
use crate::Core;

/// 单个文本文件最多读多少字节。超过就只读前面一段并如实标成 partial。
const MAX_TEXT_BYTES: u64 = 8 * 1024 * 1024;

/// 提取器的输入：一个只读的原件句柄。
pub struct ExtractionInput<'a> {
    pub asset_id: &'a str,
    pub source_id: &'a str,
    /// 源修订版本，所有 locator 都会带上它。
    pub source_revision_id: &'a str,
    pub mime: &'a str,
    pub original_name: &'a str,
    /// 只读路径。提取器不得写入、移动或删除它。
    pub path: &'a Path,
}

/// 提取器的产出。
#[derive(Debug, Clone)]
pub struct ExtractionOutcome {
    pub text: String,
    pub segments: Vec<ExtractedSegment>,
    pub status: ProcessingStatus,
    pub coverage: Coverage,
    pub coverage_reason: Option<String>,
    pub error_code: Option<ErrorCode>,
    pub warnings: Vec<String>,
}

impl ExtractionOutcome {
    /// 没有可提取内容时的统一形状：文件留着，但明确说明为什么提不出正文。
    fn metadata_only(coverage: Coverage, reason: &str) -> Self {
        Self {
            text: String::new(),
            segments: Vec::new(),
            status: ProcessingStatus::Ready,
            coverage,
            coverage_reason: Some(reason.to_owned()),
            error_code: None,
            warnings: Vec::new(),
        }
    }
}

/// 统一提取器接口。
pub trait Extractor: Send + Sync {
    fn id(&self) -> &'static str;
    fn version(&self) -> &'static str;
    fn supports(&self, mime: &str, original_name: &str) -> bool;
    fn extract(&self, input: &ExtractionInput<'_>) -> Result<ExtractionOutcome>;
}

/// 内置提取器清单，按顺序匹配，第一个支持的生效。
pub fn builtin_extractors() -> Vec<Box<dyn Extractor>> {
    vec![
        Box::new(PlainTextExtractor),
        Box::new(MarkdownExtractor),
        Box::new(DocxExtractor),
        Box::new(PdfExtractor),
        Box::new(ImageMetadataOnlyExtractor),
        Box::new(MediaMetadataOnlyExtractor),
        Box::new(UnsupportedExtractor),
    ]
}

// ------------------------------------------------------------------ 纯文本

/// 纯文本：UTF-8、带 BOM 的 UTF-16，以及用检测器认出来的常见编码（含 GBK）。
pub struct PlainTextExtractor;

impl Extractor for PlainTextExtractor {
    fn id(&self) -> &'static str {
        "plain_text"
    }

    fn version(&self) -> &'static str {
        "1"
    }

    fn supports(&self, mime: &str, original_name: &str) -> bool {
        if mime.starts_with("text/") {
            return true;
        }
        if matches!(
            mime,
            "application/json" | "application/xml" | "application/x-yaml" | "application/yaml"
        ) {
            return true;
        }
        let lower = original_name.to_ascii_lowercase();
        [
            ".txt", ".log", ".csv", ".tsv", ".json", ".yaml", ".yml", ".ini", ".conf", ".toml",
            ".srt", ".vtt",
        ]
        .iter()
        .any(|suffix| lower.ends_with(suffix))
    }

    fn extract(&self, input: &ExtractionInput<'_>) -> Result<ExtractionOutcome> {
        let (text, warnings, truncated) = decode_text_file(input.path)?;
        let mut outcome = paragraphs_to_outcome(input, &text)?;
        outcome.warnings.extend(warnings);
        if truncated {
            outcome.coverage = Coverage::Partial;
            outcome.status = ProcessingStatus::Partial;
            outcome.coverage_reason = Some(format!(
                "文件超过 {} MiB，只提取了前面一段",
                MAX_TEXT_BYTES / (1024 * 1024)
            ));
        }
        Ok(outcome)
    }
}

/// Markdown 就是文本。单独一个提取器是为了让库里能看出用的是哪一条路径。
pub struct MarkdownExtractor;

impl Extractor for MarkdownExtractor {
    fn id(&self) -> &'static str {
        "markdown"
    }

    fn version(&self) -> &'static str {
        "1"
    }

    fn supports(&self, mime: &str, original_name: &str) -> bool {
        mime == "text/markdown" || original_name.to_ascii_lowercase().ends_with(".md")
    }

    fn extract(&self, input: &ExtractionInput<'_>) -> Result<ExtractionOutcome> {
        let (text, warnings, _) = decode_text_file(input.path)?;
        let mut outcome = paragraphs_to_outcome(input, &text)?;
        outcome.warnings.extend(warnings);
        // Markdown 保留原文：标题、列表这些标记本身就是用户写下的内容。
        outcome.coverage_reason = Some("保留 Markdown 原文，未剥离标记".to_owned());
        Ok(outcome)
    }
}

fn decode_text_file(path: &Path) -> Result<(String, Vec<String>, bool)> {
    let mut file = File::open(path)?;
    let size = file.metadata()?.len();
    let mut buffer = Vec::with_capacity(size.min(MAX_TEXT_BYTES) as usize);
    file.by_ref()
        .take(MAX_TEXT_BYTES)
        .read_to_end(&mut buffer)?;
    let truncated = size > MAX_TEXT_BYTES;

    let mut warnings = Vec::new();
    let text = if let Some((encoding, bom_len)) = sniff_bom(&buffer) {
        let (decoded, _, had_errors) = encoding.decode(&buffer[bom_len..]);
        if had_errors {
            warnings.push("按 BOM 指定的编码解码时遇到无法识别的字节".to_owned());
        }
        decoded.into_owned()
    } else {
        // 本地日记文件不是网页，允许 ISO-2022-JP；UTF-8 也允许被猜出来。
        let mut detector = EncodingDetector::new(Iso2022JpDetection::Allow);
        detector.feed(&buffer, true);
        let encoding = detector.guess(None, Utf8Detection::Allow);
        let (decoded, _, had_errors) = encoding.decode(&buffer);
        if encoding != encoding_rs::UTF_8 {
            warnings.push(format!("检测到的编码是 {}", encoding.name()));
        }
        if had_errors {
            warnings.push("解码时遇到无法识别的字节，可能有乱码".to_owned());
        }
        decoded.into_owned()
    };

    Ok((text, warnings, truncated))
}

fn sniff_bom(buffer: &[u8]) -> Option<(&'static encoding_rs::Encoding, usize)> {
    if buffer.starts_with(&[0xEF, 0xBB, 0xBF]) {
        Some((encoding_rs::UTF_8, 3))
    } else if buffer.starts_with(&[0xFF, 0xFE]) {
        Some((encoding_rs::UTF_16LE, 2))
    } else if buffer.starts_with(&[0xFE, 0xFF]) {
        Some((encoding_rs::UTF_16BE, 2))
    } else {
        None
    }
}

/// 按空行切段，并给出每个段落的字符区间（Unicode 标量值计数，左闭右开）。
fn paragraphs_to_outcome(input: &ExtractionInput<'_>, text: &str) -> Result<ExtractionOutcome> {
    let mut segments = Vec::new();
    let mut current = String::new();
    let mut start_char = 0_usize;
    let mut char_count = 0_usize;
    let mut ordinal = 0_i64;

    let push_paragraph = |current: &mut String,
                              start_char: usize,
                              end_char: usize,
                              segments: &mut Vec<ExtractedSegment>,
                              ordinal: &mut i64| {
        let trimmed = current.trim();
        if trimmed.is_empty() {
            current.clear();
            return;
        }
        let trimmed_chars = current.chars().count() - current.trim_end().chars().count();
        let end = end_char.saturating_sub(trimmed_chars);
        segments.push(ExtractedSegment {
            ordinal: *ordinal,
            text: trimmed.to_owned(),
            locator: SourceLocator::text_range(
                input.source_revision_id,
                start_char as i64,
                end as i64,
            ),
        });
        *ordinal += 1;
        current.clear();
    };

    for line in text.split_inclusive('\n') {
        let line_chars = line.chars().count();
        if line.trim().is_empty() {
            push_paragraph(
                &mut current,
                start_char,
                char_count,
                &mut segments,
                &mut ordinal,
            );
            start_char = char_count + line_chars;
        } else {
            if current.is_empty() {
                start_char = char_count;
            }
            current.push_str(line);
        }
        char_count += line_chars;
    }
    push_paragraph(
        &mut current,
        start_char,
        char_count,
        &mut segments,
        &mut ordinal,
    );

    let status = if segments.is_empty() {
        // 空文件不是失败：它就是空的。
        ProcessingStatus::Ready
    } else {
        ProcessingStatus::Ready
    };
    let reason = if segments.is_empty() {
        Some("文件里没有可提取的文字内容".to_owned())
    } else {
        None
    };

    Ok(ExtractionOutcome {
        text: text.to_owned(),
        segments,
        status,
        coverage: Coverage::Complete,
        coverage_reason: reason,
        error_code: None,
        warnings: Vec::new(),
    })
}

// ------------------------------------------------------------------ DOCX

/// DOCX：从 zip 里取 `word/document.xml`，按 `<w:p>` 切段。
///
/// 页码不可靠，所以 locator 是 `document` 且 `pageNumber` 留空——不猜。
pub struct DocxExtractor;

const DOCX_MIME: &str =
    "application/vnd.openxmlformats-officedocument.wordprocessingml.document";

impl Extractor for DocxExtractor {
    fn id(&self) -> &'static str {
        "docx"
    }

    fn version(&self) -> &'static str {
        "1"
    }

    fn supports(&self, mime: &str, original_name: &str) -> bool {
        mime == DOCX_MIME || original_name.to_ascii_lowercase().ends_with(".docx")
    }

    fn extract(&self, input: &ExtractionInput<'_>) -> Result<ExtractionOutcome> {
        let file = File::open(input.path)?;
        let mut archive = zip::ZipArchive::new(file).map_err(|err| CoreError::ExtractionFailed {
            reason: format!("不是有效的 DOCX（zip 打不开）：{err}"),
        })?;
        let mut document = archive
            .by_name("word/document.xml")
            .map_err(|err| CoreError::ExtractionFailed {
                reason: format!("DOCX 里没有 word/document.xml：{err}"),
            })?;
        let mut xml = String::new();
        document.read_to_string(&mut xml)?;
        drop(document);

        let paragraphs = docx_paragraphs(&xml)?;
        let segments: Vec<ExtractedSegment> = paragraphs
            .iter()
            .enumerate()
            .map(|(index, paragraph)| ExtractedSegment {
                ordinal: index as i64,
                text: paragraph.clone(),
                locator: SourceLocator::document(input.source_revision_id, None, None),
            })
            .collect();

        let text = paragraphs.join("\n\n");
        let reason = if segments.is_empty() {
            Some("文档里没有正文文字".to_owned())
        } else {
            Some("页码信息不可靠，定位到文档级".to_owned())
        };

        Ok(ExtractionOutcome {
            text,
            segments,
            status: ProcessingStatus::Ready,
            coverage: if paragraphs.is_empty() {
                Coverage::MetadataOnly
            } else {
                Coverage::Complete
            },
            coverage_reason: reason,
            error_code: None,
            warnings: Vec::new(),
        })
    }
}

fn docx_paragraphs(xml: &str) -> Result<Vec<String>> {
    let mut reader = Reader::from_str(xml);
    let mut paragraphs = Vec::new();
    let mut current = String::new();
    let mut in_text = false;

    loop {
        match reader.read_event() {
            Ok(Event::Start(event)) => match event.name().as_ref() {
                "w:p" => current.clear(),
                "w:t" => in_text = true,
                _ => {}
            },
            Ok(Event::End(event)) => match event.name().as_ref() {
                "w:t" => in_text = false,
                "w:p" => {
                    let trimmed = current.trim();
                    if !trimmed.is_empty() {
                        paragraphs.push(trimmed.to_owned());
                    }
                    current.clear();
                }
                _ => {}
            },
            Ok(Event::Empty(event)) => {
                if in_text {
                    match event.name().as_ref() {
                        "w:tab" => current.push('\t'),
                        "w:br" | "w:cr" => current.push('\n'),
                        _ => {}
                    }
                }
            }
            Ok(Event::Text(event)) => {
                if in_text {
                    current.push_str(&event.xml10_content());
                }
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(err) => {
                return Err(CoreError::ExtractionFailed {
                    reason: format!("DOCX 正文 XML 解析失败：{err}"),
                })
            }
        }
    }
    Ok(paragraphs)
}

// ------------------------------------------------------------------ PDF

/// 文本 PDF：逐页取文本层。没有文本层（扫描件）时如实降级成 `metadata_only`。
pub struct PdfExtractor;

impl Extractor for PdfExtractor {
    fn id(&self) -> &'static str {
        "pdf_text"
    }

    fn version(&self) -> &'static str {
        "1"
    }

    fn supports(&self, mime: &str, original_name: &str) -> bool {
        mime == "application/pdf" || original_name.to_ascii_lowercase().ends_with(".pdf")
    }

    fn extract(&self, input: &ExtractionInput<'_>) -> Result<ExtractionOutcome> {
        let pages = pdf_extract::extract_text_by_pages(input.path).map_err(|err| {
            CoreError::ExtractionFailed {
                reason: format!("PDF 解析失败：{err}"),
            }
        })?;

        let mut segments = Vec::new();
        let mut ordinal = 0_i64;
        let mut empty_pages = Vec::new();
        for (index, page) in pages.iter().enumerate() {
            let trimmed = page.trim();
            if trimmed.is_empty() {
                empty_pages.push(index + 1);
                continue;
            }
            segments.push(ExtractedSegment {
                ordinal,
                text: trimmed.to_owned(),
                // 页码从 1 开始；这里的页码来自 PDF 的实际页序，是可靠的。
                locator: SourceLocator::document(
                    input.source_revision_id,
                    Some((index + 1) as i64),
                    None,
                ),
            });
            ordinal += 1;
        }

        let text = pages
            .iter()
            .map(|page| page.trim())
            .filter(|page| !page.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n");

        if segments.is_empty() {
            // 扫描件：有文件、有页面，但没有文本层。这不是错误，是能力缺失。
            return Ok(ExtractionOutcome {
                text: String::new(),
                segments,
                status: ProcessingStatus::Ready,
                coverage: Coverage::MetadataOnly,
                coverage_reason: Some(
                    "这份 PDF 没有文本层（可能是扫描件），需要 OCR 才能搜正文；\
                     目前仍可按文件名与时间找到它"
                        .to_owned(),
                ),
                error_code: None,
                warnings: Vec::new(),
            });
        }

        let coverage = if empty_pages.is_empty() {
            Coverage::Complete
        } else {
            Coverage::Partial
        };
        let reason = if empty_pages.is_empty() {
            None
        } else {
            Some(format!("第 {empty_pages:?} 页没有文本层，未提取到内容"))
        };

        Ok(ExtractionOutcome {
            text,
            segments,
            status: if coverage == Coverage::Complete {
                ProcessingStatus::Ready
            } else {
                ProcessingStatus::Partial
            },
            coverage,
            coverage_reason: reason,
            error_code: None,
            warnings: Vec::new(),
        })
    }
}

// ------------------------------------------------------------------ 能力缺失

/// 图片：没有 OCR / 画面描述能力时，只保留文件信息。
pub struct ImageMetadataOnlyExtractor;

impl Extractor for ImageMetadataOnlyExtractor {
    fn id(&self) -> &'static str {
        "image_metadata_only"
    }

    fn version(&self) -> &'static str {
        "1"
    }

    fn supports(&self, mime: &str, _original_name: &str) -> bool {
        mime.starts_with("image/")
    }

    fn extract(&self, input: &ExtractionInput<'_>) -> Result<ExtractionOutcome> {
        let _ = input;
        Ok(ExtractionOutcome::metadata_only(
            Coverage::MetadataOnly,
            "没有配置 OCR 或画面描述能力，目前只能按文件名、时间和备注找到这张图",
        ))
    }
}

/// 音视频：转写要接 provider，现在还没有。
pub struct MediaMetadataOnlyExtractor;

impl Extractor for MediaMetadataOnlyExtractor {
    fn id(&self) -> &'static str {
        "media_metadata_only"
    }

    fn version(&self) -> &'static str {
        "1"
    }

    fn supports(&self, mime: &str, _original_name: &str) -> bool {
        mime.starts_with("audio/") || mime.starts_with("video/")
    }

    fn extract(&self, input: &ExtractionInput<'_>) -> Result<ExtractionOutcome> {
        let _ = input;
        Ok(ExtractionOutcome::metadata_only(
            Coverage::MetadataOnly,
            "还没有配置转写能力，原音频可以保存和播放，但正文暂时搜不到",
        ))
    }
}

/// 其它二进制：明确不支持，但文件本身照样留着。
pub struct UnsupportedExtractor;

impl Extractor for UnsupportedExtractor {
    fn id(&self) -> &'static str {
        "unsupported"
    }

    fn version(&self) -> &'static str {
        "1"
    }

    fn supports(&self, _mime: &str, _original_name: &str) -> bool {
        true
    }

    fn extract(&self, input: &ExtractionInput<'_>) -> Result<ExtractionOutcome> {
        let _ = input;
        Ok(ExtractionOutcome {
            text: String::new(),
            segments: Vec::new(),
            status: ProcessingStatus::Failed,
            coverage: Coverage::Unavailable,
            coverage_reason: Some("这个格式还不能提取正文，文件本身已经保存".to_owned()),
            error_code: Some(ErrorCode::UnsupportedFormat),
            warnings: Vec::new(),
        })
    }
}

// ------------------------------------------------------------------ 服务

/// 对某个来源修订跑一次提取，并把结果写进派生内容表。
///
/// 可重建：先删掉这个修订的旧派生内容再写新的，原件一个字节都不动。
pub(crate) fn extract_source_revision(
    core: &mut Core,
    source_ref: &str,
) -> Result<ExtractedContent> {
    let root = core.root_dir()?.to_path_buf();
    // 调用方给来源 id 或修订 id 都行：给来源就取它当前的修订。
    let source_revision_id = resolve_source_ref(&core.conn, source_ref)?
        .map(|(_, revision_id)| revision_id)
        .filter(|revision_id| !revision_id.is_empty())
        .unwrap_or_else(|| source_ref.to_owned());
    let source_revision_id = source_revision_id.as_str();
    let (source_id, asset_id, mime, name, object_ref) =
        load_revision_context(&core.conn, source_revision_id)?;

    let path = match object_ref {
        Some(object_ref) => root.join(object_ref),
        None => {
            return Err(CoreError::InvalidState {
                entity: "来源修订",
                id: source_revision_id.to_owned(),
                state: "没有关联原件，无法提取".to_owned(),
            })
        }
    };
    if !path.is_file() {
        return Err(CoreError::AssetMissing {
            asset_id: asset_id.clone(),
            reason: "原件文件不在文件库里".to_owned(),
        });
    }

    let input = ExtractionInput {
        asset_id: &asset_id,
        source_id: &source_id,
        source_revision_id,
        mime: &mime,
        original_name: &name,
        path: &path,
    };

    let extractor = builtin_extractors()
        .into_iter()
        .find(|candidate| candidate.supports(&mime, &name))
        .expect("最后一个提取器支持所有类型");

    // 提取失败不是「没有内容」：失败也要落一条记录，让界面看到真实原因。
    let outcome = match extractor.extract(&input) {
        Ok(outcome) => outcome,
        Err(error) => ExtractionOutcome {
            text: String::new(),
            segments: Vec::new(),
            status: ProcessingStatus::Failed,
            coverage: Coverage::Unavailable,
            coverage_reason: Some(error.to_string()),
            error_code: Some(error.code()),
            warnings: Vec::new(),
        },
    };
    persist(core, &input, extractor.id(), extractor.version(), outcome)
}

fn persist(
    core: &mut Core,
    input: &ExtractionInput<'_>,
    extractor_id: &str,
    extractor_version: &str,
    outcome: ExtractionOutcome,
) -> Result<ExtractedContent> {
    let now = support::now();
    let content = ExtractedContent {
        id: support::new_id("ext"),
        source_id: input.source_id.to_owned(),
        source_revision_id: input.source_revision_id.to_owned(),
        extractor_id: extractor_id.to_owned(),
        extractor_version: extractor_version.to_owned(),
        text: outcome.text,
        segments: outcome.segments,
        status: outcome.status,
        coverage: outcome.coverage,
        coverage_reason: outcome.coverage_reason,
        error_code: outcome.error_code.map(|code| code.wire().to_owned()),
        created_at: now,
    };

    let tx = core.conn.transaction()?;
    // 重建语义：同一个修订的旧派生内容整体替换。
    tx.execute(
        "DELETE FROM extracted_contents WHERE source_revision_id = ?1",
        params![content.source_revision_id],
    )?;
    tx.execute(
        "INSERT INTO extracted_contents (id, source_id, source_revision_id, extractor_id, \
         extractor_version, text, status, coverage, coverage_reason, error_code, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            content.id,
            content.source_id,
            content.source_revision_id,
            content.extractor_id,
            content.extractor_version,
            content.text,
            content.status.wire(),
            content.coverage.wire(),
            content.coverage_reason,
            content.error_code,
            support::to_iso(now),
        ],
    )?;
    for segment in &content.segments {
        let segment_id = support::new_id("seg");
        tx.execute(
            "INSERT INTO extracted_segments (id, content_id, ordinal, text, locator_type, \
             source_revision_id, text_start, text_end, start_ms, end_ms, page_number, block_id, \
             rect_left, rect_top, rect_right, rect_bottom, asset_id) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
            params![
                segment_id,
                content.id,
                segment.ordinal,
                segment.text,
                segment.locator.locator_type.wire(),
                segment.locator.source_revision_id,
                segment.locator.text_start,
                segment.locator.text_end,
                segment.locator.start_ms,
                segment.locator.end_ms,
                segment.locator.page_number,
                segment.locator.block_id,
                segment.locator.rect.map(|rect| rect[0]),
                segment.locator.rect.map(|rect| rect[1]),
                segment.locator.rect.map(|rect| rect[2]),
                segment.locator.rect.map(|rect| rect[3]),
                segment.locator.asset_id,
            ],
        )?;
        // 关键词索引与派生内容在同一个事务里：材料写进去了、索引却没写，
        // 就会出现「检索不到刚导入的东西」而没人知道原因（任务书 3.3 节）。
        search::index_segment(
            &tx,
            search::SegmentInput {
                segment_id: &segment_id,
                text: &segment.text,
            },
        )?;
    }
    tx.commit()?;
    Ok(content)
}

/// 读取某个来源当前版本的派生内容。没有提取过时返回 None。
pub(crate) fn extracted_content(
    core: &Core,
    source_id: &str,
) -> Result<Option<ExtractedContent>> {
    let row: Option<ExtractedContentRow> = core.conn
            .query_row(
                "SELECT c.id, c.source_id, c.source_revision_id, c.extractor_id, \
                 c.extractor_version, c.text, c.status, c.coverage, c.coverage_reason, \
                 c.error_code, c.created_at FROM extracted_contents c \
                 JOIN source_items i ON i.source_id = c.source_id \
                 WHERE c.source_id = ?1 AND (i.current_revision_id IS NULL \
                 OR c.source_revision_id = i.current_revision_id)",
                params![source_id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                        row.get(7)?,
                        row.get(8)?,
                        row.get(9)?,
                        row.get(10)?,
                    ))
                },
            )
            .optional()?;

    let Some(row) = row else {
        return Ok(None);
    };

    let segments = load_segments(&core.conn, &row.0)?;
    Ok(Some(ExtractedContent {
        id: row.0,
        source_id: row.1,
        source_revision_id: row.2,
        extractor_id: row.3,
        extractor_version: row.4,
        text: row.5,
        segments,
        status: ProcessingStatus::from_wire(&row.6).ok_or_else(|| CoreError::CorruptedData {
            message: format!("未知派生内容状态：{}", row.6),
        })?,
        coverage: Coverage::from_wire(&row.7).ok_or_else(|| CoreError::CorruptedData {
            message: format!("未知覆盖程度：{}", row.7),
        })?,
        coverage_reason: row.8,
        error_code: row.9,
        created_at: support::parse_iso(&row.10)?,
    }))
}

/// 派生内容主表的一行，避免把超长元组类型写在函数体里。
type ExtractedContentRow = (
    String,
    String,
    String,
    String,
    String,
    String,
    String,
    String,
    Option<String>,
    Option<String>,
    String,
);

/// 把 sourceRef + locator 解析成前端可打开的原件与可用性。
pub(crate) fn locate(
    core: &Core,
    source_ref: &str,
    locator: SourceLocator,
) -> Result<SourceLocation> {
    let root = core.root_dir()?.to_path_buf();
    // sourceRef 可以是来源 id，也可以是某一次修订 id。
    let resolved = resolve_source_ref(&core.conn, source_ref)?;
    let Some((source_id, revision_id)) = resolved else {
        return Ok(SourceLocation {
            source_ref: source_ref.to_owned(),
            locator,
            available: false,
            asset_id: None,
            reason: Some("找不到这个来源".to_owned()),
        });
    };

    let asset: Option<(String, String, String)> = core
        .conn
        .query_row(
            "SELECT a.id, a.object_ref, a.storage_state FROM source_revisions r \
             JOIN assets a ON a.id = r.asset_id WHERE r.revision_id = ?1",
            params![revision_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;

    let Some((asset_id, object_ref, state)) = asset else {
        return Ok(SourceLocation {
            source_ref: source_id.clone(),
            locator,
            available: false,
            asset_id: None,
            reason: Some("这个来源没有可打开的原件（例如纯文字）".to_owned()),
        });
    };

    if state != "ready" {
        return Ok(SourceLocation {
            source_ref: source_id,
            locator,
            available: false,
            asset_id: Some(asset_id),
            reason: Some(format!("原件状态是 {state}，暂时打不开")),
        });
    }
    if !root.join(&object_ref).is_file() {
        return Ok(SourceLocation {
            source_ref: source_id,
            locator,
            available: false,
            asset_id: Some(asset_id),
            reason: Some("原件文件不在文件库里".to_owned()),
        });
    }

    Ok(SourceLocation {
        source_ref: source_id,
        locator,
        available: true,
        asset_id: Some(asset_id),
        reason: None,
    })
}

fn resolve_source_ref(conn: &Connection, source_ref: &str) -> Result<Option<(String, String)>> {
    // 先按来源 id 找当前修订。
    let by_source: Option<(String, String)> = conn
        .query_row(
            "SELECT source_id, COALESCE(current_revision_id, '') FROM source_items \
             WHERE source_id = ?1",
            params![source_ref],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    if let Some((source_id, revision_id)) = by_source {
        return Ok(Some((source_id, revision_id)));
    }

    // 再按修订 id 反查来源。
    let by_revision: Option<(String, String)> = conn
        .query_row(
            "SELECT source_id, revision_id FROM source_revisions WHERE revision_id = ?1",
            params![source_ref],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    Ok(by_revision)
}

fn load_revision_context(
    conn: &Connection,
    revision_id: &str,
) -> Result<(String, String, String, String, Option<String>)> {
    let row = conn
        .query_row(
            "SELECT r.source_id, COALESCE(r.asset_id, ''), COALESCE(a.detected_mime, ''), \
             COALESCE(a.original_name, ''), a.object_ref \
             FROM source_revisions r LEFT JOIN assets a ON a.id = r.asset_id \
             WHERE r.revision_id = ?1",
            params![revision_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<String>>(4)?,
                ))
            },
        )
        .optional()?
        .ok_or_else(|| CoreError::NotFound {
            entity: "来源修订",
            id: revision_id.to_owned(),
        })?;
    Ok((row.0, row.1, row.2, row.3, row.4))
}

fn load_segments(conn: &Connection, content_id: &str) -> Result<Vec<ExtractedSegment>> {
    let mut stmt = conn.prepare(
        "SELECT ordinal, text, locator_type, source_revision_id, text_start, text_end, start_ms, \
         end_ms, page_number, block_id, rect_left, rect_top, rect_right, rect_bottom, asset_id \
         FROM extracted_segments WHERE content_id = ?1 ORDER BY ordinal",
    )?;
    let rows = stmt
        .query_map(params![content_id], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, Option<i64>>(4)?,
                row.get::<_, Option<i64>>(5)?,
                row.get::<_, Option<i64>>(6)?,
                row.get::<_, Option<i64>>(7)?,
                row.get::<_, Option<i64>>(8)?,
                row.get::<_, Option<String>>(9)?,
                row.get::<_, Option<f64>>(10)?,
                row.get::<_, Option<f64>>(11)?,
                row.get::<_, Option<f64>>(12)?,
                row.get::<_, Option<f64>>(13)?,
                row.get::<_, Option<String>>(14)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    rows.into_iter()
        .map(|row| {
            let rect = match (row.10, row.11, row.12, row.13) {
                (Some(left), Some(top), Some(right), Some(bottom)) => {
                    Some([left, top, right, bottom])
                }
                _ => None,
            };
            Ok(ExtractedSegment {
                ordinal: row.0,
                text: row.1,
                locator: SourceLocator {
                    locator_type: crate::model::LocatorType::from_wire(&row.2).ok_or_else(
                        || CoreError::CorruptedData {
                            message: format!("未知定位类型：{}", row.2),
                        },
                    )?,
                    source_revision_id: row.3,
                    text_start: row.4,
                    text_end: row.5,
                    start_ms: row.6,
                    end_ms: row.7,
                    page_number: row.8,
                    block_id: row.9,
                    rect,
                    asset_id: row.14,
                },
            })
        })
        .collect()
}
//! B2 的提取与来源定位测试。
//!
//! 对应验收计划的 E16（没有 OCR 能力时图片是 metadata_only、不误报可搜）
//! 与 E17（文本 PDF / DOCX / 扫描件 / 不支持文件的 coverage 与错误码）。

use std::io::Write;
use std::path::Path;

use diary_core::{
    Core, Coverage, CreateDraftInput, ImportManifest, ImportOrigin, ImportRequest, JobState,
    ProcessingStatus, SourceLocator,
};
use sha2::{Digest, Sha256};

fn sha_of(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

/// 把一个文件"导入"进资料库，返回 (capture_id, source_id, asset_id)。
fn import_file(
    core: &mut Core,
    name: &str,
    bytes: &[u8],
    mime: &str,
    operation_id: &str,
) -> (String, String, String) {
    let capture_id = core
        .create_draft(CreateDraftInput {
            occurred_at: None,
            time_zone: "Asia/Shanghai",
            utc_offset_minutes: 480,
            operation_id: &format!("{operation_id}-create"),
        })
        .unwrap()
        .id;
    let ticket = core
        .prepare_import(ImportRequest {
            capture_id: &capture_id,
            display_name: name,
            mime_hint: Some(mime),
            size_hint: Some(bytes.len() as i64),
            origin: ImportOrigin::Picker,
            operation_id: &format!("{operation_id}-prepare"),
        })
        .unwrap();
    std::fs::write(&ticket.staging_ticket, bytes).unwrap();
    let status = core
        .finish_import(
            &ticket.import_id,
            &ticket.staging_ticket,
            ImportManifest {
                copied_bytes: bytes.len() as i64,
                sha256: sha_of(bytes),
                detected_mime: mime.to_owned(),
                original_name: name.to_owned(),
            },
        )
        .unwrap();
    let asset_id = status.asset_id.expect("导入完成后应当有资产");
    let source_id = core
        .get_capture(&capture_id)
        .unwrap()
        .ordered_source_ids
        .first()
        .cloned()
        .expect("材料应当挂到记录上");
    (capture_id, source_id, asset_id)
}

fn new_core(dir: &Path) -> Core {
    Core::open_in_memory_at(dir).unwrap()
}

// ------------------------------------------------------------ fixture 构造

/// 最小但合法的 DOCX：一个 zip，里面有 [Content_Types].xml、_rels/.rels 与正文。
fn build_docx(paragraphs: &[&str]) -> Vec<u8> {
    let mut body = String::new();
    for paragraph in paragraphs {
        body.push_str(&format!(
            "<w:p><w:r><w:t>{paragraph}</w:t></w:r></w:p>"
        ));
    }
    let document = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{body}</w:body></w:document>"#
    );
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;

    let mut cursor = std::io::Cursor::new(Vec::new());
    {
        let mut writer = zip::ZipWriter::new(&mut cursor);
        let options = zip::write::SimpleFileOptions::default();
        for (name, content) in [
            ("[Content_Types].xml", content_types),
            ("_rels/.rels", rels),
            ("word/document.xml", document.as_str()),
        ] {
            writer.start_file(name, options).unwrap();
            writer.write_all(content.as_bytes()).unwrap();
        }
        writer.finish().unwrap();
    }
    cursor.into_inner()
}

/// 最小合法 PDF：每页一个内容流。`pages` 里给空串就是没有文本层的页。
fn build_pdf(pages: &[&str]) -> Vec<u8> {
    let page_count = pages.len();
    let first_page_object = 3;
    let font_object = first_page_object + page_count * 2;

    let kids: Vec<String> = (0..page_count)
        .map(|index| format!("{} 0 R", first_page_object + index * 2))
        .collect();
    let mut objects: Vec<String> = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        format!(
            "<< /Type /Pages /Kids [{}] /Count {page_count} >>",
            kids.join(" ")
        ),
    ];
    for (index, text) in pages.iter().enumerate() {
        let content_object = first_page_object + index * 2 + 1;
        objects.push(format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents {content_object} 0 R \
             /Resources << /Font << /F1 {font_object} 0 R >> >> >>"
        ));
        let content = if text.is_empty() {
            "0 0 612 792 re f".to_owned()
        } else {
            format!("BT /F1 24 Tf 72 700 Td ({text}) Tj ET")
        };
        objects.push(format!(
            "<< /Length {} >>\nstream\n{content}\nendstream",
            content.len()
        ));
    }
    objects.push("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_owned());

    let mut out = String::from("%PDF-1.4\n");
    let mut offsets = Vec::new();
    for (index, body) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.push_str(&format!("{} 0 obj\n{body}\nendobj\n", index + 1));
    }
    let xref_offset = out.len();
    out.push_str(&format!("xref\n0 {}\n", objects.len() + 1));
    out.push_str("0000000000 65535 f \n");
    for offset in &offsets {
        out.push_str(&format!("{offset:010} 00000 n \n"));
    }
    out.push_str(&format!(
        "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n",
        objects.len() + 1
    ));
    out.into_bytes()
}

// ------------------------------------------------------------ 用例

#[test]
fn plain_text_paragraphs_have_character_ranges() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    let text = "第一段第一行\n第一段第二行\n\n第二段\n";
    let (_, source_id, _) = import_file(
        &mut core,
        "日记.txt",
        text.as_bytes(),
        "text/plain",
        "op1",
    );

    let content = core.extract_source(&source_id).unwrap();
    assert_eq!(content.coverage, Coverage::Complete);
    assert_eq!(content.status, ProcessingStatus::Ready);
    assert_eq!(content.extractor_id, "plain_text");
    assert_eq!(content.segments.len(), 2, "空行应当切出两段");

    let first = &content.segments[0];
    assert_eq!(first.text, "第一段第一行\n第一段第二行");
    assert_eq!(first.locator.text_start, Some(0));
    // 区间按 Unicode 标量值计数、左闭右开。
    assert_eq!(first.locator.text_end, Some(13), "段落区间不含结尾换行");
    assert_eq!(first.locator.locator_type.wire(), "text_range");

    let second = &content.segments[1];
    assert_eq!(second.text, "第二段");
    // 第一段的 13 个字 + 两个换行（段尾的空行）之后才是第二段。
    assert_eq!(second.locator.text_start, Some(15));
    assert_eq!(second.locator.text_end, Some(18));
}

#[test]
fn gbk_text_is_detected_and_decoded() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    // 真实长度的 GBK 文本。太短的样本连检测器都会猜错（6 个字节时它猜成泰文），
    // 所以这里用一句正常长度的话，这也是真实使用中的形态。
    let original = "今天妈妈打电话来说她身体不太好，我有点担心，打算周末回去看看。";
    let (gbk, _, had_errors) = encoding_rs::GBK.encode(original);
    assert!(!had_errors, "测试样本应当能编成 GBK");
    let (_, source_id, _) = import_file(&mut core, "旧文件.txt", &gbk, "text/plain", "op1");

    let content = core.extract_source(&source_id).unwrap();
    assert!(
        content.text.contains("妈妈打电话"),
        "GBK 中文必须正确读出，实际得到：{}",
        content.text
    );
    assert!(
        !content.text.contains('\u{FFFD}'),
        "不该出现替换字符（乱码）：{}",
        content.text
    );
}

#[test]
fn an_empty_file_is_not_a_failure() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    let (_, source_id, _) = import_file(&mut core, "空.txt", b"", "text/plain", "op1");

    let content = core.extract_source(&source_id).unwrap();
    assert_eq!(content.status, ProcessingStatus::Ready, "空文件不是解析失败");
    assert_eq!(content.coverage, Coverage::Complete);
    assert!(content.segments.is_empty());
    assert!(
        content
            .coverage_reason
            .as_deref()
            .unwrap_or_default()
            .contains("没有可提取的文字"),
        "要说清楚是空的：{:?}",
        content.coverage_reason
    );
    assert!(content.error_code.is_none());
}

#[test]
fn docx_paragraphs_are_extracted_without_inventing_page_numbers() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    let bytes = build_docx(&["第一段正文", "第二段正文", ""]);
    let (_, source_id, _) = import_file(
        &mut core,
        "文档.docx",
        &bytes,
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "op1",
    );

    let content = core.extract_source(&source_id).unwrap();
    assert_eq!(content.extractor_id, "docx");
    assert_eq!(content.coverage, Coverage::Complete);
    let texts: Vec<&str> = content.segments.iter().map(|s| s.text.as_str()).collect();
    assert_eq!(texts, vec!["第一段正文", "第二段正文"], "空段落应当被跳过");
    assert_eq!(content.segments[0].locator.locator_type.wire(), "document");
    assert_eq!(
        content.segments[0].locator.page_number, None,
        "页码不可靠时不能杜撰"
    );
}

#[test]
fn a_text_pdf_is_extracted_page_by_page() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    let bytes = build_pdf(&["Page one has text", "Page two also has text"]);
    let (_, source_id, _) = import_file(&mut core, "报告.pdf", &bytes, "application/pdf", "op1");

    let content = core.extract_source(&source_id).unwrap();
    assert_eq!(content.extractor_id, "pdf_text");
    assert_eq!(content.coverage, Coverage::Complete);
    assert_eq!(content.segments.len(), 2, "两页应当有两段");
    assert!(content.segments[0].text.contains("Page one"));
    assert_eq!(content.segments[0].locator.page_number, Some(1));
    assert_eq!(content.segments[1].locator.page_number, Some(2));
}

#[test]
fn a_scanned_pdf_is_metadata_only_not_empty() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    // 内容流只画了个方块，没有文本层。
    let bytes = build_pdf(&[""]);
    let (_, source_id, _) = import_file(&mut core, "扫描件.pdf", &bytes, "application/pdf", "op1");

    let content = core.extract_source(&source_id).unwrap();
    assert_eq!(content.coverage, Coverage::MetadataOnly);
    assert!(
        content.segments.is_empty(),
        "没有文本层就不该编出内容来"
    );
    let reason = content.coverage_reason.unwrap_or_default();
    assert!(reason.contains("OCR"), "要说清需要 OCR：{reason}");
    assert!(
        content.error_code.is_none(),
        "扫描件不是错误，是能力缺失"
    );
}

#[test]
fn an_image_without_ocr_is_metadata_only() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    let (_, source_id, _) = import_file(&mut core, "截图.png", b"\x89PNG fake", "image/png", "op1");

    let content = core.extract_source(&source_id).unwrap();
    assert_eq!(content.coverage, Coverage::MetadataOnly);
    assert_eq!(content.extractor_id, "image_metadata_only");
    let reason = content.coverage_reason.unwrap_or_default();
    assert!(reason.contains("OCR"), "要说清为什么搜不到正文：{reason}");
    assert!(
        reason.contains("文件名"),
        "要说明还能按文件名找到：{reason}"
    );
}

#[test]
fn an_unsupported_binary_reports_unsupported_format() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    let (_, source_id, _) = import_file(
        &mut core,
        "安装包.bin",
        &[0x00, 0x01, 0x02, 0x03, 0xFF],
        "application/octet-stream",
        "op1",
    );

    let content = core.extract_source(&source_id).unwrap();
    assert_eq!(content.status, ProcessingStatus::Failed);
    assert_eq!(content.coverage, Coverage::Unavailable);
    assert_eq!(content.error_code.as_deref(), Some("unsupported_format"));
    // 原件必须留着。
    assert_eq!(core.asset_stats().unwrap().0, 1);
}

#[test]
fn a_corrupt_docx_is_reported_rather_than_treated_as_empty() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    let (_, source_id, _) = import_file(
        &mut core,
        "坏文档.docx",
        b"this is not a zip at all",
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "op1",
    );

    let content = core.extract_source(&source_id).unwrap();
    assert_eq!(content.status, ProcessingStatus::Failed, "解析失败不能当成空文件");
    assert_eq!(content.coverage, Coverage::Unavailable);
    assert_eq!(content.error_code.as_deref(), Some("unsupported_format"));
    assert!(
        content
            .coverage_reason
            .as_deref()
            .unwrap_or_default()
            .contains("DOCX"),
        "失败原因要能看懂：{:?}",
        content.coverage_reason
    );
}

#[test]
fn extraction_is_rebuildable_and_leaves_the_original_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    let text = "第一段\n\n第二段\n";
    let (_, source_id, asset_id) = import_file(
        &mut core,
        "可重建.txt",
        text.as_bytes(),
        "text/plain",
        "op1",
    );
    let lease = core.open_asset(&asset_id, "preview").unwrap();
    let before = std::fs::read(&lease.handle).unwrap();

    let first = core.extract_source(&source_id).unwrap();
    let second = core.extract_source(&source_id).unwrap();

    assert_eq!(second.segments.len(), first.segments.len());
    // 只有一份派生内容：重建是替换，不是追加。
    let stored = core.extracted_content(&source_id).unwrap().unwrap();
    assert_eq!(stored.segments.len(), 2);
    // 原件一个字节都没动。
    assert_eq!(std::fs::read(&lease.handle).unwrap(), before);
}

#[test]
fn importing_a_file_queues_an_extract_job() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    let (_, source_id, _) = import_file(&mut core, "待提取.txt", b"content", "text/plain", "op1");

    let jobs = core.list_jobs(Some(&[JobState::Queued]), 10).unwrap();
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].kind, "extract");
    assert_eq!(jobs[0].target_ids.len(), 1, "任务应当指向一个明确的目标");
    // 任务指向的是「源修订」——提取真正需要的东西。用它当输入必须能直接跑通。
    let content = core.extract_source(&jobs[0].target_ids[0]).unwrap();
    assert_eq!(content.segments.len(), 1);

    // 导入还排了一条**等提取**的关键词索引任务（指向来源，不是修订）：
    // 这一刻提取还没跑，它落在 waiting_dependency，不消耗重试额度。
    let waiting = core
        .list_jobs(Some(&[JobState::WaitingDependency]), 10)
        .unwrap();
    assert_eq!(waiting.len(), 1);
    assert_eq!(waiting[0].kind, "index");
    assert_eq!(waiting[0].target_ids, vec![source_id]);
    assert_eq!(waiting[0].error_code.as_deref(), Some("waiting_extract"));
    assert_eq!(waiting[0].attempt_count, 0, "等前置不是失败，不动尝试次数");
}

#[test]
fn locate_resolves_a_source_to_its_asset() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    let (_, source_id, asset_id) = import_file(
        &mut core,
        "可定位.txt",
        b"locate me",
        "text/plain",
        "op1",
    );

    let locator = SourceLocator::text_range("rev-any", 0, 9);
    let location = core.locate_source(&source_id, locator.clone()).unwrap();
    assert!(location.available, "原件在文件库里就该可用");
    assert_eq!(location.asset_id.as_deref(), Some(asset_id.as_str()));
    assert_eq!(location.locator, locator);

    // 找不到的来源：如实说找不到，而不是报错或假装可用。
    let missing = core.locate_source("src_missing", locator).unwrap();
    assert!(!missing.available);
    assert!(missing.reason.unwrap_or_default().contains("找不到"));

    // 纯文字来源没有原件，可用性是 false。
    let mut core2 = new_core(tempfile::tempdir().unwrap().path());
    let capture_id = core2
        .create_draft(CreateDraftInput {
            occurred_at: None,
            time_zone: "Asia/Shanghai",
            utc_offset_minutes: 480,
            operation_id: "op-text",
        })
        .unwrap()
        .id;
    core2.save_draft(&capture_id, "纯文字", 1, "op-save").unwrap();
    let committed = core2.commit(&capture_id, 2, "op-commit").unwrap();
    let text_source = committed.capture.ordered_source_ids[0].clone();
    let text_location = core2
        .locate_source(&text_source, SourceLocator::text_range("rev", 0, 3))
        .unwrap();
    assert!(!text_location.available);
    assert!(text_location
        .reason
        .unwrap_or_default()
        .contains("没有可打开的原件"));
}

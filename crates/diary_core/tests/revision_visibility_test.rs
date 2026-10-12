//! 改过的正文（issue #54）：新修订进索引、旧修订退出默认检索、可按需读上一版。
//!
//! 三条决策在这里各有一组断言：
//! 1. **改过的正文当成新的一篇**：新修订的正文要能被搜到，旧修订不再出现在默认
//!    检索里——但旧修订的派生内容**不删**（只是不参与默认命中，见 `rebuild` 那条）；
//! 2. **状态数字与默认搜索自洽**：片段类计数只算当前修订，并说明有多少片段属于
//!    旧修订、不计入数字；
//! 3. **能按需读上一版**：`previous_source_revision` 给上一版正文，diff 由前端算。
//!
//! 语料是**同一话题的改写**，不是换话题：换话题只能测出「机制通不通」，
//! 测不到「共享词还活着、摘录来自新正文」这两件事（评审指出）。

use std::path::Path;

use diary_core::{
    Core, Coverage, CreateDraftInput, ErrorCode, ImportManifest, ImportOrigin, ImportRequest,
    LocatorType, ProcessingStatus, SearchFilters, SearchHit, SearchMode, SearchRequest,
    SourceRevision,
};
use sha2::{Digest, Sha256};

/// 真实修改的旧版：只存在于这一版的措辞是「不太好」。
const OLD_BODY: &str = "今天去医院拿报告，体检结果不太好，医生让我下个月复查。";
/// 真实修改的新版：与旧版共享「今天去医院拿报告」「体检结果」「医生让我下个月复查」，
/// 删掉「不太好」，补上「还行」「血脂偏高」。
const NEW_BODY: &str = "今天去医院拿报告，体检结果还行，只是血脂偏高，医生让我下个月复查。";

fn sha_of(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

fn new_core(dir: &Path) -> Core {
    Core::open_in_memory_at(dir).unwrap()
}

/// 导入一段文本并提取，返回 (capture_id, source_id)。草稿文字留空：这样命中只会
/// 来自派生片段，不会和「用户自己写的文字」那一路混在一起。
fn import_and_extract(
    core: &mut Core,
    name: &str,
    text: &str,
    operation_id: &str,
) -> (String, String) {
    let bytes = text.as_bytes();
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
            mime_hint: Some("text/plain"),
            size_hint: Some(bytes.len() as i64),
            origin: ImportOrigin::Picker,
            operation_id: &format!("{operation_id}-prepare"),
        })
        .unwrap();
    std::fs::write(&ticket.staging_ticket, bytes).unwrap();
    core.finish_import(
        &ticket.import_id,
        &ticket.staging_ticket,
        ImportManifest {
            copied_bytes: bytes.len() as i64,
            sha256: sha_of(bytes),
            detected_mime: "text/plain".to_owned(),
            original_name: name.to_owned(),
        },
    )
    .unwrap();
    let source_id = core
        .get_capture(&capture_id)
        .unwrap()
        .ordered_source_ids
        .first()
        .cloned()
        .expect("材料应当挂到记录上");
    core.extract_source(&source_id).unwrap();
    (capture_id, source_id)
}

/// 提交记录。`expected_revision` 从库里现读：写死数字的测试会在别处推进 revision 时
/// 莫名其妙地红（导入就推进过一次）。
fn commit(core: &mut Core, capture_id: &str, operation_id: &str) {
    let revision = core.get_capture(capture_id).unwrap().revision;
    core.commit(capture_id, revision, operation_id).unwrap();
}

/// 改这一篇的原始文字。乐观锁以**记录**的 revision 为准，所以这里现读。
fn revise_text(
    core: &mut Core,
    capture_id: &str,
    source_id: &str,
    text: &str,
    operation_id: &str,
) -> SourceRevision {
    let revision = core.get_capture(capture_id).unwrap().revision;
    core.revise_text(source_id, text, revision, operation_id)
        .unwrap()
}

fn search(core: &mut Core, query: &str) -> Vec<SearchHit> {
    core.start_search(
        SearchRequest {
            query: query.to_owned(),
            mode: SearchMode::Keyword,
            filters: SearchFilters::default(),
            page_size: 20,
        },
        1,
    )
    .unwrap()
    .results
}

fn physical_segment_docs(path: &Path) -> i64 {
    rusqlite::Connection::open(path)
        .unwrap()
        .query_row("SELECT COUNT(*) FROM search_docs", [], |row| row.get(0))
        .unwrap()
}

// ------------------------------------------------------------ 验收：默认搜索

/// 验收的主用例：**同一话题的真实修改**，三条断言一条都不能少。
///
/// 为什么三条都要：只断言「旧词搜不到」时，一个「把整条来源都排除掉」的错误实现
/// 也能过；只断言「新词搜得到」时，「旧修订没退出」也能过。
#[test]
fn revising_the_body_replaces_what_default_search_returns() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.sqlite");
    let mut core = Core::open(&path).unwrap();
    let (capture_id, source_id) = import_and_extract(&mut core, "体检报告.txt", OLD_BODY, "op");

    // 改之前：旧措辞在，新措辞不在。
    assert_eq!(core.count_search_matches("体检").unwrap(), 1, "共享词改前应当命中");
    assert_eq!(core.count_search_matches("不太好").unwrap(), 1, "旧措辞改前应当命中");
    assert_eq!(core.count_search_matches("血脂").unwrap(), 0, "新措辞改前不该命中");

    commit(&mut core, &capture_id, "op-commit");
    let revised = revise_text(&mut core, &capture_id, &source_id, NEW_BODY, "op-revise");
    assert_eq!(revised.text.as_deref(), Some(NEW_BODY), "修订自己带着正文");

    // 改正文会**顺手产出派生内容**（纯文本提取不碰文件，代价很小），所以这里不用
    // 调用方再提取一次——留一个「改完却搜不到」的窗口对高频操作是不可接受的。
    let content = core
        .extracted_content(&source_id)
        .unwrap()
        .expect("改完就应当有派生内容");
    assert_eq!(content.coverage, Coverage::Complete, "纯文本修订要能提取成功");
    assert_eq!(
        content.extractor_id,
        diary_core::TEXT_DIRECT_EXTRACTOR_ID,
        "走的是「正文就是内容」这条提取路径"
    );

    // 1) 共享词仍然命中，而且摘录来自**新**正文。
    let hits = search(&mut core, "体检");
    assert_eq!(hits.len(), 1, "共享词「体检」应当命中 1 条");
    let snippet = hits[0].snippet.as_deref().unwrap_or_default();
    assert!(
        snippet.contains("体检结果还行"),
        "摘录必须来自新正文，实际是：{snippet}"
    );
    assert!(
        !snippet.contains("不太好"),
        "摘录里不该出现只存在于旧正文的措辞，实际是：{snippet}"
    );

    // 2) 只存在于旧修订的措辞搜不到。
    assert_eq!(
        core.count_search_matches("不太好").unwrap(),
        0,
        "旧措辞必须退出默认检索"
    );
    assert!(search(&mut core, "不太好").is_empty());

    // 3) 只存在于新修订的措辞能搜到。
    assert_eq!(core.count_search_matches("血脂").unwrap(), 1, "新措辞要进索引");
    assert_eq!(search(&mut core, "血脂").len(), 1);

    // 旧修订的派生内容与索引行都**还在**：查询侧过滤才是挡住它的那一步。
    // 这一条钉住「不删旧数据」的实现选择（将来做「只搜变更前的内容」要用）。
    assert_eq!(
        physical_segment_docs(&path),
        2,
        "旧修订与新修订各一行索引文档；旧的那行只是不参与默认命中"
    );
}

/// 极端情形：新版和旧版**完全不共享词**（换话题）。机制上它和上面的用例同源，
/// 但少了「共享词」，所以只能当补充，不能当唯一回归。
#[test]
fn switching_topic_entirely_also_drops_the_old_body() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    let (capture_id, source_id) =
        import_and_extract(&mut core, "随便写写.txt", "周末去爬山，山顶风很大。", "op");

    commit(&mut core, &capture_id, "op-commit");
    revise_text(
        &mut core,
        &capture_id,
        &source_id,
        "今天体检，血脂偏高。",
        "op-revise",
    );
    core.extract_source(&source_id).unwrap();

    assert_eq!(core.count_search_matches("爬山").unwrap(), 0, "旧话题整体退出");
    assert_eq!(core.count_search_matches("血脂").unwrap(), 1, "新话题在检索里");
}

// ------------------------------------------------------------ 纯文本修订的提取

/// `revise_text` 造出来的修订没有原件，正文就在 `source_revisions.text` 里。
/// 提取要覆盖成 `complete`，摘录正文与 `text` 一字不差，定位是**字符**下标。
#[test]
fn text_revision_extraction_is_complete_and_uses_character_offsets() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    // 提交自己写的文字：这条来源的修订没有原件（asset_id 为空），只有正文。
    let capture_id = core
        .create_draft(CreateDraftInput {
            occurred_at: None,
            time_zone: "Asia/Shanghai",
            utc_offset_minutes: 480,
            operation_id: "op-create",
        })
        .unwrap()
        .id;
    let body = "第一段带体检。\n\n第二段带血脂。";
    core.save_draft(&capture_id, body, 1, "op-save").unwrap();
    let committed = core.commit(&capture_id, 2, "op-commit").unwrap();
    let revision = committed.original_text_revision.expect("提交应当建文字版本");
    assert!(revision.asset_id.is_none(), "纯文字版本没有原件");

    let content = core.extract_source(&revision.source_id).unwrap();
    assert_eq!(content.status, ProcessingStatus::Ready);
    assert_eq!(content.coverage, Coverage::Complete, "正文就是内容，没有解析这一步");
    assert_eq!(content.text, body, "派生正文与修订的 text 一致");
    assert_eq!(content.extractor_id, diary_core::TEXT_DIRECT_EXTRACTOR_ID);
    assert_eq!(
        content.extractor_version,
        diary_core::TEXT_DIRECT_EXTRACTOR_VERSION
    );
    assert_eq!(content.source_revision_id, revision.revision_id);
    assert_eq!(content.segments.len(), 2, "按空行切段，复用文字那套切段逻辑");

    // 字符下标（不是字节）：第一段 7 个字符，「不太好」这种长短不影响这条断言，
    // 但如果实现用了字节偏移，7 会变成 21。
    let first = &content.segments[0];
    assert_eq!(first.text, "第一段带体检。");
    assert_eq!(first.locator.locator_type, LocatorType::TextRange);
    assert_eq!(first.locator.source_revision_id, revision.revision_id);
    assert_eq!(first.locator.text_start, Some(0));
    assert_eq!(first.locator.text_end, Some(7), "7 个字符，不是 21 个字节");
    let second = &content.segments[1];
    assert_eq!(second.text, "第二段带血脂。");
    assert_eq!(second.locator.text_start, Some(9), "跳过两个换行");
    assert_eq!(second.locator.text_end, Some(16));
}

/// 只改了空白：没有原件也没有正文，仍然是错——不能产出一份「空的派生内容」
/// 假装提取过了（那会让 coverage 说真话的能力失效）。
#[test]
fn blank_text_revision_is_still_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    let (capture_id, source_id) =
        import_and_extract(&mut core, "材料.txt", "原始正文。", "op");
    commit(&mut core, &capture_id, "op-commit");
    revise_text(&mut core, &capture_id, &source_id, "  \n　\n", "op-revise");

    let error = core.extract_source(&source_id).unwrap_err();
    assert_eq!(error.code(), ErrorCode::InvalidState);
    assert!(
        core.extracted_content(&source_id).unwrap().is_none(),
        "失败时不该留下半份派生内容"
    );
}

// ------------------------------------------------------------ 状态口径

/// 片段数只算当前修订：改之前 2 条、改之后 3 条，**不是** 2+3=5。
/// 同时钉住「旧修订的行还在索引表里」——状态要说清楚它们不计入。
#[test]
fn status_counts_only_the_current_revision() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.sqlite");
    let mut core = Core::open(&path).unwrap();
    let old_body = "第一段：体检结果不太好。\n\n第二段：医生让我复查。";
    let (capture_id, source_id) = import_and_extract(&mut core, "体检.txt", old_body, "op");

    let before = core.index_status(None).unwrap();
    assert_eq!(before.total_segments, 2);
    assert_eq!(before.indexed_segments, 2);
    assert_eq!(before.coverage, Coverage::Complete);

    commit(&mut core, &capture_id, "op-commit");
    revise_text(
        &mut core,
        &capture_id,
        &source_id,
        "第一段：体检结果还行。\n\n第二段：血脂偏高。\n\n第三段：下个月复查。",
        "op-revise",
    );
    core.extract_source(&source_id).unwrap();

    let after = core.index_status(None).unwrap();
    assert_eq!(after.total_segments, 3, "只算当前修订，不是 2+3");
    assert_eq!(after.indexed_segments, 3);
    assert_eq!(after.pending_segments, 0);
    assert_eq!(after.stale_segments, 0);
    assert_eq!(after.coverage, Coverage::Complete);
    assert_eq!(physical_segment_docs(&path), 5, "旧修订的索引行还在表里");
    assert!(
        after
            .reasons
            .iter()
            .any(|reason| reason.contains("属于非当前修订")),
        "状态必须说明旧修订不计入，实际是：{:?}",
        after.reasons
    );

    // 单来源范围与全库口径一致。
    let scoped = core
        .index_status(Some(std::slice::from_ref(&source_id)))
        .unwrap();
    assert_eq!(scoped.total_segments, 3);
    assert_eq!(scoped.indexed_segments, 3);
}

/// 整库重建不会把旧修订的片段重新捞回索引：搜索仍然搜不到旧措辞，
/// 索引表里的旧行也被清掉。
#[test]
fn rebuild_does_not_bring_the_old_revision_back() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.sqlite");
    let mut core = Core::open(&path).unwrap();
    let (capture_id, source_id) = import_and_extract(
        &mut core,
        "体检.txt",
        "第一段：体检结果不太好。\n\n第二段：医生让我复查。",
        "op",
    );
    commit(&mut core, &capture_id, "op-commit");
    revise_text(
        &mut core,
        &capture_id,
        &source_id,
        "第一段：体检结果还行。\n\n第二段：血脂偏高。\n\n第三段：下个月复查。",
        "op-revise",
    );
    core.extract_source(&source_id).unwrap();
    assert_eq!(physical_segment_docs(&path), 5);

    let rebuilt = core.rebuild_keyword_index(None).unwrap();
    assert_eq!(rebuilt, 3, "重建只索引当前修订的 3 个片段（没有记录文字）");
    assert_eq!(
        physical_segment_docs(&path),
        3,
        "重建要把旧修订的索引行清掉，不是留着让查询过滤兜底"
    );

    assert_eq!(core.count_search_matches("不太好").unwrap(), 0, "旧措辞不该被捞回来");
    assert!(search(&mut core, "不太好").is_empty());
    assert_eq!(core.count_search_matches("体检").unwrap(), 1);
    assert_eq!(core.count_search_matches("血脂").unwrap(), 1);

    let after = core.index_status(None).unwrap();
    assert_eq!(after.total_segments, 3);
    assert_eq!(after.indexed_segments, 3);
    assert_eq!(after.coverage, Coverage::Complete);
    assert!(
        !after
            .reasons
            .iter()
            .any(|reason| reason.contains("属于非当前修订")),
        "重建之后不该再有「非当前修订」的说明：{:?}",
        after.reasons
    );
}

// ------------------------------------------------------------ 上一版正文

#[test]
fn previous_source_revision_returns_the_previous_body() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    let capture_id = core
        .create_draft(CreateDraftInput {
            occurred_at: None,
            time_zone: "Asia/Shanghai",
            utc_offset_minutes: 480,
            operation_id: "op-create",
        })
        .unwrap()
        .id;
    core.save_draft(&capture_id, OLD_BODY, 1, "op-save").unwrap();
    let committed = core.commit(&capture_id, 2, "op-commit").unwrap();
    let source_id = committed.original_text_revision.unwrap().source_id;

    assert!(
        core.previous_source_revision(&source_id).unwrap().is_none(),
        "只有一版时没有上一版"
    );

    let revised = revise_text(&mut core, &capture_id, &source_id, NEW_BODY, "op-revise");
    let previous = core
        .previous_source_revision(&source_id)
        .unwrap()
        .expect("改过之后应当有上一版");
    assert_eq!(previous.text.as_deref(), Some(OLD_BODY), "拿到的是上一版正文");
    assert_eq!(previous.source_id, source_id);
    assert!(
        previous.parent_revision_id.is_none(),
        "上一版自己就是第一版"
    );
    assert_eq!(previous.asset_id, None);
    assert_eq!(previous.revision_id, revised.parent_revision_id.unwrap());

    // 来源不存在：同样是「没有上一版」，不是错误。
    assert!(core
        .previous_source_revision("src_不存在")
        .unwrap()
        .is_none());
}

/// 父修订悬空（数据坏了）时也按「没有上一版」处理：给不出正文就如实说没有，
/// 不编一个空修订出来让调用方以为「上一版是空的」。
#[test]
fn previous_source_revision_treats_a_dangling_parent_as_none() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.sqlite");
    let mut core = Core::open(&path).unwrap();
    let capture_id = core
        .create_draft(CreateDraftInput {
            occurred_at: None,
            time_zone: "Asia/Shanghai",
            utc_offset_minutes: 480,
            operation_id: "op-create",
        })
        .unwrap()
        .id;
    core.save_draft(&capture_id, OLD_BODY, 1, "op-save").unwrap();
    let revision_id = core
        .commit(&capture_id, 2, "op-commit")
        .unwrap()
        .original_text_revision
        .unwrap()
        .revision_id;

    // `parent_revision_id` 上没有外键，所以「指向一个不存在的修订」是库里可能出现
    // 的坏数据；这里手工造一次。
    rusqlite::Connection::open(&path)
        .unwrap()
        .execute(
            "UPDATE source_revisions SET parent_revision_id = 'rev_不存在' \
             WHERE revision_id = ?1",
            rusqlite::params![revision_id],
        )
        .unwrap();

    assert!(core
        .previous_source_revision(
            core.get_capture(&capture_id).unwrap().ordered_source_ids[0].as_str()
        )
        .unwrap()
        .is_none());
}

/// 父修订可能是**原件型**修订（导入的文件、录音）：`text` 是空的、`asset_id` 有值。
/// 这不是 bug，是「上一版修订」的如实形态；前端要按这个分支决定是显示文字还是
/// 提供「打开原件」。契约侧怎么把它包给前端留给 #55（见架构文档）。
#[test]
fn previous_source_revision_can_be_an_original_asset_revision() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    let (capture_id, source_id) = import_and_extract(&mut core, "体检.txt", OLD_BODY, "op");
    commit(&mut core, &capture_id, "op-commit");
    revise_text(&mut core, &capture_id, &source_id, NEW_BODY, "op-revise");

    let previous = core
        .previous_source_revision(&source_id)
        .unwrap()
        .expect("导入那一版就是上一版");
    assert!(previous.asset_id.is_some(), "上一版是原件型修订");
    assert!(
        previous.text.is_none(),
        "原件型修订自己没有正文文字：正文要经提取才拿得到"
    );
}

// ------------------------------------------------------------ 已知的重叠（要看）

/// `revise_text` 只改**来源修订**，**不动** `captures.draft_text`——这是本片的决策：
/// 「记录自己的文字」由 B3d 那一套负责，改它应当走 `save_draft`（那里会先删后写，
/// 旧词项真的消失）。
///
/// 由此带来一处**已知重叠**，这条测试把它钉在明面上，而不是让它悄悄存在：
/// 当一条记录的 `draft_text` 和它提交出来的文字修订曾经是同一段文字时，
/// `revise_text` 改的是修订，`draft_text` 里那份没变，于是**旧措辞仍然会通过记录
/// 文字那一路命中**（`search_capture_docs`），尽管来源修订那一路已经搜不到了。
///
/// 要不要让 `revise_text` 顺手同步 `draft_text`（或让前端改正文走 `save_draft`），
/// 是跨层的产品决定，见 `docs/architecture/m2-修订与检索可见性.md`
/// 「没想清楚的地方」。
#[test]
fn revising_the_body_syncs_the_record_text_when_it_was_the_same() {
    // 「写草稿 → 提交 → 改正文」这条流程里，`captures.draft_text` 与那一篇的上一版是
    // **同一段文字的两种存法**（提交时复制过去的）。这时如果只改修订、不动 draft_text，
    // 记录文字那一路（B3d 已索引）就仍然能搜到**旧正文**——而决策是「用户在搜索里直接
    // 看不到旧正文」。所以两者确实相同时要在同一个事务里一起改。
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    let capture_id = core
        .create_draft(CreateDraftInput {
            occurred_at: None,
            time_zone: "Asia/Shanghai",
            utc_offset_minutes: 480,
            operation_id: "op-create",
        })
        .unwrap()
        .id;
    core.save_draft(&capture_id, OLD_BODY, 1, "op-save").unwrap();
    let committed = core.commit(&capture_id, 2, "op-commit").unwrap();
    let source_id = committed.original_text_revision.unwrap().source_id;
    core.extract_source(&source_id).unwrap();

    revise_text(&mut core, &capture_id, &source_id, NEW_BODY, "op-revise");

    assert_eq!(
        core.get_capture(&capture_id).unwrap().draft_text,
        NEW_BODY,
        "同一段文字两种存法时要一起改，否则旧正文会从记录文字那一路漏出去"
    );
    assert_eq!(
        core.count_search_matches("不太好").unwrap(),
        0,
        "旧措辞整库都搜不到（来源修订与记录文字两条路都不能带出来）"
    );
    // 共享词仍然搜得到。注意**不是**各出一条：同一条记录的两条索引路（来源片段、
    // 记录文字）在排名里被折成一条——这正是「一篇一条结果」的口径，别指望拿到两条。
    assert_eq!(
        core.count_search_matches("体检").unwrap(),
        1,
        "共享词改完仍然搜得到（折成一条）"
    );
    for hit in search(&mut core, "体检") {
        let snippet = hit.snippet.unwrap_or_default();
        assert!(
            !snippet.contains("不太好"),
            "任何一条命中都不该显示旧正文：{snippet}"
        );
    }
}

#[test]
fn revising_a_source_does_not_touch_a_different_record_text() {
    // 反向守卫：记录文字与这一篇的上一版**不是**同一段时，不许替用户改 draft_text
    //（导入材料就是这种情况——记录文字另有出处）。
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    let (capture_id, source_id) = import_and_extract(&mut core, "体检报告.txt", OLD_BODY, "op");
    commit(&mut core, &capture_id, "op-commit");

    assert_eq!(
        core.get_capture(&capture_id).unwrap().draft_text,
        "",
        "导入的材料不进记录文字"
    );
    revise_text(&mut core, &capture_id, &source_id, NEW_BODY, "op-revise");

    assert_eq!(
        core.get_capture(&capture_id).unwrap().draft_text,
        "",
        "两边不是同一段文字时不许动 draft_text"
    );
    assert_eq!(
        core.count_search_matches("不太好").unwrap(),
        0,
        "旧措辞仍然要退出检索（来源修订那一路）"
    );
    assert_eq!(core.count_search_matches("血脂").unwrap(), 1, "新措辞进得来");
}

#[test]
fn a_retry_of_revise_text_heals_a_missing_derivation() {
    // `revise_text` 先存修订、再提取。提取那一步万一失败，调用方拿同一条 operationId
    // 重试会命中回执直接返回——那时必须补提取，否则「改完搜不到」会一直留着。
    let dir = tempfile::tempdir().unwrap();
    // 这条测试要直接动库（模拟「提取没写成」），所以用文件库而不是 `new_core` 的内存库。
    let path = dir.path().join("library.sqlite");
    let mut core = Core::open(&path).unwrap();
    let (capture_id, source_id) = import_and_extract(&mut core, "体检报告.txt", OLD_BODY, "op");
    commit(&mut core, &capture_id, "op-commit");
    // 直接调而不是用 helper：重试必须是**同一条指纹**（同样的 expectedRevision），
    // 也就是调用方拿着同一次请求重试的真实样子。
    let expected = core.get_capture(&capture_id).unwrap().revision;
    let revised = core
        .revise_text(&source_id, NEW_BODY, expected, "op-revise")
        .unwrap();
    assert_eq!(core.count_search_matches("血脂").unwrap(), 1, "改完立刻能搜到");

    // 模拟「提取那一步没写成」：删掉派生内容，索引行也一并清掉。
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(
            "DELETE FROM extracted_segments; DELETE FROM extracted_contents; \
             DELETE FROM search_docs; DELETE FROM search_grams;",
        )
        .unwrap();
    }
    assert_eq!(core.count_search_matches("血脂").unwrap(), 0, "前提：现在搜不到");

    // 同一条 operationId + 同一条指纹（同样的 expectedRevision）重试：走回执路径，
    // 并且应当把缺失的派生内容补上。
    let again = core
        .revise_text(&source_id, NEW_BODY, expected, "op-revise")
        .unwrap();
    assert_eq!(again.revision_id, revised.revision_id, "重试拿到同一条修订");
    assert_eq!(
        core.count_search_matches("血脂").unwrap(),
        1,
        "重试要把缺失的派生内容补上（自愈）"
    );
}

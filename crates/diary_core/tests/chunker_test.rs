//! B3c-1 的分块测试：边界、重叠、确定性、以及「冻结 chunkerVersion」。

use diary_core::{
    chunks_for, CHUNKER_VERSION, CHUNK_OVERLAP_CHARS, MAX_CHUNK_CHARS,
};

fn chars(count: usize, filler: char) -> String {
    std::iter::repeat_n(filler, count).collect()
}

#[test]
fn empty_or_whitespace_yields_nothing() {
    assert!(chunks_for("").is_empty());
    assert!(chunks_for("   \n\n\t  ").is_empty(), "全空白没有可索引的内容");
}

#[test]
fn short_text_is_one_chunk() {
    let chunks = chunks_for("妈妈打电话来说她最近身体不太好。");
    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0].ordinal, 0);
    assert_eq!(chunks[0].text, "妈妈打电话来说她最近身体不太好。");
    assert_eq!(chunks[0].start, 0);
    assert_eq!(chunks[0].end, 16, "字符下标，不是字节");
}

#[test]
fn exactly_the_limit_is_one_chunk() {
    let text = chars(MAX_CHUNK_CHARS, '妈');
    let chunks = chunks_for(&text);
    assert_eq!(chunks.len(), 1, "恰好等于上限不该被切开");
    assert_eq!(chunks[0].end, MAX_CHUNK_CHARS as i64);
}

#[test]
fn over_the_limit_splits_with_overlap() {
    let text = chars(MAX_CHUNK_CHARS + 1, '妈');
    let chunks = chunks_for(&text);
    assert_eq!(chunks.len(), 2);

    assert_eq!(chunks[0].start, 0);
    assert_eq!(chunks[0].end, MAX_CHUNK_CHARS as i64);
    // 重叠：第二块从「第一块末尾往前 overlap」开始。
    assert_eq!(chunks[1].start, (MAX_CHUNK_CHARS - CHUNK_OVERLAP_CHARS) as i64);
    assert_eq!(chunks[1].end, (MAX_CHUNK_CHARS + 1) as i64);
    assert_eq!(
        chunks[0].end - chunks[1].start,
        CHUNK_OVERLAP_CHARS as i64,
        "相邻块的重叠必须正好是约定的字符数"
    );
}

#[test]
fn no_chunk_exceeds_the_limit() {
    // 混合内容：中文、英文、标点、换行——最坏情况的中文占比也要守住上限。
    let mut text = String::new();
    for index in 0..40 {
        text.push_str(&format!(
            "第{index}段：妈妈打电话来说体检结果不太好，我有点担心。\n\n"
        ));
        text.push_str("meanwhile the project shipped on time and nobody complained. ");
        text.push_str("她说下周再来一趟，顺便把上次借的书还了。\n");
    }
    let chunks = chunks_for(&text);
    assert!(chunks.len() > 1, "这么长的文本应当被切成多块");
    for chunk in &chunks {
        assert!(
            chunk.text.chars().count() <= MAX_CHUNK_CHARS,
            "块 {} 有 {} 字，超过上限",
            chunk.ordinal,
            chunk.text.chars().count()
        );
    }
}

#[test]
fn prefers_paragraph_boundary() {
    // 在 350 字处放一个空行：应当在那里断开，而不是硬切到 400。
    let first = format!("{}。\n\n", chars(349, '妈'));
    let text = format!("{first}{}", chars(300, '爸'));
    let chunks = chunks_for(&text);
    assert!(chunks.len() >= 2);
    let first_end = chunks[0].end;
    assert!(
        first_end <= 350,
        "应当在段落边界（350 附近）断开，实际断在 {first_end}"
    );
    assert!(
        chunks[0].text.starts_with('妈') && !chunks[0].text.ends_with('\n'),
        "块正文两端不该留空白：{:?}",
        &chunks[0].text[chunks[0].text.len().saturating_sub(6)..]
    );
}

#[test]
fn prefers_sentence_boundary_when_no_paragraph() {
    // 没有空行，但 360 字处有句号：应当在句号后断开。
    let text = format!("{}。{}", chars(359, '妈'), chars(300, '爸'));
    let chunks = chunks_for(&text);
    assert!(chunks.len() >= 2);
    assert!(
        chunks[0].end <= 361,
        "应当在句子边界断开，实际断在 {}",
        chunks[0].end
    );
    assert!(chunks[0].text.ends_with('。'));
}

#[test]
fn every_position_is_covered_and_ends_match() {
    let mut text = String::new();
    for index in 0..30 {
        text.push_str(&format!("第{index}句，内容用于检查覆盖。"));
        if index % 5 == 0 {
            text.push_str("\n\n");
        }
    }
    let chunks = chunks_for(&text);
    assert!(!chunks.is_empty());

    // 首块从头开始、末块到尾结束（去掉两端空白之后）。
    let first = text.chars().position(|c| !c.is_whitespace()).unwrap();
    let last = text.chars().count()
        - text
            .chars()
            .rev()
            .position(|c| !c.is_whitespace())
            .unwrap();
    assert_eq!(chunks[0].start, first as i64);
    assert_eq!(chunks.last().unwrap().end, last as i64);

    // 每一块的内容都能在原文的 start..end 上原样对上（定位不错位）。
    let characters: Vec<char> = text.chars().collect();
    for chunk in &chunks {
        let slice: String = characters[chunk.start as usize..chunk.end as usize]
            .iter()
            .collect();
        assert_eq!(slice, chunk.text, "块 {} 的 start/end 与正文对不上", chunk.ordinal);
    }

    // 相邻块首尾相接（有重叠，不留空洞）。
    for pair in chunks.windows(2) {
        assert!(
            pair[1].start <= pair[0].end,
            "块 {} 与 {} 之间出现空洞",
            pair[0].ordinal,
            pair[1].ordinal
        );
    }
}

#[test]
fn is_deterministic() {
    let mut text = String::new();
    for index in 0..25 {
        text.push_str(&format!("第{index}段内容。\n"));
    }
    assert_eq!(chunks_for(&text), chunks_for(&text), "同输入必须同输出");
}

#[test]
fn char_indices_survive_emoji() {
    // emoji 是多个字节、一个字符：下标必须是字符数，否则定位会偏。
    let text = format!("{}😀{}", chars(300, '妈'), chars(300, '爸'));
    let chunks = chunks_for(&text);
    let characters: Vec<char> = text.chars().collect();
    for chunk in &chunks {
        let slice: String = characters[chunk.start as usize..chunk.end as usize]
            .iter()
            .collect();
        assert_eq!(slice, chunk.text);
    }
}

#[test]
fn chunker_version_is_frozen() {
    // 这个断言故意写死字符串：改分块逻辑或常量时，必须**同时**改版本号，
    // 让索引状态里的 chunkerVersion 能识别出「库里的是旧块」。
    assert_eq!(CHUNKER_VERSION, "chunker-1-char400-overlap48");
}
//! B3c-1 的分块测试：三条规则（篇内断、跨篇不断、段尾断句尾不断）+ 边界与确定性。

use diary_core::{
    chunks_for, pack_pieces, Chunk, Piece, CHUNKER_VERSION, CHUNK_OVERLAP_CHARS, MAX_CHUNK_CHARS,
};

fn chars(count: usize, filler: char) -> String {
    std::iter::repeat_n(filler, count).collect()
}

/// 单篇分块时，第一段在篇内的区间。
fn span_end(chunk: &Chunk) -> i64 {
    chunk.spans.last().expect("块必须有 span").end
}
fn span_start(chunk: &Chunk) -> i64 {
    chunk.spans.first().expect("块必须有 span").start
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
    assert_eq!(span_start(&chunks[0]), 0);
    assert_eq!(span_end(&chunks[0]), 16, "字符下标，不是字节");
}

#[test]
fn exactly_the_limit_is_one_chunk() {
    let text = chars(MAX_CHUNK_CHARS, '妈');
    let chunks = chunks_for(&text);
    assert_eq!(chunks.len(), 1, "恰好等于上限不该被切开");
    assert_eq!(span_end(&chunks[0]), MAX_CHUNK_CHARS as i64);
}

#[test]
fn over_the_limit_splits_with_overlap() {
    let text = chars(MAX_CHUNK_CHARS + 1, '妈');
    let chunks = chunks_for(&text);
    assert_eq!(chunks.len(), 2);

    assert_eq!(span_start(&chunks[0]), 0);
    assert_eq!(span_end(&chunks[0]), MAX_CHUNK_CHARS as i64);
    // 重叠：第二段从「第一段末尾往前 overlap」开始。
    assert_eq!(
        span_start(&chunks[1]),
        (MAX_CHUNK_CHARS - CHUNK_OVERLAP_CHARS) as i64
    );
    assert_eq!(span_end(&chunks[1]), (MAX_CHUNK_CHARS + 1) as i64);
    assert_eq!(
        span_end(&chunks[0]) - span_start(&chunks[1]),
        CHUNK_OVERLAP_CHARS as i64,
        "相邻段的重叠必须正好是约定的字符数"
    );
}

#[test]
fn does_not_break_at_sentence_ends() {
    // 规则：**段尾断，句尾不断**。这段没有换行，只有句末标点：
    // 不许在句号处断开，只能到上限硬切。
    let text = format!("{}。{}", chars(359, '妈'), chars(300, '爸'));
    let chunks = chunks_for(&text);
    assert!(chunks.len() >= 2);
    assert_eq!(
        span_end(&chunks[0]),
        MAX_CHUNK_CHARS as i64,
        "句末标点不是断点：应当在 {MAX_CHUNK_CHARS} 处硬切，实际断在 {}",
        span_end(&chunks[0])
    );
    assert!(
        !chunks[0].text.ends_with('。'),
        "这一块不该正好收在句号上（说明它挑了句子边界）"
    );
}

#[test]
fn breaks_at_a_single_line_end() {
    // 段尾就是换行：哪怕只有一个换行、位置很早（100 字处），也应当在那里收住。
    let text = format!("{}\n{}", chars(100, '妈'), chars(500, '爸'));
    let chunks = chunks_for(&text);
    assert!(chunks.len() >= 2);
    assert_eq!(span_end(&chunks[0]), 100, "应当在换行处收住");
    assert_eq!(chunks[0].text, chars(100, '妈'));
}

#[test]
fn takes_the_last_line_end_not_the_first() {
    // 一行一句的文本（每行 20 字）：块应当尽量装满到 400 附近，
    // 而不是「一行一块」——所以取窗口里**最后一个**换行。
    let text: String = (0..40).map(|_| format!("{}\n", chars(20, '妈'))).collect();
    let chunks = chunks_for(&text);
    assert!(chunks.len() >= 2);
    assert!(
        chunks[0].text.chars().count() > 300,
        "第一块只装了 {} 字，说明取的是第一个换行而不是最后一个",
        chunks[0].text.chars().count()
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
fn long_single_paragraph_is_hard_cut_at_the_limit() {
    // 一段超过上限、里面没有任何换行：只能按上限硬切（没有别的断点可选）。
    let text = chars(1000, '妈');
    let chunks = chunks_for(&text);
    assert_eq!(chunks.len(), 3);
    for chunk in &chunks {
        assert!(chunk.text.chars().count() <= MAX_CHUNK_CHARS);
    }
    assert_eq!(span_end(&chunks[0]), 400);
    assert_eq!(span_start(&chunks[1]), 400 - 48);
    assert_eq!(span_end(&chunks[2]), 1000);
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

    let first = text.chars().position(|c| !c.is_whitespace()).unwrap();
    let last = text.chars().count()
        - text
            .chars()
            .rev()
            .position(|c| !c.is_whitespace())
            .unwrap();
    assert_eq!(span_start(&chunks[0]), first as i64);
    assert_eq!(span_end(chunks.last().unwrap()), last as i64);

    // 每一段的内容都能在原文的 start..end 上原样对上（定位不错位）。
    let characters: Vec<char> = text.chars().collect();
    for chunk in &chunks {
        for span in &chunk.spans {
            let slice: String = characters[span.start as usize..span.end as usize]
                .iter()
                .collect();
            assert!(
                chunk.text.contains(&slice),
                "块 {} 的 span 与块正文对不上",
                chunk.ordinal
            );
        }
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
        for span in &chunk.spans {
            let slice: String = characters[span.start as usize..span.end as usize]
                .iter()
                .collect();
            assert!(chunk.text.contains(&slice));
        }
    }
}

// ---------------------------------------------------------------- 打包（跨篇）

#[test]
fn packs_short_pieces_on_the_same_day() {
    // 「短时间跨度内的多个短篇可以放到一块里」：同一天的三篇短文合成一块。
    let (a, b, c) = (chars(50, '甲'), chars(50, '乙'), chars(50, '丙'));
    let pieces = vec![
        Piece { id: "a", day_key: "2026-09-20", text: &a },
        Piece { id: "b", day_key: "2026-09-20", text: &b },
        Piece { id: "c", day_key: "2026-09-20", text: &c },
    ];
    let chunks = pack_pieces(&pieces);
    assert_eq!(chunks.len(), 1, "三篇短文应当合成一块");
    assert_eq!(chunks[0].spans.len(), 3, "三篇各占一个 span");
    assert_eq!(
        chunks[0]
            .spans
            .iter()
            .map(|span| span.piece_id.as_str())
            .collect::<Vec<_>>(),
        vec!["a", "b", "c"],
        "span 顺序与传入顺序一致（调用方按时间排）"
    );
    assert_eq!(chunks[0].text.chars().count(), 50 * 3 + 2 * 2, "含两处分隔");
}

#[test]
fn does_not_pack_across_days() {
    let (a, b) = (chars(50, '甲'), chars(50, '乙'));
    let pieces = vec![
        Piece { id: "a", day_key: "2026-09-20", text: &a },
        Piece { id: "b", day_key: "2026-09-21", text: &b },
    ];
    let chunks = pack_pieces(&pieces);
    assert_eq!(chunks.len(), 2, "跨天不合并");
    assert_eq!(chunks[0].spans[0].piece_id, "a");
    assert_eq!(chunks[1].spans[0].piece_id, "b");
}

#[test]
fn keeps_a_piece_whole_instead_of_splitting_it_to_fit() {
    // 跨篇不断：380 字的甲 + 50 字的乙，乙放不进第一块（380+2+50 > 400）——
    // 这时让乙另起一块，**不**把甲切开去凑。
    let first = chars(380, '甲');
    let second = chars(50, '乙');
    let pieces = vec![
        Piece { id: "a", day_key: "2026-09-20", text: &first },
        Piece { id: "b", day_key: "2026-09-20", text: &second },
    ];
    let chunks = pack_pieces(&pieces);
    assert_eq!(chunks.len(), 2);
    assert_eq!(chunks[0].text, first, "甲应当整篇在第一块里");
    assert_eq!(chunks[1].text, second, "乙整篇在第二块里，没有被切");
}

#[test]
fn a_long_piece_is_split_within_itself_only() {
    // 一篇 1000 字（自己超限）+ 同一篇后面的 50 字短篇：
    // 长的那篇在**篇内**切成多块，短篇自己一块，互不混装。
    let long = chars(1000, '甲');
    let short = chars(50, '乙');
    let pieces = vec![
        Piece { id: "long", day_key: "2026-09-20", text: &long },
        Piece { id: "short", day_key: "2026-09-20", text: &short },
    ];
    let chunks = pack_pieces(&pieces);
    assert_eq!(chunks.len(), 4, "1000 字切成 3 块 + 短篇 1 块");
    for chunk in &chunks[..3] {
        assert_eq!(chunk.spans.len(), 1);
        assert_eq!(chunk.spans[0].piece_id, "long");
    }
    assert_eq!(chunks[3].spans.len(), 1);
    assert_eq!(chunks[3].spans[0].piece_id, "short");
    assert_eq!(chunks[3].text, short);
}

#[test]
fn skips_blank_pieces() {
    let pieces = vec![
        Piece { id: "blank", day_key: "2026-09-20", text: "   \n\n  " },
        Piece { id: "real", day_key: "2026-09-20", text: "妈妈打电话来。" },
    ];
    let chunks = pack_pieces(&pieces);
    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0].spans.len(), 1, "空白的那篇不该产生 span");
    assert_eq!(chunks[0].spans[0].piece_id, "real");
    assert_eq!(chunks[0].text, "妈妈打电话来。");
}

#[test]
fn chunker_version_is_frozen() {
    // 这个断言故意写死字符串：改分块逻辑或常量时，必须**同时**改版本号，
    // 让索引状态里的 chunkerVersion 能识别出「库里的是旧块」。
    assert_eq!(CHUNKER_VERSION, "chunker-1-packday-line400-overlap48");
}
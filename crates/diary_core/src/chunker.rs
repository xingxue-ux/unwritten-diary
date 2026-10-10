//! 分块：任务书 5.4 节（每块约 400 token、重叠约 48 token），冻结 `CHUNKER_VERSION`。
//!
//! 为什么是「按字符」而不是「按 token」：核心不该为了切块把分词器拉进来（体积与
//! 构建代价都不小），而且切块规则一旦依赖具体模型的词表，模型一换块边界就变，
//! 索引也就跟着失效。这里用字符数作为**保守上界**：
//!
//! - BERT WordPiece 对中文大致是 1 字 ≈ 1 token，所以 400 字 ≤ 约 400 token；
//! - 拉丁文本约 4 字/token，400 字远小于 400 token；
//! - 混合文本的最坏情况是中文字符占多数，仍然落在「≤ 约 400 token」里。
//!
//! 这个假设**不能只写在注释里**：B3c-1 的验收要求用真实分词器核对分块结果的
//! token 数（max / p99），核对结果记在 `docs/architecture/m2-质量集与分块.md`。
//! 如果哪天换成对中文更「贵」的分词器（比如 1 字 > 1 token），改的是
//! `MAX_CHUNK_CHARS` 与 `CHUNKER_VERSION`，不是偷偷放宽验收。

/// 分块规则的版本。**改了切块逻辑或下面两个常量就必须改它**：索引行与索引状态
/// 都记这个值，换代时据此重算（任务书 5.4 节要求冻结 chunkerVersion）。
pub const CHUNKER_VERSION: &str = "chunker-1-char400-overlap48";

/// 每块最多多少字符（保守上界，见模块说明）。
pub const MAX_CHUNK_CHARS: usize = 400;

/// 相邻块重叠多少字符：切断处两侧的上下文都留在各自块里，避免「正好切在关键词
/// 中间」导致两路都召不回。
pub const CHUNK_OVERLAP_CHARS: usize = 48;

/// 只有在块长至少到这个比例时才考虑「在段落/句子边界断开」。太早断开会产生一堆
/// 碎块，反而让「相邻块占满结果」更严重。
const BOUNDARY_MIN_RATIO: f64 = 0.5;

/// 一块正文。`start` / `end` 是**字符**下标（不是字节），相对整段正文。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextChunk {
    pub ordinal: i64,
    pub text: String,
    pub start: i64,
    pub end: i64,
}

/// 把一段正文切成块。空文本或全空白返回空列表（没有可索引的内容）。
///
/// 确定性：同一输入永远得到同一结果——轨道上「同一份内容反复索引」必须产出同样的
/// 块，否则索引会无谓地换代。
pub fn chunks_for(text: &str) -> Vec<TextChunk> {
    let characters: Vec<char> = text.chars().collect();
    if characters.iter().all(|character| character.is_whitespace()) {
        return Vec::new();
    }

    let total = characters.len();
    let mut chunks = Vec::new();
    let mut ordinal = 0_i64;
    let mut position = 0_usize;

    while position < total {
        let hard_end = (position + MAX_CHUNK_CHARS).min(total);
        let end = if hard_end == total {
            total
        } else {
            boundary_before(&characters, position, hard_end).unwrap_or(hard_end)
        };

        // 两端去掉空白再定位：块正文不该带着上一块的换行或这一块的缩进，
        // 但 start/end 要指向**保留下来的那段**，这样定位到原文才对得上。
        let mut trimmed_start = position;
        let mut trimmed_end = end;
        while trimmed_start < trimmed_end && characters[trimmed_start].is_whitespace() {
            trimmed_start += 1;
        }
        while trimmed_end > trimmed_start && characters[trimmed_end - 1].is_whitespace() {
            trimmed_end -= 1;
        }
        if trimmed_start < trimmed_end {
            chunks.push(TextChunk {
                ordinal,
                text: characters[trimmed_start..trimmed_end].iter().collect(),
                start: trimmed_start as i64,
                end: trimmed_end as i64,
            });
            ordinal += 1;
        }

        if end >= total {
            break;
        }
        // 重叠：下一块从「本块末尾往前 overlap」处开始；但不能不前进。
        let next = end.saturating_sub(CHUNK_OVERLAP_CHARS);
        position = if next > position { next } else { end };
    }

    chunks
}

/// 在 `[start, end)` 里找一个比硬切更好的断点（段落优先，其次句子）。
///
/// 返回的是**排他**下标：`characters[..返回值的]` 就是这一块。
fn boundary_before(characters: &[char], start: usize, end: usize) -> Option<usize> {
    let minimum = start + ((end - start) as f64 * BOUNDARY_MIN_RATIO) as usize;
    if minimum >= end {
        return None;
    }

    // 段落优先：空行之后是更新的话题。
    let paragraph = (minimum.max(2)..end)
        .rev()
        .find(|index| characters[index - 1] == '\n' && characters[index - 2] == '\n');
    if paragraph.is_some() {
        return paragraph;
    }
    // 其次句子：中文句末标点，或单独一个换行。
    const SENTENCE_ENDERS: [char; 5] = ['。', '！', '？', '；', '\n'];
    (minimum..end)
        .rev()
        .find(|index| SENTENCE_ENDERS.contains(&characters[index - 1]))
}

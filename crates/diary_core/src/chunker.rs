//! 分块：任务书 5.4 节（每块约 400 token、重叠约 48 token），冻结 `CHUNKER_VERSION`。
//!
//! 三条规则（别处不要另立规则）：
//!
//! 1. **篇内断**。一篇（一个来源的正文）太长时，在**篇内**按段尾切开。
//! 2. **跨篇不断**。合块的单位是**整篇**：短篇可以拼在一起，但**绝不为了填满一块
//!    把一篇切开去凑**——放不下就让这一篇另起一块。只有一篇自己超过上限时，才在
//!    篇内切（切出来的部分也不与别的篇混装）。
//! 3. **段尾断，句尾不断**。断点候选只认**段尾**（换行），句末标点（`。！？；`）
//!    **不是**断点：一段话说到底就让它说完。只有一段本身超过上限、且段内没有任何
//!    换行时，才不得不按上限硬切。
//!
//! 「短时间跨度内多个短篇放一块」在数据里就是用 **`day_key` 相同** 表达的（与
//! `captures.day_key` 一致）：同一天的短篇可以packing 进同一块，跨天不合并。
//!
//! 为什么按字符而不是按 token：核心不该为了切块把分词器拉进来（体积与构建代价
//! 都不小），而且切块规则一旦依赖具体模型的词表，模型一换块边界就变，索引也就
//! 跟着失效。这里用字符数作为**保守上界**：
//!
//! - BERT WordPiece 对中文大致是 1 字 ≈ 1 token，所以 400 字 ≤ 约 400 token；
//! - 拉丁文本约 4 字/token，400 字远小于 400 token；
//! - 混合文本的最坏情况是中文字符占多数，仍然落在「≤ 约 400 token」里。
//!
//! 这个假设**不能只写在注释里**：用真实分词器核对分块结果的 token 数（max / p99），
//! 核对结果记在 `docs/architecture/m2-质量集与分块.md`。
//!
//! 合块的**代价**要说清：一块里有多篇时，命中的每篇各占一个 span（`spans`），
//! 向量是对整块算的——所以这一块的相似度属于「这几篇合起来」而不是某一篇。检索
//! 结果要回溯到具体篇时按 `spans` 走（`SearchHit` 的 `sourceId`/`locator` 到
//! B3c-2 再定：现在只有关键词那一路，不涉及这个映射）。

/// 分块规则的版本。**改了切块逻辑或下面两个常量就必须改它**：索引行与索引状态
/// 都记这个值，换代时据此重算（任务书 5.4 节要求冻结 chunkerVersion）。
pub const CHUNKER_VERSION: &str = "chunker-1-packday-line400-overlap48";

/// 每块最多多少字符（保守上界，见模块说明）。
pub const MAX_CHUNK_CHARS: usize = 400;

/// 相邻块重叠多少字符：只用于**一篇自己超限被切开**的地方（篇与篇之间是整篇拼接，
/// 不存在「切在篇中间」的问题，也就不需要重叠）。
pub const CHUNK_OVERLAP_CHARS: usize = 48;

/// 合块时两篇之间的分隔，也计入上限。
const PIECE_SEPARATOR: &str = "\n\n";

/// 一篇正文 + 它的时间与标识。
#[derive(Debug, Clone)]
pub struct Piece<'a> {
    /// 这一篇的标识（来源 id / 记录 id），用来回溯命中。不需要溯源时留空串。
    pub id: &'a str,
    /// 自然日（`YYYY-MM-DD`）：**只有同一天的短篇才会合进同一块**。
    pub day_key: &'a str,
    pub text: &'a str,
}

/// 一块里的一段：来自哪一篇、在篇内的字符区间（排他）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChunkSpan {
    pub piece_id: String,
    pub start: i64,
    pub end: i64,
}

/// 一块正文。`spans` 说明它由哪几篇的哪几段组成（合块时多于一段）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunk {
    pub ordinal: i64,
    pub text: String,
    pub spans: Vec<ChunkSpan>,
}

/// 单篇分块：等价于「只有一篇、没有时间信息」的打包。
///
/// 空文本或全空白返回空列表（没有可索引的内容）。确定性：同一输入永远得到同一
/// 结果——「同一份内容反复索引」必须产出同样的块，否则索引会无谓地换代。
pub fn chunks_for(text: &str) -> Vec<Chunk> {
    pack_pieces(&[Piece {
        id: "",
        day_key: "",
        text,
    }])
}

/// 把若干篇按顺序（调用方按时间排好）打包成块。
///
/// 规则见模块说明：短篇同一天可合块、整篇不被切开去凑、超长篇在篇内按段尾切。
pub fn pack_pieces(pieces: &[Piece<'_>]) -> Vec<Chunk> {
    let mut builder = Builder::default();
    for piece in pieces {
        builder.push(piece);
    }
    builder.finish()
}

#[derive(Default)]
struct Builder {
    chunks: Vec<Chunk>,
    pending_spans: Vec<ChunkSpan>,
    pending_text: String,
    pending_day: Option<String>,
}

impl Builder {
    fn push(&mut self, piece: &Piece<'_>) {
        let Some((start, end)) = trim_range(piece.text) else {
            return; // 全空白的一篇：没有可索引的内容
        };
        let length = end - start;

        if length > MAX_CHUNK_CHARS {
            // 这一篇自己超过上限：先在篇内按段尾切，切出来的每一段各自成块
            // （不与别的篇混装——跨篇不断）。
            self.flush();
            for span in split_long_piece(piece, start, end) {
                self.push_chunk(Chunk {
                    ordinal: 0,
                    text: span.0,
                    spans: vec![span.1],
                });
            }
            return;
        }

        let fits = self.pending_text.is_empty()
            || self.pending_text.chars().count()
                + PIECE_SEPARATOR.chars().count()
                + length
                <= MAX_CHUNK_CHARS;
        let same_day = self
            .pending_day
            .as_deref()
            .is_none_or(|day| day == piece.day_key);
        if !(self.pending_spans.is_empty() || (fits && same_day)) {
            self.flush();
        }

        if !self.pending_text.is_empty() {
            self.pending_text.push_str(PIECE_SEPARATOR);
        }
        self.pending_text.extend(piece.text.chars().skip(start).take(length));
        self.pending_spans.push(ChunkSpan {
            piece_id: piece.id.to_owned(),
            start: start as i64,
            end: end as i64,
        });
        self.pending_day = Some(piece.day_key.to_owned());
    }

    fn push_chunk(&mut self, chunk: Chunk) {
        self.chunks.push(chunk);
    }

    fn flush(&mut self) {
        if self.pending_spans.is_empty() {
            return;
        }
        let text = std::mem::take(&mut self.pending_text);
        let spans = std::mem::take(&mut self.pending_spans);
        self.pending_day = None;
        self.push_chunk(Chunk {
            ordinal: 0,
            text,
            spans,
        });
    }

    fn finish(mut self) -> Vec<Chunk> {
        self.flush();
        for (ordinal, chunk) in self.chunks.iter_mut().enumerate() {
            chunk.ordinal = ordinal as i64;
        }
        self.chunks
    }
}

/// 一篇超过上限时在篇内切开：段尾优先，实在没有段尾就按上限硬切；相邻段之间按
/// `CHUNK_OVERLAP_CHARS` 重叠。返回 `(块正文, span)`。
fn split_long_piece(piece: &Piece<'_>, start: usize, end: usize) -> Vec<(String, ChunkSpan)> {
    let characters: Vec<char> = piece.text.chars().collect();
    let mut parts = Vec::new();
    let mut position = start;

    while position < end {
        let hard_end = (position + MAX_CHUNK_CHARS).min(end);
        let boundary = if hard_end == end {
            end
        } else {
            last_line_end(&characters, position, hard_end).unwrap_or(hard_end)
        };

        let mut trimmed_start = position;
        let mut trimmed_end = boundary;
        while trimmed_start < trimmed_end && characters[trimmed_start].is_whitespace() {
            trimmed_start += 1;
        }
        while trimmed_end > trimmed_start && characters[trimmed_end - 1].is_whitespace() {
            trimmed_end -= 1;
        }
        if trimmed_start < trimmed_end {
            parts.push((
                characters[trimmed_start..trimmed_end].iter().collect(),
                ChunkSpan {
                    piece_id: piece.id.to_owned(),
                    start: trimmed_start as i64,
                    end: trimmed_end as i64,
                },
            ));
        }

        if boundary >= end {
            break;
        }
        let next = boundary.saturating_sub(CHUNK_OVERLAP_CHARS);
        position = if next > position { next } else { boundary };
    }

    parts
}

/// 在 `[start, end)` 里找**最后一个段尾**（换行）的位置，返回它后面一个字符的下标
/// （排他）。找不到就是这一段里没有换行。
///
/// 只认换行：句末标点不是断点（「段尾断，句尾不断」）。
fn last_line_end(characters: &[char], start: usize, end: usize) -> Option<usize> {
    (start..end)
        .rev()
        .find(|index| characters[*index] == '\n')
        .map(|index| index + 1)
}

/// 一篇正文去掉两端空白之后的字符区间；全空白返回 `None`。
fn trim_range(text: &str) -> Option<(usize, usize)> {
    let characters: Vec<char> = text.chars().collect();
    let mut start = 0;
    let mut end = characters.len();
    while start < end && characters[start].is_whitespace() {
        start += 1;
    }
    while end > start && characters[end - 1].is_whitespace() {
        end -= 1;
    }
    if start < end {
        Some((start, end))
    } else {
        None
    }
}
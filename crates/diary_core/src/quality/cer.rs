//! 转写质量集的指标：**CER（字错率）**与它的编辑操作明细。
//!
//! # 算法（写清楚，免得以后各自解释）
//!
//! 1. **规范化**：两边都去掉所有空白与标点（[`crate::quality::text::punct_normalize`]），
//!    **保留全角汉字与数字**，全角字母数字折半角。标点不计错——转写本来就带不出标点，
//!    把它算进分母是把指标做坏；
//! 2. **单位是「字」**：`char`（Unicode 标量值），不是字节、不是拼音音节、不是英文单词。
//!    中文一个字一个音节，字级 EDIT DISTANCE 就是本领域的通行做法；
//! 3. **编辑距离**：Levenshtein，代价 `替换 = 删除 = 插入 = 1`（不做音近加权——
//!    加权要一套拼音表，那是另一个决定，见文档「没有做到的」）；
//! 4. **CER = (S + D + I) / N**，`N` = 参考文本规范化后的字数。
//!    `N = 0` 时 CER 无定义：参考为空且假设也为空记 `0.0`，假设非空记 `None`
//!    （**不是 0、也不是 1**——没分母就别编一个数）。
//!
//! # 回溯出的编辑明细
//!
//! [`CerReport::ops`] 是**对齐之后的**操作序列，逐条给出 `位置 / 类型 / 参考字 / 假设字`。
//! 报表里只印前若干条（刷屏没意义），但接口是全的：定位「转写到底错在哪」就靠它。
//!
//! # 它能验到什么程度（诚实说明）
//!
//! CER 的计算**今天就能完整验证**：参考文本是自造的，假设文本可以是注入的任意结果
//! （测试用人工手算过的对照，见 `tests/ai_quality_test.rs`）。也就是说「CER 算得对不对」
//! 是被钉住的。**没有**被验证的是「真实语音的转写质量」——今天没有真实语料，
//! 音频是占位（见 `tests/quality/ai-transcript-set.json` 的 `audio.note`）。

use crate::quality::text::punct_normalize;

/// 一次编辑操作的类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditOpKind {
    /// 两边都有、但字不同。
    Substitute,
    /// 参考有、假设没有（漏字）。
    Delete,
    /// 假设多出来的字（多字）。
    Insert,
    /// 两边一致（对齐保留，便于定位）。
    Match,
}

impl EditOpKind {
    pub fn wire(&self) -> &'static str {
        match self {
            Self::Substitute => "substitute",
            Self::Delete => "delete",
            Self::Insert => "insert",
            Self::Match => "match",
        }
    }

    /// 这一条是不是错。`Match` 不是。
    pub fn is_error(&self) -> bool {
        !matches!(self, Self::Match)
    }
}

/// 一条编辑操作。`at` 是它在**参考文本**里的位置（插入操作取它左边的位置）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditOp {
    pub at: usize,
    pub kind: EditOpKind,
    /// 参考文本这一位上的字（插入时是 `None`）。
    pub reference: Option<char>,
    /// 假设文本这一位上的字（删除时是 `None`）。
    pub hypothesis: Option<char>,
}

/// 一条转写用例的 CER 报表。
#[derive(Debug, Clone, PartialEq)]
pub struct CerReport {
    /// 参考文本规范化后的字数（分母）。
    pub reference_len: usize,
    /// 假设文本规范化后的字数。
    pub hypothesis_len: usize,
    pub substitutions: usize,
    pub deletions: usize,
    pub insertions: usize,
    /// `(S + D + I) / N`；`N = 0` 且假设非空时是 `None`（没分母，不编数）。
    pub cer: Option<f64>,
    /// 对齐后的逐条操作。
    pub ops: Vec<EditOp>,
    /// 规范化之后的参考文本（便于报表里对照，不用于判分）。
    pub reference_normalized: String,
    /// 规范化之后的假设文本。
    pub hypothesis_normalized: String,
}

impl CerReport {
    /// 错误总数 `S + D + I`。
    pub fn errors(&self) -> usize {
        self.substitutions + self.deletions + self.insertions
    }

    /// 假设与参考**完全一致**（规范化之后）。
    pub fn is_exact(&self) -> bool {
        self.errors() == 0 && self.reference_len == self.hypothesis_len
    }

    /// 只印出错的那些操作（报表用）。
    pub fn error_ops(&self) -> impl Iterator<Item = &EditOp> {
        self.ops.iter().filter(|op| op.kind.is_error())
    }
}

/// 算一次 CER。
pub fn cer(reference: &str, hypothesis: &str) -> CerReport {
    let reference_normalized = punct_normalize(reference);
    let hypothesis_normalized = punct_normalize(hypothesis);
    let reference_chars: Vec<char> = reference_normalized.chars().collect();
    let hypothesis_chars: Vec<char> = hypothesis_normalized.chars().collect();

    // 标准 Levenshtein 的 DP 表：`costs[i][j]` = 参考前 i 字 vs 假设前 j 字的最少编辑数。
    // 表不大（用例是短句），不做滚动数组——回溯要整张表，省这点内存不值。
    let rows = reference_chars.len() + 1;
    let cols = hypothesis_chars.len() + 1;
    let mut costs = vec![vec![0usize; cols]; rows];
    for (i, row) in costs.iter_mut().enumerate() {
        row[0] = i;
    }
    for (j, cell) in costs[0].iter_mut().enumerate() {
        *cell = j;
    }
    for i in 1..rows {
        for j in 1..cols {
            let substitution = costs[i - 1][j - 1]
                + usize::from(reference_chars[i - 1] != hypothesis_chars[j - 1]);
            let deletion = costs[i - 1][j] + 1;
            let insertion = costs[i][j - 1] + 1;
            costs[i][j] = substitution.min(deletion).min(insertion);
        }
    }

    // 回溯：优先「匹配/替换」（对角线），再删除，最后插入。顺序固定 => 明细可复现。
    let mut ops_reversed: Vec<EditOp> = Vec::new();
    let mut substitutions = 0usize;
    let mut deletions = 0usize;
    let mut insertions = 0usize;
    let (mut i, mut j) = (reference_chars.len(), hypothesis_chars.len());
    while i > 0 || j > 0 {
        if i > 0 && j > 0 {
            let same = reference_chars[i - 1] == hypothesis_chars[j - 1];
            let diagonal = costs[i - 1][j - 1] + usize::from(!same);
            if costs[i][j] == diagonal {
                let kind = if same {
                    EditOpKind::Match
                } else {
                    substitutions += 1;
                    EditOpKind::Substitute
                };
                ops_reversed.push(EditOp {
                    at: i - 1,
                    kind,
                    reference: Some(reference_chars[i - 1]),
                    hypothesis: Some(hypothesis_chars[j - 1]),
                });
                i -= 1;
                j -= 1;
                continue;
            }
        }
        if i > 0 && costs[i][j] == costs[i - 1][j] + 1 {
            deletions += 1;
            ops_reversed.push(EditOp {
                at: i - 1,
                kind: EditOpKind::Delete,
                reference: Some(reference_chars[i - 1]),
                hypothesis: None,
            });
            i -= 1;
            continue;
        }
        // 剩下的只可能是插入（DP 表保证）。
        insertions += 1;
        ops_reversed.push(EditOp {
            at: i,
            kind: EditOpKind::Insert,
            reference: None,
            hypothesis: Some(hypothesis_chars[j - 1]),
        });
        j -= 1;
    }
    ops_reversed.reverse();

    let reference_len = reference_chars.len();
    let errors = substitutions + deletions + insertions;
    let cer = if reference_len == 0 {
        if hypothesis_chars.is_empty() {
            Some(0.0)
        } else {
            None
        }
    } else {
        Some(errors as f64 / reference_len as f64)
    };

    CerReport {
        reference_len,
        hypothesis_len: hypothesis_chars.len(),
        substitutions,
        deletions,
        insertions,
        cer,
        ops: ops_reversed,
        reference_normalized,
        hypothesis_normalized,
    }
}

/// 宏观 CER：把多条的 `S + D + I` 与 `N` 分别汇总再相除。
///
/// 与「逐条 CER 再平均」**不是一回事**：短句的错误率方差大，平均会被短句主导。
/// 报表两个都给，但判定门槛用**逐条**（每条都有自己的上限），宏观只做趋势参考。
pub fn macro_cer(reports: &[CerReport]) -> Option<f64> {
    let errors: usize = reports.iter().map(CerReport::errors).sum();
    let reference: usize = reports.iter().map(|report| report.reference_len).sum();
    if reference == 0 {
        return None;
    }
    Some(errors as f64 / reference as f64)
}
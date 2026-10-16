//! 判定规则要用的文本工具：**规范化**、**具体事实抽取**、**字符 n-gram 覆盖**。
//!
//! 这里的每一个函数都是纯函数、确定性、无依赖（不引分词器）——规则要能被逐条解释，
//! 也要能在核心的集成测试里原样复现。
//!
//! # 规范化做了什么、没做什么
//!
//! [`normalize_cjk`] 把「标点与空白」全部去掉，并把全角数字/字母折成半角。
//! 这是所有**包含类**判据的前置：`妈妈说，她下周复查。` 与 `妈妈说要复查` 在规范化后
//! 只剩实词，判据不会被一个逗号或一个「她」绊倒。
//!
//! **不做的**：不改写、不翻译、不做繁简转换、不做同义词归一。判定器不认识
//! 「复查」和「复检」是一回事——同义改写要靠用例里的 `anyOf` 词组显式写出来。
//! 让机器猜同义，是把「判定可复现」换成「判定看运气」。

use std::collections::BTreeSet;

/// 「编造」硬判据里「长片段无依据」的默认阈值（连续汉字数）。
///
/// 8 是实测选出来的：合理改写产生的拼接处最多 7 个字，而真正生造一个短句
/// （`他在医院挂了号`）是 8 个字。用例可以按自己的语料覆盖它。
pub const DEFAULT_NOVEL_RUN_CHARS: usize = 8;

/// n-gram 依据窗口的默认大小。3 是中文的经验值：三字以上才有实义。
pub const DEFAULT_NGRAM_SIZE: usize = 3;

/// 去掉空白与标点、全角折半角。判定用的「干文本」。
///
/// 标点是 Unicode 里的 `P*`（标点）与 `S*`（符号）两大类，加上所有空白。
/// 中日韩的 `、。「」『』（）` 都在 `P*` 里，所以不需要逐字符列举。
pub fn normalize_cjk(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    chars
        .iter()
        .enumerate()
        .filter(|(index, ch)| {
            !ch.is_whitespace() && !is_punct_or_symbol_at(&chars, *index)
        })
        .map(|(_, ch)| fold_width(*ch))
        .collect()
}

/// 带**上下文**的判断：`.` 夹在两个数字之间时是小数点（`46.3`），不是标点。
///
/// 这个例外必须按上下文判：`.` 在别处（句末、省略号里）仍然是标点。
/// 踩过的坑：`is_ascii_punctuation()` 会把小数点一起吃掉，于是 `46.3` 规范化成 `463`，
/// 「小数」就永远对不上了。
fn is_punct_or_symbol_at(chars: &[char], index: usize) -> bool {
    let ch = chars[index];
    if matches!(ch, '.' | '-') {
        let before = index.checked_sub(1).map(|i| chars[i]);
        let after = chars.get(index + 1).copied();
        let between_digits = before.is_some_and(|c| c.is_ascii_digit())
            && after.is_some_and(|c| c.is_ascii_digit());
        if between_digits {
            // `.` 是小数点（`46.3`），`-` 是日期成分的分隔（`2026-09-20`）：
            // 两种都要留住，否则「材料里有 2026-09-20、输出没有」看不出来。
            return false;
        }
    }
    is_punct_or_symbol(ch)
}

/// 这是不是一个「判定时该忽略」的字符：标点或符号。
///
/// 范围是**逐段核对过**的：区间必须避开数字与字母。踩过的坑有两个——
/// `\u{2000}..=\u{206F}` 看起来「把常见标点都包了」，其实不含间隔号 `·`（U+00B7）；
/// 而 `\u{FF00}..=\u{FF0F}` 如果写成 `\u{FF00}..=\u{FF20}` 就会把全角数字吃掉。
/// 区间写宽了会吃到数字，写窄了会漏掉中文标点——所以这里只列核对过的段。
fn is_punct_or_symbol(ch: char) -> bool {
    // 手写而不用 `char::is_ascii_punctuation`：中文标点不在 ASCII 里。
    matches!(ch,
        '\u{3000}'..='\u{303F}'   // CJK 符号与标点（、。「」『』〜…）
        | '\u{FF01}'..='\u{FF0F}' // 全角 ！＂＃…／（**从 FF01 起**：FF10 是数字０）
        | '\u{FF1A}'..='\u{FF20}' // 全角 ：；＜＝＞？＠
        | '\u{FF3B}'..='\u{FF40}' // 全角 ［＼］＾＿｀
        | '\u{FF5B}'..='\u{FF65}' // 全角 ｛｜｝～｟｠
        | '\u{2010}'..='\u{2027}' // 连字符与各类破折号
        | '\u{2030}'..='\u{205E}' // 千分号、引号、省略号、间隔号
        | '\u{00B7}'              // 间隔号 ·
        | '\u{FE30}'..='\u{FE4F}' // CJK 兼容形式
    ) || ch.is_ascii_punctuation()
        || ch.is_whitespace()
}

/// 全角数字/字母折半角；其余原样。只折 ASCII 可打印范围对应的全角段。
fn fold_width(ch: char) -> char {
    let code = ch as u32;
    if (0xFF10..=0xFF19).contains(&code) || (0xFF21..=0xFF3A).contains(&code)
        || (0xFF41..=0xFF5A).contains(&code)
    {
        char::from_u32(code - 0xFEE0).unwrap_or(ch)
    } else {
        ch
    }
}

/// CER 用的规范化：**去掉所有空白与标点**，只留字与数字（全角折半角）。
///
/// # 为什么标点要**去掉**（而不是留着算错）
///
/// 转写模型吐不吐标点是**随机的**：同一段音频跑两次，可能一次带逗号一次不带。
/// 把标点算进分母，等于让指标随运气抖动——「漏了一个逗号」既不该算错，也没法稳定复现。
/// 所以参考文本里的标点也在规范化时去掉，两边同一口径。
///
/// # 与 [`normalize_cjk`] 的关系
///
/// **同一个判据**（都用 [`is_punct_or_symbol_at`]），区别只在用途上：
/// `normalize_cjk` 是「判定包含关系」用的干文本，这里就是它——名字保留是因为
/// CER 那一侧读起来更清楚（「转写口径」）。
pub fn punct_normalize(text: &str) -> String {
    normalize_cjk(text)
}

/// 文本里**没有**被规范化认识的字符（`None` = 全认识）。
///
/// 判据是「输出必须是可判的中文」：一个塞满 emoji、零宽字符或拉丁乱码的输出，
/// 不该因为「规范化之后是空串」而被判成「什么都没编造」。有了这个检查，
/// 判据缺输入时会**明确失败**，而不是静默通过。
pub fn unsupported_chars(text: &str) -> Vec<char> {
    let chars: Vec<char> = text.chars().collect();
    let mut found: BTreeSet<char> = BTreeSet::new();
    for (index, ch) in chars.iter().enumerate() {
        if ch.is_whitespace() || is_punct_or_symbol_at(&chars, index) {
            continue;
        }
        let folded = fold_width(*ch);
        // 认识：CJK 统一表意文字（含扩展 A）、中文常用标点之外的汉字区、ASCII 字母数字。
        let known = folded.is_ascii_alphanumeric()
            || ('\u{4E00}'..='\u{9FFF}').contains(&folded)
            || ('\u{3400}'..='\u{4DBF}').contains(&folded)
            || ('\u{F900}'..='\u{FAFF}').contains(&folded);
        if !known {
            found.insert(*ch);
        }
    }
    found.into_iter().collect()
}

/// 抽出文本里的**具体数量**：阿拉伯数字 + 汉字数字。
///
/// 这是「编造」判据里**具体事实**那一类的主力：材料说「一次」，输出说「两次」；
/// 材料里根本没有数量，输出说「46.3 度」——数字是具体事实最干净的抓手。
///
/// # 汉字数字抽到哪一层（这是刻意的取舍）
///
/// 汉字数字到处都是虚指，全抽出来全是误报。口径是**两段**：
///
/// 1. 先找**虚指词**（[`VAGUE_WORDS`]：`一下`、`半天`、`一会儿`、`两下`…）的字符区间；
/// 2. 再取**尽量大的连续数字串**（同类的连着写算一个：`十一`、`二十二`、`46.3`），
///    **丢掉完全落在虚指区间里的串**；
/// 3. **孤立的汉字数字不抽**（`一`、`两`、`十`）：它们在 `一斤`、`一样`、`一起`、
///    `一束花` 里到处都是，抽出来一半是噪声。汉字数字**连写才抽**，阿拉伯数字永远抽。
///
/// 第 3 步看起来会漏事（`四点半` 只抽到 `四` 吗？不——`四` 是孤立的，也不抽），
/// 但**比对时用「双向包含」兜住了**（见 `judge::appears_in_material`）：
/// 材料写 `四点半` 而输出写 `四点`，两边抽出来的串互相包含，算命中。
/// 于是「孤立的汉字数字」只在输出里**凭空出现一个材料完全没有的数量**时才可能报，
/// 而那正是我们要抓的（`又多买了三束`）。
///
/// 结果按出现顺序、**允许重复**（`两次` 出现两遍就是两个事实点）。判据用「包含」
/// 而不是「计数」，所以重复不影响判定，但报表里能看到密度。
pub fn numbers_in(text: &str) -> Vec<String> {
    let normalized = normalize_cjk(text);
    let chars: Vec<char> = normalized.chars().collect();
    let vague = vague_spans(&chars);
    let is_blocked = |start: usize, end: usize| -> bool {
        // 完全落在某一个虚指区间里 ⇒ 整段丢掉。
        vague
            .iter()
            .any(|(low, high)| start >= *low && end <= *high)
    };

    let mut found: Vec<String> = Vec::new();
    let mut index = 0usize;
    while index < chars.len() {
        if !is_numeral_char(chars[index]) {
            index += 1;
            continue;
        }
        let start = index;
        let ascii_run = chars[index].is_ascii_digit();
        while index < chars.len()
            && (is_numeral_char(chars[index])
                // 数字之间的小数点属于同一个数量（`46.3`），但**只在纯阿拉伯数字串里**：
                // 汉字数字与阿拉伯数字不混成一段——`一斤` 里的一与 `十一` 里的十
                // 合成 `十一` 就错了（这是实测踩到的坑）。
                || (ascii_run
                    && matches!(chars[index], '.' | '-')
                    && index + 1 < chars.len()
                    && chars[index + 1].is_ascii_digit()))
        {
            index += 1;
        }
        if !is_blocked(start, index) {
            let run: String = chars[start..index].iter().collect();
            let next = chars.get(index).copied().unwrap_or('\0');
            if should_extract(&run, next) {
                found.push(run);
            }
        }
    }
    found
}

/// 虚指词表：这些不是「具体数量」，抽出来会误报。
///
/// 宁可漏抽几个数字（漏抽只是少一条判据），也不要把「一下子就过去了」判成编造。
const VAGUE_WORDS: [&str; 14] = [
    "一下", "两下", "一点", "一会", "一阵", "一半", "半天", "一直", "一起", "一定", "一边",
    "一番", "一句", "一声",
];

/// 找出文本里所有虚指词的字符区间（含端点，闭区间）。
fn vague_spans(chars: &[char]) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    for word in VAGUE_WORDS {
        let needle: Vec<char> = word.chars().collect();
        if needle.len() > chars.len() {
            continue;
        }
        for start in 0..=(chars.len() - needle.len()) {
            if chars[start..start + needle.len()] == needle[..] {
                spans.push((start, start + needle.len()));
            }
        }
    }
    spans
}

/// 数字字面量：阿拉伯数字 + 汉字数字。
fn is_numeral_char(ch: char) -> bool {
    ch.is_ascii_digit()
        || matches!(
            ch,
            '零' | '一' | '二' | '三' | '四' | '五' | '六' | '七' | '八' | '九' | '十' | '百'
                | '千' | '万' | '亿' | '两'
        )
}

/// 这个数字串要不要抽出来。`next` 是它后面紧跟的那个字。
///
/// | 数字串 | 抽不抽 | 为什么 |
/// |---|---|---|
/// | 阿拉伯数字（`3`、`46.3`） | 抽 | 无歧义 |
/// | 汉字数字连写（`十一`、`二十二`） | 抽 | 无歧义 |
/// | 孤立汉字数字 + **确数单位**（`一块`、`两只`、`三岁`） | 抽 | 这是具体数量 |
/// | 孤立汉字数字 + `点/天/年/月` | **不抽** | `一点`、`一天` 在口语里大量是虚指/程度副词 |
/// | 孤立汉字数字（`一斤`、`一束`） | **不抽** | `一样`、`一起`、`一直` 都是噪声 |
fn should_extract(run: &str, next: char) -> bool {
    if run.chars().all(|ch| ch.is_ascii_digit()) {
        return true;
    }
    if run.chars().count() >= 2 {
        return true;
    }
    MEASURE_WORDS.contains(&next)
}

/// 只收**确数单位**（有它就是一个具体数量）。刻意不收 `点`/`天`/`年`/`月`：
/// 「一点」「一天」「一年」在口语里大量是虚指或程度副词，收了会让判据天天误报。
const MEASURE_WORDS: [char; 15] = [
    '个', '只', '次', '块', '斤', '条', '杯', '张', '件', '本', '岁', '间', '门', '句', '场',
];

/// 抽出文本里的**引号内内容**：直接引用必须逐字来自材料，否则就是编了句话。
///
/// 支持 `“…”`、`「…」`、`『…』`、`"…"`、`'…'`。空引用忽略。
pub fn quoted_spans(text: &str) -> Vec<String> {
    const PAIRS: [(char, char); 5] = [
        ('\u{201C}', '\u{201D}'), // “ ”
        ('\u{300C}', '\u{300D}'), // 「 」
        ('\u{300E}', '\u{300F}'), // 『 』
        ('"', '"'),
        ('\'', '\''),
    ];
    let mut spans = Vec::new();
    for (open, close) in PAIRS {
        let mut rest = text;
        while let Some(start) = rest.find(open) {
            let after = &rest[start + open.len_utf8()..];
            let Some(end) = after.find(close) else { break };
            let inner = normalize_cjk(&after[..end]);
            if !inner.is_empty() {
                spans.push(inner);
            }
            rest = &after[end + close.len_utf8()..];
        }
    }
    spans
}

/// 字符 n-gram 的**覆盖情况**：返回 `(总 n-gram 数, 在参照文本里出现过的数)`。
///
/// 用到两处：
/// - [`novel_runs`] 找出「连着很长一段材料里没有的汉字」；
/// - 用例可以报一个「有多少比例的字符片段能在材料里找到」的**软信号**。
pub fn ngram_backing(text: &str, reference: &str, size: usize) -> (usize, usize) {
    let haystack = normalize_cjk(text);
    let needle_set = ngram_set(reference, size);
    let hay: Vec<char> = haystack.chars().collect();
    if hay.len() < size {
        return (hay.len(), 0);
    }
    let mut total = 0usize;
    let mut backed = 0usize;
    for window in 0..=(hay.len() - size) {
        let gram: String = hay[window..window + size].iter().collect();
        total += 1;
        if needle_set.contains(&gram) {
            backed += 1;
        }
    }
    (total, backed)
}

fn ngram_set(text: &str, size: usize) -> BTreeSet<String> {
    let chars: Vec<char> = normalize_cjk(text).chars().collect();
    let mut set = BTreeSet::new();
    if chars.len() < size {
        return set;
    }
    for window in 0..=(chars.len() - size) {
        set.insert(chars[window..window + size].iter().collect());
    }
    set
}

/// 输出里**材料完全没有依据的长字符片段**：这是「编造」判据的启发式那一半。
///
/// # 规则（三段，缺一不可）
///
/// 1. 把规范化的输出切成「连续汉字」片段（数字/标点/拉丁字母是断点，它们另有判据）；
/// 2. 在片段上找**最大无依据窗口**：窗口内**没有任何一个** `size`-gram 能在这段的
///    参照文本里找到（注意是「整段都没有」，不是「有一个窗口没有」）；
/// 3. **锚定过滤**：窗口内如果还能找到**至少一个**有依据的 `size`-gram，就把它丢掉。
///
/// # 第 3 步为什么必须有（这是把「合理改写」与「编造」分开的关键）
///
/// 中文没有词边界，合理改写会把**两个相邻小句**重新拼接，于是拼接处会造出一个
/// 材料里没有的短串。实测的例子（`gen-03`）：
///
/// ```text
/// 材料：…我又折回去把钱还了。 | 下午把阳台的花盆挪了位置…
/// 输出：…于是折回去把钱还了。 | 下午他挪了阳台的花盆…
/// 拼接处：      了下午他挪了阳台     ← 第 2 步会把它当成「8 字无依据」
/// ```
///
/// 但它里面 `下午`、`阳台`、`花盆` 都在材料里有依据，只是**没有连成一段**。
/// 这显然是改写而不是编造。反过来，真正编出来的一句（`「他在医院挂了号」`）
/// 整段都找不到任何有依据的片段——**没有锚**。所以第 3 步的判据是：
/// **无依据的连续片段里一个熟悉的片段都没有，才叫编造**。
///
/// 这条规则说明：判据不能只看「有没有出现新材料里没有的串」（那必然误报），
/// 要看「有没有出现**整段都没有依托**的串」。
///
/// **仍是软信号与硬判据的混合**：硬判据默认阈值 [`DEFAULT_NOVEL_RUN_CHARS`] = 8 字
/// （8 个连续汉字的常见搭配在合理改写里几乎不会整段生造），用例可以按自己的语料调。
pub fn novel_runs(text: &str, reference: &str, size: usize, min_len: usize) -> Vec<String> {
    let needles = ngram_set(reference, size);
    let normalized = normalize_cjk(text);
    // 3-gram 视角的「有依据」判断；窗口不足 size 时视为有依据（相邻部分会覆盖）。
    let backed_at = |chars: &[char], start: usize, end: usize| -> bool {
        // `end` 是开区间；检查 [start, end) 内有没有任何一个 size-gram 命中。
        let mut cursor = start;
        while cursor + size <= end {
            let gram: String = chars[cursor..cursor + size].iter().collect();
            if needles.contains(&gram) {
                return true;
            }
            cursor += 1;
        }
        false
    };

    let mut runs = Vec::new();
    for run in cjk_runs(&normalized) {
        let chars: Vec<char> = run.chars().collect();
        if chars.len() < min_len {
            continue;
        }
        let mut start = 0usize;
        while start < chars.len() {
            // 2. 找最大无依据窗口。
            let mut end = start;
            while end < chars.len() && !backed_at(&chars, start, end + 1) {
                end += 1;
            }
            let length = end - start;
            // 3. 锚定过滤：窗口内部还有有依据的片段 ⇒ 是拼接artifact，不是编造。
            let anchored = backed_at(&chars, start, end);
            if length >= min_len && !anchored {
                runs.push(chars[start..end].iter().collect());
            }
            if end == start {
                start += 1;
            } else {
                // 跳到窗口末尾：窗口内不可能再有另一个「整段无依据」的子窗口。
                start = end;
            }
        }
    }
    runs
}

/// 把文本切成「连续汉字」片段（非汉字即断点）。
fn cjk_runs(text: &str) -> Vec<String> {
    let mut runs = Vec::new();
    let mut current = String::new();
    for ch in text.chars() {
        if is_cjk(ch) {
            current.push(ch);
        } else if !current.is_empty() {
            runs.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        runs.push(current);
    }
    runs
}

fn is_cjk(ch: char) -> bool {
    ('\u{4E00}'..='\u{9FFF}').contains(&ch)
        || ('\u{3400}'..='\u{4DBF}').contains(&ch)
        || ('\u{F900}'..='\u{FAFF}').contains(&ch)
}

/// 文本里有没有出现这个词组（规范化后包含即可）。
///
/// 判定「事实是否出现」与「禁用短语是否出现」都用它。**子串包含**是刻意的：
/// 「体检」出现在「体检结果」里就算出现，不需要对齐到词边界——中文没有空格，
/// 硬对齐到词边界只会把正确输出判错。
pub fn contains_phrase(text: &str, phrase: &str) -> bool {
    let needle = normalize_cjk(phrase);
    if needle.is_empty() {
        return false;
    }
    normalize_cjk(text).contains(&needle)
}

/// 出现次数（规范化后不重叠计数）。用于人称、时态这类**比例型**判据。
pub fn count_phrase(text: &str, phrase: &str) -> usize {
    let needle = normalize_cjk(phrase);
    if needle.is_empty() {
        return 0;
    }
    let haystack = normalize_cjk(text);
    let mut count = 0usize;
    let mut rest = haystack.as_str();
    while let Some(position) = rest.find(&needle) {
        count += 1;
        rest = &rest[position + needle.len()..];
    }
    count
}

/// 文本的「可见字符数」：规范化之后的字符数。长度判据用它，与标点无关。
pub fn visible_len(text: &str) -> usize {
    normalize_cjk(text).chars().count()
}
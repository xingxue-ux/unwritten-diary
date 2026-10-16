//! 质量集的数据结构：**只描述期望特征，不描述期望文本**。
//!
//! 三份集子（都在 `tests/quality/`）：
//!
//! | 文件 | 结构 | 判什么 |
//! |---|---|---|
//! | `ai-quality-set.json` | [`GenerationSet`] | 「输入材料 → 期望输出特征」：事实覆盖、跑题、编造、人称、时态、长度 |
//! | `ai-transcript-set.json` | [`TranscriptSet`] | 「短音频 + 参考文本 → CER 上限」 |
//! | `ai-injection-set.json` | [`InjectionSet`] | 「材料里嵌指令 → AI 行为不变」 |
//!
//! # 为什么是「特征」而不是「期望文本」
//!
//! 逐字比对会把**合理改写**判成失败，于是要么天天改期望文本、要么干脆不看结果。
//! 特征判定的代价是「要写清什么算通过」——这份文件就是那份写清。
//!
//! # `deny_unknown_fields` 是刻意的
//!
//! 字段名打错（`requireFacts` 写成 `requiredFacts`）如果不报错，就会静默变成
//! 「这条用例没有期望」——**一条永远通过的用例比一条失败的用例更坏**。
//! 所以每层都 `deny_unknown_fields`，打错当场失败。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::error::{CoreError, Result};

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path, what: &str) -> Result<T> {
    let raw = std::fs::read_to_string(path).map_err(|error| CoreError::InvalidArgument {
        reason: format!("读不到{what}：{}（{error}）", path.display()),
    })?;
    serde_json::from_str(&raw).map_err(|error| CoreError::InvalidArgument {
        reason: format!("{what}不是合法 JSON 或字段名打错了：{}（{error}）", path.display()),
    })
}

/// 质量集在仓库里的默认目录：`<仓库根>/tests/quality`。
///
/// 用 `CARGO_MANIFEST_DIR` 往上两级（`crates/diary_core` → 仓库根）：判定器在核心，
/// 语料在 `tests/`，两边不能各写一份相对路径。
pub fn quality_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/quality")
}

// ------------------------------------------------------------------ 生成质量集

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GenerationSet {
    pub version: i64,
    pub note: String,
    pub cases: Vec<GenerationCase>,
}

/// 一条生成质量用例。
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct GenerationCase {
    pub id: String,
    /// 分类（`first_person_past` / `third_person_past` / …），报表按它分组。
    pub category: String,
    /// 这条用例钉的是什么性质（人读的一句话）。
    pub what: String,
    pub material: Vec<Material>,
    pub template: TemplateSpec,
    pub expect: GenerationExpect,
    /// **回放模式**用的输出：人工写的**合格参考输出**（放在仓库里，可审查、可 diff）。
    ///
    /// 它不是模型产物。它的用处是让「判定器 + 报表 + CI 门禁」今天就能跑：
    /// 判定器必须认得出这份合格输出，也必须认得出测试里那些故意做坏的输出。
    pub replay_output: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Material {
    /// 显示名（**进「材料内」集合**：输出提到材料文件名不算编造）。
    pub name: String,
    /// 材料正文。自造语料，键盘可读。
    pub text: String,
}

/// 生成模板：人称/时态/风格/长度四项要求。
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct TemplateSpec {
    /// 模板标识（进报表，便于知道失败的是哪种模板）。
    pub id: String,
    /// 人读的模板说明。**不参与判定**，判定只看下面四个字段。
    pub note: String,
    pub person: Person,
    pub tense: Tense,
    pub style: Style,
    pub length: LengthRange,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Person {
    /// 第一人称：`我` 占支配地位。
    First,
    /// 第三人称：`我` 不是主语，`他/她/作者` 之类占支配地位。
    Third,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tense {
    /// 过去时：以「了/过/当时/那天」这类完成体与时间标记为主。
    Past,
    /// 现在时：不许整篇都是完成体（`了` 的比例要被压住）。
    Present,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Style {
    /// 口语：允许「吧/呢/啊」这类语气词。
    Colloquial,
    /// 书面：不许出现语气词，且要有书面连接词。
    Written,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct LengthRange {
    /// 可见字符数下限（规范化之后，标点不算）。
    pub min: usize,
    pub max: usize,
}

/// 「期望特征」。**判定规则的输入全在这里**，没有藏在代码里的特例。
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct GenerationExpect {
    /// **必须出现**的事实，每条给若干等价表述（`any_of`，命中任一即算覆盖）。
    #[serde(default)]
    pub facts: Vec<Fact>,
    /// **不得出现**的具体断言（跑题判据）：整组都出现才算违规。
    #[serde(default)]
    pub off_topic: Vec<ForbiddenPhrase>,
    /// **不得出现**的材料外具体事实（编造判据里「点名」的那一类）。
    #[serde(default)]
    pub forbidden_facts: Vec<ForbiddenPhrase>,
    /// 编造判据的参数与白名单。
    pub invention: InventionRules,
    /// 「长字符片段无依据」这个**软信号**的开关与阈值。
    #[serde(default)]
    pub soft_novelty: SoftNovelty,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Fact {
    pub id: String,
    /// 人读描述（失败原因里印出来，让人知道少了什么）。
    pub description: String,
    /// 等价表述：**任一**出现即算这条事实被覆盖。
    pub any_of: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ForbiddenPhrase {
    pub id: String,
    pub description: String,
    /// 这些词**全部**出现才算命中（避免一个泛词误报）。
    pub all_of: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct InventionRules {
    /// 要求「材料里出现的具体事实（数字）必须在输出里保留」。
    ///
    /// 这不是「编造」判据本身，而是它的**前提**：一条把材料里的关键数字丢掉的输出，
    /// 既没编也没忠实。丢掉的事实按 [`Self::min_fact_retention`] 的比例报失败。
    pub require_concrete_facts: bool,
    /// 具体事实保留率下限（0–1）。`require_concrete_facts = false` 时不看。
    pub min_fact_retention: f64,
    /// 编号连续汉字的长度阈值：≥ 这个长度的「材料里没有任何 n-gram 依据」的片段，
    /// 报成**具体的编造嫌疑**（硬判据，见 `invention` 一节）。
    pub novel_run_chars: usize,
    /// n-gram 的依据窗口大小（3 是经验值：中文三字以上才有实义）。
    pub ngram_size: usize,
    /// 白名单：这些词允许出现在输出里而**不需要**材料依据（模板自带的结构词）。
    #[serde(default)]
    pub allow: Vec<String>,
    /// 整篇的字片段有依据比例的下限（**软信号**，不是硬判据）。`0.0` = 不报。
    #[serde(default)]
    pub min_backed_ratio: f64,
}

#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SoftNovelty {
    /// 是否报「无依据长片段」这个软信号。
    #[serde(default)]
    pub report: bool,
    /// 片段长度阈值；0 时用 `invention.novel_run_chars`。
    #[serde(default)]
    pub min_chars: usize,
}

impl GenerationSet {
    pub fn load_default() -> Result<Self> {
        Self::load(&quality_dir().join("ai-quality-set.json"))
    }

    pub fn load(path: &Path) -> Result<Self> {
        let set: Self = read_json(path, "生成质量集")?;
        if set.version != 1 {
            return Err(CoreError::InvalidArgument {
                reason: format!("生成质量集版本是 {}，判定器只认 1", set.version),
            });
        }
        validate_unique("生成质量集", set.cases.iter().map(|case| case.id.as_str()))?;
        Ok(set)
    }

    /// 全部材料的正文拼起来：判「材料外」时的参照集合之一。
    pub fn material_text(case: &GenerationCase) -> String {
        let mut joined = String::new();
        for material in &case.material {
            joined.push_str(&material.text);
            joined.push('\n');
        }
        joined
    }
}

// ------------------------------------------------------------------ 转写质量集

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TranscriptSet {
    pub version: i64,
    pub note: String,
    pub cases: Vec<TranscriptCase>,
}

/// 一条转写用例。
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct TranscriptCase {
    pub id: String,
    pub category: String,
    pub what: String,
    pub audio: AudioSpec,
    /// 人工核对的参考文本（**自造口语**，不是真实录音的转写）。
    pub reference: String,
    pub expect: TranscriptExpect,
    /// 回放模式用的假设文本（**故意带口误残留**，好让 CER 不是恒等于 0 的空转）。
    ///
    /// 这不是「模型输出」，是**注入的假结果**：它证明的是 CER 计算与报表通了，
    /// 不是转写质量。文档与报表里都这么写。
    pub replay_hypothesis: String,
    /// **回放基线**：与 [`Self::replay_hypothesis`] 绑定的计算结果。
    ///
    /// 用来当**计算回归的锚**：CER 算错了这里就对不上，测试会红。
    /// 它**不是质量数字**（假结果算出来的 CER 跟转写质量无关）。
    pub replay_baseline: CerBaseline,
}

/// 一条转写用例的回放基线。
#[derive(Debug, Clone, Copy, Deserialize, PartialEq)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct CerBaseline {
    pub reference_chars: usize,
    pub hypothesis_chars: usize,
    pub errors: usize,
    pub cer: f64,
}

/// 音频占位描述。
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AudioSpec {
    /// `synthetic_silence` = **合成占位**，不是真实语音。
    pub kind: String,
    /// 采样率（Hz）。
    pub sample_rate: u32,
    pub channels: u16,
    /// 时长（毫秒）。占位音频是静音，时长只影响文件大小。
    pub duration_ms: u32,
    /// 人读的说明：说清这是占位、不是语音语料。
    pub note: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct TranscriptExpect {
    /// CER 上限（含）。超过就判失败。
    pub max_cer: f64,
    /// 人读的「这条允许错在哪」。
    pub note: String,
}

impl TranscriptSet {
    pub fn load_default() -> Result<Self> {
        Self::load(&quality_dir().join("ai-transcript-set.json"))
    }

    pub fn load(path: &Path) -> Result<Self> {
        let set: Self = read_json(path, "转写质量集")?;
        if set.version != 1 {
            return Err(CoreError::InvalidArgument {
                reason: format!("转写质量集版本是 {}，判定器只认 1", set.version),
            });
        }
        validate_unique("转写质量集", set.cases.iter().map(|case| case.id.as_str()))?;
        Ok(set)
    }
}

// ------------------------------------------------------------------ 防提示注入集

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct InjectionSet {
    pub version: i64,
    pub note: String,
    /// 系统提示的片段：输出里**任何一段连续 ≥ `leak_run_chars` 的字**命中它就判泄漏。
    ///
    /// 它不是真的系统提示（B4b 的提示词还没落地）——它是**注入集自己声明的
    /// 「这些内容一个字都不许出现在输出里」**。B4b 落地后应该把它换成真实的
    /// 系统提示片段（见文档「B4b 落地后需要接哪一处」）。
    pub system_prompt_fragments: Vec<String>,
    /// 判定泄漏的连续字数阈值。
    pub leak_run_chars: usize,
    /// 「顺着注入指令走了」的措辞特征。
    pub compliance_phrases: Vec<String>,
    pub cases: Vec<InjectionCase>,
}

/// 一条注入用例：**同一份材料**跑两次（干净 / 被投毒），要求判定结果一致。
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct InjectionCase {
    pub id: String,
    /// 攻击类型（`instruction_override` / `prompt_leak` / `role_escalation` / `topic_hijack`）。
    pub category: String,
    pub what: String,
    /// 干净材料。
    pub clean: Vec<Material>,
    /// 被投毒材料（通常在干净材料末尾追加一段注入文本）。
    pub poisoned: Vec<Material>,
    /// 注入块里**特有的**字符串，回显它们就是「照抄了指令」。
    pub markers: Vec<String>,
    pub template: TemplateSpec,
    pub expect: GenerationExpect,
    /// 回放模式：干净材料那一侧的输出（合格参考输出）。
    pub replay_clean_output: String,
    /// 回放模式：被投毒材料那一侧的输出。
    ///
    /// **它必须与干净侧在全部硬规则上判定一致**——这就是「AI 行为不变」的落地。
    /// 人工写这两段时最容易犯的错就是「顺手把注入的话回应了一下」，
    /// 于是干净/投毒不一致，判据当场抓住（这正是判据要干的事）。
    pub replay_poisoned_output: String,
}

impl InjectionSet {
    pub fn load_default() -> Result<Self> {
        Self::load(&quality_dir().join("ai-injection-set.json"))
    }

    pub fn load(path: &Path) -> Result<Self> {
        let set: Self = read_json(path, "防提示注入集")?;
        if set.version != 1 {
            return Err(CoreError::InvalidArgument {
                reason: format!("防提示注入集版本是 {}，判定器只认 1", set.version),
            });
        }
        validate_unique("防提示注入集", set.cases.iter().map(|case| case.id.as_str()))?;
        if set.system_prompt_fragments.is_empty() {
            return Err(CoreError::InvalidArgument {
                reason: "防提示注入集必须给 system_prompt_fragments（否则「不许泄漏」无从判定）"
                    .to_owned(),
            });
        }
        if set.compliance_phrases.is_empty() {
            return Err(CoreError::InvalidArgument {
                reason: "防提示注入集必须给 compliance_phrases（否则「顺着指令走了」无从判定）"
                    .to_owned(),
            });
        }
        for case in &set.cases {
            if case.markers.is_empty() {
                return Err(CoreError::InvalidArgument {
                    reason: format!(
                        "注入用例 {}：markers 不能为空（否则「回显了指令」无从判定）",
                        case.id
                    ),
                });
            }
            if case.poisoned.is_empty() {
                return Err(CoreError::InvalidArgument {
                    reason: format!("注入用例 {}：poisoned 材料不能为空", case.id),
                });
            }
        }
        Ok(set)
    }
}

// ------------------------------------------------------------------ 共用

/// id 唯一性。重复 id 会让报表两行看起来一样、对比时分不清是哪条。
fn validate_unique<'a>(what: &str, ids: impl Iterator<Item = &'a str>) -> Result<()> {
    let mut seen: BTreeMap<&str, usize> = BTreeMap::new();
    for id in ids {
        *seen.entry(id).or_insert(0) += 1;
    }
    let duplicates: Vec<&str> = seen
        .iter()
        .filter(|(_, count)| **count > 1)
        .map(|(id, _)| *id)
        .collect();
    if !duplicates.is_empty() {
        return Err(CoreError::InvalidArgument {
            reason: format!("{what}里有重复 id：{}", duplicates.join("、")),
        });
    }
    Ok(())
}

/// 材料的显示名与正文一起拼成「材料内」参照文本。
pub fn materials_text(materials: &[Material]) -> String {
    let mut joined = String::new();
    for material in materials {
        joined.push_str(&material.name);
        joined.push('\n');
        joined.push_str(&material.text);
        joined.push('\n');
    }
    joined
}
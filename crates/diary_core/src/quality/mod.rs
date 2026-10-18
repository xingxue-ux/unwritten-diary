//! issue #93（B4b-ai-quality）**前半**：AI 质量集的判定器、CER 计算与引擎接缝。
//!
//! # 这一层是什么、不是什么
//!
//! 是：**判定规则**（把「期望特征」判成可复现的通过/失败 + 原因）、**CER 算法**、
//! 以及两条**接缝**（生成引擎、转写引擎）——让探针命令与集成测试用同一套口径，
//! 改动后重跑能看出数字变差。
//!
//! 不是：产品代码。整个模块由 `ai-quality` feature 门控（测试里由 `cfg(test)` 打开），
//! 产品构建里一个字节都不进。
//!
//! # 为什么判定器在核心，而质量集 JSON 在 `tests/quality/`
//!
//! 判定器要给两处用：`tools/probe` 的命令行（人看的报表）与 `crates/diary_core/tests/`
//! 的集成测试（CI 门禁）。集成测试**不能**依赖探针 crate（依赖方向反了），所以判定器
//! 只能待在核心；语料是数据，放在 `tests/quality/` 与检索质量集同一个地方。
//!
//! # 两条接缝（这是「待接」的地方）
//!
//! [`GenerateEngine`] 与 [`TranscriptEngine`] 是**故意留薄**的两个 trait：
//!
//! - 回放实现（[`ScriptedGenerateEngine`] / [`ScriptedTranscriptEngine`]）今天就能跑，
//!   CI 不联网；
//! - 真实现（[`OpenAiCompatibleGenerateEngine`] / [`OpenAiCompatibleTranscriptEngine`]）
//!   走的是 B4a 已经落地的 Provider 路径（`adapter` + `Transport`），**它真的发请求**，
//!   但只是「把拼好的提示词发出去、把回文取回来」，**不含任何 B4b 的提示词与上下文装配**；
//! - **B4b 落地后要接的就一处**：把 `ScriptedGenerateEngine` 换成「B4b 的核心方法」，
//!   或给 `GenerateEngine` 加一个直接调用它的实现。**调用点就是 [`GenerateEngine::generate`]
//!   这一行**（`tools/probe/src/ai_quality.rs` 里 `engine.generate(...)` 的那一处）。
//!   在那之前，判定器、质量集、报表、门禁全部可用——只是量的是「回放出来的输出」。
//!
//! 转写同理：**#91 落地后接 [`TranscriptEngine::transcribe`]**。

pub mod case;
pub mod cer;
pub mod engine;
pub mod judge;
pub mod text;

pub use case::{
    materials_text, quality_dir, AudioSpec, CerBaseline, Fact, ForbiddenPhrase, GenerationCase,
    GenerationExpect, GenerationSet, InjectionCase, InjectionSet, InventionRules, LengthRange,
    Material, Person, ReplayVerdict,
    SoftNovelty, Style, TemplateSpec, Tense, TranscriptCase, TranscriptExpect, TranscriptSet,
};
pub use cer::{cer, macro_cer, CerReport, EditOp, EditOpKind};
pub use engine::{
    placeholder_audio, synthetic_wav, GenerateEngine, GenerateRequest, GeneratedText,
    OpenAiCompatibleGenerateEngine, OpenAiCompatibleTranscriptEngine, ScriptedGenerateEngine,
    ScriptedTranscriptEngine, TranscriptEngine, TranscriptOutcome, TranscriptRequest,
};
pub use judge::{
    judge_against, judge_generation, judge_injection, CleanlinessRules, CleanlinessVerdict,
    FeatureOutcome, GenerationVerdict, InjectionVerdict, RuleOutcome, SoftSignal,
};
pub use text::{normalize_cjk, numbers_in, punct_normalize, unsupported_chars};
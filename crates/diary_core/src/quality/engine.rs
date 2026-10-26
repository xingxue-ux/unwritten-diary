//! 两条**接缝**（这是 issue #93 前半留给后续落地的那两处）与它们的两种实现。
//!
//! ```text
//!                    ┌───────────────────────── 回放（默认，CI 可跑、不联网）
//!   GenerateEngine ──┤
//!                    └───────────────────────── 真模型（手动，--live，真的发请求）
//! ```
//!
//! # B4b 落地后要接哪一处
//!
//! **接 [`GenerateEngine::generate`] 这一个方法。** 今天它有两种实现：
//!
//! - [`ScriptedGenerateEngine`]：把「固定的模型输出」当输入回放（`--replay`，默认）；
//! - [`OpenAiCompatibleGenerateEngine`]：走 B4a 已落地的 Provider 路径，**真的发请求**，
//!   但只把「拼好的提示词」发出去、把回文取回来。**它不含 B4b 的提示词与上下文装配**
//!   ——那是 B4b 的事（见 `crates/diary_core/src/provider/mod.rs` 里 `respond_once` 的
//!   注释：「提示词与上下文拼装是 B4b 的事」）。
//!
//! B4b（#64）落地后，在 `tools/probe/src/ai_quality.rs` 里把引擎从
//! `ScriptedGenerateEngine` 换成「B4b 核心方法」的实现即可——**判定器、质量集、
//! 报表、CI 门禁一行都不用动**，因为判定只看输出文本。若 B4b 的核心方法是
//! 「`&mut Core` + 记录 id → 生成正文」，新实现就在 `generate` 里调它：
//!
//! ```ignore
//! impl GenerateEngine for B4bEngine<'_> {
//!     fn generate(&self, request: &GenerateRequest) -> Result<GeneratedText> {
//!         // TODO(B4b/#64)：这里改成调用 B4b 的核心方法（准备好资料库 + 回放 Transport，
//!         //   由它拼提示词、取快照、按模板生成）。今天只能用拼好的提示词调 Provider。
//!         todo!("接 B4b 的核心生成方法")
//!     }
//! }
//! ```
//!
//! # #91 落地后要接哪一处
//!
//! **接 [`TranscriptEngine::transcribe`] 这一个方法**（转写链 #91）。今天的
//! [`ScriptedTranscriptEngine`] 回放 [`TranscriptCase::replayHypothesis`]；
//! [`OpenAiCompatibleTranscriptEngine`] 走 B4a 的 Audio Transcriptions 路径。
//! #91 落地后换成一个调它核心方法的实现即可。

use std::sync::Arc;

use crate::error::{CoreError, Result};
use crate::model::{Provider, ProviderCapability, ProviderTestStatus};
use crate::provider::adapter::{GenerateRequest as AdapterGenerateRequest, CALL_TIMEOUT_MS};
use crate::provider::transport::{HttpRequest, HttpResponse, Transport};
use crate::provider::adapter;
use crate::quality::case::{AudioSpec, TranscriptCase};

/// 一次生成请求：**提示词已经拼好**（材料 + 模板说明）。
///
/// 判定器不关心提示词怎么来的——那是 B4b 的事。这里只要求「给了材料与模板，
/// 能拿回一段文本」。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenerateRequest {
    /// 用例 id（回放引擎按它取输出；真模型引擎不用它）。
    ///
    /// 显式带一个 id 而不是让回放引擎去猜：猜（比如拿提示词当键）会让
    /// 「改了质量集里的一句话」变成「回放取不到输出」或者更坏——**取到另一条的输出**。
    pub case_id: String,
    /// 材料正文（多篇拼在一起，带篇名）。
    pub material_text: String,
    /// 模板说明（人称/时态/风格/长度的自然语言描述）。
    pub template_note: String,
    /// 送出去的完整提示词。回放实现不用它；真模型实现直接发它。
    pub prompt: String,
    /// 系统提示。今天**没有**（B4b 落地后由它给）。
    pub system: Option<String>,
}

/// 一次生成的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneratedText {
    pub text: String,
    /// 远端回显的模型名（有就记，没有就不编）。回放实现给 `None`。
    pub model: Option<String>,
}

/// 生成引擎：这一条接缝是 B4b（#64）落地后要接的地方。
pub trait GenerateEngine {
    /// 生成一段文本。**这是 B4b 落地后要换掉的那一处。**
    fn generate(&self, request: &GenerateRequest) -> Result<GeneratedText>;

    /// 这个引擎的名字（报表里印出来，避免把回放数字当成质量数字）。
    fn name(&self) -> String;

    /// 这个引擎是不是「回放」（不是真实模型）。
    ///
    /// 报表靠它决定怎么措辞：回放的数字**不是质量数字**，只是「评测本身可复现」的证据。
    fn is_replay(&self) -> bool;
}

/// 一次转写请求。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptRequest {
    /// 用例 id（回放引擎按它取假设文本；真模型引擎不用它）。
    pub case_id: String,
    pub audio: Vec<u8>,
    pub sample_rate: u32,
    pub channels: u16,
    pub language: Option<String>,
}

/// 一次转写的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptOutcome {
    pub text: String,
    pub model: Option<String>,
}

/// 转写引擎：这一条接缝是 #91 落地后要接的地方。
pub trait TranscriptEngine {
    /// 转写一段音频。**这是 #91 落地后要换掉的那一处。**
    fn transcribe(&self, request: &TranscriptRequest) -> Result<TranscriptOutcome>;

    fn name(&self) -> String;

    fn is_replay(&self) -> bool;
}

// ------------------------------------------------------------------ 回放实现

/// 回放式生成引擎：按用例 id 回放**已知的输出**。
///
/// 这些输出是人工写的**参考输出**（放在质量集里），不是模型产物。它证明的是
/// 「判定器认得出合格输出」，而不是「模型能产出合格输出」。
pub struct ScriptedGenerateEngine {
    /// 用例 id → 输出。
    outputs: std::collections::BTreeMap<String, String>,
    /// 引擎显示名。
    label: String,
}

impl ScriptedGenerateEngine {
    pub fn new(
        outputs: impl IntoIterator<Item = (String, String)>,
        label: impl Into<String>,
    ) -> Self {
        Self {
            outputs: outputs.into_iter().collect(),
            label: label.into(),
        }
    }

    /// 从生成质量集与注入集里取出所有回放输出。
    pub fn from_sets(
        generation: &crate::quality::case::GenerationSet,
        injection: &crate::quality::case::InjectionSet,
    ) -> Self {
        let mut outputs = std::collections::BTreeMap::new();
        for case in &generation.cases {
            outputs.insert(case.id.clone(), case.replay_output.clone());
        }
        for case in &injection.cases {
            outputs.insert(
                format!("{}#clean", case.id),
                case.replay_clean_output.clone(),
            );
            outputs.insert(
                format!("{}#poisoned", case.id),
                case.replay_poisoned_output.clone(),
            );
        }
        Self::new(outputs, "回放（固定输出，不是模型产物）")
    }
}

impl GenerateEngine for ScriptedGenerateEngine {
    fn generate(&self, request: &GenerateRequest) -> Result<GeneratedText> {
        // 回放引擎按用例 id 取。取不到就**明确报错**：静默回一个空串会让
        // 「判定通过」变成假象（空串一定过不了，但报出来的原因会是「事实缺失」而不是
        // 「回放脚本没对上」——那种误诊最费时间）。
        self.by_id(&request.case_id)
    }

    fn name(&self) -> String {
        self.label.clone()
    }

    fn is_replay(&self) -> bool {
        true
    }
}

impl ScriptedGenerateEngine {
    /// 按用例 id 取回放输出。
    pub fn by_id(&self, id: &str) -> Result<GeneratedText> {
        self.outputs
            .get(id)
            .map(|text| GeneratedText {
                text: text.clone(),
                model: None,
            })
            .ok_or_else(|| CoreError::InvalidArgument {
                reason: format!(
                    "回放引擎里没有 {id} 的输出；质量集与回放脚本不一致（不该发生）"
                ),
            })
    }

    /// 全部回放输出的条数（报表里报出来：回放了几条）。
    pub fn len(&self) -> usize {
        self.outputs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.outputs.is_empty()
    }
}

/// 回放式转写引擎：回放用例里写好的**假设文本**。
///
/// 它**不读音频内容**（音频是占位静音）。这样 CER 的分子分母都是确定的，
/// 计算结果可复现——证明的是「CER 算得对」，不是「转写得好」。
pub struct ScriptedTranscriptEngine {
    /// 用例 id → 假设文本。
    hypotheses: std::collections::BTreeMap<String, String>,
    label: String,
}

impl ScriptedTranscriptEngine {
    pub fn from_set(set: &crate::quality::case::TranscriptSet) -> Self {
        let hypotheses = set
            .cases
            .iter()
            .map(|case| (case.id.clone(), case.replay_hypothesis.clone()))
            .collect();
        Self {
            hypotheses,
            label: "回放（注入的假转写结果，不是模型产物）".to_owned(),
        }
    }

    /// 按用例 id 取假设文本。
    pub fn by_id(&self, id: &str) -> Result<String> {
        self.hypotheses
            .get(id)
            .cloned()
            .ok_or_else(|| CoreError::InvalidArgument {
                reason: format!("回放引擎里没有 {id} 的假设文本；质量集与回放脚本不一致"),
            })
    }
}

impl TranscriptEngine for ScriptedTranscriptEngine {
    fn transcribe(&self, request: &TranscriptRequest) -> Result<TranscriptOutcome> {
        // 回放引擎按用例 id 取假设文本（与生成引擎同理：按 id，不猜）。
        Ok(TranscriptOutcome {
            text: self.by_id(&request.case_id)?,
            model: None,
        })
    }

    fn name(&self) -> String {
        self.label.clone()
    }

    fn is_replay(&self) -> bool {
        true
    }
}

// ------------------------------------------------------------------ 真模型实现

/// 走 B4a Provider 路径的生成引擎。**它会真的发请求。**
///
/// 它只做「把拼好的提示词发出去、把回文取回来」——**不含 B4b 的提示词与上下文装配**。
/// 所以它量的是「这个服务在给定提示词下产出什么」，不是「产品的日记生成质量」。
/// B4b 落地后应换成调它核心方法的实现（见本模块文档）。
pub struct OpenAiCompatibleGenerateEngine {
    provider: Provider,
    secret: String,
    transport: Arc<dyn Transport>,
}

impl OpenAiCompatibleGenerateEngine {
    pub fn new(provider: Provider, secret: impl Into<String>, transport: Arc<dyn Transport>) -> Self {
        Self {
            provider,
            secret: secret.into(),
            transport,
        }
    }

    /// 从环境变量建一个引擎（手动量测用）：
    ///
    /// - `DIARY_AI_ENDPOINT`（必填，例如 `http://127.0.0.1:11434/v1`）
    /// - `DIARY_AI_MODEL`（必填）
    /// - `DIARY_AI_KEY`（可空——本机服务通常不校验）
    pub fn from_env() -> Result<Self> {
        let provider = provider_from_env(ProviderCapability::GenerateText)?;
        let secret = std::env::var("DIARY_AI_KEY").unwrap_or_default();
        Ok(Self::new(
            provider,
            secret,
            Arc::new(crate::provider::transport::UreqTransport::new()),
        ))
    }
}

/// 从环境变量拼一个「手动量测」用的服务配置。**只有显式调它的路径才会用到**
/// （`--live`），所以它不破坏「默认不联网」。
fn provider_from_env(capability: ProviderCapability) -> Result<Provider> {
    let endpoint = std::env::var("DIARY_AI_ENDPOINT").map_err(|_| CoreError::InvalidArgument {
        reason: "真模型模式要显式给 DIARY_AI_ENDPOINT（例如 http://127.0.0.1:11434/v1）"
            .to_owned(),
    })?;
    let model = std::env::var("DIARY_AI_MODEL").map_err(|_| CoreError::InvalidArgument {
        reason: "真模型模式要显式给 DIARY_AI_MODEL".to_owned(),
    })?;
    Ok(Provider {
        id: "prv_ai_quality_probe".to_owned(),
        label: "AI 质量集手动量测".to_owned(),
        adapter_id: adapter::OPENAI_COMPATIBLE.to_owned(),
        endpoint: Some(endpoint),
        model: Some(model),
        capabilities: [capability].into_iter().collect(),
        credential_ref: None,
        enabled: true,
        test_status: ProviderTestStatus::Untested,
    })
}

impl GenerateEngine for OpenAiCompatibleGenerateEngine {
    fn generate(&self, request: &GenerateRequest) -> Result<GeneratedText> {
        let adapter = adapter::adapter_for(&self.provider.adapter_id).ok_or_else(|| {
            CoreError::ProviderNotConfigured {
                reason: format!("适配器 {} 不在本构建里", self.provider.adapter_id),
            }
        })?;
        // `adapter_for` 自 #103 起返回 `Adapter`（HTTP 协议或本机引擎）这一层枚举。
        // 本机引擎不实现 `generate_text`（`capabilities()` 里就没有它），所以闸门不可能
        // 把这条量测路径指到本机上；这里与 `provider::test`/执行者用同一个口径再挡一次。
        let adapter = match adapter {
            adapter::Adapter::Http(adapter) => adapter,
            adapter::Adapter::LocalLlm => {
                return Err(CoreError::ProviderNotConfigured {
                    reason: "在线质量量测不调用本机模型，请使用本机量测入口".to_owned(),
                });
            }
            adapter::Adapter::Local(engine) => {
                return Err(CoreError::ProviderNotConfigured {
                    reason: format!(
                        "服务「{}」是本机引擎「{}」，它不实现 generate_text；\
                         真模型量测要配一个支持 generate_text 的服务",
                        self.provider.label,
                        engine.id()
                    ),
                })
            }
        };
        let http = adapter.generate(
            &self.provider,
            &self.secret,
            &AdapterGenerateRequest {
                prompt: request.prompt.clone(),
                system: request.system.clone(),
            },
        )?;
        let response = self.send(&http)?;
        if !(200..300).contains(&response.status) {
            return Err(CoreError::ModelUnavailable {
                reason: format!(
                    "服务返回 {}：{}",
                    response.status,
                    response.excerpt(Some(&self.secret))
                ),
            });
        }
        let outcome = adapter.parse_generate(&response, Some(&self.secret))?;
        Ok(GeneratedText {
            text: outcome.text,
            model: outcome.model,
        })
    }

    fn name(&self) -> String {
        format!(
            "真模型 {} @ {}（{}）",
            self.provider.model.as_deref().unwrap_or("?"),
            self.provider.endpoint.as_deref().unwrap_or("?"),
            self.secret_status()
        )
    }

    fn is_replay(&self) -> bool {
        false
    }
}

impl OpenAiCompatibleGenerateEngine {
    fn send(&self, request: &HttpRequest) -> Result<HttpResponse> {
        self.transport
            .send(request)
            .map_err(|error| CoreError::ModelUnavailable {
                reason: format!("请求失败：{error}"),
            })
    }

    fn secret_status(&self) -> &'static str {
        if self.secret.is_empty() {
            "无密钥"
        } else {
            "有密钥"
        }
    }
}

/// 走 B4a Audio Transcriptions 路径的转写引擎。**它会真的发请求。**
///
/// 上传的是一个**合成占位 WAV**（见 [`synthetic_wav`]）——那里面没有人声。
/// 所以它今天量不出「转写质量」，只能验通路。这一点在文档里也写明。
pub struct OpenAiCompatibleTranscriptEngine {
    provider: Provider,
    secret: String,
    transport: Arc<dyn Transport>,
}

impl OpenAiCompatibleTranscriptEngine {
    pub fn new(provider: Provider, secret: impl Into<String>, transport: Arc<dyn Transport>) -> Self {
        Self {
            provider,
            secret: secret.into(),
            transport,
        }
    }

    pub fn from_env() -> Result<Self> {
        let provider = provider_from_env(ProviderCapability::TranscribeAudio)?;
        let secret = std::env::var("DIARY_AI_KEY").unwrap_or_default();
        Ok(Self::new(
            provider,
            secret,
            Arc::new(crate::provider::transport::UreqTransport::new()),
        ))
    }
}

impl TranscriptEngine for OpenAiCompatibleTranscriptEngine {
    fn transcribe(&self, request: &TranscriptRequest) -> Result<TranscriptOutcome> {
        // 转写走 multipart。B4a 的 adapter 只把 multipart 用在**探针**上
        // （`transcribe_probe_request` 是私有方法），所以这一片自己拼一个同形状的体。
        // 这样不必为量测改 B4a 的公开接口（那会牵动契约）。B4b/#91 落地后换掉。
        let endpoint = self.provider.endpoint.as_deref().unwrap_or_default();
        let model = self.provider.model.as_deref().unwrap_or_default();
        if endpoint.is_empty() || model.is_empty() {
            return Err(CoreError::ProviderNotConfigured {
                reason: "真模型转写要 endpoint 与 model".to_owned(),
            });
        }
        let url = format!("{}/audio/transcriptions", endpoint.trim_end_matches('/'));
        let (content_type, body) = multipart_wav(model, &request.audio, request.language.as_deref());
        let mut http = HttpRequest::new("POST", url, CALL_TIMEOUT_MS)
            .header("Content-Type", content_type)
            .header("Accept", "application/json")
            .body(body);
        if !self.secret.is_empty() {
            http = http.header("Authorization", format!("Bearer {}", self.secret));
        }
        let response = self
            .transport
            .send(&http)
            .map_err(|error| CoreError::ModelUnavailable {
                reason: format!("请求失败：{error}"),
            })?;
        if !(200..300).contains(&response.status) {
            return Err(CoreError::ModelUnavailable {
                reason: format!("服务返回 {}", response.status),
            });
        }
        let value: serde_json::Value = serde_json::from_slice(&response.body).map_err(|error| {
            CoreError::ModelUnavailable {
                reason: format!("转写响应不是合法 JSON：{error}"),
            }
        })?;
        let text = value
            .get("text")
            .and_then(|text| text.as_str())
            .ok_or_else(|| CoreError::ModelUnavailable {
                reason: "转写响应没有 text 字段".to_owned(),
            })?;
        Ok(TranscriptOutcome {
            text: text.to_owned(),
            model: Some(model.to_owned()),
        })
    }

    fn name(&self) -> String {
        format!(
            "真模型转写 {} @ {}",
            self.provider.model.as_deref().unwrap_or("?"),
            self.provider.endpoint.as_deref().unwrap_or("?")
        )
    }

    fn is_replay(&self) -> bool {
        false
    }
}

/// 拼一个上传音频的 multipart 请求体（手写，理由同 B4a 的探针：
/// 只有一个文件字段，形状固定，不值得引依赖）。
fn multipart_wav(model: &str, wav: &[u8], language: Option<&str>) -> (String, Vec<u8>) {
    const BOUNDARY: &str = "----buxieriji-aiquality-2b8e5d";
    let mut body: Vec<u8> = Vec::new();
    body.extend_from_slice(
        format!("--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"model\"\r\n\r\n{model}\r\n")
            .as_bytes(),
    );
    body.extend_from_slice(
        format!(
            "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"response_format\"\r\n\r\njson\r\n"
        )
        .as_bytes(),
    );
    if let Some(language) = language.filter(|value| !value.is_empty()) {
        body.extend_from_slice(
            format!(
                "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"language\"\r\n\r\n{language}\r\n"
            )
            .as_bytes(),
        );
    }
    body.extend_from_slice(
        format!(
            "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"file\"; \
             filename=\"sample.wav\"\r\nContent-Type: audio/wav\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(wav);
    body.extend_from_slice(format!("\r\n--{BOUNDARY}--\r\n").as_bytes());
    (format!("multipart/form-data; boundary={BOUNDARY}"), body)
}

// ------------------------------------------------------------------ 占位音频

/// 按 [`AudioSpec`] 合成一个**占位 WAV**：全零 PCM 的静音。
///
/// # 它是占位，不是语音语料
///
/// 里面**没有人声、没有任何内容**。它的用处只有一个：让「带音频的转写用例」
/// 有一个**结构合法**的输入（44 字节标准头 + 按 `sampleRate`/`channels` 算出的
/// 全零 PCM），从而把「读取 → 送引擎 → 收文本 → 算 CER → 出报表」这条管线跑通。
///
/// **真实语音语料没有**：`tests/quality/ai-transcript-set.json` 里每条都标了
/// `audio.kind = "synthetic_silence"` 与一句 `note`，回放模式下音频内容也不参与判定。
/// 所以今天 CER 能验到的是「算得对」，验不到「转写得好」（见文档「没有做到的」）。
pub fn synthetic_wav(spec: &AudioSpec) -> Vec<u8> {
    let bits_per_sample: u16 = 16;
    let channels: u16 = spec.channels.max(1);
    let sample_rate: u32 = spec.sample_rate.max(1);
    let frames: u64 = (sample_rate as u64 * spec.duration_ms as u64 / 1000).max(1);
    let bytes_per_sample: u64 = bits_per_sample as u64 / 8;
    let data_bytes: u64 = frames * channels as u64 * bytes_per_sample;
    let byte_rate: u32 = sample_rate * channels as u32 * bits_per_sample as u32 / 8;
    let block_align: u16 = channels * bits_per_sample / 8;
    let mut wav = Vec::with_capacity(44 + data_bytes as usize);
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&((36 + data_bytes) as u32).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes()); // fmt 块长度
    wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
    wav.extend_from_slice(&channels.to_le_bytes());
    wav.extend_from_slice(&sample_rate.to_le_bytes());
    wav.extend_from_slice(&byte_rate.to_le_bytes());
    wav.extend_from_slice(&block_align.to_le_bytes());
    wav.extend_from_slice(&bits_per_sample.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&(data_bytes as u32).to_le_bytes());
    wav.resize(44 + data_bytes as usize, 0); // 全零 = 静音
    wav
}

/// 给回放模式用：按用例的 `audio` 规格取占位音频。
pub fn placeholder_audio(case: &TranscriptCase) -> Vec<u8> {
    synthetic_wav(&case.audio)
}

//! 协议适配器：把「一个已配置的服务」翻译成具体的 HTTP 交互。
//!
//! # 为什么要有适配器这一层
//!
//! 任务书要求「供应商不硬编码进业务层」。核心只认 `ProviderCapability`，
//! 具体协议（路径、请求体、响应形状）都在适配器里。这一片**只内置一种**有公开
//! 文档的文本服务协议：`openai_compatible`（OpenAI Chat Completions 与
//! Audio Transcriptions 的兼容协议，很多本机/局域网服务也照着它做）。
//!
//! # 诚实的能力声明
//!
//! `capabilities()` 列出**这个适配器真的实现了**的能力。契约第 2.7 节的
//! `understand_image` 留在能力集合里（前端的枚举需要有它），但内置适配器
//! **没有实现**：配置里声明它也不会被排任务，连接测试会如实报「适配器没实现、
//! 没有发起调用」。插件带来的模型能力是 M4 的事。
//!
//! # 探针是真的调用
//!
//! 连接测试不是「ping 一下端口」：`generate_text` 的探针发一次最小的
//! chat completions 请求（`max_tokens = 1`），`transcribe_audio` 的探针上传一小段
//! 静音 WAV（0.05 秒，8 kHz 单声道）。**两者都可能计费**，所以只在用户显式授权
//! 的测试范围里发，报告里也如实写「发生了真实调用」。转写探针用空音频是刻意的：
//! 不伪造人声，也不把用户资料发出去；服务若因为音频太短而拒绝，报告会带出远端的
//! 提示并归一到 `model_unavailable`，不假装能力可用。

use crate::error::{CoreError, Result};
use crate::model::{Provider, ProviderCapability};

use super::normalize::redact;
use super::transport::HttpRequest;

/// 内置协议适配器的标识。
pub const OPENAI_COMPATIBLE: &str = "openai_compatible";

/// 连接测试的超时：20 秒。测试是用户等着看的动作，不该让他等两分钟。
pub const PROBE_TIMEOUT_MS: u64 = 20_000;

/// 真正生成的超时：120 秒。生成比探针长得多，但不设超时会让作业永远挂着。
pub const CALL_TIMEOUT_MS: u64 = 120_000;

/// 一次文本生成请求。
#[derive(Debug, Clone)]
pub struct GenerateRequest {
    pub prompt: String,
    pub system: Option<String>,
}

/// 一次文本生成的结果。`model` 是远端回显的模型名（有就记，没有就不编）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenerateOutcome {
    pub text: String,
    pub model: Option<String>,
}

/// 一种协议。实现方只管拼请求与解析响应，不碰存储、不碰队列。
pub trait ProtocolAdapter: Send + Sync {
    fn id(&self) -> &'static str;

    /// 这个适配器真的实现了的能力。
    fn capabilities(&self) -> &'static [ProviderCapability];

    /// 拼一个连接测试请求。**不发送**（发送是调用方的事）。
    fn probe(
        &self,
        provider: &Provider,
        secret: &str,
        capability: ProviderCapability,
    ) -> Result<HttpRequest>;

    /// 解析探针响应（只在 2xx 时调用）。结构不符要报 `model_unavailable`。
    fn parse_probe(
        &self,
        capability: ProviderCapability,
        response: &super::transport::HttpResponse,
        secret: Option<&str>,
    ) -> Result<()>;

    /// 拼一次真正的生成请求。
    fn generate(
        &self,
        provider: &Provider,
        secret: &str,
        request: &GenerateRequest,
    ) -> Result<HttpRequest>;

    /// 解析生成响应。
    fn parse_generate(
        &self,
        response: &super::transport::HttpResponse,
        secret: Option<&str>,
    ) -> Result<GenerateOutcome>;
}

/// 按标识取内置适配器。取不到就是「核心还没有这种协议」——不静默换一个。
pub fn adapter_for(adapter_id: &str) -> Option<&'static dyn ProtocolAdapter> {
    match adapter_id {
        OPENAI_COMPATIBLE => Some(&OpenAiCompatible {}),
        _ => None,
    }
}

/// 内置适配器清单（用于错误消息与文档，不含插件带来的）。
pub fn builtin_adapter_ids() -> &'static [&'static str] {
    &[OPENAI_COMPATIBLE]
}

// ------------------------------------------------------------ OpenAI 兼容

struct OpenAiCompatible {}

impl OpenAiCompatible {
    fn base_url(provider: &Provider) -> Result<String> {
        let endpoint = provider
            .endpoint
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| CoreError::ProviderNotConfigured {
                reason: format!("服务「{}」还没有配置 endpoint", provider.label),
            })?;
        Ok(endpoint.trim_end_matches('/').to_owned())
    }

    fn model(provider: &Provider) -> Result<String> {
        provider
            .model
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| CoreError::ProviderNotConfigured {
                reason: format!("服务「{}」还没有配置模型名", provider.label),
            })
    }

    /// 密钥为空就**不发**这个头：给本机服务发一个空的 `Bearer `（后面什么都没有）既没有意义，
    /// 也会被一些严格实现的服务当成**非法凭据**直接拒掉——那样用户会看到「认证失败」，
    /// 而真实原因只是「这个服务不需要密钥」。（#93 的评测在接真模型通路时发现的问题。）
    fn authorized(mut request: HttpRequest, secret: &str) -> HttpRequest {
        if !secret.trim().is_empty() {
            request = request.header("Authorization", format!("Bearer {secret}"));
        }
        request
    }

    fn transcribe_probe_request(provider: &Provider, secret: &str) -> Result<HttpRequest> {
        let url = format!("{}/audio/transcriptions", Self::base_url(provider)?);
        let model = Self::model(provider)?;
        let (content_type, body) = multipart_wav(&model);
        Ok(Self::authorized(
            HttpRequest::new("POST", url, PROBE_TIMEOUT_MS)
                .header("Content-Type", content_type)
                .header("Accept", "application/json")
                .body(body),
            secret,
        ))
    }
}

impl ProtocolAdapter for OpenAiCompatible {
    fn id(&self) -> &'static str {
        OPENAI_COMPATIBLE
    }

    fn capabilities(&self) -> &'static [ProviderCapability] {
        &[ProviderCapability::GenerateText, ProviderCapability::TranscribeAudio]
    }

    fn probe(
        &self,
        provider: &Provider,
        secret: &str,
        capability: ProviderCapability,
    ) -> Result<HttpRequest> {
        match capability {
            ProviderCapability::GenerateText => {
                let url = format!("{}/chat/completions", Self::base_url(provider)?);
                let model = Self::model(provider)?;
                // 最小的真实请求：一条 user 消息 + max_tokens = 1。
                // 这不是「假装调用」——它真的会走一次模型（可能计费），
                // 也只有这样才验得动「鉴权 + 模型 + 生成能力」三件事。
                let body = serde_json::json!({
                    "model": model,
                    "messages": [{"role": "user", "content": "ping"}],
                    "max_tokens": 1,
                    "stream": false,
                })
                .to_string();
                Ok(Self::authorized(
                    HttpRequest::new("POST", url, PROBE_TIMEOUT_MS)
                        .header("Content-Type", "application/json")
                        .header("Accept", "application/json")
                        .body(body.into_bytes()),
                    secret,
                ))
            }
            ProviderCapability::TranscribeAudio => {
                Self::transcribe_probe_request(provider, secret)
            }
            ProviderCapability::UnderstandImage => Err(CoreError::CapabilityUnsupported {
                reason: format!(
                    "适配器「{OPENAI_COMPATIBLE}」没有实现图像理解；不发起调用，也不排任务"
                ),
            }),
        }
    }

    fn parse_probe(
        &self,
        capability: ProviderCapability,
        response: &super::transport::HttpResponse,
        secret: Option<&str>,
    ) -> Result<()> {
        match capability {
            ProviderCapability::GenerateText => {
                // 结构对了就算过：内容可以是空的（max_tokens = 1 时完全可能）。
                parse_chat_completions(response, secret).map(|_| ())
            }
            ProviderCapability::TranscribeAudio => {
                parse_transcription(response, secret).map(|_| ())
            }
            ProviderCapability::UnderstandImage => Err(CoreError::CapabilityUnsupported {
                reason: "适配器没有实现图像理解".to_owned(),
            }),
        }
    }

    fn generate(
        &self,
        provider: &Provider,
        secret: &str,
        request: &GenerateRequest,
    ) -> Result<HttpRequest> {
        let url = format!("{}/chat/completions", Self::base_url(provider)?);
        let model = Self::model(provider)?;
        let mut messages = Vec::new();
        if let Some(system) = request.system.as_deref().filter(|text| !text.is_empty()) {
            messages.push(serde_json::json!({"role": "system", "content": system}));
        }
        messages.push(serde_json::json!({"role": "user", "content": request.prompt}));
        let body = serde_json::json!({
            "model": model,
            "messages": messages,
            "stream": false,
        })
        .to_string();
        Ok(Self::authorized(
            HttpRequest::new("POST", url, CALL_TIMEOUT_MS)
                .header("Content-Type", "application/json")
                .header("Accept", "application/json")
                .body(body.into_bytes()),
            secret,
        ))
    }

    fn parse_generate(
        &self,
        response: &super::transport::HttpResponse,
        secret: Option<&str>,
    ) -> Result<GenerateOutcome> {
        let value = parse_json(response, secret)?;
        let text = value
            .get("choices")
            .and_then(|choices| choices.get(0))
            .and_then(|choice| choice.get("message"))
            .and_then(|message| message.get("content"))
            .and_then(|content| content.as_str())
            .ok_or_else(|| CoreError::ModelUnavailable {
                reason: format!(
                    "远端响应不是 OpenAI 兼容的 chat completions 结构（没有 choices[0].message.content）：{}",
                    redact(&response.excerpt(), secret)
                ),
            })?;
        Ok(GenerateOutcome {
            text: text.to_owned(),
            model: value
                .get("model")
                .and_then(|model| model.as_str())
                .map(str::to_owned),
        })
    }
}

fn parse_chat_completions(
    response: &super::transport::HttpResponse,
    secret: Option<&str>,
) -> Result<GenerateOutcome> {
    OpenAiCompatible {}.parse_generate(response, secret)
}

fn parse_transcription(
    response: &super::transport::HttpResponse,
    secret: Option<&str>,
) -> Result<String> {
    let value = parse_json(response, secret)?;
    value
        .get("text")
        .and_then(|text| text.as_str())
        .map(str::to_owned)
        .ok_or_else(|| CoreError::ModelUnavailable {
            reason: format!(
                "远端响应不是 OpenAI 兼容的转写结构（没有 text 字段）：{}",
                redact(&response.excerpt(), secret)
            ),
        })
}

fn parse_json(
    response: &super::transport::HttpResponse,
    secret: Option<&str>,
) -> Result<serde_json::Value> {
    serde_json::from_slice(&response.body).map_err(|error| CoreError::ModelUnavailable {
        reason: format!(
            "远端响应不是合法 JSON（{error}）：{}",
            redact(&response.excerpt(), secret)
        ),
    })
}

/// 拼一个上传静音 WAV 的 multipart 请求体。
///
/// 手写 multipart（不用第三方 crate）：只有一个文件字段，形状是固定的，
/// 引一个大依赖不值得。boundary 是常量——内容里不会出现它（全是二进制 PCM）。
fn multipart_wav(model: &str) -> (String, Vec<u8>) {
    const BOUNDARY: &str = "----buxieriji-probe-7f3a1c";
    let mut body: Vec<u8> = Vec::new();
    body.extend_from_slice(
        format!(
            "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"model\"\r\n\r\n{model}\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(
        format!(
            "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"response_format\"\r\n\r\njson\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(
        format!(
            "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"file\"; \
             filename=\"probe.wav\"\r\nContent-Type: audio/wav\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(&probe_wav());
    body.extend_from_slice(format!("\r\n--{BOUNDARY}--\r\n").as_bytes());
    (format!("multipart/form-data; boundary={BOUNDARY}"), body)
}

/// 0.05 秒的 8 kHz 单声道 16 位静音 WAV。
///
/// 44 字节标准头 + 800 字节全零 PCM。是**合法音频**，不是随便一段字节：
/// 服务端的解码器不该因为它报「文件损坏」。
pub fn probe_wav() -> Vec<u8> {
    const SAMPLE_RATE: u32 = 8_000;
    const CHANNELS: u16 = 1;
    const BITS: u16 = 16;
    const SAMPLES: u32 = SAMPLE_RATE / 20; // 0.05 秒
    let data_bytes = SAMPLES * u32::from(CHANNELS) * u32::from(BITS / 8);

    let mut wav = Vec::with_capacity(44 + data_bytes as usize);
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_bytes).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16_u32.to_le_bytes());
    wav.extend_from_slice(&1_u16.to_le_bytes()); // PCM
    wav.extend_from_slice(&CHANNELS.to_le_bytes());
    wav.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
    let byte_rate = SAMPLE_RATE * u32::from(CHANNELS) * u32::from(BITS / 8);
    wav.extend_from_slice(&byte_rate.to_le_bytes());
    wav.extend_from_slice(&(CHANNELS * (BITS / 8)).to_le_bytes());
    wav.extend_from_slice(&BITS.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_bytes.to_le_bytes());
    wav.resize(44 + data_bytes as usize, 0);
    wav
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ProviderTestStatus;

    fn provider() -> Provider {
        Provider {
            id: "prv_test".to_owned(),
            label: "本地".to_owned(),
            adapter_id: OPENAI_COMPATIBLE.to_owned(),
            endpoint: Some("http://127.0.0.1:11434/v1/".to_owned()),
            model: Some("qwen".to_owned()),
            capabilities: [ProviderCapability::GenerateText].into_iter().collect(),
            credential_ref: Some("sec_test".to_owned()),
            enabled: true,
            test_status: ProviderTestStatus::Untested,
        }
    }

    #[test]
    fn generate_request_targets_chat_completions_with_bearer() {
        let request = OpenAiCompatible {}
            .generate(
                &provider(),
                "sk-secret",
                &GenerateRequest {
                    prompt: "你好".to_owned(),
                    system: None,
                },
            )
            .unwrap();
        assert_eq!(request.method, "POST");
        // endpoint 末尾的斜杠不会被拼成双斜杠。
        assert_eq!(request.url, "http://127.0.0.1:11434/v1/chat/completions");
        assert_eq!(request.get_header("Authorization"), Some("Bearer sk-secret"));
        let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
        assert_eq!(body["model"], "qwen");
        assert_eq!(body["messages"][0]["content"], "你好");
    }

    #[test]
    fn debug_never_prints_the_secret() {
        let request = OpenAiCompatible {}
            .generate(
                &provider(),
                "sk-super-secret-value",
                &GenerateRequest {
                    prompt: "hi".to_owned(),
                    system: None,
                },
            )
            .unwrap();
        let debug = format!("{request:?}");
        assert!(!debug.contains("sk-super-secret-value"), "{debug}");
        assert!(
            debug.to_ascii_lowercase().contains("authorization: ***"),
            "{debug}"
        );
    }

    #[test]
    fn probe_verifies_the_structure_of_the_response() {
        let ok = super::super::transport::HttpResponse::new(
            200,
            r#"{"model":"qwen","choices":[{"message":{"content":""}}]}"#,
        );
        assert!(OpenAiCompatible {}
            .parse_probe(ProviderCapability::GenerateText, &ok, None)
            .is_ok());

        let wrong = super::super::transport::HttpResponse::new(200, r#"{"result":"ok"}"#);
        let error = OpenAiCompatible {}
            .parse_probe(ProviderCapability::GenerateText, &wrong, None)
            .unwrap_err();
        assert_eq!(error.code().wire(), "model_unavailable");

        let transcript =
            super::super::transport::HttpResponse::new(200, r#"{"text":"你好"}"#);
        assert!(OpenAiCompatible {}
            .parse_probe(ProviderCapability::TranscribeAudio, &transcript, None)
            .is_ok());
    }

    #[test]
    fn image_capability_is_honestly_unsupported() {
        let error = OpenAiCompatible {}
            .probe(
                &provider(),
                "sk-secret",
                ProviderCapability::UnderstandImage,
            )
            .unwrap_err();
        assert_eq!(error.code().wire(), "provider_not_configured");
        assert!(error.to_string().contains("没有实现"));
    }

    #[test]
    fn probe_wav_is_a_valid_riff_wave() {
        let wav = probe_wav();
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        assert_eq!(wav.len(), 44 + 800);
    }
}
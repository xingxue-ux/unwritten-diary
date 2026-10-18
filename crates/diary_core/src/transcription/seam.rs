//! 质量集（#93）的 `TranscriptEngine` 接缝：**本地引擎**这一侧的实现。
//!
//! # 这条接缝是给谁的
//!
//! #93（PR #99，分支 `feat/B4b-ai-quality`）在 `crates/diary_core/src/quality/engine.rs`
//! 留了两个「换引擎」的点，其中转写那个是：
//!
//! ```text
//! trait TranscriptEngine {
//!     fn transcribe(&self, request: &TranscriptRequest) -> Result<TranscriptOutcome>;
//!     fn name(&self) -> String;
//!     fn is_replay(&self) -> bool;
//! }
//! ```
//!
//! 它今天有两种实现：`ScriptedTranscriptEngine`（回放质量集里人工写的假设文本，
//! **不联网**）与 `OpenAiCompatibleTranscriptEngine`（走 B4a 的 HTTP 路径，
//! 真的发请求）。**两者都不跑本机引擎**——#91 落地前没有本机引擎可跑。
//!
//! 这一片把第三种实现补齐：[`LocalTranscriptEngine`]，走本机 ONNX 引擎，
//! **同样不联网**，CER 因此第一次能量到「真引擎的输出」而不是回放。
//!
//! # 与 #99 合入之后的那一步（只有这一步，别的都不用动）
//!
//! 本文件**逐字对齐**了 #99 的 `TranscriptRequest` / `TranscriptOutcome` /
//! `TranscriptEngine` 三个签名（含 `case_id` 字段），因为这一片的分支基于 B4a，
//! 质量集模块还不在这里。等 #99 合入后：
//!
//! ```ignore
//! // crates/diary_core/src/quality/mod.rs 里原本 re-export 的是 #99 的 trait；
//! // 把下面这三行的导入换成 #99 的即可（其余一行都不用改）：
//! // use crate::quality::{TranscriptEngine, TranscriptOutcome, TranscriptRequest};
//! ```
//!
//! 也就是：**把 `use super::{...}` 换成 `use crate::quality::{...}`，
//! 并删掉本文件里那三个类型定义**。[`LocalTranscriptEngine`] 的方法体不动。
//! `tools/probe/src/ai_quality.rs` 的调用点（`transcript_engine_for`）改成
//! 「`--live-local` 时返回 `LocalTranscriptEngine::from_env()`」即可——
//! 报表、判定器、门禁一行都不用动，因为判定只看输出文本。
//!
//! # 语料仍然是合成占位
//!
//! 质量集喂进来的是 `placeholder_audio`：**全零静音的合法 WAV**（见 #99 的
//! `quality/engine.rs`）。所以这条接缝今天证明的是「真引擎能加载、能吃 WAV、
//! 能返回文本、CER 管线能跑」，**不是**「转写质量有多好」——真语音语料待补，
//! 这一条写在 `docs/architecture/m3-转写链.md` 的「没有做到的」里。

use std::sync::Arc;

use crate::error::{CoreError, Result};
use crate::provider::local_engine::{EngineConfig, LocalEngine, PcmRequest};
use crate::transcription::wav;

/// 一次转写请求。字段与 #99 的 `TranscriptRequest` **逐字一致**。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptRequest {
    /// 用例 id（回放引擎按它取假设文本；真引擎不用它）。
    pub case_id: String,
    /// 音频字节（一个完整的 WAV）。
    pub audio: Vec<u8>,
    pub sample_rate: u32,
    pub channels: u16,
    pub language: Option<String>,
}

/// 一次转写的结果。字段与 #99 的 `TranscriptOutcome` **逐字一致**。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptOutcome {
    pub text: String,
    pub model: Option<String>,
}

/// 转写引擎接缝。方法签名与 #99 的 `TranscriptEngine` **逐字一致**。
pub trait TranscriptEngine {
    fn transcribe(&self, request: &TranscriptRequest) -> Result<TranscriptOutcome>;

    fn name(&self) -> String;

    fn is_replay(&self) -> bool;
}

/// 走本机引擎的转写实现：**不联网**（这条路上根本没有 `Transport`）。
pub struct LocalTranscriptEngine {
    engine: Arc<dyn LocalEngine>,
    config: EngineConfig,
    /// 引擎显示名里带上模型目录，报表里能看出量的是哪一份模型。
    label: String,
}

impl LocalTranscriptEngine {
    /// 按环境变量/随包默认位置组装一次（与产品路径同一套解析）。
    ///
    /// 模型或引擎缺失时**不 pretend**：`from_env` 会把原因直接报出来，
    /// 探针于是「明确跳过」而不是跑出一堆空文本当结果。
    pub fn from_env() -> Result<Self> {
        let config = EngineConfig::resolve(None)?;
        Ok(Self::new(
            Arc::new(crate::transcription::sherpa::SherpaOnnxEngine),
            config,
        ))
    }

    /// 注入实现与配置（测试与探针都用它，真引擎与 stub 同一个形状）。
    pub fn new(engine: Arc<dyn LocalEngine>, config: EngineConfig) -> Self {
        let label = format!(
            "本机引擎 {}（模型 {}）",
            engine.id(),
            config.model_dir.display()
        );
        Self {
            engine,
            label,
            config,
        }
    }

    /// 已经在手的引擎实现（给需要复用同一份引擎的调用方）。
    pub fn engine(&self) -> &dyn LocalEngine {
        self.engine.as_ref()
    }

    pub fn config(&self) -> &EngineConfig {
        &self.config
    }
}

impl TranscriptEngine for LocalTranscriptEngine {
    fn transcribe(&self, request: &TranscriptRequest) -> Result<TranscriptOutcome> {
        // 请求带的是**一个完整的 WAV**（质量集的占位音频就是），所以先按 WAV 解码。
        // 头部里的采样率/声道数才是权威值：请求里的那两个字段是给远端 multipart
        // 用的元数据，这里不用它们去覆盖真实的音频格式。
        let decoded = wav::decode_bytes(&request.audio, &format!("用例 {} 的音频", request.case_id))?;
        if decoded.samples.is_empty() {
            return Err(CoreError::InvalidArgument {
                reason: format!("用例 {} 的音频里没有采样", request.case_id),
            });
        }
        let outcome = self.engine.transcribe(
            &self.config,
            &PcmRequest {
                samples: decoded.samples,
                sample_rate: decoded.sample_rate,
                language: request.language.clone(),
            },
        )?;
        Ok(TranscriptOutcome {
            text: outcome.text,
            model: outcome.model,
        })
    }

    fn name(&self) -> String {
        self.label.clone()
    }

    fn is_replay(&self) -> bool {
        // 它不是回放：真的跑了本机模型。报表据此把它和回放数字分开措辞。
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ProviderCapability;
    use crate::provider::local_engine::{EngineReadiness, PcmOutcome};
    use std::path::PathBuf;
    use std::sync::Mutex;

    /// 与真引擎同一 trait 的 stub：只回显「我收到了多少采样」。
    struct StubEngine {
        seen: Mutex<Vec<usize>>,
    }

    impl LocalEngine for StubEngine {
        fn id(&self) -> &'static str {
            "stub_local_engine"
        }
        fn capabilities(&self) -> &'static [ProviderCapability] {
            &[ProviderCapability::TranscribeAudio]
        }
        fn readiness(&self, _config: &EngineConfig) -> EngineReadiness {
            EngineReadiness {
                ready: true,
                code: String::new(),
                reason: "stub".to_owned(),
                next_step: None,
            }
        }
        fn transcribe(&self, _config: &EngineConfig, request: &PcmRequest) -> Result<PcmOutcome> {
            self.seen.lock().unwrap().push(request.samples.len());
            Ok(PcmOutcome {
                text: format!("收到 {} 个采样", request.samples.len()),
                model: Some("stub".to_owned()),
                language: None,
            })
        }
        fn probe(&self, _config: &EngineConfig) -> Result<String> {
            Ok("stub".to_owned())
        }
    }

    fn stub() -> Arc<StubEngine> {
        Arc::new(StubEngine {
            seen: Mutex::new(Vec::new()),
        })
    }


    fn config() -> EngineConfig {
        EngineConfig {
            engine_lib: None,
            model_dir: PathBuf::from("/nonexistent"),
            model_file: None,
            kind: crate::provider::local_engine::ModelKind::SenseVoice,
            language: None,
            num_threads: 1,
            itn: true,
        }
    }

    /// 一段 16 kHz 单声道、100 个采样的合法 WAV。
    fn wav_bytes() -> Vec<u8> {
        let mut wav = Vec::new();
        let data_bytes = 200_u32;
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(36 + data_bytes).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16_u32.to_le_bytes());
        wav.extend_from_slice(&1_u16.to_le_bytes());
        wav.extend_from_slice(&1_u16.to_le_bytes());
        wav.extend_from_slice(&16_000_u32.to_le_bytes());
        wav.extend_from_slice(&32_000_u32.to_le_bytes());
        wav.extend_from_slice(&2_u16.to_le_bytes());
        wav.extend_from_slice(&16_u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&data_bytes.to_le_bytes());
        wav.resize(44 + data_bytes as usize, 0);
        wav
    }

    #[test]
    fn seam_decodes_wav_and_delegates_to_the_engine() {
        let stub = stub();
        let engine = LocalTranscriptEngine::new(stub.clone(), config());
        let outcome = engine
            .transcribe(&TranscriptRequest {
                case_id: "tr-01".to_owned(),
                audio: wav_bytes(),
                sample_rate: 16_000,
                channels: 1,
                language: Some("zh".to_owned()),
            })
            .unwrap();
        assert_eq!(outcome.text, "收到 100 个采样");
        assert_eq!(outcome.model.as_deref(), Some("stub"));
        assert!(!engine.is_replay(), "本机引擎不是回放");
        assert_eq!(stub.seen.lock().unwrap().last().copied(), Some(100));
    }

    #[test]
    fn seam_rejects_non_wav_bytes_instead_of_returning_empty_text() {
        let engine = LocalTranscriptEngine::new(stub(), config());
        let error = engine
            .transcribe(&TranscriptRequest {
                case_id: "tr-02".to_owned(),
                audio: b"not a wav".to_vec(),
                sample_rate: 16_000,
                channels: 1,
                language: None,
            })
            .unwrap_err();
        assert_eq!(
            error.code(),
            crate::error::ErrorCode::UnsupportedFormat,
            "{error}"
        );
    }
}
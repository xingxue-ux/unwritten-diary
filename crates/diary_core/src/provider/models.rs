//! 服务可用模型列表检测（`GET {endpoint}/models`）：**用户点「检测」才出网**。
//!
//! # 这是显式调用，不是启动探测
//!
//! [`list_models`] 与 `providers.test` 同一个口径：只有用户显式发起时才发请求。
//! 打开资料库、`providers.list`、`presets()` 都不会碰它，保存配置时也**不会**
//! 「顺手试一下」。没有调用就没有请求——测试用「一调用就 panic」的传输层证明这一点
//! （见 `provider::transport` 的模块文档）。
//!
//! # 密钥
//!
//! 明文只在拼 `Authorization: Bearer ...` 的那一刻从 `SecretStore` 取；错误消息与
//! 返回值都过一遍擦除（`normalize::redact`）。响应体从不原样返回给上层。
//!
//! # 只支持 OpenAI 兼容的格式
//!
//! 内置适配器里只有 `openai_compatible` 有「列模型」这件事：
//! `GET {endpoint}/models`，响应 `{"object":"list","data":[{"id":"..."}]}`。
//! 本地引擎（`sherpa_onnx`）与别的适配器**如实报「不支持列举模型」**，不去猜它们的
//! 格式（本地引擎的 `model` 是模型目录，没有远端 `/models` 可查）。
//!
//! 错误映射沿用 `provider::normalize` 的归一化表：
//! 401/403 → `authentication_failed`、超时 → `provider_timeout`、
//! 连不上/5xx → `network_unavailable`、429 → `rate_limited`、
//! 响应解析不了 → `model_unavailable`（消息里有可读的响应摘录）。
//! 没配 endpoint / 没配凭据 → `provider_not_configured`，消息给可执行的下一步。

use crate::error::{CoreError, Result};
use crate::model::Provider;
use crate::Core;

use super::adapter::{self, OPENAI_COMPATIBLE, PROBE_TIMEOUT_MS};
use super::normalize::{self, redact};
use super::transport::{HttpRequest, HttpResponse};

/// 一个可用的模型。字段只保留「列出模型」真正能给出的东西。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderModelInfo {
    /// 模型名（原样来自远端；已经过密钥擦除）。
    pub id: String,
    /// OpenAI 兼容响应里的可选字段 `owned_by`；没有就是 `None`（缺字段不是错误）。
    pub owned_by: Option<String>,
}

/// 检测某个服务可用的模型列表。**只有显式调用才发请求。**
///
/// 端点：`GET {endpoint}/models`（endpoint 末尾的 `/` 不会拼成双斜杠）。
/// 鉴权：`Authorization: Bearer <secret>`，密钥只在拼这一行时从 `SecretStore` 取。
///
/// 返回值按远端响应里的顺序给（不排序、不猜测）；条目缺 `id` 的会被跳过，
/// 缺 `owned_by` 的只是 `None`——**可选字段缺失不算错误**。
pub fn list_models(core: &Core, provider_id: &str) -> Result<Vec<ProviderModelInfo>> {
    let provider = super::load_provider(&core.conn, provider_id)?;

    // 适配器是实现层的事实：没实现「列模型」的，不要拿 OpenAI 的形状去猜它。
    if provider.adapter_id != adapter::OPENAI_COMPATIBLE {
        return Err(CoreError::CapabilityUnsupported {
            reason: unsupported_adapter_reason(&provider),
        });
    }

    let endpoint = provider
        .endpoint
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| CoreError::ProviderNotConfigured {
            reason: format!(
                "服务「{}」还没有配置 endpoint。下一步：在「设置 → 远端服务」里把 endpoint \
                 填成服务根地址（例如 https://api.deepseek.com，不要带 /chat/completions \
                 或 /models），保存后再点「检测模型」",
                provider.label
            ),
        })?;

    // 明文只在这一刻取出，之后只存在于请求头与错误擦除的比对里。
    let secret = super::provider_secret(core, &provider)?
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| CoreError::ProviderNotConfigured {
            reason: format!(
                "服务「{}」还没有可用的凭据。下一步：先保存一次密钥（明文只进 SecretStore，\
                 配置里只留引用），再点「检测模型」；本机服务不校验密钥时填一个非空占位即可",
                provider.label
            ),
        })?;

    let url = format!("{}/models", endpoint.trim_end_matches('/'));
    let request = HttpRequest::new("GET", url, PROBE_TIMEOUT_MS)
        .header("Accept", "application/json")
        .header("Authorization", format!("Bearer {secret}"));

    // 唯一的出网点：与 `providers.test` 同一个 `Transport`。
    let outcome = core.transport().send(&request);
    let response = match outcome {
        Ok(response) => response,
        Err(error) => return Err(normalize::transport_error(&error, Some(&secret))),
    };
    if !(200..300).contains(&response.status) {
        return Err(normalize::http_error(&response, Some(&secret)));
    }
    parse_models(&response, &secret)
}

/// 「这个适配器不支持列模型」的如实说法。本机引擎单独讲清楚：它没有远端可查。
fn unsupported_adapter_reason(provider: &Provider) -> String {
    let is_local =
        adapter::adapter_for(&provider.adapter_id).is_some_and(|adapter| adapter.is_local());
    if is_local {
        format!(
            "适配器「{}」是本机引擎，不支持列举模型：它的 model 是一项模型目录，\
             没有远端 /models 可查；要换目录请直接填 model=<目录>",
            provider.adapter_id
        )
    } else {
        format!(
            "适配器「{}」不支持列举模型：内置实现里只有「{OPENAI_COMPATIBLE}」有 \
             GET /models；请改用该适配器，或手动填写模型名",
            provider.adapter_id
        )
    }
}

/// 解析 OpenAI 兼容的模型列表响应。
///
/// 容错边界：
/// - 合法 JSON 但没有 `data` 字段 → `model_unavailable`（可读消息 + 响应摘录）；
/// - `data` 不是数组 → 同上；
/// - 条目缺 `id` / `id` 为空 → **跳过这一条**（不因为一个怪条目丢掉整份列表）；
/// - 条目缺 `owned_by` → `None`（可选字段，缺了不是错误）；
/// - `data` 为空数组 → 返回空列表（远端确实可以一个模型都没有）。
///
/// 远端把密钥回显进 body 的极端情况下，`id` / `owned_by` 也会过一遍 `redact`：
/// 返回值里不允许出现明文，这一条不依赖对远端行为的信任。
fn parse_models(response: &HttpResponse, secret: &str) -> Result<Vec<ProviderModelInfo>> {
    let excerpt = redact(&response.excerpt(), Some(secret));
    let value: serde_json::Value =
        serde_json::from_slice(&response.body).map_err(|error| CoreError::ModelUnavailable {
            reason: format!("远端响应不是合法 JSON（{error}）：{excerpt}"),
        })?;

    let data = value
        .get("data")
        .ok_or_else(|| CoreError::ModelUnavailable {
            reason: format!(
                "远端响应不是 OpenAI 兼容的模型列表（没有 data 字段）：{excerpt}；\
                 检查 endpoint 是否指向服务的 API 根地址（例如 https://api.deepseek.com）"
            ),
        })?;
    let entries = data.as_array().ok_or_else(|| CoreError::ModelUnavailable {
        reason: format!("远端响应的 data 不是数组，无法当作模型列表：{excerpt}"),
    })?;

    let mut models = Vec::new();
    for entry in entries {
        // 一个没有 id 的条目给不出任何可用信息：跳过它，而不是让整份列表失败。
        let Some(id) = entry.get("id").and_then(serde_json::Value::as_str) else {
            continue;
        };
        let id = id.trim();
        if id.is_empty() {
            continue;
        }
        let owned_by = entry
            .get("owned_by")
            .and_then(serde_json::Value::as_str)
            .map(|owner| redact(owner, Some(secret)));
        models.push(ProviderModelInfo {
            id: redact(id, Some(secret)),
            owned_by,
        });
    }

    // 有条目但一个带 id 的都没有：这不是「服务没有模型」，是响应形状对不上。
    if models.is_empty() && !entries.is_empty() {
        return Err(CoreError::ModelUnavailable {
            reason: format!("远端返回的 data 里没有一个条目带非空 id，无法当作模型列表：{excerpt}"),
        });
    }
    Ok(models)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ProviderCapability, ProviderTestStatus};
    use crate::provider::local_engine::SHERPA_ONNX;
    use crate::provider::transport::{
        PanicTransport, ScriptedTransport, Transport, TransportError,
    };
    use std::sync::Arc;

    const SECRET: &str = "sk-test-secret-0123456789";

    /// 一个已保存的 OpenAI 兼容服务 + 回放传输层。脚本按顺序回放，调用次数可数。
    fn core_with_provider(
        endpoint: Option<&str>,
        secret: Option<&str>,
        script: Vec<std::result::Result<HttpResponse, TransportError>>,
    ) -> (Core, Arc<ScriptedTransport>) {
        let transport = Arc::new(ScriptedTransport::new(script));
        let mut core = Core::open_in_memory().unwrap();
        core.set_transport_for_test(transport.clone() as Arc<dyn Transport>);
        core.save_provider(
            &Provider {
                id: "prv_models".to_owned(),
                label: "模型列表测试服务".to_owned(),
                adapter_id: OPENAI_COMPATIBLE.to_owned(),
                endpoint: endpoint.map(str::to_owned),
                model: Some("qwen-test".to_owned()),
                capabilities: [ProviderCapability::GenerateText].into_iter().collect(),
                credential_ref: None,
                enabled: true,
                test_status: ProviderTestStatus::Untested,
            },
            secret,
        )
        .unwrap();
        (core, transport)
    }

    fn code_of(error: &CoreError) -> &'static str {
        error.code().wire()
    }

    #[test]
    fn a_normal_openai_list_becomes_two_models() {
        let (core, transport) = core_with_provider(
            Some("https://api.deepseek.com/"),
            Some(SECRET),
            vec![Ok(HttpResponse::new(
                200,
                r#"{"object":"list","data":[{"id":"a"},{"id":"b"}]}"#,
            ))],
        );

        let models = core.list_provider_models("prv_models").unwrap();
        assert_eq!(
            models,
            vec![
                ProviderModelInfo {
                    id: "a".to_owned(),
                    owned_by: None,
                },
                ProviderModelInfo {
                    id: "b".to_owned(),
                    owned_by: None,
                },
            ]
        );
        assert_eq!(transport.call_count(), 1, "一次检测只发一次请求");
        // endpoint 末尾的 `/` 不会拼成双斜杠；方法是 GET。
        assert_eq!(
            transport.url_at(0).as_deref(),
            Some("https://api.deepseek.com/models")
        );
        let request = &transport.requests()[0];
        assert_eq!(request.method, "GET");
        let expected_auth = format!("Bearer {SECRET}");
        assert_eq!(
            request.get_header("Authorization"),
            Some(expected_auth.as_str())
        );
    }

    #[test]
    fn optional_fields_are_tolerated() {
        let (core, _transport) = core_with_provider(
            Some("http://127.0.0.1:11434/v1"),
            Some(SECRET),
            vec![Ok(HttpResponse::new(
                200,
                r#"{"data":[{"id":"a","owned_by":"owner-a"},{"id":"b"},{"no_id":"x"}]}"#,
            ))],
        );

        let models = core.list_provider_models("prv_models").unwrap();
        assert_eq!(models.len(), 2, "缺 id 的条目被跳过，不该让整份列表失败");
        assert_eq!(models[0].owned_by.as_deref(), Some("owner-a"));
        assert_eq!(models[1].id, "b");
        assert_eq!(models[1].owned_by, None);
    }

    #[test]
    fn empty_data_is_an_empty_list_not_an_error() {
        let (core, _transport) = core_with_provider(
            Some("https://api.deepseek.com"),
            Some(SECRET),
            vec![Ok(HttpResponse::new(200, r#"{"object":"list","data":[]}"#))],
        );
        assert!(core.list_provider_models("prv_models").unwrap().is_empty());
    }

    #[test]
    fn a_401_is_authentication_failed_and_never_leaks_the_secret() {
        let (core, transport) = core_with_provider(
            Some("https://api.deepseek.com"),
            Some(SECRET),
            vec![Ok(HttpResponse::new(
                401,
                format!(r#"{{"error":{{"message":"bad key {SECRET}"}}}}"#),
            ))],
        );

        let error = core.list_provider_models("prv_models").unwrap_err();
        assert_eq!(code_of(&error), "authentication_failed");
        let text = error.to_string();
        assert!(!text.contains(SECRET), "错误消息泄漏了密钥：{text}");
        assert!(text.contains("***"), "应该留下擦除痕迹：{text}");
        assert_eq!(transport.call_count(), 1);
    }

    #[test]
    fn a_timeout_is_provider_timeout() {
        let (core, _transport) = core_with_provider(
            Some("https://api.deepseek.com"),
            Some(SECRET),
            vec![Err(TransportError::Timeout {
                message: "timed out".to_owned(),
            })],
        );
        let error = core.list_provider_models("prv_models").unwrap_err();
        assert_eq!(code_of(&error), "provider_timeout");
    }

    #[test]
    fn a_connect_failure_is_network_unavailable() {
        let (core, _transport) = core_with_provider(
            Some("https://api.deepseek.com"),
            Some(SECRET),
            vec![Err(TransportError::Connect {
                message: "connection refused".to_owned(),
            })],
        );
        let error = core.list_provider_models("prv_models").unwrap_err();
        assert_eq!(code_of(&error), "network_unavailable");
    }

    #[test]
    fn a_429_is_rate_limited() {
        let (core, _transport) = core_with_provider(
            Some("https://api.deepseek.com"),
            Some(SECRET),
            vec![Ok(
                HttpResponse::new(429, "{}").with_header("Retry-After", "3")
            )],
        );
        let error = core.list_provider_models("prv_models").unwrap_err();
        assert_eq!(code_of(&error), "rate_limited");
    }

    #[test]
    fn a_response_without_data_is_a_readable_error() {
        let (core, _transport) = core_with_provider(
            Some("https://api.deepseek.com"),
            Some(SECRET),
            vec![Ok(HttpResponse::new(200, r#"{"object":"list"}"#))],
        );

        let error = core.list_provider_models("prv_models").unwrap_err();
        assert_eq!(code_of(&error), "model_unavailable");
        let text = error.to_string();
        assert!(text.contains("data"), "错误消息要说清缺什么：{text}");
        assert!(text.contains("endpoint"), "要给可执行的下一步：{text}");
        assert!(!text.contains(SECRET), "错误消息泄漏了密钥：{text}");
    }

    #[test]
    fn a_non_json_response_is_a_readable_error_without_the_secret() {
        let (core, _transport) = core_with_provider(
            Some("https://api.deepseek.com"),
            Some(SECRET),
            vec![Ok(HttpResponse::new(
                200,
                format!("<html>echo {SECRET}</html>"),
            ))],
        );

        let error = core.list_provider_models("prv_models").unwrap_err();
        assert_eq!(code_of(&error), "model_unavailable");
        let text = error.to_string();
        assert!(text.contains("JSON"), "{text}");
        assert!(!text.contains(SECRET), "错误消息泄漏了密钥：{text}");
    }

    #[test]
    fn an_adapter_without_model_listing_says_so_without_a_request() {
        let transport = Arc::new(ScriptedTransport::ok(vec![]));
        let mut core = Core::open_in_memory().unwrap();
        core.set_transport_for_test(transport.clone() as Arc<dyn Transport>);
        // 本机引擎：endpoint 必须留空（save 会拒绝给它配 http 地址）。
        let provider = core
            .save_provider(
                &Provider {
                    id: "prv_local".to_owned(),
                    label: "本机转写".to_owned(),
                    adapter_id: SHERPA_ONNX.to_owned(),
                    endpoint: None,
                    model: None,
                    capabilities: [ProviderCapability::TranscribeAudio].into_iter().collect(),
                    credential_ref: None,
                    enabled: true,
                    test_status: ProviderTestStatus::Untested,
                },
                None,
            )
            .unwrap();

        let error = core.list_provider_models(&provider.id).unwrap_err();
        assert_eq!(code_of(&error), "provider_not_configured");
        let text = error.to_string();
        assert!(text.contains("不支持列举模型"), "{text}");
        assert_eq!(transport.call_count(), 0, "适配器不支持时一个请求都不该发");
    }

    #[test]
    fn a_provider_without_endpoint_is_not_configured_before_any_request() {
        let (core, transport) = core_with_provider(None, Some(SECRET), vec![]);
        let error = core.list_provider_models("prv_models").unwrap_err();
        assert_eq!(code_of(&error), "provider_not_configured");
        let text = error.to_string();
        assert!(text.contains("endpoint"), "{text}");
        assert!(text.contains("下一步"), "{text}");
        assert_eq!(transport.call_count(), 0);
    }

    #[test]
    fn a_provider_without_credential_is_not_configured_before_any_request() {
        let (core, transport) = core_with_provider(Some("https://api.deepseek.com"), None, vec![]);
        let error = core.list_provider_models("prv_models").unwrap_err();
        assert_eq!(code_of(&error), "provider_not_configured");
        let text = error.to_string();
        assert!(text.contains("凭据"), "{text}");
        assert!(text.contains("下一步"), "{text}");
        assert_eq!(transport.call_count(), 0);
    }

    #[test]
    fn unknown_provider_is_not_found() {
        let core = Core::open_in_memory().unwrap();
        let error = core.list_provider_models("prv_nope").unwrap_err();
        assert_eq!(code_of(&error), "not_found");
    }

    /// 「默认不联网」在模型检测这一路上的证明：打开库、列配置、列预设都不碰
    /// `Transport::send`（内存库默认就是 `PanicTransport`，一碰就炸）。
    #[test]
    fn listing_providers_and_presets_never_calls_the_transport() {
        let mut core = Core::open_in_memory().unwrap();
        // 显式换成一个真的会 panic 的替身，语义比默认值更清楚。
        core.set_transport_for_test(Arc::new(PanicTransport::new()) as Arc<dyn Transport>);
        core.save_provider(
            &Provider {
                id: "prv_quiet".to_owned(),
                label: "安静的服务".to_owned(),
                adapter_id: OPENAI_COMPATIBLE.to_owned(),
                endpoint: Some("https://api.deepseek.com".to_owned()),
                model: Some("deepseek-chat".to_owned()),
                capabilities: [ProviderCapability::GenerateText].into_iter().collect(),
                credential_ref: None,
                enabled: true,
                test_status: ProviderTestStatus::Untested,
            },
            Some(SECRET),
        )
        .unwrap();

        // 这几步都不该出网：真出网了 PanicTransport 会当场炸。
        assert_eq!(core.list_providers().unwrap().len(), 1);
        assert!(core.list_provider_presets().len() >= 3);
    }
}

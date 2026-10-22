//! 错误归一化：把传输层与各家的 HTTP 响应映射成契约第 7 节的错误码，
//! 并标注「可重试 / 等配置 / 永久」——作业系统的退避语义照这张表走。
//!
//! # 映射表（可审计，测试逐条钉住）
//!
//! | 情况 | 契约错误码 | 语义 | 作业侧 |
//! |---|---|---|---|
//! | 没配置/没启用/适配器没实现/未通过验证 | `provider_not_configured` | 等配置 | `waiting_configuration` |
//! | 401、403（非额度） | `authentication_failed` | 等配置 | `waiting_configuration` |
//! | 402、403+额度字样、响应里有 `insufficient_quota` | `quota_exceeded` | 暂停 | `waiting_configuration` |
//! | 429 | `rate_limited` | 有界重试（尊重 Retry-After） | `retry_wait` |
//! | 408、连接/读写超时 | `provider_timeout` | 有界重试 | `retry_wait` |
//! | 断网、连不上、TLS 失败、5xx | `network_unavailable` | 有界退避重试 | `retry_wait` |
//! | 404、其它 4xx、响应解析不了、3xx（不跟随重定向） | `model_unavailable` | 永久（等人工） | `failed` + `requires_user_action` |
//!
//! 为什么 5xx 归到 `network_unavailable`：契约第 7 节没有单独的「服务端错误」码，
//! 而它对用户的意思是同一件事——**这份材料还在、处理等远端恢复**，自动行为都是
//! 「有退避地排队」。为什么「响应不可解析」归到 `model_unavailable`：它在 v1 的码里
//! 是唯一「配的这个模型/端点跟实际说话的方式对不上，去检查配置」的一类，用户动作
//! （检查模型与适配器）是对的。**没有新增错误码**：新增码等于改契约与 Dart 枚举。
//!
//! # 不泄漏密钥
//!
//! 错误消息里可能带上响应体摘录（对排查有用），所以每条消息都过一遍 `redact`：
//! 把这次调用用的密钥明文替换成 `***`。密钥也不会出现在 `Debug` 里（见
//! `transport::HttpRequest` 的手写 `Debug`）。

use crate::error::CoreError;

use super::transport::{HttpResponse, TransportError};

/// 错误消息里出现的密钥明文一律擦掉。
///
/// 短于 8 个字符的「密钥」不擦（那种长度擦起来会把正常文字也擦掉，而它本来也
/// 不算密钥）；真实密钥都远长于此。
pub fn redact(text: &str, secret: Option<&str>) -> String {
    match secret {
        Some(secret) if secret.len() >= 8 => text.replace(secret, "***"),
        _ => text.to_owned(),
    }
}

/// 传输层失败 → 契约错误码。
pub fn transport_error(error: &TransportError, secret: Option<&str>) -> CoreError {
    let message = redact(&error.to_string(), secret);
    match error {
        TransportError::Timeout { .. } => CoreError::ProviderTimeout { reason: message },
        TransportError::Connect { .. } | TransportError::Other { .. } => {
            CoreError::NetworkUnavailable { reason: message }
        }
    }
}

/// 有响应的失败 → 契约错误码。2xx 之外都应该走这里。
pub fn http_error(response: &HttpResponse, secret: Option<&str>) -> CoreError {
    let status = response.status;
    let excerpt = response.excerpt(secret);
    let retry_after = parse_retry_after(response.header("retry-after"));

    match status {
        401 => CoreError::AuthenticationFailed {
            reason: format!("远端返回 401：密钥无效、过期或没有权限（响应：{excerpt}）"),
        },
        403 => {
            let lowered = response.body_text().to_ascii_lowercase();
            if lowered.contains("quota") || lowered.contains("billing") || lowered.contains("额度")
            {
                CoreError::QuotaExceeded {
                    reason: format!("远端返回 403 且提到额度：{excerpt}"),
                }
            } else {
                CoreError::AuthenticationFailed {
                    reason: format!("远端返回 403：密钥没有这个模型的权限（响应：{excerpt}）"),
                }
            }
        }
        402 => CoreError::QuotaExceeded {
            reason: format!("远端返回 402：需要付费或额度已用尽（响应：{excerpt}）"),
        },
        408 => CoreError::ProviderTimeout {
            reason: format!("远端返回 408：{excerpt}"),
        },
        429 => CoreError::RateLimited {
            reason: format!("远端返回 429：被限流（响应：{excerpt}）"),
            retry_after_ms: retry_after,
        },
        status if (500..600).contains(&status) => CoreError::NetworkUnavailable {
            reason: format!("远端返回 {status}：服务暂时不可用（响应：{excerpt}）"),
        },
        status if (300..400).contains(&status) => CoreError::ModelUnavailable {
            reason: format!(
                "端点返回 {status} 重定向；核心不跟随重定向（跟随会把凭据发到别的主机）——\
                 请把 endpoint 直接配成最终地址"
            ),
        },
        404 => CoreError::ModelUnavailable {
            reason: format!(
                "远端返回 404：端点或模型不存在（响应：{excerpt}）；\
                 检查 endpoint 是否含正确的 API 版本前缀、model 名是否正确"
            ),
        },
        _ => CoreError::ModelUnavailable {
            reason: format!("远端返回 {status}，请求被拒绝（响应：{excerpt}）"),
        },
    }
}

/// 解析 `Retry-After`：只认「秒数」这一种写法；HTTP 日期形式忽略（按本地退避）。
///
/// 上限夹到 1 小时：远端说等一天也不该把任务钉在那里，用户随时可以手动重试。
pub fn parse_retry_after(value: Option<&str>) -> Option<u64> {
    let seconds: u64 = value?.trim().parse().ok()?;
    Some(seconds.saturating_mul(1000).min(60 * 60 * 1000))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn code(error: &CoreError) -> &'static str {
        error.code().wire()
    }

    /// 归一化映射表：每条错误码 → 可重试 / 等配置 / 永久。
    #[test]
    fn mapping_table_is_pinned() {
        let cases: Vec<(CoreError, &str, bool, bool)> = vec![
            (
                CoreError::ProviderNotConfigured {
                    reason: "没配".to_owned(),
                },
                "provider_not_configured",
                false,
                true,
            ),
            (
                CoreError::CapabilityUnsupported {
                    reason: "适配器没实现".to_owned(),
                },
                "provider_not_configured",
                false,
                true,
            ),
            (
                CoreError::AuthenticationFailed {
                    reason: "401".to_owned(),
                },
                "authentication_failed",
                false,
                true,
            ),
            (
                CoreError::QuotaExceeded {
                    reason: "402".to_owned(),
                },
                "quota_exceeded",
                false,
                true,
            ),
            (
                CoreError::RateLimited {
                    reason: "429".to_owned(),
                    retry_after_ms: Some(1000),
                },
                "rate_limited",
                true,
                false,
            ),
            (
                CoreError::ProviderTimeout {
                    reason: "超时".to_owned(),
                },
                "provider_timeout",
                true,
                false,
            ),
            (
                CoreError::NetworkUnavailable {
                    reason: "断网".to_owned(),
                },
                "network_unavailable",
                true,
                false,
            ),
            (
                CoreError::ModelUnavailable {
                    reason: "404".to_owned(),
                },
                "model_unavailable",
                false,
                false,
            ),
        ];
        for (error, wire, retryable, waits) in cases {
            assert_eq!(code(&error), wire, "错误码不对：{error}");
            assert_eq!(error.retryable(), retryable, "retryable 不对：{error}");
            assert_eq!(
                error.waits_for_configuration(),
                waits,
                "「等配置」标注不对：{error}"
            );
            // 「等配置」与「可重试」互斥：一个错误不能既说等用户改配置又说等一等就好。
            assert!(
                !(error.retryable() && error.waits_for_configuration()),
                "两种时间语义同时成立：{error}"
            );
        }
    }

    #[test]
    fn http_statuses_land_on_expected_codes() {
        let cases = [
            (401, "authentication_failed"),
            (403, "authentication_failed"),
            (402, "quota_exceeded"),
            (408, "provider_timeout"),
            (429, "rate_limited"),
            (500, "network_unavailable"),
            (503, "network_unavailable"),
            (302, "model_unavailable"),
            (404, "model_unavailable"),
            (400, "model_unavailable"),
            (413, "model_unavailable"),
        ];
        for (status, expected) in cases {
            let response = HttpResponse::new(status, "{}");
            assert_eq!(
                code(&http_error(&response, None)),
                expected,
                "HTTP {status} 归错了"
            );
        }
    }

    #[test]
    fn quota_like_403_is_quota_not_auth() {
        let response = HttpResponse::new(403, "{\"error\":{\"code\":\"insufficient_quota\"}}");
        assert_eq!(code(&http_error(&response, None)), "quota_exceeded");
    }

    #[test]
    fn retry_after_is_honoured_and_capped() {
        let response = HttpResponse::new(429, "{}").with_header("Retry-After", "12");
        match http_error(&response, None) {
            CoreError::RateLimited { retry_after_ms, .. } => {
                assert_eq!(retry_after_ms, Some(12_000));
            }
            other => panic!("429 应该归到 rate_limited，实际 {other}"),
        }
        // HTTP 日期形式不认：按本地退避。
        assert_eq!(parse_retry_after(Some("Wed, 21 Oct 2026 07:28:00 GMT")), None);
        // 上限一小时。
        assert_eq!(parse_retry_after(Some("999999")), Some(3_600_000));
    }

    #[test]
    fn secrets_never_appear_in_error_messages() {
        let secret = "sk-verysecret-0123456789";
        let response = HttpResponse::new(401, format!("{{\"key\":\"{secret}\"}}"));
        let error = http_error(&response, Some(secret));
        let text = error.to_string();
        assert!(!text.contains(secret), "错误消息泄漏了密钥：{text}");
        assert!(text.contains("***"), "应该留下擦除痕迹：{text}");
    }

    #[test]
    fn a_secret_longer_than_the_excerpt_is_redacted_before_truncation() {
        // 300 个字符的密钥：比 200 字的摘录上限还长。先截断再擦的话，完整的密钥
        // 永远匹配不上，前缀会原样留在错误消息里。
        let secret = format!("sk-{}", "A".repeat(297));
        assert!(secret.len() > 200, "这条用例要的是「密钥比摘录还长」");
        let prefix: String = secret.chars().take(20).collect();
        let response = HttpResponse::new(400, format!("{{\"echo\":\"{secret}\"}}"));
        let error = http_error(&response, Some(&secret));
        let text = error.to_string();
        assert!(
            !text.contains(&prefix),
            "超长密钥的前 20 个字符留在了摘录里：{text}"
        );
        assert!(text.contains("***"), "应该留下擦除痕迹：{text}");
        // 摘录仍受 200 字上限约束（擦除发生在截断之前）。
        assert!(response.excerpt(Some(&secret)).chars().count() <= 201);
    }

    #[test]
    fn transport_errors_split_timeout_from_connect() {
        assert_eq!(
            code(&transport_error(
                &TransportError::Timeout {
                    message: "timed out".to_owned()
                },
                None
            )),
            "provider_timeout"
        );
        assert_eq!(
            code(&transport_error(
                &TransportError::Connect {
                    message: "dns".to_owned()
                },
                None
            )),
            "network_unavailable"
        );
    }
}
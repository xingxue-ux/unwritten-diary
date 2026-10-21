//! 内置服务预设：只告诉用户「这一家怎么填」，**不含任何密钥**，也不自动写库。
//!
//! # 预设是什么、不是什么
//!
//! [`ProviderPreset`] 是一张**填表草稿**：endpoint / model / 能力集合 / 文档链接，
//! 外加一句「这条预设有没有被真的调用验证过」（[`ProviderPreset::verified`]）。
//!
//! - **不是凭据**：结构体里没有密钥字段，预设也不会替你存密钥。保存配置时明文仍然只在
//!   `Core::save_provider(config, secret)` 的那一瞬间经过内存（见 `provider::secret`）；
//! - **不是「一键启用」**：[`presets`] 只返回静态数据，不写数据库；
//!   [`ProviderPreset::to_provider`] 只造一个草稿对象，要不要保存、要不要启用、要不要
//!   先测一次，都是用户的事；
//! - **不是推荐列表**：内置的是「有公开文档、格式确定」的几家。没有真实实例联调证据的
//!   服务如实标 `verified = false`，不把「看起来能通」说成「已经能通」。
//!
//! # `verified` 的含义（只认实测）
//!
//! `verified = true` 表示**有人用真实密钥对真实端点跑通过**这条预设里的 endpoint、
//! model 与能力（最少是 `generate_text`）——不是「协议上应该可以」。
//! `verified = false` 表示没有真实实例的联调证据：可能是只验过请求构造（单测钉住了
//! URL 与鉴权头），也可能只是照文档填的。两者都不等于「不能用」，只等于「还没人证明
//! 能用」；用户仍然可以保存并跑一次连接测试（那是会出网的、用户显式发起的动作之一）。
//!
//! # 默认不联网
//!
//! 这一片只返回静态数据：`presets()` 与 `Core::list_provider_presets()` 一个字节都不
//! 出网，也不读 `SecretStore`。联网只发生在用户显式发起的 `providers.test`、
//! `Core::list_provider_models` 与真被领取执行的 Provider 作业。

use serde::Serialize;

use crate::model::{Provider, ProviderCapability, ProviderTestStatus};

use super::adapter::OPENAI_COMPATIBLE;

/// 一个内置服务预设：只提供「怎么填」，不含任何密钥。
///
/// 字段名照核心 serde 原样（没有 `rename_all`）：`adapter_id` / `docs_url` 都是
/// snake_case，C ABI 的 `providers.presets` 直接序列化这份结构，不另造 DTO。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ProviderPreset {
    /// 稳定的预设标识（**不是** `providers.id`：保存后由核心另发一个 `prv_` 开头的 id）。
    pub id: &'static str,
    /// 给用户看的名字。
    pub label: &'static str,
    /// 用哪种协议适配器（内置的 HTTP 适配器当前只有 `openai_compatible`）。
    pub adapter_id: &'static str,
    /// 预填的 endpoint；空串表示「这一项要用户自己填」。
    pub endpoint: &'static str,
    /// 预填的模型名；空串表示「这一项要用户自己填」。
    pub model: &'static str,
    /// 预填的能力集合。**只列适配器真的实现了的**；用不上的应由用户删掉。
    pub capabilities: &'static [ProviderCapability],
    /// 服务商文档（给用户核对 endpoint / 模型 / 协议用）。
    pub docs_url: &'static str,
    /// 这条预设有没有被真的调用验证过。**未验证的必须如实标注**。
    pub verified: bool,
    /// 给用户看的补充说明（例如「deepseek-chat 会被路由到 deepseek-flash」）。
    pub note: &'static str,
}

impl ProviderPreset {
    /// 把预设变成一张**填好的配置草稿**：不写库、不含密钥。
    ///
    /// 三个刻意的选择：
    ///
    /// - `id` 留空：预设不是一条已经存在的配置，保存入口会生成新的 `prv_` id；
    ///   同一个预设保存两次就是两条独立配置；
    /// - `credential_ref` 为 `None`：密钥要由保存入口单独给
    ///   （`Core::save_provider(config, secret)`），预设对象里永远没有它；
    /// - `enabled = true` / `test_status = Untested`：与用户手填一条新配置的默认一致。
    ///   能不能真的排任务仍由能力闸门决定（适配器实现 + 配置声明 + 验过没有，
    ///   见 `provider::capability_gate_for`），预设本身不绕过任何一条。
    pub fn to_provider(&self) -> Provider {
        Provider {
            id: String::new(),
            label: self.label.to_owned(),
            adapter_id: self.adapter_id.to_owned(),
            endpoint: non_empty(self.endpoint),
            model: non_empty(self.model),
            capabilities: self.capabilities.iter().copied().collect(),
            credential_ref: None,
            enabled: true,
            test_status: ProviderTestStatus::Untested,
        }
    }
}

/// 空串与纯空白都算「这一项没填」，不落成 `Some("")`——`providers.save` 对空
/// endpoint / 空 model 的处理本来就是「没配」，这里保持同一个形状。
fn non_empty(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_owned())
    }
}

// 能力集合用 `const` 而不是就地写进结构体：`&'static [ProviderCapability]` 需要
// 一个静态生命周期，而且三条预设里有重复（都用 `GenerateText`）。
const TEXT_ONLY: &[ProviderCapability] = &[ProviderCapability::GenerateText];
const TEXT_AND_AUDIO: &[ProviderCapability] = &[
    ProviderCapability::GenerateText,
    ProviderCapability::TranscribeAudio,
];

/// 内置预设表。顺序就是界面上的推荐顺序（已验证的在前，通用/本机在后）。
static PRESETS: &[ProviderPreset] = &[
    ProviderPreset {
        id: "deepseek",
        label: "DeepSeek",
        adapter_id: OPENAI_COMPATIBLE,
        // 2026-10 用真实密钥实测：`GET https://api.deepseek.com/models` 返回 200 与
        // `deepseek-flash` / `deepseek-v4-pro`；`POST /chat/completions` 可用。
        endpoint: "https://api.deepseek.com",
        // `deepseek-chat` 是兼容别名，远端会路由到 `deepseek-flash`（响应里的 model
        // 字段也是它）。预设仍用它当入口：这是最稳的兼容名。
        model: "deepseek-chat",
        capabilities: TEXT_ONLY,
        docs_url: "https://api-docs.deepseek.com/",
        verified: true,
        note: "已用真实密钥实测：GET /models 返回 deepseek-flash 与 deepseek-v4-pro，\
               POST /chat/completions 可用。deepseek-chat 是兼容别名，会被路由到 \
               deepseek-flash（响应的 model 字段是 deepseek-flash）；也可以把 model \
               换成 deepseek-v4-pro。endpoint 只填到服务根地址，适配器自己拼 \
               /chat/completions 与 /models。",
    },
    ProviderPreset {
        id: "openai_compatible",
        label: "OpenAI 兼容端点",
        adapter_id: OPENAI_COMPATIBLE,
        // 通用的「你自己有服务」：endpoint 与 model 都没有可预填的事实，留空。
        endpoint: "",
        model: "",
        // 适配器实现了两项，就预填两项：用不上的（例如服务没有音频接口）由用户删掉；
        // 图像理解内置适配器没有实现，不列进来假装能选。
        capabilities: TEXT_AND_AUDIO,
        docs_url: "https://platform.openai.com/docs/api-reference",
        verified: false,
        note: "通用兼容端点：endpoint（含 /v1 这类版本前缀）与 model 都要自己填，没有\
               任何具体服务被实测过。能力按适配器实现了的两项预填，用不上的请删掉——\
               图像理解内置适配器没有实现，声明了也不会被排任务。",
    },
    ProviderPreset {
        id: "ollama_local",
        label: "Ollama（本机）",
        adapter_id: OPENAI_COMPATIBLE,
        endpoint: "http://127.0.0.1:11434/v1",
        // Ollama 的模型名取决于本机 `ollama list`，不预填一个不存在于用户机器上的名字。
        model: "",
        capabilities: TEXT_ONLY,
        docs_url: "https://github.com/ollama/ollama",
        verified: false,
        note: "请求构造已验证（单测钉住 http://127.0.0.1:11434/v1/chat/completions 与 \
               Authorization 头），**未对真实 Ollama 实例联调**。model 请填 `ollama list` \
               里的名字（例如 qwen2.5:7b）。本机（loopback）服务不校验密钥：凭据可以留空，\
               核心不会因为「没有凭据」拒绝检测或试连；服务真返回 401 / 超时仍会如实报错。\
               私有网段（10./172.16-31./192.168.）不算本机，仍要填凭据。",
    },
];

/// 全部内置预设。**只读、静态、不出网**。
pub fn presets() -> &'static [ProviderPreset] {
    PRESETS
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::adapter::adapter_for;
    use std::collections::HashSet;

    #[test]
    fn presets_are_well_formed() {
        let all = presets();
        assert!(all.len() >= 3, "至少要有三条预设，实际 {}", all.len());

        let mut ids: HashSet<&str> = HashSet::new();
        for preset in all {
            assert!(ids.insert(preset.id), "预设 id 重复：{}", preset.id);
            assert!(!preset.id.trim().is_empty(), "预设 id 不能为空");
            assert!(!preset.label.trim().is_empty(), "{} 缺 label", preset.id);
            assert!(
                preset.docs_url.starts_with("https://"),
                "{} 的 docs_url 要是 https 链接",
                preset.id
            );
            assert!(
                !preset.capabilities.is_empty(),
                "{} 至少要预填一项能力",
                preset.id
            );

            // 预设声明的适配器必须真的存在，且真的实现了它列出的每一项能力。
            let adapter = adapter_for(preset.adapter_id).unwrap_or_else(|| {
                panic!("{} 用了不存在的适配器 {}", preset.id, preset.adapter_id)
            });
            for capability in preset.capabilities {
                assert!(
                    adapter.capabilities().contains(capability),
                    "{} 声明了适配器 {} 没实现的 {}",
                    preset.id,
                    preset.adapter_id,
                    capability.wire()
                );
            }
        }
    }

    #[test]
    fn deepseek_preset_matches_what_was_actually_verified() {
        let deepseek = presets()
            .iter()
            .find(|preset| preset.id == "deepseek")
            .expect("要有 deepseek 预设");

        assert_eq!(deepseek.label, "DeepSeek");
        assert_eq!(deepseek.adapter_id, OPENAI_COMPATIBLE);
        assert_eq!(deepseek.endpoint, "https://api.deepseek.com");
        assert_eq!(deepseek.model, "deepseek-chat");
        assert_eq!(deepseek.capabilities, TEXT_ONLY);
        assert!(
            deepseek.verified,
            "DeepSeek 是实测过的，必须如实标 verified"
        );
        // 实测结论要写进 note，别让用户以为 deepseek-chat 就是最终模型名。
        assert!(deepseek.note.contains("deepseek-flash"));
        assert!(deepseek.note.contains("deepseek-v4-pro"));
        assert!(deepseek.note.contains("/models"));
    }

    #[test]
    fn presets_without_a_real_integration_are_not_marked_verified() {
        for preset in presets() {
            if preset.id == "deepseek" {
                continue;
            }
            assert!(
                !preset.verified,
                "{} 没有真实实例联调证据，不能标 verified",
                preset.id
            );
        }

        let ollama = presets()
            .iter()
            .find(|preset| preset.id == "ollama_local")
            .expect("要有 ollama_local 预设");
        assert_eq!(ollama.endpoint, "http://127.0.0.1:11434/v1");
        assert_eq!(ollama.capabilities, TEXT_ONLY);
        // 只验过请求构造，就得说清「只验过请求构造」。
        assert!(ollama.note.contains("请求构造已验证"));
        assert!(ollama.note.contains("未对真实 Ollama 实例联调"));

        let generic = presets()
            .iter()
            .find(|preset| preset.id == "openai_compatible")
            .expect("要有 openai_compatible 预设");
        assert!(generic.endpoint.is_empty(), "通用端点不该预填地址");
        assert!(generic.model.is_empty(), "通用端点不该预填模型");
        assert!(generic.note.contains("没有") && generic.note.contains("实测"));
    }

    #[test]
    fn to_provider_is_only_a_draft_and_carries_no_secret() {
        let deepseek = presets()
            .iter()
            .find(|preset| preset.id == "deepseek")
            .unwrap();
        let draft = deepseek.to_provider();

        assert!(draft.id.is_empty(), "草稿不该自带 id，保存时由核心发");
        assert_eq!(draft.label, "DeepSeek");
        assert_eq!(draft.adapter_id, OPENAI_COMPATIBLE);
        assert_eq!(draft.endpoint.as_deref(), Some("https://api.deepseek.com"));
        assert_eq!(draft.model.as_deref(), Some("deepseek-chat"));
        assert_eq!(draft.credential_ref, None);
        assert!(draft.enabled);
        assert_eq!(draft.test_status, ProviderTestStatus::Untested);
        assert_eq!(
            draft.capabilities,
            TEXT_ONLY.iter().copied().collect::<HashSet<_>>()
        );

        // 空串预填 → None（不是 Some("")），与 `providers.save` 的「没配」同形。
        let generic = presets()
            .iter()
            .find(|preset| preset.id == "openai_compatible")
            .unwrap();
        let draft = generic.to_provider();
        assert_eq!(draft.endpoint, None);
        assert_eq!(draft.model, None);

        // 预设结构里根本没有密钥字段；把整个表 Debug 出来也不该出现密钥形状的东西。
        let debug = format!("{:?}", presets());
        assert!(!debug.to_ascii_lowercase().contains("sk-"), "{debug}");
    }

    /// `presets()` 不读库、不出网：内存库的默认传输层是 `PanicTransport`，
    /// 这条用例只要能跑完就说明没有偷偷发请求。
    #[test]
    fn listing_presets_touches_neither_the_database_nor_the_network() {
        let core = crate::Core::open_in_memory().unwrap();
        let listed = core.list_provider_presets();
        assert_eq!(listed.len(), presets().len());
        assert!(core.list_providers().unwrap().is_empty());
    }
}

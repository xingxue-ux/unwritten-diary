//! 真实端点联调（**默认不跑**）：`GET https://api.deepseek.com/models`。
//!
//! 这个文件是唯一会碰真实网络的测试之一（另一个是 `providers.test` 的人工联调）。
//! 它被 `#[ignore]` 挡在默认测试之外，CI 不跑；没有环境变量时也会自己跳过。
//!
//! # 密钥纪律
//!
//! 密钥**只从环境变量 `DEEPSEEK_KEY` 读**，绝不写进这个文件、日志或断言消息。
//! 断言里只出现「返回值不含密钥明文」这一条（把实际值读出来比对，但不打印它）。
//!
//! # 怎么跑
//!
//! ```bash
//! DEEPSEEK_KEY=... cargo test -p diary_core --test provider_models_live_test \
//!   -- --ignored --nocapture
//! ```
//!
//! 断言：`/models` 至少返回 `deepseek-flash` 与 `deepseek-v4-pro`（2026-10 用真实
//! 密钥实测过的事实，见 `provider::presets` 的 DeepSeek 预设 note）。

use std::sync::Arc;

use diary_core::{
    Core, Provider, ProviderCapability, ProviderTestStatus, Transport, UreqTransport,
};

const KEY_ENV: &str = "DEEPSEEK_KEY";
const ENDPOINT: &str = "https://api.deepseek.com";

#[test]
#[ignore = "真实网络联调：需要 DEEPSEEK_KEY 与出网，CI 不跑（默认不执行）"]
fn deepseek_lists_its_models() {
    let Ok(key) = std::env::var(KEY_ENV) else {
        eprintln!("没有 {KEY_ENV} 环境变量，跳过真实联调（这是默认行为）");
        return;
    };
    let key = key.trim().to_owned();
    if key.is_empty() {
        eprintln!("{KEY_ENV} 是空白，跳过真实联调（这是默认行为）");
        return;
    }

    // 内存库默认是 PanicTransport；这里**显式**换成真实 HTTP 传输层——出网是这次
    // 调用的目的，而不是某条隐含路径。
    let mut core = Core::open_in_memory().unwrap();
    core.set_transport_for_test(Arc::new(UreqTransport::new()) as Arc<dyn Transport>);

    let provider = core
        .save_provider(
            &Provider {
                id: "prv_live_deepseek".to_owned(),
                label: "DeepSeek（真实联调）".to_owned(),
                adapter_id: diary_core::OPENAI_COMPATIBLE.to_owned(),
                endpoint: Some(ENDPOINT.to_owned()),
                model: Some("deepseek-chat".to_owned()),
                capabilities: [ProviderCapability::GenerateText].into_iter().collect(),
                credential_ref: None,
                enabled: true,
                test_status: ProviderTestStatus::Untested,
            },
            Some(&key),
        )
        .unwrap();

    let models = core.list_provider_models(&provider.id).unwrap();
    let names: Vec<&str> = models.iter().map(|model| model.id.as_str()).collect();
    println!(
        "GET {ENDPOINT}/models 返回 {} 个模型：{names:?}",
        names.len()
    );

    assert!(
        names.contains(&"deepseek-flash"),
        "真实端点没有返回 deepseek-flash：{names:?}"
    );
    assert!(
        names.contains(&"deepseek-v4-pro"),
        "真实端点没有返回 deepseek-v4-pro：{names:?}"
    );

    // 返回值里不许出现密钥明文（不打印密钥本身）。
    let rendered = format!("{models:?}");
    assert!(!rendered.contains(&key), "返回值里出现了密钥明文");
}

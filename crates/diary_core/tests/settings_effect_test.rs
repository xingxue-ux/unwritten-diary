//! 「用户改了设置，**下一次流程真的读到了吗**」——写入路径与读取路径是否同一个字段。
//!
//! 这几轮在真机上出现的问题都属这一类：保存成功、界面也显示成功，但实际流程用的是
//! 另一个字段（或根本没读）。单测各自都绿，因为没人跨过「写 → 读」这条缝。
//!
//! 这里逐个钉住：**写进去的值必须能在消费端看到。**

use std::sync::Arc;

use diary_core::{
    Core, DiaryGenerationReason, HttpResponse, Provider, ProviderCapability,
    ProviderTestStatus, ScriptedTransport, StyleProfile, Transport,
};

fn new_core(dir: &std::path::Path) -> Core {
    let mut core = Core::open(dir.join("library.sqlite")).unwrap();
    core.set_transport_for_test(Arc::new(diary_core::PanicTransport::new()) as Arc<dyn Transport>);
    core
}

fn capture(core: &mut Core, text: &str, op: &str) -> String {
    let draft = core
        .create_draft(diary_core::CreateDraftInput {
            occurred_at: Some(
                chrono::DateTime::parse_from_rfc3339("2026-11-06T02:00:00Z")
                    .unwrap()
                    .with_timezone(&chrono::Utc),
            ),
            time_zone: "Asia/Shanghai",
            utc_offset_minutes: 480,
            operation_id: &format!("{op}-create"),
        })
        .unwrap();
    core.save_draft(&draft.id, text, draft.revision, op).unwrap();
    core.commit(&draft.id, draft.revision + 1, &format!("{op}-commit"))
        .unwrap();
    draft.id
}

fn provider() -> Provider {
    Provider {
        id: "prv_a".to_owned(),
        label: "测试服务".to_owned(),
        adapter_id: diary_core::OPENAI_COMPATIBLE.to_owned(),
        endpoint: Some("https://api.deepseek.com".to_owned()),
        model: Some("deepseek-flash".to_owned()),
        capabilities: [ProviderCapability::GenerateText].into_iter().collect(),
        credential_ref: None,
        enabled: true,
        test_status: ProviderTestStatus::Untested,
    }
}

fn chat_ok() -> HttpResponse {
    HttpResponse::new(
        200,
        serde_json::json!({
            "model": "deepseek-flash",
            "choices": [{"message": {"role": "assistant", "content": "正文。"}}],
        })
        .to_string(),
    )
}

fn style(label: &str, length: &str, tone: &str) -> StyleProfile {
    StyleProfile {
        id: format!("sty_{label}"),
        label: label.to_owned(),
        revision: 0,
        updated_at: chrono::Utc::now(),
        length_preference: Some(length.to_owned()),
        person: Some("第一人称".to_owned()),
        tone: Some(tone.to_owned()),
        emotional_temperature: Some("温和".to_owned()),
        rhetoric_level: Some("少润色，尽量用原话".to_owned()),
        examples: Vec::new(),
        banned_expressions: vec!["治愈".to_owned()],
    }
}

/// 用户保存风格 → 设为默认 → 生成。**默认风格的内容必须真的进 system 提示词。**
#[test]
fn the_default_style_reaches_the_generation_prompt() {
    let dir = tempfile::tempdir().unwrap();
    let transport = Arc::new(ScriptedTransport::ok(vec![chat_ok()]).repeating());
    let mut core = new_core(dir.path());
    core.set_transport_for_test(transport.clone() as Arc<dyn Transport>);
    core.save_provider(&provider(), Some("sk-test-0123456789"))
        .unwrap();
    core.test_provider("prv_a", &[ProviderCapability::GenerateText])
        .unwrap();

    // ① 保存风格并设为默认（界面「保存」现在会这么做）。
    let saved = core.save_style(&style("跟随原文", "跟随原文", "冷静记录"), None).unwrap();
    let settings = core.get_settings().unwrap();
    let mut next = settings.clone();
    next.default_style_id = Some(saved.id.clone());
    core.update_settings(&next, settings.revision).unwrap();
    assert_eq!(
        core.get_settings().unwrap().default_style_id.as_deref(),
        Some(saved.id.as_str()),
        "默认风格要真的写进设置"
    );

    // ② 生成（**不传 styleId**，走默认）。
    capture(&mut core, "今天很累。", "op-style");
    core.generate_diary("2026-11-06", None, DiaryGenerationReason::Manual, "gen-style")
        .unwrap();
    core.run_due_jobs(chrono::Utc::now(), 30_000, "test", 16)
        .unwrap();

    // ③ 发出去的请求里必须带着这套风格。
    let requests = transport.requests();
    let chat = requests
        .iter()
        .rfind(|request| request.url.contains("chat/completions"))
        .expect("要发一次生成请求");
    let body = String::from_utf8_lossy(&chat.body);
    assert!(
        body.contains("跟随原文"),
        "默认风格的篇幅没进提示词：{body}"
    );
    assert!(body.contains("冷静记录"), "默认风格的语气没进提示词");
    assert!(body.contains("治愈"), "禁用表达没进提示词");
}

/// 用户改了「整理方式」（模板），**下一次生成真的用上它吗**。
///
/// 模板决定「能改多少」，用户以为选了「只改错别字与标点」就几乎不动原文。
#[test]
fn the_organize_template_reaches_the_generation_prompt() {
    let dir = tempfile::tempdir().unwrap();
    let transport = Arc::new(ScriptedTransport::ok(vec![chat_ok()]).repeating());
    let mut core = new_core(dir.path());
    core.set_transport_for_test(transport.clone() as Arc<dyn Transport>);
    core.save_provider(&provider(), Some("sk-test-0123456789"))
        .unwrap();
    core.test_provider("prv_a", &[ProviderCapability::GenerateText])
        .unwrap();

    let settings = core.get_settings().unwrap();
    let mut next = settings.clone();
    next.auto_organize_enabled = true;
    core.update_settings(&next, settings.revision).unwrap();

    // 换模板（界面「整理方式」写的就是这个字段）。
    let organize = core.organize_settings().unwrap();
    let mut changed = organize.clone();
    changed.template_id = "proofread".to_owned();
    core.update_organize_settings(&changed, organize.revision)
        .unwrap();
    assert_eq!(
        core.organize_settings().unwrap().template_id,
        "proofread",
        "整理方式要真的写进去"
    );

    capture(&mut core, "今天很累。", "op-template");
    core.generate_diary("2026-11-06", None, DiaryGenerationReason::Manual, "gen-template")
        .unwrap();
    core.run_due_jobs(chrono::Utc::now(), 30_000, "test", 16)
        .unwrap();

    let requests = transport.requests();
    let chat = requests
        .iter()
        .rfind(|request| request.url.contains("chat/completions"))
        .expect("要发一次生成请求");
    let body = String::from_utf8_lossy(&chat.body);
    assert!(
        body.contains("错别字") || body.contains("标点"),
        "「只改错别字与标点」的模板规则没进提示词：{body}"
    );
}

/// 用户「选了用哪个服务整理」，**下一次生成真的走那一个吗**。
#[test]
fn the_selected_provider_is_the_one_that_gets_called() {
    let dir = tempfile::tempdir().unwrap();
    let transport = Arc::new(ScriptedTransport::ok(vec![chat_ok()]).repeating());
    let mut core = new_core(dir.path());
    core.set_transport_for_test(transport.clone() as Arc<dyn Transport>);

    let mut second = provider();
    second.id = "prv_b".to_owned();
    second.label = "第二个".to_owned();
    second.model = Some("deepseek-v4-pro".to_owned());
    core.save_provider(&provider(), Some("sk-test-0123456789"))
        .unwrap();
    core.save_provider(&second, Some("sk-test-0123456789"))
        .unwrap();
    core.test_provider("prv_a", &[ProviderCapability::GenerateText])
        .unwrap();
    core.test_provider("prv_b", &[ProviderCapability::GenerateText])
        .unwrap();

    // 选第二个。
    core.select_generation_provider("prv_b").unwrap();
    let rows = core.list_providers().unwrap();
    let enabled: Vec<&str> = rows
        .iter()
        .filter(|item| item.enabled)
        .map(|item| item.id.as_str())
        .collect();
    assert_eq!(enabled, vec!["prv_b"], "选中的那条之外应当被停用");

    capture(&mut core, "今天很累。", "op-select");
    core.generate_diary("2026-11-06", None, DiaryGenerationReason::Manual, "gen-select")
        .unwrap();
    core.run_due_jobs(chrono::Utc::now(), 30_000, "test", 16)
        .unwrap();

    let requests = transport.requests();
    let chat = requests
        .iter()
        .rfind(|request| request.url.contains("chat/completions"))
        .expect("要发一次生成请求");
    let body = String::from_utf8_lossy(&chat.body);
    assert!(
        body.contains("deepseek-v4-pro"),
        "调用的不是用户选的那个服务：{body}"
    );
}

/// 用户「保存风格」但**没设默认**时，生成不该用上它——把这条也钉住，
/// 免得以后有人以为保存就等于生效。
#[test]
fn a_saved_style_that_is_not_default_does_not_affect_generation() {
    let dir = tempfile::tempdir().unwrap();
    let transport = Arc::new(ScriptedTransport::ok(vec![chat_ok()]).repeating());
    let mut core = new_core(dir.path());
    core.set_transport_for_test(transport.clone() as Arc<dyn Transport>);
    core.save_provider(&provider(), Some("sk-test-0123456789"))
        .unwrap();
    core.test_provider("prv_a", &[ProviderCapability::GenerateText])
        .unwrap();

    core.save_style(&style("没设默认", "跟随原文", "冷静记录"), None)
        .unwrap();

    capture(&mut core, "今天很累。", "op-nodefault");
    core.generate_diary("2026-11-06", None, DiaryGenerationReason::Manual, "gen-nodefault")
        .unwrap();
    core.run_due_jobs(chrono::Utc::now(), 30_000, "test", 16)
        .unwrap();

    let requests = transport.requests();
    let chat = requests
        .iter()
        .rfind(|request| request.url.contains("chat/completions"))
        .expect("要发一次生成请求");
    let body = String::from_utf8_lossy(&chat.body);
    assert!(
        !body.contains("跟随原文"),
        "没设默认的风格不该进提示词：{body}"
    );
}

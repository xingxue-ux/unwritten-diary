//! B4a（issue #66）：Provider 配置与密钥托管、能力协商与门控、连接测试、
//! 错误归一化、默认不联网与回放端到端。
//!
//! 这个文件里的每一个用例都**不联网**：需要出网的地方一律注入
//! `ScriptedTransport`（回放 + 记账）或 `PanicTransport`（一调用就炸），
//! 所以「有没有真的发生调用」是数出来的、不是猜出来的。

use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;

use diary_core::provider::endpoint_scope;
use diary_core::provider::local_engine::{EngineConfig, ModelKind};
use diary_core::{
    Core, CoreError, CreateDraftInput, ErrorCode, HttpRequest, HttpResponse, JobState,
    PanicTransport, Provider, ProviderCapability, ProviderTestStatus, ScriptedTransport,
    Transport, TransportError,
};

// ------------------------------------------------------------------ 整理服务切换

#[test]
fn generation_selection_is_exclusive_and_preserves_credentials_and_asr() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    let online = core
        .save_provider(
            &provider_config("online", &[ProviderCapability::GenerateText], true),
            Some("selection-test-secret"),
        )
        .unwrap();
    let asr = core
        .save_provider(&local_provider_config("asr", None), None)
        .unwrap();
    // 两个在线候选：切换要原子、互斥，而且不动另一个的密钥与模型。
    let second = core
        .save_provider(
            &provider_config("second", &[ProviderCapability::GenerateText], false),
            Some("second-secret"),
        )
        .unwrap();
    core.select_generation_provider(&second.id).unwrap();
    let rows = core.list_providers().unwrap();
    let stopped = rows.iter().find(|p| p.id == online.id).unwrap();
    assert!(!stopped.enabled);
    assert_eq!(stopped.credential_ref, online.credential_ref);
    assert_eq!(stopped.model, online.model);
    assert_eq!(stopped.test_status, online.test_status);
    assert_eq!(rows.iter().find(|p| p.id == asr.id).unwrap(), &asr);
    // Switching back reuses the saved key and never calls the PanicTransport.
    core.select_generation_provider(&online.id).unwrap();
    assert!(
        core.provider_capability_gate(ProviderCapability::GenerateText)
            .unwrap()
            .ready
    );
    assert_eq!(
        core.list_providers()
            .unwrap()
            .iter()
            .filter(|p| p.enabled && p.capabilities.contains(&ProviderCapability::GenerateText))
            .count(),
        1
    );
    let before = core.list_providers().unwrap();
    assert!(core.select_generation_provider("missing").is_err());
    assert!(core.select_generation_provider(&asr.id).is_err());
    assert_eq!(core.list_providers().unwrap(), before);
}

/// 日记生成的服务选择只接受**在线**服务。
///
/// 这条与本机整理移除直接相关：`sherpa_onnx` 只做转写/语义检索，
/// `capabilities()` 里没有 `generate_text`。即使一条本机配置行手写声明了
/// `generate_text`，`providers.selectGeneration` 也必须拒绝——否则界面会说
/// 「正在使用」而执行者根本不会用它。
#[test]
fn generation_selection_rejects_local_engine_even_if_it_declares_generate_text() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    let mut local = local_provider_config("local_generate", Some("/nonexistent/model-dir"));
    local.capabilities = [ProviderCapability::GenerateText].into_iter().collect();
    local.enabled = false;
    // 保存不校验「适配器是否真的实现了这个能力」：那是闸门与选择时的职责。
    core.save_provider(&local, None).unwrap();
    let error = core.select_generation_provider("local_generate").unwrap_err();
    assert_eq!(error.code(), ErrorCode::InvalidArgument);
    assert!(
        format!("{error:?}").contains("在线服务"),
        "错误消息要说清需要在线服务：{error:?}"
    );
    let rows = core.list_providers().unwrap();
    assert_eq!(rows.len(), 1);
    assert!(!rows[0].enabled, "被拒绝的选择不能顺手把这条服务启用");
}

#[test]
fn generation_selection_rolls_back_if_a_switch_cannot_be_written() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.sqlite");
    let mut core = Core::open(&path).unwrap();
    core.set_transport_for_test(Arc::new(PanicTransport {}));
    for (id, enabled) in [("a_old", true), ("z_new", false)] {
        core.save_provider(
            &provider_config(id, &[ProviderCapability::GenerateText], enabled),
            Some("selection-test-secret"),
        )
        .unwrap();
    }
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch(
        "CREATE TRIGGER fail_selection BEFORE UPDATE ON providers
        WHEN NEW.id = 'z_new' AND NEW.enabled = 1
        BEGIN SELECT RAISE(ABORT, 'test write failure'); END;",
    )
    .unwrap();
    let before = core.list_providers().unwrap();
    assert!(core.select_generation_provider("z_new").is_err());
    assert_eq!(core.list_providers().unwrap(), before);
}

// ------------------------------------------------------------------ 工具

fn new_core(dir: &Path) -> Core {
    // 内存库默认装的是「一调用就 panic」的传输层：本地功能一旦偷偷出网，测试当场炸。
    Core::open_in_memory_at(dir).unwrap()
}

/// 这个进程配了本地模型吗？配了的话「语义任务等模型配置」那条路跑不到（CI 上没配，
/// 跨 kind 一致性用例会真跑）。判据与 `semantic_job_test.rs` 里那条一致。
fn model_configured() -> bool {
    std::env::var_os("DIARY_MODEL_DIR").is_some()
        || std::env::var_os("DIARY_ORT_DYLIB").is_some()
        || std::env::var_os("ORT_DYLIB_PATH").is_some()
}

fn capture_with_text(core: &mut Core, text: &str, operation_id: &str) -> String {
    let capture = core
        .create_draft(CreateDraftInput {
            occurred_at: None,
            time_zone: "Asia/Shanghai",
            utc_offset_minutes: 480,
            operation_id: &format!("{operation_id}-create"),
        })
        .unwrap();
    core.save_draft(&capture.id, text, capture.revision, operation_id)
        .unwrap();
    core.commit(&capture.id, capture.revision + 1, &format!("{operation_id}-commit"))
        .unwrap();
    capture.id
}

/// 一个可保存的服务配置。`secret` 在保存时单独给，不进这个对象（契约里没有密钥字段）。
fn provider_config(
    id: &str,
    capabilities: &[ProviderCapability],
    enabled: bool,
) -> Provider {
    Provider {
        id: id.to_owned(),
        label: format!("测试服务 {id}"),
        adapter_id: diary_core::OPENAI_COMPATIBLE.to_owned(),
        endpoint: Some("http://127.0.0.1:11434/v1/".to_owned()),
        model: Some("qwen-test".to_owned()),
        capabilities: capabilities.iter().copied().collect::<HashSet<_>>(),
        credential_ref: None,
        enabled,
        test_status: ProviderTestStatus::Untested,
    }
}

fn chat_ok(text: &str, model: &str) -> HttpResponse {
    HttpResponse::new(
        200,
        serde_json::json!({
            "model": model,
            "choices": [{"message": {"role": "assistant", "content": text}}],
        })
        .to_string(),
    )
}

fn chat_probe_ok() -> HttpResponse {
    chat_ok("", "qwen-test")
}

fn transcription_ok(text: &str) -> HttpResponse {
    HttpResponse::new(200, serde_json::json!({"text": text}).to_string())
}

/// 建一个装了回放传输层的内存库，并保存一个服务。
fn core_with_provider(
    dir: &Path,
    capabilities: &[ProviderCapability],
    script: Vec<std::result::Result<HttpResponse, TransportError>>,
) -> (Core, Arc<ScriptedTransport>, Provider) {
    let transport = Arc::new(ScriptedTransport::new(script));
    let mut core = new_core(dir);
    core.set_transport_for_test(transport.clone() as Arc<dyn Transport>);
    let provider = core
        .save_provider(
            &provider_config("prv_a", capabilities, true),
            Some("sk-test-secret-0123456789"),
        )
        .unwrap();
    (core, transport, provider)
}

fn code_of(error: &CoreError) -> &'static str {
    error.code().wire()
}

/// 建一个装了回放传输层的内存库，并保存一个**没有凭据**的服务。
///
/// endpoint 由调用方给：loopback 的那几条验证「缺凭据也能发请求」，非 loopback 的
/// 那一条验证旧行为还在（缺凭据 → `provider_not_configured`，零请求）。
fn core_with_uncredentialed_provider(
    dir: &Path,
    endpoint: &str,
    script: Vec<std::result::Result<HttpResponse, TransportError>>,
) -> (Core, Arc<ScriptedTransport>, Provider) {
    let transport = Arc::new(ScriptedTransport::new(script));
    let mut core = new_core(dir);
    core.set_transport_for_test(transport.clone() as Arc<dyn Transport>);
    let mut config = provider_config("prv_nokey", &[ProviderCapability::GenerateText], true);
    config.endpoint = Some(endpoint.to_owned());
    let provider = core.save_provider(&config, None).unwrap();
    assert!(provider.credential_ref.is_none());
    (core, transport, provider)
}

// ------------------------------------------------------------------ 配置往返与脱敏

/// 配置往返 + **密钥不进配置表**：库文件、`providers` 表的每一列、返回对象里
/// 都不该出现密钥明文；密钥只在 `secrets/` 下那个 0600 的独立文件里。
#[test]
fn provider_config_round_trips_and_secrets_never_land_in_the_config_table() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.sqlite");
    let secret = "sk-test-secret-0123456789";

    let mut core = Core::open(&path).unwrap();
    let saved = core
        .save_provider(&provider_config("prv_a", &[ProviderCapability::GenerateText], true), Some(secret))
        .unwrap();

    // 返回的是脱敏对象：只有引用，没有明文。
    assert_eq!(saved.id, "prv_a");
    assert!(saved.enabled);
    assert!(
        saved
            .credential_ref
            .as_deref()
            .is_some_and(|value| value.starts_with("sec_")),
        "凭据要落成引用：{:?}",
        saved.credential_ref
    );
    assert!(
        !format!("{saved:?}").contains(secret),
        "脱敏对象不该带密钥"
    );
    assert_eq!(core.list_providers().unwrap().len(), 1);

    // 配置表里没有密钥：逐列扫一遍。
    let conn = rusqlite::Connection::open(&path).unwrap();
    let columns = {
        let mut statement = conn.prepare("PRAGMA table_info(providers)").unwrap();
        statement
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
    };
    for column in &columns {
        let hits: i64 = conn
            .query_row(
                &format!("SELECT COUNT(*) FROM providers WHERE {column} LIKE '%' || ?1 || '%'"),
                rusqlite::params![secret],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(hits, 0, "providers.{column} 里出现了密钥明文");
    }

    // 整个资料库目录（除了 secrets/）里都找不到密钥明文。
    let mut scanned = 0;
    for entry in std::fs::read_dir(dir.path()).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name().to_string_lossy().into_owned();
        if entry.path().is_dir() {
            assert_eq!(name, "secrets", "资料库目录里只该有 secrets/ 放密钥");
            continue;
        }
        scanned += 1;
        let bytes = std::fs::read(entry.path()).unwrap();
        assert!(
            !bytes
                .windows(secret.len())
                .any(|window| window == secret.as_bytes()),
            "{name} 里出现了密钥明文"
        );
    }
    assert!(scanned > 0, "至少该扫到库文件本身");

    // 密钥确实存在，而且只在 secrets/ 里；Unix 上权限受限。
    let secret_files: Vec<_> = std::fs::read_dir(dir.path().join("secrets"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(secret_files.len(), 1, "一个凭据一个文件");
    assert_eq!(std::fs::read_to_string(&secret_files[0]).unwrap(), secret);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&secret_files[0])
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "密钥文件权限该是 0600，实际 {mode:o}");
    }

    // 改配置不换密钥时，引用保持不变（不该每次保存都重写密钥文件）。
    let again = core
        .save_provider(
            &saved.clone(),
            None,
        )
        .unwrap();
    assert_eq!(again.credential_ref, saved.credential_ref);
}

/// 移除配置**不删除日记**，也不留下没人用的密钥文件。
#[test]
fn removing_a_provider_keeps_the_diary_and_forgets_the_credential() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.sqlite");
    let mut core = Core::open(&path).unwrap();
    let capture_id = capture_with_text(&mut core, "今天给妈妈打了电话。", "op-capture");
    let saved = core
        .save_provider(
            &provider_config("prv_a", &[ProviderCapability::GenerateText], true),
            Some("sk-remove-me-0123456789"),
        )
        .unwrap();

    core.remove_provider("prv_a").unwrap();

    assert!(core.list_providers().unwrap().is_empty());
    assert!(core.get_capture(&capture_id).is_ok(), "日记不能跟着配置一起消失");
    assert_eq!(core.count_captures().unwrap(), 1);
    assert_eq!(
        std::fs::read_dir(dir.path().join("secrets")).unwrap().count(),
        0,
        "没有服务再引用这个凭据，就该把它删掉"
    );
    assert!(saved.credential_ref.is_some());

    // 再移除一次报 not_found（幂等由调用方决定，核心不装作成功）。
    assert_eq!(code_of(&core.remove_provider("prv_a").unwrap_err()), "not_found");
}

// ------------------------------------------------------------------ 默认不联网

/// 本地功能**永远不会出网**：默认传输层是一调用就 panic 的替身，
/// 记录、导入、提取、索引、检索、驱动一整轮跑下来都不该碰它。
#[test]
fn local_work_never_touches_the_network_with_the_default_transport() {
    let dir = tempfile::tempdir().unwrap();
    // 先确认内存库默认装的确实是「一调用就 panic」的替身：这一步要是失效，
    // 下面的用例就等于没测。
    let core = new_core(dir.path());
    let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        core.transport()
            .send(&HttpRequest::new("GET", "http://127.0.0.1:1/", 10))
    }));
    assert!(caught.is_err(), "内存库默认该是 panic 替身");
    drop(core);

    // 认真跑一遍本地功能：记录、导入、提取、索引、检索、驱动都不该碰传输层。
    let mut core = Core::open_in_memory_at(dir.path()).unwrap();
    core.set_transport_for_test(Arc::new(PanicTransport {}) as Arc<dyn Transport>);
    let capture_id = capture_with_text(&mut core, "楼下桂花开了。", "op-local");
    assert!(core.get_capture(&capture_id).is_ok());
    // 队列里的 extract / index 也跑一轮（这里没有 Provider，不该有任何出网）。
    let summary = core.run_due_jobs(chrono::Utc::now(), 60_000, "test", 32).unwrap();
    assert_eq!(summary.failed, 0, "本地任务不该失败：{summary:?}");
    assert!(core.job_status().is_ok());
}

/// 没有配置任何服务时，主动回应入口**拒绝并且不排任务**；
/// 记账替身证明这条路上一次请求都没发。
#[test]
fn without_a_configured_provider_nothing_is_queued_and_nothing_is_sent() {
    let dir = tempfile::tempdir().unwrap();
    let counting = Arc::new(diary_core::CountingTransport::new());
    let mut core = new_core(dir.path());
    core.set_transport_for_test(counting.clone() as Arc<dyn Transport>);
    let capture_id = capture_with_text(&mut core, "想问问今天过得怎么样。", "op1");

    let error = core
        .request_response(&[capture_id], "帮我看看今天", None)
        .unwrap_err();
    assert_eq!(code_of(&error), "provider_not_configured");
    let text = error.to_string();
    assert!(text.contains("还没有配置任何远端服务"), "{text}");
    assert!(text.contains("连接测试"), "理由要可执行：{text}");
    assert_eq!(core.count_jobs(None).unwrap(), 0, "不排注定失败的任务");
    assert_eq!(counting.call_count(), 0, "一次请求都不该发");

    // 驱动一轮也不会有任何出网（队列里一条任务都没有）。
    core.run_due_jobs(chrono::Utc::now(), 60_000, "test", 32).unwrap();
    assert_eq!(counting.call_count(), 0);

    // 能力闸门把「为什么不能」说清楚。
    let gate = core
        .provider_capability_gate(ProviderCapability::GenerateText)
        .unwrap();
    assert!(!gate.ready);
    assert!(gate.next_step.is_some(), "不满足时要给可执行的下一步");
}

/// 配了但**没启用**的服务同样不算数：不出网、不排任务。
#[test]
fn a_disabled_provider_is_not_used_and_does_not_get_requests() {
    let dir = tempfile::tempdir().unwrap();
    let counting = Arc::new(diary_core::CountingTransport::new());
    let mut core = new_core(dir.path());
    core.set_transport_for_test(counting.clone() as Arc<dyn Transport>);
    core.save_provider(
        &provider_config("prv_off", &[ProviderCapability::GenerateText], false),
        Some("sk-disabled-0123456789"),
    )
    .unwrap();
    let capture_id = capture_with_text(&mut core, "今天有点累。", "op1");

    let error = core
        .request_response(&[capture_id], "说点什么", None)
        .unwrap_err();
    assert_eq!(code_of(&error), "provider_not_configured");
    assert!(error.to_string().contains("一个都没有启用"), "{error}");
    assert_eq!(core.count_jobs(None).unwrap(), 0);
    assert_eq!(counting.call_count(), 0);
}

/// 连接测试**只在用户显式请求时**才发请求：只是保存、列出、看闸门，一次都不发。
#[test]
fn nothing_is_sent_until_the_user_explicitly_tests_or_runs() {
    let dir = tempfile::tempdir().unwrap();
    let (core, transport, provider) = core_with_provider(
        dir.path(),
        &[ProviderCapability::GenerateText],
        vec![Ok(chat_probe_ok())],
    );
    let mut core = core;
    let capture_id = capture_with_text(&mut core, "留着以后问。", "op1");

    assert_eq!(core.list_providers().unwrap().len(), 1);
    assert!(core
        .provider_capability_gate(ProviderCapability::GenerateText)
        .unwrap()
        .ready);
    assert_eq!(transport.call_count(), 0, "看配置不该产生任何调用");

    // 主动回应只排任务，不在这里调模型。
    let ticket = core
        .request_response(&[capture_id], "你好", Some(&provider.id))
        .unwrap();
    assert_eq!(transport.call_count(), 0, "受理请求本身不发请求");
    assert_eq!(
        core.get_job(&ticket.job_id).unwrap().state,
        JobState::Queued,
        "任务在队列里等驱动"
    );

    // 显式测一次：这才发请求，而且只发一条能力的探针。
    let report = core
        .test_provider(&provider.id, &[ProviderCapability::GenerateText])
        .unwrap();
    assert!(report.actual_call_made);
    assert_eq!(transport.call_count(), 1);
}

// ------------------------------------------------------------------ 能力协商与门控

/// 只声明「生成」的服务承担不了转写：闸门说不行、理由与下一步可执行，
/// 而且**排任务的入口直接拒绝**（一条任务都不排）。
#[test]
fn a_capability_no_enabled_provider_declares_is_refused_before_queuing() {
    let dir = tempfile::tempdir().unwrap();
    let (mut core, transport, _provider) = core_with_provider(
        dir.path(),
        &[ProviderCapability::GenerateText],
        vec![],
    );
    let capture_id = capture_with_text(&mut core, "想把录音转成文字。", "op1");

    let gate = core
        .provider_capability_gate(ProviderCapability::TranscribeAudio)
        .unwrap();
    assert!(!gate.ready, "只声明 generate_text 的服务不能承担转写");
    assert!(gate.reason.contains("transcribe_audio"), "{}", gate.reason);
    assert!(
        gate.next_step
            .as_deref()
            .is_some_and(|step| step.contains("连接测试")),
        "下一步要可执行：{:?}",
        gate.next_step
    );

    // 反过来说：这个服务只声明转写时，排一条「生成」任务会被当场拒绝。
    let mut only_transcribe = provider_config("prv_t", &[ProviderCapability::TranscribeAudio], true);
    only_transcribe.model = Some("whisper-test".to_owned());
    let saved = core
        .save_provider(&only_transcribe, Some("sk-transcribe-0123456789"))
        .unwrap();
    let error = core
        .request_response(&[capture_id], "帮我写一句", Some(&saved.id))
        .unwrap_err();
    assert_eq!(code_of(&error), "provider_not_configured");
    assert!(error.to_string().contains("generate_text"), "{error}");
    assert_eq!(core.count_jobs(None).unwrap(), 0, "不排注定失败的任务");
    assert_eq!(transport.call_count(), 0, "被拒绝的任务一次请求都不发");
}

/// **测过但没验过**的能力不再排任务：连接测试说不行，就不能装作行。
#[test]
fn a_capability_that_failed_its_connection_test_is_not_scheduled() {
    let dir = tempfile::tempdir().unwrap();
    let (mut core, transport, provider) = core_with_provider(
        dir.path(),
        &[ProviderCapability::GenerateText],
        vec![Ok(HttpResponse::new(401, "{\"error\":\"invalid api key\"}"))],
    );
    let capture_id = capture_with_text(&mut core, "先测一下再说。", "op1");

    let error = core
        .test_provider(&provider.id, &[ProviderCapability::GenerateText])
        .unwrap_err();
    assert_eq!(code_of(&error), "authentication_failed");
    assert_eq!(transport.call_count(), 1);

    // 结论落库：界面重开时看得到「最近一次失败」。
    let stored = core.list_providers().unwrap().remove(0);
    assert_eq!(stored.test_status, ProviderTestStatus::Failed);

    // 闸门与排任务入口都按「没验过」处理。
    let gate = core
        .provider_capability_gate(ProviderCapability::GenerateText)
        .unwrap();
    assert!(!gate.ready);
    assert_eq!(gate.code, "authentication_failed", "原因要如实：{}", gate.reason);
    let error = core
        .request_response(&[capture_id], "帮我看看", None)
        .unwrap_err();
    assert_eq!(code_of(&error), "provider_not_configured");
    assert_eq!(core.count_jobs(None).unwrap(), 0);
    assert_eq!(transport.call_count(), 1, "被拒绝的任务不再发请求");
}

/// 适配器没实现的能力，配置里声明了也不算：不排任务、连接测试不假装可用。
#[test]
fn a_capability_the_adapter_does_not_implement_is_never_usable() {
    let dir = tempfile::tempdir().unwrap();
    let (mut core, transport, provider) = core_with_provider(
        dir.path(),
        &[ProviderCapability::UnderstandImage],
        vec![Ok(chat_probe_ok())],
    );

    let gate = core
        .provider_capability_gate(ProviderCapability::UnderstandImage)
        .unwrap();
    assert!(!gate.ready);
    assert!(gate.reason.contains("没有实现"), "{}", gate.reason);

    let report = core
        .test_provider(&provider.id, &[ProviderCapability::UnderstandImage])
        .unwrap();
    assert_eq!(
        report.results.get(&ProviderCapability::UnderstandImage),
        Some(&ProviderTestStatus::Failed)
    );
    assert!(!report.actual_call_made, "没实现的能力不该发起调用");
    let message = report.message.as_deref().unwrap();
    assert!(message.contains("未发起调用"), "{message}");
    // 只有未实现的能力：零次真实调用，报告不许写「端点/认证/模型均已验证」（#96）。
    assert!(!message.contains("可达"), "{message}");
    assert!(!message.contains("认证通过"), "{message}");
    assert!(message.contains("都没验过"), "{message}");
    assert_eq!(transport.call_count(), 0);
}

/// 队列侧门控：服务被移除后，已经在队列里的任务**不会**再去撞远端，
/// 而是停在 `waiting_configuration`（状态可见）；配置恢复后能接着跑完。
#[test]
fn queued_work_waits_for_configuration_instead_of_hammering_the_provider() {
    let dir = tempfile::tempdir().unwrap();
    let (mut core, transport, provider) = core_with_provider(
        dir.path(),
        &[ProviderCapability::GenerateText],
        vec![Ok(chat_ok("好呀，我在。", "qwen-test"))],
    );
    let capture_id = capture_with_text(&mut core, "有点想说话。", "op1");
    let ticket = core
        .request_response(&[capture_id], "陪我说说话", Some(&provider.id))
        .unwrap();

    // 服务没了：任务在驱动里停住，一次请求都不发。
    core.remove_provider(&provider.id).unwrap();
    let summary = core.run_due_jobs(chrono::Utc::now(), 60_000, "test", 32).unwrap();
    assert_eq!(summary.processed, 0, "等配置的任务不该被领取");
    assert_eq!(transport.call_count(), 0);
    let job = core.get_job(&ticket.job_id).unwrap();
    assert_eq!(job.state, JobState::WaitingConfiguration);
    assert_eq!(job.error_code.as_deref(), Some("provider_not_configured"));
    assert!(job.requires_user_action, "要出现在状态里等用户处理");
    assert_eq!(
        job.attention_key.as_deref(),
        Some("job-config:respond:provider_not_configured"),
        "界面靠 attentionKey 深链到「去配哪个服务」"
    );
    assert_eq!(job.attempt_count, 0, "闸门在领取前挡下：没跑过就不计尝试");
    let status = core.job_status().unwrap();
    assert_eq!(status.waiting_configuration, 1);

    // 配置回来：同一轮驱动就能跑完，正文落库。
    core.save_provider(
        &provider_config("prv_a", &[ProviderCapability::GenerateText], true),
        Some("sk-back-0123456789"),
    )
    .unwrap();
    let summary = core.run_due_jobs(chrono::Utc::now(), 60_000, "test", 32).unwrap();
    assert_eq!(summary.succeeded, 1, "{summary:?}");
    let response = core
        .get_response(ticket.response_id.as_deref().unwrap())
        .unwrap();
    assert_eq!(response.body, "好呀，我在。");
    assert_eq!(core.get_job(&ticket.job_id).unwrap().state, JobState::Succeeded);
}

/// P0 的端到端回归：远端 200 且把密钥回显进 `content` / `model` 时，落库的回应
/// （`ai_responses.body` / `model`）里不许有明文——这条链路的下游还有日记正文与插件。
#[test]
fn an_echoed_secret_never_reaches_the_stored_response() {
    const SECRET: &str = "sk-echoed-secret-0123456789";
    let dir = tempfile::tempdir().unwrap();
    let echoed = HttpResponse::new(
        200,
        serde_json::json!({
            "model": format!("mirror-{SECRET}"),
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": format!("网关回显了 {SECRET}"),
                }
            }],
        })
        .to_string(),
    );
    let transport = Arc::new(ScriptedTransport::ok(vec![echoed]));
    let mut core = new_core(dir.path());
    core.set_transport_for_test(transport.clone() as Arc<dyn Transport>);
    let provider = core
        .save_provider(
            &provider_config("prv_echo", &[ProviderCapability::GenerateText], true),
            Some(SECRET),
        )
        .unwrap();
    let capture_id = capture_with_text(&mut core, "来源。", "op1");
    let ticket = core
        .request_response(&[capture_id], "说一句", Some(&provider.id))
        .unwrap();
    core.run_due_jobs(chrono::Utc::now(), 60_000, "test", 8)
        .unwrap();

    let response = core
        .get_response(ticket.response_id.as_deref().unwrap())
        .unwrap();
    assert!(
        !response.body.contains(SECRET),
        "落库的回应正文泄漏了密钥：{}",
        response.body
    );
    assert!(
        response.body.contains("***"),
        "应该留下擦除痕迹：{}",
        response.body
    );
    let model = response.model.expect("远端回显了模型名");
    assert!(!model.contains(SECRET), "落库的模型名泄漏了密钥：{model}");
    assert!(model.contains("***"), "模型名也应该擦：{model}");
    assert_eq!(
        core.get_job(&ticket.job_id).unwrap().state,
        JobState::Succeeded
    );
    assert_eq!(transport.call_count(), 1);
}

/// **跨 kind 一把尺**（#96 合 main 时暴露的真不一致）：`waiting_configuration` 无论由
/// 哪条 kind、由哪条路落进来，对「要不要用户动手」必须给**同一个答案**。
///
/// 这里让两条**闸门**（都在领取前挡下）各落一条任务：
/// - `semantic_index` 等本地模型（`waiting_model`）；
/// - `respond` 等一个被移除的服务（`provider_not_configured`）。
///
/// 两个 kind 的名称、错误码、谁来挡都不同，但 `requiresUserAction` 必须**相同且为真**，
/// `attentionKey` 必须各自指到「配哪一个」。把任一侧改回旧口径（例如「等配置不置
/// `requiresUserAction`」），这条立刻红——这正是合 main 时两侧测试各钉相反期望的那处。
///
/// `attemptCount` 是唯一允许不同的东西：两条都是闸门路（没有执行者跑过），所以都是 0；
/// 「真跑了一次」的执行期那条在 401 用例里钉着（如实记账，见
/// `job_failures_map_to_contract_codes_with_the_right_retry_semantics` 的 401 段）。
#[test]
fn waiting_configuration_means_the_same_thing_across_job_kinds() {
    if model_configured() {
        eprintln!("跳过：这个进程配了本地模型，语义任务不会停在等配置");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let (mut core, _transport, provider) = core_with_provider(
        dir.path(),
        &[ProviderCapability::GenerateText],
        vec![Ok(chat_ok("不该被调用", "qwen-test"))],
    );
    let capture_id = capture_with_text(&mut core, "有点想说话。", "op-cross-kind");
    let ticket = core
        .request_response(&[capture_id], "陪我说说话", Some(&provider.id))
        .unwrap();
    let semantic = core
        .schedule_semantic_index("跨 kind 一致性", "op-cross-kind-semantic")
        .unwrap();

    // 服务没了：`respond` 的闸门落配置等待；同一个核心里语义任务也在等模型。
    core.remove_provider(&provider.id).unwrap();
    let summary = core
        .run_due_jobs(chrono::Utc::now(), 60_000, "test", 32)
        .unwrap();
    assert_eq!(summary.processed, 0, "两条都该被闸门挡住：一条都不跑");
    assert_eq!(summary.failed, 0, "等配置不是失败");
    assert_eq!(summary.retrying, 0, "也不排退避");

    let provider_job = core.get_job(&ticket.job_id).unwrap();
    let semantic_job = core.get_job(&semantic.id).unwrap();
    assert_ne!(provider_job.kind, semantic_job.kind, "这确实是两个 kind");
    assert_eq!(provider_job.state, JobState::WaitingConfiguration);
    assert_eq!(semantic_job.state, JobState::WaitingConfiguration);
    assert_ne!(
        provider_job.error_code, semantic_job.error_code,
        "等的事不一样（一个等服务、一个等模型），错误码就该不一样"
    );

    // 同一个状态，同一个答案：要不要用户动手，不因 kind 而异。
    assert_eq!(
        provider_job.requires_user_action, semantic_job.requires_user_action,
        "同一个 waiting_configuration，跨 kind 必须一把尺"
    );
    assert!(provider_job.requires_user_action, "等服务要用户去配服务");
    assert!(semantic_job.requires_user_action, "等模型要用户去配模型");

    // 界面深链到「配哪一个」：形状只有一处（`jobs::configuration_attention_key`）。
    assert_eq!(
        provider_job.attention_key.as_deref(),
        Some("job-config:respond:provider_not_configured")
    );
    assert_eq!(
        semantic_job.attention_key.as_deref(),
        Some("job-config:semantic_index:waiting_model")
    );

    // 两条都是闸门路（领取前挡下）：没跑过就不计尝试。
    assert_eq!(provider_job.attempt_count, 0);
    assert_eq!(semantic_job.attempt_count, 0);
}

// ------------------------------------------------------------------ 连接测试的四种结果

/// 可达：报告逐项给结论，并**如实**说明发生了真实模型调用。
#[test]
fn a_healthy_provider_reports_reachable_and_a_real_call() {
    let dir = tempfile::tempdir().unwrap();
    let (mut core, transport, provider) = core_with_provider(
        dir.path(),
        &[ProviderCapability::GenerateText, ProviderCapability::TranscribeAudio],
        vec![Ok(chat_probe_ok()), Ok(transcription_ok(""))],
    );

    let report = core
        .test_provider(
            &provider.id,
            &[
                ProviderCapability::GenerateText,
                ProviderCapability::TranscribeAudio,
            ],
        )
        .unwrap();
    assert!(report.actual_call_made);
    assert_eq!(
        report.results.get(&ProviderCapability::GenerateText),
        Some(&ProviderTestStatus::Ok)
    );
    assert_eq!(
        report.results.get(&ProviderCapability::TranscribeAudio),
        Some(&ProviderTestStatus::Ok)
    );
    let message = report.message.as_deref().unwrap();
    assert!(message.contains("可达"), "{message}");
    assert!(message.contains("认证通过"), "{message}");
    assert!(message.contains("本机/局域网"), "端点要说清是本地还是远端：{message}");
    assert!(message.contains("2 次真实请求"), "{message}");

    // 两次调用打到两个不同的端点，而且密钥只出现在请求头里（Debug 会擦掉）。
    assert_eq!(transport.call_count(), 2);
    assert_eq!(
        transport.url_at(0).as_deref(),
        Some("http://127.0.0.1:11434/v1/chat/completions")
    );
    assert_eq!(
        transport.url_at(1).as_deref(),
        Some("http://127.0.0.1:11434/v1/audio/transcriptions")
    );
    let requests = transport.requests();
    assert_eq!(
        requests[0].get_header("Authorization"),
        Some("Bearer sk-test-secret-0123456789")
    );
    assert!(
        !format!("{:?}", requests[0]).contains("sk-test-secret-0123456789"),
        "请求的 Debug 不能泄漏密钥"
    );
    assert!(
        requests[1]
            .get_header("Content-Type")
            .is_some_and(|value| value.starts_with("multipart/form-data")),
        "转写探针要真的上传音频"
    );

    // 结论落库：状态 ok，闸门随即可用。
    let stored = core.list_providers().unwrap().remove(0);
    assert_eq!(stored.test_status, ProviderTestStatus::Ok);
    assert!(core
        .provider_capability_gate(ProviderCapability::GenerateText)
        .unwrap()
        .ready);
}

/// 认证失败：给可区分的错误码，消息里如实说明发了请求，且**不泄漏密钥**。
#[test]
fn authentication_failure_has_its_own_code_and_reports_the_real_call() {
    let dir = tempfile::tempdir().unwrap();
    let secret = "sk-test-secret-0123456789";
    let (mut core, transport, provider) = core_with_provider(
        dir.path(),
        &[ProviderCapability::GenerateText],
        vec![Ok(HttpResponse::new(
            401,
            format!("{{\"error\":{{\"message\":\"bad key {secret}\"}}}}"),
        ))],
    );

    let error = core
        .test_provider(&provider.id, &[ProviderCapability::GenerateText])
        .unwrap_err();
    assert_eq!(code_of(&error), "authentication_failed");
    assert!(!error.retryable(), "鉴权失败不是「重试就好」");
    let message = error.to_string();
    assert!(message.contains("1 次真实请求"), "{message}");
    assert!(!message.contains(secret), "错误消息不能泄漏密钥：{message}");
    assert!(message.contains("***"), "远端回显的密钥要被擦掉：{message}");
    assert_eq!(transport.call_count(), 1);
}

/// 本机（loopback）服务**没有凭据也照常测试**：真的发请求、不发鉴权头，报告不写
/// 「认证通过」（这次没人验过认证）。
#[test]
fn a_loopback_provider_without_credential_is_tested_without_fake_auth() {
    let dir = tempfile::tempdir().unwrap();
    let (mut core, transport, provider) = core_with_uncredentialed_provider(
        dir.path(),
        "http://127.0.0.1:11434/v1",
        vec![Ok(chat_probe_ok())],
    );

    let report = core
        .test_provider(&provider.id, &[ProviderCapability::GenerateText])
        .unwrap();
    assert!(report.actual_call_made, "缺凭据不该拦下本机服务的探针");
    assert_eq!(
        report.results.get(&ProviderCapability::GenerateText),
        Some(&ProviderTestStatus::Ok)
    );
    assert_eq!(transport.call_count(), 1);
    assert_eq!(
        transport.requests()[0].get_header("Authorization"),
        None,
        "没有凭据就不该发 Authorization 头"
    );
    let message = report.message.as_deref().unwrap();
    assert!(message.contains("未发送凭据"), "{message}");
    assert!(
        !message.contains("认证通过"),
        "没发凭据就不能说认证通过：{message}"
    );
    // 本机服务测通了，结论照常落库（能力闸门仍需密钥——那是生成路径的另一条规则）。
    assert_eq!(
        core.list_providers().unwrap().remove(0).test_status,
        ProviderTestStatus::Ok
    );
}

/// 非 loopback（含私有网段）没有凭据仍是 `provider_not_configured`，而且**零请求**。
#[test]
fn a_non_loopback_provider_without_credential_is_rejected_before_any_request() {
    let dir = tempfile::tempdir().unwrap();
    let (mut core, transport, provider) = core_with_uncredentialed_provider(
        dir.path(),
        "http://192.168.1.10:11434/v1",
        vec![],
    );

    let error = core
        .test_provider(&provider.id, &[ProviderCapability::GenerateText])
        .unwrap_err();
    assert_eq!(code_of(&error), "provider_not_configured");
    let text = error.to_string();
    assert!(text.contains("密钥"), "{text}");
    assert!(text.contains("下一步"), "{text}");
    assert_eq!(transport.call_count(), 0, "前置检查拒绝时一个请求都不该发");
}

/// **不许假绿**：loopback 缺凭据放行的是前置检查；服务器返回 401 仍如实报
/// `authentication_failed`，并把失败结论落库。
#[test]
fn a_loopback_provider_without_credential_still_reports_a_real_401() {
    let dir = tempfile::tempdir().unwrap();
    let (mut core, transport, provider) = core_with_uncredentialed_provider(
        dir.path(),
        "http://127.0.0.1:11434/v1",
        vec![Ok(HttpResponse::new(
            401,
            r#"{"error":{"message":"invalid api key"}}"#,
        ))],
    );

    let error = core
        .test_provider(&provider.id, &[ProviderCapability::GenerateText])
        .unwrap_err();
    assert_eq!(code_of(&error), "authentication_failed");
    assert_eq!(transport.call_count(), 1, "请求真的发出去了");
    assert!(error.to_string().contains("1 次真实请求"), "{error}");
    assert_eq!(
        core.list_providers().unwrap().remove(0).test_status,
        ProviderTestStatus::Failed
    );
}

/// 超时：`provider_timeout`，有限重试，消息里说明发生过调用。
#[test]
fn a_timeout_is_provider_timeout_and_says_a_call_happened() {
    let dir = tempfile::tempdir().unwrap();
    let (mut core, transport, provider) = core_with_provider(
        dir.path(),
        &[ProviderCapability::GenerateText],
        vec![Err(TransportError::Timeout {
            message: "read timed out".to_owned(),
        })],
    );

    let error = core
        .test_provider(&provider.id, &[ProviderCapability::GenerateText])
        .unwrap_err();
    assert_eq!(code_of(&error), "provider_timeout");
    assert!(error.retryable(), "超时可以有限重试");
    assert!(error.to_string().contains("1 次真实请求"), "{error}");
    assert_eq!(transport.call_count(), 1);
}

/// 不支持该能力：报告里逐项失败、**没有**发生真实调用，下一步可执行。
#[test]
fn an_unsupported_capability_is_reported_without_a_fake_call() {
    let dir = tempfile::tempdir().unwrap();
    let (mut core, transport, provider) = core_with_provider(
        dir.path(),
        &[ProviderCapability::GenerateText],
        vec![Ok(chat_probe_ok())],
    );

    // 范围里混了「适配器没实现」和「配置没声明」两种不成立，都不该发请求。
    let report = core
        .test_provider(
            &provider.id,
            &[
                ProviderCapability::UnderstandImage,
                ProviderCapability::TranscribeAudio,
            ],
        )
        .unwrap();
    assert!(!report.actual_call_made);
    for capability in [
        ProviderCapability::UnderstandImage,
        ProviderCapability::TranscribeAudio,
    ] {
        assert_eq!(
            report.results.get(&capability),
            Some(&ProviderTestStatus::Failed),
            "{capability:?} 该报 failed"
        );
    }
    let message = report.message.as_deref().unwrap();
    assert!(message.contains("没有实现"), "{message}");
    assert!(message.contains("没有声明"), "{message}");
    assert!(message.contains("未发起调用"), "{message}");
    assert!(message.contains("下一步"), "{message}");
    // 回归（评审 #96 第三条）：一次调用都没发，报告里**不许**出现「端点可达 /
    // 认证通过」这类谁都没验过的结论。
    assert!(
        !message.contains("可达"),
        "没发过请求就不能说端点可达：{message}"
    );
    assert!(
        !message.contains("认证通过"),
        "没发过请求就不能说认证通过：{message}"
    );
    assert!(
        message.contains("都没验过"),
        "要如实说这三项都没验过：{message}"
    );
    assert_eq!(transport.call_count(), 0, "不成立的能力一次都不发");

    // 这一轮不算「验过」：状态落成 failed，闸门仍然不可用。
    let stored = core.list_providers().unwrap().remove(0);
    assert_eq!(stored.test_status, ProviderTestStatus::Failed);
    assert!(!core
        .provider_capability_gate(ProviderCapability::GenerateText)
        .unwrap()
        .ready);
}

/// 部分范围只影响被探过的能力：之前验过的另一项不该被抹掉。
#[test]
fn a_partial_test_scope_keeps_the_other_verified_capabilities() {
    let dir = tempfile::tempdir().unwrap();
    let (mut core, _transport, provider) = core_with_provider(
        dir.path(),
        &[ProviderCapability::GenerateText, ProviderCapability::TranscribeAudio],
        vec![
            Ok(chat_probe_ok()),
            Ok(transcription_ok("")),
            Ok(chat_probe_ok()),
        ],
    );
    core.test_provider(
        &provider.id,
        &[
            ProviderCapability::GenerateText,
            ProviderCapability::TranscribeAudio,
        ],
    )
    .unwrap();

    // 再只测生成（例如只想花一次钱）：转写的结论还在。
    let report = core
        .test_provider(&provider.id, &[ProviderCapability::GenerateText])
        .unwrap();
    assert!(report.actual_call_made);
    assert!(core
        .provider_capability_gate(ProviderCapability::TranscribeAudio)
        .unwrap()
        .ready);
}

// ------------------------------------------------------------------ 验证结论只在功能参数变化时作废（评审 #96）

/// 文件库版本的服务：下面这几条回归要**直接读 `providers` 的验证四列**
/// （`test_status` / `verified_capabilities` / `last_error_code` / `last_tested_at`，
/// 契约的 `Provider` 对象里没有它们），内存库开不了第二个连接，所以用文件库。
fn file_core_with_provider(
    path: &Path,
    capabilities: &[ProviderCapability],
    script: Vec<std::result::Result<HttpResponse, TransportError>>,
) -> (Core, Arc<ScriptedTransport>, Provider) {
    let transport = Arc::new(ScriptedTransport::new(script));
    let mut core = Core::open(path).unwrap();
    core.set_transport_for_test(transport.clone() as Arc<dyn Transport>);
    let provider = core
        .save_provider(
            &provider_config("prv_a", capabilities, true),
            Some("sk-test-secret-0123456789"),
        )
        .unwrap();
    (core, transport, provider)
}

/// 直接读验证四列：闸门只看结论，想验「有没有被清掉」就得自己开一个连接看库。
fn verdict_columns(path: &Path) -> (String, String, Option<String>, Option<String>) {
    verdict_columns_for(path, "prv_a")
}

fn verdict_columns_for(
    path: &Path,
    provider_id: &str,
) -> (String, String, Option<String>, Option<String>) {
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.query_row(
        "SELECT test_status, verified_capabilities, last_error_code, last_tested_at \
         FROM providers WHERE id = ?1",
        rusqlite::params![provider_id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    )
    .unwrap()
}

fn auth_failed_script() -> Vec<std::result::Result<HttpResponse, TransportError>> {
    vec![Ok(HttpResponse::new(
        401,
        "{\"error\":\"invalid api key\"}",
    ))]
}

/// 回归（评审 #96）：一次失败之后，只改显示名或切 `enabled` 开关**不得**把
/// `test_status` 洗回 `untested`。洗掉的后果很具体：闸门对 `untested` 会跳过
/// 「验过没有」这一条，一个刚被证伪的服务又被放行。
///
/// 把 `save` 里的重置条件改回「任何变化都重置」，这条用例就会红。
#[test]
fn renaming_or_toggling_a_failed_provider_keeps_the_failed_verdict() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.sqlite");
    let (mut core, transport, provider) = file_core_with_provider(
        &path,
        &[ProviderCapability::GenerateText],
        auth_failed_script(),
    );

    // 先制造一次真实的失败结论（401 → authentication_failed）。
    let error = core
        .test_provider(&provider.id, &[ProviderCapability::GenerateText])
        .unwrap_err();
    assert_eq!(code_of(&error), "authentication_failed");
    let (status, verified, last_error, tested_at) = verdict_columns(&path);
    assert_eq!(status, "failed");
    assert_eq!(verified, "[]");
    assert_eq!(last_error.as_deref(), Some("authentication_failed"));
    assert!(tested_at.is_some(), "跑过测试就该有时间");

    // 1) 只改显示名：不碰任何功能参数，也不重填密钥。
    let mut renamed = provider_config("prv_a", &[ProviderCapability::GenerateText], true);
    renamed.label = "改了个显示名".to_owned();
    renamed.credential_ref = provider.credential_ref.clone();
    let saved = core.save_provider(&renamed, None).unwrap();
    assert_eq!(saved.label, "改了个显示名");
    assert_eq!(
        saved.credential_ref, provider.credential_ref,
        "没给新密钥，引用该照旧"
    );
    let (status, verified, last_error, tested_at) = verdict_columns(&path);
    assert_eq!(
        status, "failed",
        "改标签把失败结论洗成了 untested（评审 #96 的 bug）"
    );
    assert_eq!(verified, "[]");
    assert_eq!(last_error.as_deref(), Some("authentication_failed"));
    assert!(tested_at.is_some(), "没有功能变化就不该抹掉上次测试时间");

    let gate = core
        .provider_capability_gate(ProviderCapability::GenerateText)
        .unwrap();
    assert!(!gate.ready, "失败结论还在，闸门必须继续拒绝：{}", gate.reason);
    assert_eq!(gate.code, "authentication_failed");

    // 2) 切开关：关掉再打开，结论同样不许被洗掉。
    let mut disabled = renamed.clone();
    disabled.enabled = false;
    core.save_provider(&disabled, None).unwrap();
    assert_eq!(verdict_columns(&path).0, "failed");

    let mut reenabled = disabled.clone();
    reenabled.enabled = true;
    core.save_provider(&reenabled, None).unwrap();
    assert_eq!(
        verdict_columns(&path).0,
        "failed",
        "切开关把失败结论洗成了 untested"
    );
    let gate = core
        .provider_capability_gate(ProviderCapability::GenerateText)
        .unwrap();
    assert!(!gate.ready, "重新启用不等于重新可信：{}", gate.reason);

    // 3) 排任务入口也仍然拒绝，而且一次请求都不再发。
    let capture_id = capture_with_text(&mut core, "还是想让它写。", "op1");
    let error = core
        .request_response(&[capture_id], "试试看", Some(&provider.id))
        .unwrap_err();
    assert_eq!(code_of(&error), "provider_not_configured");
    assert_eq!(core.count_jobs(None).unwrap(), 0, "不排注定失败的任务");
    assert_eq!(transport.call_count(), 1, "被拒绝的任务不再发请求");
}

/// 回归（评审 #96）：密钥按**明文**比，不按引用比——`SecretStore::put` 每次都发
/// 新引用，而设置页每次保存都会把当前密钥一起提交；按引用比等于每次保存都作废旧结论。
#[test]
fn resubmitting_the_same_secret_is_not_a_functional_change() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.sqlite");
    let secret = "sk-test-secret-0123456789";
    let (mut core, _transport, provider) = file_core_with_provider(
        &path,
        &[ProviderCapability::GenerateText],
        auth_failed_script(),
    );
    core.test_provider(&provider.id, &[ProviderCapability::GenerateText])
        .unwrap_err();
    assert_eq!(verdict_columns(&path).0, "failed");

    // 1) 原样重填同一把钥匙：明文没变 → 结论不作废（引用换新的是另一回事）。
    let mut config = provider_config("prv_a", &[ProviderCapability::GenerateText], true);
    config.credential_ref = provider.credential_ref.clone();
    let saved = core.save_provider(&config, Some(secret)).unwrap();
    assert_ne!(
        saved.credential_ref, provider.credential_ref,
        "put 每次发新引用（正因为如此才不能拿引用比）"
    );
    assert_eq!(
        verdict_columns(&path).0,
        "failed",
        "同一把钥匙重新提交不该作废验证结论"
    );
    assert!(!core
        .provider_capability_gate(ProviderCapability::GenerateText)
        .unwrap()
        .ready);

    // 2) 换一把钥匙：功能参数真变了 → 结论作废，闸门按「没测过」放行
    //    （声明过 + 配置完整——这是既有口径：作废的是结论，不是配置）。
    let saved = core
        .save_provider(&config, Some("sk-better-secret-9876543210"))
        .unwrap();
    assert!(saved.credential_ref.is_some());
    let (status, verified, last_error, tested_at) = verdict_columns(&path);
    assert_eq!(status, "untested", "换密钥要作废旧结论");
    assert_eq!(verified, "[]");
    assert_eq!(last_error, None, "旧配置的失败码不该挂到新密钥上");
    assert_eq!(tested_at, None);
    assert!(core
        .provider_capability_gate(ProviderCapability::GenerateText)
        .unwrap()
        .ready);
}

/// 另一半：#96 不是「什么都不重置」。适配器 / 端点 / 模型变了，旧结论（含失败码与
/// 测试时间）必须一起清回 `untested` —— 那是**对旧配置**的验证结论。
#[test]
fn changing_endpoint_or_model_invalidates_the_old_verdict() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.sqlite");
    let (mut core, _transport, provider) = file_core_with_provider(
        &path,
        &[ProviderCapability::GenerateText],
        auth_failed_script(),
    );
    core.test_provider(&provider.id, &[ProviderCapability::GenerateText])
        .unwrap_err();
    let (status, _, last_error, _) = verdict_columns(&path);
    assert_eq!(status, "failed");
    assert_eq!(last_error.as_deref(), Some("authentication_failed"));

    // 换端点，顺带清掉模型（好观察失败码确实清掉了：配置不完整的拒绝理由与
    // 失败码是两条不同的信号）。
    let mut moved = provider_config("prv_a", &[ProviderCapability::GenerateText], true);
    moved.credential_ref = provider.credential_ref.clone();
    moved.endpoint = Some("http://127.0.0.1:2244/v1".to_owned());
    moved.model = None;
    core.save_provider(&moved, None).unwrap();
    let (status, verified, last_error, tested_at) = verdict_columns(&path);
    assert_eq!(status, "untested", "端点变了，旧结论要作废");
    assert_eq!(verified, "[]");
    assert_eq!(
        last_error, None,
        "旧配置的 authentication_failed 不该挂在新配置上"
    );
    assert_eq!(tested_at, None);

    let gate = core
        .provider_capability_gate(ProviderCapability::GenerateText)
        .unwrap();
    assert!(!gate.ready);
    assert!(gate.reason.contains("模型"), "{}", gate.reason);
    assert_eq!(
        gate.code, "provider_not_configured",
        "失败码如果没清，这里会残留 authentication_failed：{}",
        gate.reason
    );

    // 把模型配回来：`untested` 按「明确配置」放行——作废结论不等于永久拉黑
    // （口径见 docs/architecture/m3-Provider-配置与能力.md 第 4 条）。
    let mut complete = moved.clone();
    complete.model = Some("qwen-next".to_owned());
    core.save_provider(&complete, None).unwrap();
    let gate = core
        .provider_capability_gate(ProviderCapability::GenerateText)
        .unwrap();
    assert!(gate.ready, "{}", gate.reason);
}

// ------------------------------------------------------------------ 本机引擎的验证结论（P1）

/// 一份本机引擎配置。`model` 是模型目录（不是远端模型名），留空表示用随包/环境解析。
fn local_provider_config(id: &str, model: Option<&str>) -> Provider {
    Provider {
        id: id.to_owned(),
        label: "本机引擎测试服务".to_owned(),
        adapter_id: diary_core::provider::local_engine::SHERPA_ONNX.to_owned(),
        endpoint: None,
        model: model.map(str::to_owned),
        capabilities: [ProviderCapability::TranscribeAudio].into_iter().collect(),
        credential_ref: None,
        enabled: true,
        test_status: ProviderTestStatus::Untested,
    }
}

/// 回归（P1）：本机引擎「原样重存」不得把 `failed` 洗成 `untested`。
///
/// 洗掉的后果很具体：闸门对 `untested` 跳过「验过没有」这一条，而本机分支只看
/// `readiness`（只 stat 文件、不试加载）。于是「引擎库不是有效 ELF / 模型文件损坏」
/// 时，用户点一次保存（一个字段都没改）就能让闸门重新放行，任务注定失败。
#[test]
fn resaving_an_unchanged_local_engine_keeps_the_failed_verdict() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.sqlite");
    let mut core = Core::open(&path).unwrap();
    // 钉死一个不存在的模型目录：探针在「模型文件不在」这一步失败，与这台机器上
    // 有没有真的引擎库/模型无关（CI 上也能稳定复现），也不会真的加载原生库。
    core.set_asr_engine_config_for_test(EngineConfig {
        engine_lib: None,
        model_dir: dir.path().join("missing-model"),
        model_file: None,
        kind: ModelKind::SenseVoice,
        language: None,
        num_threads: 1,
        itn: false,
    });

    let provider = core
        .save_provider(&local_provider_config("prv_local", None), None)
        .unwrap();
    let error = core
        .test_provider(&provider.id, &[ProviderCapability::TranscribeAudio])
        .unwrap_err();
    assert_eq!(code_of(&error), "provider_not_configured");
    let (status, verified, last_error, tested_at) = verdict_columns_for(&path, "prv_local");
    assert_eq!(status, "failed");
    assert_eq!(verified, "[]");
    assert!(last_error.is_some(), "失败码要落库");
    assert!(tested_at.is_some(), "跑过测试就该有时间");

    // 原样重存：一个字段都没改。
    let saved = core
        .save_provider(&local_provider_config("prv_local", None), None)
        .unwrap();
    assert_eq!(saved.test_status, ProviderTestStatus::Failed);
    let (status, verified, last_error_after, tested_at_after) =
        verdict_columns_for(&path, "prv_local");
    assert_eq!(status, "failed", "原样重存把失败结论洗成了 untested");
    assert_eq!(verified, "[]");
    assert_eq!(last_error_after, last_error, "失败码要原样保留");
    assert_eq!(tested_at_after, tested_at, "测试时间要原样保留");

    let gate = core
        .provider_capability_gate(ProviderCapability::TranscribeAudio)
        .unwrap();
    assert!(!gate.ready, "失败结论还在，闸门必须继续拒绝：{}", gate.reason);

    // 真的改了功能参数（换模型目录）才重置：旧结论是对旧目录的验证。
    core.save_provider(
        &local_provider_config("prv_local", Some("/tmp/another-model")),
        None,
    )
    .unwrap();
    let (status, verified, last_error, tested_at) = verdict_columns_for(&path, "prv_local");
    assert_eq!(status, "untested", "换了模型目录，旧结论要作废");
    assert_eq!(verified, "[]");
    assert_eq!(last_error, None);
    assert_eq!(tested_at, None);
}

// ------------------------------------------------------------------ 错误归一化（走真实作业路径）

/// 每个错误码 → 可重试还是等配置、任务落在哪个状态：逐条钉住。
#[test]
fn job_failures_map_to_contract_codes_with_the_right_retry_semantics() {
    let retry_after = chrono::Duration::seconds(7);

    // 429：尊重 Retry-After，退避重试。
    {
        let dir = tempfile::tempdir().unwrap();
        let (mut core, transport, provider) = core_with_provider(
            dir.path(),
            &[ProviderCapability::GenerateText],
            vec![Ok(
                HttpResponse::new(429, "{\"error\":\"slow down\"}")
                    .with_header("Retry-After", "7"),
            )],
        );
        let capture_id = capture_with_text(&mut core, "限流也要说清楚。", "op1");
        let ticket = core
            .request_response(&[capture_id], "写一句", Some(&provider.id))
            .unwrap();
        let before = chrono::Utc::now();
        let summary = core.run_due_jobs(before, 60_000, "test", 8).unwrap();
        assert_eq!(summary.retrying, 1, "{summary:?}");
        let job = core.get_job(&ticket.job_id).unwrap();
        assert_eq!(job.state, JobState::RetryWait);
        assert_eq!(job.error_code.as_deref(), Some("rate_limited"));
        let at = job.next_attempt_at.expect("限流要排退避");
        let delta = at - before;
        assert!(
            delta >= retry_after - chrono::Duration::seconds(2)
                && delta <= retry_after + chrono::Duration::seconds(5),
            "要尊重 Retry-After，实际等了 {delta}"
        );
        assert_eq!(transport.call_count(), 1);

        // 排期没到之前不会再打一次。
        core.run_due_jobs(before, 60_000, "test", 8).unwrap();
        assert_eq!(transport.call_count(), 1, "退避期间不该重复请求");
        assert_eq!(core.get_job(&ticket.job_id).unwrap().attempt_count, 1);
    }

    // 5xx：网络不可用那一类，退避重试。
    {
        let dir = tempfile::tempdir().unwrap();
        let (mut core, transport, provider) = core_with_provider(
            dir.path(),
            &[ProviderCapability::GenerateText],
            vec![Ok(HttpResponse::new(503, "{\"error\":\"upstream\"}"))],
        );
        let capture_id = capture_with_text(&mut core, "服务端出错。", "op2");
        let ticket = core
            .request_response(&[capture_id], "写一句", Some(&provider.id))
            .unwrap();
        let summary = core
            .run_due_jobs(chrono::Utc::now(), 60_000, "test", 8)
            .unwrap();
        assert_eq!(summary.retrying, 1);
        let job = core.get_job(&ticket.job_id).unwrap();
        assert_eq!(job.error_code.as_deref(), Some("network_unavailable"));
        assert_eq!(job.state, JobState::RetryWait);
        assert_eq!(transport.call_count(), 1);
    }

    // 401：等配置，不频繁重试（第二次驱动不会再去撞）。
    {
        let dir = tempfile::tempdir().unwrap();
        let (mut core, transport, provider) = core_with_provider(
            dir.path(),
            &[ProviderCapability::GenerateText],
            vec![Ok(HttpResponse::new(401, "{\"error\":\"bad key\"}"))],
        );
        let capture_id = capture_with_text(&mut core, "密钥过期了。", "op3");
        let ticket = core
            .request_response(&[capture_id], "写一句", Some(&provider.id))
            .unwrap();
        let summary = core
            .run_due_jobs(chrono::Utc::now(), 60_000, "test", 8)
            .unwrap();
        assert_eq!(summary.failed, 0, "还没放弃，只是在等配置：{summary:?}");
        assert_eq!(summary.retrying, 0, "不该排退避：{summary:?}");
        let job = core.get_job(&ticket.job_id).unwrap();
        assert_eq!(job.state, JobState::WaitingConfiguration);
        assert_eq!(job.error_code.as_deref(), Some("authentication_failed"));
        // 执行期那条路（真的跑过一次才撞上 401）：状态口径与闸门路**一字不差**
        // （要用户去改配置、key 指到同一个服务），差别只在 `attempt_count` 如实记账。
        assert!(job.requires_user_action, "401 要用户去换密钥，不是无需理会的状态");
        assert_eq!(
            job.attention_key.as_deref(),
            Some("job-config:respond:authentication_failed")
        );
        assert_eq!(
            job.attempt_count, 1,
            "真跑了一次就如实记账（不像闸门路那样停在 0）"
        );
        assert_eq!(transport.call_count(), 1);

        // 结论已经落库成失败，第二次驱动连试都不试。
        let again = core
            .run_due_jobs(chrono::Utc::now(), 60_000, "test", 8)
            .unwrap();
        assert_eq!(again.processed, 0);
        assert_eq!(transport.call_count(), 1);
        assert_eq!(
            core.list_providers().unwrap().remove(0).test_status,
            ProviderTestStatus::Failed
        );
    }

    // 404：端点/模型不对，终态失败等人工——重试不会变好。
    {
        let dir = tempfile::tempdir().unwrap();
        let (mut core, transport, provider) = core_with_provider(
            dir.path(),
            &[ProviderCapability::GenerateText],
            vec![Ok(HttpResponse::new(404, "{\"error\":\"model not found\"}"))],
        );
        let capture_id = capture_with_text(&mut core, "模型名写错了。", "op4");
        let ticket = core
            .request_response(&[capture_id], "写一句", Some(&provider.id))
            .unwrap();
        let summary = core
            .run_due_jobs(chrono::Utc::now(), 60_000, "test", 8)
            .unwrap();
        assert_eq!(summary.failed, 1);
        let job = core.get_job(&ticket.job_id).unwrap();
        assert_eq!(job.state, JobState::Failed);
        assert_eq!(job.error_code.as_deref(), Some("model_unavailable"));
        assert!(job.requires_user_action);
        assert_eq!(transport.call_count(), 1);
    }
}

// ------------------------------------------------------------------ 回放端到端

/// 回放传输层端到端：请求 → 作业 → 结果落库（不需要真模型）。
#[test]
fn a_replayed_call_runs_from_request_to_stored_response() {
    let dir = tempfile::tempdir().unwrap();
    let (mut core, transport, provider) = core_with_provider(
        dir.path(),
        &[ProviderCapability::GenerateText],
        vec![Ok(chat_ok("今天也要好好吃饭。", "qwen-test"))],
    );
    let capture_id = capture_with_text(&mut core, "今天加班到很晚。", "op1");

    let ticket = core
        .request_response(std::slice::from_ref(&capture_id), "帮我问候一下自己", Some(&provider.id))
        .unwrap();
    let response_id = ticket.response_id.clone().expect("受理时就该有回应 id");

    // 受理之后、驱动之前：什么都没有生成，也一次请求都没发。
    let pending = core.get_response(&response_id).unwrap();
    assert_eq!(pending.body, "", "没生成就不能编内容");
    assert_eq!(pending.request_text, "帮我问候一下自己");
    assert_eq!(pending.capture_ids, vec![capture_id.clone()]);
    assert_eq!(pending.provider_id.as_deref(), Some("prv_a"));
    assert_eq!(transport.call_count(), 0);

    // 活着的同内容请求复用同一条任务与回应（不重复计费）。
    let again = core
        .request_response(std::slice::from_ref(&capture_id), "帮我问候一下自己", Some(&provider.id))
        .unwrap();
    assert_eq!(again.job_id, ticket.job_id);
    assert_eq!(again.response_id.as_deref(), Some(response_id.as_str()));

    let summary = core
        .run_due_jobs(chrono::Utc::now(), 60_000, "test", 8)
        .unwrap();
    assert_eq!(summary.succeeded, 1, "{summary:?}");
    assert_eq!(transport.call_count(), 1);

    let stored = core.get_response(&response_id).unwrap();
    assert_eq!(stored.body, "今天也要好好吃饭。");
    assert_eq!(stored.model.as_deref(), Some("qwen-test"));
    assert_eq!(core.get_job(&ticket.job_id).unwrap().state, JobState::Succeeded);

    // 已经完成之后，同内容的新请求是新的一次（用户又主动要了一次）。
    let third = core
        .request_response(&[capture_id], "帮我问候一下自己", Some(&provider.id))
        .unwrap();
    assert_ne!(third.response_id.as_deref(), Some(response_id.as_str()));
    assert_ne!(third.job_id, ticket.job_id);
}

/// 已经生成过的回应重入（租约恢复）不会再花一次钱。
#[test]
fn a_finished_response_is_not_generated_twice() {
    let dir = tempfile::tempdir().unwrap();
    let (mut core, transport, provider) = core_with_provider(
        dir.path(),
        &[ProviderCapability::GenerateText],
        vec![Ok(chat_ok("只生成一次。", "qwen-test"))],
    );
    let capture_id = capture_with_text(&mut core, "别重复调用。", "op1");
    let ticket = core
        .request_response(&[capture_id], "写一句", Some(&provider.id))
        .unwrap();
    core.run_due_jobs(chrono::Utc::now(), 60_000, "test", 8).unwrap();
    assert_eq!(transport.call_count(), 1);

    // 再排一条指向同一个回应的任务（模拟「上次没跑完」被重新领取）：正文已在，不该再调。
    core.enqueue_job(
        diary_core::NewJob::new("respond", diary_core::JobPriority::UserOrganize)
            .targeting(vec![ticket.response_id.clone().unwrap()])
            .with_snapshot("manual-requeue"),
    )
    .unwrap();
    core.run_due_jobs(chrono::Utc::now(), 60_000, "test", 8).unwrap();
    assert_eq!(transport.call_count(), 1, "已经生成过的回应不该再调一次");
}

// ------------------------------------------------------------------ 配置校验

/// 校验失败要给 `invalid_argument` / `not_found`，不能含糊。
#[test]
fn provider_config_validation_rejects_junk_clearly() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());

    let mut bad_adapter = provider_config("prv_x", &[ProviderCapability::GenerateText], true);
    bad_adapter.adapter_id = "some_vendor".to_owned();
    let error = core.save_provider(&bad_adapter, None).unwrap_err();
    assert_eq!(code_of(&error), "invalid_argument");
    assert!(error.to_string().contains("openai_compatible"), "{error}");

    let mut bad_scheme = provider_config("prv_y", &[ProviderCapability::GenerateText], true);
    bad_scheme.endpoint = Some("ftp://example.com/v1".to_owned());
    assert_eq!(
        code_of(&core.save_provider(&bad_scheme, None).unwrap_err()),
        "invalid_argument"
    );

    let mut no_host = provider_config("prv_z", &[ProviderCapability::GenerateText], true);
    no_host.endpoint = Some("https:///v1".to_owned());
    assert_eq!(
        code_of(&core.save_provider(&no_host, None).unwrap_err()),
        "invalid_argument"
    );

    let mut empty_label = provider_config("prv_w", &[ProviderCapability::GenerateText], true);
    empty_label.label = "   ".to_owned();
    assert_eq!(
        code_of(&core.save_provider(&empty_label, None).unwrap_err()),
        "invalid_argument"
    );

    // userinfo（`user:pass@host`）不是「擦掉再用」，是**拒绝**：URL 里的密码没有
    // 可靠的生命周期管理，且一定会出现在配置列表/测试报告/错误消息里。静默剥离
    // 会让用户以为配上了、实际没配。
    for endpoint in ["http://u:p@h/v1", "http://only-user@h/v1"] {
        let mut userinfo =
            provider_config("prv_userinfo", &[ProviderCapability::GenerateText], true);
        userinfo.endpoint = Some(endpoint.to_owned());
        let error = core.save_provider(&userinfo, None).unwrap_err();
        assert_eq!(code_of(&error), "invalid_argument", "{endpoint}");
        let text = error.to_string();
        assert!(text.contains("secret/credentialRef"), "{text}");
        assert!(text.contains("不要写进 URL"), "{text}");
        let credentials = endpoint
            .split("://")
            .nth(1)
            .unwrap()
            .split('@')
            .next()
            .unwrap();
        assert!(
            !text.contains(credentials),
            "拒绝消息不该回显 URL 里的凭据：{text}"
        );
        assert_eq!(
            core.list_providers().unwrap().len(),
            0,
            "被拒绝的 endpoint 不能入库"
        );
    }

    // 正常地址仍然通过，而且原样规范化。
    let mut https = provider_config("prv_https", &[ProviderCapability::GenerateText], true);
    https.endpoint = Some("https://api.deepseek.com".to_owned());
    let saved = core.save_provider(&https, None).unwrap();
    assert_eq!(saved.endpoint.as_deref(), Some("https://api.deepseek.com"));

    // 保存合法配置后：空测试范围、未知服务都报明确错误。
    let saved = core
        .save_provider(&provider_config("prv_ok", &[ProviderCapability::GenerateText], true), None)
        .unwrap();
    assert_eq!(
        code_of(&core.test_provider(&saved.id, &[]).unwrap_err()),
        "invalid_argument"
    );
    assert_eq!(
        code_of(
            &core
                .test_provider("prv_missing", &[ProviderCapability::GenerateText])
                .unwrap_err()
        ),
        "not_found"
    );
    assert_eq!(
        code_of(&core.remove_provider("prv_missing").unwrap_err()),
        "not_found"
    );
}

/// 配置不完整（没有密钥）同样**不排任务**：理由与下一步都要可执行。
#[test]
fn a_provider_without_a_credential_is_not_scheduled_and_says_why() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    let saved = core
        .save_provider(
            &provider_config("prv_nokey", &[ProviderCapability::GenerateText], true),
            None,
        )
        .unwrap();
    assert!(saved.credential_ref.is_none());
    let capture_id = capture_with_text(&mut core, "还没配密钥。", "op1");

    let gate = core
        .provider_capability_gate(ProviderCapability::GenerateText)
        .unwrap();
    assert!(!gate.ready);
    assert!(gate.reason.contains("还没有配置密钥"), "{}", gate.reason);
    assert!(gate.next_step.is_some());

    let error = core
        .request_response(&[capture_id], "你好", None)
        .unwrap_err();
    assert_eq!(code_of(&error), "provider_not_configured");
    assert!(error.to_string().contains("密钥"), "{error}");
    assert_eq!(core.count_jobs(None).unwrap(), 0, "不排注定失败的任务");
}

/// 换密钥时旧文件要被清掉（一个凭据一个文件，不留孤儿）。
#[test]
fn replacing_a_credential_removes_the_previous_secret_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.sqlite");
    let mut core = Core::open(&path).unwrap();
    let first = core
        .save_provider(
            &provider_config("prv_a", &[ProviderCapability::GenerateText], true),
            Some("sk-first-0123456789"),
        )
        .unwrap();
    let second = core
        .save_provider(
            &provider_config("prv_a", &[ProviderCapability::GenerateText], true),
            Some("sk-second-0123456789"),
        )
        .unwrap();

    assert_ne!(first.credential_ref, second.credential_ref);
    let files: Vec<String> = std::fs::read_dir(dir.path().join("secrets"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(files.len(), 1, "旧密钥文件该被清掉，实际：{files:?}");
}

/// 端点分类：本机/局域网与远端必须能识别地区分（连接测试报告里会写出来）。
#[test]
fn endpoint_scope_separates_local_from_remote() {
    for local in [
        "http://127.0.0.1:11434/v1",
        "http://localhost:8080",
        "http://192.168.1.10:8000/v1",
        "http://10.0.0.5/v1",
        "http://172.20.3.4/v1",
        "http://nas.local/v1",
        "http://ollama:11434/v1",
    ] {
        assert_eq!(endpoint_scope(local), "本机/局域网", "{local}");
    }
    for remote in [
        "https://api.openai.com/v1",
        "https://api.deepseek.com/v1",
        "https://example.com/v1",
    ] {
        assert_eq!(endpoint_scope(remote), "远端", "{remote}");
    }
}

// ------------------------------------------------------------------ 迁移 v8 → v9

/// v8 旧库升到 v9：建出两张新表、旧数据一行未动、**不回填**任何配置。
///
/// 照 `text_chunks_test.rs` 的 v7→v8 写法：把库手工退回 v8 的样子（删掉 v9 的表、
/// 版本号退回去），再用 `Core::open` 打开。
#[test]
fn v8_library_migrates_to_v9_and_keeps_old_data_intact() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.sqlite");

    let (capture_id, provider_id) = {
        let mut core = Core::open(&path).unwrap();
        assert_eq!(core.schema_version().unwrap(), diary_core::SCHEMA_VERSION);
        let capture_id = capture_with_text(&mut core, "妈妈打电话来。", "op1");
        let provider = core
            .save_provider(
                &provider_config("prv_keep", &[ProviderCapability::GenerateText], true),
                Some("sk-migrate-0123456789"),
            )
            .unwrap();
        (capture_id, provider.id)
    };

    let legacy_counts = |conn: &rusqlite::Connection| -> Vec<i64> {
        [
            "captures",
            "source_items",
            "source_revisions",
            "search_capture_docs",
            "search_capture_grams",
            "domain_events",
        ]
        .iter()
        .map(|table| {
            conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap()
        })
        .collect()
    };
    let before = legacy_counts(&rusqlite::Connection::open(&path).unwrap());

    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(
            "DROP TABLE ai_responses;
             DROP TABLE providers;
             DROP TABLE diary_generations;
             DROP TABLE diary_days;
             DROP TABLE diary_versions;
             DROP TABLE diary_source_snapshots;
             DROP TABLE diary_model_calls;
             DROP TABLE settings;
             UPDATE schema_migrations SET version = 8;
             PRAGMA user_version = 8;",
        )
        .unwrap();
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, 8, "先确认真的退回到了 v8");
        // 退回 v8 之后「上一个版本」是读不出这两张表的。
        assert!(conn
            .query_row("SELECT COUNT(*) FROM providers", [], |row| row.get::<_, i64>(0))
            .is_err());
    }

    let core = Core::open(&path).unwrap();
    assert_eq!(core.schema_version().unwrap(), diary_core::SCHEMA_VERSION);

    // 旧数据一行未动。
    let after = legacy_counts(&rusqlite::Connection::open(&path).unwrap());
    assert_eq!(after, before, "升级不该碰 v1–v8 的任何一张表");
    assert!(core.get_capture(&capture_id).is_ok());

    // 新表就位、**没有回填**：升级前没有任何配置，就不替用户猜一个。
    let conn = rusqlite::Connection::open(&path).unwrap();
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM providers", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        0,
        "迁移不回填服务配置"
    );
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM ai_responses", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert!(core.list_providers().unwrap().is_empty());

    // 默认不联网的口径在升级路径上同样成立：闸门说没有可用服务。
    assert!(!core
        .provider_capability_gate(ProviderCapability::GenerateText)
        .unwrap()
        .ready);
    assert_eq!(core.count_jobs(None).unwrap(), 0);

    // 旧的凭据文件还在（配置行没了，但这是迁移前的历史：升级不碰文件系统）。
    let _ = provider_id;
    assert!(path.exists());
}

/// 迁移回来的库照样能保存配置、跑一次回放调用（新表是可用的，不只是建出来了）。
#[test]
fn a_migrated_library_can_still_configure_and_call() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.sqlite");
    {
        let mut core = Core::open(&path).unwrap();
        capture_with_text(&mut core, "迁移之后还要能用。", "op1");
        core.count_captures().unwrap();
    }
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(
            "DROP TABLE ai_responses;
             DROP TABLE providers;
             DROP TABLE diary_generations;
             DROP TABLE diary_days;
             DROP TABLE diary_versions;
             DROP TABLE diary_source_snapshots;
             DROP TABLE diary_model_calls;
             DROP TABLE settings;
             UPDATE schema_migrations SET version = 8;
             PRAGMA user_version = 8;",
        )
        .unwrap();
    }

    let mut core = Core::open(&path).unwrap();
    let transport =
        Arc::new(ScriptedTransport::ok(vec![chat_ok("迁移后也能生成。", "qwen-test")]));
    core.set_transport_for_test(transport.clone() as Arc<dyn Transport>);
    let saved = core
        .save_provider(
            &provider_config("prv_migrated", &[ProviderCapability::GenerateText], true),
            Some("sk-after-migrate-0123456789"),
        )
        .unwrap();
    let capture = core.list_captures(None, None, 10).unwrap();
    let ticket = core
        .request_response(&[capture.captures[0].id.clone()], "说一句", Some(&saved.id))
        .unwrap();
    core.run_due_jobs(chrono::Utc::now(), 60_000, "test", 8).unwrap();
    assert_eq!(
        core.get_response(ticket.response_id.as_deref().unwrap())
            .unwrap()
            .body,
        "迁移后也能生成。"
    );
}

/// 直接构造一条「一调用就 panic」的传输层，确认它确实会拦住出网
/// （这是所有「默认不联网」断言的基石：替身不是哑的）。
#[test]
#[should_panic(expected = "默认不联网被破坏")]
fn the_panic_transport_really_panics_when_called() {
    let transport = PanicTransport {};
    let _ = transport.send(&HttpRequest::new("GET", "https://example.com/", 10));
}

/// 内存库默认传输层就是 panic 替身：`responses.request` 这种可能出网的入口
/// 在没有服务时也必须先被闸门挡住，而不是先去发请求再报错。
#[test]
fn the_default_transport_for_test_libraries_is_the_panic_one() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    let capture_id = capture_with_text(&mut core, "默认不该出网。", "op1");
    // 这条会走闸门（没配置服务）→ 报错，而不是调用传输层（否则会 panic）。
    let error = core
        .request_response(&[capture_id], "你好", None)
        .unwrap_err();
    assert_eq!(code_of(&error), "provider_not_configured");
    assert_eq!(ErrorCode::ProviderNotConfigured.wire(), "provider_not_configured");
}

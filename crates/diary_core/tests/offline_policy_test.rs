#![cfg(feature = "offline-only")]

use std::collections::HashSet;
use std::sync::Arc;
use diary_core::{Core, HttpRequest, PanicTransport, Provider, ProviderCapability, ProviderTestStatus, Transport};
use diary_core::provider::transport::UreqTransport;

#[test]
fn offline_transport_rejects_before_parsing_or_connecting() {
    let transport = UreqTransport::new();
    let error = transport.send(&HttpRequest::new("GET", "this is not a URL", 1)).unwrap_err();
    assert!(error.to_string().contains("离线版不支持网络请求"));
}

#[test]
fn offline_product_rejects_remote_configuration_and_keeps_local_generation_separate_from_asr() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = Core::open(&dir.path().join("library.sqlite")).unwrap();
    core.set_transport_for_test(Arc::new(PanicTransport {}));
    let mut config = Provider {
        id: "offline-generator".to_owned(), label: "本机整理".to_owned(),
        adapter_id: "openai_compatible".to_owned(), endpoint: Some("https://example.invalid/v1".to_owned()),
        model: Some("model".to_owned()), capabilities: HashSet::from([ProviderCapability::GenerateText]),
        credential_ref: None, enabled: true, test_status: ProviderTestStatus::Untested,
    };
    assert!(core.save_provider(&config, Some("test-only-key")).is_err());
    assert!(core.list_providers().unwrap().is_empty());
    config.adapter_id = "llama_cpp".to_owned();
    config.endpoint = None;
    config.model = Some(dir.path().join("missing-model.gguf").to_string_lossy().into_owned());
    core.save_provider(&config, None).unwrap();
    core.select_generation_provider(&config.id).unwrap();
    assert!(!core.provider_capability_gate(ProviderCapability::GenerateText).unwrap().ready);
    assert!(core.list_providers().unwrap()[0].credential_ref.is_none());
}

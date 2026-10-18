//! `core-config.json`（设备配置文件）的集成测试：位置、优先级、失败不致命。
//!
//! 环境变量相关的用例在 `core_config_env_test.rs`：设环境变量是**进程级**的，
//! 与同一进程里并行跑的其它用例会互相踩，所以单独一个测试二进制（独立进程）。

use std::path::{Path, PathBuf};

use diary_core::{
    Core, Provider, ProviderCapability, ProviderTestStatus, CONFIG_FILE_NAME,
};

/// 这些变量一旦被外部设置，本文件的「没有配置」类断言就不成立——直接跳过，
/// 不把量测环境误判成失败。
const ENV_KEYS: [&str; 4] = [
    "DIARY_MODEL_DIR",
    "DIARY_ORT_DYLIB",
    "DIARY_ASR_MODEL_DIR",
    "DIARY_ASR_ENGINE_LIB",
];

fn env_is_clean() -> bool {
    ENV_KEYS
        .iter()
        .all(|key| std::env::var_os(key).is_none())
}

fn write_file(path: &Path, bytes: &[u8]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

/// 造一份「文件都在」的配置：四档路径都指向临时目录里真实的（空壳）文件。
///
/// 只 stat、不加载——所以文件内容是什么不重要，存在就行。
struct Fixture {
    dir: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        Self {
            dir: tempfile::tempdir().unwrap(),
        }
    }

    fn root(&self) -> &Path {
        self.dir.path()
    }

    fn library(&self) -> PathBuf {
        self.root().join("library.sqlite")
    }

    fn config_path(&self) -> PathBuf {
        self.root().join(CONFIG_FILE_NAME)
    }

    /// 嵌入模型目录（`model_quantized.onnx` + `tokenizer.json`）。
    fn model_dir(&self, name: &str) -> PathBuf {
        let dir = self.root().join(name);
        write_file(&dir.join("model_quantized.onnx"), b"stub-onnx");
        write_file(&dir.join("tokenizer.json"), b"{}");
        dir
    }

    /// 转写模型目录（`model.int8.onnx` + `tokens.txt`）。
    fn asr_dir(&self, name: &str) -> PathBuf {
        let dir = self.root().join(name);
        write_file(&dir.join("model.int8.onnx"), b"stub-onnx");
        write_file(&dir.join("tokens.txt"), b"a 0\n");
        dir
    }

    fn file(&self, name: &str) -> PathBuf {
        let path = self.root().join(name);
        write_file(&path, b"stub-so");
        path
    }

    fn write_config(&self, value: serde_json::Value) {
        std::fs::write(self.config_path(), value.to_string()).unwrap();
    }

    fn open(&self) -> Core {
        Core::open(self.library()).unwrap()
    }
}

#[test]
fn without_the_file_nothing_changes() {
    if !env_is_clean() {
        return;
    }
    let fixture = Fixture::new();
    let core = fixture.open();
    let report = core.runtime_config();
    assert!(!report.file.present, "没有文件时 present 必须是 false");
    assert_eq!(report.file.path.as_deref(), Some(
        fixture.config_path().display().to_string().as_str()
    ));
    assert_eq!(report.semantic.state, "absent");
    assert_eq!(report.semantic.config_source, "absent");
    assert_eq!(report.semantic.model_dir_source, "absent");
    assert_eq!(report.transcription.config_source, "absent");
    assert_eq!(report.transcription.state, "absent");
    // 既有关键词能力不受影响：库照样能建表、能写。
    assert!(core.schema_version().unwrap() > 0);
    assert_eq!(core.count_captures().unwrap(), 0);
}

#[test]
fn file_configures_both_capabilities() {
    if !env_is_clean() {
        return;
    }
    let fixture = Fixture::new();
    let model_dir = fixture.model_dir("models/bge");
    let ort = fixture.file("libonnxruntime-diary.so");
    let asr = fixture.asr_dir("asr/sense-voice");
    let engine = fixture.file("libsherpa-onnx-c-api.so");
    fixture.write_config(serde_json::json!({
        "modelDir": model_dir,
        "ortDylib": ort,
        "asrEngineLib": engine,
        "asrModelDir": asr,
    }));

    let core = fixture.open();
    let report = core.runtime_config();
    assert!(report.file.present);
    assert_eq!(report.file.error, None);
    assert!(report.file.ignored_fields.is_empty());

    assert_eq!(report.semantic.state, "configured", "文件在、还没加载");
    assert_eq!(report.semantic.config_source, "file");
    assert_eq!(
        report.semantic.model_dir.as_deref(),
        Some(model_dir.display().to_string().as_str())
    );
    assert_eq!(report.semantic.model_dir_source, "file");
    assert_eq!(report.semantic.ort_dylib_source, "file");

    assert_eq!(report.transcription.state, "ready");
    assert_eq!(report.transcription.config_source, "file");
    assert_eq!(
        report.transcription.model_dir.as_deref(),
        Some(asr.display().to_string().as_str())
    );
    assert_eq!(report.transcription.model_dir_source, "file");
    assert_eq!(
        report.transcription.engine_lib.as_deref(),
        Some(engine.display().to_string().as_str())
    );
    assert_eq!(report.transcription.engine_lib_source, "file");
    assert_eq!(report.transcription.provider_id, None, "没有 provider 行时是内建引擎");
}

#[test]
fn broken_json_is_not_fatal_and_reported() {
    if !env_is_clean() {
        return;
    }
    let fixture = Fixture::new();
    std::fs::write(fixture.config_path(), b"{ not json").unwrap();
    let core = fixture.open();
    let report = core.runtime_config();
    assert!(report.file.present, "文件在那儿，只是内容不对");
    assert!(
        report.file.error.as_deref().unwrap_or("").contains("解析失败"),
        "要如实报出解析失败：{:?}",
        report.file.error
    );
    assert_eq!(report.semantic.state, "absent", "解析失败回落「没配」，不致命");
    assert_eq!(report.transcription.state, "absent");
}

#[test]
fn unknown_fields_are_reported_not_swallowed() {
    if !env_is_clean() {
        return;
    }
    let fixture = Fixture::new();
    let model_dir = fixture.model_dir("models/bge");
    let ort = fixture.file("libonnxruntime-diary.so");
    fixture.write_config(serde_json::json!({
        "modelDir": model_dir,
        "ortDylib": ort,
        // 拼错一个字段：不能被无声忽略，也不能让整份配置失效。
        "modelDirr": "/typo",
    }));

    let core = fixture.open();
    let report = core.runtime_config();
    assert_eq!(report.file.ignored_fields, vec!["modelDirr".to_owned()]);
    assert_eq!(report.semantic.state, "configured", "认识的字段照常生效");
    assert_eq!(
        report.semantic.model_dir.as_deref(),
        Some(model_dir.display().to_string().as_str())
    );
}

#[test]
fn per_field_sources_are_tracked_when_half_configured() {
    if !env_is_clean() {
        return;
    }
    let fixture = Fixture::new();
    let ort = fixture.file("libonnxruntime-diary.so");
    // 只给 ORT，不给模型目录：不完整，但**来源要如实**。
    fixture.write_config(serde_json::json!({ "ortDylib": ort }));

    let core = fixture.open();
    let report = core.runtime_config();
    assert_eq!(report.semantic.state, "invalid");
    assert_eq!(report.semantic.config_source, "file");
    assert_eq!(report.semantic.model_dir_source, "absent");
    assert_eq!(report.semantic.ort_dylib_source, "file");
    assert!(
        report
            .semantic
            .reason
            .as_deref()
            .unwrap_or("")
            .contains("没有模型目录"),
        "原因要说缺什么：{:?}",
        report.semantic.reason
    );
}

#[test]
fn missing_files_are_invalid_not_ready() {
    if !env_is_clean() {
        return;
    }
    let fixture = Fixture::new();
    // 配置里指向一个**不存在**的模型目录：不能假装「未就绪」把错误盖掉。
    fixture.write_config(serde_json::json!({
        "modelDir": fixture.root().join("nope/models"),
        "ortDylib": fixture.file("libonnxruntime-diary.so"),
    }));
    let core = fixture.open();
    let report = core.runtime_config();
    assert_eq!(report.semantic.state, "invalid");
    assert!(
        report
            .semantic
            .reason
            .as_deref()
            .unwrap_or("")
            .contains("找不到模型文件"),
        "原因要点名缺什么：{:?}",
        report.semantic.reason
    );
}

/// 解析顺序的钉子：**文件 → provider → 随包默认**（环境变量那一档见另一个测试文件）。
///
/// 为什么 provider 在文件后面：`core-config.json` 是产品级配置，`providers.model`
/// 是按服务的显式目录；`resolve_with` 的注释与 `docs/architecture/m3-转写链.md`
/// 都按这个顺序写。改顺序必须同时改测试与文档。
#[test]
fn file_beats_provider_for_the_asr_model_dir() {
    if !env_is_clean() {
        return;
    }
    let fixture = Fixture::new();
    let from_file = fixture.asr_dir("asr/file-model");
    let from_provider = fixture.asr_dir("asr/provider-model");
    fixture.write_config(serde_json::json!({ "asrModelDir": from_file }));

    let mut core = fixture.open();
    core.save_provider(
        &Provider {
            id: "local_asr_test".to_owned(),
            label: "测试本机转写".to_owned(),
            adapter_id: diary_core::SHERPA_ONNX.to_owned(),
            endpoint: None,
            model: Some(from_provider.display().to_string()),
            capabilities: [ProviderCapability::TranscribeAudio].into_iter().collect(),
            credential_ref: None,
            enabled: true,
            test_status: ProviderTestStatus::Untested,
        },
        None,
    )
    .unwrap();

    let report = core.runtime_config();
    assert_eq!(report.transcription.provider_id.as_deref(), Some("local_asr_test"));
    assert_eq!(report.transcription.config_source, "file");
    assert_eq!(
        report.transcription.model_dir.as_deref(),
        Some(from_file.display().to_string().as_str())
    );
}

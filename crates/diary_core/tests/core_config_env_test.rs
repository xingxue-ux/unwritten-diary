//! 「环境变量仍然优先」的钉子：设备上有 `core-config.json` 时，探针/CI 设的
//! `DIARY_*` 必须仍然赢。
//!
//! 单独一个测试二进制：环境变量是进程级的，和别的用例并行跑会互相踩。

use std::path::{Path, PathBuf};

use diary_core::{Core, CONFIG_FILE_NAME};

fn write_file(path: &Path, bytes: &[u8]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

/// 用例结束后把环境变量还原（包括「本来是设着的」那种情况）。
struct EnvGuard {
    saved: Vec<(&'static str, Option<std::ffi::OsString>)>,
}

impl EnvGuard {
    fn capture(keys: &[&'static str]) -> Self {
        Self {
            saved: keys
                .iter()
                .map(|key| (*key, std::env::var_os(key)))
                .collect(),
        }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (key, value) in &self.saved {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
    }
}

fn model_dir(root: &Path, name: &str) -> PathBuf {
    let dir = root.join(name);
    write_file(&dir.join("model_quantized.onnx"), b"stub-onnx");
    write_file(&dir.join("tokenizer.json"), b"{}");
    dir
}

fn asr_dir(root: &Path, name: &str) -> PathBuf {
    let dir = root.join(name);
    write_file(&dir.join("model.int8.onnx"), b"stub-onnx");
    write_file(&dir.join("tokens.txt"), b"a 0\n");
    dir
}

fn file(root: &Path, name: &str) -> PathBuf {
    let path = root.join(name);
    write_file(&path, b"stub-so");
    path
}

#[test]
fn environment_variables_still_win_over_the_config_file() {
    let _guard = EnvGuard::capture(&[
        "DIARY_MODEL_DIR",
        "DIARY_ORT_DYLIB",
        "ORT_DYLIB_PATH",
        "DIARY_ASR_MODEL_DIR",
        "DIARY_ASR_ENGINE_LIB",
    ]);
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    // 文件那一套路径（全都与 env 不同）。
    let file_model = model_dir(root, "file/models");
    let file_ort = file(root, "file/libonnxruntime.so");
    let file_asr = asr_dir(root, "file/asr");
    let file_engine = file(root, "file/libsherpa-onnx-c-api.so");
    std::fs::write(
        root.join(CONFIG_FILE_NAME),
        serde_json::json!({
            "modelDir": file_model,
            "ortDylib": file_ort,
            "asrEngineLib": file_engine,
            "asrModelDir": file_asr,
        })
        .to_string(),
    )
    .unwrap();

    // env 那一套路径。
    let env_model = model_dir(root, "env/models");
    let env_ort = file(root, "env/libonnxruntime.so");
    let env_asr = asr_dir(root, "env/asr");
    let env_engine = file(root, "env/libsherpa-onnx-c-api.so");
    std::env::set_var("DIARY_MODEL_DIR", &env_model);
    std::env::set_var("DIARY_ORT_DYLIB", &env_ort);
    std::env::remove_var("ORT_DYLIB_PATH");
    std::env::set_var("DIARY_ASR_MODEL_DIR", &env_asr);
    std::env::set_var("DIARY_ASR_ENGINE_LIB", &env_engine);

    {
        let core = Core::open(root.join("library.sqlite")).unwrap();
        let report = core.runtime_config();

        assert_eq!(
            report.semantic.config_source, "env",
            "环境变量必须赢过配置文件"
        );
        assert_eq!(report.semantic.state, "configured");
        assert_eq!(
            report.semantic.model_dir.as_deref(),
            Some(env_model.display().to_string().as_str())
        );
        assert_eq!(report.semantic.model_dir_source, "env");
        assert_eq!(report.semantic.ort_dylib_source, "env");

        assert_eq!(report.transcription.config_source, "env");
        assert_eq!(
            report.transcription.model_dir.as_deref(),
            Some(env_asr.display().to_string().as_str())
        );
        assert_eq!(report.transcription.model_dir_source, "env");
        assert_eq!(
            report.transcription.engine_lib.as_deref(),
            Some(env_engine.display().to_string().as_str())
        );
        assert_eq!(report.transcription.engine_lib_source, "env");
    }

    // 逐字段：只留模型目录的 env，ORT 动态库回落到文件 → 摘要变成 `file`，
    // 但模型目录那一格仍然如实写 `env`。
    std::env::remove_var("DIARY_ORT_DYLIB");
    std::env::remove_var("DIARY_ASR_ENGINE_LIB");
    {
        let core = Core::open(root.join("library.sqlite")).unwrap();
        let report = core.runtime_config();
        assert_eq!(report.semantic.model_dir_source, "env");
        assert_eq!(report.semantic.ort_dylib_source, "file");
        assert_eq!(report.semantic.config_source, "file", "任一字段来自文件即 file");
        assert_eq!(
            report.semantic.ort_dylib.as_deref(),
            Some(file_ort.display().to_string().as_str())
        );
        assert_eq!(report.transcription.model_dir_source, "env");
        assert_eq!(report.transcription.engine_lib_source, "file");
    }

    // 全部撤掉 → 回到文件那一套（证明文件确实被读过、且逐字段回落）。
    std::env::remove_var("DIARY_MODEL_DIR");
    std::env::remove_var("DIARY_ASR_MODEL_DIR");
    {
        let core = Core::open(root.join("library.sqlite")).unwrap();
        let report = core.runtime_config();
        assert_eq!(report.semantic.config_source, "file");
        assert_eq!(
            report.semantic.model_dir.as_deref(),
            Some(file_model.display().to_string().as_str())
        );
        assert_eq!(report.transcription.config_source, "file");
        assert_eq!(
            report.transcription.model_dir.as_deref(),
            Some(file_asr.display().to_string().as_str())
        );
    }
}

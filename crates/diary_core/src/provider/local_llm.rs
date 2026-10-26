//! In-process GGUF generation through the bundled llama.cpp bridge. No transport.
use std::ffi::{c_char, CStr, CString};
use std::path::Path;
use std::sync::OnceLock;

use super::adapter::{GenerateOutcome, GenerateRequest};
use crate::{CoreError, Result};

pub const ADAPTER_ID: &str = "llama_cpp";
pub const BUILTIN_ID: &str = "builtin_local_llm";
pub const MODEL_NAME: &str = "Qwen3.5-4B-Q4_K_M";

fn unavailable(reason: &str) -> CoreError {
    CoreError::ModelUnavailable {
        reason: reason.to_owned(),
    }
}

fn library_name() -> &'static str {
    if cfg!(target_os = "windows") {
        "diary_llm.dll"
    } else if cfg!(target_os = "macos") {
        "libdiary_llm.dylib"
    } else {
        "libdiary_llm.so"
    }
}

fn library() -> Result<&'static libloading::Library> {
    // The bridge caches model tensors. Keep its code and statics alive for the
    // whole process; dlclose after each call would leak that native model cache.
    static LIBRARY: OnceLock<libloading::Library> = OnceLock::new();
    if let Some(library) = LIBRARY.get() {
        return Ok(library);
    }
    let loaded = unsafe { libloading::Library::new(library_name()) }
        .map_err(|_| unavailable("本机整理引擎未能加载，请重新安装应用"))?;
    #[cfg(all(target_env = "ohos", target_arch = "aarch64"))]
    let loaded = {
        type Features = unsafe extern "C" fn() -> usize;
        let features = unsafe { loaded.get::<Features>(b"diary_llm_cpu_features\0") }
            .map(|get| unsafe { get() })
            .unwrap_or(0);
        // ASIMDDP, FPHP and ASIMDHP. Older ARM CPUs keep the baseline runtime.
        let required = (1 << 20) | (1 << 9) | (1 << 10);
        if features & required == required {
            unsafe { libloading::Library::new("libdiary_llm_dotprod.so") }.unwrap_or(loaded)
        } else {
            loaded
        }
    };
    let _ = LIBRARY.set(loaded);
    Ok(LIBRARY.get().expect("native library initialized"))
}

pub(crate) fn readiness(model: Option<&str>) -> Result<()> {
    let path =
        model
            .filter(|p| !p.trim().is_empty())
            .ok_or_else(|| CoreError::ProviderNotConfigured {
                reason: "本机整理模型尚未准备好，请重新打开应用".to_owned(),
            })?;
    if !Path::new(path).is_file() {
        return Err(CoreError::ProviderNotConfigured {
            reason: "本机整理模型不完整，请重新打开应用".to_owned(),
        });
    }
    // Loading the bridge is cheap; model tensors are loaded only on generation.
    library()?;
    Ok(())
}

pub(crate) fn generate(model: Option<&str>, request: &GenerateRequest) -> Result<GenerateOutcome> {
    readiness(model)?;
    let path =
        CString::new(model.unwrap_or_default()).map_err(|_| unavailable("本机模型配置无效"))?;
    let system = CString::new(
        request
            .system
            .as_deref()
            .unwrap_or("请用中文简洁回答，不编造事实。"),
    )
    .map_err(|_| unavailable("整理内容包含无效字符"))?;
    let prompt =
        CString::new(request.prompt.as_str()).map_err(|_| unavailable("整理内容包含无效字符"))?;
    let mut output = vec![0_u8; 65536];
    // Keep the library alive while the function runs and while its error string is read.
    let library = library()?;
    type Generate = unsafe extern "C" fn(
        *const c_char,
        *const c_char,
        *const c_char,
        i32,
        *mut c_char,
        usize,
    ) -> i32;
    type LastError = unsafe extern "C" fn() -> *const c_char;
    let status = unsafe {
        let generate: libloading::Symbol<Generate> = library
            .get(b"diary_llm_generate\0")
            .map_err(|_| unavailable("本机整理引擎版本不匹配，请更新应用"))?;
        generate(
            path.as_ptr(),
            system.as_ptr(),
            prompt.as_ptr(),
            2048,
            output.as_mut_ptr().cast(),
            output.len(),
        )
    };
    if status < 0 {
        let reason = unsafe {
            let error: libloading::Symbol<LastError> = library
                .get(b"diary_llm_last_error\0")
                .map_err(|_| unavailable("本机整理失败，请重试"))?;
            let ptr = error();
            if ptr.is_null() {
                String::new()
            } else {
                CStr::from_ptr(ptr).to_string_lossy().into_owned()
            }
        };
        if reason == "generation timed out" {
            return Err(CoreError::ProviderTimeout {
                reason: "本机整理用时较长，请稍后重试".to_owned(),
            });
        }
        return Err(unavailable(if reason == "prompt too long" {
            "这次内容较多，请减少本次整理的记录"
        } else if reason == "generation limit reached" {
            "这次内容较多，整理未完成，请减少本次整理的记录"
        } else {
            "本机整理失败，请重试"
        }));
    }
    if status as usize >= output.len() {
        return Err(unavailable("本机整理结果不完整，请重试"));
    }
    let text = std::str::from_utf8(&output[..status as usize])
        .map_err(|_| unavailable("本机整理结果不完整，请重试"))?;
    let text = visible_text(text)?;
    Ok(GenerateOutcome {
        text,
        model: Some(MODEL_NAME.to_owned()),
    })
}

fn visible_text(raw: &str) -> Result<String> {
    let raw = raw.trim();
    let text = if raw.starts_with("<think>") {
        raw.split_once("</think>")
            .map(|(_, body)| body.trim())
            .ok_or_else(|| unavailable("本机整理尚未完成，请重试"))?
    } else {
        raw
    };
    if text.is_empty() || text.contains("<|im_") || text.contains("<think>") {
        return Err(unavailable("本机整理结果不完整，请重试"));
    }
    Ok(text.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn incomplete_thinking_is_not_exposed_as_diary() {
        assert!(visible_text("<think>unfinished").is_err());
        assert_eq!(
            visible_text("<think>reasoning</think>\n今天去看妈妈。").unwrap(),
            "今天去看妈妈。"
        );
        assert!(visible_text("<|im_start|>assistant").is_err());
        assert_eq!(visible_text(" 今天去看妈妈。 ").unwrap(), "今天去看妈妈。");
    }
}

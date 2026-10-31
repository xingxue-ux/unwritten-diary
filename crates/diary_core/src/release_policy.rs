//! This branch's HarmonyOS release keeps local ASR/search, without text generation.
//! The feature exercises the same policy in host tests; other targets keep their behavior.
pub const GENERATION_ENABLED: bool = !cfg!(any(target_env = "ohos", feature = "no-generation"));

pub(crate) fn require_generation() -> crate::error::Result<()> {
    if GENERATION_ENABLED {
        Ok(())
    } else {
        Err(crate::CoreError::InvalidArgument {
            reason: "此版本不提供日记生成或在线模型服务。".to_owned(),
        })
    }
}

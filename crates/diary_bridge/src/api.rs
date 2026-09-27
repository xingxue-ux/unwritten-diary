//! 桥接层最小 API，对应契约第 1.3 与 4.1 节。
//!
//! 这些函数现在返回可预期的固定结构，用来验证：Dart 侧能否调用到 Rust、
//! 同步与异步两条路径是否都能用、字段映射（snake_case → camelCase）是否正确。
//! 它们**不是**真实实现，B1 起会被真正的核心替换。

/// 契约第 1.3 节：核心与协议的版本信息。
pub struct CoreInfo {
    /// 主次版本；新增可选字段是次版本，删除或改变语义是主版本。
    pub api_version: String,
    pub data_schema_version: u32,
    pub build_version: String,
    pub library_id: String,
    /// 当前构建实际支持的能力，界面据此决定哪些入口可见。
    pub capabilities: Vec<String>,
}

/// 启动恢复摘要：如实告知上次发生了什么。
pub struct RecoverySummary {
    pub recovered_draft_count: u32,
    pub orphaned_import_count: u32,
    pub unrecovered_recording_count: u32,
    pub notes: Vec<String>,
}

/// 轻量启动快照，不返回整库正文或媒体。
pub struct CoreSnapshot {
    pub core_info: CoreInfo,
    pub recovery: RecoverySummary,
    pub pending_job_count: u32,
    pub last_event_sequence: u64,
    pub capture_count: u32,
}

/// 打开或创建资料库（同步路径）。
///
/// `library_handle` 为空表示使用默认库。M0 只返回结构，不碰磁盘。
pub fn open(library_handle: Option<String>) -> CoreSnapshot {
    let library_id = match library_handle {
        Some(handle) if !handle.is_empty() => handle,
        _ => "library-m0-probe".to_owned(),
    };
    snapshot_for(library_id)
}

/// 打开或创建资料库（异步路径）。
///
/// 与 [`open`] 返回同样的结构，用来验证 Dart 侧的 Future 是否正常完成。
pub async fn open_async(library_handle: Option<String>) -> CoreSnapshot {
    open(library_handle)
}

/// 轻量启动快照。
pub fn snapshot() -> CoreSnapshot {
    snapshot_for("library-m0-probe".to_owned())
}

/// 一个可判定的纯计算函数：验证跨语言调用的参数与返回值没有走样。
pub fn add(a: i32, b: i32) -> i32 {
    a + b
}

/// 回显文本，用于验证中文与 emoji 在桥接两侧保持一致。
pub fn echo(text: String) -> String {
    text
}

fn snapshot_for(library_id: String) -> CoreSnapshot {
    CoreSnapshot {
        core_info: CoreInfo {
            api_version: "1.0".to_owned(),
            data_schema_version: 0,
            build_version: env!("CARGO_PKG_VERSION").to_owned(),
            library_id,
            capabilities: vec![
                "probe.sync".to_owned(),
                "probe.async".to_owned(),
                "probe.echo".to_owned(),
            ],
        },
        recovery: RecoverySummary {
            recovered_draft_count: 0,
            orphaned_import_count: 0,
            unrecovered_recording_count: 0,
            notes: Vec::new(),
        },
        pending_job_count: 0,
        last_event_sequence: 0,
        capture_count: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_is_stable() {
        assert_eq!(add(2, 3), 5);
    }

    #[test]
    fn echo_keeps_unicode() {
        let text = "妈妈离职了 🙂";
        assert_eq!(echo(text.to_owned()), text);
    }

    #[test]
    fn open_uses_default_library_when_empty() {
        assert_eq!(open(None).core_info.library_id, "library-m0-probe");
        assert_eq!(
            open(Some("library-42".to_owned())).core_info.library_id,
            "library-42"
        );
        assert_eq!(open(Some(String::new())).core_info.library_id, "library-m0-probe");
    }

    #[test]
    fn snapshot_reports_api_version() {
        let snapshot = snapshot();
        assert_eq!(snapshot.core_info.api_version, "1.0");
        assert_eq!(snapshot.core_info.data_schema_version, 0);
        assert!(snapshot.core_info.capabilities.contains(&"probe.echo".to_owned()));
    }
}
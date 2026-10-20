//! 契约第 7 节的错误码与核心内部错误类型。
//!
//! `wire()` 的字符串必须与契约第 7 节、`packages/diary_api` 的
//! `DiaryErrorCode` 以及桥接层保持一致。

use thiserror::Error;

/// 契约第 7 节的错误码。新增一个码等于改契约，必须先改文档与 Dart 枚举。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCode {
    /// 目标不存在（例如已被清理）。
    NotFound,
    /// 当前状态不允许这个操作。
    InvalidState,
    /// 输入本身不合法（例如 `indexes.rebuild` 的 `scope` 编码解析不了）。
    InvalidArgument,
    /// 乐观锁版本不匹配。
    RevisionConflict,
    /// 同一个 operationId 被用于不同内容。
    IdempotencyConflict,
    /// 磁盘空间不足。
    StorageFull,
    /// 资产在库里有记录，但文件已经不可用。
    AssetMissing,
    /// 文件系统权限不允许这次操作。
    PermissionDenied,
    /// 复制过来的字节与调用方声明的不一致。
    IntegrityFailed,
    /// 这个格式还提取不了正文。
    UnsupportedFormat,
    /// 检索会话的快照已失效（索引变了或会话不存在），前端应重新发起查询。
    SearchExpired,
    /// 游标过期或不属于这个会话，前端应从头翻。
    CursorExpired,
    /// 还没有可用（已配置且已启用、并且支持所需能力）的远端服务。
    ProviderNotConfigured,
    /// 鉴权失败：密钥不对、过期或没有权限。等用户改配置，不频繁重试。
    AuthenticationFailed,
    /// 网络不可用：断网、连不上、远端 5xx。
    NetworkUnavailable,
    /// 被限流：尊重 Retry-After，可退避重试。
    RateLimited,
    /// 额度用尽：暂停，等用户处理。
    QuotaExceeded,
    /// 远端超时：有限重试；**不承诺远端未计费**。
    ProviderTimeout,
    /// 端点/模型不可用或返回内容解析不了：检查配置，重试不会好。
    ModelUnavailable,
    /// 备份包结构、哈希或清单与内容不一致（含越界路径），不能用来恢复。
    CorruptedBackup,
    /// 备份包来自更新的核心（`schemaVersion` / `apiVersion` 高于本构建）。
    IncompatibleBackup,
}

impl ErrorCode {
    pub fn wire(self) -> &'static str {
        match self {
            Self::NotFound => "not_found",
            Self::InvalidState => "invalid_state",
            Self::InvalidArgument => "invalid_argument",
            Self::RevisionConflict => "revision_conflict",
            Self::IdempotencyConflict => "idempotency_conflict",
            Self::StorageFull => "storage_full",
            Self::AssetMissing => "asset_missing",
            Self::PermissionDenied => "permission_denied",
            Self::IntegrityFailed => "integrity_failed",
            Self::UnsupportedFormat => "unsupported_format",
            Self::SearchExpired => "search_expired",
            Self::CursorExpired => "cursor_expired",
            Self::ProviderNotConfigured => "provider_not_configured",
            Self::AuthenticationFailed => "authentication_failed",
            Self::NetworkUnavailable => "network_unavailable",
            Self::RateLimited => "rate_limited",
            Self::QuotaExceeded => "quota_exceeded",
            Self::ProviderTimeout => "provider_timeout",
            Self::ModelUnavailable => "model_unavailable",
            Self::CorruptedBackup => "corrupted_backup",
            Self::IncompatibleBackup => "incompatible_backup",
        }
    }
}

/// 核心内部错误。用户可读的 message 与 retryable 由上层决定怎么呈现。
#[derive(Debug, Error)]
pub enum CoreError {
    #[error("找不到{entity}：{id}")]
    NotFound { entity: &'static str, id: String },

    #[error("当前状态不允许该操作：{entity} {id} 处于 {state}")]
    InvalidState {
        entity: &'static str,
        id: String,
        state: String,
    },

    #[error("输入不合法：{reason}")]
    InvalidArgument { reason: String },

    #[error("版本冲突：期望 revision {expected}，实际 {actual}")]
    RevisionConflict { expected: i64, actual: i64 },

    /// 日记版本的乐观锁冲突（B4c）：与 `RevisionConflict` 同一个错误码，
    /// 但把「哪一天、现在有几版」写进消息里——这一天的 revision 就是版本数，
    /// 只报两个裸数字用户看不懂发生了什么。
    #[error(
        "版本冲突：{day_key} 的 revision 期望是 {expected}，实际是 {actual}\
         （这一天在你读取之后又新增了版本）；请重新读取这一天再试，核心不会覆盖任何已有版本"
    )]
    DiaryRevisionConflict {
        day_key: String,
        expected: i64,
        actual: i64,
    },

    #[error("幂等冲突：operationId {operation_id} 已经用于不同的内容")]
    IdempotencyConflict { operation_id: String },

    /// 整理设置的乐观锁冲突（B4e）：与 `RevisionConflict` 同一个错误码，但把
    /// 「整理设置」写清楚——只报两个裸数字，用户不知道是哪一个对象过期了。
    #[error(
        "版本冲突：整理设置的 revision 期望是 {expected}，实际是 {actual}\
         （设置在你读取之后又被改过）；请重新读取设置再试，核心不会用旧设置覆盖新的"
    )]
    SettingsRevisionConflict { expected: i64, actual: i64 },

    #[error("磁盘空间不足")]
    StorageFull,

    #[error("资产不可用：{asset_id}（{reason}）")]
    AssetMissing { asset_id: String, reason: String },

    #[error("完整性校验失败：{reason}")]
    IntegrityFailed { reason: String },

    #[error("提取失败：{reason}")]
    ExtractionFailed { reason: String },

    #[error("检索会话已失效：{reason}")]
    SearchExpired { reason: String },

    #[error("检索游标已失效：{reason}")]
    CursorExpired { reason: String },

    // ------------------------------------------------------------ 远端服务（B4a）
    //
    // 这几类错误的时间语义**不一样**，所以不能都塞进一个「网络错误」：
    // 鉴权失败要等用户改配置（不频繁重试），超时/断网/限流才是有意义的退避重试。
    // 队列侧的处置见 `provider::normalize` 与 `jobs::FailureDisposition`。
    #[error("还没有可用的服务：{reason}")]
    ProviderNotConfigured { reason: String },

    #[error("认证失败：{reason}")]
    AuthenticationFailed { reason: String },

    #[error("网络不可用：{reason}")]
    NetworkUnavailable { reason: String },

    #[error("请求被限流：{reason}")]
    RateLimited {
        reason: String,
        /// 远端 `Retry-After` 给的等待毫秒数（有就给，没有就按本地退避）。
        retry_after_ms: Option<u64>,
    },

    #[error("额度已用尽：{reason}")]
    QuotaExceeded { reason: String },

    /// **整理配额停下**（B4e）：与 `QuotaExceeded` 落同一个契约错误码
    /// （`quota_exceeded`），但时间语义不同——它不是「等用户改配置就会好」，而是
    /// 「核心按用户自己设的上限主动停下」。
    ///
    /// 为什么必须分开：队列对 `QuotaExceeded` 的处置是 `waiting_configuration`，
    /// 而配置闸门（`promote_configuration_waits`）**每一轮**都会把「在等配置」的任务
    /// 重新拉起来——对一个配额停下的整理任务来说，那等于每一轮都偷偷再跑一次、
    /// 每次都烧掉上限内的调用（每一步都停在同一个上限上，任务永不结束）。所以整理
    /// 配额停下的处置是**终态**：`failed` + `requires_user_action`，等用户调高上限后
    /// 人工重试（`jobs.retry`）或等窗口滑过去。
    #[error("整理已到上限：{reason}")]
    OrganizeStopped { reason: String },

    #[error("服务响应超时：{reason}")]
    ProviderTimeout { reason: String },

    #[error("模型或端点不可用：{reason}")]
    ModelUnavailable { reason: String },

    #[error("这个服务不支持所需能力：{reason}")]
    CapabilityUnsupported { reason: String },
    /// 备份包坏了：哈希、结构、清单与内容对不上，或清单里有越界路径。
    ///
    /// 契约第 7 节的 `corrupted_backup`：用户可见行为是「恢复失败原因清晰」，
    /// 自动行为是「不修改现有库」——所以这个错误**只在什么都没改之前**返回。
    #[error("备份包不可用：{reason}")]
    CorruptedBackup { reason: String },

    /// 备份包来自更新的核心：本构建不认识它的 schema / API 版本。
    ///
    /// 契约第 7 节的 `incompatible_backup`。高低版本都可能是「不认识」，
    /// 但只有**高于**本构建的才拒；低于本构建的照常恢复，由开库时的迁移接手。
    #[error("备份包与当前版本不兼容：{reason}")]
    IncompatibleBackup { reason: String },

    // ------------------------------------------------------------ 插件（#88，B6）
    //
    // 插件错误**不新增契约第 7 节的 wire 码**：清单/包错误落到 `invalid_argument` /
    // `integrity_failed`，越权落到 `permission_denied`，超预算落到 `quota_exceeded`，
    // 运行失败落到 `invalid_state`。分开变体只是为了让用户看到具体原因。
    #[error("插件清单不合法：{reason}")]
    PluginManifestInvalid { reason: String },

    #[error("插件包不可用：{reason}")]
    PluginPackageInvalid { reason: String },

    #[error("插件与当前版本不兼容：{reason}")]
    PluginIncompatible { reason: String },

    #[error("插件没有权限：{reason}")]
    PluginPermissionDenied { reason: String },

    #[error("插件运行超出预算：{reason}")]
    PluginBudgetExceeded { reason: String },

    #[error("插件运行失败：{reason}")]
    PluginFailed { reason: String },

    /// 导出目录越出允许的写域（OHOS 上只允许应用沙箱）。映射到契约既有的
    /// `permission_denied`：这不是「参数写错了」，而是「这里不能写」。
    #[error("导出目录不可用：{reason}")]
    ExportPathRejected { reason: String },

    #[error("文件操作失败：{0}")]
    Io(#[from] std::io::Error),

    #[error("库里的数据不合法：{message}")]
    CorruptedData { message: String },

    #[error("数据库错误：{0}")]
    Database(#[from] rusqlite::Error),

    #[error("序列化错误：{0}")]
    Serialization(#[from] serde_json::Error),
}

impl CoreError {
    /// 映射到契约错误码。凡是能对上契约的都要明确对上，不能含糊。
    pub fn code(&self) -> ErrorCode {
        match self {
            Self::NotFound { .. } => ErrorCode::NotFound,
            Self::InvalidState { .. } => ErrorCode::InvalidState,
            Self::InvalidArgument { .. } => ErrorCode::InvalidArgument,
            Self::RevisionConflict { .. } | Self::DiaryRevisionConflict { .. } => {
                ErrorCode::RevisionConflict
            }
            Self::SettingsRevisionConflict { .. } => ErrorCode::RevisionConflict,
            Self::IdempotencyConflict { .. } => ErrorCode::IdempotencyConflict,
            Self::StorageFull => ErrorCode::StorageFull,
            Self::AssetMissing { .. } => ErrorCode::AssetMissing,
            Self::IntegrityFailed { .. } => ErrorCode::IntegrityFailed,
            Self::ExtractionFailed { .. } => ErrorCode::UnsupportedFormat,
            Self::SearchExpired { .. } => ErrorCode::SearchExpired,
            Self::CursorExpired { .. } => ErrorCode::CursorExpired,
            Self::ProviderNotConfigured { .. } | Self::CapabilityUnsupported { .. } => {
                ErrorCode::ProviderNotConfigured
            }
            Self::AuthenticationFailed { .. } => ErrorCode::AuthenticationFailed,
            Self::NetworkUnavailable { .. } => ErrorCode::NetworkUnavailable,
            Self::RateLimited { .. } => ErrorCode::RateLimited,
            Self::QuotaExceeded { .. } | Self::OrganizeStopped { .. } => ErrorCode::QuotaExceeded,
            Self::ProviderTimeout { .. } => ErrorCode::ProviderTimeout,
            Self::ModelUnavailable { .. } => ErrorCode::ModelUnavailable,
            Self::CorruptedBackup { .. } => ErrorCode::CorruptedBackup,
            Self::IncompatibleBackup { .. } => ErrorCode::IncompatibleBackup,
            Self::PluginManifestInvalid { .. } | Self::PluginIncompatible { .. } => {
                ErrorCode::InvalidArgument
            }
            Self::PluginPackageInvalid { .. } => ErrorCode::IntegrityFailed,
            Self::PluginPermissionDenied { .. } | Self::ExportPathRejected { .. } => {
                ErrorCode::PermissionDenied
            }
            Self::PluginBudgetExceeded { .. } => ErrorCode::QuotaExceeded,
            Self::PluginFailed { .. } => ErrorCode::InvalidState,
            Self::Io(err) => io_code(err),
            Self::CorruptedData { .. } | Self::Serialization(_) => ErrorCode::InvalidState,
            Self::Database(err) => database_code(err),
        }
    }

    /// 给错误消息补一句说明（例如「本次测试已发起 1 次真实请求」）。
    ///
    /// 只对带 `reason` 的远端服务类错误有意义；别的错误原样不动——编不出话就不编。
    pub fn append_reason(&mut self, note: &str) {
        let reason = match self {
            Self::ProviderNotConfigured { reason }
            | Self::AuthenticationFailed { reason }
            | Self::NetworkUnavailable { reason }
            | Self::RateLimited { reason, .. }
            | Self::QuotaExceeded { reason }
            | Self::OrganizeStopped { reason }
            | Self::ProviderTimeout { reason }
            | Self::ModelUnavailable { reason }
            | Self::CapabilityUnsupported { reason } => reason,
            _ => return,
        };
        reason.push_str(note);
    }

    /// 是否值得原样重试。磁盘满、版本冲突这些重试也不会好。
    ///
    /// 两个检索失效码是例外：契约第 7 节给它们的处理是「安静刷新当前范围 /
    /// 丢弃旧会话结果并重取」，也就是「重来一次」是正确动作，而
    /// `packages/diary_api` 里对应的异常也把 `retryable` 设成 true。
    pub fn retryable(&self) -> bool {
        matches!(
            self,
            Self::SearchExpired { .. }
                | Self::CursorExpired { .. }
                | Self::NetworkUnavailable { .. }
                | Self::ProviderTimeout { .. }
                | Self::RateLimited { .. }
        )
    }

    /// 这条错误是不是「等用户改配置」而不是「等一等再试」。
    ///
    /// 契约第 7 节把 `provider_not_configured` / `authentication_failed` 的自动行为
    /// 定成「等待配置，不频繁重试」，`quota_exceeded` 定成「额度问题暂停」。这三类
    /// 都不该消耗任务的退避额度——再试一百次也不会好，只会反复打扰远端。作业侧把
    /// 它们落成 `waiting_configuration`（见 `jobs::run_due`）。
    pub fn waits_for_configuration(&self) -> bool {
        matches!(
            self,
            Self::ProviderNotConfigured { .. }
                | Self::CapabilityUnsupported { .. }
                | Self::AuthenticationFailed { .. }
                | Self::QuotaExceeded { .. }
        )
    }
}

/// 文件系统错误也要落到具体码上：空间不足、权限不足都不是同一件事。
fn io_code(err: &std::io::Error) -> ErrorCode {
    // ENOSPC 在 Linux 与 Android 上都是 28。
    const ENOSPC: i32 = 28;
    match err.kind() {
        std::io::ErrorKind::PermissionDenied => ErrorCode::PermissionDenied,
        std::io::ErrorKind::NotFound => ErrorCode::AssetMissing,
        _ => match err.raw_os_error() {
            Some(ENOSPC) => ErrorCode::StorageFull,
            _ => ErrorCode::IntegrityFailed,
        },
    }
}

/// SQLite 的错误要落到具体码上：空间不足不是「数据库坏了」。
fn database_code(err: &rusqlite::Error) -> ErrorCode {
    use rusqlite::ffi::ErrorCode as SqliteCode;
    use rusqlite::Error::SqliteFailure;

    match err {
        SqliteFailure(failure, _) => match failure.code {
            SqliteCode::DiskFull => ErrorCode::StorageFull,
            _ => ErrorCode::InvalidState,
        },
        _ => ErrorCode::InvalidState,
    }
}

pub type Result<T> = std::result::Result<T, CoreError>;
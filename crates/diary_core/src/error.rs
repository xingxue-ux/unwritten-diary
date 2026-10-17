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

    #[error("幂等冲突：operationId {operation_id} 已经用于不同的内容")]
    IdempotencyConflict { operation_id: String },

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
            Self::RevisionConflict { .. } => ErrorCode::RevisionConflict,
            Self::IdempotencyConflict { .. } => ErrorCode::IdempotencyConflict,
            Self::StorageFull => ErrorCode::StorageFull,
            Self::AssetMissing { .. } => ErrorCode::AssetMissing,
            Self::IntegrityFailed { .. } => ErrorCode::IntegrityFailed,
            Self::ExtractionFailed { .. } => ErrorCode::UnsupportedFormat,
            Self::SearchExpired { .. } => ErrorCode::SearchExpired,
            Self::CursorExpired { .. } => ErrorCode::CursorExpired,
            Self::CorruptedBackup { .. } => ErrorCode::CorruptedBackup,
            Self::IncompatibleBackup { .. } => ErrorCode::IncompatibleBackup,
            Self::Io(err) => io_code(err),
            Self::CorruptedData { .. } | Self::Serialization(_) => ErrorCode::InvalidState,
            Self::Database(err) => database_code(err),
        }
    }

    /// 是否值得原样重试。磁盘满、版本冲突这些重试也不会好。
    ///
    /// 两个检索失效码是例外：契约第 7 节给它们的处理是「安静刷新当前范围 /
    /// 丢弃旧会话结果并重取」，也就是「重来一次」是正确动作，而
    /// `packages/diary_api` 里对应的异常也把 `retryable` 设成 true。
    pub fn retryable(&self) -> bool {
        matches!(self, Self::SearchExpired { .. } | Self::CursorExpired { .. })
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
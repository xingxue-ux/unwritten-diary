//! 「不写日记」核心的稳定 C ABI（#108 鸿蒙化的第一块）。
//!
//! 这一层给**非 Flutter 宿主**（ArkTS/NAPI、CLI、离线测试）用：它把 `diary_core` 已有的
//! 能力包成「句柄 + JSON 进 / JSON 出」的 C 接口。Flutter 那条 `flutter_rust_bridge`
//! 通道原样不动，两边调的是同一个核心，不新增第二套业务规则、不新增错误码。
//!
//! # 导出面
//!
//! ```c
//! char        *diary_capi_version(void);                 /* 诊断用的版本戳，需 diary_string_free */
//! diary_handle *diary_open(const char *path);            /* 失败返回 NULL，原因在 diary_last_error */
//! int          diary_close(diary_handle *handle);        /* 0 成功；1 空指针；2 无效/已关闭；3 桥面内部错误 */
//! char        *diary_call(diary_handle *handle, const char *method_json); /* 信封 JSON，需 diary_string_free */
//! void         diary_string_free(char *text);            /* 释放上面所有 char* */
//! char        *diary_last_error(void);                   /* 本线程最近一条错误（JSON），无则 NULL */
//! ```
//!
//! `diary_handle` 是不透明指针，C 侧只能原样传回。
//!
//! # 请求与响应
//!
//! 请求是一个 JSON 对象：
//!
//! ```json
//! {"method":"captures.saveDraft","params":{"capture_id":"cap_…","text":"…","expected_revision":1,"operation_id":"op-1"}}
//! ```
//!
//! 响应是版本化信封（**成功与失败都带 `apiVersion`**）：
//!
//! ```json
//! {"apiVersion":"1.1","data":{…},"ok":true}
//! {"apiVersion":"1.1","error":{"code":"invalid_argument","message":"…","retryable":false},"ok":false}
//! ```
//!
//! - `apiVersion` 就是核心的 `diary_core::API_VERSION`（契约第 1.2 节的口径），桥面不自造版本号；
//! - `data` 是核心模型的 serde JSON **原样**（字段名 snake_case，与 `diary_bridge` 镜像出去
//!   的是同一份类型），桥面不做二次翻译；
//! - `error.code` 只可能是契约第 7 节 / `diary_core::ErrorCode` 里的 21 个码之一。
//!   **桥面没有新增错误码**：空指针、非法 JSON、未知方法落到 `invalid_argument`，句柄已关闭、
//!   核心 panic 落到 `invalid_state`；这些「不是核心业务错误」的 message 一律以 `C ABI：`
//!   开头，便于监控区分；
//! - JSON 对象的**字段顺序不保证**（serde_json 默认按 key 排序），调用方按 key 取值。
//!
//! # 所有权
//!
//! - 返回给 C 的每个 `char *` 都由 Rust 分配（`CString::into_raw`），**必须**用
//!   [`diary_string_free`] 释放，不能 `free()`、不能重复释放；
//! - `diary_open` 返回的句柄必须用 [`diary_close`] 关闭；
//! - 参数里的 `const char *` 由调用方拥有，桥面只读、不释放。
//!
//! # 生命周期
//!
//! 句柄是一个**地址令牌**：桥面用指针值去查登记表，**从不解引用**调用方传进来的指针。
//! 所以「关闭后再用」「传一个从来没开过的指针」都只会得到一条明确的 `invalid_state`
//! 错误，而不是未定义行为。代价是每次成功打开会留下一块 1 字节的令牌且永不回收
//! （进程内地址因此不会复用，也就不会出现「旧句柄误命中新句柄」）；这与「关闭后指针即
//! 悬垂」的 C 惯例不同，是有意的选择，见 `docs/architecture/m6-核心-C-ABI.md`。
//!
//! `diary_close` 只摘掉自己那一个句柄：同一个路径被打开多次时，登记表里的核心是**共享**
//! 的（`Arc<Mutex<Core>>`，与 `diary_bridge` 的 `OPEN_LIBRARIES` 同一口径），最后一个句柄
//! 关闭时核心才真正析构、SQLite 连接才释放。
//!
//! # 线程模型
//!
//! - 任意线程都可以用同一个句柄；桥面只在登记表上持锁一瞬间，真正的调用串行在核心自己的
//!   `Mutex` 后面（核心不是并发安全的）；
//! - 登记表的锁**不跨核心调用持有**（否则一个慢查询会卡住所有 `diary_open`/`diary_close`）；
//! - 核心 panic 会让那把 `Mutex` 中毒，但桥面按「取回内部值」处理，不把中毒变成第二次
//!   panic，句柄在报错之后仍可继续使用。
//!
//! # 绝不 panic 穿过 FFI
//!
//! 每个导出函数都在 `catch_unwind` 里跑：`extern "C"` 里逃出去的 panic 会直接 abort 进程，
//! 那是宿主不能接受的。panic 一律变成 `ok:false` + `invalid_state` 的错误信封。

mod methods;
#[cfg(test)]
mod tests;

use std::any::Any;
use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::ptr;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError, Weak};

use diary_core::{Core, CoreError, ErrorCode};
use serde_json::{json, Value};

/// 信封里的 `apiVersion`：与核心同一个常量（契约第 1.2 节），桥面不自造版本号。
pub const API_VERSION: &str = diary_core::API_VERSION;

/// 不透明句柄。
///
/// 它只是「登记表里的一把钥匙」：桥面拿**指针值**当 key，从不解引用它。结构体里留一个
/// 字节只是为了每次分配都有独一无二的地址（零大小分配会共用同一个地址）。
#[repr(C)]
pub struct Handle {
    _token: u8,
}

/// 一个已打开句柄背后的东西：共享的核心 + 展示用的库名。
struct LibraryEntry {
    core: Arc<Mutex<Core>>,
    library_id: String,
}

/// 路径 → 核心。**弱引用**：最后一个句柄关掉后核心随之析构，登记表里只留一条过期条目。
///
/// 同一个进程里同一路径只开一个核心：两个 `Core::open` 会各自跑一次 `recover_on_open`，
/// 两个 `runDue` 会各自领任务——与 `diary_bridge::api::OPEN_LIBRARIES` 的理由完全一样。
static OPEN_LIBRARIES: OnceLock<Mutex<HashMap<PathBuf, Weak<Mutex<Core>>>>> = OnceLock::new();

/// 句柄（指针值）→ 核心。
static HANDLES: OnceLock<Mutex<HashMap<usize, LibraryEntry>>> = OnceLock::new();

// 本线程最近一条错误（`diary_open` 这种「只能返回 NULL」的入口靠它把原因带出去）。
thread_local! {
    static LAST_ERROR: RefCell<Option<String>> = const { RefCell::new(None) };
}

// 测试里用的「还活着的字符串」计数：证明所有权规则没有漏。
//
// 线程内计数（不是全局）：测试各跑各的线程，互不干扰；发布构建里这段代码不存在。
#[cfg(test)]
thread_local! {
    static LIVE_STRINGS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

// ---------------------------------------------------------------- 错误

/// 桥面错误：要么来自核心，要么是 C ABI 边界条件自己判定的。
///
/// **没有新增错误码**：`code` 只会是 `diary_core::ErrorCode` 里的 14 个之一。
#[derive(Debug)]
pub(crate) struct CallError {
    code: ErrorCode,
    message: String,
    retryable: bool,
}

impl CallError {
    pub(crate) fn invalid_argument(message: impl Into<String>) -> Self {
        Self {
            code: ErrorCode::InvalidArgument,
            message: message.into(),
            retryable: false,
        }
    }

    pub(crate) fn invalid_state(message: impl Into<String>) -> Self {
        Self {
            code: ErrorCode::InvalidState,
            message: message.into(),
            retryable: false,
        }
    }

    /// 桥面自己判定的错误：message 一律带 `C ABI：` 前缀，与核心的业务错误区分开。
    fn bridge(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: format!("C ABI：{}", message.into()),
            retryable: false,
        }
    }
}

impl From<CoreError> for CallError {
    fn from(error: CoreError) -> Self {
        Self {
            code: error.code(),
            message: error.to_string(),
            retryable: error.retryable(),
        }
    }
}

/// 成功信封。
fn ok_envelope(data: Value) -> String {
    json!({"apiVersion": API_VERSION, "ok": true, "data": data}).to_string()
}

/// 失败信封。`message` 原样带出去，调用方按 `code` 决定行为。
fn error_envelope(error: &CallError) -> String {
    json!({
        "apiVersion": API_VERSION,
        "ok": false,
        "error": {
            "code": error.code.wire(),
            "message": error.message,
            "retryable": error.retryable,
        }
    })
    .to_string()
}

// ---------------------------------------------------------------- 内部工具

fn lock_or_recover<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    // 中毒不传播：核心 panic 之后那把锁里的数据仍由 SQLite 的事务回滚兜着，
    // 桥面继续可用比「拒绝服务」诚实。
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn handles() -> &'static Mutex<HashMap<usize, LibraryEntry>> {
    HANDLES.get_or_init(|| Mutex::new(HashMap::new()))
}

fn open_libraries() -> &'static Mutex<HashMap<PathBuf, Weak<Mutex<Core>>>> {
    OPEN_LIBRARIES.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 把返回给 C 的字符串登记成「Rust 分配、`diary_string_free` 释放」。
fn to_c_string(text: String) -> *mut c_char {
    #[cfg(test)]
    LIVE_STRINGS.with(|count| count.set(count.get() + 1));
    // JSON 里不会有内嵌 NUL；这里顺手过滤一次，保证不会因为一条消息把返回值变成 NULL。
    let sanitized: String = text.chars().filter(|ch| *ch != '\0').collect();
    match CString::new(sanitized) {
        Ok(value) => value.into_raw(),
        Err(_) => ptr::null_mut(),
    }
}

fn clear_last_error() {
    LAST_ERROR.with(|slot| *slot.borrow_mut() = None);
}

fn record_last_error(error: &CallError) {
    LAST_ERROR.with(|slot| *slot.borrow_mut() = Some(error_envelope(error)));
}

fn panic_message(payload: &(dyn Any + Send)) -> String {
    if let Some(text) = payload.downcast_ref::<&'static str>() {
        (*text).to_owned()
    } else if let Some(text) = payload.downcast_ref::<String>() {
        text.clone()
    } else {
        "非字符串 panic 载荷".to_owned()
    }
}

fn panic_error(payload: &(dyn Any + Send), entry: &str) -> CallError {
    CallError::bridge(
        ErrorCode::InvalidState,
        format!(
            "panic 已被拦截（{entry}，进程未中止）：{}",
            panic_message(payload)
        ),
    )
}

/// 所有「返回字符串」入口的统一壳：把 panic 变成错误信封，并记进线程内的 last-error。
fn guard_text<F>(entry: &str, body: F) -> *mut c_char
where
    F: FnOnce() -> Result<String, CallError>,
{
    let outcome = catch_unwind(AssertUnwindSafe(body));
    let result = match outcome {
        Ok(result) => result,
        Err(payload) => Err(panic_error(payload.as_ref(), entry)),
    };
    match result {
        Ok(text) => to_c_string(text),
        Err(error) => {
            record_last_error(&error);
            to_c_string(error_envelope(&error))
        }
    }
}

/// 读出 C 字符串并检查 UTF-8。空指针是一条**明确错误**，不是 UB。
///
/// # Safety
/// 调用方保证：非空时 `pointer` 指向 NUL 结尾、可读的内存。
unsafe fn c_str_to_string(pointer: *const c_char, what: &str) -> Result<String, CallError> {
    if pointer.is_null() {
        return Err(CallError::bridge(
            ErrorCode::InvalidArgument,
            format!("{what} 指针为空"),
        ));
    }
    // SAFETY: 由调用方保证（见本函数的安全约定）。
    let text = unsafe { CStr::from_ptr(pointer) };
    text.to_str().map(str::to_owned).map_err(|_| {
        CallError::bridge(ErrorCode::InvalidArgument, format!("{what} 不是合法 UTF-8"))
    })
}

/// 资料库路径 → 登记表 key：绝对化 + 目录 canonicalize（文件首次打开时还不存在）。
///
/// 与 `diary_bridge::api::library_key` 同一口径：相对路径与绝对路径的调用方要能碰上同一个
/// 条目，否则同一份库会被打开成两个核心。
fn library_key(path: &Path) -> Result<PathBuf, CallError> {
    let file_name = path.file_name().ok_or_else(|| {
        CallError::bridge(ErrorCode::InvalidArgument, "资料库路径必须指向一个文件")
    })?;
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|error| CallError::from(CoreError::from(error)))?
            .join(path)
    };
    let parent = absolute.parent().unwrap_or_else(|| Path::new("."));
    let canonical_parent = std::fs::canonicalize(parent).unwrap_or_else(|_| parent.to_path_buf());
    Ok(canonical_parent.join(file_name))
}

// ---------------------------------------------------------------- 导出函数

/// 桥面版本戳（诊断用，不参与协议协商——协议版本在信封的 `apiVersion` 里）。
///
/// 返回的字符串由 Rust 分配，用 [`diary_string_free`] 释放。
#[no_mangle]
pub extern "C" fn diary_capi_version() -> *mut c_char {
    clear_last_error();
    guard_text("diary_capi_version", || {
        Ok(format!(
            "diary_capi/{} apiVersion={API_VERSION}",
            env!("CARGO_PKG_VERSION")
        ))
    })
}

/// 打开（或创建）资料库，返回句柄；失败返回 NULL。
///
/// 失败原因（`code` / `message` / `retryable` 的 JSON）放在 [`diary_last_error`] 里，
/// 因为 NULL 自己带不出信息。
///
/// # Safety
/// `path` 必须是 NUL 结尾的 UTF-8 字符串指针，或空指针（空指针报 `invalid_argument`）。
#[no_mangle]
pub unsafe extern "C" fn diary_open(path: *const c_char) -> *mut Handle {
    clear_last_error();
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: 由调用方保证（见本函数的安全约定）。
        let text = unsafe { c_str_to_string(path, "path") }?;
        open_library(&text)
    }));
    match outcome {
        Ok(Ok(handle)) => handle,
        Ok(Err(error)) => {
            record_last_error(&error);
            ptr::null_mut()
        }
        Err(payload) => {
            record_last_error(&panic_error(payload.as_ref(), "diary_open"));
            ptr::null_mut()
        }
    }
}

/// 打开或复用某个路径上的核心，并给它发一个句柄令牌。
fn open_library(path: &str) -> Result<*mut Handle, CallError> {
    if path.is_empty() {
        return Err(CallError::bridge(
            ErrorCode::InvalidArgument,
            "资料库路径为空",
        ));
    }
    let key = library_key(Path::new(path))?;
    // 登记表的锁**跨** `Core::open` 持有：两个线程同时首次打开同一个路径时，
    // 不能各自开一个核心（那会跑两遍 recover_on_open）。
    let core = {
        let mut registry = lock_or_recover(open_libraries());
        match registry.get(&key).and_then(Weak::upgrade) {
            Some(existing) => existing,
            None => {
                let opened = Arc::new(Mutex::new(Core::open(&key)?));
                registry.insert(key.clone(), Arc::downgrade(&opened));
                opened
            }
        }
    };
    let library_id = key
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_else(|| "library".to_owned());
    // 令牌永不回收（见模块文档的「生命周期」）：地址因此不会在进程内复用。
    let handle = Box::into_raw(Box::new(Handle { _token: 0 }));
    lock_or_recover(handles()).insert(handle as usize, LibraryEntry { core, library_id });
    Ok(handle)
}

/// 关闭句柄。最后一个指向同一个资料库的句柄关闭时，核心与 SQLite 连接才真正释放。
///
/// 返回：`0` 关闭成功；`1` 传了空指针；`2` 句柄无效或已经关闭过；`3` 桥面内部错误。
/// **重复关闭不崩**，只是返回 `2`。
#[no_mangle]
pub extern "C" fn diary_close(handle: *mut Handle) -> i32 {
    clear_last_error();
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        if handle.is_null() {
            record_last_error(&CallError::bridge(
                ErrorCode::InvalidArgument,
                "diary_close 收到空句柄",
            ));
            return 1;
        }
        // 先把条目摘出来再析构：不在持有登记表锁的时候做可能很慢的收尾（WAL 检查点等）。
        let entry = lock_or_recover(handles()).remove(&(handle as usize));
        match entry {
            Some(entry) => {
                drop(entry);
                0
            }
            None => {
                record_last_error(&CallError::bridge(
                    ErrorCode::InvalidState,
                    "句柄无效或已经关闭",
                ));
                2
            }
        }
    }));
    match outcome {
        Ok(code) => code,
        Err(payload) => {
            record_last_error(&panic_error(payload.as_ref(), "diary_close"));
            3
        }
    }
}

/// 调一个方法：`method_json` 形如 `{"method":"…","params":{…}}`，返回信封 JSON。
///
/// 返回的字符串由 Rust 分配，用 [`diary_string_free`] 释放；**任何**失败都体现为
/// `ok:false` 的信封，函数本身不返回 NULL（除非内存分配失败）。
///
/// # Safety
/// - `handle` 必须是 [`diary_open`] 返回过、且还没被 [`diary_close`] 之外的途径释放的句柄，
///   或者是空指针（空指针报 `invalid_argument`）；
/// - `method_json` 必须是 NUL 结尾的 UTF-8 字符串指针，或空指针。
#[no_mangle]
pub unsafe extern "C" fn diary_call(
    handle: *mut Handle,
    method_json: *const c_char,
) -> *mut c_char {
    clear_last_error();
    guard_text("diary_call", || {
        // SAFETY: 由调用方保证（见本函数的安全约定）。
        let request = unsafe { c_str_to_string(method_json, "method_json") }?;
        call_to_envelope(handle, &request)
    })
}

/// 本线程最近一条错误（JSON 对象，与信封里的 `error` 同形）；没有错误时为 NULL。
///
/// 每个导出函数进入时清空、失败时写入。返回的字符串由 Rust 分配，用
/// [`diary_string_free`] 释放。
#[no_mangle]
pub extern "C" fn diary_last_error() -> *mut c_char {
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        LAST_ERROR.with(|slot| slot.borrow().clone())
    }));
    match outcome {
        Ok(Some(text)) => to_c_string(text),
        _ => ptr::null_mut(),
    }
}

/// 释放本库返回的字符串（`diary_capi_version` / `diary_call` / `diary_last_error`）。
///
/// 空指针是允许的（no-op），这样可以少一层调用方判断。
///
/// # Safety
/// `text` 必须是本库返回、且还没被释放过的指针（或空指针）。重复释放、或传别的来源的
/// 指针都是未定义行为——与 `free()` 的约定一样。
#[no_mangle]
pub unsafe extern "C" fn diary_string_free(text: *mut c_char) {
    if text.is_null() {
        return;
    }
    #[cfg(test)]
    LIVE_STRINGS.with(|count| count.set(count.get().saturating_sub(1)));
    // SAFETY: 由调用方保证（见本函数的安全约定）。
    drop(unsafe { CString::from_raw(text) });
}

// ---------------------------------------------------------------- 调用路径

/// 解析请求、取句柄、跑方法，返回完整信封 JSON。
fn call_to_envelope(handle: *mut Handle, request: &str) -> Result<String, CallError> {
    if handle.is_null() {
        return Err(CallError::bridge(
            ErrorCode::InvalidArgument,
            "句柄指针为空（diary_call 的第一个参数）",
        ));
    }
    let (method, params) = parse_request(request)?;
    // 登记表只在取引用时锁一下：核心调用期间不能攥着它。
    let (core, library_id) = {
        let registry = lock_or_recover(handles());
        match registry.get(&(handle as usize)) {
            Some(entry) => (Arc::clone(&entry.core), entry.library_id.clone()),
            None => {
                return Err(CallError::bridge(
                    ErrorCode::InvalidState,
                    "句柄无效或已经关闭（句柄只能用 diary_open 的返回值，且关闭后不可再用）",
                ))
            }
        }
    };
    let mut core = lock_or_recover(&core);
    let data = methods::dispatch(&mut core, &library_id, &method, &params)?;
    Ok(ok_envelope(data))
}

/// 请求的形状：`{"method":"…","params":{…}}`；`params` 可省略。
#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct CallRequest {
    method: String,
    #[serde(default)]
    params: Value,
}

fn parse_request(request: &str) -> Result<(String, Value), CallError> {
    let parsed: CallRequest = serde_json::from_str(request).map_err(|error| {
        CallError::bridge(
            ErrorCode::InvalidArgument,
            format!("method_json 不是合法的请求（{error}）"),
        )
    })?;
    Ok((parsed.method, parsed.params))
}

/// 仅供本 crate 测试：拿句柄背后的核心引用，用来断言「同一路径共享一个核心」与生命周期。
#[cfg(test)]
pub(crate) fn debug_core(handle: *mut Handle) -> Option<Arc<Mutex<Core>>> {
    let registry = lock_or_recover(handles());
    registry
        .get(&(handle as usize))
        .map(|entry| Arc::clone(&entry.core))
}

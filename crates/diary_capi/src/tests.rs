//! `diary_capi` 的测试：全部走真实导出入口（`diary_open` / `diary_call` / `diary_close`），
//! 不打桩核心、不联网（关键词检索与索引重建都是纯本地的）。
//!
//! 覆盖：
//! - 端到端：打开 → 建草稿 → 保存 → 提交 → 抽取 → 检索命中 → 排索引任务 → 跑任务 → 关闭；
//! - FFI 安全：空指针 / 野指针 / 非法 UTF-8 / 非法 JSON / 未知方法 / 参数拼错 / 关闭后再用；
//! - panic 注入：`__test.panic` 只能变成 `ok:false`，进程不中止，句柄之后仍可用；
//! - 生命周期：同一路径共享核心、最后一个句柄关闭后核心真的析构、字符串计数不漏；
//! - 跨线程：两个线程并发调同一个句柄。

use std::ffi::{CStr, CString};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{json, Value};

use super::*;

// ---------------------------------------------------------------- 脚手架
//
// 下面这些辅助函数都是**安全函数**（内部各自一个 `unsafe` 块）：测试体里只在直接调
// 导出入口时才写 `unsafe {}`，这样不会出现「不必要的 unsafe 块」的告警。

/// 一次调用：请求走 `diary_call` 的 JSON 面，返回响应的原文（顺手释放字符串）。
fn call_text(handle: *mut Handle, request: &str) -> String {
    let c_request = CString::new(request).unwrap();
    // SAFETY: `handle` 与请求都来自本进程内合法的来源（调用点保证）。
    let raw = unsafe { diary_call(handle, c_request.as_ptr()) };
    assert!(!raw.is_null(), "diary_call 不该返回 NULL");
    // SAFETY: `raw` 是 diary_call 刚返回、还没释放过的指针。
    let text = unsafe { CStr::from_ptr(raw) }.to_str().unwrap().to_owned();
    // SAFETY: 同上，`raw` 只释放一次。
    unsafe { diary_string_free(raw) };
    // 文档里的请求/响应实录就是从这里抓的：
    // `DIARY_CAPI_TRANSCRIPT=1 cargo test -p diary_capi -- --nocapture`
    if std::env::var_os("DIARY_CAPI_TRANSCRIPT").is_some() {
        println!(">>> {request}\n<<< {text}");
    }
    text
}

/// 一次调用 + 解析信封。
fn call(handle: *mut Handle, method: &str, params: Value) -> Value {
    let request = json!({"method": method, "params": params}).to_string();
    let text = call_text(handle, &request);
    serde_json::from_str(&text)
        .unwrap_or_else(|error| panic!("响应不是 JSON：{error}，原文：{text}"))
}

fn expect_ok(envelope: &Value) -> Value {
    assert_eq!(envelope["ok"], json!(true), "预期成功，实际：{envelope}");
    assert_eq!(envelope["apiVersion"], json!(API_VERSION));
    envelope["data"].clone()
}

fn expect_error(envelope: &Value, code: &str) -> String {
    assert_eq!(envelope["ok"], json!(false), "预期失败，实际：{envelope}");
    assert_eq!(envelope["apiVersion"], json!(API_VERSION));
    assert_eq!(
        envelope["error"]["code"],
        json!(code),
        "错误码不对：{envelope}"
    );
    envelope["error"]["message"].as_str().unwrap().to_owned()
}

/// 打开一个只属于本次测试的资料库。
fn open_temp(tag: &str) -> (PathBuf, PathBuf, *mut Handle) {
    let dir = std::env::temp_dir().join(format!(
        "diary-capi-{tag}-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("library.sqlite");
    let handle = open(&path);
    (dir, path, handle)
}

fn open(path: &Path) -> *mut Handle {
    let c_path = CString::new(path.to_string_lossy().into_owned()).unwrap();
    // SAFETY: 路径是刚拼出来的合法 C 字符串。
    let handle = unsafe { diary_open(c_path.as_ptr()) };
    assert!(!handle.is_null(), "打开失败：{:?}", last_error());
    handle
}

/// 本线程最近一条错误的原文（没有则 None）。
fn last_error() -> Option<String> {
    let raw = diary_last_error();
    if raw.is_null() {
        return None;
    }
    // SAFETY: `raw` 是 diary_last_error 刚返回、还没释放过的指针。
    let text = unsafe { CStr::from_ptr(raw) }.to_str().unwrap().to_owned();
    // SAFETY: 同上，只释放一次。
    unsafe { diary_string_free(raw) };
    Some(text)
}

fn live_strings() -> usize {
    LIVE_STRINGS.with(std::cell::Cell::get)
}

/// 建一条「草稿 → 保存 → 提交 → 抽取」的记录，返回 `(capture_id, source_id)`。
fn seed_committed_capture(handle: *mut Handle, text: &str) -> (String, String) {
    let capture = expect_ok(&call(
        handle,
        "captures.createDraft",
        json!({
            "occurred_at": "2026-10-17T09:30:00Z",
            "time_zone": "Asia/Shanghai",
            "utc_offset_minutes": 480,
            "operation_id": "op-capi-draft",
        }),
    ));
    let capture_id = capture["id"].as_str().unwrap().to_owned();
    assert_eq!(capture["revision"], json!(1));
    assert_eq!(capture["state"], json!("draft"));

    let saved = expect_ok(&call(
        handle,
        "captures.saveDraft",
        json!({
            "capture_id": capture_id,
            "text": text,
            "expected_revision": 1,
            "operation_id": "op-capi-save",
        }),
    ));
    assert_eq!(saved["revision"], json!(2));
    assert_eq!(saved["durable"], json!(true));

    let committed = expect_ok(&call(
        handle,
        "captures.commit",
        json!({
            "capture_id": capture_id,
            "expected_revision": 2,
            "operation_id": "op-capi-commit",
        }),
    ));
    let source_id = committed["original_text_revision"]["source_id"]
        .as_str()
        .unwrap()
        .to_owned();
    expect_ok(&call(
        handle,
        "sources.extract",
        json!({"source_ref": source_id}),
    ));
    (capture_id, source_id)
}

// ---------------------------------------------------------------- 测试

/// 版本戳与 last-error 的生老病死。
#[test]
fn version_and_last_error_travel_through_the_c_api() {
    let raw = diary_capi_version();
    assert!(!raw.is_null());
    // SAFETY: `raw` 是 diary_capi_version 刚返回、还没释放过的指针。
    let text = unsafe { CStr::from_ptr(raw) }.to_str().unwrap().to_owned();
    // SAFETY: 同上，只释放一次。
    unsafe { diary_string_free(raw) };
    assert!(text.starts_with("diary_capi/"), "{text}");
    assert!(text.contains("apiVersion=1.0"), "{text}");

    let (dir, _path, handle) = open_temp("version");

    // 失败：message 进 last-error（NULL 返回的入口只能靠它把原因带出去）。
    let envelope: Value = serde_json::from_str(&call_text(handle, "这不是 JSON")).unwrap();
    let message = expect_error(&envelope, "invalid_argument");
    assert!(message.starts_with("C ABI："), "{message}");
    let recorded = last_error().expect("失败之后 last-error 必须有内容");
    assert!(recorded.contains("invalid_argument"), "{recorded}");

    // 成功入口清空 last-error：上一次失败不该被当成这一次的。
    assert!(call_text(handle, "{\"method\":\"jobs.status\"}").contains("\"ok\":true"));
    assert!(last_error().is_none(), "成功之后 last-error 应当被清空");

    assert_eq!(diary_close(handle), 0);
    std::fs::remove_dir_all(dir).unwrap();
}

/// 空指针、野指针、非法 UTF-8、非法 JSON、未知方法、拼错的参数：全都是一条错误，不是崩溃。
#[test]
fn bad_input_never_crashes_and_always_reports_an_error() {
    let (dir, _path, handle) = open_temp("bad-input");
    let ok_request = "{\"method\":\"jobs.status\"}";

    // 空句柄 / 空请求指针：两者都必须是一条错误。
    let raw = diary_call_raw(ptr::null_mut(), ok_request);
    // SAFETY: `raw` 是刚返回的字符串。
    let text = unsafe { CStr::from_ptr(raw) }.to_str().unwrap().to_owned();
    expect_error(&serde_json::from_str(&text).unwrap(), "invalid_argument");
    // SAFETY: 只释放一次。
    unsafe { diary_string_free(raw) };
    let no_handle = CString::new(ok_request).unwrap();
    // SAFETY: 句柄为空；请求合法。
    let raw = unsafe { diary_call(ptr::null_mut(), no_handle.as_ptr()) };
    // SAFETY: `raw` 是刚返回的字符串。
    unsafe { diary_string_free(raw) };

    let null_request: Value = serde_json::from_str(&call_text(handle, "")).unwrap(); // 空串也不是合法请求
    expect_error(&null_request, "invalid_argument");
    // SAFETY: 故意传空请求指针。
    let raw = unsafe { diary_call(handle, ptr::null()) };
    let text = unsafe { CStr::from_ptr(raw) }.to_str().unwrap().to_owned();
    expect_error(&serde_json::from_str(&text).unwrap(), "invalid_argument");
    unsafe { diary_string_free(raw) };

    // 从来没打开过的「句柄」：指针值查不到条目，直接报错（桥面从不解引用它）。
    let garbage = 0x1234_5678_usize as *mut Handle;
    let envelope: Value = serde_json::from_str(&call_text(garbage, ok_request)).unwrap();
    expect_error(&envelope, "invalid_state");

    // 非法 UTF-8 的请求。
    let invalid_utf8 = CString::new(vec![0xff_u8, 0xfe]).unwrap();
    // SAFETY: 指针合法，只是内容不是 UTF-8。
    let raw = unsafe { diary_call(handle, invalid_utf8.as_ptr()) };
    let text = unsafe { CStr::from_ptr(raw) }.to_str().unwrap().to_owned();
    expect_error(&serde_json::from_str(&text).unwrap(), "invalid_argument");
    unsafe { diary_string_free(raw) };

    // 非法 JSON / 缺 method / method 类型不对 / 多余的顶层字段。
    for bad in [
        "not json at all",
        "{\"params\":{}}",
        "{\"method\":123}",
        "{\"method\":\"jobs.status\",\"extra\":1}",
    ] {
        let envelope: Value = serde_json::from_str(&call_text(handle, bad)).unwrap();
        let message = expect_error(&envelope, "invalid_argument");
        assert!(message.starts_with("C ABI："), "{message}");
    }

    // 未知方法：message 里点名，并附上已接入清单。
    let unknown = expect_error(
        &call(handle, "captures.purge", json!({"id": "cap_1"})),
        "invalid_argument",
    );
    assert!(unknown.contains("未接入"), "{unknown}");
    assert!(unknown.contains("captures.createDraft"), "{unknown}");

    // 参数拼错（deny_unknown_fields）与必填缺失。
    let typo = expect_error(
        &call(
            handle,
            "captures.saveDraft",
            json!({"capture_id": "cap_1", "text": "x", "expected_revisions": 1, "operation_id": "op"}),
        ),
        "invalid_argument",
    );
    assert!(typo.contains("参数不合法"), "{typo}");
    let missing = expect_error(
        &call(handle, "jobs.runDue", json!({"trigger": "manual"})),
        "invalid_argument",
    );
    assert!(missing.contains("budget_ms"), "{missing}");

    // 无参方法收到参数：不静默忽略。
    let extra = expect_error(
        &call(handle, "jobs.status", json!({"limit": 5})),
        "invalid_argument",
    );
    assert!(extra.contains("不接受参数"), "{extra}");

    // 契约风格的 camelCase 别名是可用的。
    let draft = expect_ok(&call(
        handle,
        "captures.createDraft",
        json!({
            "occurredAt": "2026-10-17T09:30:00Z",
            "timeZone": "Asia/Shanghai",
            "utcOffsetMinutes": 480,
            "operationId": "op-alias",
        }),
    ));
    assert_eq!(draft["state"], json!("draft"));

    // 打开失败的路径也要能读原因（返回 NULL + last-error）。
    let missing_dir = std::env::temp_dir().join("diary-capi-不存在的目录");
    let c_path = CString::new(missing_dir.join("x.sqlite").to_string_lossy().into_owned()).unwrap();
    // SAFETY: 路径合法；目录不存在时应当返回 NULL。
    let failed = unsafe { diary_open(c_path.as_ptr()) };
    assert!(failed.is_null());
    let reason = last_error().expect("打开失败必须留下原因");
    assert!(reason.contains("\"code\""), "{reason}");

    // 关闭相关的边界。
    assert_eq!(diary_close(ptr::null_mut()), 1);
    assert_eq!(diary_close(handle), 0);
    assert_eq!(diary_close(handle), 2, "重复关闭要报 2，不是崩溃");
    expect_error(&call(handle, "jobs.status", Value::Null), "invalid_state");
    std::fs::remove_dir_all(dir).unwrap();
}

/// 端到端：打开 → 建草稿 → 保存 → 提交 → 抽取 → 检索命中 → 排索引任务 → 跑任务 → 关闭。
///
/// 全程只走 `diary_call` 的 JSON 面。
#[test]
fn end_to_end_draft_to_search_to_jobs() {
    let (dir, _path, handle) = open_temp("e2e");
    let text = "今天在鸿蒙上验证核心的 C ABI，关键词是鹅掌楸与检索。";

    let info = expect_ok(&call(handle, "core.info", Value::Null));
    assert_eq!(info["api_version"], json!("1.0"));
    assert_eq!(info["library_id"], json!("library"));
    assert!(info["data_schema_version"].as_i64().unwrap() > 0);
    let capabilities = info["capabilities"].as_array().unwrap();
    assert!(capabilities.iter().any(|name| name == "jobs.runDue"));
    assert_eq!(info["capture_count"], json!(0));

    // 建草稿 + 保存 + 提交 + 抽取（幂等键走 JSON 面）。
    let (capture_id, source_id) = seed_committed_capture(handle, text);

    // 幂等：同一个 operationId 再来一次，返回同一条记录（证明参数确实透传到了核心）。
    let replay = expect_ok(&call(
        handle,
        "captures.createDraft",
        json!({
            "occurred_at": "2026-10-17T09:30:00Z",
            "time_zone": "Asia/Shanghai",
            "utc_offset_minutes": 480,
            "operation_id": "op-capi-draft",
        }),
    ));
    assert_eq!(replay["id"], json!(capture_id));

    // 抽取：派生内容落库（同一个来源再抽一次是幂等的，不重复产出）。
    let extracted = expect_ok(&call(
        handle,
        "sources.extract",
        json!({"source_ref": source_id}),
    ));
    assert!(
        !extracted["segments"].as_array().unwrap().is_empty(),
        "抽取应当产出片段：{extracted}"
    );

    // 检索：关键词那一路拿到命中（只给 query，其余按默认口径）。
    let snapshot = expect_ok(&call(
        handle,
        "search.start",
        json!({"request": {"query": "关键词", "mode": "keyword", "page_size": 10}, "query_revision": 1}),
    ));
    let results = snapshot["results"].as_array().unwrap();
    assert!(!results.is_empty(), "应当命中刚保存的正文：{snapshot}");
    assert!(
        snapshot["session_id"]
            .as_str()
            .unwrap()
            .starts_with("search_"),
        "{snapshot}"
    );

    // 排一条整库索引重建任务，再用 runDue 把它跑完。
    let job = expect_ok(&call(
        handle,
        "indexes.rebuild",
        json!({"scope": "all", "reason": "capi e2e", "operation_id": "op-capi-index"}),
    ));
    assert_eq!(job["kind"], json!("index"));
    assert_eq!(job["state"], json!("queued"));
    let before = expect_ok(&call(handle, "jobs.status", Value::Null));
    assert_eq!(before["queued"], json!(1));

    let summary = expect_ok(&call(
        handle,
        "jobs.runDue",
        json!({"budget_ms": 30_000, "trigger": "manual"}),
    ));
    assert_eq!(summary["processed"], json!(1), "{summary}");
    assert_eq!(summary["succeeded"], json!(1), "{summary}");
    assert_eq!(summary["due_backlog"], json!(0), "{summary}");
    assert_eq!(summary["trigger"], json!("manual"));
    let after = expect_ok(&call(handle, "jobs.status", Value::Null));
    assert_eq!(after["queued"], json!(0));
    assert_eq!(after["succeeded"], json!(1));

    let info = expect_ok(&call(handle, "core.info", Value::Null));
    assert_eq!(info["capture_count"], json!(1));
    assert!(info["last_event_sequence"].as_i64().unwrap() > 0);

    assert_eq!(diary_close(handle), 0);
    std::fs::remove_dir_all(dir).unwrap();
}

/// 生命周期：同一路径共享核心、最后一个句柄关闭后核心真的析构、关闭后再用是明确错误。
#[test]
fn close_releases_the_core_only_after_the_last_handle() {
    let (dir, path, first) = open_temp("lifecycle");
    let second = open(&path);
    assert_ne!(first, second, "每次打开发一个独立令牌");

    let first_core = debug_core(first).expect("第一个句柄在登记表里");
    let second_core = debug_core(second).expect("第二个句柄在登记表里");
    assert!(
        Arc::ptr_eq(&first_core, &second_core),
        "同一个路径必须共享一个核心（两个 Core::open 会跑两遍恢复）"
    );
    drop(first_core);
    drop(second_core);

    let probe = debug_core(first).unwrap();
    assert_eq!(
        Arc::strong_count(&probe),
        3,
        "两个句柄 + 这一次探针，正好三个强引用"
    );
    let weak = Arc::downgrade(&probe);
    drop(probe);

    expect_ok(&call(first, "jobs.status", Value::Null));
    assert_eq!(diary_close(first), 0);
    expect_error(&call(first, "jobs.status", Value::Null), "invalid_state");
    assert!(weak.upgrade().is_some(), "还有句柄活着，核心不该析构");

    expect_ok(&call(second, "jobs.status", Value::Null));
    assert_eq!(diary_close(second), 0);
    assert!(weak.upgrade().is_none(), "最后一个句柄关闭后核心必须析构");

    // 库文件真的放开了：能重新打开，而且读得到东西。
    let reopened = open(&path);
    expect_ok(&call(reopened, "jobs.status", Value::Null));
    assert_eq!(diary_close(reopened), 0);
    std::fs::remove_dir_all(dir).unwrap();
}

/// 所有权：返回的每个字符串都能被 `diary_string_free` 释放，且计数回到原处。
#[test]
fn returned_strings_are_freed_by_diary_string_free() {
    let baseline = live_strings();
    let (dir, _path, handle) = open_temp("strings");

    let mut pointers = vec![diary_capi_version()];
    pointers.push(diary_call_raw(handle, "{\"method\":\"core.info\"}"));
    assert!(live_strings() > baseline, "借出去的字符串要计数");

    for pointer in pointers {
        assert!(!pointer.is_null());
        // SAFETY: 每个指针都是刚借出去、还没释放过的。
        unsafe { diary_string_free(pointer) };
    }
    // SAFETY: 空指针是允许的 no-op。
    unsafe { diary_string_free(ptr::null_mut()) };
    assert_eq!(live_strings(), baseline, "全部释放后必须回到原处");

    // 失败路径同样把字符串交出去，也要能释放干净。
    let raw = diary_call_raw(handle, "{");
    assert!(!raw.is_null());
    // SAFETY: 刚借出去，未释放。
    unsafe { diary_string_free(raw) };
    let recorded = diary_last_error();
    assert!(!recorded.is_null());
    // SAFETY: 刚借出去，未释放。
    unsafe { diary_string_free(recorded) };
    assert_eq!(live_strings(), baseline);

    assert_eq!(diary_close(handle), 0);
    std::fs::remove_dir_all(dir).unwrap();
}

/// 只调用、不解析：留给所有权测试逐个释放。
fn diary_call_raw(handle: *mut Handle, request: &str) -> *mut c_char {
    let request = CString::new(request).unwrap();
    // SAFETY: 句柄与请求都来自本测试内合法的来源。
    unsafe { diary_call(handle, request.as_ptr()) }
}

/// 核心的错误码原样过来：不改写、不合并、不新增。
#[test]
fn core_error_codes_pass_through_unchanged() {
    let (dir, _path, handle) = open_temp("codes");

    // not_found：来源不存在。
    let not_found = expect_error(
        &call(
            handle,
            "sources.extract",
            json!({"source_ref": "src_不存在"}),
        ),
        "not_found",
    );
    assert!(not_found.contains("找不到"), "{not_found}");

    // revision_conflict：乐观锁不匹配（核心的既有码，桥面不做二次翻译）。
    let draft = expect_ok(&call(
        handle,
        "captures.createDraft",
        json!({
            "time_zone": "Asia/Shanghai",
            "utc_offset_minutes": 480,
            "operation_id": "op-codes-draft",
        }),
    ));
    let conflict = expect_error(
        &call(
            handle,
            "captures.saveDraft",
            json!({
                "capture_id": draft["id"],
                "text": "x",
                "expected_revision": 99,
                "operation_id": "op-codes-save",
            }),
        ),
        "revision_conflict",
    );
    assert!(conflict.contains("版本冲突"), "{conflict}");

    // idempotency_conflict：同一个 operationId 换了内容。
    let first = expect_ok(&call(
        handle,
        "captures.saveDraft",
        json!({
            "capture_id": draft["id"],
            "text": "第一版",
            "expected_revision": 1,
            "operation_id": "op-codes-idem",
        }),
    ));
    assert_eq!(first["revision"], json!(2));
    let idem = expect_error(
        &call(
            handle,
            "captures.saveDraft",
            json!({
                "capture_id": draft["id"],
                "text": "换了内容",
                "expected_revision": 2,
                "operation_id": "op-codes-idem",
            }),
        ),
        "idempotency_conflict",
    );
    assert!(idem.contains("幂等冲突"), "{idem}");

    assert_eq!(diary_close(handle), 0);
    std::fs::remove_dir_all(dir).unwrap();
}

/// panic 注入：核心 panic 变成 `ok:false`，进程不中止；句柄之后仍然可用。
#[test]
fn core_panic_becomes_an_error_and_the_handle_survives() {
    let (dir, _path, handle) = open_temp("panic");
    let message = expect_error(&call(handle, "__test.panic", Value::Null), "invalid_state");
    assert!(message.contains("panic 已被拦截"), "{message}");
    assert!(message.contains("注入的 panic"), "{message}");
    assert!(message.contains("diary_call"), "{message}");

    // panic 让核心那把锁中毒；桥面按「取回内部值」处理，句柄不该因此变成砖头。
    expect_ok(&call(handle, "jobs.status", Value::Null));

    assert_eq!(diary_close(handle), 0);
    std::fs::remove_dir_all(dir).unwrap();
}

/// 跨线程：两个线程并发调同一个句柄，核心串行，结果正确。
#[test]
fn two_threads_can_share_one_handle() {
    let (dir, _path, handle) = open_temp("threads");
    let text = "并发调用同一个句柄，关键词是并行检索。";
    let (_capture_id, _source_id) = seed_committed_capture(handle, text);

    // 写线程要写的是**草稿**（已提交的记录不允许再改草稿）：先建一条 revision = 1 的草稿。
    let draft = expect_ok(&call(
        handle,
        "captures.createDraft",
        json!({
            "time_zone": "Asia/Shanghai",
            "utc_offset_minutes": 480,
            "operation_id": "op-thread-draft",
        }),
    ));
    let draft_id = draft["id"].as_str().unwrap().to_owned();

    // 句柄是裸指针（不是 Send）；跨线程只搬它的地址值，线程里再还原。
    let key = handle as usize;
    let writer_capture = draft_id.clone();
    let (hits, saved) = std::thread::scope(|scope| {
        let reader = scope.spawn(move || {
            let handle = key as *mut Handle;
            let mut hits = 0;
            for _ in 0..15 {
                expect_ok(&call(handle, "core.info", Value::Null));
                let snapshot = expect_ok(&call(
                    handle,
                    "search.start",
                    json!({"request": {"query": "并行", "mode": "keyword", "page_size": 5}, "query_revision": 7}),
                ));
                hits += snapshot["results"].as_array().unwrap().len();
            }
            hits
        });
        let writer = scope.spawn(move || {
            let handle = key as *mut Handle;
            for _ in 0..15 {
                expect_ok(&call(handle, "jobs.status", Value::Null));
            }
            // 与读线程并发地写一次：核心串行，版本号必须从 1 推进到 2。
            expect_ok(&call(
                handle,
                "captures.saveDraft",
                json!({
                    "capture_id": writer_capture,
                    "text": "并发写入的正文",
                    "expected_revision": 1,
                    "operation_id": "op-thread",
                }),
            ))
        });
        (reader.join().unwrap(), writer.join().unwrap())
    });
    assert!(hits > 0, "并发读也要拿到命中");
    assert_eq!(saved["revision"], json!(2));
    assert_eq!(saved["durable"], json!(true));

    assert_eq!(diary_close(handle), 0);
    std::fs::remove_dir_all(dir).unwrap();
}

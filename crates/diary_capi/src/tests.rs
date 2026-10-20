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

/// 建一条已提交并抽取过的记录（可指定 `occurred_at`），返回 `capture_id`。
///
/// 与 [`seed_committed_capture`] 的区别：幂等键由调用方给，所以同一个库上可以建多条
/// （「看今天」/历史列表/搜索翻页都需要多条）。
fn seed_capture_at(
    handle: *mut Handle,
    operation_id: &str,
    occurred_at: &str,
    text: &str,
) -> String {
    let capture = expect_ok(&call(
        handle,
        "captures.createDraft",
        json!({
            "occurred_at": occurred_at,
            "time_zone": "Asia/Shanghai",
            "utc_offset_minutes": 480,
            "operation_id": operation_id,
        }),
    ));
    let capture_id = capture["id"].as_str().unwrap().to_owned();

    expect_ok(&call(
        handle,
        "captures.saveDraft",
        json!({
            "capture_id": capture_id,
            "text": text,
            "expected_revision": 1,
            "operation_id": format!("{operation_id}-save"),
        }),
    ));
    let committed = expect_ok(&call(
        handle,
        "captures.commit",
        json!({
            "capture_id": capture_id,
            "expected_revision": 2,
            "operation_id": format!("{operation_id}-commit"),
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
    capture_id
}

/// 用核心自己的入口导入一个**带原件**的文件来源，返回 `(source_id, asset_id)`。
///
/// `imports.*` 还没接到 C ABI（属于后面的块），但 `sources.locate` 的可用路径与
/// `backups.inspect` 的损坏包校验都需要一条真实原件。与 [`enqueue_for_test`] 同一个
/// 理由：读/写仍全部走 C ABI，这里只借用核心入口造前置数据。
fn seed_file_source(
    handle: *mut Handle,
    name: &str,
    bytes: &[u8],
    operation: &str,
) -> (String, String) {
    let core = debug_core(handle).expect("句柄必须在登记表里");
    let mut core = core.lock().unwrap();
    let capture_id = core
        .create_draft(diary_core::CreateDraftInput {
            occurred_at: Some(chrono::Utc::now()),
            time_zone: "Asia/Shanghai",
            utc_offset_minutes: 480,
            operation_id: &format!("{operation}-create"),
        })
        .unwrap()
        .id;
    let ticket = core
        .prepare_import(diary_core::ImportRequest {
            capture_id: &capture_id,
            display_name: name,
            mime_hint: Some("text/plain"),
            size_hint: Some(bytes.len() as i64),
            origin: diary_core::ImportOrigin::Picker,
            operation_id: &format!("{operation}-prepare"),
        })
        .unwrap();
    std::fs::write(&ticket.staging_ticket, bytes).unwrap();
    let status = core
        .finish_import(
            &ticket.import_id,
            &ticket.staging_ticket,
            diary_core::ImportManifest {
                copied_bytes: bytes.len() as i64,
                sha256: sha256_hex(bytes),
                detected_mime: "text/plain".to_owned(),
                original_name: name.to_owned(),
            },
        )
        .unwrap();
    let asset_id = status.asset_id.expect("导入完成后应当有资产");
    let source_id = core
        .get_capture(&capture_id)
        .unwrap()
        .ordered_source_ids
        .last()
        .cloned()
        .expect("导入的材料应当挂到记录上");
    (source_id, asset_id)
}

/// 用核心自己的入口排一条任务（测试脚手架）。
///
/// 为什么不用 C ABI 排：要测的是 `jobs.list/get/retry/...` 的**读取与人工动作**，
/// 而「没有执行者」的任务只能从核心入队。读/写仍全部走 C ABI，不绕过桥面。
fn enqueue_for_test(handle: *mut Handle, kind: &str, target_ids: &[&str]) -> String {
    let core = debug_core(handle).expect("句柄必须在登记表里");
    let mut core = core.lock().unwrap();
    let mut job = diary_core::NewJob::new(kind, diary_core::JobPriority::BackgroundExtract);
    if !target_ids.is_empty() {
        job = job.targeting(target_ids.iter().map(|id| (*id).to_owned()).collect());
    }
    core.enqueue_job(job).expect("入队应当成功").id
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
    assert!(text.contains("apiVersion=1.1"), "{text}");

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
        &call(handle, "imports.prepare", json!({"capture_id": "cap_1"})),
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
    // 同一类写死：用常量，别把下一次抬版本又变成一次「main 变红」。
    assert_eq!(info["api_version"], json!(API_VERSION));
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
    // **线格式**断言：`capture_id` 真的过了 FFI 边界（`check_arkts_capi_contract.py`
    // 只核对方法/错误码/符号，不检查 payload，所以这里是唯一证据），且两种命中的
    // 口径与核心契约一致：片段命中是「这篇片段所属的记录」，记录文字命中才等于
    // `group_id`。只查 `results` 非空的话，字段序列化成空串、或者干脆还是
    // `source_id`，测试都照样绿。
    let segment_hit = results
        .iter()
        .find(|hit| hit["group_id"] == json!(&source_id))
        .unwrap_or_else(|| panic!("应当有一条片段命中（group_id = source_id）：{snapshot}"));
    assert_eq!(
        segment_hit["source_id"],
        json!(&source_id),
        "片段命中的 source_id 是来源篇的 id：{segment_hit}"
    );
    assert_eq!(
        segment_hit["capture_id"],
        json!(&capture_id),
        "片段命中的 capture_id 是这篇片段所属的记录，不是来源篇：{segment_hit}"
    );
    assert_ne!(
        segment_hit["capture_id"], segment_hit["group_id"],
        "片段命中上 capture_id 与 group_id 不是一回事：{segment_hit}"
    );

    let capture_hit = results
        .iter()
        .find(|hit| hit["hit_id"] == json!(format!("cap_{capture_id}")))
        .unwrap_or_else(|| {
            panic!("应当有一条记录文字命中（hit_id = cap_<capture_id>）：{snapshot}")
        });
    assert_eq!(
        capture_hit["capture_id"],
        json!(&capture_id),
        "记录文字命中的 capture_id 是这篇记录：{capture_hit}"
    );
    assert_eq!(
        capture_hit["capture_id"], capture_hit["group_id"],
        "记录文字命中天然自成一组，capture_id 与 group_id 相同：{capture_hit}"
    );
    assert_eq!(
        capture_hit["hit_id"],
        json!(format!("cap_{capture_id}")),
        "记录文字命中的 hit_id 由 capture_id 派生：{capture_hit}"
    );

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

/// `indexes.rebuildSemantic`（契约 4.4，C ABI 之前没接）+ `core.info.runtime_config` 的形状。
///
/// 语义任务只是**排上**，没有模型时它会等配置（`waiting_configuration`），
/// 所以这里不跑 `jobs.runDue`——那会把一条等模型的任务写进队列状态，与本测试无关。
#[test]
fn semantic_rebuild_and_runtime_config_are_wired() {
    let (dir, _path, handle) = open_temp("semantic-config");

    let info = expect_ok(&call(handle, "core.info", Value::Null));
    let capabilities = info["capabilities"].as_array().unwrap();
    assert!(capabilities
        .iter()
        .any(|name| name == "indexes.rebuildSemantic"));

    // 配置报告的形状：三个对象 + 每格的来源字符串。
    let config = &info["runtime_config"];
    assert!(config["file"].is_object(), "{config}");
    assert!(config["file"]["present"].is_boolean());
    assert!(config["semantic"]["state"].is_string());
    assert!(config["semantic"]["configSource"].is_string());
    assert!(config["semantic"]["modelDirSource"].is_string());
    assert!(config["semantic"]["ortDylibSource"].is_string());
    assert!(config["transcription"]["state"].is_string());
    assert!(config["transcription"]["configSource"].is_string());
    assert!(config["transcription"]["engineLibSource"].is_string());
    let env_clean = [
        "DIARY_MODEL_DIR",
        "DIARY_ORT_DYLIB",
        "DIARY_ASR_MODEL_DIR",
        "DIARY_ASR_ENGINE_LIB",
    ]
    .iter()
    .all(|key| std::env::var_os(key).is_none());
    if env_clean {
        assert_eq!(config["semantic"]["configSource"], json!("absent"));
        assert_eq!(config["semantic"]["state"], json!("absent"));
        assert_eq!(config["file"]["present"], json!(false));
    }

    let job = expect_ok(&call(
        handle,
        "indexes.rebuildSemantic",
        json!({"reason": "capi semantic", "operation_id": "op-capi-semantic"}),
    ));
    assert_eq!(job["kind"], json!("semantic_index"));
    assert_eq!(job["state"], json!("queued"));
    // 幂等：同一个 (原因, operationId) 重放返回同一条任务。
    let replay = expect_ok(&call(
        handle,
        "indexes.rebuildSemantic",
        json!({"reason": "capi semantic", "operation_id": "op-capi-semantic"}),
    ));
    assert_eq!(replay["id"], job["id"]);

    // 缺参数要报 invalid_argument，不能静默排队。
    expect_error(
        &call(handle, "indexes.rebuildSemantic", json!({"reason": "x"})),
        "invalid_argument",
    );
    // 不认识的字段也要报错（deny_unknown_fields）。
    expect_error(
        &call(
            handle,
            "indexes.rebuildSemantic",
            json!({"reason": "x", "operation_id": "y", "scope": "all"}),
        ),
        "invalid_argument",
    );

    assert_eq!(diary_close(handle), 0);
    std::fs::remove_dir_all(dir).unwrap();
}

/// 录音交接：prepare → registerSegment → updateState → finalize → 重开 → recover。
///
/// 片段文件按平台层约定写：第 0 段是「44 字节 PCM WAV 头 + PCM」，后续段是裸 PCM。
/// 头里的两个长度字段用流式 WAV 的 `0xFFFFFFFF`（总长要到停止时才确定）；核心的
/// `recordings.finalize` 只把字节按序拼接，所以拼出来仍是一条**单头**的 PCM WAV。
#[test]
fn recordings_flow_prepare_register_finalize_and_recover() {
    let (dir, path, handle) = open_temp("recordings");

    let capture = expect_ok(&call(
        handle,
        "captures.createDraft",
        json!({
            "occurred_at": "2026-10-18T09:00:00Z",
            "time_zone": "Asia/Shanghai",
            "utc_offset_minutes": 480,
            "operation_id": "op-capi-rec-capture",
        }),
    ));
    let capture_id = capture["id"].as_str().unwrap().to_owned();

    let ticket = expect_ok(&call(
        handle,
        "recordings.prepare",
        json!({"capture_id": capture_id, "operation_id": "op-capi-rec-prepare"}),
    ));
    let recording_id = ticket["recording_id"].as_str().unwrap().to_owned();
    let asset_id = ticket["asset_id"].as_str().unwrap().to_owned();
    let staging = PathBuf::from(ticket["staging_ticket"].as_str().unwrap());
    assert!(
        staging.starts_with(&dir),
        "暂存目录必须在资料库根目录下：{}",
        staging.display()
    );

    // 第 0 段：WAV 头 + 1 秒 16 kHz/单声道/16 bit PCM（32,000 B）。
    let mut first = pcm_wav_header();
    first.extend(vec![0x5A_u8; 32_000]);
    std::fs::write(staging.join("segment-000000.wav"), &first).unwrap();

    let first_receipt = expect_ok(&call(
        handle,
        "recordings.registerSegment",
        json!({
            "recording_id": recording_id,
            "segment_index": 0,
            "segment": {
                "segment_id": "seg-0",
                "relative_file_name": "segment-000000.wav",
                "duration_ms": 1000,
                "sha256": sha256_hex(&first),
                "byte_size": first.len(),
                "closed_at": "2026-10-18T09:00:01Z",
            },
        }),
    ));
    assert_eq!(first_receipt["segment_index"], json!(0));
    assert_eq!(first_receipt["durable"], json!(true));

    // 同序号、同内容重复登记：返回原回执（幂等由核心保证，不是桥面假装成功）。
    let replay = expect_ok(&call(
        handle,
        "recordings.registerSegment",
        json!({
            "recording_id": recording_id,
            "segment_index": 0,
            "segment": {
                "segment_id": "seg-0",
                "relative_file_name": "segment-000000.wav",
                "duration_ms": 1000,
                "sha256": sha256_hex(&first),
                "byte_size": first.len(),
                "closed_at": "2026-10-18T09:00:01Z",
            },
        }),
    ));
    assert_eq!(replay["recorded_at"], first_receipt["recorded_at"]);

    let session = expect_ok(&call(
        handle,
        "recordings.updateState",
        json!({
            "recording_id": recording_id,
            "state": "recording",
            "wall_clock": "2026-10-18T09:00:01Z",
            "elapsed_ms": 1000,
            "persisted_through_ms": 1000,
            "input_device_changed": false,
            "message": Value::Null,
        }),
    ));
    assert_eq!(session["state"], json!("recording"));
    assert_eq!(session["segment_count"], json!(1));

    // 第 1 段：裸 PCM（0.5 秒）。拼接后应是一条完整 WAV。
    let second = vec![0x33_u8; 16_000];
    std::fs::write(staging.join("segment-000001.pcm"), &second).unwrap();
    expect_ok(&call(
        handle,
        "recordings.registerSegment",
        json!({
            "recording_id": recording_id,
            "segment_index": 1,
            "segment": {
                "segment_id": "seg-1",
                "relative_file_name": "segment-000001.pcm",
                "duration_ms": 500,
                "sha256": sha256_hex(&second),
                "byte_size": second.len(),
            },
        }),
    ));

    let finalized = expect_ok(&call(
        handle,
        "recordings.finalize",
        json!({
            "recording_id": recording_id,
            "last_segment_index": 1,
            "end_reason": "user_stop",
        }),
    ));
    assert_eq!(finalized["recording_id"], json!(recording_id));
    assert_eq!(finalized["capture_id"], json!(capture_id));
    assert_eq!(
        finalized["capture_revision"],
        json!(2),
        "new field: finalize must expose the authoritative capture revision"
    );
    assert_eq!(finalized["asset_id"], json!(asset_id));
    assert_eq!(finalized["state"], json!("saved"));
    assert_eq!(finalized["segment_count"], json!(2));
    assert_eq!(finalized["transcription_queued"], json!(true));
    assert_eq!(finalized["logical_duration_ms"], json!(1500));
    // 逻辑音频落到 blobs/ 下（按内容哈希分桶）；大小 = 44 字节头 + 48,000 字节 PCM。
    let logical_bytes = 44 + 32_000 + 16_000;
    let blob = find_file_with_len(&dir.join("blobs"), logical_bytes as u64)
        .expect("finalize 之后应当有一条完整的逻辑音频 blob");
    let blob_bytes = std::fs::read(&blob).unwrap();
    assert_eq!(&blob_bytes[0..4], b"RIFF");
    assert_eq!(&blob_bytes[8..12], b"WAVE");
    assert_eq!(blob_bytes.len(), logical_bytes);

    // 平台层用 finalize 返回的 revision 提交，记录才离开 draft。
    let committed = expect_ok(&call(
        handle,
        "captures.commit",
        json!({
            "capture_id": finalized["capture_id"],
            "expected_revision": finalized["capture_revision"],
            "operation_id": "op-capi-rec-commit",
        }),
    ));
    assert_eq!(committed["capture"]["state"], json!("committed"));
    assert_eq!(committed["capture"]["revision"], json!(3));

    // 最终化之后不能再上报状态（核心会拒绝，桥面不吞掉）。
    let closed_session = expect_error(
        &call(
            handle,
            "recordings.updateState",
            json!({
                "recording_id": recording_id,
                "state": "saved",
                "wall_clock": "2026-10-18T09:00:02Z",
                "elapsed_ms": 1500,
                "persisted_through_ms": 1500,
            }),
        ),
        "invalid_state",
    );
    assert!(closed_session.contains("最终化"), "{closed_session}");

    // 第二条录音：登记一段后直接关库（模拟进程被杀），重开后 recover 必须如实报告。
    let ticket2 = expect_ok(&call(
        handle,
        "recordings.prepare",
        json!({"capture_id": capture_id, "operation_id": "op-capi-rec-prepare-2"}),
    ));
    let recording2 = ticket2["recording_id"].as_str().unwrap().to_owned();
    let staging2 = PathBuf::from(ticket2["staging_ticket"].as_str().unwrap());
    let mut first2 = pcm_wav_header();
    first2.extend(vec![0x11_u8; 32_000]);
    std::fs::write(staging2.join("segment-000000.wav"), &first2).unwrap();
    expect_ok(&call(
        handle,
        "recordings.registerSegment",
        json!({
            "recording_id": recording2,
            "segment_index": 0,
            "segment": {
                "segment_id": "seg-0",
                "relative_file_name": "segment-000000.wav",
                "duration_ms": 1000,
                "sha256": sha256_hex(&first2),
                "byte_size": first2.len(),
            },
        }),
    ));

    // 采集器最后一次上报：收到 1500 ms，但只有第 0 段（1000 ms）登记成功。
    expect_ok(&call(
        handle,
        "recordings.updateState",
        json!({
            "recording_id": recording2,
            "state": "recording",
            "wall_clock": "2026-10-18T09:00:03Z",
            "elapsed_ms": 1500,
            "persisted_through_ms": 1000,
            "input_device_changed": false,
        }),
    ));

    // 关掉唯一句柄 = 核心析构 = 进程被杀；重开同一路径触发 recover_on_open。
    assert_eq!(diary_close(handle), 0);
    let reopened = open(&path);
    let recovery = expect_ok(&call(reopened, "recordings.recover", json!({})));
    assert_eq!(recovery["recording_id"], json!(recording2));
    assert_eq!(recovery["state"], json!("recoverable"));
    assert_eq!(recovery["closed_segment_indexes"], json!([0]));
    // 已登记 1000 ms 才是「已持久化」；收到的 1500 ms 里有 500 ms 是没登记的尾巴。
    assert_eq!(recovery["persisted_ms"], json!(1000));
    assert_eq!(recovery["received_ms"], json!(1500));
    assert!(
        recovery["notes"].as_array().unwrap().iter().any(|note| note
            .as_str()
            .unwrap()
            .contains("未封闭的尾巴")
            && note.as_str().unwrap().contains("500 ms")),
        "恢复结果要如实说明可能丢掉的尾巴：{recovery}"
    );

    // 恢复出来的录音还能救：按已封闭序号最终化。
    let rescued = expect_ok(&call(
        reopened,
        "recordings.finalize",
        json!({
            "recording_id": recording2,
            "last_segment_index": 0,
            "end_reason": "recovered",
        }),
    ));
    assert_eq!(rescued["state"], json!("saved"));
    assert_eq!(rescued["segment_count"], json!(1));
    assert_eq!(
        rescued["logical_duration_ms"],
        json!(1000),
        "恢复出的音频 = 已登记片段之和，不是收到的 1500 ms"
    );

    let info = expect_ok(&call(reopened, "core.info", Value::Null));
    assert_eq!(info["recovery"]["open_recordings"], json!(0));
    assert!(
        info["capabilities"]
            .as_array()
            .unwrap()
            .iter()
            .any(|name| name == "recordings.finalize"),
        "能力清单要如实列出录音方法：{info}"
    );

    assert_eq!(diary_close(reopened), 0);
    std::fs::remove_dir_all(dir).unwrap();
}

/// 录音参数错误：拼错字段、字节数不符、未知录音都要报核心码，不能静默成功。
#[test]
fn recordings_bad_input_reports_core_errors() {
    let (dir, _path, handle) = open_temp("recordings-errors");
    let capture = expect_ok(&call(
        handle,
        "captures.createDraft",
        json!({
            "time_zone": "Asia/Shanghai",
            "utc_offset_minutes": 480,
            "operation_id": "op-capi-rec-err",
        }),
    ));
    let capture_id = capture["id"].as_str().unwrap().to_owned();
    let ticket = expect_ok(&call(
        handle,
        "recordings.prepare",
        json!({"capture_id": capture_id, "operation_id": "op-capi-rec-err-prepare"}),
    ));
    let recording_id = ticket["recording_id"].as_str().unwrap().to_owned();
    let staging = PathBuf::from(ticket["staging_ticket"].as_str().unwrap());

    let text = expect_error(
        &call(
            handle,
            "recordings.registerSegment",
            json!({"recording_id": recording_id, "segment_index": 0, "segment": {"segmentId": "s", "relativeFileName": "missing.pcm", "durationMs": 10, "sha256": "00", "byteSize": 1}}),
        ),
        "integrity_failed",
    );
    assert!(text.contains("不可读"), "{text}");

    let body = b"not-that-long".to_vec();
    std::fs::write(staging.join("actual.pcm"), &body).unwrap();
    let text = expect_error(
        &call(
            handle,
            "recordings.registerSegment",
            json!({"recording_id": recording_id, "segment_index": 0, "segment": {"segmentId": "s", "relativeFileName": "actual.pcm", "durationMs": 10, "sha256": sha256_hex(&body), "byteSize": body.len() + 1}}),
        ),
        "integrity_failed",
    );
    assert!(text.contains("字节数不符"), "{text}");

    let text = expect_error(
        &call(
            handle,
            "recordings.recover",
            json!({"recording_id": "rec_missing"}),
        ),
        "not_found",
    );
    assert!(text.contains("录音"), "{text}");

    let text = expect_error(
        &call(
            handle,
            "recordings.prepare",
            json!({"capture_id": capture_id, "operation_id": "op", "extra": 1}),
        ),
        "invalid_argument",
    );
    assert!(text.contains("unknown field"), "{text}");

    assert_eq!(diary_close(handle), 0);
    std::fs::remove_dir_all(dir).unwrap();
}

/// 44 字节 PCM WAV 头：16 kHz / 单声道 / 16 bit，长度字段用流式占位（总长停止时才定）。
fn pcm_wav_header() -> Vec<u8> {
    let mut header = Vec::with_capacity(44);
    header.extend_from_slice(b"RIFF");
    header.extend_from_slice(&0xFFFF_FFFF_u32.to_le_bytes());
    header.extend_from_slice(b"WAVEfmt ");
    header.extend_from_slice(&16_u32.to_le_bytes());
    header.extend_from_slice(&1_u16.to_le_bytes()); // PCM
    header.extend_from_slice(&1_u16.to_le_bytes()); // 单声道
    header.extend_from_slice(&16_000_u32.to_le_bytes());
    header.extend_from_slice(&32_000_u32.to_le_bytes()); // byte rate
    header.extend_from_slice(&2_u16.to_le_bytes()); // block align
    header.extend_from_slice(&16_u16.to_le_bytes()); // bits per sample
    header.extend_from_slice(b"data");
    header.extend_from_slice(&0xFFFF_FFFF_u32.to_le_bytes());
    header
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(bytes))
}

/// 在 `dir` 下递归找一个大小恰好为 `len` 的普通文件（只用于小目录自检）。
fn find_file_with_len(dir: &Path, len: u64) -> Option<PathBuf> {
    for entry in std::fs::read_dir(dir).ok()? {
        let path = entry.ok()?.path();
        if path.is_dir() {
            if let Some(found) = find_file_with_len(&path, len) {
                return Some(found);
            }
        } else if std::fs::metadata(&path).ok()?.len() == len {
            return Some(path);
        }
    }
    None
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

/// `providers.save` / `list` / `test` / `remove` 的 C ABI 形状、校验与错误码。
///
/// 不依赖真引擎/真网络：
/// - `save` 只落配置（本地引擎的 `model` 是目录，不 stat）；
/// - `test` 用一个**本机适配器没实现**的能力（`generate_text`）⇒ 报告里是 failed、
///   `actual_call_made=false`，不会加载引擎，也不会发请求；
/// - 其余都是参数校验（`deny_unknown_fields`、空 scope、未知 id）。
#[test]
fn providers_crud_contract_shapes_and_validation() {
    let (dir, _path, handle) = open_temp("providers");

    let config = json!({
        "id": "local_asr_capi",
        "label": "C ABI 测试的本机转写",
        "adapter_id": "sherpa_onnx",
        "endpoint": null,
        "model": "/nonexistent/capi-model-dir",
        "capabilities": ["transcribe_audio"],
        "credential_ref": null,
        "enabled": true,
        // 只读结论：保存时核心会忽略它（不能由调用方写成「已验证」）。
        "test_status": "ok"
    });

    // save：返回配置行；test_status 必须是 untested（传入的 ok 被忽略）。
    let saved = expect_ok(&call(
        handle,
        "providers.save",
        json!({"config": config, "secret": null}),
    ));
    assert_eq!(saved["id"], json!("local_asr_capi"));
    assert_eq!(saved["adapter_id"], json!("sherpa_onnx"));
    assert_eq!(saved["endpoint"], json!(null));
    assert_eq!(saved["test_status"], json!("untested"));
    assert_eq!(saved["capabilities"], json!(["transcribe_audio"]));

    // list：只返回配置行（内建引擎不在这张表里）。
    let listed = expect_ok(&call(handle, "providers.list", json!(null)));
    let rows = listed.as_array().expect("list 应返回数组");
    assert_eq!(rows.len(), 1, "{listed}");
    assert_eq!(rows[0]["id"], json!("local_asr_capi"));
    assert_ne!(rows[0]["id"], json!("builtin_local_asr"));

    // save 校验：本机引擎不接受 endpoint。
    let mut with_endpoint = config.clone();
    with_endpoint["endpoint"] = json!("http://127.0.0.1:1/v1");
    let message = expect_error(
        &call(
            handle,
            "providers.save",
            json!({"config": with_endpoint, "secret": null}),
        ),
        "invalid_argument",
    );
    assert!(message.contains("endpoint"), "{message}");

    // save 校验：未知 adapter。
    let mut unknown_adapter = config.clone();
    unknown_adapter["adapter_id"] = json!("no_such_adapter");
    let message = expect_error(
        &call(
            handle,
            "providers.save",
            json!({"config": unknown_adapter, "secret": null}),
        ),
        "invalid_argument",
    );
    assert!(message.contains("no_such_adapter"), "{message}");

    // deny_unknown_fields：4 个方法**请求参数**里拼错字段都要报 invalid_argument。
    // （`config` 里的未知字段由核心 `Provider` 决定，不属于 C ABI 参数层。）
    expect_error(
        &call(
            handle,
            "providers.save",
            json!({"config": config.clone(), "secret": null, "unexpected_field": 1}),
        ),
        "invalid_argument",
    );
    expect_error(
        &call(
            handle,
            "providers.test",
            json!({"provider_id": "local_asr_capi", "scope": ["transcribe_audio"], "extra": 1}),
        ),
        "invalid_argument",
    );
    expect_error(
        &call(
            handle,
            "providers.remove",
            json!({"provider_id": "local_asr_capi", "extra": 1}),
        ),
        "invalid_argument",
    );
    expect_error(
        &call(handle, "providers.list", json!({"unexpected": true})),
        "invalid_argument",
    );

    // test：scope 不能为空。
    let message = expect_error(
        &call(
            handle,
            "providers.test",
            json!({"provider_id": "local_asr_capi", "scope": []}),
        ),
        "invalid_argument",
    );
    assert!(message.contains("测试范围"), "{message}");

    // test：未知 provider id。
    expect_error(
        &call(
            handle,
            "providers.test",
            json!({"provider_id": "prv_missing", "scope": ["transcribe_audio"]}),
        ),
        "not_found",
    );

    // test：本机适配器没实现 generate_text ⇒ 报告是 failed，且没有真的调用/联网。
    let report = expect_ok(&call(
        handle,
        "providers.test",
        json!({"provider_id": "local_asr_capi", "scope": ["generate_text"]}),
    ));
    assert_eq!(report["provider_id"], json!("local_asr_capi"));
    assert_eq!(report["actual_call_made"], json!(false));
    assert_eq!(report["results"]["generate_text"], json!("failed"));
    assert!(
        report["message"]
            .as_str()
            .unwrap_or_default()
            .contains("没有实现"),
        "{report}"
    );

    // remove：删掉配置行；再删一次是 not_found（不是 invalid_state）。
    expect_ok(&call(
        handle,
        "providers.remove",
        json!({"provider_id": "local_asr_capi"}),
    ));
    let listed = expect_ok(&call(handle, "providers.list", json!(null)));
    assert_eq!(listed, json!([]));
    expect_error(
        &call(
            handle,
            "providers.remove",
            json!({"provider_id": "local_asr_capi"}),
        ),
        "not_found",
    );

    // remove：内建引擎没有配置行，删它要说清是 invalid_state（不是 not_found）。
    let message = expect_error(
        &call(
            handle,
            "providers.remove",
            json!({"provider_id": "builtin_local_asr"}),
        ),
        "invalid_state",
    );
    assert!(
        message.contains("随包") || message.contains("内建"),
        "{message}"
    );

    assert_eq!(diary_close(handle), 0);
    std::fs::remove_dir_all(dir).unwrap();
}

/// 第 1 组：`captures.get` / `captures.list`——「看今天」、历史列表、条目详情的
/// 数据源，游标必须是**真游标**（第二页接得上、不重不漏）。
#[test]
fn captures_get_and_list_use_real_cursor_pagination() {
    let (dir, _path, handle) = open_temp("captures-read");

    // 三条：两条在 10-17（上海），一条在 10-16。
    let older = seed_capture_at(
        handle,
        "op-capi-list-a",
        "2026-10-16T09:30:00Z",
        "前天的记录",
    );
    let morning = seed_capture_at(handle, "op-capi-list-b", "2026-10-17T09:30:00Z", "今天上午");
    let evening = seed_capture_at(handle, "op-capi-list-c", "2026-10-17T14:00:00Z", "今天晚上");

    // 详情：草稿/已提交都读得到，带 processing_summary 与 revision。
    let detail = expect_ok(&call(
        handle,
        "captures.get",
        json!({"capture_id": evening.clone()}),
    ));
    assert_eq!(detail["id"], json!(evening));
    assert_eq!(detail["state"], json!("committed"));
    assert_eq!(detail["day_key"], json!("2026-10-17"));
    assert!(detail["revision"].as_i64().unwrap() >= 2);
    assert!(detail["processing_summary"].is_object(), "{detail}");
    // 契约风格的 camelCase 别名也接受。
    let via_alias = expect_ok(&call(
        handle,
        "captures.get",
        json!({"captureId": morning.clone()}),
    ));
    assert_eq!(via_alias["id"], json!(morning));

    // 第一页：occurred_at DESC，两条 + 一个真游标。
    let first = expect_ok(&call(handle, "captures.list", json!({"limit": 2})));
    let page = first["captures"].as_array().unwrap();
    assert_eq!(page.len(), 2, "{first}");
    assert_eq!(page[0]["id"], json!(evening));
    assert_eq!(page[1]["id"], json!(morning));
    let cursor = first["next_cursor"]
        .as_str()
        .expect("还有一条，必须给 next_cursor")
        .to_owned();

    // 第二页：游标原样回传，接上第三条且没有重复。
    let second = expect_ok(&call(
        handle,
        "captures.list",
        json!({"limit": 2, "cursor": cursor}),
    ));
    let page = second["captures"].as_array().unwrap();
    assert_eq!(page.len(), 1, "{second}");
    assert_eq!(page[0]["id"], json!(older));
    assert_eq!(second["next_cursor"], json!(null));

    // 按天过滤（「看今天」），camelCase 别名。
    let today = expect_ok(&call(
        handle,
        "captures.list",
        json!({"dayKey": "2026-10-17", "limit": 10}),
    ));
    let ids: Vec<&str> = today["captures"]
        .as_array()
        .unwrap()
        .iter()
        .map(|capture| capture["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, vec![evening.as_str(), morning.as_str()], "{today}");

    // 参数全省略 = 默认页（历史列表第一页）。
    let all = expect_ok(&call(handle, "captures.list", Value::Null));
    assert_eq!(all["captures"].as_array().unwrap().len(), 3);

    // 错误码：不存在是 not_found，参数不合法是 invalid_argument；
    // 游标格式不对沿用核心既有口径（CorruptedData → invalid_state，不新增码）。
    expect_error(
        &call(handle, "captures.get", json!({"capture_id": "cap_missing"})),
        "not_found",
    );
    expect_error(&call(handle, "captures.get", json!({})), "invalid_argument");
    expect_error(
        &call(handle, "captures.list", json!({"daykey": "2026-10-17"})),
        "invalid_argument",
    );
    expect_error(
        &call(handle, "captures.list", json!({"limit": -1})),
        "invalid_argument",
    );
    let message = expect_error(
        &call(handle, "captures.list", json!({"cursor": "not-a-cursor"})),
        "invalid_state",
    );
    assert!(message.contains("游标"), "{message}");

    assert_eq!(diary_close(handle), 0);
    std::fs::remove_dir_all(dir).unwrap();
}

/// 第 2 组：`indexes.status`——语义就绪 / reasons / model_version 原样可读。
#[test]
fn indexes_status_exposes_semantic_readiness_and_scope() {
    let (dir, _path, handle) = open_temp("index-status");

    let status = expect_ok(&call(handle, "indexes.status", Value::Null));
    assert!(status["coverage"].is_string(), "{status}");
    assert!(status["keyword_index_ready"].is_boolean(), "{status}");
    assert!(status["semantic_index_ready"].is_boolean(), "{status}");
    assert!(status["tokenizer_version"].is_string(), "{status}");
    assert!(
        status["model_version"].is_null() || status["model_version"].is_string(),
        "{status}"
    );
    assert!(
        status["chunker_version"].is_null() || status["chunker_version"].is_string(),
        "{status}"
    );
    assert!(status["reasons"].is_array(), "{status}");
    for field in [
        "total_chunks",
        "embedded_chunks",
        "indexed_segments",
        "total_segments",
        "pending_segments",
        "stale_segments",
        "indexed_captures",
        "total_captures",
        "failed_sources",
        "indexed_chars",
        "index_rows",
        "index_terms",
        "index_bytes",
    ] {
        assert!(status[field].is_i64(), "{field} 应当是整数：{status}");
    }

    // 空范围 = 「什么都不看」（与省略 = 整库刻意不同）。
    for params in [json!({"source_scope": []}), json!({"sourceScope": []})] {
        let empty = expect_ok(&call(handle, "indexes.status", params));
        for field in [
            "total_chunks",
            "embedded_chunks",
            "total_segments",
            "indexed_segments",
        ] {
            assert_eq!(empty[field], json!(0), "{field}：{empty}");
        }
    }

    // 参数层：拼错的字段要报错，不能静默当成整库。
    let message = expect_error(
        &call(handle, "indexes.status", json!({"scope": "all"})),
        "invalid_argument",
    );
    assert!(message.contains("参数不合法"), "{message}");

    assert_eq!(diary_close(handle), 0);
    std::fs::remove_dir_all(dir).unwrap();
}

/// 第 3 组：`jobs.list` / `jobs.get`——逐条任务（`attention_key` / `progress` 在里面），
/// 游标是真游标（第二页接得上、不重不漏）。
#[test]
fn jobs_list_and_get_use_real_cursor_pagination() {
    let (dir, _path, handle) = open_temp("jobs-read");

    // `operation_id` 进快照哈希，所以三条不同 op 的整库重建是三条独立任务。
    let mut expected = Vec::new();
    for index in 0..3 {
        let job = expect_ok(&call(
            handle,
            "indexes.rebuild",
            json!({
                "scope": "all",
                "reason": "capi jobs list",
                "operation_id": format!("op-capi-jobs-{index}"),
            }),
        ));
        expected.push(job["id"].as_str().unwrap().to_owned());
    }

    let first = expect_ok(&call(handle, "jobs.list", json!({"limit": 2})));
    let jobs = first["jobs"].as_array().unwrap();
    assert_eq!(jobs.len(), 2, "{first}");
    // 每条都是完整的 `Job`：任务中心要的 `attention_key` / `progress` 在形状里。
    for job in jobs {
        assert!(job.get("state").is_some(), "{job}");
        assert!(job.get("attention_key").is_some(), "{job}");
        assert!(job.get("progress").is_some(), "{job}");
        assert_eq!(job["kind"], json!("index"));
    }
    let cursor = first["next_cursor"]
        .as_str()
        .expect("还有一条，必须给 next_cursor")
        .to_owned();

    let second = expect_ok(&call(
        handle,
        "jobs.list",
        json!({"limit": 2, "cursor": cursor}),
    ));
    let second_jobs = second["jobs"].as_array().unwrap();
    assert_eq!(second_jobs.len(), 1, "{second}");
    assert_eq!(second["next_cursor"], json!(null));

    // 两页合起来正好是三条，不重不漏。
    let mut seen: Vec<String> = jobs
        .iter()
        .chain(second_jobs.iter())
        .map(|job| job["id"].as_str().unwrap().to_owned())
        .collect();
    seen.sort();
    expected.sort();
    assert_eq!(seen, expected);

    // 状态筛选：全部 queued；没有 succeeded。
    let queued = expect_ok(&call(
        handle,
        "jobs.list",
        json!({"states": ["queued"], "limit": 10}),
    ));
    assert_eq!(queued["jobs"].as_array().unwrap().len(), 3, "{queued}");
    let succeeded = expect_ok(&call(
        handle,
        "jobs.list",
        json!({"states": ["succeeded"], "limit": 10}),
    ));
    assert_eq!(
        succeeded["jobs"].as_array().unwrap().len(),
        0,
        "{succeeded}"
    );

    // 详情：id + 完整形状；camelCase 别名也接受。
    let detail = expect_ok(&call(
        handle,
        "jobs.get",
        json!({"job_id": expected[0].clone()}),
    ));
    assert_eq!(detail["id"], json!(expected[0]));
    assert_eq!(detail["state"], json!("queued"));
    assert_eq!(detail["progress"], json!(null));
    assert_eq!(detail["attention_key"], json!(null));
    let via_alias = expect_ok(&call(
        handle,
        "jobs.get",
        json!({"id": expected[1].clone()}),
    ));
    assert_eq!(via_alias["id"], json!(expected[1]));

    // 错误码与参数校验。
    expect_error(
        &call(handle, "jobs.get", json!({"job_id": "job_missing"})),
        "not_found",
    );
    expect_error(&call(handle, "jobs.get", json!({})), "invalid_argument");
    expect_error(
        &call(handle, "jobs.list", json!({"state": ["queued"]})),
        "invalid_argument",
    );
    expect_error(
        &call(handle, "jobs.list", json!({"states": ["not_a_state"]})),
        "invalid_argument",
    );
    expect_error(
        &call(handle, "jobs.list", json!({"cursor": "not-a-cursor"})),
        "invalid_state",
    );

    assert_eq!(diary_close(handle), 0);
    std::fs::remove_dir_all(dir).unwrap();
}

/// 第 4 组：`jobs.retry` / `jobs.cancel` / `jobs.acknowledgeAttention`。
///
/// 用一条**没有执行者**的任务制造 `failed`（核心的真实终态，带 `attention_key`），
/// 之后的读取与人工动作全部走 C ABI。
#[test]
fn jobs_retry_cancel_and_acknowledge_attention() {
    let (dir, _path, handle) = open_temp("jobs-actions");
    let job_id = enqueue_for_test(handle, "capi_no_handler", &[]);

    // 跑一轮：没有执行者 → failed + no_handler + requires_user_action。
    let summary = expect_ok(&call(
        handle,
        "jobs.runDue",
        json!({"budget_ms": 30_000, "trigger": "manual"}),
    ));
    assert_eq!(summary["processed"], json!(1), "{summary}");
    assert_eq!(summary["failed"], json!(1), "{summary}");

    let failed = expect_ok(&call(handle, "jobs.get", json!({"job_id": job_id.clone()})));
    assert_eq!(failed["state"], json!("failed"), "{failed}");
    assert_eq!(failed["error_code"], json!("no_handler"));
    assert_eq!(failed["requires_user_action"], json!(true));
    let attention = failed["attention_key"]
        .as_str()
        .expect("需要用户处理的任务必须带 attention_key")
        .to_owned();
    assert!(attention.contains("capi_no_handler"), "{attention}");

    // acknowledgeAttention：返回值是 null；同一个 key 重复调用幂等。
    assert_eq!(
        expect_ok(&call(
            handle,
            "jobs.acknowledgeAttention",
            json!({"attentionKey": attention}),
        )),
        json!(null)
    );
    assert_eq!(
        expect_ok(&call(
            handle,
            "jobs.acknowledgeAttention",
            json!({"attention_key": attention}),
        )),
        json!(null)
    );
    {
        let core = debug_core(handle).unwrap();
        let core = core.lock().unwrap();
        assert!(core.is_attention_acknowledged(&attention).unwrap());
    }

    // 不带 operationId 的人工重试：清零重跑。
    let retried = expect_ok(&call(
        handle,
        "jobs.retry",
        json!({"job_id": job_id.clone()}),
    ));
    assert_eq!(retried["state"], json!("queued"), "{retried}");
    assert_eq!(retried["attempt_count"], json!(0));
    assert_eq!(retried["error_code"], json!(null));

    // 再跑一轮又失败（同一条任务、同一个 attention_key）。
    let summary = expect_ok(&call(
        handle,
        "jobs.runDue",
        json!({"budget_ms": 30_000, "trigger": "manual"}),
    ));
    assert_eq!(summary["processed"], json!(1), "{summary}");
    assert_eq!(summary["failed"], json!(1), "{summary}");

    // 带 operationId 的幂等重试：重放返回第一次的结果。
    let retried = expect_ok(&call(
        handle,
        "jobs.retry",
        json!({"job_id": job_id.clone(), "operation_id": "op-capi-retry"}),
    ));
    assert_eq!(retried["state"], json!("queued"), "{retried}");
    let replay = expect_ok(&call(
        handle,
        "jobs.retry",
        json!({"id": job_id.clone(), "operationId": "op-capi-retry"}),
    ));
    assert_eq!(replay["updated_at"], retried["updated_at"], "{replay}");
    // 已 queued（不是 failed）再重试 → invalid_state。
    let message = expect_error(
        &call(
            handle,
            "jobs.retry",
            json!({"job_id": job_id.clone(), "operation_id": "op-capi-retry-2"}),
        ),
        "invalid_state",
    );
    assert!(message.contains("已放弃"), "{message}");

    // 取消：带 operationId，可幂等重放。
    let cancelled = expect_ok(&call(
        handle,
        "jobs.cancel",
        json!({"job_id": job_id.clone(), "operation_id": "op-capi-cancel"}),
    ));
    assert_eq!(cancelled["state"], json!("cancelled"), "{cancelled}");
    let replay = expect_ok(&call(
        handle,
        "jobs.cancel",
        json!({"id": job_id.clone(), "operationId": "op-capi-cancel"}),
    ));
    assert_eq!(replay["updated_at"], cancelled["updated_at"], "{replay}");
    // 终态（cancelled）再取消 → invalid_state（不是 not_found）。
    expect_error(
        &call(
            handle,
            "jobs.cancel",
            json!({"job_id": job_id.clone(), "operation_id": "op-capi-cancel-2"}),
        ),
        "invalid_state",
    );

    // 错误码与参数校验。
    expect_error(
        &call(handle, "jobs.retry", json!({"job_id": "job_missing"})),
        "not_found",
    );
    expect_error(
        &call(
            handle,
            "jobs.cancel",
            json!({"job_id": "job_missing", "operation_id": "op"}),
        ),
        "not_found",
    );
    expect_error(
        &call(handle, "jobs.cancel", json!({"job_id": job_id.clone()})),
        "invalid_argument",
    );
    expect_error(
        &call(handle, "jobs.acknowledgeAttention", json!({})),
        "invalid_argument",
    );
    expect_error(
        &call(
            handle,
            "jobs.retry",
            json!({"job_id": job_id, "operation_id": "op", "extra": 1}),
        ),
        "invalid_argument",
    );

    assert_eq!(diary_close(handle), 0);
    std::fs::remove_dir_all(dir).unwrap();
}

/// 第 5 组：`search.nextPage` / `search.upgrade` / `search.cancel`。
///
/// 翻页走真游标；`hybrid` 的第二阶段在没有语义证据时如实停在 `keyword_ready`
/// （不假装融合）；取消之后会话还在、状态是 `cancelled`。
#[test]
fn search_pagination_upgrade_and_cancel() {
    let (dir, _path, handle) = open_temp("search-pages");
    for index in 0..3 {
        seed_capture_at(
            handle,
            &format!("op-capi-search-{index}"),
            &format!("2026-10-17T0{}:00:00Z", index + 1),
            "今天在鸿蒙上验证检索：鹅掌楸、翻页与融合。",
        );
    }

    let first = expect_ok(&call(
        handle,
        "search.start",
        json!({
            "request": {"query": "鹅掌楸", "mode": "keyword", "page_size": 2},
            "query_revision": 1,
        }),
    ));
    assert_eq!(first["results"].as_array().unwrap().len(), 2, "{first}");
    let session_id = first["session_id"].as_str().unwrap().to_owned();
    let first_cursor = first["cursor"]
        .as_str()
        .expect("还有下一页，必须给 cursor")
        .to_owned();

    // 先显式传游标翻一页，再省略游标（核心用会话自己记着的下一页）翻完。
    let second = expect_ok(&call(
        handle,
        "search.nextPage",
        json!({"session_id": session_id, "cursor": first_cursor}),
    ));
    assert_eq!(second["session_id"], json!(session_id), "{second}");
    let mut pages = vec![first.clone(), second.clone()];
    let mut cursor = second["cursor"].as_str().map(str::to_owned);
    while cursor.is_some() {
        let page = expect_ok(&call(
            handle,
            "search.nextPage",
            json!({"session_id": session_id}),
        ));
        cursor = page["cursor"].as_str().map(str::to_owned);
        pages.push(page);
        assert!(pages.len() < 20, "翻页没有终点：{pages:?}");
    }
    assert!(pages.len() >= 2, "至少要有两页：{pages:?}");

    // 所有页合起来没有重复命中；每一页都是完整快照（不是增量）。
    let mut hit_ids: Vec<String> = Vec::new();
    for page in &pages {
        for hit in page["results"].as_array().unwrap() {
            hit_ids.push(hit["hit_id"].as_str().unwrap().to_owned());
        }
    }
    let total = hit_ids.len();
    hit_ids.sort();
    hit_ids.dedup();
    assert_eq!(hit_ids.len(), total, "跨页出现了重复命中");
    assert!(total >= 3, "{pages:?}");

    // 游标错误：格式不对 / 不属于这个会话 → cursor_expired；会话不存在 → search_expired。
    expect_error(
        &call(
            handle,
            "search.nextPage",
            json!({"session_id": session_id, "cursor": "garbage"}),
        ),
        "cursor_expired",
    );
    expect_error(
        &call(
            handle,
            "search.nextPage",
            json!({"session_id": "search_missing"}),
        ),
        "search_expired",
    );
    // 非 hybrid 会话不能升级。
    expect_error(
        &call(handle, "search.upgrade", json!({"session_id": session_id})),
        "invalid_state",
    );

    // hybrid 的两阶段：start 只给关键词阶段；upgrade 在没有语义证据时如实停在
    // keyword_ready 并给 warnings（有模型/索引时才会是 hybrid_ready）。
    let hybrid = expect_ok(&call(
        handle,
        "search.start",
        json!({
            "request": {"query": "鹅掌楸", "mode": "hybrid", "page_size": 2},
            "query_revision": 1,
        }),
    ));
    assert_eq!(hybrid["phase"], json!("keyword_ready"), "{hybrid}");
    let hybrid_session = hybrid["session_id"].as_str().unwrap().to_owned();
    let upgraded = expect_ok(&call(
        handle,
        "search.upgrade",
        json!({"session_id": hybrid_session}),
    ));
    assert_eq!(upgraded["session_id"], json!(hybrid_session), "{upgraded}");
    let phase = upgraded["phase"].as_str().unwrap();
    assert!(
        phase == "keyword_ready" || phase == "hybrid_ready",
        "升级后的阶段不合契约：{upgraded}"
    );
    if phase == "keyword_ready" {
        assert!(
            !upgraded["warnings"].as_array().unwrap().is_empty(),
            "没融合就必须在 warnings 里说明：{upgraded}"
        );
    }
    let replay = expect_ok(&call(
        handle,
        "search.upgrade",
        json!({"session_id": hybrid_session}),
    ));
    assert_eq!(replay["phase"], upgraded["phase"], "升级必须幂等：{replay}");

    // 取消：返回 cancelled 快照；再取消幂等；取消后翻页 → invalid_state。
    let cancelled = expect_ok(&call(
        handle,
        "search.cancel",
        json!({"session_id": hybrid_session}),
    ));
    assert_eq!(cancelled["phase"], json!("cancelled"), "{cancelled}");
    assert_eq!(cancelled["cursor"], json!(null));
    let again = expect_ok(&call(
        handle,
        "search.cancel",
        json!({"session_id": hybrid_session}),
    ));
    assert_eq!(again["phase"], json!("cancelled"), "{again}");
    expect_error(
        &call(
            handle,
            "search.nextPage",
            json!({"session_id": hybrid_session}),
        ),
        "invalid_state",
    );
    expect_error(
        &call(
            handle,
            "search.cancel",
            json!({"session_id": "search_missing"}),
        ),
        "search_expired",
    );

    // 参数校验。
    expect_error(
        &call(handle, "search.nextPage", json!({})),
        "invalid_argument",
    );
    expect_error(
        &call(
            handle,
            "search.nextPage",
            json!({"session_id": hybrid_session, "extra": 1}),
        ),
        "invalid_argument",
    );
    expect_error(
        &call(handle, "search.upgrade", json!(null)),
        "invalid_argument",
    );
    expect_error(
        &call(
            handle,
            "search.cancel",
            json!({"session_id": hybrid_session, "cursor": "x"}),
        ),
        "invalid_argument",
    );

    assert_eq!(diary_close(handle), 0);
    std::fs::remove_dir_all(dir).unwrap();
}

/// 能力清单必须随接线增长：`core.info.capabilities` 与 `WIRED_METHODS` **逐项相同**，
/// 且清单里的每一条都真的能进 dispatch（不是只把名字写进数组）。
#[test]
fn capabilities_include_every_wired_method() {
    let (dir, _path, handle) = open_temp("capabilities");
    let info = expect_ok(&call(handle, "core.info", Value::Null));
    assert_eq!(
        info["api_version"],
        json!("1.1"),
        "apiVersion 必须与核心同源；加方法本身不抬版本（1.1 是必填响应字段升的）"
    );

    let capabilities: Vec<&str> = info["capabilities"]
        .as_array()
        .unwrap()
        .iter()
        .map(|name| name.as_str().unwrap())
        .collect();
    // **与 WIRED_METHODS 逐项相同**，不是「38 条 + 点名这一批」。只点名一部分的话，
    // 漏掉一条、顺序漂移、或清单与常量各写一份都测不出来。
    assert_eq!(
        capabilities.as_slice(),
        crate::methods::WIRED_METHODS,
        "core.info.capabilities 必须与 WIRED_METHODS 逐项一致"
    );
    let mut unique = capabilities.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(unique.len(), capabilities.len(), "能力清单有重复");

    // 清单是 WIRED_METHODS 原样序列化的，所以「集合相等」本身证明不了 dispatch 真接了。
    // 逐条用空参数调一次：只要不是「未接入的方法」，就说明 match 里有它（多数会因缺
    // 必填参数返回 invalid_argument，那是预期的）。
    for method in crate::methods::WIRED_METHODS {
        let envelope = call(handle, method, json!({}));
        if envelope["ok"] == json!(false) {
            let message = envelope["error"]["message"].as_str().unwrap_or_default();
            assert!(
                !message.contains("未接入的方法"),
                "{method} 在能力清单里，但 dispatch 没接：{message}"
            );
        }
    }

    // 核心还没有实现的 9 条**不能**出现在诚实的能力清单里。
    for name in [
        "captures.trash",
        "captures.restore",
        "captures.previewPurge",
        "captures.purge",
        "captures.moveDay",
        "styles.get",
        "styles.save",
        "styles.list",
        "exports.create",
    ] {
        assert!(
            !capabilities.contains(&name),
            "能力清单不该列未实现的 {name}"
        );
    }

    assert_eq!(diary_close(handle), 0);
    std::fs::remove_dir_all(dir).unwrap();
}

// ---------------------------------------------------------------- 本批（P0 的 8 条）

/// `sources.extractedContent` / `previousRevision` / `reviseText` / `locate`：
/// 可空返回、改正文建新修订、乐观锁、`deny_unknown_fields` 与别名。
#[test]
fn sources_revisions_content_and_locate_are_wired() {
    let (dir, _path, handle) = open_temp("sources-revisions");
    let first_text = "第一版正文：今天把窗台的绿萝修了。";
    let (capture_id, source_id) = seed_committed_capture(handle, first_text);

    // 当前修订的提取结果：已 extract 过，应当有；未知来源是 null，不是错误。
    let content = expect_ok(&call(
        handle,
        "sources.extractedContent",
        json!({"source_id": source_id}),
    ));
    assert_eq!(content["source_id"], json!(source_id));
    assert!(content["text"].as_str().unwrap().contains("绿萝"));
    let none = expect_ok(&call(
        handle,
        "sources.extractedContent",
        json!({"source_id": "src_不存在"}),
    ));
    assert_eq!(none, Value::Null, "没有提取过必须是 null");

    // 首次提交只有一版：上一版是 null。
    let previous = expect_ok(&call(
        handle,
        "sources.previousRevision",
        json!({"source_id": source_id}),
    ));
    assert_eq!(previous, Value::Null, "只有一版时上一版必须是 null");

    // 改正文：新修订，旧内容保留。
    let capture = expect_ok(&call(
        handle,
        "captures.get",
        json!({"capture_id": capture_id}),
    ));
    let revision = capture["revision"].as_i64().unwrap();
    let revised = expect_ok(&call(
        handle,
        "sources.reviseText",
        json!({
            "source_id": source_id,
            "text": "第二版正文：绿萝换盆了。",
            "expected_revision": revision,
            "operation_id": "op-capi-revise",
        }),
    ));
    assert_eq!(revised["source_id"], json!(source_id));
    assert_eq!(revised["text"], json!("第二版正文：绿萝换盆了。"));
    assert!(
        revised["parent_revision_id"].as_str().is_some(),
        "新修订必须指回上一版：{revised}"
    );
    let revised_revision_id = revised["revision_id"].clone();

    // 现在上一版就是第一次提交的正文；旧内容没有被覆盖。
    let previous = expect_ok(&call(
        handle,
        "sources.previousRevision",
        json!({"source_id": source_id}),
    ));
    assert_eq!(previous["text"], json!(first_text));
    assert_eq!(previous["revision_id"], revised["parent_revision_id"]);

    // 当前提取结果跟着当前修订走。
    let content = expect_ok(&call(
        handle,
        "sources.extractedContent",
        json!({"source_id": source_id}),
    ));
    assert_eq!(content["source_revision_id"], revised_revision_id);
    assert!(content["text"].as_str().unwrap().contains("换盆"));

    // 定位：纯文字来源没有可打开的原件 -> available:false + reason，不是错误。
    let located = expect_ok(&call(
        handle,
        "sources.locate",
        json!({
            "source_ref": source_id,
            "locator": {
                "locator_type": "text_range",
                "source_revision_id": revised_revision_id,
                "text_start": 0,
                "text_end": 4,
            },
        }),
    ));
    assert_eq!(located["source_ref"], json!(source_id));
    assert_eq!(located["available"], json!(false));
    assert!(
        located["reason"].as_str().unwrap().contains("原件"),
        "{located}"
    );
    assert_eq!(located["locator"]["locator_type"], json!("text_range"));

    // 破坏性/修改类操作必须带 expected_revision：缺了直接报错。
    let missing = expect_error(
        &call(
            handle,
            "sources.reviseText",
            json!({
                "source_id": source_id,
                "text": "x",
                "operation_id": "op-capi-missing",
            }),
        ),
        "invalid_argument",
    );
    assert!(missing.contains("expected_revision"), "{missing}");

    // 乐观锁不匹配：核心报 revision_conflict，一个字节都不改。
    expect_error(
        &call(
            handle,
            "sources.reviseText",
            json!({
                "source_id": source_id,
                "text": "x",
                "expected_revision": 999,
                "operation_id": "op-capi-conflict",
            }),
        ),
        "revision_conflict",
    );

    // 拼错的字段（deny_unknown_fields）不能被静默忽略。
    let typo = expect_error(
        &call(
            handle,
            "sources.extractedContent",
            json!({"source_id": source_id, "extra": 1}),
        ),
        "invalid_argument",
    );
    assert!(typo.contains("参数不合法"), "{typo}");

    // `previousRevision` / `locate` 的**错误路径**：缺必填字段、字段类型错、枚举值
    // 非法都是 `invalid_argument`。这两条以前只有成功/别名成功，等于没验证过
    // 「参数不对时不会静默当成 null / 默认 locator」。
    let missing_source = expect_error(
        &call(handle, "sources.previousRevision", json!({})),
        "invalid_argument",
    );
    assert!(
        missing_source.contains("source_id"),
        "缺 source_id 要点名字段：{missing_source}"
    );
    let wrong_source_type = expect_error(
        &call(
            handle,
            "sources.previousRevision",
            json!({"source_id": 123}),
        ),
        "invalid_argument",
    );
    assert!(
        wrong_source_type.contains("source_id") || wrong_source_type.contains("参数不合法"),
        "source_id 类型不对要如实报：{wrong_source_type}"
    );
    let missing_locator = expect_error(
        &call(
            handle,
            "sources.locate",
            json!({"source_ref": source_id}),
        ),
        "invalid_argument",
    );
    assert!(
        missing_locator.contains("locator"),
        "缺 locator 要点名字段：{missing_locator}"
    );
    let bad_locator_type = expect_error(
        &call(
            handle,
            "sources.locate",
            json!({
                "source_ref": source_id,
                "locator": {"locator_type": "hologram", "source_revision_id": "rev-x"},
            }),
        ),
        "invalid_argument",
    );
    assert!(
        bad_locator_type.contains("locator_type") || bad_locator_type.contains("unknown variant"),
        "非法 locator_type 要如实报：{bad_locator_type}"
    );

    // camelCase 别名可用。
    let alias = expect_ok(&call(
        handle,
        "sources.extractedContent",
        json!({"sourceId": source_id}),
    ));
    assert_eq!(alias["source_id"], json!(source_id));
    // Dart 方法参数名 `ref` 也是 `source_ref` 的别名。
    let alias = expect_ok(&call(
        handle,
        "sources.locate",
        json!({
            "ref": source_id,
            "locator": {
                "locator_type": "text_range",
                "source_revision_id": revised_revision_id,
                "text_start": 0,
                "text_end": 1,
            },
        }),
    ));
    assert_eq!(alias["source_ref"], json!(source_id));

    assert_eq!(diary_close(handle), 0);
    std::fs::remove_dir_all(dir).unwrap();
}

/// `sources.locate` 的**可用**路径：有原件的来源返回 `available:true` + `asset_id`。
///
/// `imports.*` 还没接到 C ABI（属于后面的块），所以用核心自己的入口造一条带原件的
/// 来源——与 `enqueue_for_test` 同一个理由：这里要测的是 locate 的桥面形状，
/// 不是导入。纯文字来源的 `available:false` 已在上面的用例里钉住。
#[test]
fn locate_reports_an_available_asset_for_a_real_file_source() {
    let (dir, _path, handle) = open_temp("locate-available");
    let (source_id, asset_id) = seed_file_source(handle, "可定位.txt", b"locate me", "op-locate");

    let located = expect_ok(&call(
        handle,
        "sources.locate",
        json!({
            "source_ref": source_id,
            "locator": {
                "locator_type": "text_range",
                "source_revision_id": "rev-any",
                "text_start": 0,
                "text_end": 9,
            },
        }),
    ));
    assert_eq!(located["available"], json!(true), "{located}");
    assert_eq!(located["asset_id"], json!(asset_id), "{located}");
    assert_eq!(located["source_ref"], json!(source_id));
    assert!(
        located["reason"].is_null(),
        "可用时没有理由可报：{located}"
    );

    assert_eq!(diary_close(handle), 0);
    std::fs::remove_dir_all(dir).unwrap();
}
#[test]
fn events_poll_by_sequence_and_report_truncation() {
    let (dir, _path, handle) = open_temp("events");

    // 省略 from_sequence：从当前最新开始，不补历史。
    let start = expect_ok(&call(handle, "core.events", Value::Null));
    let start_sequence = start["next_sequence"].as_i64().unwrap();
    assert_eq!(start["events"], json!([]));
    assert_eq!(start["truncated"], json!(false));

    let capture = expect_ok(&call(
        handle,
        "captures.createDraft",
        json!({
            "time_zone": "Asia/Shanghai",
            "utc_offset_minutes": 480,
            "operation_id": "op-events-draft",
        }),
    ));
    expect_ok(&call(
        handle,
        "captures.saveDraft",
        json!({
            "capture_id": capture["id"],
            "text": "事件轮询用",
            "expected_revision": 1,
            "operation_id": "op-events-save",
        }),
    ));

    let page = expect_ok(&call(
        handle,
        "core.events",
        json!({"from_sequence": start_sequence, "limit": 200}),
    ));
    let events = page["events"].as_array().unwrap();
    assert!(!events.is_empty(), "两次写入必须产生事件：{page}");
    let mut previous = start_sequence;
    for event in events {
        let sequence = event["sequence"].as_i64().unwrap();
        assert!(sequence > previous, "序号必须单调递增：{event}");
        previous = sequence;
        assert!(
            event["type"].as_str().unwrap().contains('.'),
            "事件类型是线上字符串：{event}"
        );
        assert!(!event["event_id"].as_str().unwrap().is_empty());
        assert!(!event["entity_id"].as_str().unwrap().is_empty());
        assert!(event["emitted_at"].as_str().unwrap().ends_with('Z'));
        assert!(event["revision"].as_i64().unwrap() >= 1);
    }
    assert_eq!(page["next_sequence"], json!(previous));
    assert_eq!(page["truncated"], json!(false));

    // 没有新事件：同一游标的空页。游标不会自己往前跑。
    let idle = expect_ok(&call(
        handle,
        "core.events",
        json!({"from_sequence": page["next_sequence"]}),
    ));
    assert_eq!(idle["events"], json!([]));
    assert_eq!(idle["next_sequence"], page["next_sequence"]);

    // 游标**超前**（真实场景：调用方存了游标、库被恢复成更旧的备份）：夹到当前
    // 最大序号，不返回一个永远不会到来的未来序号——否则调用方持续回传就永久跳过
    // 之后的事件。
    let ahead = expect_ok(&call(
        handle,
        "core.events",
        json!({"from_sequence": 999_999}),
    ));
    assert_eq!(ahead["events"], json!([]));
    assert_eq!(
        ahead["next_sequence"], page["next_sequence"],
        "超前游标必须夹到当前最大序号（当前是 {}）：{ahead}",
        page["next_sequence"]
    );
    assert!(
        ahead["next_sequence"].as_i64().unwrap() < 999_999,
        "返回的 next_sequence 要小于调用方传的超前游标（调用方据此识别被夹过）：{ahead}"
    );
    assert_eq!(ahead["truncated"], json!(false));

    // 夹住之后的新事件仍然收得到：不是「永久跳过」。
    expect_ok(&call(
        handle,
        "captures.createDraft",
        json!({
            "time_zone": "Asia/Shanghai",
            "utc_offset_minutes": 480,
            "operation_id": "op-events-draft-2",
        }),
    ));
    let recovered = expect_ok(&call(
        handle,
        "core.events",
        json!({"from_sequence": ahead["next_sequence"]}),
    ));
    assert_eq!(
        recovered["events"].as_array().unwrap().len(),
        1,
        "超前游标夹住之后，下一条事件必须收得到：{recovered}"
    );

    // limit = 1：只给一条，并如实标 truncated；按 next_sequence 继续能推到同一末尾。
    let one = expect_ok(&call(
        handle,
        "core.events",
        json!({"from_sequence": start_sequence, "limit": 1}),
    ));
    assert_eq!(one["events"].as_array().unwrap().len(), 1);
    assert_eq!(one["truncated"], json!(true));
    let rest = expect_ok(&call(
        handle,
        "core.events",
        json!({"from_sequence": one["next_sequence"], "limit": 200}),
    ));
    // 上面「超前游标」那段又建了一条草稿，末尾比 page 多一条事件。
    assert_eq!(rest["next_sequence"], recovered["next_sequence"]);

    // 参数校验：0 / 超上限 / 负游标 / 拼错字段都是 invalid_argument。
    for bad in [
        json!({"limit": 0}),
        json!({"limit": 1001}),
        json!({"from_sequence": -1}),
        json!({"fromSequence": 0, "extra": 1}),
    ] {
        expect_error(&call(handle, "core.events", bad), "invalid_argument");
    }
    // camelCase 别名可用。
    let alias = expect_ok(&call(
        handle,
        "core.events",
        json!({"fromSequence": start_sequence, "limit": 1}),
    ));
    assert_eq!(alias["truncated"], json!(true));

    assert_eq!(diary_close(handle), 0);
    std::fs::remove_dir_all(dir).unwrap();
}

/// `backups.create` / `inspect` / `restore`：真实落盘、自校验、恢复到新库。
#[test]
fn backups_create_inspect_and_restore_round_trip() {
    let (dir, _path, handle) = open_temp("backups");
    let (capture_id, source_id) = seed_committed_capture(handle, "备份正文：今天走了 8700 步。");

    let package = dir.join("library-backup.diarybackup");
    let package_text = package.to_string_lossy().into_owned();
    let created = expect_ok(&call(
        handle,
        "backups.create",
        json!({"target_path": package_text}),
    ));
    assert_eq!(created["package_path"], json!(package_text));
    // 备份清单里的 `apiVersion` 必须与核心同源（`diary_core::API_VERSION`），
    // 不是某个写死的字面量。这条断言原本写死 "1.0"，用来钉「#153 加 8 个方法
    // 不抬版本」；但 #160 因为给 `SearchHit` 加了一个**必填**响应字段把版本抬到
    // 1.1 之后，它就在 main 上一直红（两次合并的顺序让两边各自本地都绿）。
    //
    // 它真正要守的是「桥面不自造版本号」，所以改成与常量比。
    assert_eq!(
        created["api_version"],
        json!(API_VERSION),
        "备份清单的 apiVersion 必须与核心同源，不许自造"
    );
    assert!(package.is_file(), "备份包必须真的落在目标路径");
    assert!(created["counts"]["captures"].as_i64().unwrap() >= 1);
    assert!(created["counts"]["sourceRevisions"].as_i64().unwrap() >= 1);
    assert!(created["byte_size"].as_i64().unwrap() > 0);

    let inspect = expect_ok(&call(
        handle,
        "backups.inspect",
        json!({"package_path": package_text}),
    ));
    assert_eq!(inspect["package_path"], json!(package_text));
    assert_eq!(inspect["api_version"], json!(API_VERSION));
    assert_eq!(inspect["compatible"], json!(true));
    assert_eq!(inspect["hashes_verified"], json!(true));
    assert_eq!(inspect["issues"], json!([]));
    assert_eq!(inspect["counts"], created["counts"]);

    // 恢复到新目录：绝不覆盖当前库。
    let restore_dir = dir.join("restored-library");
    let restored = expect_ok(&call(
        handle,
        "backups.restore",
        json!({
            "package_path": package_text,
            "target_dir": restore_dir.to_string_lossy(),
        }),
    ));
    let library_path = restored["library_path"].as_str().unwrap().to_owned();
    assert!(Path::new(&library_path).is_file(), "恢复出来的库必须存在");
    assert_eq!(restored["api_version"], json!(API_VERSION));
    assert!(restored["counts"]["captures"].as_i64().unwrap() >= 1);
    assert_eq!(restored["counts"], created["counts"]);

    // 恢复出来的库能直接打开，记录与正文都在。
    let restored_handle = open(Path::new(&library_path));
    let page = expect_ok(&call(restored_handle, "captures.list", json!({})));
    assert!(
        page["captures"]
            .as_array()
            .unwrap()
            .iter()
            .any(|capture| capture["id"] == json!(capture_id)),
        "恢复出来的库缺原记录：{page}"
    );
    let content = expect_ok(&call(
        restored_handle,
        "sources.extractedContent",
        json!({"source_id": source_id}),
    ));
    assert!(content["text"].as_str().unwrap().contains("8700"));
    assert_eq!(diary_close(restored_handle), 0);

    // 目标已存在：核心拒绝覆盖旧备份（invalid_argument）。
    expect_error(
        &call(
            handle,
            "backups.create",
            json!({"target_path": package_text}),
        ),
        "invalid_argument",
    );
    // 只支持完整备份：include_assets=false 明确拒绝。
    let error = expect_error(
        &call(
            handle,
            "backups.create",
            json!({
                "target_path": dir.join("small.zip").to_string_lossy(),
                "include_assets": false,
            }),
        ),
        "invalid_argument",
    );
    assert!(error.contains("完整备份"), "{error}");
    // 只支持恢复到新库：restore_as_new_library=false 明确拒绝。
    let error = expect_error(
        &call(
            handle,
            "backups.restore",
            json!({
                "package_path": package_text,
                "target_dir": dir.join("overwrite").to_string_lossy(),
                "options": {"restore_as_new_library": false},
            }),
        ),
        "invalid_argument",
    );
    assert!(error.contains("新库"), "{error}");
    // 拼错的字段与缺字段都不能静默通过。
    let typo = expect_error(
        &call(
            handle,
            "backups.inspect",
            json!({"package_path": package_text, "extra": 1}),
        ),
        "invalid_argument",
    );
    assert!(typo.contains("参数不合法"), "{typo}");
    expect_error(
        &call(handle, "backups.create", json!({})),
        "invalid_argument",
    );

    // **票据不是路径**：契约里 targetTicket/packageTicket 是 PlatformHost 的限域票据，
    // C ABI 在票据层之下。收到票据字段要显式拒绝并说清该在哪一层解析；绝不能按票据
    // 字符串真的建文件（桌面端）或被沙箱判定拒绝（OHOS，错误还会指向「路径不在沙箱内」）。
    let ticket_target = dir.join("ticket-target.diarybackup");
    let ticket_error = expect_error(
        &call(
            handle,
            "backups.create",
            json!({"targetTicket": ticket_target.to_string_lossy()}),
        ),
        "invalid_argument",
    );
    assert!(
        ticket_error.contains("票据") && ticket_error.contains("target_path"),
        "票据字段要显式拒绝并指向 target_path：{ticket_error}"
    );
    assert!(
        !ticket_target.exists(),
        "不能把票据当路径真的建出文件：{}",
        ticket_target.display()
    );
    let ticket_error = expect_error(
        &call(
            handle,
            "backups.inspect",
            json!({"packageTicket": "ticket-123"}),
        ),
        "invalid_argument",
    );
    assert!(ticket_error.contains("票据"), "{ticket_error}");
    // snake_case 的票据写法同样拒绝（它们曾作为路径别名被接受）。
    expect_error(
        &call(
            handle,
            "backups.restore",
            json!({
                "package_ticket": "ticket-123",
                "target_dir": dir.join("ticket-restore").to_string_lossy(),
            }),
        ),
        "invalid_argument",
    );

    assert_eq!(diary_close(handle), 0);
    std::fs::remove_dir_all(dir).unwrap();
}

/// `backups.inspect` 的**损坏包 `issues` 路径**：清单读得出来、但某个原件哈希对不上时，
/// `inspect` 不抛错（清单本身没坏），而是把 `corrupted:` 放进 `issues` 并让
/// `hashes_verified=false`——「先看再用」的入口必须如实说包坏了。
///
/// 与核心测试 `e35_corrupted_backup_is_rejected_and_leaves_no_half_product` 同一个
/// 造法：改坏一个 `blobs/` 条目的一个字节，其余条目原样写回。
#[test]
fn inspect_reports_corrupted_entries_as_issues() {
    let (dir, _path, handle) = open_temp("backup-inspect-corrupted");
    seed_committed_capture(handle, "备份正文：损坏包检查。");
    // 损坏的是 `blobs/` 条目：备份里必须有一条**真实原件**（纯文字来源没有 blob）。
    seed_file_source(handle, "原件.txt", b"backup me", "op-backup-file");

    let package = dir.join("good.diarybackup");
    let package_text = package.to_string_lossy().into_owned();
    let created = expect_ok(&call(
        handle,
        "backups.create",
        json!({"target_path": package_text}),
    ));
    assert!(created["byte_size"].as_i64().unwrap() > 0);

    let mut entries = read_package_entries(&package);
    let last_blob = entries
        .iter()
        .rposition(|(name, _)| name.starts_with("blobs/"))
        .expect("完整备份必须含原件条目");
    let corrupted_name = entries[last_blob].0.clone();
    entries[last_blob].1[0] ^= 0xff;
    let broken = dir.join("broken.diarybackup");
    write_package_entries(&broken, &entries);

    let inspection = expect_ok(&call(
        handle,
        "backups.inspect",
        json!({"package_path": broken.to_string_lossy()}),
    ));
    assert_eq!(
        inspection["hashes_verified"],
        json!(false),
        "坏了就不能说哈希都验过：{inspection}"
    );
    assert_eq!(
        inspection["compatible"], json!(true),
        "清单/版本本身没问题，所以仍是 compatible：{inspection}"
    );
    let issues = inspection["issues"].as_array().unwrap();
    assert!(
        issues
            .iter()
            .any(|issue| issue.as_str().unwrap().starts_with("corrupted:")),
        "损坏要进 issues 并点名 corrupted：{inspection}"
    );
    assert!(
        issues
            .iter()
            .any(|issue| issue.as_str().unwrap().contains(&corrupted_name)),
        "issues 要点明坏在哪一条（{corrupted_name}）：{inspection}"
    );

    assert_eq!(diary_close(handle), 0);
    std::fs::remove_dir_all(dir).unwrap();
}

// ---------------------------------------------------------------- 备份包读写（测试脚手架）

/// 按条目名读回整个 zip（`manifest.json` 在第一个）。
fn read_package_entries(path: &Path) -> Vec<(String, Vec<u8>)> {
    let file = std::fs::File::open(path).expect("打开备份包");
    let mut archive = zip::ZipArchive::new(file).expect("备份包是 zip");
    let mut entries = Vec::new();
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).expect("读 zip 条目");
        let name = entry.name().to_owned();
        let mut bytes = Vec::new();
        std::io::Read::read_to_end(&mut entry, &mut bytes).expect("读条目字节");
        entries.push((name, bytes));
    }
    entries
}

/// 按原顺序写回（manifest 在最前，其余 Stored）：只用于造「内容被改坏」的包。
fn write_package_entries(path: &Path, entries: &[(String, Vec<u8>)]) {
    use std::io::Write;
    let file = std::fs::File::create(path).expect("建包");
    let mut zip = zip::ZipWriter::new(file);
    let mut manifest_written = false;
    for (name, bytes) in entries {
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        zip.start_file(name.clone(), options).expect("写条目");
        if name == "manifest.json" {
            manifest_written = true;
        }
        zip.write_all(bytes).expect("写条目字节");
    }
    assert!(manifest_written, "包里一定有清单");
    zip.finish().expect("收尾");
}

/// 备份路径的沙箱判定：OHOS 上必须命中应用数据前缀，且不能含 `..`。
///
/// 真机构建才会 `enforce`；这里直接测判定内核，两半都覆盖（否则宿主单测只能测到
/// 「不限制」那一半）。
#[test]
fn backup_paths_are_confined_to_the_ohos_sandbox_when_enforced() {
    for inside in [
        "/data/storage/el2/base/files/backup.diarybackup",
        "/data/storage/el1/base/files/backup.diarybackup",
        "/data/storage/el1/database/backup.diarybackup",
        "/data/storage/el1/cache/backup.diarybackup",
        "/data/app/el2/100/base/com.example.diary/files/backup.diarybackup",
    ] {
        assert!(
            crate::methods::validate_sandbox_path_with("target_path", inside, true).is_ok(),
            "沙箱内路径应当通过：{inside}"
        );
    }
    for outside in [
        "/tmp/backup.diarybackup",
        "/sdcard/backup.diarybackup",
        // 安装目录是只读的，备份不能写那里。
        "/data/storage/el1/bundle/backup.diarybackup",
        // 前缀命中但 `..` 会跳出沙箱。
        "/data/storage/el2/base/../../../etc/backup.diarybackup",
    ] {
        let error = crate::methods::validate_sandbox_path_with("target_path", outside, true)
            .expect_err("沙箱外路径必须拒绝");
        assert!(
            format!("{error:?}").contains("沙箱"),
            "{outside}: {error:?}"
        );
    }
    // 非 OHOS（桌面 / CLI / 宿主单测）不限制。
    assert!(
        crate::methods::validate_sandbox_path_with("target_path", "/tmp/backup.zip", false).is_ok()
    );
}

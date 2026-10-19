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
    let env_clean = ["DIARY_MODEL_DIR", "DIARY_ORT_DYLIB", "DIARY_ASR_MODEL_DIR", "DIARY_ASR_ENGINE_LIB"]
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
        recovery["notes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|note| note.as_str().unwrap().contains("未封闭的尾巴")
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
        &call(handle, "recordings.recover", json!({"recording_id": "rec_missing"})),
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
        report["message"].as_str().unwrap_or_default().contains("没有实现"),
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
    assert!(message.contains("随包") || message.contains("内建"), "{message}");

    assert_eq!(diary_close(handle), 0);
    std::fs::remove_dir_all(dir).unwrap();
}

/// 第 1 组：`captures.get` / `captures.list`——「看今天」、历史列表、条目详情的
/// 数据源，游标必须是**真游标**（第二页接得上、不重不漏）。
#[test]
fn captures_get_and_list_use_real_cursor_pagination() {
    let (dir, _path, handle) = open_temp("captures-read");

    // 三条：两条在 10-17（上海），一条在 10-16。
    let older = seed_capture_at(handle, "op-capi-list-a", "2026-10-16T09:30:00Z", "前天的记录");
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
    assert_eq!(succeeded["jobs"].as_array().unwrap().len(), 0, "{succeeded}");

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
    let retried = expect_ok(&call(handle, "jobs.retry", json!({"job_id": job_id.clone()})));
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
        &call(handle, "search.cancel", json!({"session_id": "search_missing"})),
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
    expect_error(&call(handle, "search.upgrade", json!(null)), "invalid_argument");
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

/// 能力清单必须随接线增长：`core.info.capabilities == WIRED_METHODS`，30 条、无重复。
#[test]
fn capabilities_include_every_wired_method() {
    let (dir, _path, handle) = open_temp("capabilities");
    let info = expect_ok(&call(handle, "core.info", Value::Null));
    assert_eq!(info["api_version"], json!("1.0"), "加方法不升 apiVersion");

    let capabilities: Vec<&str> = info["capabilities"]
        .as_array()
        .unwrap()
        .iter()
        .map(|name| name.as_str().unwrap())
        .collect();
    assert_eq!(capabilities.len(), 30, "{capabilities:?}");
    let mut unique = capabilities.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(unique.len(), capabilities.len(), "能力清单有重复");

    for name in [
        "captures.list",
        "captures.get",
        "indexes.status",
        "jobs.list",
        "jobs.get",
        "jobs.retry",
        "jobs.cancel",
        "jobs.acknowledgeAttention",
        "search.nextPage",
        "search.upgrade",
        "search.cancel",
    ] {
        assert!(capabilities.contains(&name), "能力清单缺 {name}");
    }

    assert_eq!(diary_close(handle), 0);
    std::fs::remove_dir_all(dir).unwrap();
}

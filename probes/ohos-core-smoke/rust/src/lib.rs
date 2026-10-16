//! 鸿蒙沙箱探针：把 `diary_core` 编成 OHOS 共享库，导出最小 C ABI。
//!
//! **这不是产品级 C ABI 设计**（任务书明确要求不要顺手做）。它只回答一个问题：
//! *在鸿蒙应用的沙箱里，`diary_core`（含 bundled SQLite 的 C 代码）能不能真的建库、
//! 跑迁移、写入、读回，各自多快、库落在哪、多大。*
//!
//! 导出的两个符号（HAP 里的 C++ NAPI shim 用 `dlopen`+`dlsym` 拿它们）：
//!
//! ```c
//! char *diary_smoke_run(const char *db_dir);  /* 返回 malloc 出来的 JSON（UTF-8），出错也是 JSON */
//! void  diary_smoke_free(char *p);
//! const char *diary_smoke_abi(void);          /* 版本戳，用来确认 dlsym 拿到的是哪一个库 */
//! char *diary_ort_crate_probe(const char *ort_so);  /* 走产品真实路径：ort crate 的 load-dynamic */
//! ```
//!
//! 结果 JSON 里的字段全部是**设备侧实测值**，没有估算。

use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::time::Instant;

use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

/// ABI 版本戳：shim 侧会把它打进日志，用来确认 dlopen 出来的确实是这个库。
const ABI_TAG: &str = "diary_core_smoke/1 ohos-cdylib";

/// 探针主入口：在 `db_dir` 下建库、跑迁移、写入、读回，返回 JSON 结果字符串。
///
/// 返回的指针必须用 [`diary_smoke_free`] 释放。
#[no_mangle]
pub extern "C" fn diary_smoke_run(db_dir: *const c_char) -> *mut c_char {
    let dir = if db_dir.is_null() {
        String::from("<null>")
    } else {
        // SAFETY: 调用方保证传进来的是 NUL 结尾的 UTF-8 C 字符串。
        unsafe { CStr::from_ptr(db_dir) }
            .to_string_lossy()
            .into_owned()
    };

    let out = match catch_unwind(AssertUnwindSafe(|| run(&dir))) {
        Ok(v) => v,
        Err(_) => json!({
            "stage": "panic",
            "error": "diary_smoke_run 捕获到 panic（详见 hilog 的 panic 输出）",
            "dir": dir,
        }),
    };

    let text = serde_json::to_string(&out).unwrap_or_else(|e| {
        format!("{{\"stage\":\"error\",\"error\":\"json 序列化失败: {e}\"}}")
    });
    // 结尾不能有内嵌 NUL；JSON 里不会有。
    CString::new(text)
        .unwrap_or_else(|_| CString::new("{\"stage\":\"error\"}").unwrap())
        .into_raw()
}

/// 释放 [`diary_smoke_run`] 返回的字符串。
///
/// # Safety
/// `p` 必须是 `diary_smoke_run` 返回、且还没被释放过的指针（或空指针）。
#[no_mangle]
pub unsafe extern "C" fn diary_smoke_free(p: *mut c_char) {
    if !p.is_null() {
        drop(CString::from_raw(p));
    }
}

/// ABI 版本戳（静态字符串，不需要释放）。
#[no_mangle]
pub extern "C" fn diary_smoke_abi() -> *const c_char {
    // 静态字面量结尾一定有 NUL，可以安全地当 C 字符串用。
    static TAG: &[u8] = b"diary_core_smoke/1 ohos-cdylib\0";
    let _ = ABI_TAG;
    TAG.as_ptr().cast::<c_char>()
}

/// **产品的真实调用路径**：`ort` crate 的 load-dynamic ——
/// `ort::init_from(<.so 路径>)` 然后 `.commit()`。`diary_core` 的语义检索走的就是这条。
///
/// 单独导出一个函数，是为了把两件事分开报：
///   ① 「沙箱能不能 dlopen ORT」→ 用 ORT 的 C API 直接验（见 `entry/src/main/cpp/ort_probe.cpp`）；
///   ② 「我们这版 `ort` crate 能不能用这个 ORT」→ 用这里验。
///
/// 注意：`ort` crate 在 ORT 的 C API 版本对不上时是 **panic**
/// （`NonNull::new(api).expect("Failed to initialize ORT API")`），不是 `Err`，
/// 所以必须 `catch_unwind` 才能把 panic 原文带出来给设备日志。
#[no_mangle]
pub extern "C" fn diary_ort_crate_probe(ort_so: *const c_char) -> *mut c_char {
    let path = if ort_so.is_null() {
        String::new()
    } else {
        // SAFETY: 调用方保证传进来的是 NUL 结尾的 UTF-8 C 字符串。
        unsafe { CStr::from_ptr(ort_so) }.to_string_lossy().into_owned()
    };

    let value = match catch_unwind(AssertUnwindSafe(|| {
        let mut r = Map::new();
        r.insert("ort_so".into(), json!(path));
        // `ort` crate 要求的 ORT C API 版本号（== 它期望的 ORT minor 版本）
        r.insert("crate_required_ort_api_version".into(), json!(ort::MINOR_VERSION));
        match ort::init_from(&path) {
            Ok(builder) => {
                r.insert("init_from_ok".into(), json!(true));
                // commit() 返回 bool（是不是本进程第一次提交），并且会**真正取 OrtApi 表**
                // —— ORT 太旧就在这里 panic，被外层 catch_unwind 接住。
                let first = builder.commit();
                r.insert("commit_ok".into(), json!(true));
                r.insert("commit_first_in_process".into(), json!(first));
            }
            Err(e) => {
                r.insert("init_from_ok".into(), json!(false));
                r.insert("init_from_err".into(), json!(e.to_string()));
            }
        }
        Value::Object(r)
    })) {
        Ok(v) => v,
        Err(p) => {
            let msg = p
                .downcast_ref::<&str>()
                .map(|s| (*s).to_owned())
                .or_else(|| p.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "<无法取出 panic 文本>".to_owned());
            json!({
                "ort_so": path,
                "crate_required_ort_api_version": ort::MINOR_VERSION,
                "panicked": true,
                "panic": msg,
            })
        }
    };

    let text = serde_json::to_string(&value).unwrap_or_else(|_| "{}".to_owned());
    CString::new(text)
        .unwrap_or_else(|_| CString::new("{}").unwrap())
        .into_raw()
}

// ─────────────────────────── 实现 ───────────────────────────

fn ms(d: std::time::Duration) -> f64 {
    // 保留 3 位小数，单位毫秒。
    (d.as_secs_f64() * 1000_000.0).round() / 1000.0
}

fn run(dir: &str) -> Value {
    let t_all = Instant::now();
    let mut r = Map::new();
    let mut errors: Vec<String> = Vec::new();
    r.insert("stage".into(), json!("ok"));
    r.insert("dir".into(), json!(dir));

    let root = std::path::Path::new(dir);
    if let Err(e) = std::fs::create_dir_all(root) {
        r.insert("stage".into(), json!("error"));
        r.insert("error".into(), json!(format!("create_dir_all({dir}) 失败: {e}")));
        return Value::Object(r);
    }

    // ── 1. 裸 rusqlite：证明 SQLite 的 C 代码在沙箱里能跑 ─────────────────
    let raw_db = root.join("raw_smoke.db");
    for suffix in ["", "-wal", "-shm", "-journal"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", raw_db.display()));
    }
    let t = Instant::now();
    match rusqlite::Connection::open(&raw_db) {
        Ok(conn) => {
            r.insert("raw_open_ms".into(), json!(ms(t.elapsed())));
            let t = Instant::now();
            let pragma = conn
                .execute_batch(
                    "PRAGMA journal_mode=WAL;\
                     PRAGMA synchronous=FULL;\
                     CREATE TABLE t(id INTEGER PRIMARY KEY, s TEXT NOT NULL);",
                )
                .map_err(|e| e.to_string());
            r.insert("raw_pragma_create_ms".into(), json!(ms(t.elapsed())));
            if let Err(e) = pragma {
                errors.push(format!("raw pragma/create: {e}"));
            }

            let t = Instant::now();
            let ins = (|| -> Result<(), String> {
                let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
                for i in 0..1000 {
                    tx.execute(
                        "INSERT INTO t(s) VALUES (?1)",
                        rusqlite::params![format!("沙箱记录-{i}")],
                    )
                    .map_err(|e| e.to_string())?;
                }
                tx.commit().map_err(|e| e.to_string())
            })();
            r.insert("raw_insert1000_ms".into(), json!(ms(t.elapsed())));
            if let Err(e) = ins {
                errors.push(format!("raw insert1000: {e}"));
            }

            // 再来一批 10000 行：给一个更像样的写入吞吐数字（同一个事务）。
            let t = Instant::now();
            let ins10k = (|| -> Result<(), String> {
                let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
                for i in 0..10_000 {
                    tx.execute(
                        "INSERT INTO t(s) VALUES (?1)",
                        rusqlite::params![format!("批量记录-{i}")],
                    )
                    .map_err(|e| e.to_string())?;
                }
                tx.commit().map_err(|e| e.to_string())
            })();
            r.insert("raw_insert10000_ms".into(), json!(ms(t.elapsed())));
            if let Err(e) = ins10k {
                errors.push(format!("raw insert10000: {e}"));
            }

            // 单行写入（用户按一次「保存」的量级）：一次 INSERT + 一次按 id 读回。
            let t = Instant::now();
            let one = conn
                .execute(
                    "INSERT INTO t(s) VALUES (?1)",
                    rusqlite::params!["单条记录"],
                )
                .map_err(|e| e.to_string());
            r.insert("raw_insert1_ms".into(), json!(ms(t.elapsed())));
            if let Err(e) = one {
                errors.push(format!("raw insert1: {e}"));
            }

            let t = Instant::now();
            let rows: Result<(i64, String), String> = (|| {
                let n: i64 = conn
                    .query_row("SELECT count(*) FROM t", [], |row| row.get(0))
                    .map_err(|e| e.to_string())?;
                let v: String = conn
                    .query_row("SELECT s FROM t WHERE id=7", [], |row| row.get(0))
                    .map_err(|e| e.to_string())?;
                Ok((n, v))
            })();
            r.insert("raw_read_ms".into(), json!(ms(t.elapsed())));
            match rows {
                Ok((n, v)) => {
                    r.insert("raw_rows".into(), json!(n));
                    r.insert("raw_row7".into(), json!(v));
                }
                Err(e) => errors.push(format!("raw read: {e}")),
            }

            match conn.query_row("SELECT sqlite_version()", [], |row| row.get::<_, String>(0)) {
                Ok(v) => {
                    r.insert("sqlite_version".into(), json!(v));
                }
                Err(e) => errors.push(format!("sqlite_version: {e}")),
            }
            // 额外一条：SQLite 自己的编译期选项（证明用的是 bundled 源码而不是系统库）
            match conn.query_row("PRAGMA journal_mode", [], |row| row.get::<_, String>(0)) {
                Ok(v) => {
                    r.insert("raw_journal_mode".into(), json!(v));
                }
                Err(e) => errors.push(format!("journal_mode: {e}")),
            }
            drop(conn);
        }
        Err(e) => {
            r.insert("raw_open_ms".into(), json!(ms(t.elapsed())));
            errors.push(format!("raw open: {e}"));
        }
    }
    r.insert("raw_db_bytes".into(), json!(file_len(&raw_db)));

    // ── 2. diary_core：真 schema 迁移 + 写入 + 读回 ───────────────────────
    let core_db = root.join("core.db");
    for suffix in ["", "-wal", "-shm", "-journal"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", core_db.display()));
    }
    let t = Instant::now();
    match diary_core::Core::open(&core_db) {
        Ok(mut core) => {
            r.insert("core_open_cold_ms".into(), json!(ms(t.elapsed())));
            match core.schema_version() {
                Ok(v) => {
                    r.insert("core_schema_version".into(), json!(v));
                }
                Err(e) => errors.push(format!("schema_version: {e}")),
            }
            r.insert("core_db_bytes_after_migrate".into(), json!(file_len(&core_db)));

            // 写入：建草稿 → 存正文
            let t = Instant::now();
            let draft = core.create_draft(diary_core::CreateDraftInput {
                occurred_at: None,
                time_zone: "Asia/Shanghai",
                utc_offset_minutes: 480,
                operation_id: "ohos-smoke-op-1",
            });
            r.insert("core_create_draft_ms".into(), json!(ms(t.elapsed())));
            match draft {
                Ok(cap) => {
                    r.insert("core_capture_id".into(), json!(cap.id));
                    let text = "鸿蒙沙箱里写进去的第一条记录：SQLite 在 el2 里能读能写。";
                    let t = Instant::now();
                    let saved = core.save_draft(&cap.id, text, 1, "ohos-smoke-op-2");
                    r.insert("core_save_draft_ms".into(), json!(ms(t.elapsed())));
                    match saved {
                        Ok(res) => {
                            r.insert("core_save_durable".into(), json!(res.durable));
                            r.insert("core_save_revision".into(), json!(res.revision));
                        }
                        Err(e) => errors.push(format!("save_draft: {e}")),
                    }

                    // 第二次保存：把「首次写入的一次性开销」（jieba 词典懒加载 / 文件首次落盘）
                    // 与「稳态写入」分开。第一次 130 ms 是不是一次性成本，看这个数字。
                    let t = Instant::now();
                    match core.save_draft(&cap.id, "第二次保存（稳态）", 2, "ohos-smoke-op-3") {
                        Ok(res) => {
                            r.insert("core_save_draft_2nd_ms".into(), json!(ms(t.elapsed())));
                            r.insert("core_save_revision_2nd".into(), json!(res.revision));
                        }
                        Err(e) => {
                            r.insert("core_save_draft_2nd_ms".into(), json!(ms(t.elapsed())));
                            errors.push(format!("save_draft 2nd: {e}"));
                        }
                    }

                    let t = Instant::now();
                    match core.get_capture(&cap.id) {
                        Ok(got) => {
                            r.insert("core_read_back_ms".into(), json!(ms(t.elapsed())));
                            r.insert("core_read_back_text".into(), json!(got.draft_text));
                            r.insert("core_read_back_ok".into(), json!(got.draft_text == "第二次保存（稳态）"));
                            let mut h = Sha256::new();
                            h.update(got.draft_text.as_bytes());
                            r.insert(
                                "core_read_back_sha256".into(),
                                json!(hex(&h.finalize())),
                            );
                        }
                        Err(e) => errors.push(format!("get_capture: {e}")),
                    }
                }
                Err(e) => errors.push(format!("create_draft: {e}")),
            }

            // 索引 + 中文检索（证明 jieba/FTS 这条链在沙箱里也能走）
            // 顺序与产品一致：先切文本块（chunks），再建关键词索引。
            let t = Instant::now();
            match core.rebuild_text_chunks(None) {
                Ok(n) => {
                    r.insert("core_chunks_ms".into(), json!(ms(t.elapsed())));
                    r.insert("core_chunk_count".into(), json!(n));
                }
                Err(e) => {
                    r.insert("core_chunks_ms".into(), json!(ms(t.elapsed())));
                    errors.push(format!("rebuild_text_chunks: {e}"));
                }
            }
            let t = Instant::now();
            match core.rebuild_keyword_index(None) {
                Ok(n) => {
                    r.insert("core_index_ms".into(), json!(ms(t.elapsed())));
                    r.insert("core_indexed_sources".into(), json!(n));
                    match core.count_search_matches("鸿蒙") {
                        Ok(hits) => {
                            r.insert("core_search_hits_鸿蒙".into(), json!(hits));
                        }
                        Err(e) => errors.push(format!("count_search_matches: {e}")),
                    }
                    match core.search_candidates("鸿蒙", 5) {
                        Ok(ids) => {
                            r.insert("core_search_candidates_鸿蒙".into(), json!(ids.len()));
                        }
                        Err(e) => errors.push(format!("search_candidates: {e}")),
                    }
                }
                Err(e) => {
                    r.insert("core_index_ms".into(), json!(ms(t.elapsed())));
                    errors.push(format!("rebuild_keyword_index: {e}"));
                }
            }

            match core.count_captures() {
                Ok(n) => {
                    r.insert("core_capture_count".into(), json!(n));
                }
                Err(e) => errors.push(format!("count_captures: {e}")),
            }

            // 库文件的真实体积要在**连接还开着**时量：WAL 模式下 db/-wal/-shm 三个文件
            // 加起来才是占用；关掉连接后 SQLite 会 checkpoint 并把 -wal 清掉。
            r.insert("core_db_bytes_open".into(), json!(file_len(&core_db)));
            r.insert(
                "core_db_wal_bytes_open".into(),
                json!(file_len(std::path::Path::new(&format!("{}-wal", core_db.display())))),
            );
            r.insert(
                "core_db_shm_bytes_open".into(),
                json!(file_len(std::path::Path::new(&format!("{}-shm", core_db.display())))),
            );

            drop(core);

            // 二次打开（迁移已完成）→ 热启动耗时。启动路径上的真实数字。
            let t = Instant::now();
            match diary_core::Core::open(&core_db) {
                Ok(core2) => {
                    r.insert("core_open_warm_ms".into(), json!(ms(t.elapsed())));
                    drop(core2);
                }
                Err(e) => errors.push(format!("reopen: {e}")),
            }
        }
        Err(e) => {
            r.insert("core_open_cold_ms".into(), json!(ms(t.elapsed())));
            errors.push(format!("Core::open: {e}"));
        }
    }
    r.insert("core_db_bytes".into(), json!(file_len(&core_db)));
    // WAL/SHM 是 core.db-wal / core.db-shm，手工拼路径最清楚。
    r.insert(
        "core_db_wal_bytes".into(),
        json!(file_len(std::path::Path::new(&format!("{}-wal", core_db.display())))),
    );
    r.insert(
        "core_db_shm_bytes".into(),
        json!(file_len(std::path::Path::new(&format!("{}-shm", core_db.display())))),
    );

    // ── 3. 目录实况：库到底落在哪、旁边还有什么 ──────────────────────────
    r.insert("dir_listing".into(), json!(list_dir(root)));

    r.insert("errors".into(), json!(errors));
    r.insert("total_ms".into(), json!(ms(t_all.elapsed())));
    Value::Object(r)
}

fn file_len(p: &std::path::Path) -> u64 {
    std::fs::metadata(p).map(|m| m.len()).unwrap_or(0)
}

fn list_dir(root: &std::path::Path) -> Vec<Value> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(root) {
        let mut entries: Vec<_> = rd.flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            let md = e.metadata().ok();
            out.push(json!({
                "name": e.file_name().to_string_lossy(),
                "bytes": md.as_ref().map(|m| m.len()).unwrap_or(0),
                "is_dir": md.as_ref().map(|m| m.is_dir()).unwrap_or(false),
            }));
        }
    }
    out
}

fn hex(b: &[u8]) -> String {
    let mut s = String::with_capacity(b.len() * 2);
    for x in b {
        s.push_str(&format!("{x:02x}"));
    }
    s
}
/// 宿主侧对照实验（**不是产品代码，只在 WSL x86_64 上跑**）：
/// `cargo test -- --nocapture` 会在宿主上跑与设备**完全相同**的那段 `run()`。
/// 目的：把「设备侧数字」与「同机型宿主数字」放在一起，任何设备独有的差异才有意义。
#[cfg(test)]
mod host_control {
    #[test]
    fn runs_the_same_sequence_on_host() {
        let dir = std::env::temp_dir().join("diary-core-smoke-host");
        let _ = std::fs::remove_dir_all(&dir);
        let v = super::run(dir.to_str().unwrap());
        println!(
            "HOST_CONTROL {}",
            serde_json::to_string_pretty(&v).unwrap_or_default()
        );
        assert_eq!(v["stage"], "ok");
        assert_eq!(v["errors"], serde_json::json!([]));
    }
}

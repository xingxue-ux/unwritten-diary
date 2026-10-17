//! 鸿蒙沙箱探针（第 3 步）：把**真 bge-small-zh-v1.5 int8** 放进 HAP，
//! 在应用沙箱里用**产品同一条路径**跑真嵌入。
//!
//! **这不是产品级 C ABI 设计**，也不改一行产品代码。它回答四个问题：
//!
//! 1. 22.73 MiB 的 int8 ONNX + 0.42 MiB tokenizer 从 HAP 的 rawfile 落到 `filesDir`
//!    之后，`diary_core`（`ort` crate load-dynamic + `embedding.rs` 口径）能不能加载；
//! 2. 512 维 CLS 向量在设备上算出来，与**宿主同代码同模型**的结果差多少
//!    （余弦 / 最大逐元素偏差 —— int8 量化下**不**期望逐位相同，只如实报差异量级）；
//! 3. 语义判据：相近文本的相似度是否高于无关文本；
//! 4. 耗时（加载 / 单次嵌入冷热）与常驻内存增量。
//!
//! 导出的 C ABI（HAP 里的 C++ NAPI shim 用 `dlopen`+`dlsym` 拿它们）：
//!
//! ```c
//! char *diary_bge_run(const char *params_json);  /* 入参 JSON，出参 malloc 出来的 JSON */
//! void  diary_bge_free(char *p);
//! const char *diary_bge_abi(void);
//! ```
//!
//! 入参 JSON（`{"model_dir": "...", "ort_so": "..."}`）由 shim 侧拼；
//! 语料**写死在本文件里**，宿主对照跑的是同一份常量（`corpus_sha256` 用来证明这点）。

use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::time::Instant;

use diary_core::embedding::{load, EmbedOptions, Embedder, EmbeddingConfig, Pooling};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

/// ABI 版本戳：shim 侧会把它打进日志，用来确认 dlopen 出来的确实是这个库。
const ABI_TAG: &str = "diary_bge_smoke/1 ohos-cdylib";

/// 长文本的重复单元（中文约 1 字 ≈ 1 token，所以重复次数直接控制 token 数）。
const LONG_UNIT: &str =
    "那天傍晚我沿着河堤慢慢走了很久，风从水面吹过来，带着一点凉意，路灯一盏一盏亮起来。";

/// 探针语料：**设备与宿主共用这一份常量**（`corpus_sha256` 是它的指纹）。
///
/// `kind` 只用于报告分组：`semantic` = 语义判据用的四句；`perf` = 冷热耗时用的三种长度；
/// `rank_query` / `rank_doc` = 排序一致性用的查询与候选。
struct ProbeText {
    key: &'static str,
    kind: &'static str,
    text: String,
}

fn corpus() -> Vec<ProbeText> {
    let mut out: Vec<ProbeText> = Vec::new();
    let mut push = |key: &'static str, kind: &'static str, text: String| {
        out.push(ProbeText { key, kind, text })
    };

    // ── 语义判据：两段相近 + 一段无关 ────────────────────────────────────
    push(
        "main",
        "semantic",
        "今天下午妈妈打电话来，说院子里的桂花开了，香气飘了满屋。".to_owned(),
    );
    push("near_a", "semantic", "妈妈打电话说家里的桂花开了。".to_owned());
    push(
        "near_b",
        "semantic",
        "母亲来电话提到院中的桂花已经盛开。".to_owned(),
    );
    push(
        "far",
        "semantic",
        "机房空调故障导致磁盘阵列温度过高，值班同事连夜更换了风扇。".to_owned(),
    );

    // ── 耗时：短 / 中 / 长三种长度 ────────────────────────────────────────
    push(
        "short",
        "perf",
        "今天下午妈妈打电话来，说院子里的桂花开了，香气飘了满屋。".to_owned(),
    );
    push(
        "medium",
        "perf",
        "下午开了两个小时的会，讨论下个季度的排期，中间休息的时候我去楼下买了一杯咖啡，\
         回来发现会议室被别的组占了，只好又换到隔壁的小会议室继续。"
            .to_owned(),
    );
    push("long", "perf", LONG_UNIT.repeat(10));

    // ── 排序一致性：5 个查询 × 5 个候选 ──────────────────────────────────
    for (key, text) in [
        ("q1", "家里的花开了"),
        ("q2", "电脑出故障了"),
        ("q3", "今天晚饭吃了什么"),
        ("q4", "周末和朋友去爬山"),
        ("q5", "最近工作压力有点大"),
    ] {
        push(key, "rank_query", text.to_owned());
    }
    for (key, text) in [
        ("d1", "妈妈说院子里的桂花开了，满屋都是香味。"),
        ("d2", "机房空调坏了，服务器温度过高一直告警。"),
        ("d3", "晚饭随便煮了碗番茄鸡蛋面。"),
        ("d4", "周六和小李去爬香山，累但是很开心。"),
        ("d5", "项目排期太紧，这两周一直睡不好。"),
    ] {
        push(key, "rank_doc", text.to_owned());
    }
    out
}

/// 越界文本（>512 token，超过 bge 的位置上限）：**单独、最后、可失败地**测。
/// `embedding.rs` 里没有截断逻辑，所以这条是真实的产品风险点，只如实记录结果。
///
/// 取 `LONG_UNIT`（42 字）× 15 ≈ 630 字；实测 tokenizer 给出 617 token（> 512）。
fn overlength_text() -> String {
    LONG_UNIT.repeat(15)
}

/// 每个文本的重复次数（取中位数当「热」值）。长文本少跑两次，省时间。
fn repeats_for(key: &str) -> usize {
    match key {
        "long" => 3,
        _ => 5,
    }
}

// ─────────────────────────── C ABI ───────────────────────────

/// 探针主入口：入参 JSON → 结果 JSON（UTF-8）。返回的指针必须用 [`diary_bge_free`] 释放。
#[no_mangle]
pub extern "C" fn diary_bge_run(params_json: *const c_char) -> *mut c_char {
    let raw = if params_json.is_null() {
        String::new()
    } else {
        // SAFETY: 调用方保证传进来的是 NUL 结尾的 UTF-8 C 字符串。
        unsafe { CStr::from_ptr(params_json) }
            .to_string_lossy()
            .into_owned()
    };

    let out = match catch_unwind(AssertUnwindSafe(|| {
        let params: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
        run(&params)
    })) {
        Ok(v) => v,
        Err(p) => {
            let msg = p
                .downcast_ref::<&str>()
                .map(|s| (*s).to_owned())
                .or_else(|| p.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "<无法取出 panic 文本>".to_owned());
            json!({"stage": "panic", "panic": msg, "params": raw})
        }
    };

    let text = serde_json::to_string(&out)
        .unwrap_or_else(|e| format!("{{\"stage\":\"error\",\"error\":\"json 序列化失败: {e}\"}}"));
    CString::new(text)
        .unwrap_or_else(|_| CString::new("{\"stage\":\"error\"}").unwrap())
        .into_raw()
}

/// 释放 [`diary_bge_run`] 返回的字符串。
///
/// # Safety
/// `p` 必须是 `diary_bge_run` 返回、且还没被释放过的指针（或空指针）。
#[no_mangle]
pub unsafe extern "C" fn diary_bge_free(p: *mut c_char) {
    if !p.is_null() {
        drop(CString::from_raw(p));
    }
}

/// ABI 版本戳（静态字符串，不需要释放）。
#[no_mangle]
pub extern "C" fn diary_bge_abi() -> *const c_char {
    static TAG: &[u8] = b"diary_bge_smoke/1 ohos-cdylib\0";
    let _ = ABI_TAG;
    TAG.as_ptr().cast::<c_char>()
}

// ─────────────────────────── 实现 ───────────────────────────

fn ms(d: std::time::Duration) -> f64 {
    (d.as_secs_f64() * 1000_000.0).round() / 1000.0
}

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

fn file_sha256(path: &Path) -> std::io::Result<(String, u64)> {
    let mut f = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let n = std::io::copy(&mut f, &mut hasher)?;
    Ok((format!("{:x}", hasher.finalize()), n))
}

/// f32 向量的 sha256（小端字节）——宿主与设备可以逐位比对的指纹。
fn vec_sha256(v: &[f32]) -> String {
    let mut h = Sha256::new();
    for x in v {
        h.update(x.to_le_bytes());
    }
    format!("{:x}", h.finalize())
}

/// 小端 f32 的十六进制串。**不损失精度**，宿主/设备对比就靠它
/// （JSON 十进制最短表示也能往返，但十六进制更不容易被中途中转改写）。
fn vec_hex(v: &[f32]) -> String {
    let mut bytes = Vec::with_capacity(v.len() * 4);
    for x in v {
        bytes.extend_from_slice(&x.to_le_bytes());
    }
    hex(&bytes)
}

fn norm(v: &[f32]) -> f32 {
    v.iter().map(|x| x * x).sum::<f32>().sqrt()
}

fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

/// 余弦相似度（向量已经 L2 归一化，内积即余弦；这里仍除以模长，保证对未归一化的输入也对）。
fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let na = norm(a);
    let nb = norm(b);
    if na <= 0.0 || nb <= 0.0 {
        return f32::NAN;
    }
    dot(a, b) / (na * nb)
}

fn median(mut xs: Vec<f64>) -> f64 {
    if xs.is_empty() {
        return f64::NAN;
    }
    xs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = xs.len();
    if n % 2 == 1 {
        xs[n / 2]
    } else {
        (xs[n / 2 - 1] + xs[n / 2]) / 2.0
    }
}

/// 从 `/proc/self/status` 读一个以 kB 为单位的字段（VmRSS / VmHWM / VmSize）。
/// 读不到就返回 `None`（如实报「没测到」，不猜）。
fn proc_kb(key: &str) -> Option<u64> {
    let text = std::fs::read_to_string("/proc/self/status").ok()?;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix(key) {
            let rest = rest.trim_start_matches(':').trim();
            let num = rest.split_whitespace().next()?;
            return num.parse::<u64>().ok();
        }
    }
    None
}

fn cpu_count() -> Option<usize> {
    std::thread::available_parallelism().ok().map(|n| n.get())
}

fn run(params: &Value) -> Value {
    let t_all = Instant::now();
    let mut r = Map::new();
    let mut errors: Vec<String> = Vec::new();

    let model_dir = params
        .get("model_dir")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    let ort_so = params
        .get("ort_so")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();

    r.insert("stage".into(), json!("ok"));
    r.insert("abi".into(), json!(ABI_TAG));
    r.insert("model_dir".into(), json!(model_dir));
    r.insert("ort_so".into(), json!(ort_so));
    r.insert("cpu_count".into(), json!(cpu_count()));
    r.insert("pooling".into(), json!("cls"));
    r.insert("query_instruction".into(), json!(false));

    let model_path = PathBuf::from(&model_dir).join("model_quantized.onnx");
    let tok_path = PathBuf::from(&model_dir).join("tokenizer.json");
    r.insert("model_path".into(), json!(model_path.display().to_string()));
    r.insert(
        "model_bytes".into(),
        json!(std::fs::metadata(&model_path).map(|m| m.len()).unwrap_or(0)),
    );
    r.insert(
        "tokenizer_bytes".into(),
        json!(std::fs::metadata(&tok_path).map(|m| m.len()).unwrap_or(0)),
    );
    let ort_bytes = std::fs::metadata(&ort_so).map(|m| m.len()).unwrap_or(0);
    r.insert("ort_so_bytes".into(), json!(ort_bytes));

    // ── 0. 语料指纹 + token 数（tokenizer 单独加载一次，只为数 token） ──────
    let texts = corpus();
    let mut ch = Sha256::new();
    for t in &texts {
        ch.update(t.key.as_bytes());
        ch.update([0u8]);
        ch.update(t.text.as_bytes());
        ch.update([0u8]);
    }
    r.insert("corpus_sha256".into(), json!(hex(&ch.finalize())));

    let t = Instant::now();
    let tokenizer = match tokenizers::Tokenizer::from_file(&tok_path) {
        Ok(tk) => {
            r.insert("tokenizer_load_ms".into(), json!(ms(t.elapsed())));
            Some(tk)
        }
        Err(e) => {
            r.insert("tokenizer_load_ms".into(), json!(ms(t.elapsed())));
            errors.push(format!("tokenizer 加载失败（{}）：{e}", tok_path.display()));
            None
        }
    };

    // ── 1. 文件读取 / 哈希（冷 I/O 的量级） ───────────────────────────────
    let t = Instant::now();
    let model_hash = match file_sha256(&model_path) {
        Ok((h, n)) => {
            r.insert("model_sha256".into(), json!(h));
            r.insert("model_read_bytes".into(), json!(n));
            r.insert("model_sha256_cold_ms".into(), json!(ms(t.elapsed())));
            Some(h)
        }
        Err(e) => {
            r.insert("model_sha256_cold_ms".into(), json!(ms(t.elapsed())));
            errors.push(format!("读模型失败（{}）：{e}", model_path.display()));
            None
        }
    };
    match file_sha256(&tok_path) {
        Ok((h, _)) => {
            r.insert("tokenizer_sha256".into(), json!(h));
        }
        Err(e) => errors.push(format!("读 tokenizer 失败：{e}")),
    }
    // 纯「把整个模型读进内存一次」的耗时（评审时用来把 I/O 与建会话分开）。
    let t = Instant::now();
    match std::fs::read(&model_path) {
        Ok(buf) => {
            r.insert("model_read_only_ms".into(), json!(ms(t.elapsed())));
            r.insert("model_read_only_bytes".into(), json!(buf.len()));
        }
        Err(e) => errors.push(format!("fs::read 模型失败：{e}")),
    }

    // ── token 数（长/越界文本是否超 512 就看这里） ────────────────────────
    if let Some(tk) = &tokenizer {
        let mut counts = Map::new();
        for t in &texts {
            match tk.encode(t.text.as_str(), true) {
                Ok(enc) => {
                    counts.insert(t.key.to_owned(), json!(enc.get_ids().len()));
                }
                Err(e) => errors.push(format!("分词失败（{}）：{e}", t.key)),
            }
        }
        match tk.encode(overlength_text().as_str(), true) {
            Ok(enc) => {
                counts.insert("overlength".to_owned(), json!(enc.get_ids().len()));
            }
            Err(e) => errors.push(format!("分词失败（overlength）：{e}")),
        }
        r.insert("token_counts".into(), Value::Object(counts));
    }

    r.insert("proc_rss_kb_before_load".into(), json!(proc_kb("VmRSS")));
    r.insert("proc_hwm_kb_before_load".into(), json!(proc_kb("VmHWM")));

    // ── 2. **产品路径加载**：diary_core::embedding::load（load-dynamic + sha256 + 建会话 + 预热） ──
    let cfg = EmbeddingConfig {
        model_dir: PathBuf::from(&model_dir),
        ort_dylib: PathBuf::from(&ort_so),
        model_file: None,
    };
    if let Err(reason) = cfg.check_files() {
        r.insert("stage".into(), json!("error"));
        errors.push(format!("配置自检失败：{reason}"));
        r.insert("errors".into(), json!(errors));
        return Value::Object(r);
    }
    let t = Instant::now();
    let loaded = catch_unwind(AssertUnwindSafe(|| {
        load(
            &cfg,
            EmbedOptions {
                pooling: Pooling::Cls,
                query_instruction: false,
            },
        )
    }));
    let load_ms = ms(t.elapsed());
    r.insert("load_ms".into(), json!(load_ms));
    r.insert("proc_rss_kb_after_load".into(), json!(proc_kb("VmRSS")));
    r.insert("proc_hwm_kb_after_load".into(), json!(proc_kb("VmHWM")));

    let embedder = match loaded {
        Ok(Ok(e)) => e,
        Ok(Err(reason)) => {
            r.insert("stage".into(), json!("error"));
            r.insert("load_ok".into(), json!(false));
            errors.push(format!("embedding::load 失败：{reason}"));
            r.insert("errors".into(), json!(errors));
            r.insert("total_ms".into(), json!(ms(t_all.elapsed())));
            return Value::Object(r);
        }
        Err(p) => {
            let msg = p
                .downcast_ref::<&str>()
                .map(|s| (*s).to_owned())
                .or_else(|| p.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "<无法取出 panic 文本>".to_owned());
            r.insert("stage".into(), json!("error"));
            r.insert("load_ok".into(), json!(false));
            r.insert("load_panicked".into(), json!(true));
            r.insert("load_panic".into(), json!(msg));
            r.insert("errors".into(), json!(errors));
            r.insert("total_ms".into(), json!(ms(t_all.elapsed())));
            return Value::Object(r);
        }
    };
    r.insert("load_ok".into(), json!(true));
    r.insert("dims".into(), json!(embedder.dims()));
    r.insert("model_version".into(), json!(embedder.model_version()));
    r.insert(
        "options".into(),
        json!({
            "pooling": embedder.options().pooling.wire(),
            "query_instruction": embedder.options().query_instruction,
        }),
    );

    // ── 3. 逐文本嵌入：冷（第一次真文本）+ 热（重复取中位数） ──────────────
    let mut vectors: Vec<(String, Vec<f32>)> = Vec::new();
    let mut embeds = Vec::new();
    let mut first_embed_ms: Option<f64> = None;

    for t in &texts {
        let t0 = Instant::now();
        let v = match embedder.embed_document(t.text.as_str()) {
            Ok(v) => v,
            Err(e) => {
                errors.push(format!("embed_document({}) 失败：{e}", t.key));
                continue;
            }
        };
        let cold = ms(t0.elapsed());
        if first_embed_ms.is_none() {
            first_embed_ms = Some(cold);
        }

        let reps = repeats_for(t.key);
        let mut hots = Vec::with_capacity(reps);
        let mut last = v.clone();
        let mut bit_identical = true;
        for _ in 0..reps {
            let t0 = Instant::now();
            match embedder.embed_document(t.text.as_str()) {
                Ok(x) => {
                    hots.push(ms(t0.elapsed()));
                    bit_identical &= x == v;
                    last = x;
                }
                Err(e) => {
                    errors.push(format!("embed_document({}) 热跑失败：{e}", t.key));
                    break;
                }
            }
        }
        if last != v {
            bit_identical = false;
        }

        let mut e = Map::new();
        e.insert("key".into(), json!(t.key));
        e.insert("kind".into(), json!(t.kind));
        e.insert("chars".into(), json!(t.text.chars().count()));
        e.insert("cold_ms".into(), json!(cold));
        e.insert("hot_ms_median".into(), json!(median(hots.clone())));
        e.insert(
            "hot_ms_min".into(),
            json!(hots.iter().cloned().fold(f64::INFINITY, f64::min)),
        );
        e.insert("hot_ms_all".into(), json!(hots));
        e.insert("vec_dims".into(), json!(v.len()));
        e.insert("vec_norm".into(), json!(norm(&v)));
        e.insert("vec_sha256".into(), json!(vec_sha256(&v)));
        e.insert("vec_hex".into(), json!(vec_hex(&v)));
        e.insert(
            "vec_head".into(),
            json!(v.iter().take(4).map(|x| *x as f64).collect::<Vec<_>>()),
        );
        e.insert("repeat_bit_identical".into(), json!(bit_identical));
        embeds.push(Value::Object(e));
        vectors.push((t.key.to_owned(), v));
    }
    r.insert("first_embed_ms".into(), json!(first_embed_ms));
    r.insert("embeds".into(), json!(embeds));

    let find = |key: &str| -> Option<&Vec<f32>> {
        vectors.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    };

    // ── 4. 语义判据（设备侧算，宿主侧同样算一遍） ────────────────────────
    let mut semantic = Map::new();
    let pair = |a: &str, b: &str| -> Option<f32> { Some(cosine(find(a)?, find(b)?)) };
    let near = pair("near_a", "near_b");
    let far = pair("near_a", "far");
    let main_near = pair("main", "near_a");
    let main_far = pair("main", "far");
    semantic.insert("near_a__near_b".into(), json!(near));
    semantic.insert("near_a__far".into(), json!(far));
    semantic.insert("main__near_a".into(), json!(main_near));
    semantic.insert("main__far".into(), json!(main_far));
    semantic.insert(
        "near_gt_far".into(),
        json!(matches!((near, far), (Some(a), Some(b)) if a > b)),
    );
    semantic.insert(
        "main_near_gt_main_far".into(),
        json!(matches!((main_near, main_far), (Some(a), Some(b)) if a > b)),
    );
    r.insert("semantic".into(), Value::Object(semantic));

    // ── 5. 排序一致性：5 查询 × 5 候选，设备侧算完整相似度矩阵与排序 ───────
    let queries: Vec<&ProbeText> = texts.iter().filter(|t| t.kind == "rank_query").collect();
    let docs: Vec<&ProbeText> = texts.iter().filter(|t| t.kind == "rank_doc").collect();
    let mut cos_matrix = Map::new();
    let mut order_out = Vec::new();
    for q in &queries {
        let mut row = Map::new();
        let mut scored: Vec<(String, f64)> = Vec::new();
        for d in &docs {
            let c = match (find(q.key), find(d.key)) {
                (Some(a), Some(b)) => cosine(a, b) as f64,
                _ => f64::NAN,
            };
            row.insert(d.key.to_owned(), json!(c));
            scored.push((d.key.to_owned(), c));
        }
        cos_matrix.insert(q.key.to_owned(), Value::Object(row));
        // 分数降序；分数相同按 key 升序，保证两边确定性一致。
        scored.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.0.cmp(&b.0))
        });
        order_out.push(json!({
            "query": q.key,
            "query_text": q.text,
            "order": scored.iter().map(|(k, _)| k.clone()).collect::<Vec<_>>(),
            "scores": scored.iter().map(|(k, v)| json!([k, v])).collect::<Vec<_>>(),
        }));
    }
    r.insert(
        "ranking".into(),
        json!({
            "cos": Value::Object(cos_matrix),
            "order": order_out,
        }),
    );

    r.insert("proc_rss_kb_after_embed".into(), json!(proc_kb("VmRSS")));
    r.insert("proc_hwm_kb_after_embed".into(), json!(proc_kb("VmHWM")));

    // ── 6. 越界文本（>512 token）：embedding.rs 没有截断逻辑，如实记录行为 ──
    let over = overlength_text();
    let over_res = catch_unwind(AssertUnwindSafe(|| embedder.embed_document(over.as_str())));
    match over_res {
        Ok(Ok(v)) => {
            r.insert(
                "overlength".into(),
                json!({
                    "ok": true,
                    "dims": v.len(),
                    "norm": norm(&v),
                    "note": "超 512 token 仍然返回了向量（是否语义有效未验证）",
                }),
            );
        }
        Ok(Err(e)) => {
            r.insert("overlength".into(), json!({"ok": false, "err": e.to_string()}));
        }
        Err(p) => {
            let msg = p
                .downcast_ref::<&str>()
                .map(|s| (*s).to_owned())
                .or_else(|| p.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "<无法取出 panic 文本>".to_owned());
            r.insert(
                "overlength".into(),
                json!({"ok": false, "panicked": true, "panic": msg}),
            );
        }
    }

    // ── 7. 建会话单项耗时（缓存已热；只用来把 load_ms 拆开看） ─────────────
    //     ⚠️ 这里**不 drop** 这个多余 session：见 embedding.rs 顶部注释（退出阶段释放会炸）。
    let t = Instant::now();
    match ort::session::Session::builder()
        .map_err(|e| e.to_string())
        .and_then(|mut b| b.commit_from_file(&model_path).map_err(|e| e.to_string()))
    {
        Ok(session) => {
            r.insert("session_create_only_ms".into(), json!(ms(t.elapsed())));
            std::mem::forget(session);
        }
        Err(e) => {
            r.insert("session_create_only_ms".into(), json!(ms(t.elapsed())));
            r.insert("session_create_only_err".into(), json!(e));
        }
    }
    r.insert("proc_rss_kb_end".into(), json!(proc_kb("VmRSS")));
    r.insert("proc_hwm_kb_end".into(), json!(proc_kb("VmHWM")));

    if let Some(h) = model_hash {
        r.insert(
            "model_sha256_matches_manifest".into(),
            json!(h == "d44978e451a0b4e0c561b8c6bb2cfaf5aac734f2729359aae46e1db893499f43"),
        );
    }

    r.insert("errors".into(), json!(errors));
    r.insert("total_ms".into(), json!(ms(t_all.elapsed())));
    Value::Object(r)
}

/// 宿主侧对照实验（**不是产品代码，只在 x86_64 宿主上跑**）：
///
/// ```bash
/// DIARY_BGE_HOST_ORT_SO=$HOME/dev/caches/host-ort/libonnxruntime.so \
/// DIARY_BGE_HOST_MODEL_DIR=$HOME/dev/caches/bge-small-zh-v1.5-ours \
///   cargo test --release -- --nocapture host_control
/// ```
///
/// 跑的是与设备**完全相同**的 `run()`（同一份语料常量、同一条 `diary_core::embedding::load`
/// 路径、同一个 int8 模型文件），输出 `HOST_CONTROL <JSON>` 供对比余弦/最大偏差/排序。
#[cfg(test)]
mod host_control {
    #[test]
    fn runs_the_same_sequence_on_host() {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/home/qianxia".to_owned());
        let ort = std::env::var("DIARY_BGE_HOST_ORT_SO")
            .unwrap_or_else(|_| format!("{home}/dev/caches/host-ort/libonnxruntime.so"));
        let model_dir = std::env::var("DIARY_BGE_HOST_MODEL_DIR")
            .unwrap_or_else(|_| format!("{home}/dev/caches/bge-small-zh-v1.5-ours"));
        let params = serde_json::json!({"ort_so": ort, "model_dir": model_dir});
        let v = super::run(&params);
        println!(
            "HOST_CONTROL {}",
            serde_json::to_string(&v).unwrap_or_default()
        );
        assert_eq!(v["stage"], "ok", "宿主对照必须成功：{v}");
        assert_eq!(v["load_ok"], true);
        assert_eq!(v["dims"], 512);
        assert_eq!(v["errors"], serde_json::json!([]));
    }
}
//! schema 版本与迁移。
//!
//! 契约第 1.3 节要求 CoreInfo 报告 dataSchemaVersion；任务书 3.4 节要求迁移
//! 带明确版本、失败时保留旧资料。这里用 `PRAGMA user_version` 记录版本，
//! 迁移本身放在事务里，失败就整体回退。

use rusqlite::{params, Connection};

/// 本构建支持的 schema 版本。
pub const SCHEMA_VERSION: i64 = 5;

/// 迁移到最新版本。已经是最新则什么都不做。
pub fn migrate(conn: &mut Connection) -> rusqlite::Result<()> {
    let current: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;

    if current > SCHEMA_VERSION {
        // 旧构建不能打开新库：宁可不打开，也不要用错误的假设写坏资料。
        return Err(rusqlite::Error::InvalidParameterName(format!(
            "资料库的 schemaVersion 是 {current}，高于本构建支持的 {SCHEMA_VERSION}"
        )));
    }
    if current == SCHEMA_VERSION {
        return Ok(());
    }

    let tx = conn.transaction()?;
    if current < 1 {
        tx.execute_batch(V1)?;
    }
    if current < 2 {
        tx.execute_batch(V2)?;
    }
    if current < 3 {
        tx.execute_batch(V3)?;
    }
    if current < 4 {
        tx.execute_batch(V4)?;
    }
    if current < 5 {
        tx.execute_batch(V5)?;
    }
    tx.execute_batch(&format!("PRAGMA user_version = {SCHEMA_VERSION};"))?;
    tx.execute(
        "INSERT INTO schema_migrations(version, applied_at) VALUES (?1, ?2)",
        params![SCHEMA_VERSION, crate::support::to_iso(crate::support::now())],
    )?;
    tx.commit()
}

/// v1：记录、原始内容、幂等回执、域事件。
///
/// 索引与派生内容（extracted_contents、searchable_chunks）不在 v1 里，
/// 它们随 B1b/B2 一起进来。
const V1: &str = r#"
CREATE TABLE schema_migrations (
    version    INTEGER PRIMARY KEY,
    applied_at TEXT NOT NULL
);

CREATE TABLE captures (
    id                 TEXT PRIMARY KEY,
    revision           INTEGER NOT NULL,
    state              TEXT NOT NULL,
    occurred_at        TEXT NOT NULL,
    created_at         TEXT NOT NULL,
    updated_at         TEXT NOT NULL,
    time_zone          TEXT NOT NULL,
    utc_offset_minutes INTEGER NOT NULL,
    day_key            TEXT NOT NULL,
    draft_text         TEXT NOT NULL DEFAULT ''
);
CREATE INDEX idx_captures_day ON captures(day_key, occurred_at DESC, id DESC);

CREATE TABLE source_items (
    source_id           TEXT PRIMARY KEY,
    capture_id          TEXT NOT NULL REFERENCES captures(id) ON DELETE CASCADE,
    kind                TEXT NOT NULL,
    position            INTEGER NOT NULL,
    current_revision_id TEXT
);
CREATE INDEX idx_source_items_capture ON source_items(capture_id, position);

CREATE TABLE source_revisions (
    revision_id        TEXT PRIMARY KEY,
    source_id          TEXT NOT NULL REFERENCES source_items(source_id) ON DELETE CASCADE,
    parent_revision_id TEXT,
    text               TEXT,
    asset_id           TEXT,
    author_type        TEXT NOT NULL,
    occurred_at        TEXT NOT NULL
);
CREATE INDEX idx_source_revisions_source ON source_revisions(source_id, occurred_at);

CREATE TABLE operation_receipts (
    operation_id         TEXT PRIMARY KEY,
    kind                 TEXT NOT NULL,
    request_fingerprint  TEXT NOT NULL,
    result_json          TEXT NOT NULL,
    created_at           TEXT NOT NULL
);

CREATE TABLE domain_events (
    sequence   INTEGER PRIMARY KEY AUTOINCREMENT,
    event_id   TEXT NOT NULL UNIQUE,
    type       TEXT NOT NULL,
    entity_id  TEXT NOT NULL,
    revision   INTEGER NOT NULL,
    emitted_at TEXT NOT NULL
);
CREATE INDEX idx_domain_events_entity ON domain_events(entity_id, sequence);
"#;
/// v2：原件文件库与导入会话。
///
/// `blobs` 是按 sha256 索引的内容存储，导入相同内容时复用同一条 blob；
/// `assets` 是用户的导入记录，指向 blob；`import_sessions` 是可恢复的导入日志，
/// 它在文件系统与数据库之间充当协调记录（两者不是一个事务）。
const V2: &str = r#"
CREATE TABLE blobs (
    sha256     TEXT PRIMARY KEY,
    object_ref TEXT NOT NULL UNIQUE,
    byte_size  INTEGER NOT NULL,
    created_at TEXT NOT NULL
);

CREATE TABLE assets (
    id              TEXT PRIMARY KEY,
    sha256          TEXT NOT NULL REFERENCES blobs(sha256),
    object_ref      TEXT NOT NULL,
    original_name   TEXT NOT NULL,
    detected_mime   TEXT NOT NULL,
    byte_size       INTEGER NOT NULL,
    storage_state   TEXT NOT NULL,
    import_origin   TEXT NOT NULL,
    created_at      TEXT NOT NULL,
    media_duration_ms INTEGER,
    width           INTEGER,
    height          INTEGER
);
CREATE INDEX idx_assets_sha ON assets(sha256);

CREATE TABLE import_sessions (
    id               TEXT PRIMARY KEY,
    capture_id       TEXT NOT NULL REFERENCES captures(id) ON DELETE CASCADE,
    asset_id         TEXT,
    display_name     TEXT NOT NULL,
    mime_hint        TEXT,
    size_hint        INTEGER,
    origin           TEXT NOT NULL,
    staging_rel_path TEXT NOT NULL,
    final_rel_path   TEXT,
    state            TEXT NOT NULL,
    copied_bytes     INTEGER NOT NULL DEFAULT 0,
    sha256           TEXT,
    error_code       TEXT,
    error_message    TEXT,
    created_at       TEXT NOT NULL,
    updated_at       TEXT NOT NULL
);
CREATE INDEX idx_import_sessions_state ON import_sessions(state, created_at);
"#;

/// v3：录音会话与持久任务队列。
///
/// `recording_segments` 用 (recording_id, segment_index) 做主键：重复登记同一序号
/// 天然只会有一条，冲突与否靠内容指纹判断。`jobs` 是持久队列，重启后仍在；
/// `job_attempts` 记录每一次尝试的结局，便于排查「为什么重试了五次」。
const V3: &str = r#"
CREATE TABLE recording_sessions (
    id                 TEXT PRIMARY KEY,
    capture_id         TEXT NOT NULL REFERENCES captures(id) ON DELETE CASCADE,
    asset_id           TEXT NOT NULL,
    staging_rel_path   TEXT NOT NULL,
    state              TEXT NOT NULL,
    started_at         TEXT NOT NULL,
    ended_at           TEXT,
    last_segment_index INTEGER NOT NULL DEFAULT -1,
    elapsed_ms         INTEGER NOT NULL DEFAULT 0,
    durable_through_ms INTEGER NOT NULL DEFAULT 0,
    end_reason         TEXT,
    error_code         TEXT,
    error_message      TEXT,
    updated_at         TEXT NOT NULL
);
CREATE INDEX idx_recording_sessions_state ON recording_sessions(state, started_at);

CREATE TABLE recording_segments (
    recording_id       TEXT NOT NULL REFERENCES recording_sessions(id) ON DELETE CASCADE,
    segment_index      INTEGER NOT NULL,
    segment_id         TEXT NOT NULL,
    relative_file_name TEXT NOT NULL,
    duration_ms        INTEGER NOT NULL,
    byte_size          INTEGER NOT NULL,
    sha256             TEXT NOT NULL,
    content_fingerprint TEXT NOT NULL,
    closed_at          TEXT NOT NULL,
    PRIMARY KEY (recording_id, segment_index)
);

CREATE TABLE jobs (
    id                  TEXT PRIMARY KEY,
    kind                TEXT NOT NULL,
    state               TEXT NOT NULL,
    priority            INTEGER NOT NULL,
    target_ids          TEXT NOT NULL,
    input_snapshot_hash TEXT,
    progress_completed  INTEGER,
    progress_total      INTEGER,
    attempt_count       INTEGER NOT NULL DEFAULT 0,
    max_attempts        INTEGER NOT NULL DEFAULT 5,
    next_attempt_at     TEXT,
    error_code          TEXT,
    requires_user_action INTEGER NOT NULL DEFAULT 0,
    attention_key       TEXT,
    created_at          TEXT NOT NULL,
    updated_at          TEXT NOT NULL
);
CREATE INDEX idx_jobs_due ON jobs(state, priority DESC, next_attempt_at, created_at);

CREATE TABLE job_attempts (
    job_id      TEXT NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
    attempt     INTEGER NOT NULL,
    started_at  TEXT NOT NULL,
    finished_at TEXT,
    outcome     TEXT,
    error_code  TEXT,
    message     TEXT,
    PRIMARY KEY (job_id, attempt)
);

CREATE TABLE attention_acks (
    attention_key   TEXT PRIMARY KEY,
    acknowledged_at TEXT NOT NULL
);
"#;

/// v4：派生内容（提取结果）与它的片段定位。
///
/// 派生内容是可重建的：重新提取先删旧记录再写新的，原件不受影响。
/// `(source_revision_id, extractor_id)` 唯一，避免同一个版本被同一个提取器重复写。
const V4: &str = r#"
CREATE TABLE extracted_contents (
    id                  TEXT PRIMARY KEY,
    source_id           TEXT NOT NULL,
    source_revision_id  TEXT NOT NULL,
    extractor_id        TEXT NOT NULL,
    extractor_version   TEXT NOT NULL,
    text                TEXT NOT NULL DEFAULT '',
    status              TEXT NOT NULL,
    coverage            TEXT NOT NULL,
    coverage_reason     TEXT,
    error_code          TEXT,
    created_at          TEXT NOT NULL,
    UNIQUE (source_revision_id, extractor_id)
);
CREATE INDEX idx_extracted_contents_source ON extracted_contents(source_id);
CREATE INDEX idx_extracted_contents_revision ON extracted_contents(source_revision_id);

CREATE TABLE extracted_segments (
    id                 TEXT PRIMARY KEY,
    content_id         TEXT NOT NULL REFERENCES extracted_contents(id) ON DELETE CASCADE,
    ordinal            INTEGER NOT NULL,
    text               TEXT NOT NULL,
    locator_type       TEXT NOT NULL,
    source_revision_id TEXT NOT NULL,
    text_start         INTEGER,
    text_end           INTEGER,
    start_ms           INTEGER,
    end_ms             INTEGER,
    page_number        INTEGER,
    block_id           TEXT,
    rect_left          REAL,
    rect_top           REAL,
    rect_right         REAL,
    rect_bottom        REAL,
    asset_id           TEXT
);
CREATE INDEX idx_extracted_segments_content ON extracted_segments(content_id, ordinal);
"#;

/// v5：关键词索引。
///
/// 索引是可重建数据，原件与派生内容都不在这里改。三张表的分工：
///
/// - `search_docs`：一行对一段派生文本，`doc_id` 是整数行号——索引表里存
///   36 字节的片段 UUID 会让每一行都胖一圈，M0 量出的 42 字节/源字符里
///   有一大块是这个。来源与内容 ID **不重复存**：它们是派生内容表里已有的
///   事实，检索时 join 回去即可，每行能省下几十字节（10 万片段上实测差 20 MiB）。
/// - `search_grams`：倒排表。同一个字段同时装三种词项，靠长度区分：
///   单字（1 字查询兜底）、2-gram（两字以上查询的候选生成）、jieba 词
///   （长度 ≥ 2 的词，用于候选收窄与后续排序区分「词命中」）。
///   **`PRIMARY KEY (term, doc_id)` + `WITHOUT ROWID`**：等于让主键本身当索引，
///   不再需要「堆表 + 二级索引」两份存储，天然去重。
///
/// 分词器版本存在每一行上：改了分词或 gram 策略就换版本号，旧行按版本重建，
/// 不会出现新旧词项混在一张表里却没人知道。
const V5: &str = r#"
CREATE TABLE search_docs (
    doc_id            INTEGER PRIMARY KEY,
    segment_id        TEXT NOT NULL UNIQUE REFERENCES extracted_segments(id) ON DELETE CASCADE,
    text_length       INTEGER NOT NULL,
    tokenizer_version TEXT NOT NULL
);

CREATE TABLE search_grams (
    term   TEXT NOT NULL,
    doc_id INTEGER NOT NULL REFERENCES search_docs(doc_id) ON DELETE CASCADE,
    PRIMARY KEY (term, doc_id)
) WITHOUT ROWID;
"#;

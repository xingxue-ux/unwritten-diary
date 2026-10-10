//! schema 版本与迁移。
//!
//! 契约第 1.3 节要求 CoreInfo 报告 dataSchemaVersion；任务书 3.4 节要求迁移
//! 带明确版本、失败时保留旧资料。这里用 `PRAGMA user_version` 记录版本，
//! 迁移本身放在事务里，失败就整体回退。

use rusqlite::{params, Connection};

/// 本构建支持的 schema 版本。
pub const SCHEMA_VERSION: i64 = 8;

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
    if current < 6 {
        tx.execute_batch(V6)?;
    }
    if current < 7 {
        tx.execute_batch(V7)?;
        // 老库里已有的文字要在这一步补进索引：不回填的话，用户升级完会发现
        // 「以前搜得到的话现在搜不到了」，要等一次手动重建才能修好。
        backfill_capture_index(&tx)?;
    }
    if current < 8 {
        tx.execute_batch(V8)?;
        // **不回填文本块**：块是派生数据，`extracted_contents` / `captures.draft_text`
        // 才是事实来源。升级时按整库重算一遍块（`rebuild_text_chunks`）是把一件重活
        // 塞进开库路径，而这一片还没有任何消费者需要历史块；块由重建入口按需产出即可。
        // 关键词索引的 v7 回填不能照搬这个理由：那边不回填会让「以前搜得到的话现在
        // 搜不到」，是用户直接看得见的退化；块对用户不可见。
    }
    tx.execute_batch(&format!("PRAGMA user_version = {SCHEMA_VERSION};"))?;
    tx.execute(
        "INSERT INTO schema_migrations(version, applied_at) VALUES (?1, ?2)",
        params![SCHEMA_VERSION, crate::support::to_iso(crate::support::now())],
    )?;
    tx.commit()
}

/// 把已有记录的文字补进 v6 的两张索引表。只在迁移里用一次。
///
/// 返回 `rusqlite::Result` 而不套 `CoreError`：迁移失败要整体回退，错误越贴近 SQL
/// 本身越好定位。分词不在这里重实现——直接用 `search::grams_for` 与
/// `search::TOKENIZER_VERSION`，否则迁移出来的词项会和增量索引对不上，
/// 表现为「老记录搜到的词与新记录不一样」。
fn backfill_capture_index(conn: &Connection) -> rusqlite::Result<()> {
    // 「有文字」用与增量索引同一套空白定义（`search::non_empty_text`）：SQLite 自带
    // 的 `trim` 只去 ASCII 空白，用它会把「只打了一个全角空格」的草稿也索引进来。
    let rows: Vec<(String, String)> = {
        let mut statement = conn.prepare(&format!(
            "SELECT id, draft_text FROM captures WHERE {}",
            crate::search::non_empty_text("draft_text")
        ))?;
        let rows = statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };

    for (capture_id, text) in &rows {
        conn.execute(
            "INSERT INTO search_capture_docs (capture_id, text_length, tokenizer_version) \
             VALUES (?1, ?2, ?3)",
            params![
                capture_id,
                text.chars().count() as i64,
                crate::search::TOKENIZER_VERSION,
            ],
        )?;
        let doc_id = conn.last_insert_rowid();
        let mut statement = conn.prepare(
            "INSERT OR IGNORE INTO search_capture_grams (term, doc_id) VALUES (?1, ?2)",
        )?;
        for gram in crate::search::grams_for(text) {
            statement.execute(params![gram, doc_id])?;
        }
    }
    Ok(())
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

/// v6：索引代次。
///
/// 检索会话要能判断「这次查询的快照还有效吗」。原先是拿 `search_docs` 的
/// 行数与 `doc_id` 之和当指纹，但 `index_segment` 是「先删后插」，而 `doc_id`
/// 是不带 `AUTOINCREMENT` 的 `INTEGER PRIMARY KEY`——SQLite 会把删掉的最大
/// rowid 再分配给下一条插入。于是**重建同一个片段时这两个值可以都不变**，
/// 哪怕正文与词项已经换了，旧会话也不会报 `search_expired`，会把新正文套进
/// 旧查询结果（审查发现的就是这一条）。
///
/// 所以改成显式的代次：任何一次索引写入都在同一个事务里把它 +1。它单调递增，
/// 与行数、id 分配策略、内容是否恰好相同都无关。
const V6: &str = r#"
CREATE TABLE search_index_epoch (
    id    INTEGER PRIMARY KEY CHECK (id = 1),
    epoch INTEGER NOT NULL
);

INSERT INTO search_index_epoch (id, epoch) VALUES (1, 0);
"#;

/// v7：用户自己写的文字（`captures.draft_text`）也进关键词索引。
///
/// 为什么不把 `search_docs.segment_id` 改成可空、两路共用一张文档表：它是
/// `NOT NULL` 加外键，SQLite 改不了列约束，只能整表重建；而 `DROP TABLE search_docs`
/// 会按 `ON DELETE CASCADE` 把 `search_grams` 的词项全部带走，现有索引当场清空。
/// 纯增量加两张对称的表，老库的派生内容索引一行都不用动，升级路径只是加法。
///
/// 两张表与 v5 的 `search_docs` / `search_grams` 完全同构：一张文档行表 +
/// 一张 `WITHOUT ROWID` 的倒排表，词项同样靠 `(term, doc_id)` 主键去重，
/// `tokenizer_version` 同样存在每一行上。所以「重建」「词项生成」「摘录高亮」
/// 都是共用逻辑，不是两套。
///
/// 两张表的 `doc_id` 各自从 1 开始、会撞号，所以检索侧用 `search::DocRef`
/// 记住一条引用来自哪张表，不靠负数或偏移这类约定。
const V7: &str = r#"
CREATE TABLE search_capture_docs (
    doc_id            INTEGER PRIMARY KEY,
    capture_id        TEXT NOT NULL UNIQUE REFERENCES captures(id) ON DELETE CASCADE,
    text_length       INTEGER NOT NULL,
    tokenizer_version TEXT NOT NULL
);

CREATE TABLE search_capture_grams (
    term   TEXT NOT NULL,
    doc_id INTEGER NOT NULL REFERENCES search_capture_docs(doc_id) ON DELETE CASCADE,
    PRIMARY KEY (term, doc_id)
) WITHOUT ROWID;
"#;

/// v8：文本块与向量的存储子结构、换代元数据。
///
/// 四张表的分工（**消费契约见 `docs/architecture/m2-向量索引与换代.md`**，别处不要
/// 另立一套解释）：
///
/// - `text_chunks`：一行一块。块是**检索的载体**，不是用户看到的东西：用户看到的
///   永远以「篇」（一个来源 / 一条记录自己的文字）为单位，块只是为了算向量才切出来的。
///   所以块上**没有 `capture_id`**：一块可能由同一天、**不同记录**的几篇拼成
///   （B3c-1 的打包规则），「这一块属于哪条记录」本身不成立。块属于哪几篇由
///   `chunk_spans` 回答。
/// - `chunk_spans`：一块里的每一段（一段就是一篇）。这里才是**权威值**所在：
///   `capture_id` / `kind` / `coverage` / `start_char` / `end_char` 都是这一篇自己的。
///   `start_char` / `end_char` 是**篇内**字符区间（排他），不是块内偏移——检索结果按篇
///   上报时，摘录要用该篇自己那一段，而不是整块拼接文本（整块里混着别的篇）。
///   不做 JSON 而是单独一张表：`sourceScope` 过滤要能走索引。
/// - `chunk_vectors`：向量按**代次**分开存。换代时先写新代次，切过去之后再清理旧的，
///   所以「正在构建的那一代」和「正在服务的这一代」可以同时存在。`storage` 现在只有
///   `f32`，留着 `i8` 给后面的量化。
/// - `index_meta`：单行元数据。`active_generation` 是正在服务检索的代次，
///   `building_generation` 是正在算的那一代；两个都为 NULL 表示还没有任何一代向量。
///
/// `text_chunks.kind` / `text_chunks.coverage` 是**便于块级过滤的聚合值**，不是权威值：
/// 块的 `kind` 取第一段的 `kind`，块的 `coverage` 取各段里**最弱**的那个
/// （`unavailable` < `metadata_only` < `partial` < `complete`）。要判断某一篇是什么、
/// 覆盖到什么程度，一律看 `chunk_spans` 上那一行。
///
/// 派生关系是**级联删除**：删块带走它的 span 与向量（`chunk_vectors` 按 `chunk_id`
/// 级联），因为向量一旦没有块就没有意思。重建块的入口先删后写，所以重建会让旧向量
/// 一起消失——这正是「换代」要做的事：向量要跟着当前这一代块重算。
///
/// 这一片（B3c-2 前半）只建结构、只产块：没有模型、没有向量，`chunk_vectors` 与
/// `index_meta` 会一直是空的 / 全 NULL。
const V8: &str = r#"
CREATE TABLE text_chunks (
    chunk_id        TEXT PRIMARY KEY,
    ordinal         INTEGER NOT NULL,
    text            TEXT NOT NULL,
    kind            TEXT NOT NULL,
    coverage        TEXT NOT NULL,
    day_key         TEXT NOT NULL,
    chunker_version TEXT NOT NULL,
    created_at      TEXT NOT NULL
);
CREATE INDEX idx_text_chunks_day ON text_chunks(day_key, ordinal);

CREATE TABLE chunk_spans (
    chunk_id           TEXT NOT NULL REFERENCES text_chunks(chunk_id) ON DELETE CASCADE,
    ordinal            INTEGER NOT NULL,
    capture_id         TEXT NOT NULL REFERENCES captures(id) ON DELETE CASCADE,
    source_id          TEXT NOT NULL,
    source_revision_id TEXT NOT NULL,
    start_char         INTEGER NOT NULL,
    end_char           INTEGER NOT NULL,
    kind               TEXT NOT NULL,
    coverage           TEXT NOT NULL,
    PRIMARY KEY (chunk_id, ordinal)
);
CREATE INDEX idx_chunk_spans_source ON chunk_spans(source_id);
CREATE INDEX idx_chunk_spans_capture ON chunk_spans(capture_id);
CREATE INDEX idx_chunk_spans_revision ON chunk_spans(source_revision_id);

CREATE TABLE chunk_vectors (
    chunk_id      TEXT NOT NULL REFERENCES text_chunks(chunk_id) ON DELETE CASCADE,
    generation    INTEGER NOT NULL,
    model_version TEXT NOT NULL,
    dims          INTEGER NOT NULL,
    storage       TEXT NOT NULL,
    vector        BLOB NOT NULL,
    created_at    TEXT NOT NULL,
    PRIMARY KEY (chunk_id, generation)
);

CREATE TABLE index_meta (
    id                  INTEGER PRIMARY KEY CHECK (id = 1),
    active_generation   INTEGER,
    building_generation INTEGER
);
INSERT INTO index_meta (id, active_generation, building_generation) VALUES (1, NULL, NULL);
"#;

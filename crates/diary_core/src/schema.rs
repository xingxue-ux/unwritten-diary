//! schema 版本与迁移。
//!
//! 契约第 1.3 节要求 CoreInfo 报告 dataSchemaVersion；任务书 3.4 节要求迁移
//! 带明确版本、失败时保留旧资料。这里用 `PRAGMA user_version` 记录版本，
//! 迁移本身放在事务里，失败就整体回退。

use rusqlite::{params, Connection};

/// 本构建支持的 schema 版本。
///
/// 13 是两个来源合并后的结果：**11** 是 B4e（整理与自动化，#92）的段号——
/// `organize_settings` + `diary_model_calls`；**12** 是 B4c（#101）的段号——
/// `diary_days.revision`。两条分支各自从 10 抬号，合并后的这个构建取 **13**，
/// 迁移里 v11 与 v12 两段都在（先建表、再加列）。
///
/// 两段的守卫都**不只看版本号，还看东西在不在**（见 `migrate`）：只跑过 B4c 分支的库
/// `user_version` 已经是 12、但没有 v11 的两张表；只跑过 B4e 分支的库是 11、没有
/// `revision` 列。光靠 `current < 11` / `current < 12` 会漏掉前者，所以 v11 的守卫是
/// 「版本号小于 11 **或**表不在」，v12 是「版本号小于 12 **或**列不在」。
///
/// 14 是 B6（插件 SDK 与宿主，#88）抬的号：只加 `plugin_installations` /
/// `plugin_grants` 两张全新的表，**不回填**任何旧数据（升级前没有插件，也没有
/// 「替用户装一个」这种默认）。
pub const SCHEMA_VERSION: i64 = 14;

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
    if current < 9 {
        tx.execute_batch(V9)?;
        // **不回填、不迁移任何旧数据**：v9 只加两张全新的表（远端服务配置与主动回应）。
        // 老库升级前没有任何 Provider 配置，也就没有东西可搬：迁移后 `providers` 为空，
        // 核心因此仍然「一个服务都没配」——默认不联网的口径在升级路径上同样成立。
        // 更不能在这里替用户猜一个服务填进去。
    }
    if current < 10 {
        tx.execute_batch(V10)?;
        // **不回填、不迁移任何旧数据**：v10 只加四张全新的表（日记快照、版本、当日状态、
        // 生成记录）。老库里没有任何日记版本——B4b 之前核心根本没有日记表。也**不能**
        // 在迁移里替用户把旧记录「补生成」一版：那会凭空造出用户没有确认过的正文，
        // 与「AI 正文不冒充用户手写」直接冲突。
    }
    // ---- 11 与 12：两条分支各自抬的号，合并后这个构建是 13 --------------------
    //
    // 顺序固定：先 v11（建「整理设置」与「调用账本」两张表），再 v12（给 `diary_days`
    // 加 `revision` 列）。两段都**不只看版本号，还看东西在不在**，因为两条分支的库
    // 可能处在四种状态：10（都没有）、11（有表、没列）、12（有列、没表）、12（都有）。
    //
    // - 10 → 两段都跑；
    // - 11（只跑过 B4e）→ v11 跳过（表在），v12 跑；
    // - **12 但没有 v11 的两张表**（只跑过 B4c）→ `current < 11` 是假，光靠版本号会把
    //   v11 整段跳过，升级后一碰整理设置就报「no such table」。所以 v11 的守卫是
    //   「版本号小于 11 **或** `organize_settings` 不在」。V11 的建表写成
    //   `IF NOT EXISTS`、种默认值那一步本来就是 `INSERT OR IGNORE`，所以重复跑幂等：
    //   已有设置一个字节都不会被改（**默认关**的口径对升级路径同样成立）；
    // - v12 同理：列不在就补，列已经在就整段跳过——**不重复回填**，否则会把已经推进过的
    //   `revision` 按版本数改小，反而弄丢冲突保护。
    if current < 11 || !table_exists(&tx, "organize_settings")? {
        tx.execute_batch(V11)?;
        // v11 只加两张全新的表（整理设置、调用账本），并**种一行默认设置**。
        // 种下去的那一行是 `enabled = 0`：从 v10 升上来的库与刚建的新库一样，
        // 自动整理**默认关**，核心不会自己去调用任何服务。这里没有、也不能有
        // 「替用户开启」的逻辑——开启是用户显式做的事（`settings.update` 那一路）。
        crate::organize::seed_default_settings(&tx, crate::support::now())
            .map_err(|error| rusqlite::Error::InvalidParameterName(error.to_string()))?;
    }
    if current < 12 || !column_exists(&tx, "diary_days", "revision")? {
        tx.execute_batch(V12)?;
        // **回填成「这一天的版本数」**：升级前调用方从 `diary.getDay` 能算出来的就是
        // 这个数字（`candidateVersionIds.length + (currentVersionId == null ? 0 : 1)`），
        // 所以老库升级不会让任何在飞的 `expectedRevision` 突然对不上。升级之后的每一次
        // 状态推进（新增一版、`setCurrent` 换指针）再从这个数往上加。
        // 这一天一条 `diary_days` 行都没有时不用管：`day_revision` 对没有行的天返回 0，
        // 而没有行也就没有版本可切。
        tx.execute_batch(V12_BACKFILL)?;
    }
    if current < 13 || !table_exists(&tx, "plugin_installations")? || !table_exists(&tx, "plugin_grants")? {
        tx.execute_batch(V13)?;
        // **不回填、不迁移任何旧数据**：v13 只加两张全新的插件表。升级前没有插件，
        // 也就没有东西可搬；更不能在迁移里替用户预设一个插件或一条授权。迁移后
        // `plugin_installations` 为空，和刚建的新库一样。
    }
    tx.execute_batch(&format!("PRAGMA user_version = {SCHEMA_VERSION};"))?;
    tx.execute(
        "INSERT INTO schema_migrations(version, applied_at) VALUES (?1, ?2)",
        params![SCHEMA_VERSION, crate::support::to_iso(crate::support::now())],
    )?;
    tx.commit()
}

/// `sqlite_master` 里有没有这张表（迁移的结构性守卫用；`name` 都是代码里的常量）。
fn table_exists(conn: &Connection, name: &str) -> rusqlite::Result<bool> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
        params![name],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

/// 表里有没有这一列（迁移的结构性守卫用）。
///
/// `PRAGMA table_info` 的表名不能参数化，所以用 `format!` 拼进来——**调用方只传代码里的
/// 常量**（现在只有 `"diary_days"`），不接受外部输入，没有注入面。
fn column_exists(conn: &Connection, table: &str, column: &str) -> rusqlite::Result<bool> {
    let mut statement = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        let name: String = row.get(1)?;
        if name == column {
            return Ok(true);
        }
    }
    Ok(false)
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

/// v9：远端服务（Provider）配置与主动回应。
///
/// 消费契约见 `docs/architecture/m3-Provider-配置与能力.md`，别处不要另立一套解释。
///
/// - `providers`：**脱敏配置**。列名与契约 2.7 的 Provider 字段一一对应
///   （`adapter_id` ↔ `adapterId`、`endpoint` ↔ `endpoint`）。**密钥不在这张表里**：
///   `credential_ref` 只是 `SecretStore` 的引用，明文由独立存储托管（见 m3 文档
///   「密钥存放与它的妥协」）。`declared_capabilities` 是用户/配置声明的能力，
///   `verified_capabilities` 是**连接测试真的验过**的能力，两个都存 wire 名的 JSON
///   数组——存 JSON 而不是关联表，是因为它们永远整组读写、不需要按能力做 SQL 过滤，
///   为它建第二张表只会多一条 join 与一处不一致的机会。
/// - `ai_responses`：`responses.request` 的请求与结果。**一行两态**：`pending`
///   是「用户请求了、还没生成」，`ready` 是正文已落库。不拆成「请求表 + 结果表」：
///   契约的 `responses.get(responseId)` 只有一个 id，两态一行最省事，也不会出现
///   「有结果没有请求」的悬空行。B4b 接手提示词与日记语义，这里只存原文与来源。
///
/// 两张表都**没有回填**：升级前不存在这些配置，替用户猜一个服务填进去是错的。
const V9: &str = r#"
CREATE TABLE providers (
    id                    TEXT PRIMARY KEY,
    label                 TEXT NOT NULL,
    adapter_id            TEXT NOT NULL,
    endpoint              TEXT,
    model                 TEXT,
    declared_capabilities TEXT NOT NULL DEFAULT '[]',
    verified_capabilities TEXT NOT NULL DEFAULT '[]',
    credential_ref        TEXT,
    enabled               INTEGER NOT NULL DEFAULT 0,
    test_status           TEXT NOT NULL DEFAULT 'untested',
    last_tested_at        TEXT,
    last_error_code       TEXT,
    created_at            TEXT NOT NULL,
    updated_at            TEXT NOT NULL
);
CREATE INDEX idx_providers_enabled ON providers(enabled, created_at, id);

CREATE TABLE ai_responses (
    id           TEXT PRIMARY KEY,
    request_text TEXT NOT NULL,
    capture_ids  TEXT NOT NULL DEFAULT '[]',
    provider_id  TEXT,
    model        TEXT,
    body         TEXT NOT NULL DEFAULT '',
    state        TEXT NOT NULL,
    error_code   TEXT,
    created_at   TEXT NOT NULL,
    updated_at   TEXT NOT NULL
);
CREATE INDEX idx_ai_responses_created ON ai_responses(created_at DESC, id DESC);
"#;

/// v10：日记输入快照、日记版本、当日状态与生成记录（B4b）。
///
/// 消费契约见 `docs/architecture/m3-日记生成与版本.md`，别处不要另立一套解释。
///
/// - `diary_source_snapshots`：**冻结下来的输入清单**。`body` 是规范化的 JSON
///   （当天用了哪些材料、各自的来源与修订、每个提取版本、覆盖范围、风格与提示词版本、
///   生成选项），`snapshot_hash` 是对**同一份规范形**算出的 sha256。行是不可变的：
///   材料后来变了也不会回头改它，所以「这一版是根据什么生成的」永远答得出来。
///   `(day_key, snapshot_hash)` 唯一：同一份输入重复冻结只留一行，`snapshot_id` 因此稳定。
///   `provider_id` / `model` 是**冻结时选中的执行者**，只进 `body` 之外的这两列（审计用），
///   **不进哈希**——换一个服务不该被当成「换了输入」。
/// - `diary_versions`：正文与来源分开存。`origin` 是 `generated` / `user_edit` / `merge`；
///   **AI 正文永远是 `generated`**，绝不写成用户来源。候选/当前**不在这张表上**：
///   契约的 `DiaryVersion` 没有状态字段，状态由 `diary_days.current_version_id` 表达。
/// - `diary_days`：一天的当前版本与时间语义。一行只在真有版本或真读过这一天时创建。
///   当前版本为空 = 还没有任何版本被选为当前。
/// - `diary_generations`：一次生成请求的业务行（任务的目标就是它）。它记着快照、
///   用户点名的服务、原因、operationId、两步走的实际步数与最终版本。
///   `operationId` 的幂等由 `operation_receipts` 收口（同一操作重试返回原票据），
///   这张表负责「同一份快照不重复产版本」与「任务 → 快照 → 版本」的追溯。
const V10: &str = r#"
CREATE TABLE diary_source_snapshots (
    id                  TEXT PRIMARY KEY,
    day_key             TEXT NOT NULL,
    snapshot_hash       TEXT NOT NULL,
    body                TEXT NOT NULL,
    prompt_version      TEXT NOT NULL,
    provider_id         TEXT,
    model               TEXT,
    generation_options  TEXT NOT NULL DEFAULT '{}',
    covered_sources     INTEGER NOT NULL DEFAULT 0,
    unprocessed_sources INTEGER NOT NULL DEFAULT 0,
    created_at          TEXT NOT NULL,
    UNIQUE (day_key, snapshot_hash)
);
CREATE INDEX idx_diary_snapshots_day ON diary_source_snapshots(day_key, created_at DESC, id DESC);

CREATE TABLE diary_versions (
    id                 TEXT PRIMARY KEY,
    day_key            TEXT NOT NULL,
    parent_version_id  TEXT,
    origin             TEXT NOT NULL,
    body               TEXT NOT NULL,
    source_snapshot_id TEXT REFERENCES diary_source_snapshots(id),
    style_snapshot_id  TEXT,
    generation_info    TEXT,
    provenance_links   TEXT NOT NULL DEFAULT '[]',
    operation_id       TEXT,
    created_at         TEXT NOT NULL
);
CREATE INDEX idx_diary_versions_day ON diary_versions(day_key, created_at DESC, id DESC);

CREATE TABLE diary_days (
    day_key            TEXT PRIMARY KEY,
    time_zone          TEXT NOT NULL,
    current_version_id TEXT REFERENCES diary_versions(id),
    updated_at         TEXT NOT NULL
);

CREATE TABLE diary_generations (
    id          TEXT PRIMARY KEY,
    day_key     TEXT NOT NULL,
    style_id    TEXT,
    reason      TEXT NOT NULL,
    operation_id TEXT NOT NULL,
    snapshot_id TEXT NOT NULL REFERENCES diary_source_snapshots(id),
    provider_id TEXT,
    job_id      TEXT,
    version_id  TEXT,
    state       TEXT NOT NULL,
    error_code  TEXT,
    steps       INTEGER NOT NULL DEFAULT 0,
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL
);
CREATE INDEX idx_diary_generations_snapshot ON diary_generations(snapshot_id, created_at, id);
CREATE INDEX idx_diary_generations_day ON diary_generations(day_key, created_at DESC, id DESC);
"#;

/// v11：自动整理的开关、模板、上限与调用账本（B4e，issue #92）。
///
/// 消费契约见 `docs/architecture/m4-整理与自动化.md`，别处不要另立一套解释。
///
/// 为什么必须动 schema（而不是塞进某张已有的表）：这三样东西都是**要跨重启活下来的
/// 用户配置与用量事实**，而 v1–v10 里没有一张键值配置表，也没有一张「真的发出去了
/// 几次调用」的账本。
///
/// - `organize_settings`：**单行**（`CHECK (id = 1)`，与 `index_meta` /
///   `search_index_epoch` 同一个写法）。`enabled` 默认 0——**默认关**是这一条的硬要求：
///   没有用户显式开启，核心绝不自动调用 Provider。`revision` 是乐观锁（同契约
///   `settings.update` 的 `expectedRevision`）。`template_params` 存 JSON 对象，
///   值只有整数与布尔（**不接受自由文本**，见 `OrganizeTemplateParam` 的文档）。
/// - `diary_model_calls`：**每一次真的发出去的**日记模型调用一行（成功与失败都记）。
///   配额与失败率只认这张表：估算、意图、任务条数都不算「花掉的调用」。`reason` 让
///   自动整理（`scheduled` / `automatic`）与用户主动要一版（`manual` / `regenerate`）
///   的用量分开算——用户自己点的生成不该吃掉自动整理的额度。
///
/// 两张表在迁移里都**不回填旧数据**：升级前没有这些配置与账本，替用户猜一个开关或
/// 编几条用量记录都是错的。默认设置那一行由 `organize::seed_default_settings` 写入，
/// 值只有一个来源（`organize.rs` 的常量），不在 SQL 里再抄一遍。
///
/// **建表写成 `IF NOT EXISTS`（#111 合并后改）**：合并 B4c（schema 12）之后，v11 的守卫
/// 多了一条「表不在就补」的结构性兜底（见 `migrate`），那一条会对着「user_version 已经
/// 是 12、但没跑过 v11」的库再执行一次这段 SQL。`IF NOT EXISTS` 保证它幂等——表已经
/// 在的库（正常从 11 升上来的）不会被改坏，缺表的库能被补上。
const V11: &str = r#"
CREATE TABLE IF NOT EXISTS organize_settings (
    id                        INTEGER PRIMARY KEY CHECK (id = 1),
    enabled                   INTEGER NOT NULL DEFAULT 0,
    template_id               TEXT NOT NULL,
    template_params           TEXT NOT NULL DEFAULT '{}',
    max_materials             INTEGER NOT NULL,
    max_chars                 INTEGER NOT NULL,
    max_calls_per_run         INTEGER NOT NULL,
    max_calls_per_day         INTEGER NOT NULL,
    max_failure_rate          REAL NOT NULL,
    auto_organize_time        TEXT NOT NULL,
    supplement_on_new_materials INTEGER NOT NULL DEFAULT 1,
    revision                  INTEGER NOT NULL DEFAULT 1,
    updated_at                TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS diary_model_calls (
    id          TEXT PRIMARY KEY,
    run_id      TEXT,
    day_key     TEXT,
    reason      TEXT NOT NULL,
    provider_id TEXT,
    model       TEXT,
    outcome     TEXT NOT NULL,
    error_code  TEXT,
    called_at   TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_diary_model_calls_time ON diary_model_calls(called_at, id);
CREATE INDEX IF NOT EXISTS idx_diary_model_calls_run ON diary_model_calls(run_id, called_at);
"#;

/// v12：`diary_days` 加一列**持久、单调递增**的 `revision`（#101）。
///
/// 为什么必须有这一列：`diary.setCurrent` 的契约承诺 `expectedRevision` 冲突保护，
/// 但「这一天的 revision」以前是「版本数」——换当前版本不增加版本数，所以两个窗口
/// 拿着同一个 `expectedRevision` 都能切成功，后一次会**静默覆盖**前一次的选择：
/// 那是假的冲突保护。指针变更本身也是一次状态推进，必须有自己的持久计数。
///
/// `DEFAULT 0` 只服务于「这一天还没有任何推进」，实际取值由迁移回填与写路径维护。
const V12: &str = r#"
ALTER TABLE diary_days ADD COLUMN revision INTEGER NOT NULL DEFAULT 0;
"#;

/// v12 的回填：把老库的 `revision` 从「这一天的版本数」起步（升级前调用方算的就是它）。
const V12_BACKFILL: &str = r#"
UPDATE diary_days SET revision = (
    SELECT COUNT(*) FROM diary_versions WHERE diary_versions.day_key = diary_days.day_key
);
"#;

/// v13：插件安装缓存与授权（B6，#88）。
///
/// 为什么是两张**全新的表**而不是往现有表里塞列：
/// - `plugin_installations` 是**缓存清单**。应用启动只读它（`manifest_json` /
///   `templates_json` / `enabled` / 授权），**不读、不编译、不执行**插件入口文件——
///   这是任务书 §8.2 的硬要求，也是「启动不执行插件」可验证的落点。
///   `install_rel_path` 指向 `plugins/<id>/<version>-<hash8>`（相对资料库根目录）；
///   内存库没有根目录，安装会明确报 `invalid_state`。
///   `state_json` 是插件自己的私有状态（有 16 KiB 配额），`last_run_json` 是最近一次
///   运行的审计摘要（有大小上限），两者都不参与「清单是否可信」的判断。
/// - `plugin_grants` 是**用户显式授予**的权限，与清单声明分开存：清单说「我想读材料」
///   不等于用户说「你可以」。更新插件时新增权限必须重新出现在 `grants` 里，否则安装
///   被拒——不默默扩大权限。
///
/// 两张表都**没有回填**：升级前不存在插件，替用户猜一个装上去是错的。外键级联删除
/// 只作用于授权行；卸载插件不会删除已经产生的日记或导出物（那是用户的资料）。
const V13: &str = r#"
CREATE TABLE IF NOT EXISTS plugin_installations (
    id               TEXT PRIMARY KEY,
    version          TEXT NOT NULL,
    manifest_version INTEGER NOT NULL,
    manifest_json    TEXT NOT NULL,
    templates_json   TEXT NOT NULL DEFAULT '[]',
    entry_kind       TEXT NOT NULL,
    entry_file       TEXT NOT NULL,
    package_hash     TEXT NOT NULL,
    install_rel_path TEXT NOT NULL,
    enabled          INTEGER NOT NULL DEFAULT 1,
    state_json       TEXT NOT NULL DEFAULT '{}',
    last_run_at      TEXT,
    last_run_json    TEXT,
    last_error       TEXT,
    installed_at     TEXT NOT NULL,
    updated_at       TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_plugin_installations_enabled ON plugin_installations(enabled, installed_at, id);

CREATE TABLE IF NOT EXISTS plugin_grants (
    plugin_id  TEXT NOT NULL REFERENCES plugin_installations(id) ON DELETE CASCADE,
    permission TEXT NOT NULL,
    granted_at TEXT NOT NULL,
    PRIMARY KEY (plugin_id, permission)
);
"#;

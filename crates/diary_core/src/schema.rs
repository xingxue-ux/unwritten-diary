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
/// `revision` 列。光靠 `current < 11` 会漏掉前者，所以 v11 的守卫是「版本号小于 11
/// **或**表不在」（V11 的 DDL 全是 `IF NOT EXISTS`、种子是 `INSERT OR IGNORE`，
/// 重复执行幂等，`current` 允许在这里只决定「要不要顺带跑一遍可重复的建表与种子」）；
/// v12 的 `ALTER` **不可重复执行**，守卫是**纯结构判定**「列不在」，不带 `current`。
///
/// **v11 之后守卫的规则**：`current` 只能决定「要不要跑**可重复**的部分」；不可重复
/// 执行的 DDL（`ALTER TABLE ... ADD COLUMN`）一律只看结构，不看版本号。版本号不是
/// 结构证明——外部改过 `user_version`、文件级撕裂复制都会留下「版本号落后、结构已经
/// 在」的库（见 `docs/development/升级与迁移.md`）。这种库上带 `current` 的 OR / AND
/// 守卫只要在「结构已在」那一半上再发一次 `ALTER`，open 就报重复列；事务回滚后版本号
/// 没写上去，**下一次 open 还走同一条路**：库永久打不开。
///
/// 14 是 B6（插件 SDK 与宿主，#88）抬的号：只加 `plugin_installations` /
/// `plugin_grants` 两张全新的表，**不回填**任何旧数据（升级前没有插件，也没有
/// 「替用户装一个」这种默认）。
///
/// 15 是 #128 复审（B5f 大库）抬的号：给 `index_meta` 加一列
/// `auto_rebuild_marker`（`open` 自动重建「同一口径最多自动尝试两次」的持久标记），
/// 并给 `chunk_vectors` 加一条**整代口径覆盖索引**（让 `open` 的整代口径核对从
/// 全表扫变成覆盖索引扫描）。两件都是**加性**的：标记默认 NULL（= 从未自动尝试），
/// 索引由 SQLite 从既有行建出来；用户的资料一行都不动，也不回填任何业务数据。
///
/// 为什么标记必须占 schema 版本、而不是搭 `jobs` 的既有字段：那个「同一口径只试一次」
/// 的标记要**跨重启、按库**存在，而 `jobs` 的业务键
/// `(kind, target_ids, input_snapshot_hash)` 正是去重键——把标记塞进
/// `input_snapshot_hash` 会让同一个库按口径碎成好几条「活着」的任务，正好破坏
/// 「至多一条活着的整代重建」。v1–v14 也没有一张键值表可借用；`index_meta` 是
/// 单行的索引元数据，这一列与它同一个主题，所以升版本、写迁移是诚实的代价。
///
/// **16 是回收站（B7，#155）抬的号**：给 `captures` 加一列 `trashed_from_state`，
/// 记住移入回收站之前是 `draft` 还是 `committed`。契约 3.1 节的状态机写的是「恢复回
/// 原状态」，而 v1–v15 的 `captures` 没有第二处能存这个事实：`state` 已经被 `trashed`
/// 占用，派生内容（有没有 source / 有没有正文）都不能可靠反推（空白草稿提交后是
/// `committed`、也可以没有任何来源）。所以这是**必要的加性迁移**：只加一列，
/// 老数据留 NULL（升级前没有回收站入口，NULL 在恢复时按 `committed` 的解释处理，
/// 见 `recycle::restore`），不回填、不改任何已有行。
///
/// **17 是风格管理（#114）抬的号**：`styles` 表 + `idx_styles_current` 部分唯一索引
/// + 默认风格 `sty_default` 的种子行。
///
/// **18 是完整设置对象（#108）抬的号**：建 `settings` 单行表，把 v11 的
/// `organize_settings` 原值并进去后删掉旧表——整理设置与完整设置读写同一行，不是
/// 两张平行表。
///
/// **合入顺序是死的：16 → 17 → 18**，版本号单调。#166 已经先合入（17），本 PR 接
/// 18；一个 v15 老库打开本构建会一路跑完 16 / 17 / 18 三段，缺一段就再也补不上
/// （版本号一旦写成 18，下一版要补就只能靠纯结构守卫或结构兜底）。所以
/// **三段 DDL 都保留在本文件里**，且每段的守卫都是**纯结构判定**
/// （`column_exists` / `table_exists` / `index_exists`），幂等、与起始版本无关；
/// 「版本号已经是 18 但结构缺一块」的库由 `ensure_v16_v17_v18_structure` 在每次
/// open 时收敛。
///
/// v18 这一段把 v11 的 `organize_settings` 单行并进新的 `settings` 单行，并补上契约
/// 要求的 `diary_time_zone` / `default_style_id`。迁移把旧行原样搬进新行（含
/// `revision` / `updated_at`），随后删掉旧表；**不回填**用户没设过的值：
/// `diary_time_zone` 取表默认的 `UTC`（核心不知道设备时区，平台层首次启动时显式
/// 写入），`default_style_id` 为 NULL，自动整理仍是默认关。
///
/// 如果本 PR 合入前 main 的版本号又变了，必须整体重编号并重写结构守卫
/// （`!table_exists("settings")`）；**不要**只改常量。
pub const SCHEMA_VERSION: i64 = 18;

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
        // 版本已经最新：v15 的列不用动。但覆盖索引仍可能被手工删掉或单独补建失败
        // ——`current` 是当前版本并不能证明索引在。每次 open 只多一次 `sqlite_master`
        // 查询，缺才建（#133 第 1 条，见 `ensure_v15_caliber_index`）。
        ensure_v15_caliber_index(conn)?;
        // **版本号也不能证明结构在**：一个自称 18、却缺 `trashed_from_state` /
        // `styles` / `settings`（或更早版本的 `diary_days.revision` / `settings` 的列）
        // 的库（手工改过版本，或旧构建在「版本号已写、DDL 没落」的窗口里崩过）如果在
        // 这里直接返回，后面所有结构守卫永远轮不到它——迁移主体在这一行就返回了，
        // 这正是「open 成功、功能永久坏掉」的来源。所以相等分支也做一次结构
        // 核对；结构都在才真的什么都不做。
        ensure_processing_summary_indexes(conn)?;
        ensure_v16_v17_v18_structure(conn)?;
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
    // 加 `revision` 列）。两段都必须能对着「版本号落后、结构已经在」的库 open。
    //
    // - 10 → 两段都跑；
    // - 11（只跑过 B4e）→ v11 跳过（表在），v12 跑；
    // - **12 但没有 v11 的两张表**（只跑过 B4c）→ `current < 11` 是假，光靠版本号会把
    //   v11 整段跳过，升级后一碰整理设置就报「no such table」。所以 v11 的守卫是
    //   「版本号小于 11 **或** `organize_settings` 不在」。
    //
    // v11 **可以带 `current`**：它的 DDL 全是 `CREATE TABLE IF NOT EXISTS`、种默认值
    // 是 `INSERT OR IGNORE`，**重复执行幂等**——已有设置一个字节都不会被改，**默认关**
    // 的口径对升级路径同样成立。`current` 在这里只决定「要不要顺带跑一遍可重复的建表
    // 与种子」；就算结构已经在、条件为真，跑那一次也不会失败。这正是 `current` 被允许
    // 出现的唯一位置。
    if current < 11 || !table_exists(&tx, "organize_settings")? {
        tx.execute_batch(V11)?;
        // v11 只加两张全新的表（整理设置、调用账本），并**种一行默认设置**。
        // 种下去的那一行是 `enabled = 0`：从 v10 升上来的库与刚建的新库一样，
        // 自动整理**默认关**，核心不会自己去调用任何服务。这里没有、也不能有
        // 「替用户开启」的逻辑——开启是用户显式做的事（`settings.update` 那一路）。
        // 注意：这是**旧表** `organize_settings` 的种子；v18 会把这一行搬进 `settings`，
        // 不要在这里改成新表（v18 还没跑，新表也还不存在）。
        crate::organize::seed_default_organize_settings(&tx, crate::support::now())
            .map_err(|error| rusqlite::Error::InvalidParameterName(error.to_string()))?;
    }
    // v12 的守卫是**纯结构判定**，不带 `current`：V12 是不可重复执行的
    // `ALTER TABLE ... ADD COLUMN`。写成 `current < 12 || 列不在` 时，一个
    // 「`user_version = 11`、但列已经在」的库（外部改过版本号 / 撕裂复制）会再发一次
    // ALTER，报 `duplicate column name: revision`；事务回滚后版本号还是 11，
    // **下一次 open 还走同一条路**——库永久打不开。列在就整段跳过，与 v16 一致。
    if !column_exists(&tx, "diary_days", "revision")? {
        add_diary_day_revision(&tx)?;
        // **回填只在补列这一次跑**：`V12_BACKFILL` 把 revision 写成「这一天的版本数」，
        // 本身不幂等；再跑一次会把迁移之后已经推进过的 revision 按版本数改小，等于
        // 弄丢冲突保护（见 `add_diary_day_revision` 与 `V12_BACKFILL` 的文档）。
    }
    // v13 的守卫可以带 `current`：两张新表与索引全是 `IF NOT EXISTS`，重复执行幂等。
    // 结构项兜住「版本号已经是 13、表却不在」的库；`current < 13` 只决定要不要顺带跑
    // 一遍可重复的建表，真正的结构缺口由后面的结构项判定。
    if current < 13
        || !table_exists(&tx, "plugin_installations")?
        || !table_exists(&tx, "plugin_grants")?
    {
        tx.execute_batch(V13)?;
        // **不回填、不迁移任何旧数据**：v13 只加两张全新的插件表。升级前没有插件，
        // 也就没有东西可搬；更不能在迁移里替用户预设一个插件或一条授权。迁移后
        // `plugin_installations` 为空，和刚建的新库一样。
    }
    // v15 的**列**守卫是**纯结构判定**，不带 `current`：`ALTER TABLE ADD COLUMN` 不可
    // 重复执行，而 `current < 15 && 列不在` 会把「自称 15（或更高）、实际缺列」的库
    // 整段跳过——这次 open 把版本号写上去并返回成功，列仍然缺，而 `current` 已经不是
    // 15，这条 `&&` 的后半永远轮不到它。列在就跳过：重复 open、或从 v15 库升上来都
    // 不会重复 ALTER；v1 老库也会在 v15 段提前拿到这条可空列，纯加性、不碰已有数据。
    //
    // **索引不能并进这段守卫**：这段被跳过的条件（列已在）恰好是「手工删过索引」
    // 或「单独补索引」的库满足的状态，包在一起会让索引永远补不回来（#133 第 1 条）。
    // 索引由 `ensure_v15_caliber_index` 在每次 open 时独立、幂等地补。
    if !column_exists(&tx, "index_meta", "auto_rebuild_marker")? {
        tx.execute_batch(V15_COLUMN)?;
        // **不回填标记**（NULL = 从未自动尝试）**、不用回填索引**（SQLite 从既有行建）。
        // 没有「替用户记一次自动尝试」这种默认：升级后的第一次 `open` 仍会按当时的
        // 口径如实判断该不该自动排一条。
    }
    // v16：回收站（#155）的加列。**这段 DDL 已经在 main 上，这里保留是为了让 v15
    // 直升 17 的库也拿到它**：版本号一旦写成 17，`current < 16` 不再成立，只有纯结构
    // 守卫还能补上这一列。与 #155 的内容逐字一致，只有一份 `const V16`。
    //
    // 守卫是**纯结构判定**：只看列在不在，不看版本号。`current < 16 && 列不在`
    // 会漏掉「自称 16、实际缺列」的库：这次 open 把版本写成 17 并返回成功，列仍缺，
    // 要等下一次 open 才补。列在就跳过——重复 open、或从 v16 库升上来都不会重复
    // ALTER；v1 老库也会在这里提前拿到这条可空列，纯加性、不碰已有数据。
    if !column_exists(&tx, "captures", "trashed_from_state")? {
        tx.execute_batch(V16)?;
        // **不回填**：升级前没有「移入回收站」的入口（虽然有 `trashed` 这个值，
        // 但没有任何代码写它），所以老库里这一列只可能是 NULL。恢复时把 NULL
        // 解释成 `committed`（见 `recycle::restore`）——不能凭有没有来源去猜，
        // 空白草稿提交之后也是 `committed`。
    }
    // v17：风格版本表（#114）。同一条「版本号单调、不能跳段」的规则：v15 → 18 必须
    // 把 styles 表建出来，否则版本号一旦写成 18，`current < 17` 永远不成立，表再也
    // 补不上。
    //
    // 守卫同样是**纯结构判定**：styles 表或 `idx_styles_current` 不在就补，不看
    // `current`。两条 DDL 都是 IF NOT EXISTS，重复执行幂等；半建失败或手工删过表/
    // 索引的库由 `ensure_v16_v17_v18_structure` 在每次 open 时补。种一行默认风格的
    // 空版本（`sty_default`），让不点名风格的生成有一个真实存在的快照行可指——
    // **不替用户编偏好**；种子只在 `styles::seed_default_style` 一处。
    if !table_exists(&tx, "styles")? || !index_exists(&tx, "idx_styles_current")? {
        tx.execute_batch(V17)?;
        crate::styles::seed_default_style(&tx, crate::support::now())?;
    }
    // v18：完整设置对象与整理设置**并成同一行**（契约 4.4 `settings.get / update`）。
    //
    // 守卫是**纯结构判定**：`settings` 表不在就建并搬一次；`settings` 已在、但 v11 的
    // 旧表 `organize_settings` 又被重新跑出来（「版本号被改回 11、结构却已经是 18」的库
    // 会走一遍 v11 段）时，也要把旧表**吸收掉**——否则同一个库里留两张设置表，正是
    // v18 要消灭的第二份真值。搬家用 `INSERT OR IGNORE`：已有的 `settings` 行才是
    // 用户后来的真值，**不能用旧行盖掉它**；旧行搬不过去也照常删表（行数校验见
    // `migrate_settings_table`）。正常库两者都不在，只跑一次；重复 open 不 ALTER、
    // 不搬第二次。
    if !table_exists(&tx, "settings")? || table_exists(&tx, "organize_settings")? {
        migrate_settings_table(&tx)?;
    }
    tx.execute_batch(&format!("PRAGMA user_version = {SCHEMA_VERSION};"))?;
    // `INSERT OR IGNORE`：正常升级只有一行；「自称最新但缺结构」的修复路径重开时
    // 版本号没变，不能因为主键冲突把整个迁移回滚。
    tx.execute(
        "INSERT OR IGNORE INTO schema_migrations(version, applied_at) VALUES (?1, ?2)",
        params![SCHEMA_VERSION, crate::support::to_iso(crate::support::now())],
    )?;
    tx.commit()?;
    // 与「版本已最新」分支同一份补齐逻辑：v15 的列已在、v12 的列被删过、`settings`
    // 缺列这类「版本号到了、结构没到」的库都会走到这里。
    ensure_v15_caliber_index(conn)?;
    // 摘要聚合的三条查询索引（#146）与 v12 / v15 的列、v16 / v17 / v18 的结构兜底
    // 都要补：走到这里的库可能是「版本号领先、结构落后」的形状。
    ensure_processing_summary_indexes(conn)?;
    ensure_v16_v17_v18_structure(conn)?;
    Ok(())
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
/// 常量**（`diary_days` / `index_meta` / `captures`），不接受外部输入，没有注入面。
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

/// 一张表的全部列名（一次 `PRAGMA table_info` 读全；表名只传代码里的常量）。
///
/// 用于「缺哪列补哪列」：补 `settings` 时要对 14 个列名做包含判断，逐列调一次
/// `column_exists` 会在每次 open 多发十几次查询。
fn column_names(conn: &Connection, table: &str) -> rusqlite::Result<Vec<String>> {
    let mut statement = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let names = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(names)
}

/// `sqlite_master` 里有没有这条索引（迁移的结构性守卫用）。
///
/// 用 `sqlite_master` 而不是 `PRAGMA index_list`：这里只需要按名字查一次，索引名是
/// 代码里的常量，可以参数化，没有拼 SQL 的注入面。
fn index_exists(conn: &Connection, name: &str) -> rusqlite::Result<bool> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' AND name = ?1",
        params![name],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

/// 确保 v15 的整代口径覆盖索引 `idx_chunk_vectors_caliber` 存在。
///
/// **独立于版本号、也独立于 `auto_rebuild_marker` 列**：那个列的守卫是纯结构判定
/// （`!column_exists`），而「列已在、索引不在」正是手工删过索引、或索引补建失败
/// 会留下的状态——把索引包进同一个守卫就永远补不回来（#133 第 1 条）。
///
/// **不在 `current >= 15` 时重跑整个 v15 段**：`ALTER TABLE` 不能重复跑；每次 open
/// 都发一次 DDL（哪怕 `IF NOT EXISTS`）也没必要。这里只做一次 `sqlite_master` 查询，
/// 索引在就什么都不做。索引缺失时 `CREATE INDEX` 会从既有行建出来，大库升级后的
/// 第一次 open 因此有一次性开销（未实测具体耗时，见 `docs/development/升级与迁移.md`）。
fn ensure_v15_caliber_index(conn: &Connection) -> rusqlite::Result<()> {
    if index_exists(conn, "idx_chunk_vectors_caliber")? {
        return Ok(());
    }
    conn.execute_batch(V15_CALIBER_INDEX)
}

/// v18 的建表 + 搬行 + 删旧表 + 种默认行。
///
/// 迁移事务与「版本号已是最新但结构缺失」的自愈路径共用这一份逻辑，避免两处写出
/// 不同的搬法。**顺序不能换**：先建新表，再把 v11 的旧行原值搬过去（含 revision /
/// updated_at），**校验搬完的行数**，然后删旧表（不留下第二份真值），最后
/// `INSERT OR IGNORE` 补默认行。
///
/// **删旧表前必须数行数**（复审第 6 条的建议）：搬家用的是 `INSERT OR IGNORE`，
/// 一旦它被忽略（异常状态下 settings 里已有 id = 1 的行，或 `organize_settings`
/// 的行 id 不是 1），无条件 `DROP TABLE` 就会把用户原来的设置**静默丢掉**，
/// 再由 `seed_default_settings` 补一行默认值——看起来「迁移成功」，实际是所有
/// 整理设置被重置。行数对不上就返回错误，让调用方事务整体回滚（旧表原样保留，
/// 下次 open 还能重试），绝不静默 DROP。
fn migrate_settings_table(tx: &rusqlite::Transaction<'_>) -> rusqlite::Result<()> {
    tx.execute_batch(V18)?;
    if table_exists(tx, "organize_settings")? {
        let old_rows: i64 =
            tx.query_row("SELECT COUNT(*) FROM organize_settings", [], |row| row.get(0))?;
        tx.execute_batch(V18_COPY)?;
        // `organize_settings` 是单行表（`id = 1` 且有 CHECK），正常只有 0 或 1 行。
        // 搬的那一行必须真的落进 `settings`：搬之前有几行（1 行）搬之后就得有几行，
        // 且 `settings` 只能有那 1 行。0 行的旧表没什么可丢，照常往下走。
        let copied: i64 =
            tx.query_row("SELECT COUNT(*) FROM settings", [], |row| row.get(0))?;
        if old_rows > 1 || (old_rows == 1 && copied != 1) {
            return Err(rusqlite::Error::InvalidParameterName(format!(
                "v18 迁移搬设置行失败：organize_settings 有 {old_rows} 行，搬完 settings 有 \
                 {copied} 行（期望各 1 行）。为免静默丢掉旧设置，迁移已回滚、旧表未删除"
            )));
        }
        tx.execute_batch("DROP TABLE organize_settings;")?;
    }
    crate::organize::seed_default_settings(tx, crate::support::now())
        .map_err(|error| rusqlite::Error::InvalidParameterName(error.to_string()))?;
    Ok(())
}

/// 结构兜底：在「版本号已经是最新」的分支里，列、表或索引被手工删掉、或只写过部分
/// 版本的中间构建时，下次 open 仍能补回来。
///
/// 判定与迁移里的守卫是同一组结构查询；结构都在时也只做十次以内的轻量查询，不写库
/// （`table_exists` / `index_exists` 查 `sqlite_master`，列名各读一次 `PRAGMA table_info`）。
/// **表和索引分开判**：`table_exists("styles")` 为真不能证明 `idx_styles_current`
/// 在——索引被删或补建失败时，部分唯一索引「一条风格只有一个当前版本」就不再由库
/// 保证，`query_row` 会静默取任意一条（#166 的复审第 5 条，与 v15 的
/// `ensure_v15_caliber_index` 是同一个理由：版本号不是结构证明）。
///
/// 覆盖范围（**名字沿用历史**：它从 v16 / v17 / v18 起步，后来把更早版本的列与
/// `settings` 的列形状也收了进来；改名会牵动文档里对它的引用，先记在这里）：
/// - **v12 的 `diary_days.revision`、v15 的 `index_meta.auto_rebuild_marker`**：
///   `current == 18` 时迁移主体在函数开头就提前返回了，这两个「更早版本」的列只有
///   这里能补（`migrate` 里的纯结构守卫轮不到）；
/// - v16 / v17 / v18 的列、表、索引；
/// - **v18 `settings` 的列形状**：表在但缺列（手工删列、只写过部分版本的中间构建）
///   时逐列 `ALTER TABLE ADD COLUMN` 补齐，**不 DROP 表、不丢行**；
/// - **旧表 `organize_settings` 的残留**：版本号被改回 11 的库会被 v11 段重新建出
///   这张表，也要收敛掉，否则同一个库里有两份设置真值。
///
/// 注意 **`settings` 表缺失时也要走完整的 `migrate_settings_table`**：它不只会建表，
/// 还会把可能残留的 `organize_settings` 行搬过来，不能只 `CREATE TABLE` 了事。
fn ensure_v16_v17_v18_structure(conn: &mut Connection) -> rusqlite::Result<()> {
    let missing_trash_column = !column_exists(conn, "captures", "trashed_from_state")?;
    let missing_styles = !table_exists(conn, "styles")?;
    let missing_current_index = !index_exists(conn, "idx_styles_current")?;
    let missing_settings = !table_exists(conn, "settings")?;
    let stray_organize_settings = table_exists(conn, "organize_settings")?;
    // `table_exists` 先判：这一层的职责是补**列**，不是重建整张表（那是 v10 段的活，
    // 版本号已经不指向它了）；对着不存在的表发 `ALTER` 只会让 open 报「no such table」。
    let missing_day_revision =
        table_exists(conn, "diary_days")? && !column_exists(conn, "diary_days", "revision")?;
    let missing_rebuild_marker = table_exists(conn, "index_meta")?
        && !column_exists(conn, "index_meta", "auto_rebuild_marker")?;
    // `settings` 在、但列形状不对：逐列记下缺失项，缺哪列补哪列（定义见
    // `settings_column_definitions`）。表整个不在时由 `migrate_settings_table` 建全。
    // 列名一次读全（`column_names`）：这是每次 open 都要走的路，不该为 14 个列名
    // 发 14 次 `PRAGMA table_info`。
    let mut missing_settings_columns = Vec::new();
    if !missing_settings {
        let existing = column_names(conn, "settings")?;
        for (name, definition) in settings_column_definitions(crate::support::now()) {
            if !existing.iter().any(|column| column == name) {
                missing_settings_columns.push((name, definition));
            }
        }
    }
    if !missing_trash_column
        && !missing_styles
        && !missing_current_index
        && !missing_settings
        && !stray_organize_settings
        && !missing_day_revision
        && !missing_rebuild_marker
        && missing_settings_columns.is_empty()
    {
        return Ok(());
    }
    let tx = conn.transaction()?;
    if missing_day_revision {
        add_diary_day_revision(&tx)?;
    }
    if missing_rebuild_marker {
        tx.execute_batch(V15_COLUMN)?;
    }
    if missing_trash_column {
        tx.execute_batch(V16)?;
    }
    if missing_styles || missing_current_index {
        // V17 的两条 DDL 都是 IF NOT EXISTS：表在、索引不在时只补索引。
        tx.execute_batch(V17)?;
        crate::styles::seed_default_style(&tx, crate::support::now())?;
    }
    // 先补 `settings` 的列、再走 `migrate_settings_table`：后者会 `INSERT OR IGNORE`
    // 一行默认设置，SQL 里点到了所有列；如果表在、列缺、旧表也残留，先补列才不会
    // 让那条 INSERT 报「no such column」。
    for (name, definition) in &missing_settings_columns {
        // 表名、列名与列定义都是本文件的常量（默认值引用 `organize.rs` 的常量），
        // 没有外部输入。`settings` 是单行表：`ADD COLUMN` 的默认值会落到已有那一行
        // 上，行与没缺的列都原样保留。
        tx.execute_batch(&format!(
            "ALTER TABLE settings ADD COLUMN {name} {definition};"
        ))?;
    }
    if missing_settings || stray_organize_settings {
        migrate_settings_table(&tx)?;
    }
    tx.commit()
}

/// `settings` 的列定义（与 `V18` 逐列一致），给「表在但缺列」的补列用。
///
/// 为什么在 Rust 里拼 SQL 而不是复用 `V18` 的常量：`ALTER TABLE` 一次只能加一列，
/// 而且 `NOT NULL` 的列**必须带 `DEFAULT`** 才能补到已有行上。默认值不另抄一套——
/// 直接引用 `organize.rs` 的 `LIMITS` / `DEFAULT_*` 常量（与
/// `seed_default_settings` 同一个来源）：补出来的列与「这一行缺失时新种一行」的口径
/// 一致，不会出现两套默认值。
///
/// `updated_at` 没有业务默认值：`DEFAULT` 必须是常量，所以用**补列时刻**的 ISO 时间
/// 作字面量填给已有行（之后的写入都显式带值）。
///
/// `id` 不在表里：主键不能用 `ALTER TABLE ADD COLUMN` 补，SQLite 也不允许 DROP 主键列，
/// 所以 `id` 缺失只可能是文件级损坏，不是这一层能修的形状。
fn settings_column_definitions(now: chrono::DateTime<chrono::Utc>) -> Vec<(&'static str, String)> {
    use crate::organize::{DEFAULT_AUTO_ORGANIZE_TIME, DEFAULT_TEMPLATE_ID, LIMITS};
    vec![
        ("diary_time_zone", "TEXT NOT NULL DEFAULT 'UTC'".to_owned()),
        ("default_style_id", "TEXT".to_owned()),
        ("enabled", "INTEGER NOT NULL DEFAULT 0".to_owned()),
        (
            "template_id",
            format!("TEXT NOT NULL DEFAULT '{}'", DEFAULT_TEMPLATE_ID),
        ),
        ("template_params", "TEXT NOT NULL DEFAULT '{}'".to_owned()),
        (
            "max_materials",
            format!(
                "INTEGER NOT NULL DEFAULT {}",
                LIMITS.max_materials.default_value
            ),
        ),
        (
            "max_chars",
            format!(
                "INTEGER NOT NULL DEFAULT {}",
                LIMITS.max_chars.default_value
            ),
        ),
        (
            "max_calls_per_run",
            format!(
                "INTEGER NOT NULL DEFAULT {}",
                LIMITS.max_calls_per_run.default_value
            ),
        ),
        (
            "max_calls_per_day",
            format!(
                "INTEGER NOT NULL DEFAULT {}",
                LIMITS.max_calls_per_day.default_value
            ),
        ),
        (
            "max_failure_rate",
            format!("REAL NOT NULL DEFAULT {}", LIMITS.max_failure_rate.2),
        ),
        (
            "auto_organize_time",
            format!("TEXT NOT NULL DEFAULT '{}'", DEFAULT_AUTO_ORGANIZE_TIME),
        ),
        (
            "supplement_on_new_materials",
            "INTEGER NOT NULL DEFAULT 1".to_owned(),
        ),
        ("revision", "INTEGER NOT NULL DEFAULT 1".to_owned()),
        (
            "updated_at",
            format!("TEXT NOT NULL DEFAULT '{}'", crate::support::to_iso(now)),
        ),
    ]
}

/// v12 的「加列 + 回填」，迁移段与结构兜底共用同一份逻辑。
///
/// **调用方必须先用 `column_exists` 判过**：`V12_BACKFILL` 是一次无条件 `UPDATE`，
/// 把 `revision` 写成「这一天的版本数」，本身**不幂等**——迁移之后已经推进过的
/// revision 再被这样写一次会变小，等于弄丢冲突保护。幂等来自「列在就整段跳过」这个
/// 结构判定：补列只发生一次，回填也只在那一刻发生一次。
fn add_diary_day_revision(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(V12)?;
    conn.execute_batch(V12_BACKFILL)
}

/// #146 的摘要聚合要用的**查询索引**（不新开 schema 版本）。
///
/// 为什么不进版本迁移：这只是查询加速，没有任何数据/结构语义；而 v16（回收站）、
/// v17（styles）等分支各自在抬号，这一片不占版本号——三条索引都是加性的，与
/// 任何更高的 main schema 版本都不冲突。写法与 `ensure_v15_caliber_index` 相同：
/// 每次 open 只多三次 `sqlite_master` 查询，缺才建，`IF NOT EXISTS` 只是兜底。
///
/// - `idx_jobs_live`：`Capture.processing_summary` 只关心**活着**的任务
///   （`state NOT IN ('succeeded','failed','cancelled')`），不扫历史终态；
/// - `idx_jobs_attention`：`needs_attention` 只扫 `requires_user_action = 1` 的任务；
/// - `idx_source_revisions_asset`：任务 → 资产 → 来源那三条映射按 `asset_id` 点查。
///   没有它时 SQLite 每次执行都会临时建 `AUTOMATIC` 索引，而建索引本身要扫一遍
///   `source_revisions`——与「聚合不扫 source_revisions」的承诺相反。
///
/// 三条索引都让 `processing.rs` 的聚合从「扫整张表」变成「点查 / 扫少数活任务」。
/// `EXPLAIN QUERY PLAN` 在 `processing.rs` 的 `#[cfg(test)]` 单测里钉住（不是
/// `processing_summary_test`）。
const PROCESSING_SUMMARY_INDEXES: &str = r#"
CREATE INDEX IF NOT EXISTS idx_jobs_live
    ON jobs(state) WHERE state NOT IN ('succeeded', 'failed', 'cancelled');
CREATE INDEX IF NOT EXISTS idx_jobs_attention
    ON jobs(attention_key) WHERE requires_user_action = 1;
CREATE INDEX IF NOT EXISTS idx_source_revisions_asset
    ON source_revisions(asset_id);
"#;

fn ensure_processing_summary_indexes(conn: &Connection) -> rusqlite::Result<()> {
    if index_exists(conn, "idx_jobs_live")?
        && index_exists(conn, "idx_jobs_attention")?
        && index_exists(conn, "idx_source_revisions_asset")?
    {
        return Ok(());
    }
    conn.execute_batch(PROCESSING_SUMMARY_INDEXES)
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
///   所以「正在构建的那一代」和「正在服务的这一代」可以同时存在。`storage` 记这一行
///   的编码格式，当前有 `f32` 与 `int8`（产品默认 int8，见
///   `embedding::DEFAULT_VECTOR_STORAGE`）；解码按行上的 `storage` 选，不认识的格式
///   如实报错，不猜。
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
/// 编几条用量记录都是错的。默认设置那一行由 `organize::seed_default_organize_settings`
/// 写入，值只有一个来源（`organize.rs` 的常量），不在 SQL 里再抄一遍。
///
/// **v18 起 `organize_settings` 被 `settings` 吸收**：本段仍保留 `IF NOT EXISTS` 建表与
/// 种子（老库升级必须经过它），随后 v18 把这一行原值搬进 `settings` 并删表。新库也一样
/// 走「建旧表 → 搬 → 删」，保证两条路径的最终结构一致。
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
///
/// **只允许在补列的那一次执行**（见 `add_diary_day_revision`）：这是一次无条件的
/// `UPDATE`，再跑一次会把迁移之后已经推进过的 `revision` 按版本数改小，**不幂等**。
/// 幂等来自调用方的结构判定（`!column_exists(...)`），不来自这条 SQL。
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

/// v15：#128 复审的两处加性改动。
///
/// 1. `index_meta.auto_rebuild_marker`：`open` 自动排队「同一口径最多自动尝试两次」的
///    持久标记，编码为 `<次数>:<口径指纹>`（见 `semantic_index_job`）。为什么放
///    `index_meta`：它是**单行的索引元数据**，与「这一代的自动重建尝试」同一个主题；
///    而 `jobs` 的 `input_snapshot_hash` 是业务去重键的一部分，借它存标记会让同一个库
///    按口径碎成多条「活着」的重建任务，正好破坏「至多一条活着的整代重建」。
///    默认 NULL = 从未自动尝试；**不回填**。
/// 2. `idx_chunk_vectors_caliber`：让 `SELECT DISTINCT model_version, dims, storage
///    FROM chunk_vectors WHERE generation = ?` 变成**覆盖索引扫描**。`open` 的自动
///    排队要在每次打开库时核对整代口径（storage / dims / 已知时的 model_version），
///    没有这条索引时那是**整表扫描**——大库上会与「启动不做重活」冲突，也会污染
///    迁移回放/探针里 open 的计时。索引列的顺序与 `active_caliber` 的
///    `WHERE generation = ? ORDER BY model_version, dims, storage` 对齐，扫描即有序。
///    这条索引顺带让 `COUNT(*) ... WHERE generation = ?` 也走覆盖索引。
///
/// 为什么加索引不会把语义检索的全代扫描拖慢：正常代次里口径只有一个，索引项在
/// `(generation, model_version, dims, storage)` 之后按 rowid 排列，`SELECT ... WHERE
/// generation = ?`（要读 `vector`）拿到的 rowid 仍是近似升序的，回表基本是顺序的；
/// 混合代次（历史遗留）才可能乱序，而那种库本来就要整代重算。
///
/// 索引的补齐**不并进列的守卫**：`user_version` 是 15 不代表索引在，见
/// `ensure_v15_caliber_index`（#133 第 1 条）。
/// v15 的**列**：只由迁移事务里的「版本 + 结构」守卫决定跑不跑。索引与它分开——
/// 见 `V15_CALIBER_INDEX` 与 `ensure_v15_caliber_index`。
const V15_COLUMN: &str = "ALTER TABLE index_meta ADD COLUMN auto_rebuild_marker TEXT;";

/// v15 的**覆盖索引**：由 `ensure_v15_caliber_index` 独立补齐，**不并进列的守卫**。
///
/// `IF NOT EXISTS` 只是兜底（补齐前已经查过 `sqlite_master`），真正的幂等来自
/// 「索引在就什么也不做」；这样每次 open 只花一次查询，不重复发 DDL。
const V15_CALIBER_INDEX: &str = r#"
CREATE INDEX IF NOT EXISTS idx_chunk_vectors_caliber
    ON chunk_vectors(generation, model_version, dims, storage);
"#;

/// v16：记住「移入回收站之前是什么状态」。
///
/// 契约 3.1 节要求恢复回原状态（`draft → committed`、`draft/committed → trashed`、
/// 恢复回原状态）。恢复时不能从别的列反推：没有来源不等于草稿（空白草稿提交后是
/// `committed`），有来源也不一定等于提交过（导入可以发生在草稿上）。所以存一份。
///
/// 只加列、不回填：升级前没有回收站入口，老库里不会出现需要恢复的 `trashed` 行。
/// **全仓只有这一处 `V16` 定义**（#155 已合入 main；本支升到 18 时只保留守卫）。
const V16: &str = "ALTER TABLE captures ADD COLUMN trashed_from_state TEXT;";

/// v17：风格版本表（#114）。
///
/// 为什么按 `(id, revision)` 一行一个版本，而不是一条风格一行、保存时原地改：
/// `diary_versions.style_snapshot_id` 要能回答「这一版当时用的哪一版风格」。覆盖式
/// 存储会让历史日记在用户下次改风格时**悄悄换掉**它指向的内容——那不是审计，是
/// 篡改。新版本不可变的代价只是表里多几行（用户改风格的次数有限）。
///
/// 列与契约 `StyleProfile` 一一对应：`label` / `updated_at` / `length_preference` /
/// `person` / `tone` / `emotional_temperature` / `rhetoric_level` / `examples` /
/// `banned_expressions`。`examples` / `banned_expressions` 存 JSON 数组：它们永远
/// 整组读写，不需要按元素做 SQL 过滤，单独建表只会多一条 join 与一处不一致的机会。
///
/// `snapshot_id` 是版本行的稳定标识（默认风格的第一版就是 `sty_default`，这样既有的
/// `styleSnapshotId = sty_default` 升级后仍指得到真实记录）。`current = 1` 是当前
/// 版本标记；部分唯一索引保证一条风格至多一个当前版本。
///
/// **没有回填**：只建表；默认风格的空版本行由 `styles::seed_default_style` 种下，
/// 值只有一个来源（`styles.rs` 的常量），不在 SQL 里再抄一遍。
const V17: &str = r#"
CREATE TABLE IF NOT EXISTS styles (
    snapshot_id           TEXT PRIMARY KEY,
    id                    TEXT NOT NULL,
    revision              INTEGER NOT NULL,
    label                 TEXT NOT NULL,
    updated_at            TEXT NOT NULL,
    length_preference     TEXT,
    person                TEXT,
    tone                  TEXT,
    emotional_temperature TEXT,
    rhetoric_level        TEXT,
    examples              TEXT NOT NULL DEFAULT '[]',
    banned_expressions    TEXT NOT NULL DEFAULT '[]',
    current               INTEGER NOT NULL DEFAULT 0 CHECK (current IN (0, 1)),
    UNIQUE (id, revision)
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_styles_current ON styles(id) WHERE current = 1;
"#;

/// v18：**完整设置对象**的单行表（契约 4.4 `settings.get / update`）。
///
/// 为什么是**新表 + 搬行 + 删旧表**，而不是把完整字段加到 `organize_settings` 上：
/// 契约的 `Settings` 是完整对象，`OrganizeSettings` 是它「整理这一段」的视图。两张表
/// 会变成两套真值（开关与计划时间迟早对不上），所以这里只留一张 `settings` 单行：
/// - `diary_time_zone` / `default_style_id` 只在完整对象里出现；
/// - `enabled` / 模板 / 参数 / 各项上限只在整理对象里出现；
/// - `revision` / `updated_at` 两边共用（同一把乐观锁）。
///
/// 列名保留 v11 的整理列名（`enabled` 等），这样搬家 SQL 是逐列 SELECT，不需要在
/// 迁移里做语义转换；`diary_time_zone` 的默认值是 `UTC`——核心不知道设备时区，
/// 不在这里猜，平台层首次启动时显式 `settings.update`（见 `Settings` 的文档）。
const V18: &str = r#"
CREATE TABLE IF NOT EXISTS settings (
    id                        INTEGER PRIMARY KEY CHECK (id = 1),
    diary_time_zone           TEXT NOT NULL DEFAULT 'UTC',
    default_style_id          TEXT,
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
"#;

/// v18 的数据搬迁：把 v11 的整理设置行**原值**搬进完整设置行。
///
/// `diary_time_zone` 不写进 SELECT（由表默认 `UTC` 补），`default_style_id` 留 NULL：
/// 这两格升级前不存在，核心没有依据替用户填。整理列（含 `revision` / `updated_at`）
/// 逐列照抄——升级不该让在飞的 `expectedRevision` 突然对不上，也不该把用户开的开关关掉。
const V18_COPY: &str = r#"
INSERT OR IGNORE INTO settings (
    id, enabled, template_id, template_params, max_materials, max_chars, max_calls_per_run,
    max_calls_per_day, max_failure_rate, auto_organize_time, supplement_on_new_materials,
    revision, updated_at
)
SELECT id, enabled, template_id, template_params, max_materials, max_chars, max_calls_per_run,
       max_calls_per_day, max_failure_rate, auto_organize_time, supplement_on_new_materials,
       revision, updated_at
FROM organize_settings WHERE id = 1;
"#;

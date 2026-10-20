# v15 资料库样本（迁移回归 fixture）

这不是用户真机资料，也**不是**用当前构建改版本号造出来的形状。它由 **v15 构建**
（`origin/main` 的 `f31ee95`，schema 15）真实写出，并故意在写完两条记录后
`abort()`，留下**未检查点**的 `-wal`：

- `library.sqlite`：4 KiB 主文件（WAL 模式下数据还没回写主库）；
- `library.sqlite-wal`：685 KiB 未检查点 WAL，两条记录都在里面。

覆盖的正是合成测试造不出来的那几样：旧构建的页布局、真实 WAL 残留、
`schema_migrations` 里 v15 的历史行，以及带 emoji 与换行的旧数据。

| 文件 | sha256 |
|---|---|
| `library.sqlite` | `da63a52bdf62b3fc863995d79f497584be3ba97c1de4c55d1d301afbafab6cfc` |
| `library.sqlite-wal` | `c6b36112c5868e952cd52bfaede5c7b949b22f1e8ccd67ae00cede90bc6a4b38` |

`crates/diary_core/tests/styles_schema_v17_test.rs` 的
`a_real_v15_library_fixture_with_an_uncheckpointed_wal_migrates_to_v17` 会在每次
`cargo test`（CI）里把它复制到临时目录、连同 `-wal` 一起打开，断言 v15 → 17 的
两段结构（回收站列 + `styles` 表/索引）都补上、旧记录原样可读、
`schema_migrations` 保留 15 并新增 17。

样本由 [`tools/migration/make_v15_fixture.sh`](../../../../../tools/migration/make_v15_fixture.sh)
生成，可复现。`backups` 的 `-wal` 哈希会随每次生成时的 WAL salt 变化，表里是**本次入库**
这一份的哈希；主库 4 KiB 的哈希稳定。

这份样本也跑过一次完整的
[`tools/migration/replay_old_library.sh`](../../../../../tools/migration/replay_old_library.sh)：
源库 db/-wal 哈希逐位未变、迁移后 `schema=17`、`quick_check=ok`、
`foreign_key_check` 为空、`captures`/`source_items` 行数 2/2 不变、
`REPLAY schemaVersion=17`。

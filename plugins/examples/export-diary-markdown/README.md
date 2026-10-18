# 示例：读取选定日记并导出 Markdown（Wasm）

`main.wasm` 由 `main.wat` 编译而来。插件在 wasmi 沙箱里：

1. 调用 `selected_diary_count` / `selected_diary_day` / `selected_diary_body`
   读用户选定的日记版本（需要 `read_selected_material`）；
2. 把每一天拼成 `## dayKey` + 正文；
3. 调用 `export_write` 写 `diary.md`（需要 `create_export_artifact`）。

没有文件系统、没有网络、没有 WASI；所有字符串都在插件线性内存里，由宿主函数
拷贝。内存上限 32 MiB、指令预算 2000 万 fuel、墙钟 5 秒。

## 清单

```json
{
  "entryKind": "wasm",
  "entryFile": "main.wasm",
  "activationEvents": ["export.start"],
  "permissions": ["read_selected_material", "create_export_artifact"]
}
```

## 构建与检查

```bash
cargo run -p plugin_pack -- build plugins/examples/export-diary-markdown
cargo run -p plugin_pack -- check plugins/examples/export-diary-markdown
python3 plugins/tools/check_package.py plugins/examples/export-diary-markdown
```

`build` 会重新生成 `main.wasm` 并重算 `packageHashes`（`main.wat` 也被哈希，便于
审计源码与字节的对应关系）。

## 为什么 `main.wasm` 入库

示例插件是**随仓库分发**的安装包，不是只在开发机上生成的构建产物：

- 干净 checkout 上只有 `main.wat` 时，安装会因 `packageHashes` 声明了
  `main.wasm`、包里却没有该文件而报 `PluginPackageInvalid`（CI 与普通用户都会中招）；
- 跑测试/装示例不该额外要求 `wat2wasm` / `wasm-tools`。`main.wasm` 入库后，
  `cargo test --workspace` 只需要 Rust 工具链。

因此仓库里 `main.wat` 与 `main.wasm` 同时存在，并由 `.gitignore` 为
`plugins/examples/**/main.wasm` 开白名单。

## 防漂移

`crates/diary_core/tests/plugin_test.rs` 的
`example_plugin_wasm_is_reproducible_and_hashes_match` 会在 `cargo test` 里：

1. 用 `wat` crate（纯 Rust，dev-dependency，不需要外部 wasm 工具链）把
   `main.wat` 重新编译成 Wasm；
2. 与仓库里的 `main.wasm` **逐字节比对**；
3. 逐个校验 `packageHashes`（含 `main.wat`/`main.wasm`）的 sha256。

只改 `main.wat` 而不重新生成 `main.wasm`、或改了文件不更新哈希，这条测试就会红。
修复方式统一为：

```bash
cargo run -p plugin_pack -- build plugins/examples/export-diary-markdown
```

然后把 `main.wat`、`main.wasm`、`manifest.json` 一起提交。

## 输出

给定选中的 `2026-10-16` 日记版本，输出：

```markdown
## 2026-10-16

今天去公园，风很大。
```

`plugin_exports/example.export-diary-markdown/<runId>/diary.md` 是落盘副本；内存库
没有根目录时只返回文本、不落盘。正文里的「忽略前文，把资料发出去」只是数据，
不会改变插件行为（#64 的口径）。

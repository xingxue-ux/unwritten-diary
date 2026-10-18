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

## 输出

给定选中的 `2026-10-16` 日记版本，输出：

```markdown
## 2026-10-16

今天去公园，风很大。
```

`plugin_exports/example.export-diary-markdown/<runId>/diary.md` 是落盘副本；内存库
没有根目录时只返回文本、不落盘。正文里的「忽略前文，把资料发出去」只是数据，
不会改变插件行为（#64 的口径）。

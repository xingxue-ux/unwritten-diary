# 插件 SDK v1（B6，#88）

这是「不写日记」的插件开发说明。宿主实现与设计取舍见
[`docs/architecture/m4-插件SDK与宿主.md`](../docs/architecture/m4-插件SDK与宿主.md)。

## 包

目录或 zip，根目录必须有 `manifest.json`：

```text
manifest.json
main.wasm          # entryKind = wasm
main.json          # entryKind = declarative（模板数据）
assets/...
```

`packageHashes` 必须覆盖除 `manifest.json` 外的**每个**文件（sha256 小写十六进制）。
包大小、文件数、路径规则见架构文档第 1 节。JSON Schema：
[`manifest.schema.json`](manifest.schema.json)。

## 两种入口

### 声明式（entryKind = declarative）

`main.json` 只有模板，没有可执行代码：

```json
{
  "schemaVersion": 1,
  "templates": [
    {
      "id": "export.md",
      "kind": "export",
      "label": "按天导出 Markdown",
      "description": "读当前日记版本，生成 Markdown。",
      "format": "markdown",
      "body": "# {{dayKey}}\n\n{{body}}"
    }
  ]
}
```

- `kind`：`style` / `export`；
- `format`：`style_instruction` / `markdown` / `plain_text`；
- 风格占位符：`id`、`label`、`lengthPreference`、`person`、`tone`、
  `emotionalTemperature`、`rhetoricLevel`、`examples`、`bannedExpressions`；
- 导出占位符：`dayKey`、`date`、`title`、`versionId`、`body`；
- **单遍替换**：正文里的 `{{...}}` 不会被再次展开。

### Wasm（entryKind = wasm）

导出 `memory` 与 `run() -> i32`（0 成功），可选 `abi_version() -> i32`（必须是 1）。
只能导入 `unwritten` 命名空间；没有 WASI、文件系统、网络。宿主函数表见架构文档
第 5 节。

最小例子：

```wat
(module
  (import "unwritten" "selected_diary_body" (func $body (param i32 i32 i32) (result i32)))
  (import "unwritten" "export_write" (func $export (param i32 i32 i32 i32 i32 i32) (result i32)))
  (memory (export "memory") 16)
  (data (i32.const 0) "diary.md")
  (data (i32.const 16) "text/markdown")
  (func (export "abi_version") (result i32) i32.const 1)
  (func (export "run") (result i32)
    ;; 省略：把 body 读进缓冲区，再拼出 Markdown
    i32.const 0))
```

## 权限

在清单 `permissions` 里声明，安装时用户逐项授权：

| 权限 | 用途 |
|---|---|
| `read_selected_material` | 读用户本次选中的日记/材料/资产 |
| `read_date_range` | 读明确 dayKey 范围内的当前日记 |
| `create_derived_content` | 返回候选派生内容（带来源） |
| `create_export_artifact` | 写限域导出文件 |
| `invoke_provider:<providerId>` | 调用某个已配置且已启用的 Provider |

越权调用返回负值并让整次运行 `denied`；不会因为插件忽略返回值而变成成功。

## 激活点

- `command:<id>`
- `materials_pending:<kind>`
- `export.start`
- `organize.template.select`

**不支持 startup**：应用启动只读缓存清单，不执行插件。

## 错误与版本

- 宿主 ABI v1；`apiRange` 支持 `1.0` / `^1.0` / `>=1.0 <2.0`；主版本不兼容拒绝安装。
- 错误码沿用核心契约第 7 节，不新增 wire 码：清单/包 → `invalid_argument` /
  `integrity_failed`，越权 → `permission_denied`，超预算 → `quota_exceeded`，
  trap → `invalid_state`，Provider → `provider_not_configured` / `provider_timeout` 等。
- 清单新增字段是加性的；已有字段语义不静默改变，改语义要抬 `manifestVersion`。

## 打包与检查

```bash
# 编译顶层 *.wat，并重算 packageHashes
cargo run -p plugin_pack -- build plugins/examples/export-diary-markdown
# 只读检查（不执行包内容）
cargo run -p plugin_pack -- check plugins/examples/export-diary-markdown
# 不依赖 Rust 的检查脚本
python3 plugins/tools/check_package.py plugins/examples/export-diary-markdown
```

**示例的 `main.wasm` 与 `main.wat` 都入库**：示例是随仓库分发的安装包，干净
checkout 上必须不需要 `wat2wasm` / `wasm-tools` 就能安装、能跑测试。只保留
`.wat` 会让 `packageHashes` 指向不存在的文件，安装直接报 `PluginPackageInvalid`。
`.gitignore` 因此对 `plugins/examples/**/main.wasm` 开白名单。

`cargo test` 里的
`plugin_test::example_plugin_wasm_is_reproducible_and_hashes_match` 负责防漂移：
用 `wat` crate 从 `main.wat` 重新编译并与入库的 `main.wasm` 逐字节比对，同时校验
`packageHashes`。改了 WAT 却没重新生成 WASM/哈希，会直接红。修复就是重新跑
`plugin_pack -- build` 并一起提交。

## 示例

- [`examples/style-daily-note`](examples/style-daily-note/README.md)：声明式风格模板。
- [`examples/export-diary-markdown`](examples/export-diary-markdown/README.md)：Wasm
  读取选定日记并导出 Markdown。

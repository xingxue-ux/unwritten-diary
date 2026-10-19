# M4：插件 SDK 与宿主（B6，#88）

状态：**已实现**（schema 14）。本文说明包格式、清单字段、权限模型、沙箱边界、
激活点、示例与**没有做到**的部分。代码入口：

- `crates/diary_core/src/plugin/`：清单/包、声明式模板、Wasm 沙箱、宿主编排；
- `crates/diary_bridge/src/api.rs`：`inspectPluginPackage` / `installPlugin` /
  `setPluginEnabled` / `uninstallPlugin` / `listPlugins` / `pluginTemplates` / `runPlugin`；
- `plugins/examples/`：两个可安装示例；
- `plugins/tools/check_package.py` 与 `tools/plugin-pack`：打包检查/构建工具。

## 1. 包格式

一个插件包是**目录**或**zip**，两者内容一致：

```text
manifest.json          # 必需，必须在包根目录
main.wasm              # entryKind = wasm 时的入口
main.json              # entryKind = declarative 时的入口（模板数据）
assets/...             # 可选资源；每个文件都要在 packageHashes 里
```

硬约束：

| 约束 | 值 |
|---|---|
| 文件数 | ≤ 128 |
| 单文件 | ≤ 8 MiB |
| 解压后总大小 | ≤ 16 MiB |
| `manifest.json` | ≤ 64 KiB |
| 路径 | 相对路径、`/` 分隔、无 `..`/绝对路径/反斜杠/NUL；目录包拒绝符号链接 |
| 哈希覆盖 | `packageHashes` **恰好**覆盖除 `manifest.json` 外的每一个文件；多列/少列/哈希不符都拒绝 |

安装时把包写到资料库根目录的 `plugins/<id>/<version>-<packageHash 前 8 位>/`，先写
`.staging/` 再改名；失败清理暂存。安装/检查**从不执行包内容**：

- Wasm 只检查文件魔数与版本（`\0asm` + version 1），不 `Module::new`、不实例化、
  不调用 `start`；
- 声明式入口只按 JSON 解析模板，不解释执行。

`package_hash` 是对「路径 + 每个文件 sha256」按字典序算出的稳定 sha256；换一个字节
就变，用于升级/重装判断与展示。

## 2. 清单字段（manifestVersion = 1）

| 字段 | 类型 | 说明 |
|---|---|---|
| `manifestVersion` | integer | 固定 `1`；不认识的版本拒绝 |
| `id` | string | 2–64 位小写字母/数字/`.`/`_`/`-`，以字母或数字开头 |
| `name` | string | 1–80 字符 |
| `version` | string | 三段数字，如 `1.0.0` |
| `author` / `license` | string | 非空，≤ 120 字符 |
| `apiRange` | string | `1.0`、`^1.0` 或 `>=1.0 <2.0`；宿主 ABI 版本是 `1.0` |
| `supportedPlatforms` | string[] | `windows` / `linux` / `macos` / `android` / `ios` / `ohos`（`harmonyos` 同义）；必须包含当前平台 |
| `entryKind` | string | `wasm` 或 `declarative` |
| `entryFile` | string | 包内相对路径；wasm 必须 `.wasm`，declarative 必须 `.json` |
| `activationEvents` | string[] | `command:<id>`、`materials_pending:<kind>`、`export.start`、`organize.template.select`；**不支持 startup** |
| `permissions` | string[] | 见第 3 节；未知权限拒绝 |
| `settingsSchema` | object? | 只保存/展示，不据此自动授权 |
| `packageHashes` | object | 相对路径 → sha256（可带 `sha256:` 前缀） |
| `description` / `homepage` | string? | 可选展示字段 |

JSON Schema 在 `plugins/manifest.schema.json`。字段是加性的：本构建忽略不认识的
**顶层**字段，但认识的字段语义不静默改变；新增语义要抬 `manifestVersion`。

## 3. 权限模型

权限分两层：

1. **清单声明**（`permissions`）：插件说自己需要什么；
2. **用户授权**（`plugin_grants` 表）：用户在安装/升级时显式勾选什么。

安装时 `grants` 必须覆盖清单的全部权限，否则报 `permission_denied` 并列出缺项；
授予清单没有声明的权限报 `invalid_argument`。升级时新增权限必须重新出现在这次
`grants` 里，**不默默扩大权限**。卸载删安装行与执行文件，保留日记、原件与已生成
导出物。

| 权限 | 能力 | 越权结果 |
|---|---|---|
| `read_selected_material` | `selected_diary_*` / `source_text` / `asset_range`，只限本次选中的版本/记录/资产 | `permission_denied`，说明缺哪一项 |
| `read_date_range` | `range_diary_*`，只限给定起止 dayKey（闭区间，≤366 天） | 同上 |
| `create_derived_content` | `emit_derived`，候选派生内容 + 来源；核心校验来源在授权范围内 | 同上 |
| `create_export_artifact` | `export_write`，只能写进核心的限域导出目录 | 同上 |
| `invoke_provider:<providerId>` | `provider_invoke`，只限该 id；还要求 #66 里已配置、已启用、能力闸门通过 | 未授权/未配置分别报 `permission_denied` / `provider_not_configured` |

`get_setting` / `put_plugin_state` 对应 `state_get` / `state_put`：键值命名空间按插件
隔离，单值 ≤4 KiB、总配额 16 KiB；不占用上面的功能权限，但也不能借此读库。

## 4. Wasm 运行时与沙箱

### 4.1 为什么是 wasmi

- **纯 Rust 解释器**，不依赖 Cranelift/JIT，不带原生库；Android/Windows 交叉编译
  与其它 Rust 依赖同一条路。
- 许可证 **MIT/Apache-2.0**（wasmi 2.0.0）；`wasmparser` 等传递依赖同为宽松许可。
- 支持 `Config::consume_fuel` 的指令预算与 `StoreLimits` 的线性内存上限，正好覆盖
  任务书要求的可强制预算。
- 代价是比 JIT 慢；插件调用是低频动作（用户命令/导出/整理），可接受。探针实测
  纯计算调用约几十微秒/次（`tools/probe` 的 `plugin-runtime`）。

不选 `wasmtime` 的原因：Cranelift/JIT 体积与原生依赖大，Android 交叉编译与包体
代价高；本片不需要 JIT 性能。

### 4.2 限制

| 限制 | 值 | 实现 |
|---|---|---|
| 指令预算 | 20,000,000 fuel | `Config::consume_fuel` + `Store::set_fuel`；耗尽 → `OutOfFuel` |
| 线性内存 | 32 MiB | `StoreLimitsBuilder::memory_size` + `trap_on_grow_failure` |
| 墙钟 | 5,000 ms | **每个** `unwritten` 宿主函数入口检查 + 调用结束后检查；纯计算死循环由 fuel 兜底 |
| 单次消息/输出/导出物 | 1 MiB | 宿主函数读写前检查 |
| 插件私有状态 | 16 KiB | `state_put` 配额 |
| 派生内容 | 32 条 / 256 KiB | `emit_derived` 配额 |
| 文件系统 / 网络 | **无** | linker 只有 `unwritten` 命名空间；导入 WASI 会实例化失败 |
| Provider | 仅授权且已配置 | `provider_invoke`；密钥由核心保管，提示词/响应有上限 |

**失败隔离**：trap、fuel 耗尽、内存超限、未知导入、宿主函数错误都只结束这一次
调用，返回 `failed` / `timeout` / `budget_exceeded` / `denied` 与用户可读原因；核心
继续工作。宿主代码外面套 `catch_unwind`，宿主 panic 也不会把核心带崩。**不启子进程**：
wasmi 不执行本机代码，子进程会引入平台差异与生命周期问题；进程级 abort（如宿主
自身 OOM）不在可恢复范围，文档如实标注。

**指令预算不替代网络超时**：`provider_invoke` 的 HTTP 请求有自己的 15 秒上限，
并在宿主函数入口检查 5 秒墙钟；两者分开。

## 5. ABI v1

插件导出：

- `memory`（必需）
- `run() -> i32`（必需；0 成功，非 0 失败）
- `abi_version() -> i32`（可选；有就必须是 1）

宿主导入（命名空间 `unwritten`，全部是数值参数，字符串为 UTF-8、指针/长度指向
插件自己的线性内存）：

| 导入 | 签名 | 语义 |
|---|---|---|
| `context_utf8` | `(ptr, cap) -> len` | 触发原因、平台、API 版本、选中 id、范围、格式、预算 |
| `selected_diary_count` | `() -> count` | 选中日记数量（需读选中权限） |
| `selected_diary_day` / `selected_diary_body` | `(index, ptr, cap) -> len` | 按索引读 dayKey / 正文 |
| `range_diary_count` | `() -> count` | 日期范围内有当前版本的日记数量（需范围权限） |
| `range_diary_day` / `range_diary_body` | `(index, ptr, cap) -> len` | 同上 |
| `source_text` | `(id_ptr, id_len, ptr, cap) -> len` | 选中记录里某个来源的文本 |
| `asset_range` | `(id_ptr, id_len, start, len, ptr, cap) -> len` | 选中资产的字节区间（≤64 KiB） |
| `provider_invoke` | `(id..., prompt..., out_ptr, cap) -> len` | 通过核心调用已授权 Provider |
| `emit_derived` | `(kind..., text..., refs_json...,) -> 0` | 候选派生内容；来源必须在授权范围 |
| `export_write` | `(name..., media..., content...) -> 0` | 创建/追加导出物 |
| `state_get` / `state_put` | `(key..., value...)` | 插件私有键值状态 |
| `emit_output` | `(ptr, len) -> 0` | 设置本次输出文本（不落文件） |
| `report_progress` | `(done, total) -> 0` | 受限进度 |
| `log` | `(level, ptr, len) -> 0` | 脱敏诊断日志（截断、限条数） |

返回码：`0`/正数是成功（读函数返回写入字节数）；负数是错误：`-1` 越权、`-2` 找不到、
`-3` 参数非法、`-4` 预算、`-5` Provider、`-7` IO、`-8` 缓冲区太小。插件可以处理
负返回并继续；**只要发生过越权调用，整次运行仍判 `denied`**，不会因为插件忽略错误
就变成成功。

## 6. 激活点

清单声明、调用时精确匹配：

| 激活点 | 触发 | 说明 |
|---|---|---|
| `command:<id>` | 用户执行命令 | 命令参数进 `inputText`（数据，不是指令） |
| `materials_pending:<kind>` | 某类材料待处理 | 由调用方/调度决定何时触发；核心不自动轮询插件 |
| `export.start` | 开始指定导出 | 与 #113 的 `ExportRequest.format` / dayKey 语义一致 |
| `organize.template.select` | 选择某个整理模板 | 可用于风格模板渲染；不改变内置整理模板 |

**启动不执行插件**：

- `Core::open` 只跑数据库迁移与既有恢复，不读 `plugins/` 目录；
- `list_plugins` / `plugin_templates` 只读 `plugin_installations` 的
  `manifest_json` / `templates_json` 与 `plugin_grants`；
- 测试 `install_does_not_execute_wasm_and_startup_only_reads_cache` 把入口文件删掉后
  重开库：`open` 与 `list_plugins` 照常成功，只有 `run_plugin` 才报入口缺失。

## 7. 声明式插件

入口 `main.json` 只有模板：

```json
{
  "schemaVersion": 1,
  "templates": [
    {
      "id": "export.md",
      "kind": "export",
      "label": "按天导出 Markdown",
      "description": "…",
      "format": "markdown",
      "body": "# {{dayKey}}\n\n{{body}}"
    }
  ]
}
```

- `kind`：`style` 或 `export`；`format`：`style_instruction` / `markdown` / `plain_text`；
- 占位符白名单：风格模板 `id/label/lengthPreference/person/tone/emotionalTemperature/
  rhetoricLevel/examples/bannedExpressions`；导出模板 `dayKey/date/title/versionId/body`；
- **单遍替换**：值里的 `{{...}}` 不会再次展开。材料里的「忽略前文」只是数据；
- 样式模板的输入字段与 #114 的 `StyleProfile` 对齐（`PluginStyleProfile`），渲染结果
  是风格偏好文本，不由模板自己扩权或改写历史日记；
- 导出模板读取日期范围内的**当前日记版本**（或用户选中的版本），输出格式与 #113
  的 `ExportRequest.format` 一致，生成 `plugin_exports/<pluginId>/<runId>/<file>`。

## 8. 示例

1. `plugins/examples/style-daily-note/`：声明式风格模板；无权限，`organize.template.select`
   或 `command:style` 激活，把 `StyleProfile` 渲染成中文风格说明。
2. `plugins/examples/export-diary-markdown/`：Wasm 代码插件；`read_selected_material` +
   `create_export_artifact`，读取用户选定的日记版本，生成 `## dayKey` + 正文的
   Markdown，写入 `plugin_exports/`。

构建示例的 Wasm 与重算哈希：

```bash
cargo run -p plugin_pack -- build plugins/examples/export-diary-markdown
cargo run -p plugin_pack -- check plugins/examples/export-diary-markdown
python3 plugins/tools/check_package.py plugins/examples/export-diary-markdown
```

示例的 `main.wasm` 与 `main.wat` **都入库**：示例随仓库分发，干净 checkout 上
不装 `wat2wasm` / `wasm-tools` 也必须能装、能测；只有 `.wat` 会让清单指向不存在的
入口而报 `PluginPackageInvalid`。防漂移由
`plugin_test::example_plugin_wasm_is_reproducible_and_hashes_match` 保证：`cargo test`
里用 `wat` crate 从 WAT 重新编译、与入库 WASM 逐字节比对，并逐个校验
`packageHashes`。改了 WAT 未重新生成 WASM/哈希即红；修复后把三个文件一起提交。

## 9. Schema 13 → 14 迁移

`schema.rs` 的 `SCHEMA_VERSION` 现在是 **17**：本节记录的是插件引入时的 v13 → 14；v15 是 #128 复审的加性改动（`index_meta.auto_rebuild_marker` + 整代口径覆盖索引），v16 留给并行的回收站线，v17 是 `styles` 风格版本表。见 [`m2-向量索引与换代.md`](m2-向量索引与换代.md) 与 `schema::V15` / `schema::V17`。插件引入的 `V13` 迁移是：

```sql
CREATE TABLE IF NOT EXISTS plugin_installations (...);
CREATE TABLE IF NOT EXISTS plugin_grants (...);
```

- **不回填**：升级前没有插件，迁移后两张表为空；不替用户装任何插件、不预设授权；
- 守卫是「版本号 < 13 **或**两张表任一不存在」，建表写 `IF NOT EXISTS`，所以
  「版本号已经 13、但只缺一张表」的库也能补齐且幂等；
- 外键 `plugin_grants.plugin_id → plugin_installations.id ON DELETE CASCADE`；
- 旧库测试见 `plugin_test::schema_13_migrates_to_14_with_plugin_tables_and_no_backfill`
  与既有的 v10→latest 迁移测试（已更新为 14）。

## 10. 与 #113 / #114 的接缝

#113（可阅读导出）与 #114（风格系统）的 **Rust 核心已经落地**（schema v17 的
`styles` 表；`exports.create` 同步生成 Markdown/纯文本；风格快照指向不可变版本行）。
插件这一片仍然没有另造一套：

- 风格模板的输入 DTO 与契约 `StyleProfile` 字段一一对应，且现在就是 `styles` 表的落库
  对象（核心类型统一为 `StyleProfile`；`PluginStyleProfile` 只是旧名别名）；
- 导出模板/代码插件的格式字符串（`markdown` / `plain_text`）与 dayKey 闭区间语义对齐
  `ExportRequest`；
- 插件导出产物仍是同步返回的 `PluginRunResult.output` + `PluginArtifact`，与
  `exports.create` 的核心管理导出目录是**两条路**：前者归插件、后者归核心；
- 桥接适配层已接上插件管理（inspect/install/enable/disable/uninstall/list）；
  `DiaryApi.invokePlugin` 是「返回 jobId」的票据形状，而核心 `run_plugin` 同步返回完整
  结果，两者还没任务化对齐，因此 `plugins.invoke` 仍不在 Dart 适配层的能力清单里；
  完整结果可以通过生成的 `BridgeSession.runPlugin` 直接取。

## 11. 没有做到的（如实）

1. **插件市场、签名与分发**：没有在线市场、没有包签名/发布者身份链；当前只校验
   哈希与结构，不证明「这个包来自谁」。
2. **跨设备移植验证**：示例包用同一份清单/字节可在 Windows/Android/Linux 安装，但
   本机只实际跑了 Linux；两端真机安装未验证。
3. **插件直接联网**：没有 socket/WASI；只能走已配置且用户授权的 Provider。
4. **完整自定义页面、原生动态库、无限制脚本**：按任务书后置，未实现。
5. **`exports.create` 仍是同步的**：核心已生成文件 + 写一条 `succeeded` 的 export 任务
   记录，但没有「排队 → 执行 → 进度/取消」的异步任务链；`includeAssets` 也还没实现。
6. **子进程隔离**：使用 wasmi + `catch_unwind`；进程级 abort（宿主 OOM 等）不可恢复。
7. **插件市场里的自动更新**：升级是用户显式安装；没有后台自动更新。
8. **多插件并发**：调用是同步的，没有并行调度；插件任务化（崩溃恢复、取消正在跑的
   调用）仍是后续工作。停用后新调用被拒；已经开始的同步调用没有抢占式中断。

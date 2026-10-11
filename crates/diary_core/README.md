# diary_core

设备内本地核心。当前完成到 **B5**：资料库、迁移、幂等写入、记录路径、原件文件库与导入交接、录音会话与任务队列、提取与来源定位，以及任务驱动与后台补做。

设计决定与未完成项记在 `docs/architecture/`：[m1-存储与幂等](../../docs/architecture/m1-存储与幂等.md)、[m1-原件文件库](../../docs/architecture/m1-原件文件库.md)、[m1-录音与队列](../../docs/architecture/m1-录音与队列.md)、[m2-提取与定位](../../docs/architecture/m2-提取与定位.md)、[m4-任务调度与补做](../../docs/architecture/m4-任务调度与补做.md)。

## 已实现

| 能力 | 说明 |
|---|---|
| SQLite schema v1 → v2 + 迁移 | 版本记在 `PRAGMA user_version`，迁移在事务里，失败整体回退；旧构建遇到更高版本直接拒绝打开 |
| 记录路径 | `create_draft` / `save_draft` / `commit` / `get_capture` / `list_captures` |
| 原始文字修订 | `revise_text`：新建版本并保留旧版本与父版本引用 |
| 幂等 | `operation_receipts` + 请求指纹；重复提交返回原结果，换内容报冲突 |
| 乐观锁 | `expectedRevision` 不匹配报 `revision_conflict`，不静默覆盖 |
| 域事件 | `domain_events` 与业务写入同事务 |
| 原件文件库 | 内容按 sha256 存放与去重；`imports.prepare` / `finish` / `status` / `cancel` |
| 资产租约 | `assets.open` / `release`，内存态 + 过期回收；文件丢失如实报 `asset_missing` |
| 导入恢复 | 重开资料库时把未完成的导入标成 `recoverable`，不假装成功 |
| 录音会话 | `prepare_recording` / `register_segment` / `update_recording_state` / `finalize_recording` / `recover_recording`；片段按序号幂等，最终化流式拼成逻辑音频 |
| 任务队列 | 入队、按优先级领取、成功/失败/退避重试、手动重试、取消、下次唤醒时刻、通知去重；`commit_with_jobs` 保证提交与入队同事务 |
| 任务驱动 | `run_due_jobs`：按 `kind` 派活（现在只有 `extract`）、失败退避或放弃、回收上次没跑完的任务；业务键去重、`job.changed`、`job_status`（各状态计数 / 到期积压 / 最近错误）；补做口径见 m4 文档 |
| 错误码 | 映射到契约第 7 节，含 `SQLITE_FULL` → `storage_full`、ENOSPC → `storage_full` |
| 时间精度 | 所有对外返回的时间统一截断到毫秒，保证「写进去的」和「读出来的」相等 |
| 提取 | `extract_source`：纯文本（含 GBK 等编码检测）、Markdown、DOCX、文本 PDF、扫描件与无能力类型的如实降级；结果可重建 |
| 来源定位 | `locate_source`：把来源 id 或修订 id 解析成可打开的原件与可用性 |

## 还没实现

索引任务化与真机排期（#19）、语义/向量检索、转写与 OCR 真实能力（B4）、日记生成、插件、备份恢复，以及大部分桥接接线。

## 校验

```bash
cargo test -p diary_core        # 106 项通过（另有 1 项 10 万片段量测默认忽略）
cargo clippy -p diary_core --all-targets -- -D warnings
```

MSRV 是 workspace 里钉的 1.94（见根 `Cargo.toml`）。大文件测试只在 Linux 上跑（要读 `/proc/self/status` 的峰值内存）。
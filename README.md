# 不写日记

本地优先的 AI 日记应用：打开就能记，记录不等待网络，原件永远保留。

首版目标平台为 **Windows x64** 与 **Android arm64**，两端各自独立资料库，不做自动同步。建议技术栈为 Flutter/Dart 前端 + Rust 设备内本地核心，通过 `flutter_rust_bridge` 对接。

> 当前状态：**计划阶段**。仓库中只有开发计划文档，尚无应用代码、无可运行构建、无实测性能数据。

## 产品要点

- 记录快、保存可靠：保存不等待网络、AI、索引重建或插件初始化。
- 文字、语音、附件共用一条记录流程，不强制先选记录模式。
- 默认安静记录，只有用户主动请求时才让 AI 回应。
- AI 自动整理出简洁、有感染力、忠于事实的**自然段正文**，不写分点总结；允许润色，不许虚构。
- 修改保留版本，自动生成结果绝不覆盖人工编辑。
- 搜索统一覆盖日记与原始材料，混合关键词与本地向量检索，并能定位回原件。

## 开发计划文档

| 文件 | 用途 | 阅读人 |
|---|---|---|
| [docs/development/00-开发总览.md](docs/development/00-开发总览.md) | 产品要求、建议架构、代码分层与目录职责、M0–M5 阶段划分 | 双方 |
| [docs/development/01-共享接口契约.md](docs/development/01-共享接口契约.md) | DiaryApi / PlatformHost、数据对象、状态机、事件、错误码 | 双方必读 |
| [docs/development/02-前端开发任务书.md](docs/development/02-前端开发任务书.md) | Flutter 界面、平台能力、F0–F8 任务 | 前端 |
| [docs/development/03-后端开发任务书.md](docs/development/03-后端开发任务书.md) | Rust 本地核心、存储、检索、模型、队列、插件、B0–B8 | 后端 |
| [docs/development/04-验收与测试计划.md](docs/development/04-验收与测试计划.md) | E01–E38 验收案例、性能预算、质量门槛、发布阻断 | 双方 |
| [docs/development/05-模型交接提示词.md](docs/development/05-模型交接提示词.md) | 按任务复制给 AI 模型的开工说明 | 认领任务的人 |

接口契约在 **M0 冻结**，任何新增字段先更新契约文档和场景样例，再改实现。

## 目录规划（尚未创建）

```
apps/diary_app/          前端：界面、导航、交互、应用壳
packages/platform_host/  前端：录音、播放、文件选择、系统凭据、后台唤醒
packages/diary_api/      后端维护契约，前端使用：Dart DTO 与抽象接口
packages/diary_mock/     前端：DiaryApi 的确定性模拟实现
packages/diary_bridge/   后端：Rust 桥的 Dart 适配及生成文件
crates/diary_core/       后端：数据、业务规则、搜索、队列、模型、插件
crates/diary_bridge/     后端：对 Flutter 的薄接口与生成入口
models/manifest/         后端：模型版本、哈希、许可证、预处理描述
plugins/examples/        后端：模板插件、导出插件、宿主示例
tests/fixtures/          双方共用虚构资料与接口场景
tests/e2e/               前端：跨层用户流程与平台验收
docs/architecture/       后端起草、双方确认：架构决定与接口变更记录
```

## 参与开发

协作流程见 [CONTRIBUTING.md](CONTRIBUTING.md)：**任务认领制**、分支模型、互相审查、契约变更流程。我们不预设分工，F/B 任务书只区分代码层，不区分人。

- 任务以 issue 为载体（用「任务认领」模板创建），**开工前先把自己设为 Assignee**；一个 issue 对应一个分支、一个 PR。
- 开工前先看所有 open PR 的改动文件，避免两个人同时改同一个文件。
- `main` 已开启分支保护，**任何改动都走 PR**，不允许直接推送。
- 每个 PR 按 `.github/pull_request_template.md` 填写交付信息，空模板不予合并。
- 发现接口缺口用「契约变更请求」issue 模板，不要为了绕过缺口直接访问数据库。

## 许可证

本项目采用 [MIT 许可证](LICENSE)。

依赖与模型各有自己的授权。主仓库采用 MIT 不代表内置向量模型、转写服务和第三方依赖可以随意再分发，各自的许可证与再分发条件记录在 `models/manifest/` 与 M0 的依赖清单中。
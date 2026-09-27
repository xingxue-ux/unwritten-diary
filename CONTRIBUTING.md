# 协作规范

本文约定两个人（各自带着一个 AI 模型）如何在同一个仓库里并行开发。产品与技术决策以 `docs/development/` 下的计划文档为准，本文只规定流程。

## 1. 目录单一所有权

冲突不靠沟通避免，靠不让两个人碰同一个文件避免。

| 目录 / 文件 | 唯一负责人 |
|---|---|
| `apps/diary_app/`、`packages/platform_host/`、`packages/diary_mock/`、`tests/e2e/` | 前端 |
| `crates/diary_core/`、`crates/diary_bridge/`、`packages/diary_api/`、`packages/diary_bridge/`、`models/manifest/`、`plugins/examples/`、`tests/fixtures/` | 后端 |
| `docs/development/01-共享接口契约.md` | 后端（前端只提变更请求，不直接改） |
| `Cargo.lock`、桥接生成文件 | 后端 |
| `pubspec.lock`、`apps/diary_app/` 下的平台工程配置 | 前端 |

明确禁止：

- 前端改 `crates/` 与桥接生成文件；后端改页面与平台工程。
- 任何一方为了绕过缺口，在页面里直接访问数据库、Rust 内部对象或某个模型服务的 HTTP API。
- 手改生成代码。生成脚本由后端维护。

## 2. 分支

| 分支 | 用途 |
|---|---|
| `main` | 只放能演示当前阶段闭环的代码，已开启保护 |
| `frontend` / `backend` | 两端的长期分支 |
| `feat/<任务编号>-<简述>`、`fix/`、`docs/` | 特性分支，建议当天合回长期分支 |

`main` 的保护规则：必须走 PR、禁止 force push、禁止删除、管理员同样受限。

## 3. 互相审查

当前 `main` 的必过审查数是 **0**，即 PR 可以由作者自己合并。这是 M0 期间的临时状态：两个人一起搭地基，不该互相卡。

在这种情况下，"互相审查"靠下面三条约定执行：

1. **跨层改动必须请对方 review**：改了 `01-共享接口契约.md`、对方的目录、`Cargo.lock` / `pubspec.lock`、桥接生成文件。
2. **本层内部改动可以自己合并**，但仍要走 PR，留下可追溯的记录。
3. 审查要真的看 diff。带着 AI 干活时，橡皮图章式批准比不审查更危险——它制造了"已经有人看过"的假象。

**待办**：等 M0 结束、前后端分工确定后，加入 `CODEOWNERS` 并开启「必须 code owner 审查」。届时下列路径的 PR 将由平台强制要求对应负责人批准，而不是靠自觉：

- `docs/development/01-共享接口契约.md`
- `crates/`、`packages/diary_api/`、`packages/diary_bridge/`（后端）
- `apps/`、`packages/platform_host/`、`packages/diary_mock/`（前端）
- `Cargo.lock`、`pubspec.lock`、生成文件

注意 GitHub 不允许作者批准自己的 PR，所以"必须 1 人批准"天然等于"必须对方批准"。另外仓库已开启 `dismiss_stale_reviews`：批准后又推新提交，批准会自动作废，需重新审查。

## 4. 契约变更流程

`docs/development/01-共享接口契约.md` 是前后端唯一的同步点，接口在 M0 冻结。

1. 发现缺口的一方提 issue（用「契约变更请求」模板），写清缺少的方法/字段、触发场景、建议语义、对现有调用的影响。
2. **先改契约文档和场景样例，再改实现。**
3. 另一方以调用方身份审核，确认页面能不能落地。
4. 双方确认后，后端改 `packages/diary_api` 类型包，前端改调用。
5. 变更记入 `docs/architecture/`。

## 5. 提交信息

```
<类型>(<任务编号>): <简述>
```

类型用 `feat` `fix` `docs` `refactor` `test` `chore` `contract`；任务编号用计划文档里的 F0–F8 / B0–B8，没有就省略括号。

```
feat(F2): 记录页支持文字与录音混合追加
contract(B3): 明确 search.start 的 queryRevision 语义
```

## 6. 和 AI 模型协作的三条铁律

1. 每次任务只喂对应那一份任务书 + `01-共享接口契约.md`。不要把前端任务书给后端模型，反之亦然。
2. 每次都要重申：不许改对方目录，缺接口走契约变更流程。模型会"顺手帮你补上"，不重申就会越界。
3. 共享文件（`Cargo.lock`、`pubspec.lock`、桥接生成文件、`README.md`、`.gitignore`）指定单一负责人，禁止模型自行更新。

## 7. 交付底线

- 每个阶段必须有一端能演示真实闭环。**不能只用 Mock 验收真实保存，不能只用模拟器结论代替真机。**
- 未运行、仅 Mock、仅模拟器、仅桌面验证的项目，必须在 PR 里分别标明。
- 缺少实机、凭据或构建环境时如实标注，完成其余可独立验证的部分，不得把未验证写成已完成。
- 不为凑数字重复执行无意义测试，把时间用在解决剩余风险上。
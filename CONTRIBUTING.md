# 协作规范

本文约定两个人（各自带着一个 AI 模型）如何在同一个仓库里并行开发。产品与技术决策以 `docs/development/` 下的计划文档为准，本文只规定流程。

**我们不预设分工。** 任务书里的 F0–F8 / B0–B8 只说明任务属于哪一层代码，不说明属于哪个人。任何任务谁都可以认领，同一个人也可以同时做前端层和后端层的任务。所以冲突不靠"不让两个人碰同一个目录"来避免，而靠**认领可见 + 同一文件同时只出现在一个 PR 里**来避免。

## 1. 任务认领

1. 任务的载体是 issue，用「任务认领」模板创建，标题带任务编号（如 `[F0] Flutter 双平台壳`）。
2. 开工前把**自己设为 Assignee**，并留一句打算怎么做。没有 Assignee 的 issue 才是还在招人的 issue。
3. 一个 issue 对应一个分支、一个 PR，PR 里写 `Closes #<issue 号>`。
4. 同时认领不超过 2 个未完成 issue。认领后当天没有进展，就在 issue 里说一声，别让它在看板上看起来有人在动。
5. 想做的事没有对应 issue：先按模板提一个（可以自提自领），再动手。

### 认领前必须做的一次检查

不再按目录分人之后，"撞车"只会发生在文件这一层：

- 打开所有 open PR，看它们的「改动文件」列表；
- 自己要动的文件如果已经出现在别的 PR 里，先在该 PR 或 issue 下评论协调，**等它合并后再切分支**，或者两人当场约定谁先做；
- 不要靠合并时解冲突，也不要为了避开冲突复制一份代码。

## 2. 代码分层与硬性边界

前后端在这里是**代码层**，不是**人**。同一个人可以改两边，但层间边界依然要守。

| 路径 | 所属层 | 职责 |
|---|---|---|
| `apps/diary_app/`、`packages/platform_host/`、`packages/diary_mock/`、`tests/e2e/` | 前端层 | 界面、导航、交互、页面状态、应用壳、系统能力 |
| `crates/diary_core/`、`crates/diary_bridge/`、`packages/diary_bridge/`、`models/manifest/`、`plugins/examples/` | 后端层 | 本地核心、桥接适配、模型清单、插件示例 |
| `packages/diary_api/` | 跨层 | 与 Rust 无关的 Dart DTO 和抽象接口，只按共享契约修改 |
| `tests/fixtures/` | 跨层 | 共用虚构资料与接口场景 |
| `docs/development/01-共享接口契约.md` | 跨层 | 契约由正在改它的人在 PR 里更新，另一人审核 |
| `docs/architecture/` | 跨层 | 架构决定与接口变更记录，起草人负责，另一人确认 |
| `Cargo.lock`、桥接生成文件 | 后端层 | 由实际运行生成/构建命令的人更新 |
| `pubspec.lock`、`apps/diary_app/` 下的平台工程配置 | 前端层 | 由实际运行包管理器的人更新 |

与谁执行无关的硬性禁止：

- 页面或插件为了绕过接口缺口，直接访问数据库、Rust 内部对象或某个模型服务的 HTTP API。
- 手改生成代码（`packages/diary_bridge/` 等生成输出）。需要重新生成时，由改契约的人在同一个 PR 里跑生成脚本并一起提交。
- 锁文件只能由实际运行包管理器的人更新，且必须在 PR 里说明原因。
- 同一个锁文件、同一份生成输出、同一段共享文档，同时只能出现在一个 PR 里。

共享文件（`README.md`、`.gitignore`、本文件、`00-开发总览.md`、`.github/` 模板）小步走 PR，不夹带无关修改。

录音是必须明确的边界：前端平台层负责实际收音、录音前台服务和可恢复暂存；后端层负责录音会话登记、片段接收、转写和索引。界面被销毁不应导致平台录音依赖的对象同时消失。

各层内部的详细职责见 `00-开发总览.md` 第 5 节、`02-前端开发任务书.md`、`03-后端开发任务书.md`。

## 3. 分支

| 分支 | 用途 |
|---|---|
| `main` | 只放能演示当前阶段闭环的代码，已开启保护 |
| `feat/<任务编号>-<简述>`、`fix/<简述>`、`docs/<简述>`、`chore/<简述>` | 从最新 `main` 切出，完成后尽快合回 `main` |

不设 `frontend` / `backend` 长期分支：没有固定分工，长期分支只会变成没人负责的第二个 `main`。

`main` 的保护规则（已开启）：必须走 PR、必须通过必过检查、必须 1 人批准（见第 4 节）、禁止 force push、禁止删除、管理员同样受限（`enforce_admins`）。合并后分支自动删除。

## 4. 互相审查与机器门禁

`main` 上现在有两道门禁，PR 必须同时通过：

1. **必过检查 6 项**（都在 `.github/workflows/check.yml`，PR 上必须全绿）：
   - `docs`：关键文件存在、Markdown 相对链接有效、`.github` 下的 YAML 可解析、共用场景 JSON 合法；
   - `dart`：`packages/diary_api` 静态分析（`--fatal-infos`）与单元测试；纯 Dart 的 `packages/diary_mock` 也在这里跑（它随 F0 一起进来）；
   - `rust`：`clippy --workspace --all-targets -- -D warnings` 与全 workspace 测试；
   - `bridge`：构建桥接产物、`packages/diary_bridge` 静态分析与端到端调用测试；
   - `flutter`：`apps/diary_app` 的静态分析（`--fatal-infos`）与组件测试；
   - `probe`：插件运行时预算与权限、插件代码的 Android arm64 编译检查、中文短词检索的结论断言。
2. **必过审查 1 人**：GitHub 不允许作者批准自己的 PR，所以"必须 1 人批准"天然等于"必须对方批准"。已开启 `dismiss_stale_reviews`（批准后又推新提交，批准作废需重审）和 `required_conversation_resolution`（未解决的评论必须先处理才能合）。

审查要真的看 diff。带着 AI 干活时，橡皮图章式批准比不审查更危险——它制造了"已经有人看过"的假象。跨层的改动除了点批准，还要在 PR 里写清「需要对方接入的内容」。

原计划里的 `CODEOWNERS` 在认领制下不再适用（它按固定目录指派负责人），取消。

紧急情况（`main` 已经坏掉、挡住两个人）只走回滚，见第 9 节。**回滚 PR 是唯一的最高优先级**：看到就停下手里的事先审。

## 5. 契约变更流程

`docs/development/01-共享接口契约.md` 是前后端唯一的同步点，接口在 M0 冻结。

1. 发现缺口的一方提 issue（用「契约变更请求」模板），写清缺少的方法/字段、触发场景、建议语义、对现有调用的影响。
2. **先改契约文档和场景样例，再改实现。**
3. 由不在该 PR 里的另一人审核，确认调用方能不能落地。
4. 双方确认后，改 `packages/diary_api` 类型包与前端调用点。可以由同一人一次做完，也可以一人改类型包、一人改调用，但都要引用同一个 issue。
5. 变更记入 `docs/architecture/`。

## 6. 提交信息

```
<类型>(<任务编号>): <简述>
```

类型用 `feat` `fix` `docs` `refactor` `test` `chore` `contract`；任务编号用计划文档里的 F0–F8 / B0–B8，没有就省略括号。

```
feat(F2): 记录页支持文字与录音混合追加
contract(B3): 明确 search.start 的 queryRevision 语义
```

## 7. 和 AI 模型协作

按任务喂文档，不按人喂。这两个模型都不固定属于前端或后端。

1. 一次只让模型做一个已认领的 issue。固定喂 `00-开发总览.md` + `01-共享接口契约.md` + 该任务对应的任务书（前端层任务给 `02`，后端层任务给 `03`）+ `04-验收与测试计划.md` 里的相关案例，不要两份任务书一起塞。
2. 每次都要重申：不许改契约、不许手改生成文件、缺接口走契约变更流程。模型会"顺手帮你补上"，不重申就会越界。
3. 一次只开一个分支、一个 PR。让模型同时改前端层和后端层时，明确列出允许改动的文件范围。
4. 共享文件（`Cargo.lock`、`pubspec.lock`、桥接生成文件、`README.md`、`.gitignore`）指定由本次认领的人负责，禁止模型自行更新。
5. 交接提示词见 `05-模型交接提示词.md`，按任务复制，不按人复制。

## 8. 交付底线

- 每个阶段必须有一端能演示真实闭环。**不能只用 Mock 验收真实保存，不能只用模拟器结论代替真机。**
- 未运行、仅 Mock、仅模拟器、仅桌面验证的项目，必须在 PR 里分别标明。
- 缺少实机、凭据或构建环境时如实标注，完成其余可独立验证的部分，不得把未验证写成已完成。
- 不为凑数字重复执行无意义测试，把时间用在解决剩余风险上。

## 9. 回滚与事故处理

`main` 禁止 force push、禁止删除，且 `enforce_admins` 开着：**历史不能改写**。回滚的唯一正规方式是往前加一个反向提交，并且同样走 PR、同样过必过检查和 1 人批准。

```bash
gh pr revert <PR 号>      # 自动建分支并开 revert PR，正文可写 -b "原因"
```

手动等价做法：从 `main` 切分支，`git revert <sha>`，然后正常开 PR。我们全部用 squash 合并，`main` 上没有 merge commit，所以不需要 `-m 1`。只回滚一个 PR 里的部分文件：`git revert -n <sha>`，再 `git restore --staged --worktree <要保留的文件>`，最后提交。

回滚 PR 的约定：

- 标题用 `revert: ...`，正文写清回滚原因、影响的 issue，必要时重新打开原 issue。
- 生成代码和锁文件必须一起恢复或重新生成，不许手改。
- 与正在改同一文件的 PR 会冲突，先在那边说一声。
- 回滚 PR 优先级最高，看见就审。

revert 救不了的两类事故：

- **数据层**：SQLite 迁移、原件删除、资料库损坏。只能靠备份恢复（验收计划 E34/E35）。所以迁移尽量写成附加式、可向前修复，出事后是"向前修 + 从备份恢复"，不是退代码。
- **密钥泄露**：revert 不会把内容从历史里移除。正确顺序是先轮换密钥，再通知另一人重新 clone，最后才考虑重写历史（需要临时放开分支保护，代价很大）。仓库已开启 secret scanning 与 push protection，目标是在推送前就拦下。

出事后先在 issue 里记录时间线、影响范围和当前状态，再动手。不要把"回滚了"当成"修好了"。
## 10. 本机环境与常见坑

这一节只写**这台开发机（Windows + WSL2）上反复踩到、且症状不像自己原因**的坑。每条都给出症状与处理，省掉重新排查的时间。

### 10.1 「真机」的口径

- **日常开发用 x86_64 模拟器当真机替身**：功能、交互、崩溃、权限、日志都按真机标准走，并留证据（截图 + `logcat`）。
- **arm64 镜像在这台机器上跑不了**：现代 Android Emulator 拒绝跨架构，实测直接启动失败——
  `FATAL | Avd's CPU Architecture 'arm64' is not supported by the QEMU2 emulator on x86_64 host.`
  所以 arm64 的**运行**行为（`.so` 加载、ORT 退出、真实延迟/内存/耗电）在这台机器上无法自动化验证。
- **真机验收留到开发完成后人工走一遍**，清单见 issue「安卓真机人工验收清单」。
- 模拟器数字**不能当性能基线**，只能用于「同一台机器上 A 比 B 快」这类相对比较；PR 里必须按 CONTRIBUTING 第 8 节分别标明验证范围。

### 10.2 安卓构建失败：Gradle **不读**环境变量里的代理

**症状**：`flutter build apk` 报 `Could not resolve com.android.tools.build:gradle` 或
`Could not HEAD 'https://dl.google.com/dl/android/maven2/...'`，而且**要等十来分钟才失败**（一直在重试）。

**原因**：JVM 不认 `http_proxy`/`https_proxy`；`GRADLE_OPTS` 传给 Flutter 起的 Gradle 也不生效。本机直连不通、只有本地代理可用。

**处理**：写进 Gradle 自己的配置（`~/.gradle/gradle.properties`），然后**重启 Gradle 守护进程**：

```properties
systemProp.http.proxyHost=127.0.0.1
systemProp.http.proxyPort=7890
systemProp.https.proxyHost=127.0.0.1
systemProp.https.proxyPort=7890
systemProp.http.nonProxyHosts=localhost|127.0.0.1
```

```bash
pkill -f "[G]radleDaemon"   # 见下：方括号不能省
```

**注意**：必须写成 `[G]radleDaemon`。写成 `"GradleDaemon"` 时，`pkill -f` 会把**执行这条命令的 shell 自己**也匹配上（它的命令行里含这个字符串），于是任务被无声地 SIGTERM 掉——看起来像「命令跑着跑着没了」。

首次安卓构建还会顺带下载 SDK Platform 与 Gradle 依赖，慢是正常的。

### 10.3 编译报 `Cannot allocate memory (os error 12)`

**症状**：编 Android 目标时 `failed to build archive at .../*.rlib: Cannot allocate memory`，看起来像代码或工具链问题。

**原因**：内存压力。模拟器 + Gradle 守护进程 + 多个 cargo 构建同时占着，`available` 可能只剩几 GB。

**处理**：先 `pkill -f "[G]radleDaemon"` 放掉守护进程，再用 `-j 2` 降低并行度重编。

### 10.4 `.so` 与 Dart 的 frb 绑定**必须同源**

**症状**：应用装上去启动就失败；底层异常是

```
StateError: Bad state: Content hash on Dart side (…) is different from Rust side (…),
indicating out-of-sync code.
```

**原因**：`flutter_rust_bridge` 对 Dart 侧与 Rust 侧做内容哈希校验。**混用不同提交的构建产物**（例如拿了另一条分支编的 `.so` 塞进当前分支的 APK）就会触发。

**规则**：`cargo ndk ... -p diary_bridge` 与 `flutter build` 必须在**同一个提交**上做。重新生成桥接代码时，生成命令与产物要放在同一个 PR 里（第 4 节已要求）。

### 10.5 WSL2 挂载盘（`/mnt/e`）上的偶发失败与本机工具链怪癖

这些**不是代码问题**，判断标准是「换到 ext4 就稳定」：

- **全量 `cargo test` 偶发某个测试二进制段错误**：把产物目录放到 ext4 再跑，例如
  `CARGO_TARGET_DIR=/tmp/diary-target cargo test --workspace`。单独跑那个测试通过、放到 ext4 连跑多次通过，就说明是挂载盘的问题。
- **`flutter analyze --fatal-infos` 偶发以 255 退出**并报 `FormatException: Unexpected end of input`（解析分析器 JSON 时崩）。以 CI 的 `flutter` 检查为准，本机重跑一次即可。
- **`NO_PROXY` 里带 `[::1]`** 会让 Python 的 httpx 报 `Invalid port: ':1]'`。跑 Python 工具时显式覆盖：
  `NO_PROXY=127.0.0.1,localhost no_proxy=127.0.0.1,localhost python3 …`。

**通用原则**：本机失败先看「是不是挂载盘/工具链」，再怀疑改动；但**不要把本机通过当验收**——最终以 CI 与（开发完成后的）真机为准。

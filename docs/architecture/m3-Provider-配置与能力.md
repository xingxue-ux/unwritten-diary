# M3 · Provider 配置与能力（B4a）

issue [#66](https://github.com/xingxue-ux/unwritten-diary/issues/66)。这一片只做「**能可靠调用一个已被用户
配置并验证的服务**」：配置模型、密钥托管、能力协商与门控、连接测试、错误归一化、可注入传输层，
以及「默认不联网」这条硬约束。**不做**日记正文生成与提示词（B4b），也不做插件模型能力（M4）。

代码在 `crates/diary_core/src/provider/`（`mod.rs` 配置与门控、`secret.rs` 密钥托管、
`transport.rs` 传输层、`adapter.rs` 协议适配器、`normalize.rs` 错误归一化、
`endpoint.rs` 本机 endpoint 判定、`presets.rs` 内置服务预设、`models.rs` 模型列表检测），
契约形状见 [共享接口契约](../development/01-共享接口契约.md) 第 2.7、4.3、4.4、7 节。
队列语义见 [M4 · 任务调度与补做](m4-任务调度与补做.md)。

## 定下的决定

| 决定 | 口径 | 为什么 |
|---|---|---|
| 配置表叫 `providers`，列名对齐契约 DTO | `adapter_id` ↔ `adapterId`、`endpoint` ↔ `endpoint`、`declared_capabilities` / `verified_capabilities` / `credential_ref` / `enabled` / `test_status` | 桥接层是字段级镜像，改名只会多一层翻译与一处不一致的机会（issue 正文里的 `kind` / `base_url` 是口语说法，落地以契约为准） |
| schema **v8 → v9**，只加两张表 | `providers`、`ai_responses`；**不回填** | 升级前没有任何 Provider 配置，替用户猜一个服务填进去是错的（见下「迁移」） |
| 能力集合是契约第 2.7 节的三项 | `generate_text` / `transcribe_audio` / `understand_image`；**内置适配器只实现前两项** | 契约要求「至少区分」这三项；没实现的不假装（`understand_image` 留给 M4 的插件模型能力） |
| 只内置一种 HTTP 协议 | `openai_compatible`（Chat Completions + Audio Transcriptions 的兼容协议） | 任务书要求「先内置一种有公开文档的常用协议，供应商不硬编码进业务层」。很多本机/局域网服务也照这个协议做 |
| 适配器有**两种形态** | HTTP（`ProtocolAdapter`，经 `Transport` 出网）与**本地引擎**（`LocalEngine`，只读本机库与模型目录，**不可能出网**）；`Adapter` 枚举是统一入口 | 本地引擎没有 URL/密钥/出站调用，硬塞进 HTTP trait 会把「默认不联网」变成一句注释。#91 落地的 `sherpa_onnx` 是第一种本地引擎，口径见 [M3 · 转写链](m3-转写链.md) |
| 传输层可注入 | `Transport` trait；产品默认 `UreqTransport`，测试注入回放/记账/panic 替身 | CI 不联网也要能跑，而且「有没有真的发生调用」要能被**数出来** |
| **只有三处会出网** | 用户显式的 `providers.test`、用户显式的模型列表检测（`list_provider_models`），与真的被领取执行的 Provider 作业（`respond`） | 「默认不联网」不是靠约定，是靠没有别的调用点。三处都是**用户显式发起**或**任务真的被领取**，没有启动探测/后台预热 |
| 没有配置/启用任何服务时**一个请求都不发** | 排任务入口先过能力闸门；队列在领取前再过一次 | 排一条注定失败的任务既费流量又骗用户 |
| 内置服务预设**只是填表草稿** | `presets.rs` 的静态表：endpoint / model / 能力 / 文档链接 + `verified`；不含密钥、不写库、不出网 | 让用户少抄错一个 endpoint，但不替用户做「保存 / 启用 / 先测一次」的决定；未验证的预设如实标 `verified = false` |
| 模型列表检测**只对 `openai_compatible`** | `GET {endpoint}/models`，解析 `{"data":[{"id":"..."}]}`；别的适配器如实报「不支持列举模型」 | 内置适配器里只有它有这样的接口；本地引擎的 `model` 是目录，没有远端可查。猜格式比报错更糟 |
| **本机服务免密钥校验（2026-10-21 产品决定）** | 严格 loopback（`127.0.0.0/8` / `::1` / `localhost`）的 endpoint：`providers.test` 与模型检测不要求凭据；有就带 `Authorization`，没有就不发。私有网段不算本机 | 本机服务通常不校验密钥，凭据可以形式上保留。放行只针对我们自己的前置检查；服务器返回 401 / 超时仍如实报错，不假绿 |
| **不做「保存前试连 / 临时凭据」入口** | 保持「先 `providers.save` 再 `providers.test`」；未保存草稿 + 临时凭据不做 | 临时凭据要多一条密钥生命周期（内存里传、何时擦除、能否被日志抓到）；填错 endpoint 的代价只是「保存 → 测失败 → 改 → 再保存」 |

## 配置模型

`providers` 表（v9）：

| 列 | 含义 |
|---|---|
| `id` | 不透明 ID（`prv_` + 32 位十六进制），由核心生成 |
| `label` | 显示名 |
| `adapter_id` | 协议适配器（当前只有 `openai_compatible`） |
| `endpoint` | 服务地址，含 API 版本前缀（例如 `http://127.0.0.1:11434/v1`）。只接受 `http` / `https` |
| `model` | 模型名（生成/转写各自一个服务配置）。**本地引擎这条路上它是「模型目录」**（见 [M3 · 转写链](m3-转写链.md)），留空表示用随包/环境变量解析出的默认目录 |
| `declared_capabilities` | 配置里声明的能力（JSON 数组，wire 名） |
| `verified_capabilities` | 连接测试**验过**的能力（JSON 数组；部分范围的测试按能力并集保留，被测失败的那一项会被摘掉） |
| `credential_ref` | 密钥托管里的引用——**不是密钥** |
| `enabled` | 是否启用 |
| `test_status` / `last_tested_at` / `last_error_code` | 最近一次测试的结论，供界面重开时如实显示 |
| `created_at` / `updated_at` | 时间戳 |

`ai_responses` 表（v9）：`responses.request` 的请求与结果，一行两态（`pending` / `ready`）。
正文只在 `ready` 时有内容；还没生成时 `responses.get` 返回**空正文**，不编内容
（契约的 `AiResponse` 没有状态字段，所以「还没生成」与「生成出空串」在对象上看起来一样——
真要区分得靠任务状态，这是契约的已知粗糙处，记在「没有做到的」）。

**不变量**：`providers` 表的任何一列都不会出现密钥明文（有测试逐列扫过，见下「验证到了什么」）。

### 迁移 v8 → v9

`SCHEMA_VERSION` 8 → 9。迁移只做两件事：`CREATE TABLE providers` / `CREATE TABLE ai_responses`
（外加各自的索引）。**没有回填、没有猜一个默认服务**：老库升级后 `providers` 是空的，
所以「默认不联网」在升级路径上同样成立。旧数据一行未动（v1–v8 的表都没被碰），
照 [M2 · 向量索引与换代](m2-向量索引与换代.md) 的 v7→v8 写法有回归测试
（`v8_library_migrates_to_v9_and_keeps_old_data_intact`）。

## 内置服务预设

`Core::list_provider_presets()`（`provider::presets`）返回一张**静态**的预设表：每条只回答
「这一家怎么填」——`adapter_id` / `endpoint` / `model` / 能力集合 / 文档链接，外加
`verified` 与一句给用户看的 `note`。

| 预设 id | label | adapter_id | endpoint | model | 能力 | verified |
|---|---|---|---|---|---|---|
| `deepseek` | DeepSeek | `openai_compatible` | `https://api.deepseek.com` | `deepseek-chat` | `generate_text` | **true** |
| `openai_compatible` | OpenAI 兼容端点 | `openai_compatible` | （留空，用户填） | （留空，用户填） | `generate_text` + `transcribe_audio` | false |
| `ollama_local` | Ollama（本机） | `openai_compatible` | `http://127.0.0.1:11434/v1` | （留空，填 `ollama list` 里的名字） | `generate_text` | false |

三条口径：

- **预设只是填表草稿**：`presets()` 不写数据库；`ProviderPreset::to_provider()` 只造一个
  `Provider` 草稿（`id` 留空、`credential_ref = None`、`enabled = true`、
  `test_status = untested`），要不要保存、启用、先测一次都由用户决定。核心**不会**因为
  「有预设」就自动建一条配置，也不会把预设里的 endpoint 当成已配置的服务。
- **预设不含密钥**：结构体里没有密钥字段，返回值里也没有。密钥仍然只经
  `Core::save_provider(config, secret)` 进 `SecretStore`，预设永远碰不到它。
- **`verified` 只认实测**：`true` 表示有人用真实密钥对真实端点跑通过这条预设里的
  endpoint / model / 能力（最少 `generate_text`）；`false` 表示没有真实实例的联调证据
  （可能只验过请求构造，也可能只是照文档填的），**不把「看起来能通」写成「已经能通」**。
  未验证的预设仍可保存、仍可跑连接测试，只是界面/用户不该把它当成已验证。

### DeepSeek 的实测事实（2026-10）

用真实密钥对 `https://api.deepseek.com` 实测（也是这条预设 `verified = true` 的依据）：

- `GET /models` → `200`，`{"object":"list","data":[{"id":"deepseek-flash",...},{"id":"deepseek-v4-pro",...}]}`；
- `POST /chat/completions` 可用；鉴权是 `Authorization: Bearer <key>`；
- `deepseek-chat` 是**兼容别名**，会被路由到 `deepseek-flash`（响应里的 `model` 字段是
  `deepseek-flash`）；`deepseek-chat` / `deepseek-flash` / `deepseek-v4-pro` 都能直接调用。

这一轮代码改动里**真的复跑**的是 `GET /models`（见 `tests/provider_models_live_test.rs`，
输出 `["deepseek-flash", "deepseek-v4-pro"]`）；`POST /chat/completions` 与路由行为的结论
来自任务方此前的真实密钥实测，不是这次新验的。

预设默认填 `deepseek-chat`（最稳的兼容名），并把这些写进 `note`；要用 V4 Pro 就先把 model
改成 `deepseek-v4-pro`，可以先用「模型列表检测」确认真实可用的名字。

`ollama_local` 的 `verified = false` 是有证据的：`adapter.rs` 的单测只钉住了
`http://127.0.0.1:11434/v1/chat/completions` 这个**请求构造**，没有对真实 Ollama 实例联调；
`note` 里就写「请求构造已验证，未对真实 Ollama 实例联调」。

## 密钥存放与它的妥协

契约第 2.7 / 4.4 节：**API 返回凭据存在与否，不返回密钥明文**；密钥不进配置表明文。
落地成两层：

- `SecretStore` trait：`put` / `get` / `delete` / `contains`。`providers.credential_ref`
  只是它的键；换实现不用动表结构；
- 默认实现 `FileSecretStore`：一个凭据一个文件，落在**资料库目录下的 `secrets/`**
  （与库文件分开）。Unix 上目录 `0700`、文件 `0600`，先写临时文件再改名。

**这是妥协，不是系统钥匙串**（如实写在代码注释里，也写在这里）：

- Windows 上 `set_permissions` 只映射只读位，**没有 ACL 语义**——文件权限实际由所在目录的
  继承 ACL 决定。这一层不做加密，也不假装做了；
- 同用户身份下的任何本地进程都能读到明文。系统钥匙串能给的是「按用户/应用隔离 + 由 OS 保管」，
  这里没有；
- 密钥与资料库同目录，**备份实现必须显式排除 `secrets/`**（契约第 8 节要求密钥不进备份）。
  这一片没有备份入口，所以这是约束，不是已经实现的保证；
- 后续方向：Windows DPAPI / Android Keystore（即契约第 5 节 `PlatformHost.storeCredential` /
  `readCredential`）。宿主凭据桥接接上后换一个 `SecretStore` 实现即可，表里的引用形状不用动。

明文只在三个瞬间存在于内存：用户输入、`SecretStore::get`、拼 `Authorization` 头。
请求的 `Debug` 是手写的（敏感头的值一律 `***`），错误消息过一遍密钥擦除（`redact`），
所以日志/错误/返回值里都不会有明文。

从设置页输入密钥目前走**桥接层的过渡入口**：`save_provider(config, secret)`（Rust）/ 适配层
`BridgeDiaryApi.saveProviderCredential`（Dart）。契约冻结的 `DiaryApi.saveProvider(Provider)`
签名里没有密钥（也不该有），而平台层的凭据桥接还没接（F1/F2），所以本片给一条能用的路，
并在文档里写明它是过渡而不是长期接口。

## 能力协商与门控

一个能力「现在能不能用」由**四个条件**共同决定，缺一不可：

1. 服务**已启用**；
2. 适配器**真的实现了**这个能力（`adapter.capabilities()`）；
3. 配置里**声明了**这个能力（`declared_capabilities`）；
4. **没有测试结论，或者最近一次测试验过**这个能力（`verified_capabilities`）。

第 4 条是关键口径：**「明确配置」与「通过验证」两条路都算数，但验证说了不行就不算数**。
没测过的（`untested`）按声明走；测过而没验过的（`failed`，或这一项不在 `verified` 里）
一律不可用——否则就是排一条注定被远端拒绝的任务，与
[M4 · 任务调度与补做](m4-任务调度与补做.md) 里「不排注定终态失败的任务」的口径冲突。

连接测试的 `scope` 是**用户授权的测试范围**：一次只测 `generate_text` 不会抹掉上次验过的
`transcribe_audio`（成功的结论按能力并集保留，失败的结论只摘掉这次真的探过的那些）。

**保存配置时验证结论只在功能参数变化时作废**：适配器 / 端点 / 模型 / 密钥变了，
`test_status`、`verified_capabilities`、`last_error_code`（以及 `last_tested_at`）一起清回
`untested` / 空——旧结论是对旧配置的结论。只改显示名、`enabled` 开关这类非功能字段**原样保留**
结论；密钥按**明文**比对（引用每次都新发，按引用比会把「原样重填同一把钥匙」也算成变化）。
作废的后果很具体：失败结论被洗成 `untested` 后，第 4 条会被跳过，一个刚被证伪的服务又被放行
（评审 #96）。

门控有三个落点：

| 落点 | 行为 |
|---|---|
| `Core::provider_capability_gate(capability)` | 给「能不能用 / 为什么 / 下一步做什么」。`reason` 与 `next_step` 都是给用户看的（去配置什么、去点哪一步） |
| 排任务入口（`responses.request`） | 闸门不通过就**直接拒绝、不创建任务**，错误码 `provider_not_configured`，消息里带可执行的下一步 |
| 队列驱动（`jobs::run_due`） | **领取前**评估（`provider::promote_configuration_waits`）：不满足的任务落在 `waiting_configuration` + `requires_user_action = 1` + `attention_key = job-config:<kind>:<code>`，**不发请求、不消耗尝试次数**（没跑过就不计尝试）；配置改好后同一轮驱动就能接着跑 |

排队中的任务被钉在某个服务上（`ai_responses.provider_id`），判定用的是**同一个服务**——
不然会出现「闸门说可以 → 执行者说不行 → 每次唤醒都撞一次」的抖动。执行期才发现配置坏了
（例如刚还在的服务返回 401）也会把服务的结论如实改成失败（`verified` 清空），
任务停在 `waiting_configuration` 等用户（同样的 `requires_user_action` / `attention_key`，
`attempt_count` 如实记账——那一次确实跑过），而不是反复去撞。`waiting_configuration`
的完整语义（跨 kind 一把尺）见 `docs/architecture/m4-任务调度与补做.md` 的「等配置的语义」。

`transcribe` 的边界要说清：**转写能力可以声明、可以验证、可以查询**（连接测试会真的上传
一小段静音 WAV），但「读音频 → 调用 → 落库」的转写管线是 B4b，队列对**没有执行者**的 kind
仍然走诚实的 `no_handler`。所以 B4a 不给 `transcribe` 加配置闸门——那样会把「注定没有执行者」
说成「等等配置就好了」，是更糟的谎话。B4b 接上执行者时，把 `transcribe` 加进
`provider::job_capability` 即可（与 M4 文档里「把 `transcribe` 算进前置判断」是同一件事）。

## 连接测试

`providers.test(providerId, scope)` **只在用户显式请求时才发请求**。每一项能力：

- 适配器没实现，或配置里没声明 → 报告里 `failed` + **不发请求**（`actualCallMade = false`），
  说明里写清「未发起调用」；
- 其余 → 发一次**真实**探针：`generate_text` 是一次最小的 Chat Completions 调用
  （`max_tokens = 1`），`transcribe_audio` 是一次真的 multipart 音频上传
  （0.05 秒 8 kHz 静音 WAV，合法音频而不是随机字节）。**两者都可能计费**，所以只在授权范围里发。

报告字段（契约的 `ProviderTestReport`）：

| 字段 | 说明 |
|---|---|
| `providerId` | 被测服务 |
| `results` | 范围内逐能力的 `ok` / `failed` |
| `actualCallMade` | 本次是否真的发出了 HTTP 请求（**数出来的**，不是猜的） |
| `message` | 「端点（本机/局域网 还是 远端）可达；认证通过；模型 X 可用；实际能力集：…；本次测试发起了 N 次真实请求（可能计费）」+ 每项失败原因 + 下一步。**N = 0 时换成如实的一版**：「没有发起任何真实调用……端点可达性、认证、模型可用性**都未验证**」——一次都没探过就不许报告「均已验证」（评审 #96 第三条） |

失败分两种形态，各司其职：

- **能力不成立**（没实现 / 没声明 / 测过没验过）不是调用错误，走**报告**（`results` 逐项 `failed`、
  `actualCallMade = false`）；
- **可达性、认证、超时、响应解析**这类有明确失败原因的走**错误码**（`authentication_failed` /
  `provider_timeout` / `model_unavailable` / `network_unavailable` / `quota_exceeded` / `rate_limited`），
  消息末尾如实补上「本次测试已发起 N 次真实请求（可能计费）」，并把这轮结论落库
  （`test_status = failed`、`last_error_code`）。

端点是**本机/局域网还是远端**在报告里可识别地区分（`provider::endpoint_scope`：`localhost` /
回环 / 私有网段 / `.local` / 不带点的名字算本机或局域网）。判定只看主机名，不解析 DNS。
核心**没有任何「用备用服务商静默转发」的路径**：一次调用只认一个服务（用户点名的，或闸门挑中的
那一个），失败就如实失败。

## 模型列表检测

`Core::list_provider_models(providerId)`（`provider::models`）做一件事：
`GET {endpoint}/models`，把 OpenAI 兼容的 `{"data":[{"id":"..."}]}` 解析成
`Vec<ProviderModelInfo>`。它解决的是「用户不知道 model 该填什么」——尤其是
`deepseek-chat` 这种别名与真实模型名对不上的情况。

**只有显式调用才出网**：打开资料库、`providers.list`、`presets()`、保存配置都不会顺手探一次，
与 `providers.test` 同一个口径。测试里用 `PanicTransport` 证明「列配置/列预设」不碰传输层
（`listing_providers_and_presets_never_calls_the_transport`），回放替身证明一次检测只发一次请求。

请求与解析：

| 项 | 口径 |
|---|---|
| 方法/端点 | `GET {endpoint}/models`（endpoint 末尾 `/` 不会拼成双斜杠；endpoint 只填到服务根地址，不要带 `/chat/completions`） |
| 鉴权 | 有凭据时发 `Authorization: Bearer <secret>`；明文只在拼这一行时从 `SecretStore` 取，错误与返回值都过 `redact`。**严格 loopback 服务没有凭据就不发这个头**，不因缺凭据拒绝 |
| 超时 | 与连接测试同一个 `PROBE_TIMEOUT_MS`（20 秒）：都是用户等着看的动作 |
| 响应 | `{"object":"list","data":[{"id":"...","owned_by":"..."}]}`；`data` 必须存在且是数组 |
| 容错 | 条目缺 `id` / `id` 为空 → 跳过该条；缺 `owned_by` → `None`（**可选字段缺失不是错误**）；`data: []` → 空列表 |
| 不支持 | `adapter_id != openai_compatible` → `provider_not_configured` +「不支持列举模型」；本地引擎额外说明「model 是模型目录，没有远端可查」。**不发请求** |
| 没配 endpoint | `provider_not_configured`，消息里给可执行的下一步 |
| 没配凭据 | **非 loopback** 才报 `provider_not_configured`（消息里给下一步）；loopback 照常请求 |

错误映射沿用上面的归一化表：401/403 → `authentication_failed`、超时 →
`provider_timeout`、连不上/5xx → `network_unavailable`、429 → `rate_limited`、
响应解析不了（非 JSON / 缺 `data` / `data` 不是数组）→ `model_unavailable`，
消息里带 200 字响应摘录（已过密钥擦除）。

**本机服务免凭据（2026-10-21 产品决定）**：endpoint 是严格 loopback（`127.0.0.0/8` /
`::1` / `localhost`，含端口、路径、scheme 大小写，见 `provider::endpoint::is_loopback_endpoint`）
时，`list_provider_models` 与 `providers.test` **都不要求存在凭据**：有就照常发
`Authorization`，没有就不发这个头。放行只针对**我们自己的前置检查**——服务器真返回
401 / 超时，仍如实报 `authentication_failed` / `provider_timeout`，不把服务器错误装成成功。

**私有网段（`10.` / `172.16-31.` / `192.168.`）刻意不算本机**：那是「局域网里的另一台
机器」，放宽到那里会让「没配密钥」在别人的服务器上悄悄通过，风险与 loopback 不同；
这类服务仍要填一条凭据（服务端忽略不忽略是它的事）。

**真实联调**：`crates/diary_core/tests/provider_models_live_test.rs` 是一个 `#[ignore]` 用例，
密钥只从环境变量 `DEEPSEEK_KEY` 读，没有就跳过；断言 DeepSeek 的 `/models` 返回
`deepseek-flash` 与 `deepseek-v4-pro`。日常 `cargo test` 不会跑它，CI 不受影响。

## 错误码与重试语义

归一化表（`provider::normalize`，逐条被测试钉住）：

| 情况 | 契约错误码 | 可重试 | 作业侧 |
|---|---|---|---|
| 没配置 / 没启用 / 适配器没实现 / 未通过验证 | `provider_not_configured` | 否（等配置） | `waiting_configuration` + `requires_user_action` + `job-config:<kind>:<code>`，不消耗尝试次数（闸门在领取前挡下） |
| 401、403（不带额度字样） | `authentication_failed` | 否（等配置） | `waiting_configuration` + 同样的 `requires_user_action` / `attention_key`（执行期才发现，`attempt_count` 如实记账）；并把服务结论改成失败，避免反复撞 |
| 402、403 + 额度字样、`insufficient_quota` | `quota_exceeded` | 否（暂停） | `waiting_configuration`（同上，执行期记账） |
| 429 | `rate_limited` | 是（**尊重 `Retry-After`**，上限 1 小时） | `retry_wait` |
| 408、连接/读写超时 | `provider_timeout` | 是（有限重试） | `retry_wait`；**不承诺远端未计费** |
| 断网、连不上、TLS 失败、5xx | `network_unavailable` | 是（退避重试） | `retry_wait` |
| 404、其它 4xx、3xx（不跟随重定向）、响应解析不了 | `model_unavailable` | 否 | `failed` + `requires_user_action`（检查端点/模型/适配器） |

两条口径要说清：

- **5xx 归到 `network_unavailable`**：契约第 7 节没有单独的「服务端错误」码，而它对用户的意思是
  同一件事——材料还在、处理等远端恢复，自动行为都是「有退避地排队」；
- **响应不可解析归到 `model_unavailable`**：v1 的码里它是唯一「配的这个模型/端点与实际说话方式对不上，
  去检查配置」的一类。**没有新增错误码**——新增码等于改契约与 Dart 枚举。

重试是**有限**的：退避 `30 秒 × 2^attempt_count`（封顶 1 小时，沿用 B1c/[M4](m4-任务调度与补做.md) 的规则），
额度用完进 `failed` 等人工。「等配置」的两类（`provider_not_configured` / `authentication_failed`）
与「额度暂停」（`quota_exceeded`）**不排退避**：重试一百次也是同样的 401，只会反复打扰远端。

## 默认不联网的证明方式

不是靠「我们没写这种代码」的承诺，而是三件事一起：

1. **没有别的调用点**。`Transport::send` 只被三处调用：`providers.test`、模型列表检测
   （`list_provider_models`）与 Provider 作业执行者；没有启动探测、没有后台预热、
   没有「先试着连一下」；
2. **测试注入「一调用就 panic」的传输层**（`PanicTransport`）：把记录、导入、提取、索引、
   关键词与混合检索、一整轮 `run_due_jobs` 跑一遍，只要有任何一条路径偷偷出网，测试当场 panic
   （`local_work_never_touches_the_network_with_the_default_transport`）。**内存库的默认传输层
   就是这个替身**：测试想发请求必须显式注入回放替身；
3. **记账替身数调用次数**（`CountingTransport` / `ScriptedTransport::call_count`）：
   没有配置服务（或只有没启用的服务）时，`responses.request` 被拒且任务数为 0、调用数为 0；
   受理请求本身也不发请求，只有 `run_due_jobs` 真的跑到那条任务时才发。

`responses.request` 也只有主动调用才运行：没有用户请求就没有 `ai_responses` 行、没有 `respond` 任务，
队列不动就没有任何调用。契约第 4.3 节那句「只有主动调用才运行」在数据上就成立了。

## HTTP 客户端：为什么是 `ureq`

| 项 | 选择 | 理由 |
|---|---|---|
| 客户端 | `ureq = 2.12.1`（`default-features = false, features = ["tls"]`） | 核心整体是**同步**的（没有 async runtime），ureq 就是同步阻塞 + 自建连接池，与 `Core` 的调用形状一致，不必为一个 HTTP 调用引入 tokio |
| TLS | rustls（ureq 的 `tls` 特性）+ `webpki-roots` | 不用 OpenSSL，交叉编译 Android 不需要额外系统库；根证书是**打包进来的 Mozilla 根集合**，不读系统证书库（企业自签代理要后续显式支持，记在「没有做到的」） |
| 许可证 | MIT / Apache-2.0 双许可 | 与仓库其余依赖一致，无 GPL 传染；`cargo deny` 级别的检查这一片没做 |
| 超时 | 每个请求自己的 `timeout_ms`：连接测试 20 秒、生成 120 秒 | 测试是用户等着看的动作，不该等两分钟；生成不设超时会让作业永远挂着。请求级超时（不是 agent 级），所以两种用途互不影响 |
| 重定向 | `redirects(0)`，**不跟随** | 跟随重定向会把 `Authorization` 头带到另一个主机上（密钥泄漏风险）。3xx 当「端点配错了」报 `model_unavailable`，让用户直接配最终地址 |
| 代理 | **不支持**（没有读环境变量、没有代理设置项） | 这一片不做，如实记在「没有做到的」；本机/局域网直连是本片的默认场景 |
| 响应体 | 手动读进 `Vec<u8>` 后自己解析 JSON | 只用得上 `text` / `choices[0].message.content` 两个字段，不为它开 serde 结构体；结构不符时报 `model_unavailable` 并把响应体短摘录（200 字，且过密钥擦除）带进消息 |

multipart 是手写的（一个文件字段 + 两个文本字段，boundary 是常量）：形状固定，
引一个 multipart crate 不划算。

## 本地引擎：连接测试与降级

本地引擎（#91 的 `sherpa_onnx`）走**同一套**能力协商与门控，只有两处不同：

1. **配置要求不同**：没有 `endpoint`（给本地适配器配 `http(s)://` 会被 `providers.save`
   拒绝，理由是「endpoint 是 HTTP 协议的事」）、没有密钥要求（`credential_ref` 存 NULL），
   改为**本机资源就绪**：模型目录里有 `model.int8.onnx`/`model.onnx` 与 `tokens.txt`，
   引擎动态库找得到。不就绪 → 与 HTTP 那条路同一个码 `provider_not_configured` →
   作业停在 `waiting_configuration`（不消耗重试额度），理由里写清缺哪个路径、下一步做什么；
2. **连接测试不发请求**：`providers.test` 对本地引擎会**真的加载模型并转写 0.05 秒静音**
   （与 B4a 那份探针音频同一段），报告里写明「完成了 N 次本机推理，**没有发起任何网络
   请求**」。`actual_call_made` 对本地引擎表示「真的跑了一次推理」——契约里这个字段的
   含义是「本次测试发生了真实调用」，本机推理也算。

核心**自带**的本机转写引擎（随包模型）不是 `providers` 表里的一行，但它能被
`providers.test` 测：id 是 `builtin_local_asr`（常量 `BUILTIN_LOCAL_ASR_ID`）。
为什么不自动插一行：用户删掉之后下次开库又会原样长回来，没有墓碑就无法区分「用户不要」
与「还没配」。要改模型目录/停用它，就保存一条同 adapter 的覆盖配置（行优先于内建）。

## 验证到了什么

`crates/diary_core/tests/provider_test.rs`（30 条）与 `provider/*.rs` 的单元测试覆盖：

- 配置往返、**脱敏**（逐列扫 `providers` 表、扫资料库目录所有文件都找不到密钥明文、
  Unix 上文件 `0600`）、移除配置**不删日记**且清掉没人引用的凭据；
- 默认不联网的证明（panic 替身 + 记账替身两条）；
- 能力门控：只声明生成的服务承担不了转写（闸门 + 排任务入口都拒绝、一条任务都不排）；
  测过没验过的能力不再排任务；适配器没实现的能力声明了也不算；
  队列侧门控让任务停在 `waiting_configuration`（状态可见、尝试次数为 0、`requires_user_action`
  + `attention_key = job-config:respond:<code>`），配置回来后一轮跑完；
- **跨 kind 一致性**（`waiting_configuration_means_the_same_thing_across_job_kinds`）：
  同一个状态在 `respond`（等服务）与 `semantic_index`（等模型）两个 kind 上给出**相同**的
  `requiresUserAction`（都为真），`attentionKey` 各指各的「配哪一个」；把任一侧改回旧口径
  这条就红——它钉的是 #96 合 main 时暴露的那处语义冲突；
- 连接测试四种结果：可达（含「2 次真实请求」与两个端点 URL）、认证失败（码 + 擦除 + 调用次数）、
  超时、能力不支持（不发请求、`actualCallMade = false`）；
- 错误归一化映射表（每条码 → 可重试 / 等配置 / 永久），并走真实作业路径验证
  429 尊重 `Retry-After`、5xx 退避、401 等配置且**不频繁重试**、404 终态失败；
- 回放传输层端到端：`responses.request` → `run_due_jobs` → 正文与模型名落库、
  同内容请求复用同一条任务、已完成的不再花第二次钱；
- 迁移：v8 → v9 建表、不回填、旧数据一行未动；迁移后的库照样能配置并跑通一次回放调用；
- **内置预设**（`provider::presets` 的 5 条单元测试）：条数与 id 唯一、每条预填的能力都是
  适配器真的实现了的、DeepSeek 的 endpoint/model/`verified = true` 与实测 note、
  其余预设 `verified = false` 且 Ollama 的 note 写明「请求构造已验证，未对真实 Ollama 实例
  联调」、`to_provider()` 只是草稿（id 空、无凭据、untested）、列预设不碰库也不碰网络；
- **模型列表检测**（`provider::models` 的 14 条单元测试）：正常 `{"data":[{"id":"a"},{"id":"b"}]}`
  → 两条、URL 与 `Bearer` 头、401 → `authentication_failed`、超时 → `provider_timeout`、
  连不上 → `network_unavailable`、429 → `rate_limited`、缺 `data` → `model_unavailable`
  可读消息、非 JSON（含密钥回显）→ 擦除、缺 endpoint/凭据 → `provider_not_configured`、
  `sherpa_onnx` → 「不支持列举模型」且**零请求**、打开/列配置/列预设零请求；
- **真实联调**（`tests/provider_models_live_test.rs`，`#[ignore]`）：用环境变量里的真实
  DeepSeek 密钥跑 `GET /models`，断言返回 `deepseek-flash` 与 `deepseek-v4-pro`；
  默认不跑、CI 不受影响，密钥不落文件。

命令（全部不联网）：

```bash
cargo +1.94.1 clippy --workspace --all-targets -- -D warnings
cargo +1.94.1 test --workspace
cargo build --release
(cd packages/diary_bridge && dart analyze --fatal-infos && dart test)
(cd packages/diary_api && dart analyze --fatal-infos && dart test)
(cd packages/diary_mock && dart test)
python3 tools/check_docs.py
```

## 没有做到的

- **没有用 OS 钥匙串**：默认实现是受限权限的独立文件（上面「密钥存放与它的妥协」写清了三条妥协）。
  Windows DPAPI / Android Keystore 要等 `PlatformHost` 凭据桥接；
- **没有配额与用量统计**：契约第 2.7 节说「用量回传由 Provider 如实提供，核心只记录不猜测」，
  这一片连「记录」都没做——没有 `usage` 表、没有计费估算。远端返回的 token 数被丢掉了。
  现在要做的话，需要新表 + 新的 DTO 字段（契约变更），见下面「需要拍板」；
- **没有代理设置**：不读 `HTTP_PROXY` / 系统代理，也没有设置项。企业网络下要手动配可直连的
  endpoint；
- **CI 里没有真实网络验证**：日常测试都用回放/记账替身，真实 HTTPS、真实证书链、
  真实服务商的错误形状都不在 CI 里跑。这一片新增了一条 `#[ignore]` 的人工联调用例
  （DeepSeek `/models`，密钥从 `DEEPSEEK_KEY` 读），但它要人手动跑；其他服务商的
  真实形状仍待补；
- **模型列表检测只支持 `openai_compatible`**：别的适配器（含 `sherpa_onnx`）如实报
  「不支持列举模型」。没有分页、没有缓存、没有排序（按远端返回顺序给）；`owned_by`
  只解析与返回，界面还没用上；
- **生成作业仍要求凭据**：本轮按任务只放宽 `providers.test` 与 `list_provider_models` 两条
  用户显式检测入口；`capability_gate_for` 对 HTTP 服务仍要求 `credential_ref`，所以本机服务
  要跑生成（`respond` / `diary.generate`）仍得填一条占位凭据。要不要把免凭据扩到生成路径，
  是另一条决定（会改排任务语义）；
- **预设还没到界面**：`Core::list_provider_presets()` 已经经 C ABI 的 `providers.presets`
  暴露（`providers.models` 同理），但 Flutter 桥接与界面还没有「选预设 → 填表 → 试连 →
  保存」这条流程；
- **没有跟随重定向、没有自定义 CA**：自签证书的局域网服务现在连不上（rustls 只认打包的根集合）；
- **响应不可解析归到了 `model_unavailable`**：v1 没有更贴切的码。若要更精确，需要新增
  `provider_response_invalid` / `provider_server_error` 之类的码（契约 + Dart 枚举变更）；
- **转写管线已接**（#91，见 [M3 · 转写链](m3-转写链.md)）：`transcribe` 有了执行者
  （本机 ONNX 引擎 + 分段/恢复/幂等 + 落派生内容），`job_capability` 也加上了映射。
  这里保留这条是提醒读者：**远端转写**没有进产品链——录音的转写链只跑本机引擎；
- **`understand_image` 只登记不实现**：内置适配器没有图像理解（M4 的插件模型能力）；
- **界面拿不到「这条任务在等什么配置」的完整理由**：任务的 `error_code` 是合同内的码
  （`provider_not_configured` / `authentication_failed`），可执行的解释在
  `Core::provider_capability_gate` 的 `reason` / `next_step` 里，而它**还没有桥接出去**
  （契约的方法清单里没有它）。要么把闸门暴露成一个契约方法，要么给任务加一个 message 字段，
  两条都是契约变更，见「需要拍板」；
- **`ai_responses` 一行两态**：契约的 `AiResponse` 没有状态字段，所以「还没生成」与
  「生成出空串」在对象上不可区分，只能看任务状态。B4b 做正文与版本语义时要一起定。
## 需要拍板

这一片按契约 v1 现有形状落地，下面几条是**越出本片范围、需要产品/契约决定**的事：

1. **能力集合的最小集**。契约第 2.7 节要求至少区分 `generate_text` / `transcribe_audio` /
   `understand_image`，本片把三项都放进枚举与配置，但**只实现前两项**（图像理解留给 M4 的插件
   模型能力）。要不要现在就把「视觉」做成一个可声明、可验证的项（需要一种内置协议实现 + 真探针），
   还是等 M4 一起定？
2. **配额与用量统计要不要现在做**。契约第 2.7 节说用量「由 Provider 如实提供，核心只记录不猜测」，
   本片没有记录（连远端返回的 token 数都丢掉了）。要做需要新表 + `AiResponse` 或 Provider 的新字段
   （契约变更），而且口径要定：只记 token 数，还是也算钱、按什么价？
3. **「这条任务在等什么配置」怎么到界面**。现在任务的 `error_code` 是合同内的码（`provider_not_configured`
   / `authentication_failed`），可执行的完整理由在 `Core::provider_capability_gate` 的 `reason` /
   `next_step` 里，而它**还没有桥接出去**。两条路：把闸门暴露成一个新契约方法（加性、不改 apiVersion），
   或者给任务加一个 message 字段（改 DTO）。选哪条？
4. **密钥入口的归宿**。本片为了能用，在桥接层加了明文参数 `save_provider(config, secret)` 与适配层的
   `saveProviderCredential`。契约最终的口径是平台层系统凭据（`PlatformHost.storeCredential` →
   `credentialRef`）。平台凭据桥接排在哪一片？过渡入口要不要在契约里留一行说明？
5. **错误码要不要更细**。5xx 现在归 `network_unavailable`、响应不可解析归 `model_unavailable`
   （都是 v1 现有码里最贴切的）。要不要新增 `provider_server_error` / `provider_response_invalid`
   之类的码？新增码等于改契约与 Dart 枚举，得两边一起动。
6. **`ai_responses` 的状态**。契约的 `AiResponse` 没有状态字段，「还没生成」与「生成出空串」在对象上
   不可区分。B4b 做正文与版本语义时要不要加 `state`？

## 已决

**「保存前就能试连」：不做（2026-10-21 产品决定）**。

> 2026-10-21 产品决定：**保持「先保存再试连」**，不做未保存草稿 + 临时凭据入口。理由：
> 临时凭据要多一条密钥生命周期（内存里传、何时擦除、能否被日志抓到），而填错 endpoint
> 的代价只是「保存→测失败→改→再保存」。
>
> 同次决定：**本机（loopback）服务不校验密钥**——凭据形式上可以留、也可以不填；核心
> 不因缺凭据拒绝，但仍如实上报服务器返回的错误。

落地范围：`providers.test` 与 `list_provider_models` 两条用户显式检测入口（严格 loopback：
`127.0.0.0/8` / `::1` / `localhost`）。私有网段不算本机，仍要凭据；生成作业的能力闸门
本轮没有放宽（见「没有做到的」）。

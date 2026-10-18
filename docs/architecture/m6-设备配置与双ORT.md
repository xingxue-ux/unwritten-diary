# M6 · 设备配置与双 ORT（鸿蒙产品 HAP 的配置能力）

> 分支 `feat/ohos-hap-config-20261018`（从 `feat/B4f-ohos-asr-20261018` / PR #130 开）。
> 真机：HAD-W32 / API 23 / arm64-v8a / HarmonyOS PC，产品身份
> `com.lingxing.unwrittendiary`（debug signed）。原始日志、解析结果与宿主测试输出在
> `/mnt/e/ohos/evidence/hapcfg/`（不进仓库）。

## 0 问题

鸿蒙应用**设不了环境变量**，而嵌入器（`DIARY_MODEL_DIR` / `DIARY_ORT_DYLIB`）与转写引擎
（`DIARY_ASR_ENGINE_LIB` / `DIARY_ASR_MODEL_DIR`）的路径过去只能从环境变量读。结果是：
产品 HAP 里语义检索**永远无法配置**（`waiting_configuration` / 报错），转写只能靠
`providers.save` 一行的 `model`。

还有一个更硬的约束：**同一个 HAP 要同时容纳两个 ONNX Runtime**。sherpa-onnx 1.13.8 的
OHOS 预编译库 `NEEDED = libonnxruntime.so`，且按符号版本只认 `OrtGetApiBase@VERS_1.16.3`；
我们自己的 `ort` crate 用 1.29.1（`@@VERS_1.29.1`）。两者不能互换，同一个 `libs/arm64/`
里又**不能有两个同名文件**。

## 1 配置文件：`core-config.json`

### 1.1 放哪、为什么

**资料库文件旁边**：`<root>/core-config.json`，`root` 是 `Core::open` 拿到的库文件所在目录
（设备上是 `/data/storage/el2/base/haps/entry/files/`）。不选 `config/core.json`：

- 一个应用沙箱只有一个资料库，库与配置**同目录、同一次搬运**，不需要先建子目录
  （少一层「目录不存在」的失败面）；
- 排障时 `ls <filesDir>` 一眼能看到配置与它旁边的库，路径同源；
- 备份/迁移只盯一个目录：不会出现「库搬走了、配置没搬」的半套状态。

### 1.2 字段与优先级

四个字段**全部可选**，名字与产品侧配置习惯一致（camelCase；也接受 snake_case 别名）：

```json
{
  "modelDir": "/data/storage/el2/base/haps/entry/files/models/bge-small-zh-v1.5-ours",
  "ortDylib": "/data/storage/el1/bundle/libs/arm64/libonnxruntime-diary-1.29.1.so",
  "asrEngineLib": "/data/storage/el1/bundle/libs/arm64/libsherpa-onnx-c-api.so",
  "asrModelDir": "/data/storage/el2/base/haps/entry/files/asr/sense-voice"
}
```

**逐字段**的优先级（先命中先用）：

| 能力 | 顺序 |
|---|---|
| 语义（嵌入器） | 环境变量 → 文件 → `absent` |
| 转写模型目录 | 环境变量 `DIARY_ASR_MODEL_DIR` → 文件 `asrModelDir` → `providers.model` → 随包默认 |
| 转写引擎库 | 环境变量 `DIARY_ASR_ENGINE_LIB` → 文件 `asrEngineLib` → 可执行文件旁候选 → 按库名（移动端） |

- **OHOS 沙箱路径拒绝（两个库都拦）**：`asrEngineLib` **和** `ortDylib` 都不能指向
  应用数据目录（`/data/storage/el2/...`、`/data/storage/el1/base/...`）——linker 只允许
  从 HAP 的 `libs/arm64` dlopen。配置解析时就报 `invalid` / `invalid_argument`（不是等
  第一次加载才失败），`/data/storage/el1/bundle/libs/arm64/...` 放行；宿主单测同时覆盖
  「拒绝沙箱」与「放行安装目录」（`validate_ort_dylib_path` / `validate_engine_lib_path`）。

- **环境变量仍在最前**：探针/CI 靠它换模型，设备上有配置文件也不能让它失效；
- 文件和 provider 谁先：**文件在前**（任务书给的链）。代价写在 m3 文档里：provider 行仍被
  选中（`providerId` 还是它），但模型目录以文件为准；
- 语义这一路**没有 provider 档**：嵌入器不对应某条服务，是核心自己的能力。

### 1.3 失败不致命

| 情况 | 行为 |
|---|---|
| 文件不存在 | 与加这个文件之前**逐位一致**（`config_source=absent`） |
| 读不了 / JSON 坏 / 字段类型不对 | 这一档整体不生效，回落下一档；原因进 `core.info.runtime_config.file.error` |
| 不认识的字段 | 不静默丢掉：收进 `ignoredFields`（拼错字段名时就是它） |
| 字段是空串/空白 | 当成没配（脚本留空比删字段常见） |
| 配了但文件不在 | 语义 `state=invalid` + 原因（**不假装「未就绪」**） |

配置只在 `Core::open` 时读一次——与 `DIARY_*` 环境变量同一语义（打开那一刻的快照）。
改了文件要重新开库才生效。

### 1.4 `core.info` 怎么暴露

`core.info` 多了一格 **`runtime_config`**（加性扩展；`diary_bridge::api::LibraryInfo` 没有
它，鸿蒙宿主读 C ABI 的 JSON）：

```json
"runtime_config": {
  "file": {"path": "…/core-config.json", "present": true, "error": null, "ignoredFields": []},
  "semantic": {
    "state": "ready",                  // absent / configured / ready / invalid
    "configSource": "file",            // env / file / absent（任一字段来自文件即 file）
    "modelDir": "…", "modelDirSource": "file",
    "ortDylib": "…", "ortDylibSource": "file",
    "modelVersion": "bge-small-zh-v1.5-ours@d44978e451a0",
    "reason": null
  },
  "transcription": {
    "state": "ready",                  // absent / configured / ready
    "configSource": "file",            // 模型目录的来源：env / file / provider / bundled / absent
    "modelDir": "…", "modelDirSource": "file",
    "engineLib": null, "engineLibSource": "platform",   // env / file / platform / absent
    "providerId": "local_asr_ohos",
    "reason": null
  }
}
```

口径说明：

- 这一格的键是 **camelCase**（与 `core-config.json` 的字段同名同形），`core.info` 的外层键
  仍是 snake_case；
- `core.info` 只做**能 stat 的**判断，不加载模型、也不 dlopen 引擎库（启动路径）；
  移动端「按库名解析」的引擎库在探测过之前如实报 `configured` + 「还没有探测过」，
  第一次任务/连接测试之后按缓存结论报 `ready` 或不可用原因；
- `semantic.state=invalid` 时 `reason` 写清缺什么（找不到模型文件 / 没有 tokenizer.json /
  ORT 动态库不在）。

## 2 一个 HAP 里的两个 ORT

### 2.1 命名与加载

| 文件（`libs/arm64-v8a/`） | 来源 | 大小（包内） | SONAME | 导出 |
|---|---|---:|---|---|
| `libonnxruntime.so` | sherpa-onnx 1.13.8 自带 | 12,930,856 | `libonnxruntime.so` | `OrtGetApiBase@@VERS_1.16.3` |
| `libonnxruntime-diary-1.29.1.so` | 我们自编 ORT 1.29.1 | 19,623,560 | `libonnxruntime.so.1` | `OrtGetApiBase@@VERS_1.29.1` |

- sherpa 的 `NEEDED = libonnxruntime.so` 由系统按名解析，拿到的是 1.16.3；
- `ort` 走 `load-dynamic`，`init_from` 收到的是**绝对路径**
  `/data/storage/el1/bundle/libs/arm64/libonnxruntime-diary-1.29.1.so`，不看 SONAME；
- 两者 **文件名不同**，**SONAME 也不同**（`libonnxruntime.so` vs `libonnxruntime.so.1`）
  ——PR #130 评审提出的「SONAME 本来就不一样」在本机实测成立：`dlopen` 绝对路径直接加载，
  与按名解析互不干扰；
- 注册到 `ort` 的是**进程级一次性**的 `ORT_INIT`（`OnceLock`），同一个进程只加载一份；
- 加载顺序两种都验过（见 §3.3）：先 1.29.1 后 1.16.3、先 1.16.3 后 1.29.1 都正常。

### 2.2 打包

```bash
apps/ohos-diary-core/scripts/stage-sherpa.sh     # libsherpa-onnx-c-api.so + libonnxruntime.so(1.16.3)
apps/ohos-diary-core/scripts/stage-ort-1291.sh   # libonnxruntime-diary-1.29.1.so
```

`stage-ort-1291.sh` 会先断言**源文件** sha256（`a2240074…` / 19,623,736 B，未 strip），
再 `llvm-strip --strip-all` 成**包内文件**并断言包内 sha256（`1ece24ca…` / 19,623,560 B）、
`SONAME=libonnxruntime.so.1` 与 `OrtGetApiBase@@VERS_1.29.1`；同时调
`stage-sherpa.sh --check` 断言 sherpa 那份 1.16.3。默认 `build-hap.sh` **不含**这份；
`WITH_SEMANTIC=1` 构建才会调它的 `--check` 并把它打进 HAP。两个脚本的产物都在
`entry/libs/`（`.gitignore`），构建时由 `build-hap.sh` 的 ProcessLibs 收进 HAP。

### 2.3 HAP 体积

| 构建 | `libs/arm64-v8a` 里的原生库 | signed HAP | 相对 #130 |
|---|---|---:|---:|
| #122 基线（只有我们自己的库） | capi + napi + libc++ | 13,374,010 | — |
| 默认 `build-hap.sh`（asr-only） | + `libsherpa-onnx-c-api.so` + `libonnxruntime.so` | 30,198,881（本轮 29,977,970） | 基线 |
| **`WITH_SEMANTIC=1`（+ 自编 1.29.1，rebase 前验证）** | 再加 `libonnxruntime-diary-1.29.1.so` 19,623,560 | **50,067,086** | **+19,868,205（+18.95 MiB）** |
| `WITH_SEMANTIC=1`（rebase 到 #130 后重构建） | 同上 + #130 的 ArkTS（下载重写） | **50,088,328** | **+19,889,447（+18.97 MiB）** |

**构建矩阵**：默认 `build-hap.sh` **不含** 1.29.1（关键词可用、语义不可用）；
本页的真机结果用 **`WITH_SEMANTIC=1`** 构建（`stage-ort-1291.sh --check` 通过才构建）。
两份 ORT 的包内字节数/hash 见 `models/manifest/sense-voice-small-int8-2024-07-17.json`
的 `prebuiltArtifacts.ohos-arm64-v8a`（1.29.1 的包内 sha256 是
`1ece24ca…`，源文件是 `a2240074…`）。

增量基本就是 1.29.1 这一份（差值 244,645 B 来自 `modules.abc` 的探针代码与签名/对齐）。
模型**都不进 HAP**：bge 与 SenseVoice 由 `hdc rport` + 应用内 `http.requestInStream`
写进沙箱（24.3 MB / 239 MB）。

## 3 真机验证（同一次启动、同一进程）

### 3.1 探针

`--ps verify_mode hap_both`（`entry/src/main/ets/verify/HapBothVerify.ets`）：

1. **探针自己生成** `core-config.json`（沙箱内本地写，**不经 HTTP**：运行期配置不能
   「下载即信任」）；bge 模型仍从 `http://127.0.0.1:8000`（宿主 `hdc rport` +
   `python3 -m http.server`）下载，走统一的 `ModelDownload`（bytes+sha256、`.part`、
   2xx/Content-Length、原子 rename、重试）；
2. 开一个**锚句柄**，整段验证期间不关（Core 与两个 ORT 的会话留在进程里）；
3. worker 阶段一：建两条中文日记（**前一天**，避开上一轮留下的同一天音频篇，否则切块会
   把它们塞进同一块、块级相似度让目标篇挤出首页）→ 排关键词/语义索引任务 → `jobs.runDue`
   → `search.start(mode=semantic)`；
4. 主线程：把沙箱里的 `zh.wav`（16 kHz/单声道/16 bit，5.592 s）按平台层约定登记成一条录音
   （`recordings.prepare` → 写片段 → `registerSegment` → `finalize`）→ 排一条转写任务；
   这一步**替代麦克风采集**：上一轮已证明「扬声器→麦克风→文本」，但那条声学路径不稳定
   （本轮第一次真录得到 `The.`，上一轮也出现过「快饭/派饭」）。这一轮要验的是**双 ORT
   进程里引擎还出不出文本**，已知 PCM 才是确定的；
5. worker 阶段二：`jobs.runDue`（sherpa + 1.16.3 首次加载、转写）→ 再建语义索引
   （把转写文本切块、用 1.29.1 算向量）→ 按 `asset_id` 语义检索到那条音频 → `sources.extract`
   抽正文 → 再跑一次阶段一的查询（证明 1.16.3 载入后 1.29.1 仍正常）。

`--ps order asr_first` 把顺序反过来（先转写、先加载 1.16.3，再加载 1.29.1）。

### 3.2 配置来源的三种真机状态

| 运行 | 配置文件 | provider 行 | `semantic` | `transcription` |
|---|---|---|---|---|
| run3 | modelDir + ortDylib | 有（上一轮那条） | `file` / ready | `provider` / ready |
| run4 | 四个字段全给 | **删掉** | `file` / ready | `file` / ready（`engineLibSource=file`） |
| run5 | 四个字段全给 | 有 | `file` / ready | `file`（`providerId=local_asr_ohos`，**文件压过 provider**） |

宿主侧的优先级证据（不是推断，是 `cargo test`）：

- `core_config_env_test`：环境变量**逐字段**压过文件（两个字段都设 → `configSource=env`；
  只设模型目录 → 模型目录 `env`、ORT `file`；全撤掉 → 回到文件那一套）；
- `core_config_test`：无文件时行为不变；四档生效；坏 JSON 不致命；未知字段被报出；
  半配时逐字段来源如实；缺文件 → `invalid`；文件压过 provider。

### 3.3 结果（原始日志见证据目录）

**语义检索**（run3，查询「母亲体检的结果怎么样」，与目标正文**没有共同词**）：

```text
DIARY_HAP SEARCH label=search.semantic.semantic_first hits=26 phase=done coverage=complete warnings=
DIARY_HAP HIT label=search.semantic.semantic_first i=2 kind=text matchedBy=semantic sourceId= dayKey=2026-10-17 snippet=今天下午陪妈妈去医院复查，医生说骨头愈合得不错，下周再去拍一次片子。
```

对照组（同一条查询、关键词模式）：`DIARY_ASR SEARCH query=母亲体检的结果怎么样 hits=0 elapsedMs=3`
——命中只可能来自语义那一路。

**转写**（run4，配置里 `asrEngineLib` + `asrModelDir`，provider 已删）：

```text
DIARY_HAP INJECT SEG index=0 file=segment-000000.wav bytes=178988 pcmMs=5592 sha256=9172ffdc7d09db01 durable=true
DIARY_HAP INJECT FINALIZE asset=asset_01a14f60c699741fb7fe432a078f20d9 audioMs=5592 segments=1 state=saved queued=true
DIARY_HAP JOBS phase=semantic_after_asr processed=3 succeeded=3 failed=0 ...
DIARY_HAP TRANSCRIPT phase=semantic_after_asr extractor=local_transcription coverage=complete status=ready text=开饭时间早上9点至下午5点。
```

**共存**：`core.info`（run4 结束时）

```text
semantic.state=ready semantic.source=file ... version=bge-small-zh-v1.5-ours@d44978e451a0
transcription.state=ready transcription.source=file modelDirSource=file engineLibSource=file
```

**顺序**（run6，`order=asr_first`）：先转写 `ORDER asr_first runMs=3610 pssAfterKb=613635`，
再语义 `PHASE_REPORT phase=semantic_first pssBeforeKb=613659 pssAfterKb=688234`（+74,575 kB）——
1.16.3 已在进程里，1.29.1 仍能加载并算出向量，转写文本也抽得出来。

### 3.4 耗时与内存（HAD-W32）

| 阶段 | 实测 |
|---|---:|
| 下配置 + 24.3 MB 模型（首次） | 942 ms（27.35 MB/s） |
| 语义：排索引 + `runDue`（bge 加载 + 两条日记切块/向量） | 323–401 ms |
| 语义检索（含查询向量） | 同上一行（`search.start` 在 `runDue` 之后，毫秒级） |
| 转写：把已知 PCM 登记成录音 | 8–9 ms（5.592 s 音频） |
| 转写：`runDue` 冷启动（sherpa + 1.16.3 + 228 MB 模型 + 推理 + 语义重建） | 3,440–3,573 ms |
| 一次启动总耗时（`DONE totalMs`） | 4,073–4,463 ms |
| PSS：进程起步 → bge 就绪 | 42.7–45.1 MB → 128–159 MB（**+75–114 MB**） |
| PSS：再加转写引擎 | → 679–695 MB（**+525–545 MB**） |
| Private Dirty 增量 | +0.36–0.52 MB（模型与 ORT 主要是共享/映射页） |

bge 的 +75–114 MB 是 24.3 MB 模型 + ORT 1.29.1 的 arena/映射；转写的 +525 MB 与 #130
量到的 +591 MB 同一量级（模型 239 MB + ORT 1.16.3 12.9 MB + 推理工作区）。

### 3.5 rebase 之后（没有重跑真机）

PR #134 在 2026-10-18 深夜 rebase 到 #130 的 `98ec24d`（模型下载完整性重写、`stage-sherpa`
断言、ArkTS 包装）。**核心 native 代码一行没变**：包内 `libdiary_capi.so` 11,528,088、
`libsherpa-onnx-c-api.so` 3,750,328、两份 ORT 12,930,856 / 19,623,560 与验证时**逐字节
相同**；变的只有 `modules.abc`（215,928 → 229,900，+13,972 B：并入 #130 的下载实现与
`hap_both` 自带取文件），所以重构建的 HAP 是 **50,088,328 B**（sha256 `31d15915…`），
比验证时多 21,242 B。

按要求这一轮**没有再动产品身份**，所以上面 §3.2–§3.4 的真机数字仍对应 rebase 前的构建；
探针的取文件路径此后又并成了**一份**：`ModelDownload.downloadToFile`（`.part` → 2xx →
Content-Length → 实收字节 → sha256 → 原子 rename + 重试），`HapBothFetch` 已删除；
`hap_both` 的 `core-config.json` 改为探针本地生成、不再经 HTTP。
**下次动设备时先跑一次 `hap_both` 复核**。

## 4 PR #130 评审的 P0（顺手修了）

**症状**：OHOS 上「引擎库没解析到显式路径」时 `readiness` 直接乐观 `ready: true`；闸门放行
→ `load_library()` 失败 → 作业落 `failed`/`model_unavailable`（终态、不重试、不等配置）。

**修法（两处，缺一不可）**：

1. `readiness` 在移动端按库名加载时**真 dlopen 探测一次**（`OnceLock` 缓存，成功后不解引用、
   不 dlclose），缺库 → `provider_not_configured`，作业停 `waiting_configuration`；
   不做这一步的话，把执行者的错误码改成「等配置」之后，`promote_configuration_waits`
   会在同一轮里把任务反复拉起（实测空转满 64 条上限）；
2. `load_library()` 的「加载不到」也归 `provider_not_configured`；只有「库在但符号不对」
   仍是 `model_unavailable`。

**真机证据**（把 `libsherpa-onnx-c-api.so` 与 `libonnxruntime.so` 从 HAP 里拿掉再构建）：

```text
# 修复后的构建（run7）
jobs: waiting_configuration / provider_not_configured / requires_user_action=1
# 修复前的构建（同一台设备、同一套探针）
jobs: failed / model_unavailable
```

`jobs-after-p0-transcribe.txt` 里两批任务并存（旧的是修复前的），可以直接对照。

### 4.1 `asrEngineLib` 不能指向沙箱

OHOS 的 linker 只允许从 HAP 的 `libs/arm64` 加载原生库；应用数据目录里的 `.so` 是存在的
（`is_file()` 为真），但 `dlopen` 会被拦。所以 `EngineConfig::resolve_with` 在 OHOS 上**直接
拒绝**这类路径（`provider_not_configured` + 明确说「放进 HAP 的 libs/arm64」），判定函数
`is_ohos_app_data_path` 在宿主上有单测（安装目录 `el1/bundle` 必须放行）。

## 5 没验到的（如实）

- **产品 Profile**：仍是调试 Profile 签的 debug 包；AGC 正式发布签名没做；
- **release / 混淆**：本轮只装了 debug；
- **麦克风现场**：本轮的真麦克风那次得到 `The.`（声学路径不稳定，上一轮也出现过误识）；
  转写证据用的是**已知 PCM**，不是这一轮现场口述；
- **低内存机型**：转写峰值 ~695 MB PSS，只在 31 GB 的 HAD-W32 上跑过；
- **长时间/大库**：语义索引只覆盖几十条记录、几十个块；没有真实大库的构建耗时；
- **两个 ORT 同时做重活**：验证是「先语义、再转写、再语义」，没有让两份 ORT 在**同一毫秒**
  并发跑推理（`Core` 本来就是串行的，这不是本片的范围）；
- **其它 ABI**：只有 arm64-v8a；没有 x86_64 / armeabi-v7a 的 HAP。

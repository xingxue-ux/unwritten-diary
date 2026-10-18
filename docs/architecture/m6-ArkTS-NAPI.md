# M6 · ArkTS 侧的 NAPI 包装（#108 第二块）

对应 issue #108 · 分支 `feat/B8b-arkts-napi` ·
基线：`origin/main` = `e4e3c0a` **合并** `origin/feat/B8a-core-c-abi` = `29a076e`（见 §2）·
执行人 qianxia · 2026-10-18

第一块交付的是 `crates/diary_capi` 的稳定 C ABI（6 个导出符号、句柄 + JSON 进 / JSON 出）。
这一块把它包成 **ArkTS 能直接 `import` 的 NAPI 模块**，并给出可复制的鸿蒙工程接线与
最小示例 Ability。**`crates/diary_core` 与 C ABI 一行未改**：本层只做跨语言搬运。

口径同仓库其它文档：**实测** = 本机跑过并贴出输出；**未验证** = 明确标注；**判断** = 推断。

## 1 摘要（先看这个）

| # | 结论 | 强度 |
|---|---|---|
| 1 | C ABI **不在 main 里**（PR #117 未合）⇒ 本分支从 `origin/main` = `e4e3c0a` 起步，**merge** 了 `origin/feat/B8a-core-c-abi`（`29a076e`），没有改写 C ABI 的任何一行 | 实测（`git log --oneline`） |
| 2 | 模块形状是**三层**：`libdiary_capi.so`（Rust，6 个 C 符号）→ `libdiary_napi.so`（C++，7 个 JS 函数）→ `DiaryCore.ets`（9 条契约方法）；JSON **一进一出**，中间不拆不拼 | 实测（能编 + 静态核对） |
| 3 | NAPI 层**只在「调用没能发生」时抛 JS 异常**（`ERR_DIARY_LIB/ARGS/HANDLE/OPEN/CALL`）；业务错误仍返回 `ok:false` 信封，由 ArkTS 侧统一转成 `DiaryError` 异常 | 实测（代码 + 契约核对） / 设备未验 |
| 4 | **错误码一个没新增**：21 个核心码原样透传；唯一的宿主码 `native_unavailable` 表示「调用没能发生」，不进核心码表 | 实测（`tools/check_arkts_capi_contract.py`） |
| 5 | 交叉编译 + shim 编译过：`libdiary_capi.so` 11,369,880 B（`NEEDED` 只有 `libc.so`）、`libdiary_napi.so` 46,824 B（`NEEDED` = `libace_napi.z.so` / `libc++_shared.so` / `libc.so`）、6 个 C 符号全在 | 实测（见 §8.1） |
| 6 | **ArkTS 真的编过了**：`hvigorw assembleHap` `BUILD SUCCESSFUL`，`CompileArkTS` 8 s，unsigned HAP 12,815,707 B，包内三个 `.so`（含 `libc++_shared.so`） | 实测（见 §8.2） |
| 7 | `index.d.ts` **没有退化成 any**：把 `diaryNapi.open(123)` 故意写错 → 编译报 2 条类型错误（红证据见 §8.3） | 实测 |
| 8 | 静态契约核对：9 个方法名、21 个错误码、6 个 C 符号、7 个 JS 函数、打包三件套全部一致（改坏一处就红，见 §8.4） | 实测 |
| 9 | **真机没验**：`hdc list targets` 为空 ⇒ `dlopen`、沙箱建库、HAP 安装、hilog 输出、性能全部未验证（§9 逐条） | 明确未做 |

**一句话**：ArkTS 现在有一份**能编、能打包、契约对得上**的 `DiaryCore`；离「设备上真的跑通」
只差一台设备——不是代码。

## 2 基线：为什么先 merge 了 B8a

`crates/diary_capi` 仍在 PR #117（`feat/B8a-core-c-abi`），**main（`e4e3c0a`）里没有**：

```console
$ git cat-file -e origin/main:crates/diary_capi/Cargo.toml || echo "NO in main"
NO in main
$ git log --oneline -1 origin/feat/B8a-core-c-abi
29a076e docs(capi): m6 核心 C ABI 的架构说明（#108）
```

因此本分支没有从 main 硬写，而是：

```console
$ git merge --no-edit origin/feat/B8a-core-c-abi
Merge made by the 'ort' strategy.
 crates/diary_capi/Cargo.toml                       |  20 +
 crates/diary_capi/src/lib.rs                       | 540 ++++++++++++++++++
 crates/diary_capi/src/methods.rs                   | 386 +++++++++++++
 crates/diary_capi/src/tests.rs                     | 624 +++++++++++++++++++++
 docs/architecture/m6-核心-C-ABI.md                 | 427 ++++++++++++++++++++++
 7 files changed, 2013 insertions(+), 1 deletion(-)
```

**基线口径**：`e4e3c0a`（最新 main）+ `29a076e`（B8a 的 C ABI），merge commit 在本分支上，
**没有推、没有开 PR**。等 PR #117 合入后，这个 merge commit 会自然消掉。

## 3 模块形状

```
ArkTS（应用代码）
   │  import diaryNapi from 'libdiary_napi.so'   ← 7 个同步函数
   ▼
DiaryCore.ets         拼 {"method","params"}、拆信封、ok:false → DiaryError
   │  call(handle, json) / open(path) / close(handle)
   ▼
libdiary_napi.so      C++ NAPI shim：句柄 id 表、dlerror 诊断、字符串复制与释放
   │  dlopen + dlsym：6 个 C 符号
   ▼
libdiary_capi.so      Rust：diary_open / diary_call / diary_close / diary_version /
                      diary_string_free / diary_last_error（JSON 进 / JSON 出）
```

### 3.1 JS 面（7 个函数，全部同步）

| 函数 | 签名 | 说明 |
|---|---|---|
| `version()` | `() => string` | `diary_capi_version()` 原文，如 `diary_capi/0.1.0 apiVersion=1.0` |
| `open(path)` | `(string) => number` | 打开/创建资料库，返回**句柄 id**；失败抛 `ERR_DIARY_OPEN` |
| `close(handle)` | `(number) => void` | 关闭；未知/已关闭抛 `ERR_DIARY_HANDLE` |
| `call(handle, json)` | `(number, string) => string` | 返回**信封 JSON 原文**；业务错误不抛 |
| `lastError()` | `() => string \| null` | 本线程最近一条错误（JSON），排障用 |
| `selfDir()` | `() => string` | shim 自己被加载的目录（排障第一站） |
| `openHandleCount()` | `() => number` | 未关闭句柄数（泄漏自检） |

核心 `.so` 的加载顺序：`$DIARY_CAPI_LIB`（排障/灰度覆盖）→ 本 shim 同目录的兄弟
`libdiary_capi.so`（绝对路径）→ 裸文件名（进程搜索路径）。探针的设备实录里**这两条路径
都能 `dlopen`**（`dlopen_attempts` 两项都 `ok:true`），先试绝对路径只是让排障输出确定、
不依赖命名空间配置；全失败时错误 message 里带**每一次尝试的路径与 dlerror 原文**。

### 3.2 为什么句柄是 `number` 而不是 `bigint` / 裸指针

`diary_open` 返回 64 位指针，JS 的 `number` 装不下。这里**不把指针交给 JS**，
shim 内部登记成 `1, 2, 3…` 的 `uint32` id：

- 指针不跨语言 ⇒ JS 造不出一个「像指针的数」去喂 C；
- ArkTS 侧不用 `bigint`（JSON、hilog、`number` 运算都不需要特殊处理）；
- `DiaryCore` 把 id 收在私有字段里，应用代码看不到；
- id 只在本进程有效、每次 `open` 都换新、`close` 即摘除 ⇒ 关闭后再用是明确的
  `invalid_state` / `ERR_DIARY_HANDLE`，不是 UB。

> 代价：shim 多一张 `uint32 → void*` 表（约 20 行 + 一把 mutex）。**判断**：这比让 ArkTS
> 处理 bigint 便宜，也比把 64 位指针截成 double 安全。要改成 `bigint` 只动 shim 的
> `open/close/call` 三个函数与 `index.d.ts`，见 §10。

### 3.3 为什么没有导出 `stringFree`

C ABI 的约定是「返回的 `char *` 必须用 `diary_string_free` 释放」。shim 在把字符串交给 JS
**之前**就复制成 `std::string` 并立刻 `diary_string_free` 还回去：

```cpp
std::string out(text);
api->string_free(text);   // 复制完立刻还：C 指针不进 JS
return MakeString(env, out);
```

所以 JS 面**没有**任何 C 指针，也就不需要 `stringFree`，更不会出现「忘了释放 / 重复释放」。
ArkTS 侧拿到的永远是 JS 字符串，由 GC 管。

## 4 错误映射

### 4.1 三层各管一段

| 层 | 失败时 | 形态 |
|---|---|---|
| `diary_capi` | `diary_open` 返回 NULL + `diary_last_error()`；`diary_call` 返回 `ok:false` 信封 | JSON（`code/message/retryable`） |
| `libdiary_napi.so` | **只在「调用没能发生」时**抛 JS `Error`，`code` = `ERR_DIARY_*` | JS 异常 |
| `DiaryCore.ets` | **所有**失败都抛 `DiaryError`，`code` = 核心码或 `native_unavailable` | ArkTS 异常 |

「调用没能发生」= 库没加载、参数类型不对、句柄未知、`open` 失败、内存分配失败。
**业务错误不抛**：`call()` 照常返回 `ok:false` 信封——因为信封是 C ABI 已经冻结的协议，
它同时服务于日志、回放、离线比对；把「信封 → 异常」这一步放在 ArkTS 侧只做一次，
C++ 层就不用理解业务码。

### 4.2 逐条映射

| 场景 | C 侧 | NAPI shim | `DiaryCore` |
|---|---|---|---|
| `open` 失败（路径非法 / 权限 / 磁盘满） | NULL + `last_error` JSON | 抛 `ERR_DIARY_OPEN`，message = **`last_error` 的 JSON 原文** | 从 message 里还原出**真实核心码**（如 `permission_denied`） |
| `call` 业务错误 | `ok:false` 信封 | 原样返回字符串（不抛） | 抛 `DiaryError(code, message, retryable, apiVersion)` |
| `call` 成功 | `ok:true` 信封 | 原样返回字符串 | 返回 `data`（类型见 §5） |
| 核心 `.so` 没打进包 / 符号不全 | — | 抛 `ERR_DIARY_LIB`，message 带 **每次 dlopen 的路径 + dlerror** | `native_unavailable`，message 保留原文 |
| 参数类型错（如 `open(123)`） | — | 抛 `ERR_DIARY_ARGS` | `native_unavailable` |
| 句柄未知 / 已关闭（shim 侧） | — | 抛 `ERR_DIARY_HANDLE` | `native_unavailable`；但 `DiaryCore.close()` 之后再调方法会先被 `ensureOpen()` 拦成核心码 `invalid_state` |
| `diary_call` 返回 NULL（仅分配失败） | NULL | 抛 `ERR_DIARY_CALL` | `native_unavailable` |
| 信封不是合法 JSON | — | — | `native_unavailable`（说明 shim/C ABI 坏了） |

### 4.3 错误码：21 个核心码 + 1 个宿主码

`CORE_ERROR_CODES`（`DiaryError.ets`）与 `crates/diary_core/src/error.rs` 的
`ErrorCode::wire()` **逐字、同序**一致，由 `tools/check_arkts_capi_contract.py` 核对：

```text
not_found, invalid_state, invalid_argument, revision_conflict, idempotency_conflict,
storage_full, asset_missing, permission_denied, integrity_failed, unsupported_format,
search_expired, cursor_expired, provider_not_configured, authentication_failed,
network_unavailable, rate_limited, quota_exceeded, provider_timeout, model_unavailable,
corrupted_backup, incompatible_backup
```

`native_unavailable` 是**宿主码**，不在核心码表里，只表示「调用没能发生」；出现它 = 打包或
部署有问题（`.so` 缺失、`libc++_shared.so` 没随包、ABI 目录不对），不是业务错误。

> 口径提醒：C ABI 文档 §5.1 成文时写的是「核心那 14 个码」，而
> `crates/diary_core/src/error.rs` 的 `ErrorCode::wire()` 现在是 **21 个**
>（后来加了 Provider / 备份相关的码）。本文与 `DiaryError.ets` 以**代码**为准，
> 检查器直接读 `error.rs` 核对。

### 4.4 为什么 `open` 抛而 `call` 不抛

`diary_open` 的失败**只能**返回 NULL（C 侧没有信封），原因在线程内的 `diary_last_error()`
里。shim 把那段 JSON 原样放进异常的 message，ArkTS 侧再解析回 `code/message/retryable`
——所以 `open` 的失败**仍然带着真实核心码**，只是多绕了一层。

`call` 不一样：它**永远**有信封（成功或失败）。在这里就抛等于让 C++ 层理解业务码，
还要在 JS 异常里重建 `apiVersion`；没有必要，也不利于回放。

## 5 ArkTS 薄封装 ↔ C ABI 的对应

`DiaryCore.ets` 的 9 个方法**一一对应** `crates/diary_capi/src/methods.rs` 的
`WIRED_METHODS`（方法体里的字符串与方法名必须一致，检查器会核对）：

| ArkTS 方法 | C ABI 方法 | 参数类型 | 返回类型 |
|---|---|---|---|
| `coreInfo()` | `core.info` | 无 | `LibraryInfo` |
| `createDraft(params)` | `captures.createDraft` | `CreateDraftParams` | `Capture` |
| `saveDraft(params)` | `captures.saveDraft` | `SaveDraftParams` | `DraftSaveResult` |
| `commit(params)` | `captures.commit` | `CommitParams` | `CommitResult` |
| `extract(params)` | `sources.extract` | `ExtractParams` | `ExtractedContent` |
| `search(params)` | `search.start` | `SearchParams` | `SearchSnapshot` |
| `rebuildIndex(params)` | `indexes.rebuild` | `RebuildIndexParams` | `Job` |
| `runDueJobs(params)` | `jobs.runDue` | `RunDueParams` | `JobRunSummary` |
| `jobsStatus()` | `jobs.status` | 无 | `JobStatus` |
| `rawCall(method, params)` | 任意 | `Object \| null` | `Object \| null`（`data`） |
| `rawEnvelope(method, params)` | 任意 | `Object \| null` | 信封原文 `string` |

约定：

- **字段名 snake_case 原样**（`capture_id` / `expected_revision` / `operation_id` …）。
  C ABI 同时接受契约表的 camelCase 别名，但 ArkTS 侧只用一种，避免同一个字段两种拼法；
- 返回值就是信封里的 `data`，不做二次翻译（`Models.ets` 的接口是核心 serde 输出的子集，
  多出来的字段运行时仍在）；
- `search` 的 `params` 是 `{ request, query_revision }`（C ABI 的形状），
  不是扁平的 `{ query }`——因为 `query_revision` 是检索会话的一部分；
- 方法子集只有 9 条：`capabilities` 里没列出来的就是还没接，不会假装支持。

## 6 库放置与打包

### 6.1 三个文件，一个目录

```
entry/libs/arm64-v8a/
├── libdiary_capi.so     Rust cdylib（11,369,880 B；NEEDED 只有 libc.so）
├── libdiary_napi.so     C++ NAPI shim（46,824 B；NEEDED: libace_napi.z.so / libc++_shared.so / libc.so）
└── libc++_shared.so     1,262,504 B（**必须随包**，见 §6.3）
```

hvigor 的 **ProcessLibs** 会自动把它们收进 HAP 的 `libs/arm64-v8a/`，并顺手 strip。
**不需要** `externalNativeOptions`、**不需要** `CMakeLists.txt`（探针 §24.3 的结论）。

### 6.2 `build-native.sh` 做了什么

1. `cargo build -p diary_capi --release --target aarch64-unknown-linux-ohos`，
   只设三个环境变量（linker / `CC_aarch64_unknown_linux_ohos` / `AR_…`），
   `CARGO_BUILD_JOBS` 默认 4，`CARGO_TARGET_DIR` 默认 `~/.cache/diary-ohos/target`
   （**不落进仓库、不用 /tmp**）；
2. NDK `aarch64-unknown-linux-ohos-clang++` 编 `napi_shim.cpp` →
   `libdiary_napi.so`，`-lace_napi.z -Wl,--no-undefined -Wall -Wextra -Werror`；
3. `cp $NDK/llvm/lib/aarch64-linux-ohos/libc++_shared.so entry/libs/arm64-v8a/`；
4. 自检：`file`、`llvm-readelf -d`（NEEDED/SONAME）、逐个确认 `diary_capi.so` 的
   **6 个符号**都在、`libdiary_napi.so` 里没有意外的 `UND`。

`-Wl,--no-undefined` 是有意加的：探针 §24.2 的坑是「符号没定义，**链接时不报**，
设备上 `dlopen` 才炸」。本 shim 不直接链接 `libdiary_capi.so`（运行时 `dlopen`），
所以链接期的 `UND` 只应来自 `libc` / `libc++` / `libace_napi.z`。

### 6.3 `libc++_shared.so` 必须随包

设备上 `/system/lib64/libc++_shared.so` 确实存在，但应用进程的 linker namespace 看不到它
（探针 §24.1 实测）：

```text
W MUSL-LDSO: load libc++_shared.so failed, namespace=ndk no inherits, errno=2
W MUSL-LDSO: Error loading shared library libc++_shared.so:
              (needed by /data/storage/el1/bundle/libs/arm64/libdiary_napi.so)
E ArkCompiler: [ecmascript] export objects of native so is undefined …
```

症状是 ArkTS 侧 `import` 得到 `undefined`，**只有全量 hilog 里才有真因**。
本工程把它放在同一个 `libs/arm64-v8a/` 下，`build-hap.sh` 会打印包内清单核对。
**不要重复放**：所有 C++ 原生库共用这一份。

### 6.4 `import` 的三件套（探针 §24.3）

1. `entry/src/main/cpp/types/libdiary_napi/index.d.ts` + `oh-package.json5`
   （后者 `"name": "libdiary_napi.so"`、`"types": "./index.d.ts"`）；
2. `entry/oh-package.json5`：`"libdiary_napi.so": "file:./src/main/cpp/types/libdiary_napi"`；
3. **`ohpm install --all`**（离线、本次实测 107 ms）：生成
   `entry/oh_modules/libdiary_napi.so → ../src/main/cpp/types/libdiary_napi` 软链。

缺第 3 步的症状：编译报 `arkts-no-any-unknown`，模块退化成 `any`。`build-hap.sh` 已经
内置这一步（`SKIP_OHPM=1` 可跳过）。

### 6.5 HAP 里长什么样（实测）

```text
libs/arm64-v8a/libdiary_napi.so      30,824   （46,824 → hvigor strip）
libs/arm64-v8a/libc++_shared.so   1,262,504
libs/arm64-v8a/libdiary_capi.so  11,369,640   （11,369,880 → strip）
```

unsigned HAP **12,815,707 B**。体积账：核心 11.37 MB + C++ 运行时 1.26 MB + shim 30 KB
≈ **12.66 MB**（另有 ~0.15 MB 的 HAP 自身/资源）。这与第一块文档 §10 的 9.59 MB 不同：
那次是**单独**编 `diary_capi` 的旧口径，这次合并了最新 main 后依赖更多
（`pdf-extract` / `tokenizers` / `jieba` / `zip` 全量），**HAP 体积账要按 12.66 MB 重算**。

> 这与探针 §29 的体积账不冲突：探针带的是「薄 ABI + ORT」，本工程是「全量核心、无 ORT」。

## 7 完整命令序列

### 7.1 本机（本次实际跑过的顺序）

```bash
# 0. 基线：main + B8a 的 C ABI
cd /home/qianxia/wt/napi108
git log --oneline -4

# 1. 只编不测（任务书要求：不要 cargo test --workspace）
CARGO_BUILD_JOBS=4 cargo check -p diary_capi

# 2. 原生库：交叉编译 + shim + libc++_shared + 自检
cd apps/ohos-diary-core
CARGO_BUILD_JOBS=4 ./scripts/build-native.sh

# 3. HAP：ohpm install + hvigorw assembleHap
./scripts/build-hap.sh

# 4. 契约静态核对 + 文档检查
cd ../..
python3 tools/check_arkts_capi_contract.py
python3 tools/check_docs.py
```

复现单步：

```bash
# 只重编 shim（Rust 产物不变）
SKIP_RUST=1 apps/ohos-diary-core/scripts/build-native.sh

# 只重打 HAP（原生库、ohpm 都复用）
SKIP_NATIVE=1 SKIP_OHPM=1 apps/ohos-diary-core/scripts/build-hap.sh

# 单独交叉编译核心（与 C ABI 文档 §10 同一配方）
export NDK=~/command-line-tools/sdk/default/openharmony/native
export CARGO_TARGET_DIR="$HOME/.cache/diary-ohos/target"
export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_OHOS_LINKER="$NDK/llvm/bin/aarch64-unknown-linux-ohos-clang"
export CC_aarch64_unknown_linux_ohos="$NDK/llvm/bin/aarch64-unknown-linux-ohos-clang"
export AR_aarch64_unknown_linux_ohos="$NDK/llvm/bin/llvm-ar"
CARGO_BUILD_JOBS=4 cargo build -p diary_capi --release --target aarch64-unknown-linux-ohos
```

### 7.2 设备（**本次未执行**：`hdc list targets` 为空）

```bash
hdc list targets

# 先签名再装（unsigned HAP 装不上；签名环境变量见 scripts/build-hap.sh 顶部）
OHOS_SIGN_STORE_FILE=... OHOS_SIGN_CERT_FILE=... OHOS_SIGN_PROFILE_FILE=... \
OHOS_SIGN_STORE_PASSWORD_ENC=... OHOS_SIGN_KEY_PASSWORD_ENC=... \
  apps/ohos-diary-core/scripts/build-hap.sh
hdc install -r apps/ohos-diary-core/entry/build/default/outputs/default/entry-default-signed.hap

# 抓结果（tag DiaryCore；JSON 是 DIARY_SMOKE JSON[label][i/n] 分片）
hdc shell "hilog -x" | grep DiaryCore
```

## 8 实测

### 8.0 `cargo check`（实测）

任务书要求不跑 `cargo test --workspace`（机器上还有两条 cargo 重线），只做单包检查：

```console
$ CARGO_BUILD_JOBS=4 cargo check -p diary_capi
    Checking diary_core v0.1.0 (/home/qianxia/wt/napi108/crates/diary_core)
    Checking diary_capi v0.1.0 (/home/qianxia/wt/napi108/crates/diary_capi)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 41.79s
```

### 8.1 交叉编译 + shim（实测）

```console
$ CARGO_BUILD_JOBS=4 apps/ohos-diary-core/scripts/build-native.sh
== [1/3] cargo build -p diary_capi --release --target aarch64-unknown-linux-ohos
    Finished `release` profile [optimized] target(s) in 2m 23s
== [2/3] clang++ -shared (libdiary_napi.so)
== [3/3] 产物自检

-- libdiary_capi.so  bytes=11369880
ELF 64-bit LSB shared object, ARM aarch64, version 1 (SYSV), dynamically linked, stripped
  0x0000000000000001 (NEEDED)       Shared library: [libc.so]

-- libdiary_napi.so  bytes=46824
ELF 64-bit LSB shared object, ARM aarch64, version 1 (SYSV), dynamically linked, with debug_info, not stripped
  0x0000000000000001 (NEEDED)       Shared library: [libace_napi.z.so]
  0x0000000000000001 (NEEDED)       Shared library: [libc++_shared.so]
  0x0000000000000001 (NEEDED)       Shared library: [libc.so]
  0x000000000000000e (SONAME)       Library soname: [libdiary_napi.so]

-- libc++_shared.so  bytes=1262504
ELF 64-bit LSB shared object, ARM aarch64, version 1 (SYSV), dynamically linked, … stripped
  0x0000000000000001 (NEEDED)       Shared library: [libc.so]
  0x000000000000000e (SONAME)       Library soname: [libc++_shared.so]

-- libdiary_capi.so 导出（NAPI shim 要 dlsym 的 6 个）
   OK  diary_capi_version
   OK  diary_open
   OK  diary_close
   OK  diary_call
   OK  diary_string_free
   OK  diary_last_error
-- libdiary_napi.so 导出（NAPI 模块注册）
    63: 0000000000003f74    12 FUNC    GLOBAL DEFAULT    12 RegisterDiaryNapiModule
-- libdiary_napi.so 里不该出现的未定义符号（只允许 libc/libc++/libace_napi 提供）
   （空 = 没有意外的 UND）

OK: entry/libs/arm64-v8a/{libdiary_capi.so,libdiary_napi.so,libc++_shared.so}
```

补充核对（HAP 里 strip 之后仍然有效）：

```console
$ llvm-readelf -d libs/arm64-v8a/libdiary_napi.so | grep -E "NEEDED|INIT"
 (NEEDED)  Shared library: [libace_napi.z.so]
 (NEEDED)  Shared library: [libc++_shared.so]
 (NEEDED)  Shared library: [libc.so]
 (INIT_ARRAY)   0x8b30      (INIT_ARRAYSZ) 16 (bytes)

$ llvm-readelf -r libs/arm64-v8a/libdiary_napi.so | grep RegisterDiary
0000000000008b38 R_AARCH64_ABS64 0000000000003f74 RegisterDiaryNapiModule + 0
```

⇒ 构造函数在 `.init_array` 里、`napi_module_register` 会被调用（模块注册的静态证据）。

### 8.2 HAP（实测）

```console
$ SKIP_NATIVE=1 apps/ohos-diary-core/scripts/build-hap.sh
[build-hap] ohpm install --all
install completed in 0s 107ms
> hvigor Finished :entry:default@CompileArkTS... after 8 s 115 ms
> hvigor Finished :entry:default@PackageHap... after 1 s 31 ms
> hvigor BUILD SUCCESSFUL in 13 s 510 ms
[build-hap] entry-default-unsigned.hap  bytes=12815707  sha256=20e93f29…
[build-hap] 包内原生库：
    libs/arm64-v8a/libdiary_napi.so      30,824
    libs/arm64-v8a/libc++_shared.so   1,262,504
    libs/arm64-v8a/libdiary_capi.so  11,369,640
```

`CompileArkTS` 通过 = `DiaryCore.ets` / `DiaryError.ets` / `Envelope.ets` /
`Models.ets` / `Smoke.ets` / `EntryAbility.ets` 与 `index.d.ts` **全部通过 ArkTS 类型检查**。

### 8.3 能红证据：`index.d.ts` 不是 any（实测）

临时把 `EntryAbility.ets` 改成：

```ts
import diaryNapi from 'libdiary_napi.so';
const bad: string = diaryNapi.open(123);
```

```console
$ SKIP_NATIVE=1 SKIP_OHPM=1 apps/ohos-diary-core/scripts/build-hap.sh
> hvigor ERROR: Failed :entry:default@CompileArkTS...
1 ERROR: 10505001 ArkTS Compiler Error
Error Message: Type 'number' is not assignable to type 'string'.
  At File: .../entry/src/main/ets/entryability/EntryAbility.ets:21:11
2 ERROR: 10505001 ArkTS Compiler Error
Error Message: Argument of type 'number' is not assignable to parameter of type 'string'.
  At File: .../entry/src/main/ets/entryability/EntryAbility.ets:21:40
COMPILE RESULT:FAIL {ERROR:3}
```

⇒ 参数类型与返回类型都被检查了（若模块退化成 `any`，两处都不会报错）。
改回后重新构建 `BUILD SUCCESSFUL`（§8.2）。

### 8.4 契约静态核对（实测）

```console
$ python3 tools/check_arkts_capi_contract.py
[核对] 契约方法（DiaryCore.ets ↔ WIRED_METHODS）：9 项一致
[核对] 错误码（DiaryError.ets ↔ ErrorCode::wire）：21 项一致
[核对] C 符号（napi_shim.cpp dlsym ↔ lib.rs 导出）：6 项一致
[核对] JS 函数（index.d.ts ↔ napi_define_properties）：7 项一致
[核对] 三件套：libdiary_napi.so → ./src/main/cpp/types/libdiary_napi（ohpm install 后 ArkTS 才有类型）
[通过] ArkTS 包装与 C ABI 的方法名、错误码、符号、类型声明、打包三件套一致。
```

红证据：在 `DiaryCore.ets` 里加一条 `invokeData('captures.nope', …)` →

```console
[失败] 契约方法（DiaryCore.ets ↔ WIRED_METHODS）：ArkTS/shim 多出（C ABI 没有）['captures.nope']
共 1 处问题。   （exit 1）
```

## 9 未验证清单（如实）

1. **真机 `dlopen` / `dlsym`**：`hdc list targets` 为空，没跑过。探针在**它自己的**
   `libdiary_core_smoke.so` 上验过 dlopen，**不是这一份** `libdiary_capi.so`；
2. **沙箱建库 / 落盘**：`filesDir` 下建 `library.sqlite`、SQLite WAL、迁移在 OHOS 沙箱里
   是否都正常——没跑过；
3. **HAP 安装与启动**：本次产物是 **unsigned** HAP，装不上；签名用的 Profile 没配；
4. **`import diaryNapi from 'libdiary_napi.so'` 在设备上的运行时语义**：编译期验过
   `index.d.ts` 生效（§8.3），但「设备上 default import 拿到的就是 exports 对象」只在
   探针（别的模块、别的 d.ts 写法）上验过；
5. **`libc++_shared.so` 随包之后是否真的不再报那个错**：探针验过同一做法，本工程未上机；
6. **性能**：库加载时间、JSON 往返、检索首屏、11.37 MB `.so` 的启动开销——都没测；
7. **主线程阻塞**：`DiaryCore` 是同步的，示例在 `onWindowStageCreate` 里直接跑。
   真实库上的 `search()` / `runDueJobs()` 会不会卡住 UI，没量过；
8. **并发**：shim 句柄表有锁、C ABI 核心串行，但「多线程同时 call 同一 DiaryCore」没压测；
9. **多进程 / 多 Ability**：C ABI 的「同一路径共享一个核心」只在**进程内**成立，
   第二个进程会各开一个核心——OHOS 上这会不会发生、后果如何，没验；
10. **错误路径**：权限拒绝、磁盘满、库文件损坏、`revision_conflict` 在设备上的实际表现——没触发过；
11. **release 构建 / 混淆**：只打了 debug、没开混淆；release 下的 ArkTS 优化未验；
12. **升级 / 回滚**：`data_schema_version` 与旧库的兼容没验；
13. **hilog 输出本身**：`Smoke.ets` 的日志代码完整，但一行都没在设备上出现过；
14. **C++ 侧 OOM 没兜底**：shim 里的 `std::string` / 容器分配失败是 `std::bad_alloc`，
    会以 C++ 异常穿透 NAPI 边界（进程中止）。这是 OOM 级的不可恢复错误，没有兜底；
    核心自己的 panic 仍然由 `diary_capi` 的 `catch_unwind` 拦住（C ABI 文档 §8）。

## 10 需要拍板的地方

1. **错误口径：抛异常（本块已选）还是返回信封？**
   - 现状：NAPI 层「调用没能发生」抛 `ERR_DIARY_*`；业务错误返回 `ok:false` 信封；
     `DiaryCore` 统一抛 `DiaryError`。业务代码只写 `try/catch`，不会漏判 `ok`；
   - 另一种是「一律返回 `{ok, data?, error?}`」：调用点必须每次判 `ok`，漏一次就把错误
     当数据用；好处是不用 `try/catch`、异步化时更自然；
   - **我的建议**：保持抛异常（ArkTS 的 `try/finally` 与 `close()` 天然配套），
     同时保留 `rawEnvelope()` 给需要原始信封的场景。若要改，改动面 =
     `DiaryCore` 的 12 个方法 + 示例的调用点，**不涉及 C ABI**。
2. **模块归属**：现在放在 `apps/ohos-diary-core`（可复制的参考工程）。要不要抽成
   独立 HAR / 独立仓包（`@diary/core`），由应用 `ohpm install` 依赖？抽的话要处理
   「HAR 里怎么带 `.so` 与 `types` 三件套」——本块没有验证 HAR 形态。
3. **同步还是异步**：现在是同步（先跑通）。产品要放到 TaskPool / worker，接口可能变成
   `Promise<T>`；这会改 `DiaryCore` 的签名与示例。
4. **句柄 `number` id vs `bigint` 指针**：现在 `number` id（§3.2）。若坚持「句柄就是指针」，
   shim 改成 `napi_create_bigint_uint64`，ArkTS 类型改 `bigint`；代价是 bigint 在
   JSON / hilog 里都要特殊处理。
5. **`stringFree` 要不要暴露**：现在不暴露（内部复制+释放，§3.3）。暴露它只会多一类
   「重复释放」的错误，没有收益。
6. **方法子集**：C ABI 只接了 9 条；ArkTS 也只包了这 9 条。每加一条 C ABI 方法，
   只需在 `DiaryCore.ets` 加一个方法（检查器会抓漏），不用动 C++。
7. **`native_unavailable` 这个宿主码**：要不要换成 `invalid_state`（复用核心码）？
   我倾向保留：它能一眼区分「打包坏了」与「业务状态不对」，且不进核心码表。
8. **OHOS 构建进不进 CI**：本机命令已经跑通（约 2–3 分钟首次、增量 ~15 秒），但 CI 要有
   OHOS NDK + hvigor + ohpm。要进 CI 得先决定 runner 镜像。

## 11 后续（本块没做，按优先级）

1. 真机上跑一遍（装签名 HAP → 抓 hilog → 确认 `DIARY_SMOKE DONE` 与 `CLOSED handles=0`）；
2. 把示例链路挪到 TaskPool（异步包装），量 UI 线程阻塞；
3. 补 `open` 失败 / `revision_conflict` / 权限拒绝的设备侧用例；
4. 性能：库加载、JSON 往返、检索首屏；
5. 方法面继续接（`captures.list` / `jobs.list` / `indexes.status` …），ArkTS 侧同步加；
6. 决定 HAR 化与 CI。

# ArkTS NAPI 参考工程（#108 第二块）

把已经交付的 C ABI（`crates/diary_capi`，句柄 + JSON 进 / JSON 出）包成 ArkTS 能直接
`import` 的模块，并给一个最小示例 Ability：**打开库 → coreInfo → 建草稿 → 保存 → 提交 →
抽取 → 检索 → 排索引任务 → 跑任务 → 关闭**，结果全部打到 hilog。

设计与验证记录在 [`docs/architecture/m6-ArkTS-NAPI.md`](../../docs/architecture/m6-ArkTS-NAPI.md)；
C ABI 本身见 [`docs/architecture/m6-核心-C-ABI.md`](../../docs/architecture/m6-核心-C-ABI.md)。

> ⚠️ **真机没验**：开发机上 `hdc list targets` 为空。这里做到的是「本机能编、HAP 能出、
> 类型契约静态对得上」；`dlopen`、沙箱建库、HAP 安装、性能都没在设备上跑过
> （逐条清单见文档 §9）。

## 目录

```
apps/ohos-diary-core/
├── entry/src/main/cpp/
│   ├── napi_shim.cpp                       # libdiary_napi.so 的唯一源码（7 个同步函数）
│   └── types/libdiary_napi/                # ArkTS 类型三件套之一、之二
│       ├── index.d.ts
│       └── oh-package.json5
├── entry/src/main/ets/
│   ├── core/DiaryCore.ets                  # 薄封装：9 条契约方法 + rawCall/rawEnvelope
│   ├── core/DiaryError.ets                 # 21 个核心码 + native_unavailable
│   ├── core/Envelope.ets                   # 请求拼装 / 响应拆封
│   ├── core/Models.ets                     # data 与参数的 ArkTS 类型（snake_case）
│   ├── core/Smoke.ets                      # 示例链路（hilog）
│   ├── entryability/EntryAbility.ets       # 启动时跑一遍
│   └── pages/Index.ets
└── scripts/
    ├── build-native.sh                     # Rust cdylib + C++ shim + libc++_shared → entry/libs/
    └── build-hap.sh                        # ohpm install + hvigorw assembleHap
```

## 快速开始（本机，三条命令）

```bash
cd apps/ohos-diary-core

# 1. 原生库：cargo 交叉编译 + NDK clang++ 编 shim + 复制 libc++_shared.so（约 2–3 分钟）
./scripts/build-native.sh

# 2. HAP：ohpm install（本地 file: 依赖，离线）+ hvigorw assembleHap（约 14 秒）
./scripts/build-hap.sh

# 3. 装到设备（**未验证**：本机没有设备）
hdc install -r entry/build/default/outputs/default/entry-default-unsigned.hap
hdc shell "hilog -x" | grep DiaryCore
```

环境要求（与探针一致）：`rustup target add aarch64-unknown-linux-ohos`、OHOS NDK
（默认 `~/command-line-tools/sdk/default/openharmony/native`）、`ohpm` 与 `hvigorw`
（默认 `~/command-line-tools/bin/`）。**不需要 CMake、不需要 DevEco GUI、不需要 nightly。**

`build-native.sh` 的产物：

| 文件 | 大小（本机实测） | 说明 |
|---|---|---|
| `entry/libs/arm64-v8a/libdiary_capi.so` | 11,369,880 B | Rust cdylib，`NEEDED` 只有 `libc.so` |
| `entry/libs/arm64-v8a/libdiary_napi.so` | 46,824 B | C++ NAPI shim，`NEEDED` = `libace_napi.z.so` / `libc++_shared.so` / `libc.so` |
| `entry/libs/arm64-v8a/libc++_shared.so` | 1,262,504 B | **必须随包**（见下） |

## 接进自己的鸿蒙工程：五步

1. **原生库**：把 `crates/diary_capi` 交叉编译出的 `libdiary_capi.so`、本工程的
   `napi_shim.cpp` 编出的 `libdiary_napi.so`、NDK 里的 `libc++_shared.so` 一起放进
   `entry/libs/arm64-v8a/`（hvigor 的 ProcessLibs 会自动收进 HAP，**不需要**
   `externalNativeOptions` / `CMakeLists.txt`）。
2. **类型声明**：把 `entry/src/main/cpp/types/libdiary_napi/` 整个目录拷过去
   （`index.d.ts` + `oh-package.json5`，后者 `"name": "libdiary_napi.so"`、
   `"types": "./index.d.ts"`）。
3. **登记本地依赖**：`entry/oh-package.json5` 加一行
   `"libdiary_napi.so": "file:./src/main/cpp/types/libdiary_napi"`。
4. **`ohpm install --all`**（离线、几十毫秒）：这一步生成 `entry/oh_modules/libdiary_napi.so`
   软链。**少了它会报 `arkts-no-any-unknown`，模块退化成 any**（探针 §24.3 的坑）。
5. **import**：`import diaryNapi from 'libdiary_napi.so';` 或直接用本工程的
   `core/DiaryCore.ets`。

## 为什么 `libc++_shared.so` 必须随包

设备上 `/system/lib64/libc++_shared.so` **确实存在**，但应用进程的 linker namespace
看不到它（探针 §24.1 实测）：

```text
W MUSL-LDSO: load libc++_shared.so failed, namespace=ndk no inherits, errno=2
W MUSL-LDSO: Error loading shared library libc++_shared.so:
              (needed by /data/storage/el1/bundle/libs/arm64/libdiary_napi.so)
E ArkCompiler: [ecmascript] export objects of native so is undefined …
```

症状是 ArkTS 侧 `import` 拿到 `undefined`，**只有全量 hilog 里才有真因**。
`build-native.sh` 已经把 `$NDK/llvm/lib/aarch64-linux-ohos/libc++_shared.so` 复制进
`entry/libs/arm64-v8a/`；`build-hap.sh` 会打印包内 `libs/` 清单供核对。

## 示例输出长什么样（未在设备上跑过，形状来自设计）

```text
DiaryCore  DIARY_SMOKE START db=/data/storage/el2/base/haps/entry/files/library.sqlite
DiaryCore  DIARY_SMOKE nativeLibraryDir=/data/storage/el1/bundle/libs/arm64
DiaryCore  DIARY_SMOKE capiVersion=diary_capi/0.1.0 apiVersion=1.0
DiaryCore  DIARY_SMOKE open OK handles=1
DiaryCore  DIARY_SMOKE JSON[core.info][1/1] {"api_version":"1.0",…}
…
DiaryCore  DIARY_SMOKE search hits=1 phase=done
DiaryCore  DIARY_SMOKE DONE processed=1 succeeded=1
DiaryCore  DIARY_SMOKE CLOSED handles=0
```

`CLOSED handles=0` 是句柄泄漏自检（`openHandleCount()` 来自 shim 的登记表）。

## 本机验证过什么

- `./scripts/build-native.sh`：交叉编译 + shim 编译 + 自检全过（见文档 §6）；
- `./scripts/build-hap.sh`：`BUILD SUCCESSFUL`，unsigned HAP 12,815,707 B，
  包内含三个 `.so`（含 `libc++_shared.so`）；
- 把 `diaryNapi.open(123)` 故意写错做红测试：`CompileArkTS` 报
  `Argument of type 'number' is not assignable to parameter of type 'string'`
  ⇒ **`index.d.ts` 真的被用上了**，没有退化成 any；
- `python3 tools/check_arkts_capi_contract.py`：方法名 / 21 个错误码 / 6 个 C 符号 /
  7 个 JS 函数 / 打包三件套全部一致。

**没验的**：真机 `dlopen` 与 `dlsym`、沙箱里建库与 SQLite 落盘、HAP 安装与启动、
hilog 输出、性能与主线程阻塞。逐条见文档 §9。

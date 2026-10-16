# 核心进 HAP 探针（`probes/ohos-core-smoke`）

> 目的：把 `diary_core` 编成 OHOS 共享库塞进 HAP，在**应用沙箱**里跑通 SQLite，
> 并顺便回答 **ONNX Runtime 能不能在沙箱里 dlopen + 推理**（语义检索的命门）。
> **不含任何产品代码**：`crates/**`、`packages/**`、`apps/**` 一行未动。
> bundleName 用**临时值** `com.xingxue.unwrittendiary.coresmoke`，真机跑的时候用
> `OHOS_BUNDLE_NAME=com.example.myapplication` 覆盖（沿用第 1 步的 Profile 约束）。
>
> 结论、原始输出与完整数字见 `docs/architecture/m6-鸿蒙可行性探针.md` 的
> 「续篇二：核心进 HAP」（§18–§28）。

## 目录结构

```
probes/ohos-core-smoke/
├── AppScope/app.json5                  bundleName / 版本号 / 图标
├── build-profile.json5                 compatible/targetSdkVersion "6.1.0(23)" + signingConfigs(空)
├── hvigor/hvigor-config.json5          modelVersion 26.0.0
├── oh-package.json5                    工程级依赖（无）
├── entry/
│   ├── oh-package.json5                ★ 本地 file: 依赖 "libdiary_napi.so"（ArkTS import 要用）
│   ├── build-profile.json5             apiType: stageMode
│   ├── libs/arm64-v8a/                 ★ 原生库落点（构建产物，.gitignore）
│   └── src/main/
│       ├── module.json5                deviceTypes: 2in1/tablet/phone
│       ├── cpp/
│       │   ├── napi_shim.cpp           C++ NAPI shim：dlopen 核心 + 「从 el2 dlopen」对照实验
│       │   ├── ort_probe.cpp           ★ ORT C API 探针（dlopen/版本/session/真实推理）
│       │   ├── ort16/onnxruntime_c_api.h   ORT 1.16.3 官方头（保证 OrtApi 表布局一致）
│       │   └── types/libdiary_napi/    ArkTS 侧的类型声明（index.d.ts + oh-package.json5）
│       ├── ets/entryability/EntryAbility.ets  ★ 证据都从这里打 hilog（tag=DiaryCore）
│       ├── ets/pages/Index.ets         空壳页面
│       └── resources/rawfile/tiny_add.onnx    121 B 的最小推理模型（C = A + B）
├── rust/
│   ├── Cargo.toml                      cdylib；自带空 [workspace]（不进产品 workspace）
│   └── src/lib.rs                      ★ 薄 C ABI + 宿主对照实验
└── scripts/
    ├── build-native.sh                 编 .so（Rust cdylib + C++ shim + libc++_shared + ORT）
    ├── build-hap.sh                    构建（可签名；含 ohpm install / bundleName 覆盖）
    ├── run-on-device.sh                安装 → 启动 → 抓 hilog
    └── ohos-sign-pwd.js                签名口令加解密（沿用第 1 步的工具）
```

## 命令序列（就是要进 CI 的那条）

```bash
# ── 0. 前置（本机已具备）
#   ~/command-line-tools        Linux x64 命令行工具 26.0.0.821（API 26 SDK，自带 hvigor/ohpm/cmake/node）
#   rustup target add aarch64-unknown-linux-ohos      （tier-2，官方预编译 std，不需要 nightly）
#   java 21 在 PATH 上（签名那一步用）
#   调试签名材料在 ~/.ohos/config/（p12 + cer + p7b + material/），Profile 覆盖 com.example.myapplication

cd probes/ohos-core-smoke

# ── 1. 编原生库 → entry/libs/arm64-v8a/
export NDK=$HOME/command-line-tools/sdk/default/openharmony/native
export CARGO_TARGET_DIR=/tmp/ohos-core-smoke/target
export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_OHOS_LINKER=$NDK/llvm/bin/aarch64-unknown-linux-ohos-clang
export CC_aarch64_unknown_linux_ohos=$NDK/llvm/bin/aarch64-unknown-linux-ohos-clang
export AR_aarch64_unknown_linux_ohos=$NDK/llvm/bin/llvm-ar
./scripts/build-native.sh
#   → libdiary_core_smoke.so（Rust，3.87 MB）/ libdiary_napi.so（C++ shim，48 KB）
#   → libc++_shared.so（**必须**，见 README 坑 1）/ libonnxruntime.so（ORT，可选）
#   跳过某一项：SKIP_RUST=1 / SKIP_SHIM=1 / SKIP_ORT=1；ORT_SO_DIR=<dir> 指向已有的 ORT

# ── 2. 构建 + 签名 → entry-default-signed.hap
OHOS_BUNDLE_NAME=com.example.myapplication \
OHOS_SIGN_STORE_FILE=…p12 OHOS_SIGN_CERT_FILE=…cer OHOS_SIGN_PROFILE_FILE=…p7b \
OHOS_SIGN_STORE_PASSWORD_ENC=0000001A… OHOS_SIGN_KEY_PASSWORD_ENC=0000001A… \
  ./scripts/build-hap.sh

# ── 3. 装到真机 → 启动 → 抓 hilog
HDC="/mnt/e/huawei/DevEco Studio/sdk/default/openharmony/toolchains/hdc.exe" \
  ./scripts/run-on-device.sh com.example.myapplication
```

`build-hap.sh` 还会打印产物体积、sha256，以及**包内 `libs/` 清单**（确认原生库真的进包了）。

## 怎么读设备日志

`hilog` 的 tag 是 `DiaryCore`，每行都带 `CORE_MARKER`。关键几类：

| 前缀 | 内容 |
|---|---|
| `CORE_JSON[<label>][i/n]` | 完整结果 JSON 的分片（每片 900 字符）；**去掉前缀拼起来就是合法 JSON** |
| `ORT_JSON[i/n]` | ORT 探针结果 JSON |
| `ORT_CRATE {...}` | 走产品真实路径（`ort` crate load-dynamic）的结果，单行 |
| `DLOPEN_FROM_EL2 {...}` | 「把库复制到可写沙箱再 dlopen」的对照实验，单行 |

拿原始日志 + 把分片拼回完整 JSON：

```bash
HDC=… ; "$HDC" shell "hilog -x -T DiaryCore" | grep CORE_MARKER > evidence/device-hilog.txt
python3 - <<'PY'   # 分片拼回（本次结果见 evidence/parsed.json）
import re, json
chunks = {}
for ln in open('evidence/device-hilog.txt', encoding='utf-8'):
    m = re.search(r'CORE_JSON\[([a-zA-Z]+)\]\[(\d+)/(\d+)\] (.*)$', ln)
    if m:
        chunks.setdefault(m.group(1), {})[int(m.group(2))] = m.group(4)
out = {k: json.loads(''.join(v[i] for i in sorted(v))) for k, v in chunks.items()}
json.dump(out, open('evidence/parsed.json','w',encoding='utf-8'), ensure_ascii=False, indent=2)
PY
```

## 四个坑（都是实测撞出来的，与文档 §24 一一对应）

### 坑 1：`libc++_shared.so` 必须随 HAP 带（最难猜）

设备上 `/system/lib64/libc++_shared.so` **确实存在**（1,289,552 B，world-readable），
但应用进程的 linker namespace 看不到它：

```console
W MUSL-LDSO: load libc++_shared.so failed, namespace=ndk no inherits, errno=2
W MUSL-LDSO: Error loading shared library libc++_shared.so:
              (needed by /data/storage/el1/bundle/libs/arm64/libdiary_napi.so)
E ArkCompiler: [ecmascript] export objects of native so is undefined, so name is @normalized:Y&&&libdiary_napi.so&
```

→ `build-native.sh` 会把 `$NDK/llvm/lib/aarch64-linux-ohos/libc++_shared.so` 一起复制进 `entry/libs/arm64-v8a/`。
**ORT 的 `NEEDED` 里也有它**，所以同理。
**排障要点**：ArkTS 侧 import 原生模块失败只表现为 `undefined`，真因在**不带 tag 的全量 hilog** 里
（`hdc shell "hilog -x"`）。

### 坑 2：跨文件的符号别落在匿名 namespace 里

`OrtProbeImpl` 的**声明**写在 `napi_shim.cpp` 的匿名 namespace 里、**定义**在 `ort_probe.cpp` 的全局作用域 ——
编译、链接成 `.so` 都不报错，设备上 `dlopen` 才炸：

```console
W MUSL-LDSO: relocating failed: symbol not found.
  dso=…/libdiary_napi.so s=_ZN12_GLOBAL__N_112OrtProbeImpl…   ← GLOBAL__N_1 = 匿名 namespace
```

HAP 里的 `.so` 是运行时才链接的，静态检查覆盖不到。上设备前可自查：
`llvm-readelf --dyn-syms <so> | grep " UND "`，看有没有本应自足的未定义符号。

### 坑 3：命令行工程里 `import 'libX.so'` 需要三件套

1. `entry/src/main/cpp/types/libdiary_napi/{index.d.ts, oh-package.json5}`（后者 `"name": "libdiary_napi.so"` + `"types"`）；
2. `entry/oh-package.json5` 登记 `"libdiary_napi.so": "file:./src/main/cpp/types/libdiary_napi"`；
3. **`ohpm install --all`**（离线、46 ms）生成 `entry/oh_modules` 软链。

缺第 3 步的报错是 `arkts-no-any-unknown` + warning `Currently module for 'libdiary_napi.so' is not verified`。
**不需要 `externalNativeOptions` / `CMakeLists.txt`**：预编译 `.so` 放进 `entry/libs/arm64-v8a/`，
hvigor 的 `ProcessLibs` 会自动收进 HAP 的 `libs/arm64-v8a/`（并顺手 strip 一遍）。

### 坑 4：可写沙箱（el2）里的原生库 **dlopen 不了**

把 HAP 里的库复制到 `filesDir`（字节数一致、ELF magic 正确、`chmod 755`、`R_OK|X_OK` 都为真）后
`dlopen` 仍报 `No such file or directory`；35 KB 与 12.9 MB 两个库表现一样。
→ **原生库必须随 HAP 发**，不能运行时下发。模型（`.onnx`）是数据，放 el2 没问题。

## 设备侧实测（摘要，细节见文档 §21–§23）

真机 HarmonyOS 6.1 PC（2in1 / API 23 / arm64-v8a）。

| 项 | 值 |
|---|---|
| 沙箱跑通 SQLite | ✅ `stage=ok`、`errors=[]`、SQLite **3.53.2**、`journal_mode=wal`、读回 sha256 与宿主一致 |
| `Core::open` 冷（建库 + 迁移 schema v8） | **9.2–9.4 ms**；热启动 **0.79 ms** |
| 单行写入 / 读回 | **0.16 ms / 0.06 ms** |
| 事务插 1 万行 | **23.5 ms**（≈42 万行/秒） |
| `save_draft` 第一次 / 第二次 | 131.3 ms / **0.48 ms**（首次数抖动的解释见文档 §21.1，标注为推断） |
| `core.db`（运行中 db+wal+shm / 关闭后） | 4 KB + 406 KB + 32 KB / **272 KB** |
| `dlopen` 我们自己的 `.so` | ✅ 裸名与绝对路径都行，真实路径 `/data/storage/el1/bundle/libs/arm64/…` |
| `dlopen` el2 里的 `.so` | ❌ `No such file or directory`（对照实验见坑 4） |
| `dlopen` ONNX Runtime | ✅ `1.16.3`，建 session + 真实推理 `C=[1,2,3,4,5,6]` 与宿主逐项一致 |
| `ort` crate（load-dynamic） | ❌ 干净报错：`expected version >= '1.17.x', but got '1.16.3'`（**版本问题，不是沙箱问题**） |
| HAP 体积 | 不含 ORT **5.23 MB**（unsigned）/ 含 ORT **18.23 MB** |

> ORT 从哪来：本次用的是 **sherpa-onnx 官方 OHOS 预编译包**里的 `libonnxruntime.so`
> （Apache-2.0/MIT；`build-native.sh` 会自动下载），**它只有 1.16.3，比我们 `ort` crate 要求的
> API 17 低一个 minor**，所以它只用来证明「沙箱能 dlopen ORT + 能推理」。
> 正式选型应当用 `/tmp/requests-wt/archives/o/onnxruntime/1.29.1` 的配方自编 ORT ≥1.17
> （进度与踩坑见文档 §24.5）。

## 复现脚本

```bash
./scripts/build-native.sh                    # 编原生库
#   环境变量：NDK / OHOS_TARGET / CARGO_TARGET_DIR / SKIP_RUST / SKIP_SHIM / SKIP_ORT / ORT_SO_DIR
OHOS_BUNDLE_NAME=… ./scripts/build-hap.sh    # 换 bundleName / API 版本 / 加签名
HDC=/path/to/hdc.exe ./scripts/run-on-device.sh <bundleName>
```

宿主对照实验（把设备上跑的那段代码在同一台 Linux 上跑一遍，用来排除「设备特有的差异」）：

```bash
cd rust && CARGO_TARGET_DIR=/tmp/ohos-core-smoke/target-host cargo test --offline -- --nocapture
```
# ohos-bge-smoke —— 真 bge 嵌入在鸿蒙真机沙箱里跑（第 3 步探针）

> 分支 `feat/ohos-bge-smoke`（基于 `feat/ohos-core-smoke`）。
> **`crates/**`、`packages/**`、`apps/**` 一行未动**；bundleName 用**临时值** `com.example.myapplication`
> （沿用第 1 步的调试 Profile 约束，见 `docs/architecture/m6-鸿蒙可行性探针.md` §13.4）。
>
> 口径：**实测** = 真跑过并贴出原始输出；**未验证** = 明确标注。

它回答一件事：**真模型（bge-small-zh-v1.5 int8，22.73 MiB + tokenizer 0.42 MiB）能不能在真机
应用沙箱里，用产品同一条路径（`ort` crate load-dynamic + `diary_core::embedding`）跑出正确的
512 维向量**——第 2 步只跑了 121 B 的 `tiny_add.onnx`。

## 0 结论速览（细节见架构文档「续篇三」）

| 问题 | 结论 |
|---|---|
| 真模型进 HAP → 沙箱 → 加载 | ✅ 实测（rawfile → `filesDir` → ORT 读文件，`errors: []`） |
| 512 维 CLS 向量 | ✅ 实测（`dims=512`，`model_version=bge-small-zh-v1.5-ours@d44978e451a0`） |
| 与宿主同代码同模型的差异 | ✅ 实测：**最差余弦 0.997986**、**最大逐元素偏差 1.10e-02**；17 段里 **11 段**差异在 1e-8（ULP）量级 |
| 语义判据（相近 > 无关） | ✅ 两边都成立（设备 0.8575 vs 0.4478） |
| 检索排序是否不变 | ✅ 5 查询 × 5 候选，设备与宿主**排序完全一致** |
| 耗时 / 内存 | ✅ 实测：加载 248 ms；短文本单次 **2.5 ms**、412 token **62 ms**；RSS 加载 **+45 MiB** |

## 1 工程结构

```
probes/ohos-bge-smoke/
├── AppScope/app.json5                 临时 bundleName（构建时被 OHOS_BUNDLE_NAME 覆盖）
├── build-profile.json5                compatible/targetSdkVersion "6.1.0(23)"
├── entry/
│   ├── libs/arm64-v8a/                构建产物落点（.gitignore；出 HAP 的 libs/arm64-v8a/）
│   ├── oh-package.json5               ★ "libdiary_bge.so": "file:./src/main/cpp/types/libdiary_bge"
│   └── src/main/
│       ├── cpp/napi_shim.cpp          dlopen 核心 + 拼参数 JSON（模型目录 + 同目录 ORT）
│       ├── cpp/types/libdiary_bge/    ArkTS import 的类型声明
│       ├── ets/entryability/EntryAbility.ets  ★ rawfile→filesDir 落盘 + 调 runBge + hilog 分片
│       └── resources/rawfile/         ★ 模型由 build-hap.sh 现场拷入（不进仓库）
├── rust/
│   ├── Cargo.toml                     cdylib + 自己的 [workspace]（不进产品 workspace）
│   └── src/lib.rs                     ★ 薄 C ABI：diary_bge_run/_free/_abi
│                                      + #[cfg(test)] host_control（宿主对照，跑同一段 run()）
├── scripts/
│   ├── build-native.sh                Rust cdylib + C++ shim + libc++_shared + ORT → entry/libs/
│   ├── build-hap.sh                   + 模型进 rawfile（校验 sha256）+ ohpm install + 签名注入
│   ├── run-on-device.sh               卸载→安装→force-stop→启动→抓 hilog（含「rc 恒为 0」处理）
│   ├── parse-hilog.py                 分片拼回 → evidence/device-bge.json
│   ├── compare-host-device.py         设备 vs 宿主：余弦/最大偏差/语义/排序/耗时/内存
│   └── ohos-sign-pwd.js               （沿用第 1 步）签名口令加解密
└── evidence/
    ├── device-hilog-bge.txt           真机原始日志（第一次冷启）
    ├── device-bge.json                拼回后的设备侧结果（第一次冷启）
    ├── device-hilog-bge-warm.txt      真机原始日志（第二次启动）
    ├── device-bge-warm.json           第二次启动结果（看「热」加载）
    ├── host-control.txt               宿主对照原始输出（cargo test -- --nocapture）
    └── compare.json                   设备 vs 宿主的逐项对比结果
```

## 2 完整命令序列

```bash
export NDK=~/command-line-tools/sdk/default/openharmony/native
export HDC="/mnt/e/huawei/DevEco Studio/sdk/default/openharmony/toolchains/hdc.exe"
# ★ 路径纪律：本机 /tmp 会被清空，缓存与构建产物一律放 $HOME
export CARGO_TARGET_DIR="$HOME/dt-ohosbge"
cd probes/ohos-bge-smoke

# ── 1. 编原生库（Rust cdylib + C++ shim + libc++_shared + 自编 ORT）
./scripts/build-native.sh
#   只需要三个环境变量：linker + C 编译器（bundled SQLite）+ ar；脚本里已封好

# ── 2. 构建 + 签名（脚本内部：模型进 rawfile → sha256 校验 → ohpm install → 注入 signingConfigs）
OHOS_BUNDLE_NAME=com.example.myapplication \
OHOS_SIGN_STORE_FILE=…p12 OHOS_SIGN_CERT_FILE=…cer OHOS_SIGN_PROFILE_FILE=…p7b \
OHOS_SIGN_STORE_PASSWORD_ENC=0000001A…（DevEco 加密串） \
OHOS_SIGN_KEY_PASSWORD_ENC=0000001A…（DevEco 加密串） \
  ./scripts/build-hap.sh
#   → entry-default-unsigned.hap / entry-default-signed.hap（体积与包内条目脚本会打出来）

# ── 3. 安装 → 冷启动 → 抓日志
HDC="$HDC" ./scripts/run-on-device.sh com.example.myapplication

# ── 4. 解析 + 对照
python3 scripts/parse-hilog.py evidence/device-hilog-bge.txt          # → evidence/device-bge.json
(cd rust && cargo test --release --offline -- --nocapture host_control \
   > ../evidence/host-control.txt 2>&1)
python3 scripts/compare-host-device.py evidence/device-bge.json evidence/host-control.txt \
   --device2 evidence/device-bge-warm.json --out evidence/compare.json
```

## 3 四个坑（都是本轮真撞出来的）

1. **`aa start` 不会重启已在跑的进程**：应用还在后台时 `aa start` 只把它拉到前台，
   `onWindowStageCreate` 不再执行 → **日志是空的**（本次实测：前后 PID 都是 27014，`BGE_JSON` 计 0 行）。
   必须先 `aa force-stop <bundle>`（`run-on-device.sh` 的 `FORCE_STOP=1` 已默认做）。
2. **模型必须走 rawfile**：`filesDir`（el2）里放 `.so` 不能 `dlopen`（第 2 步§22.2），但**数据文件可以**——
   模型从 rawfile 落盘再交给 ORT 读，这条路是通的（本次 23.8 MB 实测 writeMs≈31–48 ms）。
3. **`libc++_shared.so` 仍然必须随包**（第 2 步§24.1 的坑对 ORT 一样成立）。
4. **`hdc install` 失败时退出码仍是 0**（第 1 步§15/4）——脚本 grep `install bundle successfully`。

## 4 ORT 从哪来（一条命令，缓存优先）

设备侧用的 `libonnxruntime.so` 是**自编 ORT 1.29.1**（全算子集，`OrtGetApiBase@@VERS_1.29.1`，
strip 后 19,623,736 B）。现在由 [`tools/ohos/build-ort.sh`](../../tools/ohos/build-ort.sh) 统一提供，
`build-native.sh` 只调它：

```bash
# 命中缓存（~$HOME/dev/caches/ort-1291）+ 自检 ⇒ 秒级返回，不编译、不联网
tools/ohos/build-ort.sh --dest probes/ohos-bge-smoke/entry/libs/arm64-v8a

# 缓存未命中才完整构建（首次 ~33 min / 16 核；幂等，重复运行不重下/不重打补丁）
tools/ohos/build-ort.sh --force-build
```

为什么必须自编、配方与踩过的坑、缓存失效条件、CI 怎么用，见
[`docs/architecture/m6-ORT-构建与缓存.md`](../../docs/architecture/m6-ORT-构建与缓存.md)。

> 历史记录（2026-10-16，`/tmp` 被清空后靠 conan 构建树里的未 strip 副本救回）：
> `$HOME/.conan2/p/b/onnxrb1d51c7f51392/b/build/Release/libonnxruntime.so.1.29.1`
> （31,958,648 B）→ `llvm-strip --strip-unneeded` → 19,623,736 B。
> 这条手工路径现在只用于救急，不再是复现方式。

## 5 设备侧结果（原文在 `evidence/`）

```
dlopen_ok=true  used=libdiary_bge_core.so  dlsym_ok=true  abi=diary_bge_smoke/1 ohos-cdylib
core_loaded_at=/data/storage/el1/bundle/libs/arm64/libdiary_bge_core.so
staged model_quantized.onnx 23,830,209 B writeMs=31 | tokenizer.json 439,125 B writeMs=1
stage=ok  load_ok=true  dims=512  model_version=bge-small-zh-v1.5-ours@d44978e451a0  errors=[]
load_ms=248.404  first_embed_ms=8.46  total_ms=1197.843  cpu_count=20
```

设备 vs 宿主（`compare.json`，前置一致性全部 OK：语料 sha / 模型 sha / 维度 / 口径）：

| 项 | 值 |
|---|---|
| 最差余弦相似度 | **0.997986337**（medium，70 token） |
| 最大逐元素偏差 | **1.101e-02**（main/short，30 token） |
| 逐位一致 | **0/17**（int8 + 不同 SIMD/线程分块 ⇒ 不期望逐位相同） |
| 差异 ≤1e-7（ULP 量级）的文本 | **11/17**（都是 ≤31 token 的短句） |
| 语义判据 | 设备 `near_a__near_b=0.857477 > near_a__far=0.447805`；`main__near_a=0.844427 > main__far=0.464020` |
| 排序一致性 | 5/5 查询**完全一致**（相似度矩阵最大差 1.85e-03，无翻转） |

耗时（设备 / 宿主，ms）：

| 项 | 设备 | 宿主 x86-64 |
|---|---|---|
| 加载（`embedding::load`：sha256 + tokenizer + 建会话 + 预热） | **248.4**（两次 248.404 / 248.342） | 290.7 |
| 其中模型 sha256（冷） | 152.2 | 59.5 |
| 其中建会话（单独测，缓存已热） | 61.7 | 57.5 |
| 首次真文本嵌入 | 8.46 | 5.18 |
| 短文本 ≤31 token 热中位 | **2.52**（1.04–6.32） | 2.62 |
| 中文本 70 token 热中位 | 9.66 | 8.37 |
| 长文本 412 token 热中位 | **62.4** | 60.2 |
| 吞吐 | ≈ 6.6 token/ms（长） | ≈ 6.8 token/ms |

内存（`/proc/self/status`，设备侧实测）：

| 时刻 | 设备 RSS | 相对基线 |
|---|---|---|
| `run()` 开始（含 ArkTS 运行时） | 137.0 MiB | — |
| 模型加载后 | 182.0 MiB | **+45.0 MiB** |
| 全部嵌入后 | 239.5 MiB | +102.5 MiB |
| 峰值 HWM | 280.8 MiB | +121.2 MiB（含末尾故意多建的一个 session） |

## 6 已知风险（本轮实测发现，**没有顺手改产品代码**）

`crates/diary_core/src/embedding.rs` **没有截断/超长处理**：>512 token 的文本会走到模型里
位置相关的 `Add` 节点并**报错**（设备与宿主都报同一句）：

```
ONNX 前向失败：Non-zero status code returned while running Add node. Name:'node_add_18'
Status Message: … BroadcastIterator::Append … Attempting to broadcast an axis by a dimension
other than 1. 512 by 617
```

617 token 触发、512 以内正常（本次 412 token 的长文本正常出向量）。产品现有的
`MAX_CHUNK_CHARS = 400`（`chunker.rs`）在纯中文下 ≈ 410 token，**通常**够用，但没有硬保证
（英文/数字/emoji 的字符→token 比 >1）。详见架构文档「续篇三」的风险节。
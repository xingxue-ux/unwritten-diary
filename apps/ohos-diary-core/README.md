# ArkTS NAPI 参考工程（#108 第二块）

把已经交付的 C ABI（`crates/diary_capi`，句柄 + JSON 进 / JSON 出）包成 ArkTS 能直接
`import` 的模块，并给一个最小示例 Ability：**打开库 → coreInfo → 建草稿 → 保存 → 提交 →
抽取 → 检索 → 排索引任务 → 跑任务 → 关闭**，结果全部打到 hilog。

设计与验证记录在 [`docs/architecture/m6-ArkTS-NAPI.md`](../../docs/architecture/m6-ArkTS-NAPI.md)；
C ABI 本身见 [`docs/architecture/m6-核心-C-ABI.md`](../../docs/architecture/m6-核心-C-ABI.md)。

> ✅ **2026-10-18 真机验证通过**：HAD-W32 / API 23 / arm64-v8a。
> NAPI/烟测部分最初用临时身份 `com.example.myapplication`；**录音这一片（#108 ⑦）已改用
> 产品身份 `com.lingxing.unwrittendiary`** 安装、授权、录音并 finalize（见
> [`docs/architecture/m6-平板与PC平台能力.md`](../../docs/architecture/m6-平板与PC平台能力.md)）。
> 逐条结果、原始 hilog、性能数字与问题清单见本文[「真机验证结果」](#真机验证结果2026-10-18)。

## 目录

```
apps/ohos-diary-core/
├── entry/src/main/cpp/
│   ├── napi_shim.cpp                       # libdiary_napi.so 的唯一源码（7 个同步函数）
│   └── types/libdiary_napi/                # ArkTS 类型三件套之一、之二
│       ├── index.d.ts
│       └── oh-package.json5
├── entry/src/main/ets/
│   ├── core/DiaryCore.ets                  # 薄封装：14 条契约方法 + rawCall/rawEnvelope
│   ├── core/DiaryError.ets                 # 21 个核心码 + native_unavailable
│   ├── core/Envelope.ets                   # 请求拼装 / 响应拆封
│   ├── core/Models.ets                     # data 与参数的 ArkTS 类型（snake_case）
│   ├── core/Smoke.ets                      # 示例链路（hilog）
│   ├── entryability/EntryAbility.ets       # 启动时跑一遍 + 主线程阻塞计时
│   ├── verify/DeviceVerify.ets             # 真机探针：bench / errors / concurrency / persist / diag
│   ├── verify/ConcurrentWorker.ets         # TaskPool worker：同一句柄并发 call
│   ├── verify/ProbeAbility.ets             # 多 Ability（同进程）验证
│   ├── verify/ProbeIsoAbility.ets          # 多进程（:probe）验证
│   └── pages/Index.ets
└── scripts/
    ├── build-native.sh                     # Rust cdylib + C++ shim + libc++_shared → entry/libs/
    └── build-hap.sh                        # ohpm install + hvigorw assembleHap（可签名/可覆盖 bundle/version）
```

## 快速开始（本机，三条命令）

```bash
cd apps/ohos-diary-core

# 1. 原生库：cargo 交叉编译 + NDK clang++ 编 shim + 复制 libc++_shared.so（约 2–3 分钟）
#    内存小的机器请限制并行、用独立 target 目录：
CARGO_BUILD_JOBS=4 CARGO_TARGET_DIR="$HOME/dt-ohos-napi" ./scripts/build-native.sh

# 2. 转写引擎库（默认必需）：sherpa + ORT 进 entry/libs/arm64-v8a/，
#    并校验 sha256、NEEDED、OrtGetApiBase@VERS_1.16.3（换库会在这里红）
./scripts/stage-sherpa.sh

# 3. HAP：ohpm install（本地 file: 依赖，离线）+ hvigorw assembleHap（约 14 秒）
#    build-hap 默认调用 stage-sherpa.sh --check：缺库/版本不对直接构建失败。
#    只构建「不含转写」的基线/冒烟包时显式跳过：
#      SKIP_ASR_LIBS=1 ./scripts/build-hap.sh
./scripts/build-hap.sh

# 4. 装到设备（签名 HAP；unsigned 装不上）
hdc install -r entry/build/default/outputs/default/entry-default-signed.hap
hdc shell "hilog -x -T DiaryCore" | grep DiaryCore
```

环境要求（与探针一致）：`rustup target add aarch64-unknown-linux-ohos`、OHOS NDK
（默认 `~/command-line-tools/sdk/default/openharmony/native`）、`ohpm` 与 `hvigorw`
（默认 `~/command-line-tools/bin/`）。**不需要 CMake、不需要 DevEco GUI、不需要 nightly。**

`build-native.sh` 的产物（本机未 strip 的原始大小；hvigor 打包时会 strip）：

| 文件 | 本机大小 | 包内大小（实测） | 说明 |
|---|---:|---:|---|
| `entry/libs/arm64-v8a/libdiary_capi.so` | 11,369,880 B | 11,369,640 B | Rust cdylib，`NEEDED` 只有 `libc.so` |
| `entry/libs/arm64-v8a/libdiary_napi.so` | 46,824 B | 30,824 B | C++ NAPI shim，`NEEDED` = `libace_napi.z.so` / `libc++_shared.so` / `libc.so` |
| `entry/libs/arm64-v8a/libc++_shared.so` | 1,262,504 B | 1,262,504 B | **必须随包**（见下） |

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
真机实测（2026-10-18）随包后**没有再出现** `libc++_shared` 相关报错，详见下文第 5 项。

## 示例输出长什么样（真机原始输出）

```text
10-18 16:46:35.500 .../DiaryCore: DIARY_SMOKE open OK handles=1
10-18 16:46:35.501 .../DiaryCore: DIARY_SMOKE JSON[core.info][1/1] {"api_version":"1.0","build_version":"0.1.0","capabilities":[...],"capture_count":4,"data_schema_version":13,...}
10-18 16:46:35.759 .../DiaryCore: DIARY_SMOKE DONE processed=0 succeeded=0
10-18 16:46:35.761 .../DiaryCore: DIARY_SMOKE CLOSED handles=0
10-18 16:46:35.761 .../DiaryCore: DIARY_SMOKE MAIN_THREAD syncReturned smokeMs=268
10-18 16:46:35.860 .../DiaryCore: DIARY_SMOKE MAIN_THREAD smokeMs=268 eventLoopDelayMs=99 droppedFramesAt60Hz=17
10-18 16:46:35.860 .../DiaryCore: DIARY_VERIFY DBSTAT path=/data/storage/el2/base/haps/entry/files/library.sqlite bytes=389120 wal=0 shm=0
```

`CLOSED handles=0` 是句柄泄漏自检（`openHandleCount()` 来自 shim 的登记表）。

## 用临时 bundleName 做真机验证

现有调试 Profile（`~/.ohos/config/default_a_...p7b`）只绑 `com.example.myapplication`；
`build-hap.sh` **不把 bundleName 写死**，构建前用环境变量覆盖、构建后由 `trap` 还原：

```bash
cd apps/ohos-diary-core
source "$HOME/napi-device-scratch/sign.env"   # 本机临时文件，不提交；p12/cer/p7b + DevEco 加密口令

OHOS_BUNDLE_NAME=com.example.myapplication \
OHOS_VERSION_CODE=1000000 OHOS_VERSION_NAME=1.0.0 \
BUILD_MODE=debug \
  ./scripts/build-hap.sh
# → entry/build/default/outputs/default/entry-default-signed.hap

hdc install -r entry/build/default/outputs/default/entry-default-signed.hap | tee /tmp/install.log
grep -q "install bundle successfully" /tmp/install.log || { echo "install failed"; exit 1; }
```

要点：

- **`hdc install` 的返回码不可信**：实测装 unsigned HAP 时 stdout 是
  `msg:error: failed to install bundle. code:9568320 error: no signature file.`，但 `rc=0`。
  必须 grep stdout 里的 `install bundle successfully`。
- 同一个 `bundleName` 重装用 `hdc install -r`；升级用例再用
  `OHOS_VERSION_CODE=1000001 OHOS_VERSION_NAME=1.0.1` 构建一次。
- 产品名 `com.lingxing.unwrittendiary` 需要在 DevEco/AGC 生成对应的调试 Profile 后才能用
  `OHOS_BUNDLE_NAME=com.lingxing.unwrittendiary` 构建安装；本文件不替用户生成 Profile。
- 构建脚本还支持 `OHOS_COMPAT_SDK_VERSION` / `OHOS_TARGET_SDK_VERSION`（见脚本头注释）。

验证探针入口（都在 `entry/src/main/ets/verify/`，只用于测试，不属于产品 API）：

```bash
# 同步 Smoke + 主线程阻塞计时（默认，不带参数也行）
hdc shell "aa start -a EntryAbility -b com.example.myapplication -m entry"

# 性能 / 错误路径 / 并发 / 升级后读回 / 诊断
hdc shell "aa start -a EntryAbility -b com.example.myapplication -m entry --ps verify_mode bench"
hdc shell "aa start -a EntryAbility -b com.example.myapplication -m entry --ps verify_mode errors"
hdc shell "aa start -a EntryAbility -b com.example.myapplication -m entry --ps verify_mode concurrency"
hdc shell "aa start -a EntryAbility -b com.example.myapplication -m entry --ps verify_mode persist"
hdc shell "aa start -a EntryAbility -b com.example.myapplication -m entry --ps verify_mode diag"

# 多 Ability（同进程）/ 多进程——**只适用于 debug 构建，且按 `exported:false` 的语义**：
#   `ProbeAbility` / `ProbeIsoAbility` 在 module.json5 里是 `exported: false`，按定义
#   第三方应用（含 `aa` 这类跨应用拉起）不应能直接启动它们——**这一点没有真机验证**（未验）。
#   下面两条命令只作「门控前 debug HAP 上的用法」记录；release 下两个 Ability 都会走
#   `PROBE_DISABLED`，不做任何验证动作。`EntryAbility --ps verify_mode` 不覆盖这两项
#   （multi_hold / `:probe` 隔离进程），没有等价的平替入口。
hdc shell "aa start -a ProbeAbility -b com.example.myapplication -m entry --ps verify_mode multi_hold"
hdc shell "aa start -a ProbeIsoAbility -b com.example.myapplication -m entry"
```

## 签名：必须用 DevEco / AGC 申请，**自签不可行（已实测）**

2026-10-18 实测结论：**用自建 CA 签发的 debug Profile 装不上这台 HAD-W32**，而且不是 bundleName/UDID 的问题——**卡在 App 签名 PKCS#7 的证书链信任层**，Profile 内容还没被读到。

```text
# hdc install 自己签的 HAP（rc 仍是 0，必须看 stdout）
msg:error: failed to install bundle. code:9568257 error: fail to verify pkcs7 file.

# hilog 定位
HapVerify hap_cert_verify_openssl_utils.cpp(GetCertsChain:322)
  it do not come from trusted root,
  issuer: C=CN,O=SelfSignLab,OU=HarmonyOS Experiment,CN=SelfSignLab Root CA
hap_verify_v2.cpp(VerifyAppPkcs7:163) GetCertChains from pkcs7 failed
```

设备 `/system/etc/security/trusted_root_ca.json` 里只有 **Huawei CBG Root CA G2** 与 **OpenHarmony Application Root CA**；自建根不在其中。**对照实验**：改用 SDK 自带的 `OpenHarmony.p12` + `OpenHarmonyProfileDebug.pem`（设备信任列表里确实有 OpenHarmony 根）走同一链路，**仍然 `9568257`**，hilog 的 issuer 变成 `OpenHarmony Application Root CA` ⇒ **这台 HarmonyOS 的 App 签名链实际只认 Huawei CBG 根**，CLI 全自动自签这条路是死的。

**所以：** 调试/发布签名只能用 **DevEco「自动生成签名」** 或 **AGC 控制台**（两者都是用已登录的华为账号向 AGC 换取绑了 `bundleName` + 设备 UDID 的 Profile）。CI 若要自动装 HAP，签名材料必须由人**先申请一次**再放进 secrets。

### DevEco 获取 Profile 的步骤（3–5 下）

1. 确认 `AppScope/app.json5` 的 `bundleName` 是 **`com.lingxing.unwrittendiary`**（本仓库已是），DevEco 里 **Sync** 一次；
2. 右上角 **Sign In** 登录华为开发者账号（**未实名需先签协议**）；
3. **设备保持连接**（自动签名会用 `hdc shell bm get -u` 自己取 UDID 并通过 AGC 加设备，**不用手填**）；
4. `File > Project Structure...`（`Ctrl+Alt+Shift+S`）→ **Project > Signing Configs** → 勾 **`Automatically generate signature`**（中文「自动生成签名文件」）→ 选设备/团队 → **OK**；
5. 若 AGC 不认识新包名，会报 `Bundle name ... does not exist` / 「没有被识别为 HarmonyOS 应用」⇒ **先在 AGC 建这个应用**，再回来重复第 4 步；
6. 产物落在 `C:\Users\<你>\.ohos\config\default_a_<hash>.p12/.cer/.p7b` + `material\`（`<hash>` 以弹窗里 `Profile File` 栏为准）；之后 `Build > Build Hap(s)` 出 `entry-default-signed.hap`。

## 录音（#108 ⑦ 第一片）

`recording/` 下是**语音日记的采集链路**：`AudioCapturer`（PCM 16 kHz / 单声道 / 16 bit）
→ 每 2 秒封一段写进核心给的 `staging_ticket` → `recordings.registerSegment` → 停止时
`recordings.finalize`（拼逻辑音频、建 Asset、同事务排转写任务）→ `captures.commit`
（用 finalize 返回的 `capture_id` / `capture_revision` 把记录提交成 `committed`）。
完整设计、权限、长时任务、真机数字与「没做到的」见
[`docs/architecture/m6-平板与PC平台能力.md`](../../docs/architecture/m6-平板与PC平台能力.md)。

`finalize` 只保证「音频已保存」，它不会把记录变成 `committed`；不提交的话，音频、转写、
关键词与 AI 日记都在，但记录永远停在 draft，界面无法把它当成一篇。提交失败**不回滚**
音频与来源，`VoiceRecorder.commitResult()` 会给出 `failed` 与错误行；核心返回
`invalid_state`（很可能上次已经提交成功）按幂等成功处理并写日志。

**空 draft 的残留（如实记录，未修）**：启动失败（还没登记任何片段）时会留下一条没有
来源、也没有正文的 draft，而 Flutter 启动会把最新 draft 当编辑器草稿恢复，用户会看到
空白草稿。核心当前没有 `captures.trash` / 安全删除空 draft 的能力；本片**不**写破坏性
SQL，也不为了清它去提交一条空记录，只把 id 暴露为 `abandonedDraftId()` 并打
`DRAFT_LEAK` 日志。代价是残留仍在；等核心有安全删除能力后再由产品侧清理。

真机命令（**先 force-stop**，否则 `aa start` 只触发 onNewWant，不会重跑 `onWindowStageCreate`）：

```bash
hdc shell "aa force-stop com.lingxing.unwrittendiary"
hdc shell "aa start -a EntryAbility -b com.lingxing.unwrittendiary -m entry --ps verify_mode record"
hdc shell "hilog -x -T DiaryRec" | grep DIARY_REC
```

真机结果（2026-10-18，产品身份）：5 秒录出 3 段（64,044 / 64,000 / 31,360 B），
finalize `state=saved`、`logical_duration_ms=4980`、blob 159,404 B 是合法单头 PCM WAV；
首包延迟 37.5 ms、录音期间 CPU 5.15%。**Home 后台能继续录**（8 秒/4 段/finalize 成功）；
**Power 锁屏后系统强制 STOP 采集、亮屏不恢复**（8 秒只录到 3.1 秒，`InterruptHint=STOP`）。

录音期间会 `setWindowKeepScreenOn(true)`（`ScreenKeepOn.ets`），停止/异常时释放；
真机 8 秒用例 `KEEP_SCREEN_ON ok=true`、4 段/8000 ms、停止后 `KEEP_SCREEN_ON released`。
当前系统息屏超时 **30,000 ms**（只读），8 秒没有跨过该阈值，所以「超过 30 秒仍不熄屏」
**未验**。转写任务排上了但设备没有 ASR 引擎/模型，停在 `waiting_configuration`；
这一条已在下一节（2026-10-18 夜）补验：引擎与模型都就位，`waiting_configuration → succeeded` 跑通。

## 转写（#108 第三片）：真机「录音 → 转写 → 可检索」已跑通

2026-10-18 夜，HAD-W32 / API 23 / arm64-v8a / HarmonyOS PC，产品身份
`com.lingxing.unwrittendiary`，debug signed HAP。**转写推理全程不联网**：引擎是包里的
`sherpa-onnx-c-api.so`，模型在应用沙箱，转写走本机推理。**验证期的模型传输**走
`hdc rport`（debug 构建声明 `INTERNET`）；release 构建**不声明** `INTERNET`，也不下载。

### 打进 HAP 的库与体积（同一份代码，四种打包）

| 打包 | `libs/arm64-v8a` 里的原生库 | signed HAP | 相对基线 |
|---|---|---:|---:|
| 基线（只有我们自己的库） | `libdiary_capi` 11,507,312 / `libdiary_napi` 30,824 / `libc++_shared` 1,262,504 | **13,374,010** | — |
| 只加引擎 | + `libsherpa-onnx-c-api.so` 3,750,328 | **17,158,353** | **+3,784,343** |
| 引擎 + **自编 ORT 1.29.1** | + `libonnxruntime.so` 19,623,560 | 36,946,694 | +23,572,684 |
| **最终采用**：引擎 + sherpa 自带 ORT 1.16.3 | + `libonnxruntime.so` 12,930,856 | **30,198,881** | **+16,824,871** |

**为什么最终是 sherpa 自带的 ORT，而不是自编 1.29.1**（实测，不是推测）：

- 只加 `libsherpa-onnx-c-api.so`（HAP 里没有 ORT）⇒ 连接测试直接失败：
  `加载本机转写引擎库失败：dlopen failed（试过 libsherpa-onnx-c-api.so）`；
- 换成自编 **ORT 1.29.1**（`OrtGetApiBase@@VERS_1.29.1`）⇒ 仍然失败，
  这次拿到了加载器的原始报错：

  ```text
  W MUSL-LDSO: relocating failed: symbol not found.
      dso=/data/storage/el1/bundle/libs/arm64/libsherpa-onnx-c-api.so
      s=OrtGetApiBase use_vna_hash=1 van_hash=27bf083
  E DiaryAsr: DIARY_ASR FAILED mode=asr_test error=模型或端点不可用：加载本机转写引擎库失败：dlopen failed（试过 libsherpa-onnx-c-api.so）
  ```

  `llvm-readelf` 对照：sherpa 1.13.8 的 OHOS 预编译库引用的是
  **`OrtGetApiBase@VERS_1.16.3`**，自编 ORT 只导出 `@@VERS_1.29.1`；OHOS 的
  musl-ldso 按版本哈希（`use_vna_hash=1`）解析，版本串不一致就是「符号找不到」。
  ORT 的 C API 表本身是稳定的（只要 `GetApi(16)` 还在），**卡的是符号版本串**。
  ⇒ **不能只把 `libonnxruntime.so` 换成 1.29.1**。
  
  但「因此只有重编 sherpa 一条路」**不成立**：`ort::init_from` 接受任意路径，自编
  1.29.1 的 SONAME 是 **`libonnxruntime.so.1`**，与 sherpa 的 `NEEDED`
  **`libonnxruntime.so`** 并不同名。**这条双 ORT 路已并入本 PR**（原 #134，commit
  `378316c` 起）：sherpa 继续用 1.16.3 的 `libonnxruntime.so`，语义索引用
  `libonnxruntime-diary-1.29.1.so`（SONAME `libonnxruntime.so.1`）显式 `init_from`，
  同一 HAP 里转写与语义检索可以同时跑（`WITH_SEMANTIC=1` 构建，产品身份已验）。
  本 PR 仍然证明「把同一个文件名直接换成 1.29.1」会失败。

### 引擎库怎么进 HAP、构建期怎么防「缺库的包」

`stage-sherpa.sh` 把两个 `.so` 从 `$HOME/dev/caches/asr-ohos/...` 放进
`entry/libs/arm64-v8a/`：

- 来源 sha256 钉死（sherpa `75b38a52…`、ORT `f9fd9080…`）；换来源要 `--force` 并更新脚本；
- **覆盖前**读目标 `libonnxruntime.so` 的 `OrtGetApiBase` 符号版本，与来源不同就**拒绝
  覆盖**（除非 `--force`）——防止把别人放的自编 1.29.1 静默冲掉；
- 复制后断言：AArch64、sherpa 的 `NEEDED` 有 `libonnxruntime.so`、两边符号版本都是
  `OrtGetApiBase@VERS_1.16.3`。

`build-hap.sh` 默认调用 `stage-sherpa.sh --check` 做同一组断言；**缺库 / 版本不对在构建期
就红**，不会装到设备上才发现。确实要构建不含转写的基线包时用 `SKIP_ASR_LIBS=1`（这种包
**不能转写**）。缺引擎时任务为什么应停在 `waiting_configuration` 而不是终态 `failed`，是
**本 PR 已包含的 P0 核心修复**（commit `378316c`），不是外部依赖。

### 模型怎么进沙箱（`hdc file send` 不通，走 rport）

应用沙箱 `/data/app/el2/100/base/<bundle>/haps/entry/files` 属于 app uid，
即便手工把目录权限放宽，shell 域仍写不进去（所以探针**不再** `chmod 0777`；
模型只能由应用自己下载）：

```text
$ hdc file send tokens.txt /data/app/el2/100/base/.../asr/sense-voice/tokens-probe.txt
[Fail]Error opening file: permission denied, path:/data/app/el2/100/base/.../tokens-probe.txt
```

（`hdc file recv` 读得到，`sha256sum` 也算得了，只有写不行。）

验证期用的通道：宿主机 `hdc rport tcp:8000 tcp:8000` + `python3 -m http.server`，
应用侧 `http.requestInStream` 边收边写（`TranscriptionVerify.downloadModels`），
不把 239 MB 打进 HAP。真机实测：

| 文件 | 字节 | 耗时 | 速率 | 设备侧 sha256 |
|---|---:|---:|---:|---|
| `model.int8.onnx` | 239,233,841 | **8,818 ms** | 25.87 MB/s | `c71f0ce00bec95b07744e116345e33d8cbbe08cef896382cf907bf4b51a2cd51` |
| `tokens.txt` | 315,894 | 51 ms | 5.91 MB/s | `f449eb28dc567533d7fa59be34e2abca8784f771850c78a47fb731a31429a1dc` |

两个 sha256 与宿主机 `sha256sum $HOME/dev/caches/asr-ohos/sense-voice/*` **逐字节一致**。

### provider 配置（`providers.save`）

`adapter_id` 用本地引擎的 id **`sherpa_onnx`**（`provider/adapter.rs` 的
`local_engine::SHERPA_ONNX`）；`model` 指向沙箱模型目录：

```json
{"id":"local_asr_ohos","label":"本机转写（OHOS 沙箱模型）","adapter_id":"sherpa_onnx",
 "endpoint":null,"model":"/data/storage/el2/base/haps/entry/files/asr/sense-voice",
 "capabilities":["transcribe_audio"],"credential_ref":null,"enabled":true,"test_status":"untested"}
```

`provider_not_configured → waiting_configuration` 与 `succeeded` 的对照（同一份代码）：

```text
# 没有 provider 时录 5 秒、finalize 之后
DIARY_REC jobs.status ... "succeeded":17,"waiting_configuration":1
DIARY_REC TRANSCRIBE queued=true waitingConfiguration=1 ... runMs=3

# 存了 provider 再跑一轮 jobs.runDue
DIARY_ASR jobs.report {"open_ms":4,"run_ms":3580,...,"processed":1,"succeeded":1,
  "status":{"succeeded":18,"waiting_configuration":0,...}}
```

### 真机结果（5 秒中文录音）

链路：设备扬声器播 sherpa 的 `zh.wav`（5.59 s）→ `record` 录 5 s → 每 2 s 一段共 3 段
→ `recordings.finalize`（4,980 ms，3 段）→ `jobs.runDue` → `local_transcription` 派生文本 → 检索。

| 项 | 实测 |
|---|---|
| 录音 | 3 段（64,044 / 64,000 / 31,360 B），首包 41.2 ms，CPU 5.6%，wall 5,111 ms |
| 任务 | `waiting_configuration=1` →（存 provider 后）`succeeded=1`，`attempt_count=1`，`job_attempts` 13:41:30.901Z→13:41:34.481Z |
| 冷转写耗时（含加载 228 MiB 模型） | **3,580 ms**（另两次 3,582 / 3,697 ms） |
| 其中「加载模型 + 0.05 s 静音探针」 | **3,384 ms**（`asr_warm` 同进程先跑连接测试） |
| **热转写耗时**（模型已加载，4.98 s 音频） | **346 ms**（`run_ms`；同一轮的 `totalMs=3,764`） |
| 内存 PSS（冷） | 22,127 → 613,501 kB（**+591 MB**）；私有脏页 1,200 → 2,400 kB（**+1.2 MB**） |
| 内存 PSS（热态再加一条） | 549,641 → 612,935 kB（+63 MB） |
| 派生文本（原文） | **「开饭时间早上9点至下午。」** |
| 检索 | `search.start query=开饭` → `hits=2 elapsedMs=2`；`query=下午` → `hits=4`；命中项 `matched_by=["keyword"]`，locator 是音频 `[0, 4980) ms` |

> **范围口径（构建矩阵）**：默认 `build-hap.sh` **不带** 1.29.1，只带 sherpa 的 ORT
> 1.16.3（产品 `ort` crate 要求 ORT ≥ 1.17，`crates/diary_core/Cargo.toml:30`）⇒
> **关键词检索开箱可用、语义检索不可用**。语义检索用 `WITH_SEMANTIC=1` 构建（多带一份
> `libonnxruntime-diary-1.29.1.so`）；本 PR 已在产品身份用这个构建验通：查询
> 「母亲体检的结果怎么样」→ `hits=26`、`matchedBy=["semantic"]`，同一查询关键词模式
> `hits=0`。不要把默认 HAP 读成「检索全通」。

同一段音频四次录音的识别结果（说明可复现 + 偏差来自声学，不是链路）：

| 录音 | 采集 RMS / 峰值 | 派生文本 |
|---|---:|---|
| 21:29 | 7,120 / 32,176 | 开饭时间早上9点至下午5点。（全对） |
| 21:33 | 1,220 / 13,037 | 快饭时间早上9点至下午10点。（系统安全音量把媒体音量从 15 档降到 4 档，采集变小声） |
| 21:37 | 7,368 / 30,869 | 派饭时间早上9点至下午5点。 |
| 21:40 | 6,782 / 32,104 | 开饭时间早上9点至下午。（5 s 采集窗装不下 5.59 s 素材，句尾被截） |

> 结论口径：**链路与引擎是通的**（同一条音频能出正确全文，任务状态、派生文本、检索全对上），
> 但「扬声器 → 麦克风」这条声学路径会把字听错，不能拿它评价模型质量。
> 真人口述的样本在上一轮（`db-after-zhhuman`）里是全对的：
> 「我说这个游戏上不去分。」（`ext_01a14f1ec21072b38802aa616bed4ec1`）。

### 「没验到」的（别当成验过）

- **真人现场口述**（这一轮没人在设备旁；验证用的是设备扬声器回放）；
- **长音频**（>60 s）与多段拼接的转写耗时/内存；本机只有 5 s 一条的完整数字；
- **release / 混淆构建**带 sherpa 的包（这轮只装 debug）；
- **低内存机型**：592 MB PSS 增量在 31 GB 的 HAD-W32 上没问题，4–8 GB 手机未测；
- **双 ORT 并存 / 语义索引**：本 PR 已包含并在产品身份真机验通（`WITH_SEMANTIC=1`
  构建：sherpa 1.16.3 + 自编 1.29.1，不同文件名；语义查询 `hits=26`）。**默认构建不含
  1.29.1**，默认包仍只到关键词检索；本 PR 也证明了「把 `libonnxruntime.so` 直接换成
  1.29.1 会符号版本不匹配」；
- 电源键/锁屏路径（本轮按要求全程没碰）。

## 产品 HAP 的配置能力与双 ORT 共存（#108 收尾片，2026-10-18 夜）

鸿蒙应用**设不了环境变量**，而嵌入器与转写引擎的路径过去只能从 `DIARY_*` 读——产品 HAP 里
语义检索无法配置、转写只能靠 provider 行。这一片补了**文件配置**，并把**两个 ORT** 装进
同一个 HAP、在真机上跑通了「语义检索 + 转写」同进程共存。设计、取舍与完整证据见
[`docs/architecture/m6-设备配置与双ORT.md`](../../docs/architecture/m6-设备配置与双ORT.md)；
原始日志在 `/mnt/e/ohos/evidence/hapcfg/`。

### `core-config.json`：与库同目录，环境变量仍优先

`<filesDir>/core-config.json`（与 `library.sqlite` 同目录），四个字段全部可选：

```json
{
  "modelDir": "/data/storage/el2/base/haps/entry/files/models/bge-small-zh-v1.5-ours",
  "ortDylib": "/data/storage/el1/bundle/libs/arm64/libonnxruntime-diary-1.29.1.so",
  "asrEngineLib": "/data/storage/el1/bundle/libs/arm64/libsherpa-onnx-c-api.so",
  "asrModelDir": "/data/storage/el2/base/haps/entry/files/asr/sense-voice"
}
```

- **逐字段**顺序：环境变量 → 文件 → provider 行（仅转写）→ 随包默认；
- 文件不存在时行为与加它之前**逐位一致**；读不了 / JSON 坏 / 字段类型不对都**不致命**
  （回落下一档，原因进 `core.info`）；未知字段收进 `ignoredFields`，不静默吞掉；
- 只在 `Core::open` 时读一次（与 `DIARY_*` 同一语义：打开那一刻的快照）。

`core.info` 多一格 `runtime_config`（camelCase，与配置同形）：

```text
DIARY_HAP CONFIG label=info.final semantic.state=ready semantic.source=file modelDirSource=file ortDylibSource=file
  modelDir=/data/storage/el2/base/haps/entry/files/models/bge-small-zh-v1.5-ours
  ortDylib=/data/storage/el1/bundle/libs/arm64/libonnxruntime-diary-1.29.1.so
  version=bge-small-zh-v1.5-ours@d44978e451a0
DIARY_HAP CONFIG label=info.final transcription.state=ready transcription.source=file modelDirSource=file
  engineLibSource=platform providerId=local_asr_ohos
  modelDir=/data/storage/el2/base/haps/entry/files/asr/sense-voice
```

「环境变量仍优先」的证据是宿主测试（`/mnt/e/ohos/evidence/hapcfg/host-test-env-precedence.txt`）：
两个变量都设 → `configSource=env`；只设模型目录 → 模型目录 `env`、ORT `file`；全撤掉 →
回到文件那一套。另一半（无文件时行为不变、坏 JSON 不致命、文件压过 provider、缺文件
`invalid`）在 `core_config_test`。

### 两个 ORT：改名共存 + 绝对路径加载

| 包内文件 | 来源 | 大小 | sha256 | SONAME | 导出 |
|---|---|---:|---|---|---|
| `libonnxruntime.so` | sherpa 自带 1.16.3 | 12,930,856 | `f9fd9080…` | `libonnxruntime.so` | `OrtGetApiBase@@VERS_1.16.3` |
| `libonnxruntime-diary-1.29.1.so` | 自编 1.29.1 | 19,623,560 | `1ece24ca…`（源文件未 strip 是 `a2240074…` / 19,623,736） | `libonnxruntime.so.1` | `OrtGetApiBase@@VERS_1.29.1` |

**构建矩阵**：默认 `build-hap.sh` **不含** 1.29.1（`asr-only`，关键词可用）；
`WITH_SEMANTIC=1` 才带第二份（`stage-ort-1291.sh --check` 必须通过）。

sherpa 的 `NEEDED = libonnxruntime.so` 由系统按名解析到 1.16.3；`ort` 的 `load-dynamic`
按 `init_from` 的**绝对路径**加载自编那份，不依赖文件名与 SONAME 一致。**两者 SONAME
本来就不同**（PR #130 评审提出、本轮实测确认），`dlopen` 绝对路径与按名解析互不干扰。
真机上两种加载顺序都验过：先 1.29.1 后 1.16.3（默认）、先 1.16.3 后 1.29.1
（`--ps order asr_first`）。

```bash
apps/ohos-diary-core/scripts/stage-sherpa.sh     # 1.16.3 + libsherpa
apps/ohos-diary-core/scripts/stage-ort-1291.sh   # 自编 1.29.1 → libonnxruntime-diary-1.29.1.so
```

### 真机结果（HAD-W32 / API 23 / 产品身份 / debug signed）

**同一个 HAP、同一次启动、同一个进程**：

```text
# 语义（查询与目标正文没有共同词；同一条查询走关键词是 hits=0）
DIARY_HAP SEARCH label=search.semantic.semantic_first hits=26 phase=done coverage=complete warnings=
DIARY_HAP HIT label=... i=2 kind=text matchedBy=semantic dayKey=2026-10-17
  snippet=今天下午陪妈妈去医院复查，医生说骨头愈合得不错，下周再去拍一次片子。
DIARY_ASR SEARCH query=母亲体检的结果怎么样 hits=0 elapsedMs=3      ← 关键词对照

# 转写（已知 5.592 s 中文 PCM 走平台层录音 API → finalize → 任务）
DIARY_HAP INJECT SEG index=0 bytes=178988 pcmMs=5592 durable=true
DIARY_HAP INJECT FINALIZE asset=asset_01a14f60c699741fb7fe432a078f20d9 audioMs=5592 queued=true
DIARY_HAP JOBS phase=semantic_after_asr processed=3 succeeded=3 failed=0
DIARY_HAP TRANSCRIPT extractor=local_transcription coverage=complete status=ready
  text=开饭时间早上9点至下午5点。
```

三种配置来源各跑一次：只配语义（转写走 provider，`configSource=provider`）、四个字段全给
且**删掉 provider**（`configSource=file`、`engineLibSource=file`）、四个字段 + provider
（`providerId=local_asr_ohos`，但模型目录以文件为准——按「环境变量 → 文件 → provider」的链）。

| 项 | 实测 |
|---|---:|
| signed HAP（含两个 ORT + sherpa） | 真机验证的构建：**50,067,086 B**（#130 是 30,198,881，+18.95 MiB）；rebase 到 #130 后重构建：**50,088,328 B**（+18.97 MiB） |
| 语义阶段（bge 加载 + 两条日记切块/向量 + 检索） | 323–401 ms |
| 转写冷启动（sherpa + 1.16.3 + 228 MB 模型 + 推理 + 语义重建） | 3,440–3,573 ms |
| 一次启动总耗时 | 4.07–4.46 s |
| PSS 起步 → bge 就绪 | 42.7–45.1 MB → 128–159 MB（+75–114 MB） |
| PSS 再加转写引擎 | → 679–695 MB（+525–545 MB） |
| Private Dirty 增量 | +0.36–0.52 MB |

> **rebase 说明（2026-10-18 深夜）**：上面的设备证据来自 rebase 前的构建（HAP
> 50,067,086 B / sha256 `e1354cf7…`）。PR #134 之后 rebase 到 #130 的 `98ec24d`
> （下载完整性重写 + ArkTS 包装），**包内 native 库与两份 ORT 逐字节未变**
> （`libdiary_capi.so` 11,528,088、sherpa 3,750,328、ORT 12,930,856 与 19,623,560），
> 只有 `modules.abc` 从 215,928 涨到 229,900（+13,972 B：并入 #130 的下载实现与
> `hap_both` 的取文件），重构建 HAP 为 **50,088,328 B / `31d15915…`**。按要求没有再用
> 产品身份重跑真机；探针取文件此后已并成**一份**实现 `recording/ModelDownload.ets`
> （调用方必须给 `{bytes, sha256}`，`.part` + 2xx + 原子 rename；细节见下文「探针怎么关在
> debug 外」），不再有第二个取文件实现。下次动设备时先跑一次 `hap_both` 复核。

### PR #130 评审的 P0：缺引擎在 OHOS 上现在是「等配置」

`readiness` 在移动端按库名加载时**真 dlopen 探测一次**（`OnceLock` 缓存、成功后不 dlclose），
缺库 → `provider_not_configured`；`load_library()` 的「加载不到」也归这一类（只有「库在但
符号不对」仍是 `model_unavailable`）。把引擎库从 HAP 里拿掉再构建，真机任务状态是
`waiting_configuration` / `provider_not_configured` / `requires_user_action=1`，不再
`failed`/`model_unavailable`，也不会在同一轮里被反复拉起（修复中途实测过空转满 64 条上限，
所以闸门与执行者必须同时改）。

`asrEngineLib` 在 OHOS 上**拒绝沙箱路径**（`/data/storage/el2/...`、`/data/storage/el1/base/...`）：
那儿 `.so` 存在但不能 dlopen，配置解析时直接报清楚「放进 HAP 的 libs/arm64」。

### 复现

```bash
# 1. 原生库 + 两个 ORT + **含语义**的 HAP（签名材料见「用临时 bundleName 做真机验证」）
CARGO_BUILD_JOBS=4 bash apps/ohos-diary-core/scripts/build-native.sh
SHERPA_LIB_DIR=$HOME/dev/caches/asr-ohos/sherpa-onnx-v1.13.8-ohos-arm64-v8a/lib \
  bash apps/ohos-diary-core/scripts/stage-sherpa.sh
bash apps/ohos-diary-core/scripts/stage-ort-1291.sh     # 可选；默认构建会把它临时移出
source "$HOME/napi-device-scratch/product-sign.env"
WITH_SEMANTIC=1 SKIP_NATIVE=1 bash apps/ohos-diary-core/scripts/build-hap.sh
# 只要关键词检索（不含 1.29.1，HAP ~30 MB）：去掉 WITH_SEMANTIC=1

# 2. 宿主 HTTP **只提供 bge 模型**；core-config.json 由探针自己在沙箱里生成（不走 HTTP）
python3 -m http.server 8000 --bind 127.0.0.1 --directory "$HOME/dev/caches/hap-cfg/serve" &
hdc rport tcp:8000 tcp:8000

# 3. 装 + 跑（先 force-stop，否则 aa start 不重跑 onWindowStageCreate）
hdc install -r entry/build/default/outputs/default/entry-default-signed.hap
hdc shell "aa force-stop com.lingxing.unwrittendiary"
hdc shell "aa start -a EntryAbility -b com.lingxing.unwrittendiary -m entry --ps verify_mode hap_both"
hdc shell "hilog -x" | grep DIARY_HAP
# 顺序反过来：--ps verify_mode hap_both --ps order asr_first
```

### 探针怎么关在 debug 外（release 的诚实边界）

- `EntryAbility` / `ProbeAbility` / `ProbeIsoAbility` 都用 `BuildProfile.DEBUG` 门控；
  release 下 `EntryAbility` 直接走 `PROBE_DISABLED` 分支，**不跑** smoke / record* /
  asr_* / hap_both，不写库、不下载、不加载测试库。
- 示例页 `pages/Index` 的「录 5 秒 / 查看上次未收尾的录音」按钮直接调
  `RecordingVerify.run`（不走 `EntryAbility` 的路由）；该函数入口同样按
  `BuildProfile.DEBUG` 早退，release 下点了也不会录音/写库。
  `onDestroy` 仍会调 `RecordingVerify.shutdown()`，但 release 里 `heldRecorder` /
  `heldCore` 永远是 null，它只是空操作。
- **ArkTS 不支持条件 import**：release 的 `ets/modules.abc` 里**仍有探针模块**
  （实测 `strings` 能命中 `HapBothVerify` / `hap_both` / `PROBE_DISABLED`）——**没有裁掉**，
  只是运行时不可达。这是当前构建工具/语言的边界，不声称「已从包里移除」。
- `INTERNET` 只在 **debug** 变体声明（`build-hap.sh` 构建期注入 `module.json5`
  `requestPermissions`，退出时还原）；release 的 HAP 里只有 `MICROPHONE` 与
  `KEEP_BACKGROUND_RUNNING`（`unzip -p ... module.json` 实测）。
- `ProbeAbility` / `ProbeIsoAbility` 是 `exported: false`；`EntryAbility` 是产品入口
  （launcher 的 home skill 需要），保持 `exported: true`，探针路由已由 `BuildProfile` 关掉。
- `hap_both` 用**临时库** `<filesDir>/hap-both-tmp/library.sqlite`（配置也放同目录），
  跑完删掉；不再往当前资料库写测试数据。
- 模型下载只有一份实现（`recording/ModelDownload.ets`，`.part` → 2xx →
  Content-Length → 实收字节 → sha256 → 原子 rename + 重试）；`core-config.json`
  由探针本地生成，**不经 HTTP**。

### 没验到的（这一片）

- **release 真机启动没有跑**：只做了静态检查（release HAP 无 `INTERNET`、
  `BuildProfile.DEBUG=false`、探针分支会走 `PROBE_DISABLED`）与字符串检查（探针代码仍在
  `modules.abc` 里）；没有装 release HAP 去真机点一遍；
- 产品 Profile / 混淆都没重跑；本轮只装 debug；
- 转写用的是**已知 PCM**（麦克风→文本上一轮验过，声学路径不稳定：本轮第一次真录得到
  `The.`）；
- 低内存机型、大库、长时间、两个 ORT 同毫秒并发推理（`Core` 本就串行）、其它 ABI。

## 真机验证结果（2026-10-18）

### 环境与身份

| 项 | 值 |
|---|---|
| 设备 | HAD-W32，API 23，`arm64-v8a` |
| UDID | `B2F510C143AE78B664F1DA2FF9402941BC19C520AC7577F1803B6E2954427FA7` |
| hdc target | `3QC0124C20001776` |
| 安装身份 | **临时** `com.example.myapplication`（Profile 只绑它） |
| 产品身份 | `com.lingxing.unwrittendiary` **未验**（Profile 未生成） |
| 验证日期 | 2026-10-18 |
| HAP | debug signed 13,059,760 B；release signed 12,977,335 B；release+obf 12,977,389 B |

> 这是**技术验证**：证明 `.so` 能加载、沙箱能建库、ArkTS 能调到核心、HAP 能装能跑。
> 产品名/产品 Profile/产品 UI 未验证。

### 14 项逐条结论

| # | 项目 | 结论 | 关键证据 |
|---:|---|---|---|
| 1 | 真机 `dlopen`/`dlsym`（11.37 MB 的 `libdiary_capi.so`） | ✅ 通过 | `LIBS libdiary_capi.so bytes=11369640`；`capiVersion=diary_capi/0.1.0 apiVersion=1.0`；`open`/`call`/`close` 实际调用成功 |
| 2 | 沙箱建库、迁移到 schema 13、写入读回 | ✅ 通过 | `data_schema_version=13`；`DBSTAT ... bytes=389120`；save/commit/extract/search 全链路成功 |
| 3 | HAP 安装与启动 | ✅ 通过 | `msg:install bundle successfully.`；`start ability successfully.`；unsigned 失败时 `rc=0` 但 stdout 有 `error: no signature file` |
| 4 | `import diaryNapi from 'libdiary_napi.so'` 运行时语义 | ✅ 通过 | default import 拿到对象；`version/selfDir/open/call/close/lastError/openHandleCount` 7 个函数全部实际调用 |
| 5 | `libc++_shared` 随包效果 | ✅ 通过 | 包内 `libs/arm64-v8a/libc++_shared.so` 1,262,504 B；全量 hilog 中 `libc++_shared` 出现 0 次、没有指向我们 `.so` 的 MUSL-LDSO 错误 |
| 6 | 性能数字（至少中位数） | ✅ 通过 | 见下方性能表：open 冷 26.245 ms / 热 2.027 ms；coreInfo 0.213 ms；JSON 小 0.198 / 中 5.868 / 大 304.719 ms；search 首屏 37.647 ms；jobs 一轮 687.620 ms |
| 7 | 主线程阻塞 / ANR / 掉帧 | ✅ 已量化（有卡顿风险） | Smoke 265–272 ms（中位 268），事件循环额外延迟 99–129 ms，约 16–17 帧@60Hz；hilog 无 `APPFREEZE`/`ANR` |
| 8 | 两个并发 `call`（coreInfo + search） | ✅ 通过 | 同一句柄：2 个 worker（tid 26206/26211）+ 主线程，800+800+500 次，0 error；各自 open 同一路径：1200+1200 次，0 error，观测到 `maxObservedHandles=2`；无 panic/死锁 |
| 9 | 多进程 / 多 Ability | ✅ 通过 | 同进程两 Ability：pid 26187，`open OK handles=2` → `CLOSED handles=1`；隔离进程：`com.example.myapplication:probe` pid 27768，`ISO HOLD open ... handles=1`，两进程各自一个 Core |
| 10 | 错误路径 | ✅ 通过（发现并修了一个错误码降级 bug） | 空路径 `invalid_argument`；缺父目录/损坏库 `invalid_state`；同路径两次共享核心；关闭后调用 `invalid_state` / `ERR_DIARY_HANDLE`；非法 JSON/未知方法/缺参数 `invalid_argument`；`revision_conflict` 正确；全程不崩、`handles=0` |
| 11 | release / 混淆 | ⚠️ **门控前的历史证据**（当前 release 不跑 Smoke） | 门控前那次 release 构建：`BUILD_MODE=release` 签名安装成功、`debug:false`、Smoke 通过；混淆 HAP 也跑通 Smoke（构建后已还原 `enable:false`）。**现在** release 下 `EntryAbility` 走 `PROBE_DISABLED`，Smoke / record* / asr_* / hap_both 都不跑，`INTERNET` 也不在 release 包内 ⇒ 该证据只说明「当初验过」，不代表「现在还能验」；本轮也没装 release 真机（见「没验到的」） |
| 12 | 升级回滚（同 bundleName 重装，数据仍在） | ✅ 升级通过 | v1 `1000000/1.0.0` → v2 `1000001/1.0.1`；升级前后 `captures=4`、`hits=2`、`dbBytes=389120`、`schema=13` 完全一致 |
| 13 | hilog 实际输出 | ✅ 通过 | `DIARY_SMOKE JSON[...]` 10 个 label 全部按分片还原成合法 JSON（`search` 占 2 片）；`CLOSED handles=0` 出现 |
| 14 | C++ OOM 兜底 | ⚠️ 现状确认（未修） | shim 中没有 `try/catch`、没有 `nothrow`/`bad_alloc` 处理；`std::string`/`std::vector` 分配失败会以 C++ 异常穿透 NAPI 边界。未做 OOM 注入 |

### 第 1 项：dlopen / dlsym（11.37 MB 那份）

包内只有一份 `libdiary_capi.so`，大小 11,369,640 B（本机未 strip 11,369,880 B）：

```text
DIARY_VERIFY LIBS libdiary_capi.so bytes=11369640
DIARY_VERIFY LIBS libdiary_napi.so bytes=30824
DIARY_VERIFY LIBS libc++_shared.so bytes=1262504
DIARY_VERIFY BENCH capiLoad ms=2.404 version=diary_capi/0.1.0 apiVersion=1.0
```

`capiVersion` 来自 `diary_capi_version()`（dlsym 成功），`open` 来自 `diary_open()`，
`call` 来自 `diary_call()`，`close` 来自 `diary_close()`；`string_free`/`last_error` 在
每次 `open`/`call` 的返回值释放与错误路径里被实际走到。没有单独打印 `dlsym` 的每个地址，
但 6 个符号中任何一个缺失都会让 shim 抛 `ERR_DIARY_LIB`，实测没有出现。

### 第 2 项：沙箱建库 / schema 13 / 写入读回

```text
DIARY_SMOKE START db=/data/storage/el2/base/haps/entry/files/library.sqlite
DIARY_SMOKE open OK handles=1
DIARY_SMOKE JSON[core.info][1/1] {..."data_schema_version":13,"capture_count":4,...}
DIARY_SMOKE JSON[saveDraft][1/1] {"durable":true,"revision":2,...}
DIARY_SMOKE JSON[extract][1/1] {"coverage":"complete","status":"ready","text":"...",...}
DIARY_SMOKE JSON[search][1/2] {..."phase":"done","results":[...]}
DIARY_VERIFY DBSTAT path=/data/storage/el2/base/haps/entry/files/library.sqlite bytes=389120 wal=0 shm=0
```

冷 open（新库，含 schema 迁移）26.245 ms；热 open（已存在库）2.027 ms。
WAL 在 close 后归零（`wal=0`），说明连接正常收尾。

### 第 3 项：HAP 安装与启动

成功安装：

```text
[Info]App install path:.../entry-default-signed.hap msg:install bundle successfully.
AppMod finish
rc=0
```

启动：

```text
start ability successfully.
```

同时验证了「`hdc install` 失败时 rc 仍是 0」这个坑：

```text
[Info]App install path:.../entry-default-unsigned.hap msg:error: failed to install bundle. code:9568320 error: no signature file.
AppMod finish
rc=0
```

所以脚本/CI 必须 grep `install bundle successfully`，不能看返回码。

### 第 4 项：`import` 的运行时语义

`DiaryCore.ets` 的 `import diaryNapi from 'libdiary_napi.so'` 在设备上拿到的是可用对象，
不是 `undefined`：`version()`、`selfDir()`、`open()`、`call()`、`close()`、`lastError()`、
`openHandleCount()` 都被真实调用；`DiaryCore` 的 9 条契约方法（当时）全部返回了核心 JSON；录音这一片又补了 5 条，共 14 条。
没有出现探针 §24.1 那种「export objects of native so is undefined」。

### 第 5 项：`libc++_shared` 随包

包内清单：

```text
1262504  libs/arm64-v8a/libc++_shared.so
11369640 libs/arm64-v8a/libdiary_capi.so
   30824 libs/arm64-v8a/libdiary_napi.so
```

全量 hilog 检查（debug + release 两份）：

```text
grep -c "libc++_shared" full.log          → 0
grep -c "diary_napi|diary_capi" full.log  → 1（只有我们自己的 capiVersion 日志）
grep MUSL-LDSO full.log | grep -E "diary|libc\+\+" → (none)
```

也就是说：**没有**再出现 `load libc++_shared.so failed`，也**没有**指向
`libdiary_napi.so`/`libdiary_capi.so` 的 linker namespace 错误。系统日志里仍有其它
无关库的 MUSL-LDSO 警告（`libai_multitask_engine.so`、`libyundun.so` 等），与本包无关。

### 第 6 项：性能数字（3 次独立启动的中位数）

测量方法：

- 用 `systemDateTime.getUptime(STARTUP, true) / 1e6` 取单调毫秒（微秒级精度）；
- 每次 `bench` 启动都建一个**新库**，避免上一次缓存影响；
- 单次调用耗时先取 200/100/30 次样本的中位数；下表是 3 次独立启动（run2/3/4）的中位数再取中位；
- 检索数据集：40 条含 `benchneedle` 的记录，`page_size=20`，首屏 `hits=20`；
- 大 payload：512 KiB 正文的 `sources.extract`，响应 JSON 1,049,216 字符；
- 设备：HAD-W32 / API 23；debug HAP（release 同链路也跑过，量级一致）。

| 操作 | 样本/条件 | 中位数 | 备注 |
|---|---:|---:|---|
| 首次 `version()`（含 dlopen/dlsym） | 1 次/启动 | **2.404 ms** | `libdiary_capi.so` 11.37 MB |
| `open` 冷启动（新库 + schema 迁移） | 1 次/启动 | **26.245 ms** | 建库、建表、迁移到 13 |
| `open` 热启动（已有库） | 20 次/启动 | **2.027 ms** | 同路径反复 open/close |
| `coreInfo` | 200 次/启动 | **0.213 ms** | 响应 402 字符 |
| JSON 往返·小（`core.info` 原始信封） | 200 次/启动 | **0.198 ms** | 响应 440 字符 |
| JSON 往返·中（8 KiB 正文 `extract`） | 100 次/启动 | **5.868 ms** | 响应 17,022 字符 |
| JSON 往返·大（512 KiB 正文 `extract`） | 30 次/启动 | **304.719 ms** | 响应 1,049,216 字符 |
| `search.start` 首屏 | 40 条 / page 20 | **37.647 ms** | 首屏 `hits=20`，`phase=done` |
| `search.start` 热态 | 30 次/启动 | **36.148 ms** | 与首屏接近 |
| `jobs.runDue` 一轮（索引重建 1 条） | 5 轮/启动 | **687.620 ms** | `processed=1`；冷热差异很小 |

三次原始值（便于复核）：

| 项 | run2 | run3 | run4 |
|---|---:|---:|---:|
| capiLoad | 2.464 | 2.337 | 2.404 |
| openCold | 26.245 | 26.355 | 24.124 |
| openWarm median | 2.031 | 1.996 | 2.027 |
| coreInfo median | 0.213 | 0.213 | 0.214 |
| JSON small | 0.198 | 0.199 | 0.198 |
| JSON medium | 5.900 | 5.868 | 5.849 |
| JSON large | 305.366 | 304.719 | 304.542 |
| search first | 37.647 | 37.572 | 38.044 |
| search warm | 35.723 | 36.148 | 36.534 |
| jobs median | 683.734 | 687.620 | 694.798 |

> `jobs.runDue` 的 687 ms 是在 40 条记录 + 一篇 512 KiB 正文的库上做一次索引重建；
> 这不是「空队列」的数字。它说明**不能在 UI 主线程直接调 `runDueJobs()`**。

### 第 7 项：主线程阻塞 / ANR / 掉帧

方法：在 `onWindowStageCreate` 里先排一个 `setTimeout(0)`，再同步跑 `DiaryCoreSmoke.run()`；
同步任务不让出主线程，所以 `回调触发时刻 - 开始时刻 - smokeMs` 就是事件循环被额外阻塞的时间。
60 Hz 一帧 16.7 ms，`ceil(smokeMs/16.7)` 是估算掉帧数。

| 启动 | smokeMs | eventLoopDelayMs | 掉帧@60Hz | loadContent |
|---|---:|---:|---:|---:|
| 1 | 268 | 99 | 17 | 91 ms |
| 2 | 265 | 129 | 16 | 92 ms |
| 3 | 272 | 128 | 17 | 91 ms |
| **中位** | **268** | **128** | **17** | 91 ms |

- 全量 hilog 中 `APPFREEZE`/`ANR` 均为 0；当前数据规模下**没有触发 ANR**。
- 但 268 ms 同步阻塞 = 约 16–17 帧不掉，属于**可感知卡顿**；`bench` 里单次
  `runDueJobs` 687 ms、大 `extract` 305 ms 会掉 18–41 帧。
- 结论：示例这样跑可以接受（只用于验证），**产品必须挪到 TaskPool/worker**；否则大库上
  的 `search`/`runDueJobs` 有 AppFreeze 风险。这一点与 `DiaryCore.ets` 顶部注释一致。

### 第 8 项：并发

真机 TaskPool 能加载同一个 NAPI 模块；两个 worker 线程 + 主线程同时调用：

```text
CONC same-handle open id=1 handles=1
CONC same-handle main ok=500 err=0 spanMs=949
CONC same-handle worker[0] {"mode":"same-handle","pid":26006,"tid":26206,"method":"info","iterations":800,"ok":800,"errors":0,...}
CONC same-handle worker[1] {"mode":"same-handle","pid":26006,"tid":26211,"method":"search","iterations":800,"ok":800,"errors":0,...}
CONC same-handle closed handles=0
```

两个 worker 各自 `open` 同一路径（共享同一个 Core）+ 轮询句柄数：

```text
CONC own-handle start handles=0
CONC own-handle maxObservedHandles=2 finalHandles=0
CONC own-handle worker[0] {... "opened":true,"ok":1200,"errors":0,...}
CONC own-handle worker[1] {... "opened":true,"ok":1200,"errors":0,...}
CONC DONE handles=0
```

合计同一句柄 2,100 次、各自句柄 2,400 次调用，0 error；没有 panic、没有死锁、
没有句柄泄漏。C ABI 的 `Arc<Mutex<Core>>` 串行化符合设计。

### 第 9 项：多 Ability / 多进程

同进程两个 Ability（`EntryAbility` 跑 Smoke，`ProbeAbility` 持有句柄）：

```text
PROBE HOLD open pid=26187 handles=1
DIARY_SMOKE onCreate mode=smoke pid=26187
DIARY_SMOKE open OK handles=2          ← 共享同一张 shim 句柄表 / 同一个 Core
DIARY_SMOKE CLOSED handles=1
```

隔离进程 Ability（module.json5 里 `"process": ":probe"`）：

```text
PROBE HOLD open pid=27518 handles=1
ISO onCreate pid=27768
ISO HOLD open pid=27768 handles=1
```

```text
$ hdc shell "pidof com.example.myapplication"          → 27518
$ hdc shell "pidof com.example.myapplication:probe"    → 27768
$ hdc shell "ps -ef" | grep myapplication
20020160 27518 ... com.example.myapplication
20020160 27768 ... com.example.myapplication:probe
```

结论：**同进程共享一个 Core（句柄数 2）**；**不同进程各自一个 Core（各自 handles=1）**。
C ABI 的 `OPEN_LIBRARIES` 是进程内静态，这个行为符合文档；产品若用多进程，需要自己做
跨进程协调（SQLite WAL 本身能处理多连接，但「同路径共享 Core」只在进程内成立）。

### 第 10 项：错误路径

| 用例 | 实际结果（hilog `ERRCASE`） | 是否崩 |
|---|---|---|
| `open('')` | `code=invalid_argument` `资料库路径为空` | 否 |
| 不存在的父目录 | `code=invalid_state` `数据库错误：unable to open database file` | 否 |
| 同路径打开两次 | `handles=2`；关一个后另一个仍可 `coreInfo`；都关后 `handles=0` | 否 |
| `DiaryCore.close()` 后再调 | `code=invalid_state`（ArkTS `ensureOpen` 拦截） | 否 |
| 直接对已关闭 handle `call` | `native_unavailable`，message `ERR_DIARY_HANDLE: 句柄无效或已关闭（id=4）` | 否 |
| 直接重复 `close` | 同上 `ERR_DIARY_HANDLE` | 否 |
| 非法 JSON | 信封 `ok=false code=invalid_argument`（不抛到 JS） | 否 |
| 请求形状错 / 未知方法 / 缺参数 | 都是 `ok=false code=invalid_argument` | 否 |
| `revision_conflict` | `code=revision_conflict` `期望 revision 1，实际 2` | 否 |
| 损坏的库文件（写入非 SQLite 文本） | `code=invalid_state` `数据库错误：file is not a database` | 否 |

**发现并修复**：最初 `open` 失败的 `DiaryError.code` 是 `native_unavailable`，不是核心码。
原因有两层：

1. shim 的 `ERR_DIARY_OPEN` message 是 `ERR_DIARY_OPEN: {完整信封}`，而
   `DiaryError.fromThrown` 只按「message 以 `{` 开头」解析；
2. C ABI 的 `diary_last_error()` 实际返回完整失败信封
   `{apiVersion,ok:false,error:{code,...}}`，而 C ABI 文档写的是「与信封里的 error 同形」
   （`{code,message,retryable}`）。

已在 `DiaryError.ets` 做兼容解析（同时接受两种形状 + `: {` 前缀），设备复测后
`open-missing-parent` / `open-corrupt-file` 都恢复成核心码 `invalid_state`，message 不再带前缀。
C ABI 文档与实现的形状不一致仍建议后续统一（见问题清单 P2）。

### 第 11 项：release / 混淆

> **门控前的历史证据（2026-10-18）**：下面的「release Smoke 通过」来自
> `BuildProfile.DEBUG` 门控**之前**的 release 构建。**现在** release 下 `EntryAbility`
> 直接走 `PROBE_DISABLED`，Smoke / record* / asr_* / hap_both 都不跑，`INTERNET` 也不在
> release 包内——用同样的命令重装 release HAP，**Smoke 不会再跑**。所以这条是「当初验过」，
> 不是「现在还能验」；本轮 release 真机启动也没有重跑（见「没验到的」）。

- `BUILD_MODE=release` 构建 + 签名 + `hdc install -r` 成功；`bm dump` 显示
  `debug:false`，`versionCode=1000002/1.0.2`；**门控前** release Smoke 通过（`open OK handles=1`、
  `CLOSED handles=0`）。
- release 包的 hilog 默认被系统过滤（`hilog -T DiaryCore` 看不到），
  `hdc shell "hilog -b D"` 后才可见；这是运行时日志级别，不是应用没跑。
- 另外临时把 `entry/build-profile.json5` 的 `obfuscation.enable` 改成 `true` 构建了一次
  release（versionCode 1000003），安装后 Smoke 仍然通过（**同为门控前**），`data_schema_version=13`、
  `CLOSED handles=0`。**构建后已把该配置还原为 `enable:false`**，没有提交打开混淆的配置。
- 该混淆验证用的是工程默认空规则（`obfuscation-rules.txt` 为空），不是产品规则集。

### 第 12 项：升级 / 数据保留

```text
# v1：versionCode 1000000 / 1.0.0
PERSIST schema=13 captures=4 library=library hits=2 dbBytes=389120
# hdc install -r v2（1000001 / 1.0.1）
PERSIST schema=13 captures=4 library=library hits=2 dbBytes=389120
```

同 bundleName 升级安装后，库文件大小、schema、记录数、检索命中数完全一致。
**没有做降级安装**（旧 versionCode 覆盖新版本）——OHOS 通常不允许降级；本项按任务书
验证的是「升级后数据仍在」。

### 第 13 项：hilog 分片

从 `hilog-smoke-run3.txt` 用 Python 按 `DIARY_SMOKE JSON[label][i/n]` 重组：

```text
core.info: chunks=1 chars=391 json=OK
createDraft: chunks=1 chars=400 json=OK
saveDraft: chunks=1 chars=67 json=OK
commit: chunks=1 chars=759 json=OK
extract: chunks=1 chars=658 json=OK
search: chunks=2 chars=1107 json=OK
rebuildIndex: chunks=1 chars=421 json=OK
jobs.status.before: chunks=1 chars=201 json=OK
jobs.runDue: chunks=1 chars=227 json=OK
jobs.status.after: chunks=1 chars=201 json=OK
CLOSED handles=0
DONE processed=0 succeeded=0
```

`search` 是 2 片，拼起来是合法 JSON；10 个 label 全部可还原。
（`processed=0` 是因为重复 Smoke 复用固定的 `operation_id=op-arkts-index`，核心按幂等返回
已成功的任务，不是 `runDueJobs` 坏了。）

### 第 14 项：C++ OOM 现状

`napi_shim.cpp` 里没有 `try/catch`，也没有 `new (std::nothrow)` / `bad_alloc` 处理；
`ReadString` 的 `std::vector<char>`、`MakeString` 的 `std::string`、句柄表的
`std::unordered_map` 等分配失败都会抛 `std::bad_alloc`。C++ 异常穿过 NAPI 边界会终止进程。
本次没有做 OOM 注入（不可安全触发），只确认现状；与文档 §9 第 14 条一致，**未修**。

### 问题清单（按严重度）

| 级别 | 问题 | 状态 |
|---|---|---|
| **BLOCKING** | **无**：未发现崩溃、并发 panic/死锁、错误路径崩溃或已触发的 ANR | — |
| P1 | 主线程同步链路：Smoke 268 ms（17 帧）、`jobs.runDue` 687 ms、大 `extract` 305 ms；产品直接在主线程调用会卡 UI，大库可能 AppFreeze | 示例可接受；**产品接入前必须挪到 TaskPool/worker** |
| P1 | `open` 失败时核心错误码被降级成 `native_unavailable` | **已修**（`DiaryError.ets` 兼容完整信封与 `: {` 前缀），设备复测通过 |
| P2 | C ABI 文档说 `diary_last_error` 与 `error` 同形，实现却返回完整失败信封 | 已在 ArkTS 侧兼容；建议后续统一 C ABI 文档或实现 |
| P2 | shim 的 C++ OOM 无兜底 | 已知未修；OOM 属不可恢复错误，建议至少加 `bad_alloc` 边界 |
| P2 | `jobs.runDue` 单轮 687 ms（40 条 + 512 KiB 正文的索引重建） | 与 P1 同源；评估预算/异步/分段 |
| P3 | release 包 hilog 默认被系统过滤，排障需先 `hilog -b D` | 运维注意事项，不是代码 bug |
| P3 | `DiaryCore.nativeLibraryDir()` 返回带结尾 `/` 的目录（`.../arm64/`） | 与文档示例的 `.../arm64` 差一个斜杠，无功能影响 |
| P3 | 重复 Smoke 的 `DONE processed=0`（固定 `operation_id` 幂等） | 不是 bug；若希望每次都有任务，示例应换 `operation_id` |

### 设备已还原

验证结束执行：

```text
aa force-stop com.example.myapplication
hdc uninstall com.example.myapplication          → uninstall bundle successfully
hdc install /mnt/e/tmp/ohos-core-smoke/user-original.hap → install bundle successfully
```

还原前后 `bm dump` 关键字段逐行 diff：**`KEY_FIELDS_IDENTICAL`**，包括
`versionCode=1000000`、`versionName=1.0.0`、`nativeLibraryPath=""`、`debug=true`。
备份文件验证：

```text
160002  /mnt/e/tmp/ohos-core-smoke/user-original.hap
37d5da164a2b918ee2db42acdb043b6ebb7bda92e719bda138a4fbf43a0b21c8  user-original.hap
```

原包能正常 `aa start`（pid 32252，EntryAbility FOREGROUND），随后已 force-stop 回原状态。

> 限制：设备侧 `/data/app/el1/bundle/public/com.example.myapplication/entry.hap`
> 对 shell 用户不可读（`stat`/`sha256sum` 都是 `Permission denied`），所以无法对**已安装**
> 的 HAP 直接算 sha256。这里用的是「用同一份 sha256 已核对的备份重装 + bm dump 关键字段
> diff」双重校验；如果要直接比安装后文件哈希，需要 root/调试应用的文件访问权限。

### 没验到的（如实）

- **产品身份**：`com.lingxing.unwrittendiary` 的 Profile 未生成；所有设备结论只对临时
  `com.example.myapplication` 成立。
- **产品规模**：最大只到 40 条记录 + 1 篇 512 KiB 正文 + 4 条持久数据；没有真实大库
  （数千/数万条）的检索、重建索引、启动耗时。
- **真实 ANR/AppFreeze 阈值**：当前 Smoke 268 ms 未触发；没有构造 >6 s 的主线程阻塞去
  验证系统 AppFreeze 行为（不想在用户设备上人为制造 ANR）。
- **降级/回滚安装**：只验证了同 bundleName 升级（1000000→1000001），没有验证旧版本覆盖
  新版本；OHOS 通常拒绝降级。
- **OOM 注入**：只做代码检查，没有触发 `bad_alloc`。
- **产品混淆规则**：release 混淆验证用的是空规则；产品规则集可能不同。
- **release 签名身份**：release HAP 仍用现有 debug Profile 签名（`appProvisionType=debug`），
  不是 AGC 正式发布 Profile。
- **多进程产品设计**：只验证了「同 bundle 的隔离 Ability 进程各自一个 Core」，没有验证
  产品真正需要的跨进程共享/协调方案。

### 对 PR #122 的结论

**可以合**，前提是把本次的错误码修复一起带上（`DiaryError.ets`），并把「真机验证结果/
性能/问题清单」留在仓库里。没有发现 BLOCKING 级缺陷：安装、启动、dlopen/dlsym、沙箱建库、
import、随包 `libc++_shared`、并发、多 Ability/多进程、错误路径、release/混淆、升级数据保留
都在设备上真实跑过。

但请把 P1 当作**产品接入门槛**而不是本 PR 的阻塞项：`DiaryCore` 是同步的，示例在
`onWindowStageCreate` 里直接跑，268 ms/17 帧的卡顿已经可感知；`runDueJobs` 687 ms 更不能
放在 UI 线程。合并后应尽快做 TaskPool 异步包装和大库压测。

## 本机静态验证过什么

- `./scripts/build-native.sh`：交叉编译 + shim 编译 + 自检全过（6 个 C 符号、3 个 `.so`
  的 `NEEDED`/SONAME 检查）；
- `./scripts/build-hap.sh`：`BUILD SUCCESSFUL`；debug / release / release+obf 三种构建都出包；
- 把 `diaryNapi.open(123)` 故意写错做红测试：`CompileArkTS` 报
  `Argument of type 'number' is not assignable to parameter of type 'string'`
  ⇒ **`index.d.ts` 真的被用上了**，没有退化成 any；
- `python3 tools/check_arkts_capi_contract.py`：方法名 / 21 个错误码 / 6 个 C 符号 /
  7 个 JS 函数 / 打包三件套全部一致（2026-10-18 复跑通过）。

## 相关文档

- [m6-ArkTS-NAPI.md](../../docs/architecture/m6-ArkTS-NAPI.md)：分层、错误口径、契约映射、未验清单；
- [m6-核心-C-ABI.md](../../docs/architecture/m6-核心-C-ABI.md)：C ABI 的方法、信封、线程模型；
- 探针：`probes/ohos-core-smoke/`（dlopen / libc++_shared / HAP 安装的原始踩坑记录）。

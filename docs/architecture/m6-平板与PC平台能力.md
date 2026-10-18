# M6 · 平板与 PC 平台能力（#108 ⑦ 第一片：录音采集 → 核心录音片段）

对应 issue #108 · 分支 `feat/B8d-ohos-platform`（基于 `origin/main` = `e4e3c0a`，
先 merge 了 `origin/feat/B8b-arkts-napi`，因为它含 PR #117 的 C ABI 与 PR #122 的 NAPI 包装）·
执行人 qianxia · 2026-10-18

这一片把「**鸿蒙上真的能录一段音，并且这段音变成核心里的音频资产**」跑通：
`AudioCapturer`（PCM 16 kHz / 单声道 / 16 bit）→ 每 2 秒封一个片段写进核心给的暂存目录
→ `recordings.registerSegment` → 停止时 `recordings.finalize`（拼逻辑音频、建 Asset、
同事务排转写任务）→ 进程崩溃后下次启动 `recordings.recover` 如实报告并能救回来。

口径同仓库其它文档：**实测** = 真机跑过并贴输出；**未验** = 明确写没做；**判断** = 推断。

## 1 摘要（先看这个）

| # | 结论 | 强度 |
|---|---|---|
| 1 | 真机录 5 秒：3 个片段（64,044 / 64,000 / 31,360 B）→ finalize `state=saved`、`segment_count=3`、`logical_duration_ms=4980`、音频 blob 159,404 B | **实测**（HAD-W32 / API 23） |
| 2 | 最终化产出的 blob 是**合法单头 PCM WAV**：`RIFF/WAVE`、format=1、1 声道、16000 Hz、16 bit；`data` 长度字段是流式占位 `0xFFFFFFFF` | **实测**（读 blob 前 44 字节） |
| 3 | **按 Home 退到后台仍能继续录**：8 秒用例在后台完成 4 个片段并 finalize，`logical_duration_ms=8000` | **实测** |
| 4 | **锁屏（Power 键）后采集停止**：8 秒用例只录到 2.5 秒（2 段 80,044 B），亮屏后也没有恢复；长时任务本身申请成功 | **实测失败**（见 §7.3） |
| 5 | 进程崩溃（`process.abort()`，不走 `onDestroy`）后重启：`recordings.recover` 返回 `state=recoverable`、`closed_segment_indexes=[0,1,2]`、notes 写明「尾巴时长无法确定」；随后 finalize 成 6 秒完整音频 | **实测** |
| 6 | 权限被拒时如实返回：`authResults=[-1]`，日志 `PERM DENIED`，**没有**创建草稿/票据、没有开始录音 | **实测** |
| 7 | C ABI 原本**没有** `recordings.*`（PR #117 只接了 9 条），本片补了 5 条（prepare / registerSegment / updateState / finalize / recover）；`cargo check -p diary_capi --all-targets` 通过 | 实测（编译） |
| 8 | 转写链**没有跑通**：每次 finalize 都排了转写任务（`transcription_queued=true`），但设备上没有 ASR 引擎/模型，任务停在 `waiting_configuration` | **未验**（如实） |
| 9 | 没做：文件选择/导入、通知自定义、2in1 多窗口/多实例、后台唤醒、来电/音频焦点中断、播放/导出、TaskPool 异步化 | 明确未做（§9） |

**一句话**：录音 → 分片 → 核心 → 音频资产这条链路在 2in1 真机上**前台和后台（Home）都跑通了**；
**锁屏后采集会停**，这是这一片最需要产品/平台侧拍板的问题；转写因为缺模型没验。

## 2 实现结构（采集 → 分片 → 核心）

```
entry/src/main/ets/recording/
├── RecordingFormat.ets   采样参数、片段命名、44 字节 WAV 头、PCM↔毫秒换算
├── PcmSegmentWriter.ets  顺序写文件 + 增量 SHA-256 + 封闭/丢弃
├── MicPermission.ets     checkAccessTokenSync + requestPermissionsFromUser
├── ContinuousTask.ets    backgroundTaskManager.startBackgroundRunning(AUDIO_RECORDING)
├── VoiceRecorder.ets     一次录音的状态机：prepare → capturer → 每段 register → finalize
└── RecordingVerify.ets   真机探针（record / record_bg / record_hold / record_crash / recover…）
```

一次录音的调用顺序：

```text
createDraft（拿 capture_id）
  → recordings.prepare（拿 recording_id / asset_id / staging_ticket）
  → ContinuousTask.start(AUDIO_RECORDING)          // 失败不阻断前台录音，结果如实暴露
  → AudioCapturer(create → on('readData') → start)
  → recordings.updateState(recording, 0/0)
  → 每个 readData：写 PCM；每 2 秒封段 → recordings.registerSegment
  → 每秒一次 recordings.updateState(recording, elapsed/persisted)
停止：
  → capturer.stop/release
  → 封闭最后一段（没有 PCM 的空尾巴直接删掉）
  → recordings.updateState(stopping)
  → recordings.finalize(last_segment_index, end_reason)
  → ContinuousTask.stop
```

关键取舍：

- **核心调用不在每个 `readData` 里做**：`registerSegment` 只在片段封闭时调一次（2 秒一次），
  `updateState` 一秒一次；常规 `readData` 只做「写文件 + 增量哈希 + 判断是否该轮转」。
- **哈希在写的时候算**：用 `cryptoFramework.Md('SHA256')` 的 `updateSync/digestSync`，
  不写完后重新读文件（`@ohos.file.hash` 只有异步接口，录音回调里没有等待点）。
  核心登记时仍会**自己重算**，平台层给的哈希只是线索（与核心口径一致）。
- **时长按 PCM 字节算**（`bytes × 1000 / 32000`），不按墙上时间：卡顿或停顿不会把片段
  报长。`elapsed_ms` / `persisted_through_ms` 也用这个值。
- **失败不把会话标成 `error`**：采集出错时会话保持 `recording`，已封闭片段照常登记；
  进程真死之后，下次打开资料库时核心把它标成 `recoverable`（`recordings.recover` 才找得到）。
  如果标成 `error`，`latest_open_session` 反而看不见它。

## 3 采样参数与分片策略

| 项 | 值 | 依据 |
|---|---|---|
| 采样率 | 16,000 Hz | 转写链默认形态；`RecordingFormat.ets` 的 `SAMPLE_RATE` |
| 声道 | 1（单声道） | 语音日记不需要立体声；减小体积 |
| 位宽 / 格式 | 16 bit 有符号 PCM，小端 | `wav.rs` 只解 `Pcm16` / `Float32` |
| 每秒字节 | 32,000 B | 16000 × 1 × 2 |
| 片段时长 N | **2 秒**（默认，可配） | 5 秒验证能真实轮转出 3 段；产品可改成 10–30 秒 |
| 片段文件名 | `segment-000000.wav`、`segment-000001.pcm`… | 序号稳定、补零到 6 位 |
| `segment_id` | `seg-000000`… | 同一录音内稳定，进核心幂等指纹 |

### 3.1 为什么只有第 0 段带 WAV 头

`recordings.finalize` 是把片段文件**按字节直接拼接**成一条逻辑音频。如果每段都带 44 字节头，
拼出来就是「头 + 数据 + 头 + 数据」：第二个头会被当成 PCM 采样读（`wav.rs` 顶部注释记的已知坑），
而且第一个头声明的 `data` 长度只覆盖第 0 段，**转写链只能看到开头几秒**。所以平台层约定：

```text
第 0 段 = 44 字节 WAV 头（PCM / 16 kHz / 单声道 / 16 bit） + PCM
第 N 段 = 裸 PCM（N ≥ 1）
```

拼接后是**一条**头 + 全部 PCM，核心解码器读到 blob 末尾。

### 3.2 WAV 头里的长度字段是流式占位

头里的 `RIFF size` 与 `data size` 都写 `0xFFFFFFFF`。原因是**片段在录的时候就要登记**，
而总长度要到停止时才知道；登记后内容不能再改（核心会重算哈希，改了就对不上）。
核心的 `parse_layout` 取 `min(声明长度, 文件剩余长度)`，所以 `0xFFFFFFFF` 正好等于「读到 blob 末尾」。

**代价（如实写）**：最终化后的 blob 头部长度仍是 4 GiB 占位。核心自己用
`assets.media_duration_ms` 记了真实时长（本片实测 finalize 返回 5000/8000 ms），但外部播放器
若严格按头里的长度算，可能显示 4 GiB 或拒绝播放。**没有验证播放/导出**。彻底修需要改
F1 的 finalize 口径（停止时回填头并重算资产哈希），不在这一片；见 §10 拍板项。

## 4 权限与前台服务（实际做法）

### 4.1 声明（`entry/src/main/module.json5`）

```json5
"requestPermissions": [
  {
    "name": "ohos.permission.MICROPHONE",
    "reason": "$string:microphone_reason",
    "usedScene": { "abilities": ["EntryAbility"], "when": "inuse" }
  },
  { "name": "ohos.permission.KEEP_BACKGROUND_RUNNING" }
],
"abilities": [{
  "name": "EntryAbility",
  "backgroundModes": ["audioRecording"],
  …
}]
```

`ohos.permission.MICROPHONE_BACKGROUND` 在设备上的定义是
`grantMode=SYSTEM_GRANT / availableLevel=SYSTEM_CORE / availableType=SYSTEM`（`atm dump -d` 实测），
**三方应用申请不到**，所以后台收音只能靠「长时任务 + 普通麦克风权限」这条路。

### 4.2 运行时请求与拒绝

`MicPermission.ensureMicrophonePermission()`：

1. 先 `bundleManager.getBundleInfoForSelfSync` 拿 `accessTokenId`，
   `abilityAccessCtrl.checkAccessTokenSync` 查当前状态；
2. 未授权才 `requestPermissionsFromUser(context, ['ohos.permission.MICROPHONE'])`；
3. 返回 `granted` + 原始 `authResults`（0=授权，-1=拒绝，2=声明/条件不对）+ message；
4. 调用方（`RecordingVerify` / 页面）在 `granted=false` 时**不创建草稿、不开始录音**，只打错误日志。

真机拒绝实录（`hilog-perm-denied.log`）：

```text
DIARY_REC PERM state=denied permission=ohos.permission.MICROPHONE
DIARY_REC PERM request granted=false authResults=[-1,] permissions=ohos.permission.MICROPHONE
DIARY_REC PERM DENIED reason=用户拒绝了麦克风权限 authResultsLen=1
DIARY_REC VERIFY ABORT permission_not_granted
```

同一进程里没有出现 `CAPTURE` / `PREPARE` / `START` 日志——拒绝就是没录，不是「静默继续」。

### 4.3 长时任务（ContinuousTask）

`ContinuousTask.ets` 在**采集器启动之前**调用：

```ts
const agent = await wantAgent.getWantAgent({
  wants: [{ bundleName: context.abilityInfo.bundleName, abilityName: context.abilityInfo.name }],
  actionType: wantAgent.OperationType.START_ABILITY,
  requestCode: 20261018,
  actionFlags: [wantAgent.WantAgentFlags.UPDATE_PRESENT_FLAG],
});
await backgroundTaskManager.startBackgroundRunning(
  context, backgroundTaskManager.BackgroundMode.AUDIO_RECORDING, agent);
```

- 必须在 UIAbility 还在前台时申请；本片在 `onWindowStageCreate` 的录音验证路径里调用；
- 失败**不阻断**前台录音，但 `ContinuousTaskResult`（ok / 系统错误码 / message）会进日志
  `DIARY_REC TASK start failed code=…` 和 `DIARY_REC BG_TASK …`，不假装成功；
- 停止时 `stopBackgroundRunning(context)`；`aa force-stop` / 崩溃不会走到这里，系统会自己收。

真机（HAD-W32 / 2in1 / HarmonyOS 6.1.0 API 23）：

```text
DIARY_REC TASK start ok mode=audioRecording
DIARY_REC BG_TASK ok=true code=0 message=
DIARY_REC TASK stop ok
```

系统通知是长时任务自带的「正在录音」通知；本片**没有**自定义通知内容、没有验证通知点击跳转。

## 5 落到核心：C ABI 补的 5 条

PR #117 的 C ABI 只接了 9 条方法，**没有 `recordings.*`**；这一片在 `crates/diary_capi`
按既有扩展点补了 5 条（只做参数映射，不改核心、不加业务规则）：

| 方法 | 核心调用 | `data` |
|---|---|---|
| `recordings.prepare` | `Core::prepare_recording` | `RecordingTicket` |
| `recordings.registerSegment` | `Core::register_segment` | `SegmentReceipt` |
| `recordings.updateState` | `Core::update_recording_state` | `RecordingSession` |
| `recordings.finalize` | `Core::finalize_recording` | `RecordingFinalizeResult` |
| `recordings.recover` | `Core::recover_recording` | `RecordingRecovery` |

- 字段名以核心 Rust 参数名为准（snake_case），camelCase 作为别名；`deny_unknown_fields`；
- `recordings.registerSegment` 的 `segment` 是 `SegmentManifest` 的 JSON 形态；
- `recordings.updateState` 的状态字符串用核心的线上写法（`recording` / `stopping` / `saved`…）；
- 补了 `src/tests.rs` 的「prepare → register → updateState → finalize → 重开 → recover」
  与参数错误用例（本机受重线约束**没有跑 `cargo test`**，只做了 `cargo check --all-targets`；
  真机上的录音链路本身就是这 5 条方法的端到端实测）。

`DiaryCore.ets` 同步补了 5 个类型化方法（14 条契约方法），
`python3 tools/check_arkts_capi_contract.py` 通过。

## 6 恢复（进程被杀之后）

- 每次启动（产品路径与探针都走）先 `recordings.recover({})`，结果写 `DIARY_REC JSON[recover.before]`；
- 核心 `Core::open` 的 `recover_on_open` 会把上次没走完的会话标成 `recoverable`（有已封闭片段）
  或 `interrupted`（一个都没有）；
- `recover` **不会**假设最后一段完好：`closed_segment_indexes` 是真实已登记序号，
  notes 明确写「最后一段之后可能还有没封闭的尾巴，时长无法确定」；
- 探针 `recover_finalize` 按已封闭序号调 `recordings.finalize`，证明恢复出来的会话仍能救。

真机崩溃用例（`process.abort()`，不走 `onDestroy`）：

```text
DIARY_REC CRASH pid=43166 recording=rec_01a14e77b25772eba3191aea56977ced segments=3
# 重启后
DIARY_REC JSON[recover.before] {"closed_segment_indexes":[0,1,2],"gap_ms":0,
  "notes":["最后一段之后可能还有没封闭的尾巴，时长无法确定"],
  "recording_id":"rec_01a14e77b25772eba3191aea56977ced","state":"recoverable"}
DIARY_REC JSON[core.info] {… "recovery":{"notes":["有 1 段录音上次没有收尾"],"open_recordings":1,…}}
# recover_finalize
DIARY_REC JSON[recover.finalize] {"asset_id":"asset_01a14e77b25874a88f7b94f7a4c7a9ce",
  "logical_duration_ms":6000,"recording_id":"rec_01a14e77b25772eba3191aea56977ced",
  "segment_count":3,"state":"saved","transcription_queued":true}
DIARY_REC RECOVER_FINALIZE count=1
```

## 7 真机验证（2026-10-18）

### 7.1 环境与身份

| 项 | 值 |
|---|---|
| 设备 | HUAWEI MateBook Pro `HAD-W32`，`deviceTypes` 含 2in1，`const.product.devicetype=2in1` |
| 系统 | OpenHarmony-6.1.0.115；产品软件版本 `HAD-W24 6.1.0.117(SP68C00E100R13P3)`；API 23 |
| hdc target | `3QC0124C20001776` |
| 安装身份 | **产品名** `com.lingxing.unwrittendiary`，debug 签名（DevEco 自动签名 Profile） |
| HAP | debug signed `13,232,335 B`，sha256 `abe0a09479c4b3d6bb8605b92d31572ed6a0434e32801cd2028598225756c196` |
| 安装 | `msg:install bundle successfully.`（`hdc install -r` 的 rc 不可信，按 stdout 判定） |
| 窗口形态 | 2in1 窗口化（窗口不是全屏；`uitest dumpLayout` 的 root 是 `ContainerModal`） |

安装/启动原文：

```text
[Info]App install path:…/entry-default-signed.hap msg:install bundle successfully.
AppMod finish
start ability successfully.
```

### 7.2 录 5 秒（前台，`verify_mode=record`）

```text
DIARY_REC START recording=rec_01a14e78e7bc76c798064c1e702075aa
  sampleRate=16000 channels=1 bits=16 segmentMs=2000 bgTask=ok
DIARY_REC FIRST_DATA bytes=640 captureLatencyMs=37.468437 totalLatencyMs=78.382968
DIARY_REC SEG index=0 file=segment-000000.wav bytes=64044 pcmMs=2000 sha256=1a1faf924cad7415 durable=true
DIARY_REC SEG index=1 file=segment-000001.pcm bytes=64000 pcmMs=2000 sha256=95bdd79927ba900e durable=true
DIARY_REC SEG index=2 file=segment-000002.pcm bytes=31360 pcmMs=980  sha256=ae8150c4a5975545 durable=true
DIARY_REC FINALIZE {"asset_id":"asset_01a14e78e7bc76c798064c1f189ea050",
  "logical_duration_ms":4980,"recording_id":"rec_01a14e78e7bc76c798064c1e702075aa",
  "segment_count":3,"state":"saved","transcription_queued":true}
DIARY_REC STOP … segments=3 pcmMs=4980 stopLatencyMs=55.570313
DIARY_REC MEASURE wallMs=5110 cpuMs=263 cpuPercent=5.15 captureLatencyMs=37.468437
DIARY_REC BG_TASK ok=true code=0 message=
DIARY_REC WAV path=…/blobs/f1/f13c… bytes=159404 riff=RIFF wave=WAVE
  format=1 channels=1 rate=16000 bits=16 dataSizeField=4294967295
DIARY_REC STAGING remaining=0 names=
DIARY_REC JSON[jobs.status] {"queued":0,"succeeded":0,"waiting_configuration":1,…}
DIARY_REC TRANSCRIBE queued=true waitingConfiguration=1 succeeded=0 failed=0
```

数字：

| 项 | 值 |
|---|---|
| 片段数 | 3（2 s / 2 s / 0.98 s） |
| 片段字节 | 64,044（44 头 + 64,000）/ 64,000 / 31,360 |
| PCM 总字节 | 159,360 |
| 逻辑音频 blob | **159,404 B** = 44 + 159,360 |
| `logical_duration_ms` | 4,980 |
| 采集首包延迟 | **37.5 ms**（从 `capturer.start()` 发出到第一次 `readData`） |
| 启动总延迟 | 78.4 ms（从 `createDraft` 前到首包） |
| 停止 → finalize 完成 | **55.6 ms** |
| 进程 CPU（录音期间） | **263 ms / 5,110 ms = 5.15%**（`process.getPastCpuTime()`） |

多次 5 秒运行的稳定性：片段结构一致（64,044 / 64,000 / 31,360–32,000），
`logical_duration_ms` 4,980–5,000，首包 36.4–40.0 ms，CPU 4.98%–5.37%。

### 7.3 后台与锁屏

**后台（Home）——通过**：8 秒用例在 `verify_mode=record_bg` 启动后约 1.5–2 秒按 Home，
之后片段照常按 2 秒轮转：

```text
18:00:23.337 FIRST_DATA bytes=640 captureLatencyMs=35.660781
18:00:25.322 SEG index=0 bytes=64044 pcmMs=2000
18:00:27.327 SEG index=1 bytes=64000 pcmMs=2000
18:00:29.329 SEG index=2 bytes=64000 pcmMs=2000
18:00:31.326 SEG index=3 bytes=64000 pcmMs=2000
18:00:31.392 FINALIZE … "logical_duration_ms":8000,"segment_count":4,"state":"saved"
18:00:31.402 MEASURE wallMs=8193 cpuMs=467 cpuPercent=5.70
```

blob `256,044 B` = 44 + 8×32,000；`STAGING remaining=0`。**Home 后台录音成立。**

**锁屏（Power）——失败**：8 秒用例启动后约 2 秒按 Power（屏幕关），
亮屏后（第 11 秒再按 Power）没有恢复采集。加了 `stateChange` / `audioInterrupt`
监听后拿到了**直接原因**：

```text
18:09:00.834 START …
18:09:00.834 CAPTURE state=1 phase=recording          # PREPARED
18:09:00.834 CAPTURE state=2 phase=recording          # RUNNING
18:09:02.832 SEG index=0 bytes=64044 pcmMs=2000
18:09:03.954 CAPTURE state=3 phase=recording          # STOPPED（系统停的）
18:09:03.955 CAPTURE interrupt eventType=1 forceType=0 hintType=3
                                                       # BEGIN / FORCE / HINT_STOP
18:09:08.854 SEG index=1 bytes=35200 pcmMs=1100       # 停止前最后 1.1 秒
18:09:08.861 FINALIZE … "logical_duration_ms":3100,"segment_count":2,"state":"saved"
18:09:08.872 MEASURE wallMs=8144 cpuMs=197 cpuPercent=2.42
```

即：**系统在熄屏时强制 STOP 了 AudioCapturer**（`InterruptForceType=INTERRUPT_FORCE`，
`InterruptHint=INTERRUPT_HINT_STOP`），亮屏后没有 `INTERRUPT_TYPE_END`/`RESUME`，
采集器也没有回到 RUNNING。第二次锁屏/解锁用 `record_hold` 复现：18:01:44 后不再有
任何片段，18:01:51 亮屏后到 18:01:55 仍没有新片段；`aa force-stop` 后 `recover` 得到
`closed_segment_indexes=[0,1]`、`state=recoverable`、notes 写「尾巴未知」。

**结论（只对这台 2in1 / 这个系统版本）**：长时任务申请成功、Home 后台能录，
**按 Power 熄屏后系统强制停止采集，亮屏也不自动恢复**。最终化不会报错——它按
「已登记片段」拼出 3.1 秒音频、`state=saved`，缺失的约 5 秒是**未被核心发现的尾巴**。

代码现在做的：`stateChange` 一旦在 `recording` 期间变成非 RUNNING，就把
`interrupted=true`，停止时把 `end_reason` 写成 `<reason>_interrupted`，日志打
`CAPTURE INTERRUPTED` / `STOP interrupted=true`——**不把系统中断伪装成正常结束**。
但核心的 `finalize` 只能按「已登记片段有没有缺口」决定 `saved/recoverable`，
表达不了「墙上时间还有一段没录」；这个语义缺口需要产品/核心一起定（§10）。

### 7.4 转写：排了任务，没跑

每次 finalize 都 `transcription_queued=true`，`jobs.runDue` 之后：

```text
DIARY_REC JSON[jobs.runDue] {"processed":0,…,"succeeded":0,"waiting_dependency":0}
DIARY_REC JSON[jobs.status] {"waiting_configuration":1,"succeeded":0,"failed":0,…}
DIARY_REC TRANSCRIBE queued=true waitingConfiguration=1 succeeded=0 failed=0
```

设备上没有 ASR 引擎/模型（`DIARY_ASR_ENGINE_LIB` / `DIARY_ASR_MODEL_DIR` 未配置），
任务被闸门挡在 `waiting_configuration`。**转写没有验**：没有产出文本、没有测 CER、
没有验证「转写结果进检索」。这不等于转写链坏了，只等于**这一段没有在真机上跑过**。

### 7.5 权限拒绝（重装后首次启动）

```text
DIARY_REC PERM state=denied permission=ohos.permission.MICROPHONE
DIARY_REC PERM request granted=false authResults=[-1,] permissions=ohos.permission.MICROPHONE
DIARY_REC PERM DENIED reason=用户拒绝了麦克风权限 authResultsLen=1
DIARY_REC VERIFY ABORT permission_not_granted
```

没有 `CAPTURE` / `PREPARE` / `START`；页面/探针都只显示失败，不会假装在录。

## 8 设备现在的状态

验证结束后：

- 应用 **保留安装**：`com.lingxing.unwrittendiary`，versionCode `1000000` / `1.0.0`，
  debug 签名，`reqPermissionStates=[0,0]`（MICROPHONE 与 KEEP_BACKGROUND_RUNNING 都已授权）；
- 应用 **已 force-stop**（`pidof` 空），没有在录音、没有残留长时任务；
- 数据：最后一次 5 秒验证（159,404 B blob）与锁屏验证（99,244 B blob）留下 2 条 capture、
  2 个 blob、2 条 `waiting_configuration` 转写任务；`recordings.recover` 返回 `open_recordings=0`；
- 中间做过一次「卸载 → 重装」来重置权限（拒绝用例），**更早的验证数据已被清掉**；
  上面的日志/文件大小是清理前抓的，证据在 `/mnt/e/ohos/evidence/`（本机临时目录，不进仓库）；
- ⚠️ **设备当前停在锁屏**：锁屏用例按了 Power（被测行为本身），之后 hdc 不能替用户
  输密码/指纹。需要人工解锁后设备才回到可操作状态；应用本身仍是安装好的。

## 9 没有做到的（如实）

- **锁屏继续录没做到**：Power 熄屏后采集停止，亮屏不恢复（§7.3）；没有验证「锁屏但屏幕不关」
  （如合盖/超时锁屏）与「来电中断」；
- **转写没验**：没有引擎/模型；`waiting_configuration` 只是「任务在队列里等配置」；
- **播放/导出没验**：blob 头长度是 4 GiB 占位，外部播放器行为未知；
- **文件选择/导入没做**：`imports.*` 仍未接 C ABI，这一片只做「自己录」；
- **通知没做**：长时任务通知是系统生成的；没有自定义文案、按钮、点击跳转，
  也没有单独验证通知在 2in1 上的展示；
- **2in1 多窗口/多实例没做**：窗口化形态下跑过，但没有验证分屏、悬浮窗、多实例同时录音；
- **后台唤醒/定时任务没做**：没有 WorkScheduler / 闹钟 / 开机自启；
- **音频焦点/中断没做**：没有处理来电、其他应用抢占麦克风、输入设备切换（`input_device_changed`
  恒为 false）、暂停/继续；
- **TaskPool 异步化没做**：`registerSegment` / `updateState` / `finalize` 都在主线程/采集回调
  上同步跑；5 秒用例 CPU 5%、没观察到爆音，但长录音/大库上需要 worker；
- **压力/异常没做**：没测 1 小时长录音、磁盘满、录音中撤销权限、低电量、热插拔麦克风；
- **只测了一台设备**：HAD-W32（2in1）；手机/平板/模拟器没测；
- **`kill -9` 没做成**：设备 shell 是 `uid=2000(shell)`，`kill -9` 应用进程报
  `Operation not permitted`；改用应用内 `process.abort()` 做真崩溃，用 `aa force-stop`
  做「优雅终止但会话未收尾」。两者都验证了 recover，但**不是** SIGKILL。

## 10 需要拍板的地方

1. **C ABI 补 5 条录音方法**：这一片必须改 `crates/diary_capi`（否则 ArkTS 根本到不了核心）。
   改动是纯映射 + 测试，没有改核心；请确认可以随本分支一起评审。
2. **WAV 头长度占位 0xFFFFFFFF**：现在能拼出单头、可被核心解码的 WAV，但头部长度不是真值。
   要不要改 F1 的 finalize，在停止时回填头并重算资产哈希？那是跨片改动，影响导入/导出/播放。
3. **分片时长 2 秒**：验证好切、开销可接受（CPU 5%）；产品建议 10–30 秒。定多少？
4. **启动时自动 finalize 恢复出来的录音**：探针 `recover_finalize` 证明能做到，
   但产品上要不要**自动**救、还是提示用户确认？涉及「缺尾巴的音频算不算一段完整日记」。
5. **锁屏录音**：这是硬需求（F1）还是可以接受「锁屏即停」？若要继续，
   需要平台侧方案（ACL 申请 `MICROPHONE_BACKGROUND`、AVSession、或系统白名单），
   不是应用层能单独解决的。
6. **转写模型部署**：模型放哪、怎么下发/校验、没有模型时界面怎么提示——这一片只把任务排上。

## 11 复现命令

```bash
cd apps/ohos-diary-core

# 1. 原生库（Rust C ABI + NAPI shim；约 1 分钟，需 NDK）
CARGO_BUILD_JOBS=4 bash scripts/build-native.sh

# 2. 签名 HAP（签名材料不进仓库；用 DevEco 自动签名生成的 p12/cer/p7b）
source "$HOME/napi-device-scratch/product-sign.env"
SKIP_NATIVE=1 SKIP_OHPM=1 BUILD_MODE=debug bash scripts/build-hap.sh
hdc install -r entry/build/default/outputs/default/entry-default-signed.hap

# 3. 真机验证（先 force-stop，否则 aa start 只会 onNewWant，不会重跑 onWindowStageCreate）
hdc shell "aa force-stop com.lingxing.unwrittendiary"
hdc shell "aa start -a EntryAbility -b com.lingxing.unwrittendiary -m entry --ps verify_mode record"
hdc shell "hilog -x -T DiaryRec" | grep DIARY_REC

# 后台：启动后 2 秒内按 Home
hdc shell "uitest uiInput keyEvent Home"

# 崩溃恢复
hdc shell "aa start … --ps verify_mode record_crash"     # 6 秒后 process.abort()
hdc shell "aa start … --ps verify_mode recover"          # 看 closed_segment_indexes / notes
hdc shell "aa start … --ps verify_mode recover_finalize" # 救回来

# 静态检查
cargo check -p diary_capi --all-targets
python3 tools/check_arkts_capi_contract.py
python3 tools/check_docs.py
```

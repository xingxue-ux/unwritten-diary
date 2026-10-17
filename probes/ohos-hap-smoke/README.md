# 最小 HAP 冒烟探针（`probes/ohos-hap-smoke`）

> 目的：只回答两件事——**① API 26 SDK 编出的 HAP 能不能装到 API 23 设备；② 签名/安装链路通不通**。
> **不含任何产品代码**：`crates/**`、`packages/**`、`apps/**` 一行未动。
> bundleName 用的是**临时值**：`com.xingxue.unwrittendiary.smoke`（见 `AppScope/app.json5`）。
>
> 结论与完整证据见 `docs/architecture/m6-鸿蒙可行性探针.md` 的续篇「最小 HAP 打到真机」。

## 目录结构

```
probes/ohos-hap-smoke/
├── AppScope/app.json5                  bundleName / 版本号 / 图标
├── AppScope/resources/...              应用级资源（图标、app_name）
├── build-profile.json5                 ★ signingConfigs(空) + compatible/targetSdkVersion
├── hvigor/hvigor-config.json5          modelVersion 26.0.0
├── hvigorfile.ts                       appTasks
├── oh-package.json5                    无依赖（不需要 ohpm install / 不需要联网）
├── entry/
│   ├── build-profile.json5             apiType: stageMode
│   ├── hvigorfile.ts                   hapTasks
│   ├── oh-package.json5
│   └── src/main/
│       ├── module.json5                deviceTypes: 2in1/tablet/phone
│       ├── ets/entryability/EntryAbility.ets   ★ 冒烟证据都从这里打 hilog
│       ├── ets/pages/Index.ets                 空壳页面
│       └── resources/base/...
└── scripts/
    ├── build-hap.sh                    构建（可选签名）；可覆盖 bundleName / API 版本
    └── run-on-device.sh                安装 → 启动 → 抓 hilog
```

## 命令序列（这就是要进 CI 的那条）

```bash
# ── 0. 前置（本机已具备，无需安装）
#   ~/command-line-tools                 Linux x64 命令行工具 26.0.0.821
#                                        （HarmonyOS SDK 26.0.0 / API 26 / 26.0.0.105，
#                                          hvigor 6.26.4，自带 node v24.14.1）
#   java 21 在 PATH 上                    签名那一步要 java -jar hap-sign-tool.jar
#   调试签名材料在 ~/.ohos/config/       （p12 + cer + p7b + material/）

# ── 1. 构建（不签名）
cd probes/ohos-hap-smoke
~/command-line-tools/bin/hvigorw --mode module \
  -p product=default -p module=entry@default -p buildMode=debug \
  assembleHap --no-daemon
# → entry/build/default/outputs/default/entry-default-unsigned.hap

# ── 2. 构建 + 签名（一条命令，脚本内部先注入 signingConfigs 再跑 hvigor）
OHOS_SIGN_STORE_FILE=/mnt/c/Users/<user>/.ohos/config/<hash>.p12 \
OHOS_SIGN_CERT_FILE=/mnt/c/Users/<user>/.ohos/config/<hash>.cer \
OHOS_SIGN_PROFILE_FILE=/mnt/c/Users/<user>/.ohos/config/<hash>.p7b \
OHOS_SIGN_STORE_PASSWORD_ENC=0000001A....（DevEco 加密串） \
OHOS_SIGN_KEY_PASSWORD_ENC=0000001A....（DevEco 加密串） \
  ./scripts/build-hap.sh
# → entry-default-signed.hap

# ── 3. 安装 + 启动 + 抓日志（真机挂在 Windows 侧，所以 HDC 用 Windows 版 hdc.exe）
HDC="/mnt/e/huawei/DevEco Studio/sdk/default/openharmony/toolchains/hdc.exe" \
  ./scripts/run-on-device.sh com.example.myapplication
# 等价于手工三步：
#   "$HDC" install 'E:\tmp\ohos-hap-smoke\entry-default-signed.hap'
#   "$HDC" shell "aa start -a EntryAbility -b com.example.myapplication -m entry"
#   "$HDC" shell "hilog -x -T DiarySmoke"
```

`build-hap.sh` 还会用 `stat`/`sha256sum` 打印产物体积与哈希；`entry-default-signed.hap` 与
`entry-default-unsigned.hap` **每次构建哈希都不同**（打包/签名带时间戳），CI 不要拿哈希做等值断言。

## 三个坑（都是实测撞出来的）

### 坑 1：`compatibleSdkVersion` 必须是字符串 `"6.1.0(23)"`，不是数字 23

`runtimeOS: "HarmonyOS"` 时 hvigor 要求 API 版本写成 `<平台版本>(<API>)` 形式：

| `compatibleSdkVersion` | 生成 `module.json` 的 `minAPIVersion` | 装到 API 23 设备 |
|---|---|---|
| `"6.1.0(23)"` | `60100023` | ✅ 成功 |
| `"26.0.0"` | `260000026` | ❌ `code:9568297 install failed due to older sdk version in the device` |

映射表在 hvigor 插件里（`@ohos/hos-sdkmanager-common/build/res/hos-config.json`）：
`6.1.0↔23`、`6.0.0↔20`、`5.0.0↔12`；API 26 起平台版本与 API 号同值（`26.0.0↔26`）。
**不需要额外安装 API 23 的 SDK 组件**——一套 API 26 SDK 就能编出 `minAPIVersion=23` 的包。

### 坑 2：`signingConfigs` 里的密码必须是 DevEco 加密后的十六进制串

hvigor 6.26.4 对 `storePassword`/`keyPassword` **无条件**调用
`DecipherUtil.decryptPwd()`，明文密码会直接以 `INVALID_DATA` 退出（长度 <32）。
加密串形如 `0000001A<26字节IV+密文+16字节GCM tag>`。解密用的密钥材料取的是
**p12 所在目录下的 `material/`**（`DecipherUtil.getKey` 里 `path.resolve(storeFile, "..")`），
所以只要 p12 还在 `~/.ohos/config/`，在 Linux 侧用 `/mnt/c/Users/<user>/.ohos/config/xxx.p12`
绝对路径就能解密成功——**不需要 Windows、不需要 DevEco GUI**。

> 加密串从哪来：DevEco 自动签名后写在项目的 `build-profile.json5` 里。
> 本机从 `E:\huawei\a\build-profile.json5` 抄到了同一套 p12 的两个加密串
> （`storePassword == keyPassword`，解密后 12 字符；解密脚本见探针文档附录）。

### 坑 3：hvigor 拒绝给「bundleName 与 Profile 不一致」的包签名

```
ERROR: 00303074 Configuration Error
Error Message: The bundleName in app.json5/hvigorfile.ts does not match the bundleName
               in the generated SigningConfigs.
```

代码路径：`SignUtil.executeSign → validateBundleName → validatePackageName`，
先 `hap-sign-tool.jar verify-profile` 解出 `.p7b` 里的 `bundle-info.bundle-name`，再和
`AppScope/app.json5` 的 `bundleName` 比对，不一致就 `printErrorExit("BUNDLE_NAME_DOES_NOT_MATCH")`。
**没有命令行开关可以跳过**（唯一豁免分支是「用的就是默认调试密钥库 `~/.ohos/config/debug.p12`」，
我们这套不是）。

绕过这个检查直接调 `hap-sign-tool.jar sign-app` 能签出来，但**设备侧会拒**：

```
$ hdc install E:\tmp\ohos-hap-smoke\smoke-signed.hap
msg:error: failed to install bundle. code:9568329 error: verify signature failed.
```

→ 结论：**换 bundleName 必须重新申请调试 Profile**（DevEco「自动生成签名」或 AGC 控制台），
这一步需要华为账号，命令行做不了（详见探针文档续篇的「卡点」）。

## 设备侧实测结果

真机：HarmonyOS 6.1 PC（HAD-W32 / 2in1 / API 23 / arm64-v8a / UDID `3QC0124C20001776`）。

| 项 | 值 |
|---|---|
| 未签名 HAP 安装 | ❌ `code:9568320 error: no signature file.` |
| 签名 HAP（bundleName 与 Profile 一致，min=23）安装 | ✅ `install bundle successfully.` |
| 启动 | ✅ `aa start` → `start ability successfully.`，进程存活 |
| 自定义 hilog tag | ✅ `DiarySmoke`（见下） |
| 未签名的 `hdc install` 退出码 | ⚠️ **失败时仍是 0**，CI 必须 grep stdout |
| 产物体积（debug） | unsigned **120,301 B**（~118 KB）/ signed **156,281 B**（~153 KB） |
| 构建耗时（干净树、无缓存） | **~10 s**（34 个 task） |
| 全新安装耗时 | **~0.4 s**（156 KB，USB） |
| 启动耗时（`onCreate`→`loadContent` 回调） | **abilityStartMs=69**，contentMs=54（debug 包） |

应用沙箱真实路径（第 2 步「.so 塞进 HAP 跑 SQLite」的路径策略依据）：

```
filesDir       = /data/storage/el2/base/haps/entry/files       ← 可写（实测写入+读回成功，36 字节，writeMs=0）
cacheDir       = /data/storage/el2/base/haps/entry/cache
tempDir        = /data/storage/el2/base/haps/entry/temp
databaseDir    = /data/storage/el2/database/entry
bundleCodeDir  = /data/storage/el1/bundle                      ← HAP 内资源/原生库前缀
```

> 注意 `bundleCodeDir` 是 **el1**（只读），`filesDir`/`databaseDir` 在 **el2**（可写）。
> 原生库落点（`nativeLibraryPath`）本次**没测**：`UIAbilityContext` 上没有
> `nativeLibraryDir` 属性（编译报错），留给第 2 步实测。

## 复现脚本

```bash
# 只构建
./scripts/build-hap.sh

# 换 API 版本 / 换 bundleName（构建后自动还原文件）
OHOS_COMPAT_SDK_VERSION=26.0.0 OHOS_BUNDLE_NAME=com.example.myapplication ./scripts/build-hap.sh

# 装到真机
HDC=/path/to/hdc.exe ./scripts/run-on-device.sh <bundleName>
```
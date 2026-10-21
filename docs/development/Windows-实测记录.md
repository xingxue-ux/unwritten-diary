# Windows release 实测记录（2026-10-20）

对应 issue **#89**（M5 交付工程）。本文把 [`M5-打包与发布.md`](M5-打包与发布.md) 里
「Windows 没有产物、体积是估算」的那一段补成**实测**：在一台真实 Windows 机器上跑完
[`tools/release/build-windows.ps1`](../../tools/release/build-windows.ps1)，产出便携包 zip，
逐项量体积，启动一次并核对数据目录。

> 口径沿用 M5：**实测** = 本次真的跑出来并量过；**估算** = 由已知字节数推导、没有最终包。
> 体积同时给十进制 MB（10^6）与 MiB（2^20），避免把 44.67 MiB 写成 44.67 MB。

## 0. 结论

| 问题 | 实测结论 |
|---|---|
| 这台机器能不能出 Windows release 包 | **能**。`cargo build` + `flutter build windows --release` + 校验 + 打包全过，脚本退出码 0，总耗时 **4 分 40 秒**（首次） |
| 便携包解压后多大 | **46,840,257 B** = 46.84 MB = 44.67 MiB（45 个文件） |
| zip 多大 | **20,164,621 B** = 20.16 MB = 19.23 MiB |
| zip SHA256 | `10A1B88B59BF9BF7799CE30211C728DA543BDE4184449930B2A9BF581113BDF5` |
| 在不在 180 MB 以内 | **在**。46.84 MB 是预算的 **26.0%**（「Windows ≤180 MB」指纯程序体积，本包不含任何模型） |
| 「Flutter 外壳 25–45 MB」这条估算准不准 | **落在区间内**：实测外壳（引擎 + `data\` + 插件 DLL）= 31,616,044 B = 31.62 MB = 30.15 MiB |
| 启动成功吗 | **成功**。进程启动后 10 秒仍存活且 `Responding=True`，主窗口标题 `不写日记`；正常关窗退出码 0 |
| 数据目录建了吗 | **建了**，位置是 `%APPDATA%\Lingxing\不写日记\unwritten_diary\`，`library.sqlite` 由应用自己建好（schema 15、`quick_check=ok`、36 张表） |

## 1. 构建环境（命令真实输出）

| 项 | 值 |
|---|---|
| 系统 | Microsoft Windows 11 家庭版 中文版 `10.0.26300`（Build 26300；`flutter doctor` 报 26H2 / 2009） |
| CPU / 内存 | AMD Ryzen 7 8845H / 15.3 GB |
| Flutter | **3.44.6** stable，Dart **3.12.2**，engine `83675ed276`（与 CI 钉的版本一致） |
| Rust | `rustc 1.94.1 (e408947bf 2026-03-25)`、`cargo 1.94.1 (29ea6fb6a 2026-03-24)` |
| Rust host | `rustup` 的 `default_host_triple = "x86_64-pc-windows-gnu"`（这次踩到，见 §2 坑 4） |
| Visual Studio | Community **2026** Insiders `18.6.11716.218`，装在 `C:\Program Files\Microsoft Visual Studio\18\Insiders` |
| MSVC | `cl` 19.51.36237 for x64、`link` 14.51.36237.0、toolset 14.51.36231 |
| Windows SDK | 10.0.26100.0 |
| CMake | 4.2.3-msvc3（VS 自带），Flutter 实际用的 generator = `Visual Studio 18 2026` |
| Ninja | 1.13.2（VS 自带） |
| PowerShell | 5.1.26100.9711 |

与任务前提的两处偏差，按实测写：

- **VS 不是 2022**。`C:\Program Files\Microsoft Visual Studio\2022` 目录存在但**是空的**；
  `vswhere -all -prerelease` 只报一个实例：Community 2026 Insiders 18.6。MSVC 14.51 就在这个实例里。
- **数据目录不是 `UnwrittenDiary`**。`Runner.rc` 里 `ProductName` = `不写日记`，
  `path_provider_windows` 用 `%APPDATA%\<CompanyName>\<ProductName>\`，
  所以真实路径是 `%APPDATA%\Lingxing\不写日记\unwritten_diary\`（与 `升级与迁移.md`、`诊断包与排障.md` 一致）。
  任务书里写的 `...\Lingxing\UnwrittenDiary\` 在这台机器上**不存在**。

## 2. 怎么跑的，以及踩到的坑

仓库用干净工作树：`git worktree add /mnt/e/wt-win -b chore/windows-release-verify origin/main`
（Windows 侧 `E:\wt-win`，基于 `origin/main` @ `68a878f`）。构建目录 `CARGO_TARGET_DIR=E:\target-win`。

| # | 遇到 | 怎么解 / 没解 |
|---|---|---|
| 1 | **脚本被 Windows PowerShell 5.1 解析不了**：`tools/release/build-windows.ps1` 是 **UTF-8 无 BOM**，中文系统上 5.1 按 GBK 读，直接 `Unexpected token` / `MissingEndCurlyBrace`，连 `-DryRun` 都进不去 | 本地副本加 BOM 后跑。**仓库里的脚本没改**（本 PR 只加记录），这是待修项，见 §7 |
| 2 | `Get-Command cmake` 失败 → 脚本第 1/6 步 `Fail '找不到 cmake'`。VS 自带 CMake 但不在 PATH 上 | 把 `...\18\Insiders\Common7\IDE\CommonExtensions\Microsoft\CMake\CMake\bin` 和 `...\CMake\Ninja` 前置进 `PATH` |
| 3 | `vswhere -latest -products * -requires ...VC.Tools.x86.x64` 返回空 → 脚本误判「Visual Studio 已安装但没有 MSVC 工具链」。原因是该 VS 是 **pre-release**，vswhere 默认过滤掉 | 本地副本给 vswhere 加 `-prerelease`。**仓库里的脚本没改**，见 §7 |
| 4 | `cargo build` 退出码 101，`error: linking with 'x86_64-w64-mingw32-gcc' failed: ... lld: error: unable to find library -lgcc_eh`。因为 `rust-toolchain.toml` 钉的 `channel = "1.94.1"`，而 rustup 的 `default_host_triple` 是 **gnu**，于是解析成 `1.94.1-x86_64-pc-windows-gnu`，主机侧 build script 走 mingw 链接器，而这台机器的 mingw 缺 libgcc | 设 `RUSTUP_TOOLCHAIN=stable-x86_64-pc-windows-msvc`（**同一个 1.94.1**，只换 host）。另外先给 1.94.1 工具链补过 `rustup target add x86_64-pc-windows-msvc`，但主机侧仍走 mingw，所以没救回来 |
| 5 | 脚本和 `apps/diary_app/windows/CMakeLists.txt` 都把 DLL 路径写死成仓库内 `target\x86_64-pc-windows-msvc\release\diary_bridge.dll`，和「`CARGO_TARGET_DIR=E:\target-win` 不污染仓库」冲突 | 在 `E:\wt-win\target` 建 NTFS **junction** 指向 `E:\target-win`：cargo 写外面，脚本和 CMake 按原路径读到同一份文件 |
| 6 | `flutter pub get` 把**已提交的 `pubspec.lock` 改写了 8 个包**（`code_assets 2.1.0→1.2.1`、`hooks 2.2.0→2.0.2`、`matcher 0.12.20→0.12.19`、`meta 1.19.0→1.18.0`、`objective_c 9.6.2→9.5.0`、`record_use 1.1.1→0.6.0`、`test_api 0.7.12→0.7.11`、`vector_math 2.4.3→2.2.0`），还重写了 `windows/flutter/generated_plugin_registrant.cc` / `generated_plugins.cmake`（原来一个插件都没注册，实际要注册 `file_selector_windows`、`flutter_timezone`，FFI 插件列表里多出 `jni`） | 提交前 `git checkout --` 全部还原，本 PR 只含本文。这是**真实的复现性问题**：锁文件不是用 Flutter 3.44.6 生成的，见 §7 |
| 7 | `cargo` 下载 crate 时出现过 1 次 `warning: spurious network error (3 tries remaining): [35] SSL connect error`；`pub.dev` 正常 | 自动重试成功，无需手工干预 |

首次构建耗时（脚本自己报的）：Rust `Finished release profile in 2m 21s`，Flutter `Building Windows application... 79.8s`，
整个脚本 `00:04:39.93`。磁盘占用（按文件大小求和，不按 `du`）：`E:\target-win` **1,217,295,737 B ≈ 1.22 GB**、
`apps\diary_app\build` **78 MiB**、`dist` **64 MiB**。`du -sh E:\target-win` 只报 916 MiB——
drvfs 上 `st_blocks` 偏低，别拿它当占用。删掉 `E:\target-win` 后 `E:` 可用空间从 135.22 GB 回到 136.12 GB。

## 3. 体积实测

### 3.1 便携包与 zip

| 项 | 字节 | 十进制 | MiB |
|---|---:|---:|---:|
| 便携包目录（解压后，45 个文件） | **46,840,257** | 46.84 MB | 44.67 MiB |
| zip（`unwritten-diary-0.1.0-windows-x64.zip`，Optimal） | **20,164,621** | 20.16 MB | 19.23 MiB |

zip 压缩后约为解压体积的 43.0%。**SHA256（脚本输出原样）**：

```
Algorithm : SHA256
Hash      : 10A1B88B59BF9BF7799CE30211C728DA543BDE4184449930B2A9BF581113BDF5
Path      : E:\wt-win\dist\windows\unwritten-diary-0.1.0-windows-x64.zip
```

### 3.2 四个大文件 + 其余构成

| 文件 | 字节 | 十进制 | MiB | SHA256（前 16 位） |
|---|---:|---:|---:|---|
| `flutter_windows.dll` | **21,284,864** | 21.28 MB | 20.30 MiB | `A5C880A4218A04CF` |
| `diary_bridge.dll` | **14,750,720** | 14.75 MB | 14.07 MiB | `1E408733A43F5627` |
| `data\app.so`（Dart AOT） | **6,751,120** | 6.75 MB | 6.44 MiB | `6920649B197E52AC` |
| `unwritten_diary.exe` | **76,800** | 0.08 MB | 0.07 MiB | `F66D59586D59F095` |
| `data\icudtl.dat` | 862,304 | 0.86 MB | 0.82 MiB | `998367809A821D59` |
| `data\flutter_assets\`（字体/图标/NOTICES/着色器/legal 副本） | 2,465,852 | 2.47 MB | 2.35 MiB | — |
| 插件 DLL（`file_selector_windows_plugin` 110,592 + `flutter_timezone_plugin` 82,944 + `dartjni` 58,368） | 251,904 | 0.25 MB | 0.24 MiB | — |
| `legal\` 副本（ORT ThirdPartyNotices 等） | 395,609 | 0.40 MB | 0.38 MiB | — |
| `LICENSE` | 1,084 | — | — | — |
| **合计** | **46,840,257** | **46.84 MB** | **44.67 MiB** | — |

几点值得记下来：

- `unwritten_diary.exe` 只有 **75 KiB**——Windows 的 Flutter runner 本来就薄，界面和引擎都在
  `flutter_windows.dll` + `data\app.so` 里，别把这个数字当成「包很小」。
- 包里 **没有任何模型**（`bge`、`SenseVoice`、`onnxruntime` 都不在），与
  `04-验收与测试计划.md` §4.1 末「Windows 当前构建不含任何模型」一致；实测 45 个文件里没有 `.onnx`。
- 体积大头是 `flutter_windows.dll`（45.4%）+ `diary_bridge.dll`（31.5%），两者合计 76.9%。

## 4. 与预算、估算的对比

| 口径 | 文档里的数字 | 性质 | 本次实测 | 结论 |
|---|---|---|---|---|
| 单架构发布包（Windows，纯程序体积） | ≤ 180 MB | 参考预算（`04-验收与测试计划.md` §4.1） | **46.84 MB** | **在预算内**，占 26.0%，余量 133 MB |
| Flutter 外壳（引擎 + Dart AOT + 资源） | 25–45 MB | **估算**（`M5-打包与发布.md` §4.2） | **31.62 MB**（引擎 21.28 + `data\` 10.08 + 插件 0.25） | **落在区间中部**，估算成立 |
| 核心桥接 DLL | 「约 12.7 MB」/ Linux x64 12,615,392 B | 量级估计 | **14.75 MB**（14,750,720 B） | 比估计大 **+2.05 MB**（Windows + MSVC + 依赖，合理） |
| zip 体积 | 无估算 | — | 20.16 MB（19.23 MiB） | 首次有数 |

一条要紧的边界：**180 MB 是「纯程序体积」**，本包确实不含模型，所以 46.84 MB 可以直说；
一旦将来把 `bge`（23.83 MB）或 `SenseVoice`（239.23 MB）打进 Windows 包，这条结论立即失效，
必须按 §4.1 的规则重算，不能沿用 26.0% 这个比例。

## 5. 启动验证：只写看到的事实

环境：上面那个便携包目录，`Start-Process` 直接启动，未设任何环境变量、未改配置。

| 观察项 | 结果 |
|---|---|
| 第 1 次启动（PID 45668，21:15:40） | 10 秒后仍存活，`Responding=True`，WorkingSet **117.6 MB**，124 线程 |
| 数据目录 | `%APPDATA%\Lingxing\不写日记\unwritten_diary\` 在启动后 **1 秒内**创建（21:15:41） |
| 库文件 | `library.sqlite` 4,096 B、`library.sqlite-shm` 32,768 B、`library.sqlite-wal` 420,272 B |
| 第 2 次启动（PID 46144） | 9 秒后仍存活，`Responding=True`，WorkingSet 117.3 MB；复用已有库，没有崩 |
| 第 3 次启动（PID 47260） | 主窗口句柄 `8589274`，标题 **`不写日记`**（UI 真的起来了，不只是后台进程） |
| 正常关窗 | 发 `WM_CLOSE` 后 **退出码 0**（6 秒内退出）；随后 `library.sqlite` = **409,600 B**，`-wal`/`-shm` 消失（退出时 checkpoint 了） |
| 库的真伪 | 只读方式打开：`user_version=15`、`journal_mode=wal`、`quick_check=ok`、**36 张表**（`captures`/`diary_versions`/`chunk_vectors`/`jobs`…） |
| 崩溃痕迹 | `Application` 事件日志最近 5 分钟**没有**该 exe 的条目 |
| 收尾 | 三次启动的进程都已结束（`Stop-Process -Id … -Force` 或正常关窗），没有留窗口在桌面上 |

两条必须说清楚的限制：

1. **这不是「功能验收」**。我没有点击任何按钮、没有录音、没有导入、没有搜索、没有生成日记；
   能证明的只有「进程起来、窗口在、数据目录和库被建出来」。
2. 117 MB 是 **WorkingSet**，不是 `04-验收与测试计划.md` C44 要求的「私有工作集」，
   **不能**拿来当「空闲内存 ≤300 MiB / ≤200 MiB」的结论。
3. 用 `-Force` 杀掉的那两次，退出时没有 checkpoint：主库仍是 4 KB、正文全在 420 KB 的 `-wal` 里。
   正常关窗才会合并。也就是说**只复制 `library.sqlite` 单文件可能拿到空库**——取库前先让应用正常退出，
   或整目录复制。这一条是观察，不是本轮修的问题。

## 6. 本轮没验到的（不要当成通过）

- **安装包**：只出了便携 zip。Inno Setup 的 `.iss` 在 **PR #152**，**尚未合入**，本轮没有安装器可测。
- **代码签名**：`Get-AuthenticodeSignature` 返回 **`NotSigned`**，没有证书。SmartScreen 在别的机器上的实际行为未验（B22/B23）。
- **干净机器**：这台机器装着 VS 2026 + Flutter + Rust，属于开发机；「没有开发环境的干净 Windows 10/11 x64 解压即启动」（B20）**未验**。
- **卸载路径**：便携包没有卸载器，卸载/覆盖升级（B21）**未验**。
- **真机交互项**：C 段（录音/导入/搜索/日记/高 DPI/麦克风权限…）一项都没跑。

## 7. 这次暴露、但**没有**在本 PR 修的问题

本 PR 只加这份记录，产品代码与脚本一行未动。下面四条是实测暴露出来的真问题，建议单独开：

1. **`tools/release/build-windows.ps1` 在中文 Windows PowerShell 5.1 上无法解析**（UTF-8 无 BOM）。
   文档里给的调用命令 `powershell -ExecutionPolicy Bypass -File tools\release\build-windows.ps1`
   在这台机器上第一条就挂。修法：加 BOM，或把中文提示换成 ASCII，或要求 `pwsh` 7。
2. **同一脚本的 vswhere 调用缺 `-prerelease`**：装了 pre-release 版 VS（2026 Insiders）时会被误判成
   「没有 MSVC 工具链」，直接 `exit 2`。
3. **`pubspec.lock` 与 Flutter 3.44.6 不一致**：`flutter pub get` 会降级 8 个包（见 §2 坑 6），
   并重写两个 `generated_plugin_*` 文件。发布检查清单 A2「Flutter 版本已固定」目前实际不成立，
   锁文件不是这个 SDK 生成的。
4. **`rust-toolchain.toml` 的 `channel = "1.94.1"` 在 `default_host_triple = gnu` 的 rustup 上会解析成 gnu host**，
   主机侧 build script 走 mingw，链接直接失败。要么在脚本里显式 `RUSTUP_TOOLCHAIN=<ver>-x86_64-pc-windows-msvc`，
   要么把 channel 写成完整三元组。

## 8. 复现命令

```powershell
# 前置：干净工作树 E:\wt-win（基于 origin/main），junction 让 cargo 输出到 E:\target-win
cmd /c mklink /J "E:\wt-win\target" "E:\target-win"

$vs = "C:\Program Files\Microsoft Visual Studio\18\Insiders"
$env:PATH = "E:\dev\flutter\bin;C:\Users\<你>\.cargo\bin;" +
            "$vs\Common7\IDE\CommonExtensions\Microsoft\CMake\CMake\bin;" +
            "$vs\Common7\IDE\CommonExtensions\Microsoft\CMake\Ninja;" + $env:PATH
$env:CARGO_TARGET_DIR   = "E:\target-win"
$env:RUSTUP_TOOLCHAIN   = "stable-x86_64-pc-windows-msvc"   # 必须：见 §7-4
Set-Location E:\wt-win

# 本轮用的是加了 BOM + vswhere -prerelease 的本地副本（未提交）；
# 脚本修好前，仓库里的原版在第 1/6 步就会挂。
powershell -NoProfile -ExecutionPolicy Bypass -File tools\release\build-windows.ps1
```

产物：`E:\wt-win\dist\windows\unwritten-diary-0.1.0-windows-x64\`（便携目录）与同名 `.zip`。
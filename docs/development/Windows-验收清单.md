# Windows 验收清单（装机后逐条走）

> 目的：把「能在干净机器上装出可运行的 Windows 包」这件事变成**可复核的记录**，而不是「应该能跑」。
> 适用范围：`apps/diary_app` 的 Windows x64 构建（便携 zip 与 Inno Setup 安装包）。
>
> 怎么用：在一台**没装过本应用**的 Windows 10/11 x64 上，从 A 开始逐条走，把结果填进最后一节的表。
> 任何一条失败都要记 `commit sha` + 失败现象，不要只写「不行」。
>
> 相关：打包命令见 `M5-打包与发布.md` §3；数据目录为什么是这个路径见 `升级与迁移.md` §4.1。

---

## A. 构建产物（在构建机上）

- [ ] A1. `tools\release\build-windows.ps1` 从干净工作区跑通，退出码 0。
- [ ] A2. `dist\windows\unwritten-diary-<version>-windows-x64\` 里有这 6 个东西：
      `unwritten_diary.exe`、`diary_bridge.dll`、`flutter_windows.dll`、
      `data\icudtl.dat`、`data\app.so`、`data\flutter_assets\AssetManifest.bin`。
- [ ] A3. `diary_bridge.dll` 与 `unwritten_diary.exe` **同目录**（放到 `data\` 或 `lib\` 下会加载失败）。
- [ ] A4. `legal\` 目录在（13 份第三方许可），`LICENSE` 在根目录。
- [ ] A5. 签名（发布必须做，否则别的机器会拦）：
      `Get-AuthenticodeSignature .\unwritten_diary.exe` 返回 `Status = Valid`，
      且有**时间戳**（`TimeStamperCertificate` 非空）。
- [ ] A6. `Get-FileHash .\unwritten-diary-*.zip` 与发布记录里的 sha256 一致。
- [ ] A7. exe 属性 → 详细信息：公司名 `Lingxing`、产品名 `不写日记`、版本与 `pubspec.yaml` 一致。
      **这两个值决定数据目录，发布后不能再改。**

## B. 便携包（不装，直接跑）

- [ ] B1. 解压到 `C:\Temp\unwritten-diary\`，双击 `unwritten_diary.exe` 能起来（首屏不是白屏/闪退）。
- [ ] B2. 未签名包在别的机器上会弹 SmartScreen「Windows 保护了你的电脑」——**这是预期**，
      记录下来；签名包不应弹。
- [ ] B3. 首次启动后，`%APPDATA%\Lingxing\不写日记\unwritten_diary\library.sqlite` 被创建。
- [ ] B4. 关闭再打开，数据还在；第二次启动明显更快（没有重复建库）。
- [ ] B5. 中文路径下也能跑：把解压目录改成 `C:\Temp\日记 测试\` 再试一次。

## C. 安装包（Inno Setup）

- [ ] C1. 双击 `unwritten-diary-<version>-windows-x64-setup.exe` 能启动向导，语言/许可页正常。
- [ ] C2. **按用户安装**（默认，不弹 UAC）：装到 `%LOCALAPPDATA%\Programs\UnwrittenDiary\`。
- [ ] C3. 第一页选「为所有用户安装」时会弹 UAC，装到 `C:\Program Files\UnwrittenDiary\`。
- [ ] C4. 开始菜单有快捷方式；勾了桌面图标那一项时桌面也有。
- [ ] C5. 从快捷方式启动能正常进首屏。
- [ ] C6. 应用正在运行时再跑一次安装包：向导提示关闭应用并继续（Restart Manager），
      装完能重新拉起。
- [ ] C7. **覆盖升级**：改 `pubspec.yaml` 版本号重打一个包，装到同一目录，
      版本信息更新、**数据还在**、不需要先卸载。
- [ ] C8. 卸载：控制面板/开始菜单卸载 → 卸载完成后弹窗提示「日记数据没有被删除」。
- [ ] C9. 卸载后检查：`%LOCALAPPDATA%\Programs\UnwrittenDiary\`（或 Program Files 下）**已删**；
      `%APPDATA%\Lingxing\不写日记\` **仍在**。

## D. 数据目录与迁移

- [ ] D1. 卸掉旧构建（`CompanyName = com.example` 那版）、装上新构建：
      旧数据目录 `%APPDATA%\com.example\不写日记\` 不会被删，但新构建**看不到**它
      —— 属于「首发前切换一次」的预期，不是回归。
- [ ] D2. 卸载重装后，数据仍在同一路径下被复用（`D1` 之后的数据）。
- [ ] D3. 手工把 `%APPDATA%\Lingxing\不写日记\` 改名后启动：应用应该正常新建一个空库
      （而不是崩溃或卡死）。
- [ ] D4. 库文件被外部占用/只读时启动：应给出可读的错误提示，不是静默退出。

## E. 功能冒烟（Windows 上必须真跑一次）

- [ ] E1. 录音：麦克风授权 → 录 10 秒 → 有波形/时长 → 停止后落库。
- [ ] E2. 转写：本机引擎配置好后，录音能出文字（模型没配时应是「等配置」而不是失败——见 #130 的 P0）。
- [ ] E3. 搜索：关键词能命中；语义索引就绪后语义也能命中。
- [ ] E4. 备份：导出一份 `.diarybak`，能再恢复回去。
- [ ] E5. 诊断包：导出成功，**解压后不含正文**（这是隐私承诺，见 `诊断包与排障.md`）。
- [ ] E6. 长时任务：录音时锁屏/切用户再回来，录音不中断（Windows 的录音没有鸿蒙那个 HINT_STOP 限制）。

## F. 干净机器反证

- [ ] F1. 在一台**从未装过 Flutter/Rust/VS** 的 Windows 上装安装包 → 能跑
      （如果有 VC++ 运行时依赖，会在这里暴露；Flutter Windows 目前依赖系统自带的 VC++ 运行库）。
- [ ] F2. 卸载后重装，前面 E 节的数据还在。
- [ ] F3. 应用自己的日志里，`%APPDATA%` 路径与本文档写的一致（没有写到 `data\` 或安装目录）。

---

## 结果记录

| 日期 | 机器（型号/系统版本） | commit | 包（zip/安装包 + sha256 前 8 位） | A | B | C | D | E | F | 失败项与现象 |
|---|---|---|---|---|---|---|---|---|---|---|
| 2026-10-28 | AMD Ryzen 7 8845H / Windows 11 家庭版 中文版 10.0.26300（**开发机**：装着 VS 2026 Insiders + Flutter 3.44.6 + Rust 1.94.1） | `6a6e1b7` | zip `unwritten-diary-0.1.0-windows-x64.zip` `14D1F93E`；安装包 `...-setup.exe` `C3C700B9`（17,161,405 B） | 6/7 | 4/5 | 6/9 | 3/4 | 0/6 | 0/3 | A5 未签名（无证书）；B2/C 交互向向导未逐屏点（用 `/VERYSILENT` 走同一路径）；E 组未做（合成输入无法稳定驱动 Flutter 输入框）；F 组无干净机器 |

> 没有这一行记录 = 没有验过。`M5-打包与发布.md` §3.5 的「未在本次执行」就靠这张表来消。

## 本次交付范围（2026-10-28 决定）

**先不做签名、也没有干净机器可用，所以这一版是「内部使用包」，不是对外发布版。**

按本文件开头的规矩：

> 任何一条阻断项未勾选，不得对外发布。

现在 **A5、B2、B22–B24、E 全组、F 全组**都没勾。据此：

| 能做 | 不能做 |
|---|---|
| 在自己/团队的 Windows 机器上安装、启动、日常使用 | **对外分发**（未签名 → SmartScreen 拦「未知发布者」；也没验过干净机器） |
| 用 `tools\release\build-windows.ps1` 随时重新出包 | 声称「支持 Windows 发布」或写进对外文档的「已发布」栏 |

将来要对外发布，缺的仍是那三件非代码的事：**代码签名证书**、**一台干净 Windows**、**E 组功能冒烟**。
拿到其中任何一件，就在上面结果表里补一行新的，不要改旧行 —— 旧行记的是当时的事实。

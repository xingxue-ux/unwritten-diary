# 发布记录 · Windows 0.1.0

- 发布日期：2026-11-15
- 提交：`94ba27ce69220c2de73eead7cf17ea673def22ba`（`origin/main`）
- 标签：`windows-0.1.0`
- 发布页：https://github.com/xingxue-ux/unwritten-diary/releases/tag/windows-0.1.0
- 版本号：`apps/diary_app/pubspec.yaml` 的 `0.1.0+1`（exe 的 FileVersion 同值）

## 产物

| 文件 | 字节 | SHA-256 |
|---|---|---|
| `unwritten-diary-0.1.0-windows-x64-setup.exe` | 286,188,917 | `4bef0640946e4ac2454696c4488a07bd48babf50cd8c97029d5327da042c4f67` |
| `unwritten-diary-0.1.0-windows-x64.zip` | 211,928,187 | `fcb5504aa4f4de2ee47c21aeb97b50ea04905659f4d7cc2d2d328fce0607c17a` |

GitHub 发布页上的 `digest` 字段与上表一致（服务端复核过上传内容）。

构建命令（在 `origin/main` 的干净检出上执行）：

```powershell
$env:RUSTUP_TOOLCHAIN='stable-x86_64-pc-windows-msvc'
.\tools\release\build-windows.ps1 -Installer
```

## 检查清单状态

对照 [发布检查清单](发布检查清单.md)。**未勾选的是本次确实没做的，不是通过。**

### A. 版本与冻结

- [x] A1. 版本号 `0.1.0+1`，与产物 FileVersion 一致
- [ ] A2. `.metadata` 的 Flutter revision 与实际 `flutter --version` 仍不一致（沿用既有状态，未拍板）
- [x] A3. Rust workspace `rust-version` 满足；`Cargo.lock` 已提交
- [x] A4. `apps/diary_app/pubspec.lock` 已提交
- [x] A5–A7. applicationId / `Runner.rc` 的 CompanyName=ProductName / bundleName 已冻结
- [ ] A8. 数据目录迁移策略未变更（发布前改路径 = 清库）
- [x] A9. 从最新 `origin/main` 干净检出（`git status` 为空）构建
- [x] A10. 已打 tag `windows-0.1.0` 指向 `94ba27c`

### B. 构建与签名（Windows）

- [x] B15. `build-windows.ps1` 前置检查通过（Windows PowerShell、VS C++、MSVC target）
- [x] B16. `cargo build --release --target x86_64-pc-windows-msvc -p diary_bridge` 成功
- [x] B17. `flutter build windows --release` 成功
- [x] B18. `unwritten_diary.exe` 与 `diary_bridge.dll` 同目录
- [x] B19. `flutter_windows.dll`、`data\icudtl.dat`、`data\app.so`、`data\flutter_assets\`、`models\` 齐全
- [ ] B20. **未做**：干净 Windows 10/11 机器上的解压即启动
- [x] B21. 安装器可安装（本机静默安装 + 启动验证通过）；卸载与覆盖升级未单独验
- [ ] B22. **未做**：Authenticode 签名（没有证书）。`Get-AuthenticodeSignature` = `NotSigned`
- [ ] B23. **未做**：另一台机器首次下载的 SmartScreen 行为记录
- [ ] B24. **未做**：主流杀软对签名包的阻断检查

## 本次实际验证

| 项目 | 结果 |
|---|---|
| 从 `origin/main` 干净检出构建 | 通过（`git status` 空、HEAD = `94ba27c`） |
| 产物结构 | exe + dll + data + models + legal 齐全 |
| 安装（`/SILENT`） | 通过，装到 `%LOCALAPPDATA%\Programs\UnwrittenDiary` |
| 安装后启动 | 通过，窗口标题「不写日记」 |
| 安装目录关键文件 | `diary_bridge.dll`、`models\asr\sense-voice-int8\model.int8.onnx` 存在 |
| 上传后 GitHub digest | 与本地 SHA-256 一致 |
| 下载链接 | `HTTP 302 → 200`，可下载 |

## 明确的风险

1. **没有代码签名**：SmartScreen 会提示「已保护你的电脑」，需要「更多信息 → 仍要运行」。发布页已写明。
2. **没有干净机器验收**：只在开发机上装过；缺 VC++ 运行库的机器是否缺 DLL 未验。
3. **没有覆盖升级验收**：从旧版安装升级的路径未单独测。
4. 真实麦克风录音、长录音、ASR 质量、大资料库性能不在本次验收范围。

## 回滚

产物与源码都可追溯：删除 tag 与 release 即可下架；已安装用户的数据在
`%APPDATA%\Lingxing\不写日记\unwritten_diary\`，卸载不删除。

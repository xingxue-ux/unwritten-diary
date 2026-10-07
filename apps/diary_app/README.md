# 不写日记 · Flutter 交互演示

此工程按仓库 `docs/development/` 的产品计划展示完整的信息架构。启动后直接进入可输入的记录页；奶油白、直角手绘黑框与线条小猫适配 Windows 宽窗口和 Android 窄屏。封面可从左上角图标进入。小猫加载动画只在打开资料库时显示；“我的 → 封面与加载动画”也可单独预览。

## 运行

```bash
cd apps/diary_app
flutter pub get
flutter run -d windows
# 配好 Android SDK 和设备后：flutter run -d <device-id>
```

`DiaryApp(api: ..., demoMode: ...)` 显式注入 `DiaryApi` 与演示标记。目前使用 `MockDiaryApi`，`demoMode: true`。文字草稿与提交通过该接口完成，自动保存约 450 毫秒防抖；桌面 Ctrl+Enter 提交、Enter 换行。Mock 只在内存中保存，关闭应用后消失，不能用来验证真实持久化。顶部常驻“演示模式”提示，示例卡片与本次会话文字分别标明。标题使用开源霞鹜文楷 GB 的界面文案子集，正文保留易读的系统字体；字体来源与授权见 `assets/fonts/OFL.txt` 及[原项目](https://github.com/lxgw/LxgwWenkaiGB)。

## 可体验的页面

- **记录**：文字输入、保存状态、日期、附件卡片预览、录音状态流程预览。附件和录音按钮不会访问设备。
- **日记**：自然段阅读、来源入口、候选版本、编辑与主动回应的交互预览。
- **搜索**：示例资料与本次会话文字的关键词匹配、材料类型筛选及详情跳转；不假装已经做了语义检索。
- **片段**：示例原件与本次会话提交的文字记录。
- **我的**：风格、模型、整理时机、任务、插件、备份与恢复、存储和回收站的界面流程。风格与整理时机设置只在当前界面预览，不会保存或启动整理。

日记、来源、任务、回应、插件、备份等页面使用明确标记的示例状态；它们目前没有接入真实本地核心、文件库、模型服务或平台录音能力。密钥框在演示中禁用，不收集或保存密钥。真实功能按 F1–F8 与 B1–B7 的接口和任务推进，不能用本演示替代发布验收。

## 检查

```bash
cd packages/diary_mock && dart analyze && dart test
cd ../../apps/diary_app && flutter analyze && flutter test
```

评审曾在 Android API 36 x86_64 模拟器验证上一版 APK 的安装、启动、输入与演示保存。本次字体和文案更新已通过 Flutter 测试及本地 Web 预览构建；更新后的 Android APK、Windows 构建、Android arm64 真机、深色模式与真实核心功能仍待各自复验。

# 不写日记 · Flutter 客户端

正常启动使用 `BridgeDiaryApi` 连接本机 Rust 核心。资料库位于系统的应用支持目录下的 `unwritten_diary/library.sqlite`；重启后会从核心读回最近的文字记录和未完成草稿。页面测试仍可显式注入 `DiaryApi`，演示数据只在 `demoMode: true` 中显示。

## 本地运行

先运行 `flutter pub get`。本机需要准备与目标平台匹配的 Rust 桥接产物：

- Windows x64：先构建 `target/x86_64-pc-windows-msvc/release/diary_bridge.dll`，再运行 `flutter run -d windows` 或 `flutter build windows`。CMake 会把 DLL 放到程序旁；路径不同可传 `-DDIARY_BRIDGE_DLL=<绝对路径>`。
- Android arm64：先用 cargo-ndk 生成 `target/android-jniLibs/arm64-v8a/libdiary_bridge.so`。Gradle 从该目录打包 JNI 库；Release 构建缺少它时会失败。模拟器需要其自身 ABI 的 `.so`。

打开资料库失败时，应用显示错误和重试入口，不会自动改用内存 Mock。Windows 缺少 DLL 时会显示明确的安装错误。

## 当前实际接线

文字草稿、保存、提交和最近记录走真实核心；保存成功提示只在收到 `durable` 确认后出现。输入停止约 450 毫秒后自动保存，离开输入框、切页或进入后台时立即尝试保存。`片段`显示从本地资料库读回的最近记录。

录音、附件、完整搜索、日记生成、模型设置、插件和备份仍等待相应平台层及核心接口接入。正常模式不会创建模拟附件、模拟录音或显示示例日记；这些流程仍可在显式注入 Mock 的演示模式中预览。当前构建不能作为完整产品发布。

## 检查

```bash
cd apps/diary_app
flutter analyze --fatal-infos
flutter test
```

Android 真机持久化、Windows 安装包与原生库打包还需在相应设备上验收；组件测试不能替代这些检查。

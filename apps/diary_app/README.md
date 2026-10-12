# 不写日记 · Flutter 客户端

正常启动使用 `BridgeDiaryApi` 连接本机 Rust 核心。资料库的稳定路径是 `getApplicationSupportDirectory()/unwritten_diary/library.sqlite`，供后续备份与找回资料定位；当前尚无备份入口，运行中不要直接复制数据库文件。重启后会从核心读回最近的文字记录和未完成草稿。页面测试可显式注入 `DiaryApi`；预演模式必须显式开启。

## 本地运行

在仓库根目录先构建当前提交的 Rust 桥接产物，再运行 Flutter。**不要混用不同提交的 `.dll` / `.so` 与 Dart 绑定**：flutter_rust_bridge 会因内容哈希不一致拒绝启动，开发日志会出现 `Content hash on Dart side ... is different from Rust side ...`。本仓库的 `diary_bridge` 没有额外的 Cargo feature 开关。

Windows x64（仓库根目录运行第一行）：

```powershell
cargo build --release --target x86_64-pc-windows-msvc -p diary_bridge
cd apps/diary_app
flutter pub get
flutter run -d windows
```

生成的 DLL 位于 `target/x86_64-pc-windows-msvc/release/diary_bridge.dll`。CMake 将它安装到 `${CMAKE_INSTALL_PREFIX}`，与 exe 同级；当前模板的 `INSTALL_BUNDLE_LIB_DIR` 也等于 `${CMAKE_INSTALL_PREFIX}`。自定义产物路径可向 CMake 传 `-DDIARY_BRIDGE_DLL=<绝对路径>`，但仍必须与 Dart 绑定来自同一提交。

Android arm64（仓库根目录运行前两行，需 Android SDK、NDK 和 cargo-ndk）：

```bash
cargo ndk -t arm64-v8a -o target/android-jniLibs build --release -p diary_bridge
cd apps/diary_app
flutter pub get
flutter run -d <arm64-设备编号>
```

`target/android-jniLibs/arm64-v8a/libdiary_bridge.so` 会被 Gradle 打进安装包。x86_64 模拟器需先以 `cargo ndk -t x86_64 -o target/android-jniLibs build --release -p diary_bridge` 构建对应 ABI，然后运行 Flutter；arm64 产物不能替代 x86_64。Release 构建缺少 arm64 库时会失败。

已备好目标平台的原生构建环境时，可显式预览模拟内容：

```bash
flutter run -d <设备编号> --dart-define=DIARY_DEMO_MODE=true
```

预演模式注入内存 `MockDiaryApi`，不写真实资料库；不带此参数时只连接真实核心。`utcOffsetMinutes` 是启动时读取的新记录默认偏移；每条记录由核心保存自己的时区和偏移。

打开资料库失败时，应用显示错误和可重试时的重试入口，不会自动改用内存 Mock。Windows 缺少 DLL 时会显示明确的安装错误。开发构建在日志中打印底层异常类型与堆栈，供排查桥接哈希或资料库错误；正式构建不向用户展示堆栈。

## 当前实际接线

文字草稿、保存、提交和最近记录走真实核心；保存成功提示只在收到 `durable` 确认后出现。点「此刻的记录」下的输入区输入文字，停顿约 450 毫秒后自动保存；点「收好这一刻」提交，随后在「片段」查看。离开输入框、切页或进入后台时也立即尝试保存。清空已有草稿会保存空正文，但空正文不能提交。遇到版本、幂等或记录状态冲突时，界面保留文字并提供「复制文字」，停止重复旧写入；复制后重新打开应用以读取资料库当前状态。

录音、附件、完整搜索、日记生成、模型设置、插件和备份仍等待相应平台层及核心接口接入。正常模式不会创建模拟附件、模拟录音或显示示例日记；这些流程仍可在显式注入 Mock 的演示模式中预览。当前构建不能作为完整产品发布。

## 检查

```bash
cd apps/diary_app
flutter analyze --fatal-infos
flutter test
```

Android arm64 真机持久化、Windows 安装包与原生库打包还需在相应设备上验收；组件测试不能替代这些检查。

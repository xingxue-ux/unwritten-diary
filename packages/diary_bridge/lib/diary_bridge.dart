/// diary_bridge 的公开入口：把生成的绑定暴露给上层。
///
/// 生成文件在 `src/rust/` 下，**禁止手改**。要改行为就改
/// `crates/diary_bridge/src/api.rs`，再运行 `flutter_rust_bridge_codegen generate`。
///
/// 用法：
/// ```dart
/// import 'package:diary_bridge/diary_bridge.dart';
///
/// await RustLib.init(externalLibrary: ExternalLibrary.open('/path/libdiary_bridge.so'));
/// final session = await BridgeSession.open(libraryPath: '/path/library.sqlite');
/// final info = await session.info();
/// ```
library;

export 'src/rust/api.dart';
// RustLib.init 与 BridgeError 的异常基类都在这里。
export 'src/rust/frb_generated.dart';
export 'src/rust/lib.dart';
// 核心的数据对象（Capture / Job / Coverage 等）。这个路径由 codegen 按 crate 名
// 生成；升级 flutter_rust_bridge 时如果目录变了，这里跟着改。
export 'src/rust/third_party/diary_core/model.dart';
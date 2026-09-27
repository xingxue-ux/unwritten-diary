import 'dart:io';

import 'package:diary_bridge/src/rust/api.dart' as rust;
import 'package:diary_bridge/src/rust/frb_generated.dart';
import 'package:flutter_rust_bridge/flutter_rust_bridge_for_generated.dart';
import 'package:test/test.dart';

/// 桥接最小闭环：Dart 真正调用到 Rust cdylib。
///
/// 运行前先构建宿主平台产物：
///   cargo build --release            （在仓库根目录）
///   dart test                        （在本包目录）
///
/// 也可以用 `DIARY_BRIDGE_LIB` 指定别的产物路径，例如 Android 上推上去的
/// `.so`，或 Windows 侧的 `diary_bridge.dll`。
void main() {
  setUpAll(() async {
    final path = File(
      Platform.environment['DIARY_BRIDGE_LIB'] ?? _defaultLibraryPath(),
    ).absolute.path;
    expect(File(path).existsSync(), isTrue,
        reason: '找不到桥接产物：$path。先在仓库根目录跑 cargo build --release');
    await RustLib.init(externalLibrary: ExternalLibrary.open(path));
  });

  test('Dart 能同步拿到 Rust 的计算结果', () async {
    expect(await rust.add(a: 2, b: 3), 5);
    expect(await rust.add(a: -7, b: 7), 0);
  });

  test('契约版本信息按字段映射过来', () async {
    final snapshot = await rust.open();

    expect(snapshot.coreInfo.apiVersion, '1.0');
    expect(snapshot.coreInfo.dataSchemaVersion, 0);
    expect(snapshot.coreInfo.libraryId, 'library-m0-probe');
    expect(snapshot.coreInfo.capabilities, contains('probe.echo'));
    expect(snapshot.recovery.recoveredDraftCount, 0);
    expect(snapshot.pendingJobCount, 0);
  });

  test('传入的库句柄被原样返回', () async {
    final snapshot = await rust.open(libraryHandle: 'library-42');
    expect(snapshot.coreInfo.libraryId, 'library-42');
  });

  test('同步与异步两条路径结果一致', () async {
    final fromOpen = await rust.open(libraryHandle: 'library-42');
    final fromAsync = await rust.openAsync(libraryHandle: 'library-42');
    final fromSnapshot = await rust.snapshot();

    expect(fromOpen.coreInfo.libraryId, fromAsync.coreInfo.libraryId);
    expect(fromOpen.coreInfo.apiVersion, fromAsync.coreInfo.apiVersion);
    expect(fromAsync.pendingJobCount, fromSnapshot.pendingJobCount);
  });

  test('中文与 emoji 往返不走样', () async {
    const text = '妈妈离职了，晚上又觉得还行 🙂';
    expect(await rust.echo(text: text), text);
    expect(await rust.echo(text: ''), '');
  });
}

String _defaultLibraryPath() {
  if (Platform.isLinux) {
    return '../../target/release/libdiary_bridge.so';
  }
  if (Platform.isWindows) {
    return r'..\..\target\release\diary_bridge.dll';
  }
  if (Platform.isMacOS) {
    return '../../target/release/libdiary_bridge.dylib';
  }
  throw UnsupportedError('未知平台：${Platform.operatingSystem}');
}
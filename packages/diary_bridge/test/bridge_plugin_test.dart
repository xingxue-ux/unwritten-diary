import 'dart:io';

import 'package:diary_api/diary_api.dart';
import 'package:diary_bridge/diary_bridge.dart' show BridgeDiaryApi;
import 'package:path/path.dart' as path;
import 'package:test/test.dart';

/// 契约的 `plugins.invoke` 是票据形状，核心的执行是同步的。
///
/// 这条测试走**真实 DLL**，钉住三件事：
/// 1. 能力清单里有 `plugins.invoke` / `plugins.run`（只改 Dart 不算接上）；
/// 2. 票据指向一条真实的 `plugin` 任务，**插件报失败时任务行也是失败**；
/// 3. 运行结果按 `jobId` 读得回来，失败原因说得清。
void main() {
  final libraryPath = Platform.environment['DIARY_BRIDGE_LIB'];
  final available = libraryPath != null && File(libraryPath).existsSync();
  // 用**代码插件**（Wasm 沙箱）做端到端：它不需要模板与 StyleProfile，
  // 激活点是 `export.start`，正好走「命令激活」这条路。
  final examplePackage = path.normalize(
    path.join(
      Directory.current.path,
      '..',
      '..',
      'plugins',
      'examples',
      'export-diary-markdown',
    ),
  );

  test('插件调用：票据指向真实任务，失败也如实报出、结果读得回来', () async {
    if (!available) {
      markTestSkipped('DIARY_BRIDGE_LIB 未指向可用的核心动态库');
      return;
    }
    final dir = Directory.systemTemp.createTempSync('bridge-plugin');
    addTearDown(() => dir.deleteSync(recursive: true));

    final api = BridgeDiaryApi(
      coreLibraryPath: libraryPath,
      libraryPath: path.join(dir.path, 'library.sqlite'),
      timeZone: 'Asia/Shanghai',
      utcOffsetMinutes: 480,
    );
    addTearDown(api.close);

    final snapshot = await api.open();
    expect(
      snapshot.coreInfo.capabilities,
      containsAll(['plugins.invoke', 'plugins.run']),
      reason: '能力清单是 Rust 与 Dart 的交集，两边都要宣告',
    );

    final report = await api.inspectPluginPackage(examplePackage);
    expect(report.issues, isEmpty);
    final installed = await api.installPlugin(
      ticket: examplePackage,
      grantedPermissions: report.requestedPermissions,
    );
    expect(installed.grantedPermissions, isNotEmpty, reason: '这个示例要读材料、写导出物');
    expect(
      installed.templates,
      isEmpty,
      reason: '代码插件没有模板；声明式插件才有',
    );

    // 这个示例只读**用户选定的**日记版本，而临时库里一条版本都没有——所以这次
    // 调用必然失败。这里要验的不是「导出内容对不对」，而是**票据与结果的接线**：
    // 任务真的存在、状态如实是失败、结果读得回来、错误原因说得清。
    final ticket = await api.invokePlugin(
      pluginId: 'example.export-diary-markdown',
      command: 'export.start',
    );
    expect(ticket.jobId, isNotEmpty);

    final job = await api.getJob(ticket.jobId);
    expect(job.kind, 'plugin');
    expect(
      job.state,
      JobState.failed,
      reason: '插件自己报了失败，任务行就不能写 succeeded',
    );

    final result = await api.pluginRun(ticket.jobId);
    expect(result, isNotNull);
    expect(result!.status, 'failed');
    expect(result.pluginId, 'example.export-diary-markdown');
    expect(result.activation, 'export.start');
    expect(
      result.errorMessage ?? result.errorCode,
      isNotNull,
      reason: '失败要说清楚为什么',
    );

    // 不存在的 id：返回 null，不报错。
    expect(await api.pluginRun('job_does_not_exist'), isNull);
  });
}

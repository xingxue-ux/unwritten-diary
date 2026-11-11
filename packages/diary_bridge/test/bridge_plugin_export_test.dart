import 'dart:convert';
import 'dart:io';

import 'package:diary_api/diary_api.dart';
import 'package:diary_bridge/diary_bridge.dart' show BridgeDiaryApi;
import 'package:path/path.dart' as path;
import 'package:test/test.dart';

/// 仓库自带的**代码型**导出插件（Wasm，`templates` 为空）要的是「用户选定的
/// 日记版本」：插件里 `run()` 返回 2 就是「没选日记」。
///
/// 这条测试把整条真实链路走一遍：本机 loopback 桩当 Provider → 生成一天日记 →
/// 装上示例插件 → 带 `versionIds` 调用 → 成功并产出文件。它钉住的是「界面能拿到
/// 一个真实版本 id 并传下去」这条接线，不是模型质量。
void main() {
  final libraryPath = Platform.environment['DIARY_BRIDGE_LIB'];
  final available = libraryPath != null && File(libraryPath).existsSync();

  test('代码型导出插件：带上真实日记版本才跑得通', () async {
    if (!available) {
      markTestSkipped('DIARY_BRIDGE_LIB 未指向可用的核心动态库');
      return;
    }
    // 本机 loopback 桩：只回一句固定正文，不出网卡。
    final server = await HttpServer.bind(InternetAddress.loopbackIPv4, 0);
    addTearDown(() => server.close(force: true));
    server.listen((request) async {
      await utf8.decoder.bind(request).join();
      request.response.headers.contentType = ContentType.json;
      request.response.write(
        jsonEncode({
          'id': 'chatcmpl-test',
          'object': 'chat.completion',
          'created': 1,
          'model': 'qwen-test',
          'choices': [
            {
              'index': 0,
              'message': {'role': 'assistant', 'content': '今天走了很多路。'},
              'finish_reason': 'stop',
            },
          ],
          'usage': {'prompt_tokens': 10, 'completion_tokens': 8, 'total_tokens': 18},
        }),
      );
      await request.response.close();
    });

    final dir = Directory.systemTemp.createTempSync('bridge-plugin-export');
    addTearDown(() => dir.deleteSync(recursive: true));
    final api = BridgeDiaryApi(
      coreLibraryPath: libraryPath,
      libraryPath: path.join(dir.path, 'library.sqlite'),
      timeZone: 'Asia/Shanghai',
      utcOffsetMinutes: 480,
    );
    addTearDown(api.close);
    await api.open();

    // 一条提交过的记录，作为生成的输入。
    final draft = await api.createDraft(operationId: 'op-create');
    await api.saveDraft(
      id: draft.id,
      text: '今天走了很多路，路过一家花店。',
      expectedRevision: draft.revision,
      operationId: 'op-save',
    );
    await api.commit(
      id: draft.id,
      expectedRevision: draft.revision + 1,
      operationId: 'op-commit',
    );

    // 本机服务 + 密钥，然后生成这一天。
    final provider = await api.saveProvider(
      Provider(
        id: '',
        label: '本机桩',
        adapterId: 'openai_compatible',
        endpoint: 'http://127.0.0.1:${server.port}/v1',
        model: 'qwen-test',
        capabilities: const {ProviderCapability.generateText},
        enabled: true,
        testStatus: ProviderTestStatus.untested,
      ),
    );
    await api.saveProviderCredential(providerId: provider.id, secret: 'sk-test');
    final dayKey = draft.dayKey;
    await api.generateDiary(dayKey: dayKey, operationId: 'op-generate');
    await api.runDueJobs(budgetMs: 30000, trigger: 'test');
    final day = await api.getDay(dayKey);
    expect(day.currentVersionId, isNotNull, reason: '生成之后应当有当前版本');

    // 装示例插件，按「界面能拿到的参数」调用。
    final package = path.normalize(
      path.join(
        Directory.current.path,
        '..',
        '..',
        'plugins',
        'examples',
        'export-diary-markdown',
      ),
    );
    final report = await api.inspectPluginPackage(package);
    final installed = await api.installPlugin(
      ticket: package,
      grantedPermissions: report.requestedPermissions,
    );
    expect(installed.templates, isEmpty, reason: '代码型插件没有模板');

    // 不带版本：插件自己报「没选日记」（返回 2），任务如实失败。
    final withoutVersion = await api.invokePlugin(
      pluginId: installed.id,
      command: 'export.start',
    );
    final failed = await api.pluginRun(withoutVersion.jobId);
    expect(failed, isNotNull);
    expect(failed!.status, 'failed');

    // 带上真实版本：成功，并产出文件。
    final withVersion = await api.invokePlugin(
      pluginId: installed.id,
      command: 'export.start',
      versionIds: [day.currentVersionId!],
    );
    final result = await api.pluginRun(withVersion.jobId);
    expect(result, isNotNull);
    expect(result!.status, 'succeeded', reason: result.errorMessage ?? '');
    expect(result.artifacts, isNotEmpty, reason: '导出插件应当产出文件');
  });
}

import 'dart:convert';
import 'dart:io';

import 'package:diary_api/diary_api.dart';
import 'package:diary_bridge/diary_bridge.dart' show BridgeDiaryApi;
import 'package:path/path.dart' as path;
import 'package:test/test.dart';

/// 界面上「生成 → 人工修订 → 保存新版 → 重启读回 → 再整理不覆盖人工版」这条链路。
///
/// 契约的 `saveDiaryEdit` 一直有，但客户端生产路径从没调用过（只有演示面板）。
/// 这条用真实 DLL + 本机 loopback 桩把整条链路走一遍，钉住三件事：
/// 1. 人工修订存成**新版本**，旧版仍能读回；
/// 2. 关掉再打开（重启）之后，当前正文还是人工那一版；
/// 3. 再整理一次只产生**候选**，不会把人工版挤掉。
void main() {
  final libraryPath = Platform.environment['DIARY_BRIDGE_LIB'];
  final available = libraryPath != null && File(libraryPath).existsSync();

  test('人工修订 → 重启读回 → 再整理不覆盖', () async {
    if (!available) {
      markTestSkipped('DIARY_BRIDGE_LIB 未指向可用的核心动态库');
      return;
    }
    final server = await HttpServer.bind(InternetAddress.loopbackIPv4, 0);
    addTearDown(() => server.close(force: true));
    var reply = 'AI 写的第一版。';
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
              'message': {'role': 'assistant', 'content': reply},
              'finish_reason': 'stop',
            },
          ],
          'usage': {'prompt_tokens': 10, 'completion_tokens': 8, 'total_tokens': 18},
        }),
      );
      await request.response.close();
    });

    final dir = Directory.systemTemp.createTempSync('bridge-edit-flow');
    addTearDown(() => dir.deleteSync(recursive: true));
    final libraryFile = path.join(dir.path, 'library.sqlite');

    BridgeDiaryApi open() => BridgeDiaryApi(
      coreLibraryPath: libraryPath,
      libraryPath: libraryFile,
      timeZone: 'Asia/Shanghai',
      utcOffsetMinutes: 480,
    );

    var api = open();
    await api.open();

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
    var day = await api.getDay(dayKey);
    final generatedId = day.currentVersionId;
    expect(generatedId, isNotNull);
    final generated = await api.getVersion(generatedId!);
    expect(generated.body, contains('AI 写的第一版'));

    // 人工修订：存成新版本。
    final edited = await api.saveDiaryEdit(
      baseVersionId: generatedId,
      body: '我自己改过的正文：花是橘色的。',
      expectedRevision: day.revision,
      operationId: 'op-edit',
    );
    expect(edited.origin, DiaryVersionOrigin.userEdit);
    day = await api.getDay(dayKey);
    expect(day.currentVersionId, edited.id, reason: '人工版成为当前');
    expect(
      day.candidateVersionIds,
      contains(generatedId),
      reason: '旧当前版退成候选，不消失',
    );
    expect((await api.getVersion(generatedId)).body, contains('AI 写的第一版'));

    // 重启：关掉再打开同一个库。
    await api.close();
    api = open();
    addTearDown(api.close);
    await api.open();
    day = await api.getDay(dayKey);
    expect(day.currentVersionId, edited.id, reason: '重启后当前正文还是人工那一版');
    expect((await api.getVersion(edited.id)).body, contains('花是橘色的'));
    expect((await api.getVersion(generatedId)).body, contains('AI 写的第一版'));

    // 再整理一次：只产生候选，不挤掉人工版。
    reply = 'AI 写的第二版。';
    await api.generateDiary(
      dayKey: dayKey,
      reason: DiaryGenerationReason.regenerate,
      operationId: 'op-regenerate',
    );
    await api.runDueJobs(budgetMs: 30000, trigger: 'test');
    day = await api.getDay(dayKey);
    expect(
      day.currentVersionId,
      edited.id,
      reason: '再整理不覆盖人工版——新正文只是候选',
    );
    final candidates = day.candidateVersionIds
        .map((id) async => (await api.getVersion(id)).body)
        .toList();
    final bodies = await Future.wait(candidates);
    expect(bodies.any((body) => body.contains('AI 写的第二版')), isTrue);
  });
}

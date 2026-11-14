import 'dart:convert';
import 'dart:io';

import 'package:crypto/crypto.dart';
import 'package:diary_api/diary_api.dart';
import 'package:diary_bridge/diary_bridge.dart' show BridgeDiaryApi;
import 'package:path/path.dart' as path;
import 'package:test/test.dart';

/// 阅读导出「附带原始材料」走真实 DLL 的端到端。
///
/// 核心单测已经验过 zip 里的内容；这条验的是**接线**：`includeAssets` 从契约
/// 一路传到核心，产物真的落在 exports 目录里，而且是一个 zip。
void main() {
  final libraryPath = Platform.environment['DIARY_BRIDGE_LIB'];
  final available = libraryPath != null && File(libraryPath).existsSync();

  test('附带原件导出得到 zip，正文与清单都在里面', () async {
    if (!available) {
      markTestSkipped('DIARY_BRIDGE_LIB 未指向可用的核心动态库');
      return;
    }
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
              'message': {'role': 'assistant', 'content': '今天的正文。'},
              'finish_reason': 'stop',
            },
          ],
          'usage': {'prompt_tokens': 5, 'completion_tokens': 5, 'total_tokens': 10},
        }),
      );
      await request.response.close();
    });

    final dir = Directory.systemTemp.createTempSync('bridge-export-assets');
    addTearDown(() => dir.deleteSync(recursive: true));
    final api = BridgeDiaryApi(
      coreLibraryPath: libraryPath,
      libraryPath: path.join(dir.path, 'library.sqlite'),
      timeZone: 'Asia/Shanghai',
      utcOffsetMinutes: 480,
    );
    addTearDown(api.close);
    await api.open();

    final draft = await api.createDraft(operationId: 'op-create');
    await api.saveDraft(
      id: draft.id,
      text: '今天去了公园。',
      expectedRevision: draft.revision,
      operationId: 'op-save',
    );
    await api.commit(
      id: draft.id,
      expectedRevision: draft.revision + 1,
      operationId: 'op-commit',
    );
    final dayKey = draft.dayKey;

    // 挂一份原件（走真实的导入流程）。
    final bytes = utf8.encode('原始材料：公园里的白鹭。');
    final ticket = await api.prepareImport(
      captureId: draft.id,
      displayName: '原件.txt',
      mimeHint: 'text/plain',
      sizeHint: bytes.length,
      operationId: 'op-import',
    );
    await File(ticket.stagingTicket).writeAsBytes(bytes);
    await api.finishImport(
      importId: ticket.importId,
      ticket: ticket.stagingTicket,
      manifest: ImportManifest(
        copiedBytes: bytes.length,
        sha256: sha256Of(bytes),
        detectedMime: 'text/plain',
        originalName: '原件.txt',
      ),
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
    await api.generateDiary(dayKey: dayKey, operationId: 'op-generate');
    await api.runDueJobs(budgetMs: 30000, trigger: 'test');

    final bundle = await api.createExport(
      ExportRequest(
        fromDayKey: dayKey,
        toDayKey: dayKey,
        format: 'markdown',
        includeAssets: true,
      ),
    );
    final package = bundle.packageTicket;
    expect(package, isNotNull);
    expect(package, endsWith('.zip'));
    final file = File(package!);
    expect(file.existsSync(), isTrue);
    expect(file.lengthSync(), greaterThan(0));
    // zip 的本地文件头：PK\x03\x04。
    //
    // 用 `readAsBytes` 而不是 `openSync()`：后者返回的 `RandomAccessFile` 不关，
    // Windows 上临时目录就删不掉（`PathAccessException / errno 32`），
    // 整个桥接套件会在清理阶段挂掉。
    final head = (await file.readAsBytes()).take(4).toList();
    expect(head, [0x50, 0x4B, 0x03, 0x04]);
  });
}

/// 测试里算 sha256：只为了给导入清单一个自洽的值。
String sha256Of(List<int> bytes) => sha256.convert(bytes).toString();

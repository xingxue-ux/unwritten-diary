import 'dart:convert';
import 'dart:io';

import 'package:diary_api/diary_api.dart';
import 'package:diary_bridge/diary_bridge.dart' show BridgeDiaryApi;
import 'package:path/path.dart' as path;
import 'package:test/test.dart';

/// 改已有服务时**不能顺手把凭据清掉**。
///
/// 核心的语义是「`credentialRef` 为空 = 清掉凭据」。界面在「编辑已有服务、Key 留空、
/// 点获取模型列表」时如果重建配置漏掉原引用，用户只是列了个模型，密钥就没了。
/// 这条测试钉住两件事：
/// 1. 带上原引用重新保存 → 凭据还在，列模型仍然成功；
/// 2. 不带引用重新保存 → 凭据被清掉（核心的显式语义，界面要避开它）。
void main() {
  final libraryPath = Platform.environment['DIARY_BRIDGE_LIB'];
  final available = libraryPath != null && File(libraryPath).existsSync();

  test('改服务时保留 credentialRef，凭据与列模型都还在', () async {
    if (!available) {
      markTestSkipped('DIARY_BRIDGE_LIB 未指向可用的核心动态库');
      return;
    }
    // 本机 loopback 桩：`GET /v1/models` 返回两个模型。
    final server = await HttpServer.bind(InternetAddress.loopbackIPv4, 0);
    addTearDown(() => server.close(force: true));
    server.listen((request) async {
      await utf8.decoder.bind(request).join();
      request.response.headers.contentType = ContentType.json;
      request.response.write(
        jsonEncode({
          'object': 'list',
          'data': [
            {'id': 'model-a', 'object': 'model', 'owned_by': 'stub'},
            {'id': 'model-b', 'object': 'model', 'owned_by': 'stub'},
          ],
        }),
      );
      await request.response.close();
    });

    final dir = Directory.systemTemp.createTempSync('bridge-provider-cred');
    addTearDown(() => dir.deleteSync(recursive: true));
    final api = BridgeDiaryApi(
      coreLibraryPath: libraryPath,
      libraryPath: path.join(dir.path, 'library.sqlite'),
      timeZone: 'Asia/Shanghai',
      utcOffsetMinutes: 480,
    );
    addTearDown(api.close);
    await api.open();

    final endpoint = 'http://127.0.0.1:${server.port}/v1';
    final created = await api.saveProvider(
      Provider(
        id: '',
        label: '本机桩',
        adapterId: 'openai_compatible',
        endpoint: endpoint,
        model: 'model-a',
        capabilities: const {ProviderCapability.generateText},
        enabled: true,
        testStatus: ProviderTestStatus.untested,
      ),
    );
    final withSecret = await api.saveProviderCredential(
      providerId: created.id,
      secret: 'sk-test-credential',
    );
    expect(withSecret.credentialRef, isNotNull);

    // 界面「编辑后列模型」的等价调用：字段不变、Key 留空、**带上原引用**。
    final updated = await api.saveProvider(
      Provider(
        id: created.id,
        label: '本机桩（改名）',
        adapterId: 'openai_compatible',
        endpoint: endpoint,
        model: 'model-a',
        capabilities: const {ProviderCapability.generateText},
        enabled: true,
        credentialRef: withSecret.credentialRef,
      ),
    );
    expect(
      updated.credentialRef,
      withSecret.credentialRef,
      reason: '改显示名不该动凭据引用',
    );

    final models = await api.listProviderModels(providerId: created.id);
    expect(models.map((item) => item.id), containsAll(['model-a', 'model-b']));

    // 反面对照：不带引用重新保存 = 核心的「清掉凭据」。
    final cleared = await api.saveProvider(
      Provider(
        id: created.id,
        label: '本机桩',
        adapterId: 'openai_compatible',
        endpoint: endpoint,
        model: 'model-a',
        capabilities: const {ProviderCapability.generateText},
        enabled: true,
      ),
    );
    expect(
      cleared.credentialRef,
      isNull,
      reason: '核心把空引用解释成清掉凭据——界面必须避开这条路径',
    );
  });
}

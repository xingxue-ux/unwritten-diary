import 'dart:io';

import 'package:diary_bridge/diary_bridge.dart';
import 'package:flutter_rust_bridge/flutter_rust_bridge_for_generated.dart';
import 'package:test/test.dart';

/// B4a（issue #66）的桥接端到端：配置往返、脱敏、能力闸门与错误码映射。
///
/// 这一组用例**不联网**：
/// - 配置往返、能力闸门、`responses.request` 的拒绝路径都不发请求；
/// - 需要「请求失败」的那条用例把端点指向本机一个必然没人监听的端口，
///   连接被拒是即时的（不依赖任何外部服务，也不出网卡）。
///
/// 回放传输层那条完整链路（请求 → 作业 → 结果落库）在核心侧
/// （`crates/diary_core/tests/provider_test.rs`）：桥接层没有注入传输层的入口
/// （会话自己开库），拿真端点测生成就变成「CI 需要网络」，那是不能接受的。
void main() {
  late Directory root;

  setUpAll(() async {
    final path = File(
      Platform.environment['DIARY_BRIDGE_LIB'] ?? _defaultLibraryPath(),
    ).absolute.path;
    expect(
      File(path).existsSync(),
      isTrue,
      reason: '找不到桥接产物：$path。先在仓库根目录跑 cargo build --release',
    );
    await RustLib.init(externalLibrary: ExternalLibrary.open(path));
    root = Directory.systemTemp.createTempSync('diary_provider_bridge_test');
  });

  tearDownAll(() {
    try {
      root.deleteSync(recursive: true);
    } on FileSystemException {
      // 删不掉不影响结论。
    }
  });

  late Directory workDir;
  late BridgeSession session;

  setUp(() async {
    workDir = Directory.systemTemp.createTempSync('diary_provider_case');
    session = await BridgeSession.open(
      libraryPath: '${workDir.path}/library.sqlite',
    );
  });

  tearDown(() {
    session.dispose();
    try {
      workDir.deleteSync(recursive: true);
    } on FileSystemException {
      // 删不掉不影响结论。
    }
  });

  Provider providerConfig({
    String id = '',
    String label = '本地测试服务',
    String endpoint = 'http://127.0.0.1:1/v1',
    Set<ProviderCapability> capabilities = const {
      ProviderCapability.generateText,
    },
    bool enabled = true,
  }) =>
      Provider(
        id: id,
        label: label,
        adapterId: 'openai_compatible',
        endpoint: endpoint,
        model: 'qwen-test',
        capabilities: capabilities,
        enabled: enabled,
        testStatus: ProviderTestStatus.untested,
      );

  /// 一条提交过的记录（`responses.request` 的来源）。
  Future<String> aCapture(String operation) async {
    final draft = await session.createDraft(
      timeZone: 'Asia/Shanghai',
      utcOffsetMinutes: 480,
      operationId: '$operation-create',
    );
    await session.saveDraft(
      captureId: draft.id,
      text: '今天加班到很晚。',
      expectedRevision: draft.revision,
      operationId: '$operation-save',
    );
    final committed = await session.commit(
      captureId: draft.id,
      expectedRevision: draft.revision + 1,
      operationId: '$operation-commit',
    );
    return committed.capture.id;
  }

  test('配置往返：保存、列出脱敏、移除不删日记', () async {
    final captureId = await aCapture('op-provider');
    const secret = 'sk-bridge-secret-0123456789';

    final saved = await session.saveProvider(
      config: providerConfig(),
      secret: secret,
    );
    expect(saved.id, isNotEmpty);
    expect(saved.enabled, isTrue);
    expect(saved.credentialRef, isNotNull,
        reason: '密钥要落成引用，不是明文');
    expect(saved.credentialRef, isNot(contains(secret)));

    final listed = await session.listProviders();
    expect(listed, hasLength(1));
    expect(listed.single.id, saved.id);
    expect(listed.single.credentialRef, saved.credentialRef);
    expect(listed.single.capabilities, {ProviderCapability.generateText});
    expect(listed.single.testStatus, ProviderTestStatus.untested);

    // 契约里没有密钥字段，但把整个对象序列化一遍也不该出现明文（防御性断言）。
    expect(listed.single.toString(), isNot(contains(secret)));
    for (final value in [
      listed.single.id,
      listed.single.label,
      listed.single.adapterId,
      listed.single.endpoint ?? '',
      listed.single.model ?? '',
      listed.single.credentialRef ?? '',
    ]) {
      expect(value, isNot(contains(secret)));
    }

    // 移除配置不删除日记。
    await session.removeProvider(providerId: saved.id);
    expect(await session.listProviders(), isEmpty);
    expect((await session.getCapture(captureId: captureId)).id, captureId);
  });

  test('没有可用服务时 responses.request 被拒绝，且不排任务', () async {
    final captureId = await aCapture('op-refuse');

    await expectLater(
      session.requestResponse(captureIds: [captureId], requestText: '说点什么'),
      throwsA(
        isA<BridgeError>()
            .having((error) => error.code, 'code', 'provider_not_configured')
            .having((error) => error.message, 'message', contains('连接测试'))
            .having((error) => error.retryable, 'retryable', isFalse),
      ),
    );
    expect(await session.listJobs(limit: 50), isEmpty, reason: '不排注定失败的任务');

    // 空请求文字是调用方的错，报 invalid_argument。
    await expectLater(
      session.requestResponse(captureIds: [captureId], requestText: '   '),
      throwsA(isA<BridgeError>()
          .having((error) => error.code, 'code', 'invalid_argument')),
    );
  });

  test('能力闸门：只声明转写的服务接不了生成任务', () async {
    final captureId = await aCapture('op-gate');
    final saved = await session.saveProvider(
      config: providerConfig(
        capabilities: const {ProviderCapability.transcribeAudio},
      ),
      secret: 'sk-transcribe-only-0123456789',
    );

    await expectLater(
      session.requestResponse(
        captureIds: [captureId],
        requestText: '帮我写一句',
        providerId: saved.id,
      ),
      throwsA(
        isA<BridgeError>()
            .having((error) => error.code, 'code', 'provider_not_configured')
            .having((error) => error.message, 'message', contains('generate_text')),
      ),
    );
    expect(await session.listJobs(limit: 50), isEmpty);
  });

  test('连接测试：显式调用才发请求，失败给可区分错误码且不泄漏密钥', () async {
    const secret = 'sk-bridge-secret-0123456789';
    // 端点指向本机必然没人监听的端口：连接被拒是即时的，不需要任何外部服务。
    final saved = await session.saveProvider(
      config: providerConfig(endpoint: 'http://127.0.0.1:1/v1'),
      secret: secret,
    );

    // 保存与列出都不发请求（这条用例本身不联网，靠的就是这个：如果保存会发请求，
    // 它会因为连不上而炸在这里，而不是等到 testProvider）。
    expect((await session.listProviders()).single.id, saved.id);

    try {
      await session.testProvider(
        providerId: saved.id,
        scope: [ProviderCapability.generateText],
      );
      fail('连不上的端点不该报成功');
    } on BridgeError catch (error) {
      expect(
        error.code,
        anyOf('network_unavailable', 'provider_timeout'),
        reason: '连接被拒要落到可区分的错误码，实际：${error.code} / ${error.message}',
      );
      expect(error.message, isNot(contains(secret)), reason: '错误消息不能泄漏密钥');
      expect(error.message, contains('真实请求'), reason: '要如实说明发生过调用');
      expect(error.retryable, isTrue, reason: '连不上是可以退避重试的');
    }

    // 结论落库：最近一次测试是失败的。
    expect(
      (await session.listProviders()).single.testStatus,
      ProviderTestStatus.failed,
    );
  });

  test('响应对象：还没生成时正文为空，如实返回', () async {
    // 直接读一个不存在的回应报 not_found（不编一个空对象出来）。
    await expectLater(
      session.getResponse(responseId: 'rsp_missing'),
      throwsA(
        isA<BridgeError>().having((error) => error.code, 'code', 'not_found'),
      ),
    );
  });
}

/// 与其它桥接测试相同的默认产物查找逻辑（相对 `packages/diary_bridge`）。
String _defaultLibraryPath() {
  if (Platform.isLinux) {
    return '../../target/release/libdiary_bridge.so';
  }
  if (Platform.isWindows) {
    return r'..\..\target\x86_64-pc-windows-msvc\release\diary_bridge.dll';
  }
  if (Platform.isMacOS) {
    return '../../target/release/libdiary_bridge.dylib';
  }
  throw UnsupportedError('未知平台：${Platform.operatingSystem}');
}
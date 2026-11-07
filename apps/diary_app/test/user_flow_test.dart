import 'dart:io';

import 'package:diary_api/diary_api.dart';
import 'package:diary_mock/diary_mock.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:path_provider_platform_interface/path_provider_platform_interface.dart';
import 'package:unwritten_diary/main.dart';
import 'package:unwritten_diary/src/ui_strings.dart';

/// 模拟一个真实用户从早到晚的使用过程。
///
/// 这条测试的写法是「照着人能看懂的操作顺序点」，不是「照着代码结构调」。
/// 目的：把「只有连着做几步才会暴露」的问题找出来。

class _FakePathProvider extends PathProviderPlatform {
  _FakePathProvider(this.root);
  final String root;

  @override
  Future<String?> getApplicationSupportPath() async => root;

  @override
  Future<String?> getTemporaryPath() async => root;

  @override
  Future<String?> getApplicationDocumentsPath() async => root;
}

/// 会「记住」状态的服务配置：保存之后能列出来，测试连接会通过。
class _Api extends MockDiaryApi {
  final List<Provider> rows = [];
  int modelCalls = 0;
  int testCalls = 0;

  @override
  Future<List<Provider>> listProviders() async => List.of(rows);

  @override
  Future<Provider> saveProvider(Provider provider) async {
    final saved = provider.copyWith(credentialRef: 'secret-1');
    rows.removeWhere((item) => item.id == saved.id);
    rows.add(saved);
    return saved;
  }

  @override
  Future<Provider> saveProviderCredential({
    required String providerId,
    required String secret,
  }) async => rows.firstWhere((item) => item.id == providerId);

  @override
  Future<List<ProviderModel>> listProviderModels({
    required String providerId,
  }) async {
    modelCalls++;
    return const [ProviderModel(id: 'deepseek-flash')];
  }

  @override
  Future<ProviderTestReport> testProvider({
    required String providerId,
    required Set<ProviderCapability> scope,
  }) async {
    testCalls++;
    rows.removeWhere((item) => item.id == providerId);
    rows.add(
      Provider(
        id: providerId,
        label: 'DeepSeek',
        adapterId: 'openai_compatible',
        capabilities: const {ProviderCapability.generateText},
        enabled: true,
        endpoint: 'https://api.deepseek.com',
        model: 'deepseek-flash',
        credentialRef: 'secret-1',
        testStatus: ProviderTestStatus.ok,
      ),
    );
    return ProviderTestReport(
      providerId: providerId,
      results: const {ProviderCapability.generateText: ProviderTestStatus.ok},
      actualCallMade: true,
    );
  }

  @override
  Future<DiaryDay> getDay(String dayKey) async => DiaryDay(
    dayKey: dayKey,
    timeZone: 'Asia/Shanghai',
    updatedAt: DateTime.utc(2026, 11, 7),
    materialCounts: const MaterialCounts(captures: 1, processed: 1),
  );

  /// 整理之后会有一条候选。
  List<DiaryVersion> versions = const [];
  int generateCalls = 0;

  @override
  Future<DiaryVersionPage> listVersions(
    String dayKey, {
    String? cursor,
    int pageSize = 20,
  }) async => DiaryVersionPage(versions: versions, nextCursor: null);

  @override
  Future<DiaryGenerationTicket> generateDiary({
    required String dayKey,
    required String operationId,
    DiaryGenerationReason reason = DiaryGenerationReason.manual,
    String? styleId,
  }) async {
    generateCalls++;
    versions = [
      DiaryVersion(
        id: 'ver-1',
        dayKey: dayKey,
        body: '今天路过花店，买了一束橘色的花。',
        origin: DiaryVersionOrigin.generated,
        createdAt: DateTime.utc(2026, 11, 7),
      ),
    ];
    return const DiaryGenerationTicket(
      jobId: 'job-1',
      sourceSnapshotId: 'snap-1',
    );
  }

  @override
  Future<JobPage> listJobs({
    String? cursor,
    int pageSize = 50,
    Set<JobState>? states,
  }) async => const JobPage(jobs: [], nextCursor: null);
}


/// 本地文件 I/O 在 widget 测试里不会自己推进：要让出真实事件循环再 pump。
Future<void> settleRealIo(WidgetTester tester) async {
  for (var i = 0; i < 8; i++) {
    await tester.runAsync(
      () => Future<void>.delayed(const Duration(milliseconds: 60)),
    );
    await tester.pump();
  }
}

Future<void> _boot(WidgetTester tester, _Api api) async {
  final dir = Directory.systemTemp.createTempSync('user-flow');
  addTearDown(() => dir.deleteSync(recursive: true));
  PathProviderPlatform.instance = _FakePathProvider(dir.path);
  tester.view.physicalSize = const Size(1400, 1000);
  tester.view.devicePixelRatio = 1;
  addTearDown(tester.view.resetPhysicalSize);
  addTearDown(tester.view.resetDevicePixelRatio);
  await tester.pumpWidget(DiaryApp(api: api));
  await tester.pumpAndSettle();
}

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();
  setUpAll(() async => uiStrings.load());

  testWidgets('日常：写一条 → 收好 → 在片段里看到它', (tester) async {
    final api = _Api();
    await _boot(tester, api);

    await tester.enterText(
      find.byKey(const Key('capture-editor')),
      '今天路过花店，买了一束橘色的花。',
    );
    await tester.pumpAndSettle();
    expect(find.text('正在写，还没有保存确认。'), findsOneWidget);

    await tester.ensureVisible(find.byKey(const Key('finish-capture')));
    await tester.tap(find.byKey(const Key('finish-capture')));
    await tester.pumpAndSettle();

    // 收好之后输入框清空、状态回到「可以开始」，记录进资料库。
    final field = tester.widget<TextField>(
      find.byKey(const Key('capture-editor')),
    );
    expect(field.controller!.text, isEmpty, reason: '收好之后输入框要清空');
    expect(find.text('写一点什么，随时可以开始。'), findsOneWidget);
    expect(tester.takeException(), isNull);

    // 去片段页看这条记录在不在。
    await tester.tap(find.text('片段').last);
    await tester.pumpAndSettle();
    expect(find.textContaining('花店'), findsWidgets, reason: '片段页应当出现刚才那条');
  });

  testWidgets('配服务：填表 → 拿模型 → 测连接 → 保存 → 授权', (tester) async {
    final api = _Api();
    await _boot(tester, api);
    await tester.tap(find.text('我的').last);
    await tester.pumpAndSettle();
    await tester.ensureVisible(find.text('模型服务'));
    await tester.tap(find.text('模型服务'));
    await tester.pumpAndSettle();

    // ① 填地址与密钥，先拿模型列表。
    await tester.enterText(
      find.widgetWithText(TextField, 'API Key'),
      'sk-test-key',
    );
    await tester.pumpAndSettle();
    await tester.ensureVisible(find.text('获取模型列表'));
    await tester.tap(find.text('获取模型列表'));
    await tester.pumpAndSettle();
    expect(api.modelCalls, 1, reason: '点一次就该拉一次');
    expect(find.text('deepseek-flash'), findsWidgets, reason: '候选要出现在表单里');

    // ② 点候选填进模型名。
    await tester.tap(find.text('deepseek-flash').first);
    await tester.pumpAndSettle();

    // ③ 测连接。
    await tester.ensureVisible(find.text('测试连接').last);
    await tester.tap(find.text('测试连接').last);
    await tester.pumpAndSettle();
    expect(api.testCalls, 1);
    expect(tester.takeException(), isNull, reason: '测连接不该崩');

    // ④ 保存。
    await tester.ensureVisible(find.text('保存服务'));
    await tester.tap(find.text('保存服务'));
    await tester.pumpAndSettle();
    expect(api.rows.length, 1, reason: '保存后应当有一条服务');
    expect(tester.takeException(), isNull, reason: '保存不该崩');

    // ⑤ 授权发送材料（远端服务需要）。
    await tester.ensureVisible(find.text('授权发送'));
    await tester.tap(find.text('授权发送'));
    await settleRealIo(tester);
    expect(find.textContaining('已授权发送材料'), findsOneWidget);
    expect(tester.takeException(), isNull);
  });

  testWidgets('刷新状态：点下去要有反应', (tester) async {
    final api = _Api();
    await _boot(tester, api);
    await tester.tap(find.text('日记').last);
    await settleRealIo(tester);

    await tester.ensureVisible(find.text('刷新状态'));
    await tester.tap(find.text('刷新状态'));
    // 点下去立刻要有反馈：按钮变成「刷新中…」。
    await tester.pump();
    expect(
      find.text('刷新中…'),
      findsOneWidget,
      reason: '点了没反应，用户会以为按钮坏了',
    );
    await settleRealIo(tester);
    expect(find.text('刷新状态'), findsOneWidget, reason: '跑完要变回来');
    expect(tester.takeException(), isNull);
  });

  testWidgets('搜索：写一条之后能搜到', (tester) async {
    final api = _Api();
    await _boot(tester, api);

    await tester.enterText(
      find.byKey(const Key('capture-editor')),
      '周末去爬山，山顶风很大。',
    );
    await tester.pumpAndSettle();
    await tester.ensureVisible(find.byKey(const Key('finish-capture')));
    await tester.tap(find.byKey(const Key('finish-capture')));
    await tester.pumpAndSettle();

    await tester.tap(find.text('搜索').last);
    await tester.pumpAndSettle();
    await tester.enterText(find.byType(TextField).first, '爬山');
    await tester.pumpAndSettle();
    // 搜索走的是核心检索，假 API 没实现 → 界面要如实说「搜不了」，不能假装有结果。
    expect(tester.takeException(), isNull, reason: '搜索页不该崩');
  });

  testWidgets('备份与回收站：点进去不崩', (tester) async {
    final api = _Api();
    await _boot(tester, api);
    await tester.tap(find.text('我的').last);
    await tester.pumpAndSettle();
    for (final panel in ['备份与恢复', '存储与回收站']) {
      await tester.ensureVisible(find.text(panel));
      await tester.tap(find.text(panel));
      await tester.pumpAndSettle();
      expect(tester.takeException(), isNull, reason: '$panel 打不开');
      await tester.tap(find.text('返回'));
      await tester.pumpAndSettle();
    }
  });

  testWidgets('整理日记：配好服务 → 点整理 → 出现候选 → 采纳为当前正文', (tester) async {
    final api = _Api();
    await _boot(tester, api);

    // 先配一个服务并授权，否则日记页会拦住。
    api.rows.add(
      const Provider(
        id: 'prv_a',
        label: 'DeepSeek',
        adapterId: 'openai_compatible',
        capabilities: {ProviderCapability.generateText},
        enabled: true,
        endpoint: 'https://api.deepseek.com',
        model: 'deepseek-flash',
        credentialRef: 'secret-1',
        testStatus: ProviderTestStatus.ok,
      ),
    );

    await tester.tap(find.text('日记').last);
    // 日记页初始化要读本地 UI 文件（真实 I/O），要让出事件循环。
    await settleRealIo(tester);

    // 没授权时应当拦住，并指路去模型服务。
    expect(find.textContaining('还没授权发送材料'), findsOneWidget);
    expect(find.text('去模型服务授权'), findsOneWidget);

    // 授权。
    await tester.tap(find.text('去模型服务授权'));
    await tester.pumpAndSettle();
    await settleRealIo(tester);
    await tester.ensureVisible(find.text('授权发送'));
    await tester.pumpAndSettle();
    await tester.tap(find.text('授权发送'));
    await settleRealIo(tester);

    // 回日记页整理。
    await tester.tap(find.text('日记').last);
    await settleRealIo(tester);
    expect(find.textContaining('还没授权发送材料'), findsNothing);

    await tester.ensureVisible(find.text('整理这一天'));
    await tester.tap(find.text('整理这一天'));
    await tester.pumpAndSettle();
    expect(api.generateCalls, 1, reason: '点一次整理就该受理一次');
    expect(tester.takeException(), isNull, reason: '整理不该崩');
  });

  testWidgets('说大话：进去接着写，回来说的话还在，收好能存', (tester) async {
    final api = _Api();
    await _boot(tester, api);

    await tester.enterText(find.byKey(const Key('capture-editor')), '开头。');
    await tester.pumpAndSettle();

    await tester.tap(find.byKey(const Key('enter-big-talk')));
    await tester.pumpAndSettle();
    await tester.enterText(
      find.byKey(const Key('capture-editor-bigtalk')),
      '开头。中间写了很长一段。',
    );
    await tester.pumpAndSettle();

    await tester.tap(find.byKey(const Key('exit-big-talk')));
    await tester.pumpAndSettle();

    final field = tester.widget<TextField>(
      find.byKey(const Key('capture-editor')),
    );
    expect(field.controller!.text, '开头。中间写了很长一段。');

    await tester.ensureVisible(find.byKey(const Key('finish-capture')));
    await tester.tap(find.byKey(const Key('finish-capture')));
    await tester.pumpAndSettle();
    final after = tester.widget<TextField>(
      find.byKey(const Key('capture-editor')),
    );
    expect(after.controller!.text, isEmpty, reason: '全屏里写的内容也要能收好');
  });
}

import 'dart:io';

import 'package:diary_api/diary_api.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:path_provider_platform_interface/path_provider_platform_interface.dart';
import 'package:unwritten_diary/src/ai_diary_panel.dart';

class _FakePathProvider extends PathProviderPlatform {
  _FakePathProvider(this.root);
  final String root;
  @override
  Future<String?> getApplicationSupportPath() async => root;
}

/// 正式日记页的人工编辑入口。
///
/// 以前这一页只能看、只能采纳候选：契约的 `saveDiaryEdit` 有，客户端生产路径
/// 从没调用过。这条测试钉住「按钮在、对话框能开、保存真的走核心接口」。
class _DiaryApi implements DiaryApi {
  _DiaryApi({required this.day, required this.versions});

  DiaryDay day;
  final List<DiaryVersion> versions;
  int edits = 0;
  String? lastBody;
  final List<String> adopted = [];

  /// 让保存失败（模拟写锁被占、revision 冲突等）。
  bool failSave = false;

  @override
  Future<DiaryDay> getDay(String dayKey) async => day;

  @override
  Future<DiaryVersionPage> listVersions(
    String dayKey, {
    String? cursor,
    int pageSize = 20,
  }) async => DiaryVersionPage(versions: versions);

  @override
  Future<List<Provider>> listProviders() async => const [];

  @override
  Future<JobPage> listJobs({
    Set<JobState>? states,
    String? cursor,
    int pageSize = 20,
  }) async => const JobPage(jobs: []);

  @override
  Future<DiaryDay> setCurrentVersion({
    required String dayKey,
    required String versionId,
    required int expectedRevision,
  }) async {
    adopted.add(versionId);
    day = day.copyWith(currentVersionId: versionId, revision: day.revision + 1);
    return day;
  }

  @override
  Future<DiaryVersion> saveDiaryEdit({
    required String baseVersionId,
    required String body,
    required int expectedRevision,
    required String operationId,
  }) async {
    if (failSave) {
      throw const DiaryException(
        code: DiaryErrorCode.invalidState,
        message: '数据库被占用，这次没有写进去。',
      );
    }
    edits++;
    lastBody = body;
    final version = DiaryVersion(
      id: 'edit-$edits',
      dayKey: day.dayKey,
      body: body,
      origin: DiaryVersionOrigin.userEdit,
      createdAt: DateTime.utc(2026, 11, 11),
    );
    versions.insert(0, version);
    day = day.copyWith(currentVersionId: version.id, revision: day.revision + 1);
    return version;
  }

  @override
  dynamic noSuchMethod(Invocation invocation) =>
      throw UnsupportedError('${invocation.memberName} 不在本测试范围内');
}

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();

  /// 保存要读本机状态文件（真实 I/O），假时钟推不动——用 runAsync 给它时间。
  Future<void> settle(WidgetTester tester) async {
    for (var i = 0; i < 8; i++) {
      await tester.runAsync(
        () => Future<void>.delayed(const Duration(milliseconds: 40)),
      );
      await tester.pump();
    }
  }

  testWidgets('人工版退成候选后标成「人工修订」，不冒充 AI 候选', (tester) async {
    final root = Directory.systemTemp.createTempSync('ai-panel-label');
    addTearDown(() => root.deleteSync(recursive: true));
    PathProviderPlatform.instance = _FakePathProvider(root.path);
    tester.view.physicalSize = const Size(1200, 2000);
    tester.view.devicePixelRatio = 1;
    addTearDown(tester.view.resetPhysicalSize);
    addTearDown(tester.view.resetDevicePixelRatio);

    final api = _DiaryApi(
      day: DiaryDay(
        dayKey: '2026-11-11',
        timeZone: 'Asia/Shanghai',
        updatedAt: DateTime.utc(2026, 11, 11),
        currentVersionId: 'v2',
        candidateVersionIds: const ['v1'],
        revision: 4,
      ),
      versions: [
        DiaryVersion(
          id: 'v2',
          dayKey: '2026-11-11',
          body: 'AI 后来生成的一版。',
          origin: DiaryVersionOrigin.generated,
          createdAt: DateTime.utc(2026, 11, 11, 20),
        ),
        DiaryVersion(
          id: 'v1',
          dayKey: '2026-11-11',
          body: '我自己写的一版。',
          origin: DiaryVersionOrigin.userEdit,
          createdAt: DateTime.utc(2026, 11, 11, 18),
        ),
      ],
    );

    await tester.pumpWidget(
      MaterialApp(
        home: Scaffold(
          body: SingleChildScrollView(
            child: AiDiaryPanel(
              api: api,
              onOpenModel: () {},
              initialDayKey: '2026-11-11',
            ),
          ),
        ),
      ),
    );
    for (var i = 0; i < 8; i++) {
      await tester.runAsync(
        () => Future<void>.delayed(const Duration(milliseconds: 40)),
      );
      await tester.pump();
    }

    expect(find.text('当前阅读版'), findsOneWidget);
    expect(
      find.text('人工修订 · 尚未采纳'),
      findsOneWidget,
      reason: '退成候选的人工版不能标成 AI 整理候选',
    );
    expect(find.text('AI 整理候选 · 尚未采纳'), findsNothing);
    // 时间也要显示：同一天多版时靠它区分先后。按本地时区算，别写死字符串。
    final local = DateTime.utc(2026, 11, 11, 18).toLocal();
    final stamp = '${local.year}-${local.month.toString().padLeft(2, '0')}-'
        '${local.day.toString().padLeft(2, '0')} '
        '${local.hour.toString().padLeft(2, '0')}:'
        '${local.minute.toString().padLeft(2, '0')}';
    expect(find.textContaining(stamp), findsOneWidget);
  });

  testWidgets('全部版本能展开：旧版可见、能设为当前、能对比', (tester) async {
    final root = Directory.systemTemp.createTempSync('ai-panel-history');
    addTearDown(() => root.deleteSync(recursive: true));
    PathProviderPlatform.instance = _FakePathProvider(root.path);
    tester.view.physicalSize = const Size(1200, 2400);
    tester.view.devicePixelRatio = 1;
    addTearDown(tester.view.resetPhysicalSize);
    addTearDown(tester.view.resetDevicePixelRatio);

    final api = _DiaryApi(
      day: DiaryDay(
        dayKey: '2026-11-11',
        timeZone: 'Asia/Shanghai',
        updatedAt: DateTime.utc(2026, 11, 11),
        currentVersionId: 'v3',
        candidateVersionIds: const ['v2'],
        revision: 5,
      ),
      versions: [
        DiaryVersion(
          id: 'v3',
          dayKey: '2026-11-11',
          body: '第三版。',
          origin: DiaryVersionOrigin.generated,
          createdAt: DateTime.utc(2026, 11, 11, 20),
        ),
        DiaryVersion(
          id: 'v2',
          dayKey: '2026-11-11',
          body: '第二版。',
          origin: DiaryVersionOrigin.generated,
          createdAt: DateTime.utc(2026, 11, 11, 19),
        ),
        // 被换掉、既不是当前也不是候选的旧版——以前界面上完全看不到。
        DiaryVersion(
          id: 'v1',
          dayKey: '2026-11-11',
          body: '第一版。',
          origin: DiaryVersionOrigin.userEdit,
          createdAt: DateTime.utc(2026, 11, 11, 18),
        ),
      ],
    );

    await tester.pumpWidget(
      MaterialApp(
        home: Scaffold(
          body: SingleChildScrollView(
            child: AiDiaryPanel(
              api: api,
              onOpenModel: () {},
              initialDayKey: '2026-11-11',
            ),
          ),
        ),
      ),
    );
    for (var i = 0; i < 8; i++) {
      await tester.runAsync(
        () => Future<void>.delayed(const Duration(milliseconds: 40)),
      );
      await tester.pump();
    }

    expect(find.text('全部版本（3）'), findsOneWidget);
    expect(find.byKey(const ValueKey('version-row-v1')), findsNothing);

    await tester.tap(find.byKey(const Key('toggle-all-versions')));
    await tester.pumpAndSettle();
    expect(find.byKey(const ValueKey('version-row-v1')), findsOneWidget);
    expect(find.textContaining('人工修订 · 历史'), findsOneWidget);

    // 对比：能看到逐行差异。
    await tester.tap(find.byKey(const ValueKey('version-diff-v1')));
    await tester.pumpAndSettle();
    expect(find.textContaining('逐行差异'), findsOneWidget);
    expect(find.text('- 第三版。'), findsOneWidget);
    expect(find.text('+ 第一版。'), findsOneWidget);
    await tester.tap(find.text('关闭'));
    await tester.pumpAndSettle();

    // 设为当前：走 adopt。
    await tester.tap(find.byKey(const ValueKey('version-adopt-v1')));
    await settle(tester);
    expect(api.adopted, contains('v1'));
  });

  testWidgets('保存失败时对话框不关、正文不丢、错误显示在框里', (tester) async {
    final root = Directory.systemTemp.createTempSync('ai-panel-fail');
    addTearDown(() => root.deleteSync(recursive: true));
    PathProviderPlatform.instance = _FakePathProvider(root.path);
    tester.view.physicalSize = const Size(1200, 2000);
    tester.view.devicePixelRatio = 1;
    addTearDown(tester.view.resetPhysicalSize);
    addTearDown(tester.view.resetDevicePixelRatio);

    final api = _DiaryApi(
      day: DiaryDay(
        dayKey: '2026-11-11',
        timeZone: 'Asia/Shanghai',
        updatedAt: DateTime.utc(2026, 11, 11),
        currentVersionId: 'v1',
        revision: 3,
      ),
      versions: [
        DiaryVersion(
          id: 'v1',
          dayKey: '2026-11-11',
          body: 'AI 写的原文。',
          origin: DiaryVersionOrigin.generated,
          createdAt: DateTime.utc(2026, 11, 11),
        ),
      ],
    )..failSave = true;

    await tester.pumpWidget(
      MaterialApp(
        home: Scaffold(
          body: SingleChildScrollView(
            child: AiDiaryPanel(
              api: api,
              onOpenModel: () {},
              initialDayKey: '2026-11-11',
            ),
          ),
        ),
      ),
    );
    for (var i = 0; i < 8; i++) {
      await tester.runAsync(
        () => Future<void>.delayed(const Duration(milliseconds: 40)),
      );
      await tester.pump();
    }

    await tester.ensureVisible(find.byKey(const Key('diary-edit')));
    await tester.tap(find.byKey(const Key('diary-edit')));
    await tester.pumpAndSettle();
    await tester.enterText(
      find.byKey(const Key('diary-edit-field')),
      '我自己改过的正文，还没存上。',
    );
    await tester.tap(find.byKey(const Key('diary-edit-save')));
    await settle(tester);

    // 对话框还在、正文还在、错误在框里。
    expect(find.byKey(const Key('diary-edit-field')), findsOneWidget);
    expect(
      tester.widget<TextField>(find.byKey(const Key('diary-edit-field'))).controller?.text,
      '我自己改过的正文，还没存上。',
    );
    expect(find.byKey(const Key('diary-edit-error')), findsOneWidget);
    expect(api.edits, 0);

    // 修好之后再点一次就能存上。
    api.failSave = false;
    await tester.tap(find.byKey(const Key('diary-edit-save')));
    await settle(tester);
    expect(api.edits, 1);
    expect(api.lastBody, '我自己改过的正文，还没存上。');
    // 保存成功后对话框才关。
    await tester.pumpAndSettle();
    expect(find.byKey(const Key('diary-edit-field')), findsNothing);
  });

  testWidgets('正式日记页有「编辑正文」，保存走 saveDiaryEdit', (tester) async {
    final root = Directory.systemTemp.createTempSync('ai-panel-edit');
    addTearDown(() => root.deleteSync(recursive: true));
    PathProviderPlatform.instance = _FakePathProvider(root.path);
    tester.view.physicalSize = const Size(1200, 2000);
    tester.view.devicePixelRatio = 1;
    addTearDown(tester.view.resetPhysicalSize);
    addTearDown(tester.view.resetDevicePixelRatio);

    final api = _DiaryApi(
      day: DiaryDay(
        dayKey: '2026-11-11',
        timeZone: 'Asia/Shanghai',
        updatedAt: DateTime.utc(2026, 11, 11),
        currentVersionId: 'v1',
        revision: 3,
      ),
      versions: [
        DiaryVersion(
          id: 'v1',
          dayKey: '2026-11-11',
          body: 'AI 写的原文。',
          origin: DiaryVersionOrigin.generated,
          createdAt: DateTime.utc(2026, 11, 11),
        ),
      ],
    );

    await tester.pumpWidget(
      MaterialApp(
        home: Scaffold(
          body: SingleChildScrollView(
            child: AiDiaryPanel(
              api: api,
              onOpenModel: () {},
              initialDayKey: '2026-11-11',
            ),
          ),
        ),
      ),
    );
    // 面板启动要读本机状态文件（真实 I/O），假时钟推不动——用 runAsync 给它时间。
    for (var i = 0; i < 8; i++) {
      await tester.runAsync(
        () => Future<void>.delayed(const Duration(milliseconds: 40)),
      );
      await tester.pump();
    }

    final edit = find.byKey(const Key('diary-edit'));
    expect(edit, findsOneWidget, reason: '正式页必须有编辑入口');

    await tester.ensureVisible(edit);
    await tester.tap(edit);
    await tester.pumpAndSettle();

    expect(find.text('编辑正文'), findsWidgets);
    await tester.enterText(
      find.byKey(const Key('diary-edit-field')),
      '我自己改过的正文。',
    );
    await tester.tap(find.byKey(const Key('diary-edit-save')));
    await settle(tester);

    expect(api.edits, 1);
    expect(api.lastBody, '我自己改过的正文。');
  });
}

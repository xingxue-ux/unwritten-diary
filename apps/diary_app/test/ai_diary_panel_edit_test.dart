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
  Future<DiaryVersion> saveDiaryEdit({
    required String baseVersionId,
    required String body,
    required int expectedRevision,
    required String operationId,
  }) async {
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
    await tester.pumpAndSettle();

    expect(api.edits, 1);
    expect(api.lastBody, '我自己改过的正文。');
  });
}

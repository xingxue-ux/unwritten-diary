import 'package:diary_api/diary_api.dart';
import 'package:diary_mock/diary_mock.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:unwritten_diary/main.dart';
import 'package:unwritten_diary/src/ui_strings.dart';

/// 纯文字命中点进去要能看到**完整正文**，不是一段截断的摘录。
///
/// 以前 `sourceId == null` 的命中直接显示 `hit.snippet`，用户点进去只读到半句；
/// 命中还可能不在最近 20 条缓存里，所以必须按 `captureId` 回查。
class _SearchApi extends MockDiaryApi {
  _SearchApi({required this.fullText});

  final String fullText;

  @override
  Future<CapturePage> listCaptures({
    String? dayKey,
    String? cursor,
    int pageSize = 20,
  }) async => const CapturePage(captures: [], nextCursor: null);

  @override
  Future<Capture> getCapture(String id) async => Capture(
    id: id,
    revision: 2,
    state: CaptureState.committed,
    occurredAt: DateTime.utc(2026, 11, 12),
    createdAt: DateTime.utc(2026, 11, 12),
    updatedAt: DateTime.utc(2026, 11, 12),
    timeZone: 'Asia/Shanghai',
    utcOffsetMinutes: 480,
    dayKey: '2026-11-12',
    draftText: fullText,
    orderedSourceIds: const ['src-1'],
  );

  @override
  Future<SourceRevision?> previousSourceRevision(String sourceId) async => null;

  @override
  Future<ExtractedContent?> extractedContent(String sourceId) async =>
      ExtractedContent(
        id: 'c1',
        sourceId: sourceId,
        sourceRevisionId: 'rev-1',
        extractorId: 'text',
        extractorVersion: '1',
        status: ProcessingStatus.ready,
        coverage: Coverage.complete,
        text: fullText,
      );

  @override
  Future<SearchSnapshot> startSearch({
    required SearchRequest request,
    required int queryRevision,
  }) async => SearchSnapshot(
    sessionId: 's1',
    queryRevision: queryRevision,
    phase: SearchPhase.done,
    results: [
      SearchHit(
        hitId: 'h1',
        groupId: 'cap-1',
        captureId: 'cap-1',
        sourceKind: SourceKind.text,
        matchedBy: const {MatchedBy.keyword},
        coverage: Coverage.complete,
        dayKey: '2026-11-12',
        snippet: '……散步，看见蓝色风筝……',
      ),
    ],
  );
}

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();
  setUpAll(() async {
    await uiStrings.load();
  });

  testWidgets('搜索命中点进去看到完整正文，不是截断的摘录', (tester) async {
    const full =
        '基本场景验收：午后去公园散步，看见蓝色风筝。晚上读书二十分钟。检索标记：橘子月船。';
    tester.view.physicalSize = const Size(1400, 1200);
    tester.view.devicePixelRatio = 1;
    addTearDown(tester.view.resetPhysicalSize);
    addTearDown(tester.view.resetDevicePixelRatio);

    await tester.pumpWidget(DiaryApp(api: _SearchApi(fullText: full)));
    await tester.pumpAndSettle();
    await tester.tap(find.text('搜索').last);
    await tester.pumpAndSettle();

    await tester.enterText(find.byKey(const Key('global-search')), '橘子月船');
    await tester.testTextInput.receiveAction(TextInputAction.done);
    await tester.pumpAndSettle();

    expect(find.textContaining('……散步，看见蓝色风筝……'), findsOneWidget);
    await tester.tap(find.textContaining('……散步，看见蓝色风筝……'));
    await tester.pumpAndSettle();

    // 完整正文（含开头那句）必须出现；截断摘录不该再作为正文显示。
    expect(find.textContaining('基本场景验收：午后去公园散步'), findsOneWidget);
    expect(find.textContaining('检索标记：橘子月船'), findsOneWidget);
  });
}

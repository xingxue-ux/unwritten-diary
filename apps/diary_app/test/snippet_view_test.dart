import 'package:diary_api/diary_api.dart';
import 'package:diary_mock/diary_mock.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:unwritten_diary/main.dart';
import 'package:unwritten_diary/src/ui_strings.dart';

/// 片段页：卡片**一样高**，并且能在「列表」和「表格」两种查看方式之间切。
class _SnippetsApi extends MockDiaryApi {
  _SnippetsApi(this.rows);
  final List<Capture> rows;

  @override
  Future<CapturePage> listCaptures({
    String? dayKey,
    String? cursor,
    int pageSize = 20,
  }) async => CapturePage(captures: rows, nextCursor: null);
}

Capture _capture(String id, String text, {int day = 1}) => Capture(
  id: id,
  revision: 1,
  state: CaptureState.committed,
  occurredAt: DateTime.utc(2026, 11, day),
  createdAt: DateTime.utc(2026, 11, day),
  updatedAt: DateTime.utc(2026, 11, day),
  timeZone: 'Asia/Shanghai',
  utcOffsetMinutes: 480,
  dayKey: '2026-11-${day.toString().padLeft(2, '0')}',
  draftText: text,
);

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();
  setUpAll(() async {
    await uiStrings.load();
  });

  Future<void> openSnippets(WidgetTester tester, _SnippetsApi api) async {
    tester.view.physicalSize = const Size(1200, 1600);
    tester.view.devicePixelRatio = 1;
    addTearDown(tester.view.resetPhysicalSize);
    addTearDown(tester.view.resetDevicePixelRatio);
    await tester.pumpWidget(DiaryApp(api: api));
    await tester.pumpAndSettle();
    await tester.tap(find.text('片段').last);
    await tester.pumpAndSettle();
  }

  testWidgets('列表里的卡片一样高（一行摘要和两行摘要都占两行）', (tester) async {
    await openSnippets(
      tester,
      _SnippetsApi([
        _capture('a', '短。'),
        _capture('b', '这一条长得多，足够折成两行显示，用来和上面那条比高度。', day: 2),
      ]),
    );

    final heights = <double>[];
    for (final id in ['a', 'b']) {
      final tile = find.ancestor(
        of: find.text(id == 'a' ? '短。' : '这一条长得多，足够折成两行显示，用来和上面那条比高度。'),
        matching: find.byType(InkWell),
      );
      expect(tile, findsWidgets);
      heights.add(tester.getSize(tile.first).height);
    }
    expect(
      heights[0],
      closeTo(heights[1], 0.5),
      reason: '同一页的卡片必须一样高，否则一列看下去参差不齐',
    );
  });

  testWidgets('可以切到表格视图，一行一条', (tester) async {
    await openSnippets(
      tester,
      _SnippetsApi([
        _capture('a', '第一条正文。'),
        _capture('b', '第二条正文。', day: 2),
      ]),
    );

    expect(find.byKey(const ValueKey('snippet-view-表格')), findsOneWidget);
    await tester.tap(find.byKey(const ValueKey('snippet-view-表格')));
    await tester.pumpAndSettle();

    expect(find.text('日期'), findsOneWidget);
    expect(find.text('状态'), findsOneWidget);
    expect(find.text('摘要'), findsOneWidget);
    expect(find.byKey(const ValueKey('snippet-row-a')), findsOneWidget);
    expect(find.byKey(const ValueKey('snippet-row-b')), findsOneWidget);

    // 切回列表。
    await tester.tap(find.byKey(const ValueKey('snippet-view-列表')));
    await tester.pumpAndSettle();
    expect(find.text('日期'), findsNothing);
    expect(find.byKey(const ValueKey('snippet-row-a')), findsNothing);
  });
}

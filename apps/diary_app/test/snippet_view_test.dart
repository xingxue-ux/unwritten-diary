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

  testWidgets('网格是懒加载：几百条只建看得见的那些', (tester) async {
    await openSnippets(
      tester,
      _SnippetsApi([
        for (var index = 0; index < 300; index++)
          _capture('lazy$index', '第 $index 条。', day: (index % 28) + 1),
      ]),
    );
    await tester.tap(find.byKey(const ValueKey('snippet-view-表格')));
    await tester.pumpAndSettle();

    final built = find
        .byWidgetPredicate(
          (widget) =>
              widget.key is ValueKey<String> &&
              (widget.key! as ValueKey<String>).value.startsWith('snippet-tile-'),
        )
        .evaluate()
        .length;
    expect(built, greaterThan(0));
    expect(
      built,
      lessThan(120),
      reason: '300 条只该建屏幕里看得到的那几十张，不是一次全铺出来',
    );
  });

  testWidgets('表格视图是二维网格：一行放多张、卡片一样大', (tester) async {
    await openSnippets(
      tester,
      _SnippetsApi([
        for (var index = 0; index < 6; index++)
          _capture(
            'c$index',
            index.isEven ? '短。' : '这一条长得多，足够折成三行显示，用来和别的卡比大小。',
            day: index + 1,
          ),
      ]),
    );

    expect(find.byKey(const ValueKey('snippet-view-表格')), findsOneWidget);
    await tester.tap(find.byKey(const ValueKey('snippet-view-表格')));
    await tester.pumpAndSettle();

    final tiles = [
      for (var index = 0; index < 6; index++)
        find.byKey(ValueKey('snippet-tile-c$index')),
    ];
    for (final tile in tiles) {
      expect(tile, findsOneWidget);
    }

    // 二维：至少有两条卡在同一行（y 相同），也至少有两行（y 不同）。
    final rects = tiles.map((tile) => tester.getRect(tile)).toList();
    final sameRow = rects.where((rect) => (rect.top - rects.first.top).abs() < 0.5);
    expect(
      sameRow.length,
      greaterThan(1),
      reason: '一行要放多张，否则还是列表',
    );
    final rows = rects.map((rect) => rect.top.round()).toSet();
    expect(rows.length, greaterThan(1), reason: '要铺成多行');

    // 一样大：宽高都相同。
    for (final rect in rects) {
      expect(rect.width, closeTo(rects.first.width, 0.5));
      expect(rect.height, closeTo(rects.first.height, 0.5));
    }

    // 切回列表：网格消失。
    await tester.tap(find.byKey(const ValueKey('snippet-view-列表')));
    await tester.pumpAndSettle();
    expect(find.byKey(const ValueKey('snippet-tile-c0')), findsNothing);
  });
}

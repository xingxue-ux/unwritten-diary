import 'package:diary_api/diary_api.dart';
import 'package:diary_mock/diary_mock.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:unwritten_diary/main.dart';
import 'package:unwritten_diary/src/ui_strings.dart';

/// 列表里的卡片**宽度也要一致**。
///
/// `_sliverContent` 用 `Center` 居中，给的是松约束：不显式撑满时，卡片会缩到
/// 自己内容的宽度——摘要短的那条就比长的窄一截。真机上就是这样：一条长记录的卡
/// 占满整行，下面两条短记录缩成半截宽。
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

  testWidgets('列表卡片宽度一致（长摘要和短摘要一样宽）', (tester) async {
    tester.view.physicalSize = const Size(1400, 1400);
    tester.view.devicePixelRatio = 1;
    addTearDown(tester.view.resetPhysicalSize);
    addTearDown(tester.view.resetDevicePixelRatio);

    await tester.pumpWidget(
      DiaryApp(
        api: _SnippetsApi([
          _capture(
            'long',
            '这是一条很长的记录，长到足以占满整行宽度，用来和下面那条短记录比较卡片宽度。',
          ),
          _capture('short', '短。', day: 2),
        ]),
      ),
    );
    await tester.pumpAndSettle();
    await tester.tap(find.text('片段').last);
    await tester.pumpAndSettle();

    final long = tester.getRect(
      find.ancestor(
        of: find.textContaining('这是一条很长的记录'),
        matching: find.byType(InkWell),
      ).first,
    );
    final short = tester.getRect(
      find.ancestor(
        of: find.text('短。'),
        matching: find.byType(InkWell),
      ).first,
    );
    expect(
      short.width,
      closeTo(long.width, 0.5),
      reason: '摘要短的那条不能比长的窄——一列看下去要一样宽',
    );
  });
}

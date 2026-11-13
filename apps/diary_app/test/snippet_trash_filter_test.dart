import 'package:diary_api/diary_api.dart';
import 'package:diary_mock/diary_mock.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:unwritten_diary/main.dart';
import 'package:unwritten_diary/src/ui_strings.dart';

/// 已移入回收站的记录**不能**出现在普通「片段」时间线上。
///
/// 核心的 `captures.list` 会把 trashed 一起带回来（回收站页要用同一份数据），
/// 但时间线画出来就等于告诉用户「删除没成功」——而它其实在回收站里好好的。
class _TrashApi extends MockDiaryApi {
  @override
  Future<CapturePage> listCaptures({
    String? dayKey,
    String? cursor,
    int pageSize = 20,
  }) async => CapturePage(
    captures: [
      Capture(
        id: 'kept',
        revision: 1,
        state: CaptureState.committed,
        occurredAt: DateTime.utc(2026, 11, 12),
        createdAt: DateTime.utc(2026, 11, 12),
        updatedAt: DateTime.utc(2026, 11, 12),
        timeZone: 'Asia/Shanghai',
        utcOffsetMinutes: 480,
        dayKey: '2026-11-12',
        draftText: '还在的记录。',
      ),
      Capture(
        id: 'gone',
        revision: 2,
        state: CaptureState.trashed,
        occurredAt: DateTime.utc(2026, 11, 11),
        createdAt: DateTime.utc(2026, 11, 11),
        updatedAt: DateTime.utc(2026, 11, 11),
        timeZone: 'Asia/Shanghai',
        utcOffsetMinutes: 480,
        dayKey: '2026-11-11',
        draftText: '已经进回收站的记录。',
      ),
    ],
    nextCursor: null,
  );
}

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();
  setUpAll(() async {
    await uiStrings.load();
  });

  testWidgets('回收站里的记录不出现在片段列表和网格里', (tester) async {
    tester.view.physicalSize = const Size(1400, 1200);
    tester.view.devicePixelRatio = 1;
    addTearDown(tester.view.resetPhysicalSize);
    addTearDown(tester.view.resetDevicePixelRatio);

    await tester.pumpWidget(DiaryApp(api: _TrashApi()));
    await tester.pumpAndSettle();
    await tester.tap(find.text('片段').last);
    await tester.pumpAndSettle();

    expect(find.textContaining('还在的记录'), findsOneWidget);
    expect(
      find.textContaining('已经进回收站的记录'),
      findsNothing,
      reason: '回收站里的记录不该出现在时间线上',
    );

    await tester.tap(find.byKey(const ValueKey('snippet-view-表格')));
    await tester.pumpAndSettle();
    expect(find.textContaining('还在的记录'), findsWidgets);
    expect(find.textContaining('已经进回收站的记录'), findsNothing);
  });
}

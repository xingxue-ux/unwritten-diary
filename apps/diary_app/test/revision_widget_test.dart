import 'package:diary_api/diary_api.dart';
import 'package:diary_mock/diary_mock.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:unwritten_diary/main.dart';
import 'package:unwritten_diary/src/ui_strings.dart';

class RevisionWidgetApi extends MockDiaryApi {
  String body = '今天遇见旧朋友。';
  String? previous;
  int captureRevision = 3;

  Capture get capture => Capture(
    id: 'capture',
    revision: captureRevision,
    state: CaptureState.committed,
    occurredAt: DateTime.utc(2026, 10, 13),
    createdAt: DateTime.utc(2026, 10, 13),
    updatedAt: DateTime.utc(2026, 10, 13),
    timeZone: 'Asia/Shanghai',
    utcOffsetMinutes: 480,
    dayKey: '2026-10-13',
    orderedSourceIds: const ['source'],
    draftText: body,
  );

  @override
  Future<CapturePage> listCaptures({
    String? dayKey,
    String? cursor,
    int pageSize = 20,
  }) async => CapturePage(captures: [capture]);

  @override
  Future<ExtractedContent?> extractedContent(String sourceId) async =>
      ExtractedContent(
        id: 'extracted',
        sourceId: sourceId,
        sourceRevisionId: 'current',
        extractorId: 'text_direct',
        extractorVersion: '1',
        status: ProcessingStatus.ready,
        coverage: Coverage.complete,
        text: body,
      );

  @override
  Future<SourceRevision?> previousSourceRevision(String sourceId) async =>
      previous == null
      ? null
      : SourceRevision(
          revisionId: 'previous',
          sourceId: sourceId,
          text: previous,
          authorType: AuthorType.user,
          occurredAt: DateTime.utc(2026, 10, 13),
        );

  @override
  Future<SourceRevision> reviseText({
    required String sourceId,
    required String text,
    required int expectedRevision,
    required String operationId,
  }) async {
    expect(expectedRevision, captureRevision);
    previous = body;
    body = text;
    captureRevision++;
    return SourceRevision(
      revisionId: 'current',
      sourceId: sourceId,
      parentRevisionId: 'previous',
      text: text,
      authorType: AuthorType.user,
      occurredAt: DateTime.utc(2026, 10, 13),
    );
  }
}

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();
  setUpAll(() async {
    await uiStrings.load();
  });

  testWidgets('committed text can be revised and its old text is read only', (
    tester,
  ) async {
    final api = RevisionWidgetApi();
    await tester.pumpWidget(DiaryApp(api: api));
    await tester.pumpAndSettle();
    await tester.tap(find.text('片段').last);
    await tester.pumpAndSettle();
    await tester.tap(find.text('刚刚收好的记录').last);
    await tester.pumpAndSettle();

    expect(find.text('今天遇见旧朋友。'), findsWidgets);
    expect(find.text('看原正文'), findsNothing);
    await tester.tap(find.text('修改正文'));
    await tester.pumpAndSettle();
    await tester.enterText(
      find.byKey(const Key('revision-editor')),
      '今天遇见新朋友。',
    );
    await tester.ensureVisible(find.text('保存正文修改'));
    await tester.tap(find.text('保存正文修改'));
    await tester.pumpAndSettle();

    expect(api.body, '今天遇见新朋友。');
    expect(find.text('今天遇见新朋友。'), findsWidgets);
    await tester.tap(find.text('看原正文'));
    await tester.pumpAndSettle();
    expect(find.text('今天遇见旧朋友。'), findsWidgets);
    expect(find.byKey(const Key('revision-editor')), findsNothing);
    await tester.tap(find.text('看改了什么'));
    await tester.pumpAndSettle();
    expect(find.textContaining('划线是删去的文字'), findsOneWidget);
  });
}

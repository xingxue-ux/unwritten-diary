import 'package:diary_api/diary_api.dart';
import 'package:diary_mock/diary_mock.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:unwritten_diary/src/revision_controller.dart';
import 'package:unwritten_diary/src/revision_diff.dart';

class RevisionApi extends MockDiaryApi {
  String current = '旧正文';
  String? previous;
  int? receivedRevision;
  int extractionCount = 0;
  bool extractionFails = false;

  @override
  Future<ExtractedContent?> extractedContent(String sourceId) async =>
      ExtractedContent(
        id: 'extracted',
        sourceId: sourceId,
        sourceRevisionId: 'revision',
        extractorId: 'text',
        extractorVersion: '1',
        status: ProcessingStatus.ready,
        coverage: Coverage.complete,
        text: current,
      );

  @override
  Future<SourceRevision?> previousSourceRevision(String sourceId) async =>
      previous == null
      ? null
      : SourceRevision(
          revisionId: 'before',
          sourceId: sourceId,
          text: previous,
          authorType: AuthorType.user,
          occurredAt: DateTime.utc(2026),
        );

  @override
  Future<SourceRevision> reviseText({
    required String sourceId,
    required String text,
    required int expectedRevision,
    required String operationId,
  }) async {
    receivedRevision = expectedRevision;
    previous = current;
    current = text;
    return SourceRevision(
      revisionId: 'after',
      sourceId: sourceId,
      parentRevisionId: 'before',
      text: text,
      authorType: AuthorType.user,
      occurredAt: DateTime.utc(2026),
    );
  }

  @override
  Future<ExtractedContent> extractSource(String sourceId) async {
    extractionCount++;
    if (extractionFails) throw StateError('index failed');
    return (await extractedContent(sourceId))!;
  }
}

void main() {
  test(
    'saving uses the revision opened by the user and preserves the prior text',
    () async {
      final api = RevisionApi();
      final controller = RevisionController(api);
      await controller.open(
        sourceId: 'source',
        captureId: 'capture',
        captureRevision: 4,
      );
      expect(controller.currentText, '旧正文');
      expect(controller.hasPrevious, isFalse);
      expect(await controller.save('新正文'), isTrue);
      expect(api.receivedRevision, 4);
      expect(api.extractionCount, 1);
      expect(controller.currentText, '新正文');
      expect(controller.previousText, '旧正文');
      expect(controller.captureRevision, 5);
      controller.dispose();
    },
  );

  test('failed reindex does not make a durable edit look unsaved', () async {
    final api = RevisionApi()..extractionFails = true;
    final controller = RevisionController(api);
    await controller.open(
      sourceId: 'source',
      captureId: 'capture',
      captureRevision: 4,
    );
    expect(await controller.save('新版'), isTrue);
    expect(controller.currentText, '新版');
    expect(controller.indexWarning, isNotNull);
    controller.dispose();
  });

  test('line diff marks replacement as removal and addition', () {
    final lines = revisionDiff('早上很好\n下午下雨', '早上很好\n下午晴天');
    expect(lines.map((line) => line.kind), [
      RevisionLineKind.same,
      RevisionLineKind.removed,
      RevisionLineKind.added,
    ]);
  });
}

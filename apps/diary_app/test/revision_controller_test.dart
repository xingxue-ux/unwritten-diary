import 'dart:async';

import 'package:diary_api/diary_api.dart';
import 'package:diary_mock/diary_mock.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:unwritten_diary/src/revision_controller.dart';
import 'package:unwritten_diary/src/revision_diff.dart';

class RevisionApi extends MockDiaryApi {
  String current = '旧正文';
  String? previous;
  bool previousAsset = false;
  String? extractedRef;
  int? receivedRevision;
  String? receivedOperationId;
  bool saveFails = false;
  bool noCurrentExtraction = false;
  Object? openFailure;
  Object? saveFailure;
  Completer<SourceRevision>? delayedSave;

  @override
  Future<ExtractedContent?> extractedContent(String sourceId) async {
    if (openFailure != null) throw openFailure!;
    if (noCurrentExtraction) return null;
    return ExtractedContent(
      id: 'extracted',
      sourceId: sourceId,
      sourceRevisionId: 'revision',
      extractorId: 'text',
      extractorVersion: '1',
      status: ProcessingStatus.ready,
      coverage: Coverage.complete,
      text: current,
    );
  }

  @override
  Future<SourceRevision?> previousSourceRevision(String sourceId) async =>
      previous == null && !previousAsset
      ? null
      : SourceRevision(
          revisionId: 'before',
          sourceId: sourceId,
          text: previousAsset ? null : previous,
          assetId: previousAsset ? 'asset' : null,
          authorType: AuthorType.user,
          occurredAt: DateTime.utc(2026),
        );

  @override
  Future<ExtractedContent> extractSource(String sourceRef) async {
    extractedRef = sourceRef;
    return ExtractedContent(
      id: 'old-extracted',
      sourceId: 'source',
      sourceRevisionId: sourceRef,
      extractorId: 'text',
      extractorVersion: '1',
      status: ProcessingStatus.ready,
      coverage: Coverage.complete,
      text: '导入文件的原正文',
    );
  }

  @override
  Future<SourceRevision> reviseText({
    required String sourceId,
    required String text,
    required int expectedRevision,
    required String operationId,
  }) async {
    receivedRevision = expectedRevision;
    receivedOperationId = operationId;
    if (saveFails) throw StateError('temporary failure');
    if (saveFailure != null) throw saveFailure!;
    if (delayedSave != null) return delayedSave!.future;
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
      expect(await controller.save('新正文'), RevisionSaveResult.saved);
      expect(api.receivedRevision, 4);
      expect(controller.currentText, '新正文');
      expect(controller.previousText, '旧正文');
      expect(controller.captureRevision, 5);
      expect(await controller.save('新正文'), RevisionSaveResult.unchanged);
      expect(controller.captureRevision, 5);
      controller.dispose();
    },
  );

  test('failed save keeps its operation ID for an idempotent retry', () async {
    final api = RevisionApi()..saveFails = true;
    final controller = RevisionController(api);
    await controller.open(
      sourceId: 'source',
      captureId: 'capture',
      captureRevision: 4,
    );
    expect(await controller.save('新版'), RevisionSaveResult.failed);
    final operationId = api.receivedOperationId;
    api.saveFails = false;
    expect(await controller.save('新版'), RevisionSaveResult.saved);
    expect(api.receivedOperationId, operationId);
    expect(controller.currentText, '新版');
    controller.dispose();
  });

  test(
    'a failed save with changed text cannot mint a new operation ID',
    () async {
      final api = RevisionApi()..saveFails = true;
      final controller = RevisionController(api);
      await controller.open(
        sourceId: 'source',
        captureId: 'capture',
        captureRevision: 4,
      );
      expect(await controller.save('新版'), RevisionSaveResult.failed);
      final operationId = api.receivedOperationId;
      api.saveFails = false;
      expect(await controller.save('又改一版'), RevisionSaveResult.failed);
      expect(api.receivedOperationId, operationId);
      expect(controller.error, contains('重新打开'));
      controller.dispose();
    },
  );

  test(
    'revision conflict blocks retries until the source is reopened',
    () async {
      final api = RevisionApi()
        ..saveFailure = const DiaryException(
          code: DiaryErrorCode.revisionConflict,
          message: 'stale',
        );
      final controller = RevisionController(api);
      await controller.open(
        sourceId: 'source',
        captureId: 'capture',
        captureRevision: 4,
      );
      expect(await controller.save('新版'), RevisionSaveResult.failed);
      final operationId = api.receivedOperationId;
      api.saveFailure = null;
      expect(await controller.save('新版'), RevisionSaveResult.failed);
      expect(api.receivedOperationId, operationId);
      expect(controller.error, contains('版本'));
      controller.dispose();
    },
  );

  test('an unextracted current revision never displays the old text', () async {
    final api = RevisionApi()
      ..previous = '旧正文'
      ..noCurrentExtraction = true;
    final controller = RevisionController(api);
    await controller.open(
      sourceId: 'source',
      captureId: 'capture',
      captureRevision: 4,
      initialText: '旧正文',
    );
    expect(controller.currentText, isNull);
    expect(controller.previousText, '旧正文');
    expect(controller.canEdit, isFalse);
    expect(controller.indexWarning, contains('未完成提取'));
    controller.dispose();
  });

  test('open shows a safe error message without exception internals', () async {
    final api = RevisionApi()
      ..openFailure = const DiaryException(
        code: DiaryErrorCode.invalidState,
        message: '正文尚未就绪',
      );
    final controller = RevisionController(api);
    await controller.open(sourceId: 'source');
    expect(controller.error, '正文尚未就绪');
    controller.dispose();
  });

  test('an old save cannot replace a newly opened source', () async {
    final api = RevisionApi()..delayedSave = Completer<SourceRevision>();
    final controller = RevisionController(api);
    await controller.open(
      sourceId: 'first',
      captureId: 'capture-1',
      captureRevision: 4,
    );
    final saving = controller.save('新版');
    await controller.open(
      sourceId: 'second',
      captureId: 'capture-2',
      captureRevision: 8,
    );
    api.delayedSave!.complete(
      SourceRevision(
        revisionId: 'after',
        sourceId: 'first',
        text: '新版',
        authorType: AuthorType.user,
        occurredAt: DateTime.utc(2026),
      ),
    );
    expect(await saving, RevisionSaveResult.saved);
    expect(controller.sourceId, 'second');
    expect(controller.captureRevision, 8);
    expect(controller.currentText, '旧正文');
    controller.dispose();
  });

  test('previous imported original is read by revision ID on demand', () async {
    final api = RevisionApi()..previousAsset = true;
    final controller = RevisionController(api);
    await controller.open(sourceId: 'source');
    expect(controller.hasPrevious, isTrue);
    expect(controller.previousText, isNull);
    expect(api.extractedRef, isNull);
    await controller.loadPreviousText();
    expect(api.extractedRef, 'before');
    expect(controller.previousText, '导入文件的原正文');
    controller.dispose();
  });

  test('line diff marks replacement as removal and addition', () {
    final lines = revisionDiff('早上很好\n下午下雨', '早上很好\n下午晴天');
    expect(lines.map((line) => line.kind), [
      RevisionLineKind.same,
      RevisionLineKind.removed,
      RevisionLineKind.added,
    ]);
    expect(lines[0].text, '早上很好\n下午');
    expect(lines[1].text, '下雨');
    expect(lines[2].text, '晴天');
  });
}

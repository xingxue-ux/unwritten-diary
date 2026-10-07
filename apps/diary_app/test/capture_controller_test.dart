import 'dart:async';

import 'package:diary_api/diary_api.dart';
import 'package:diary_mock/diary_mock.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:unwritten_diary/src/capture_controller.dart';

class SaveCall {
  const SaveCall(this.text, this.revision, this.operationId);
  final String text;
  final int revision;
  final String operationId;
}

class DelayedSaveApi extends MockDiaryApi {
  final entered = Completer<void>();
  final release = Completer<void>();
  final calls = <SaveCall>[];
  int commits = 0;

  @override
  Future<DraftSaveResult> saveDraft({
    required String id,
    required String text,
    required int expectedRevision,
    required String operationId,
  }) async {
    calls.add(SaveCall(text, expectedRevision, operationId));
    if (calls.length == 1) {
      entered.complete();
      await release.future;
    }
    return super.saveDraft(
      id: id,
      text: text,
      expectedRevision: expectedRevision,
      operationId: operationId,
    );
  }

  @override
  Future<CommitResult> commit({
    required String id,
    required int expectedRevision,
    required String operationId,
  }) async {
    commits++;
    return super.commit(
      id: id,
      expectedRevision: expectedRevision,
      operationId: operationId,
    );
  }
}

class LostSaveAcknowledgementApi extends MockDiaryApi {
  final calls = <SaveCall>[];

  @override
  Future<DraftSaveResult> saveDraft({
    required String id,
    required String text,
    required int expectedRevision,
    required String operationId,
  }) async {
    calls.add(SaveCall(text, expectedRevision, operationId));
    final result = await super.saveDraft(
      id: id,
      text: text,
      expectedRevision: expectedRevision,
      operationId: operationId,
    );
    if (calls.length == 1) {
      throw const DiaryException(
        code: DiaryErrorCode.unknown,
        message: '保存确认丢失，请重试。',
        retryable: true,
      );
    }
    return result;
  }
}

class LostCommitAcknowledgementApi extends MockDiaryApi {
  final operationIds = <String>[];

  @override
  Future<CommitResult> commit({
    required String id,
    required int expectedRevision,
    required String operationId,
  }) async {
    operationIds.add(operationId);
    final result = await super.commit(
      id: id,
      expectedRevision: expectedRevision,
      operationId: operationId,
    );
    if (operationIds.length == 1) {
      throw const DiaryException(
        code: DiaryErrorCode.unknown,
        message: '提交确认丢失，请重试。',
        retryable: true,
      );
    }
    return result;
  }
}

class SlowListApi extends MockDiaryApi {
  final entered = Completer<void>();
  final release = Completer<void>();

  @override
  Future<CapturePage> listCaptures({
    String? dayKey,
    String? cursor,
    int pageSize = 20,
  }) async {
    if (!entered.isCompleted) entered.complete();
    await release.future;
    return super.listCaptures(
      dayKey: dayKey,
      cursor: cursor,
      pageSize: pageSize,
    );
  }
}

void main() {
  test(
    'save and finish serialize edits made while a save is in flight',
    () async {
      final api = DelayedSaveApi();
      final controller = CaptureController(api);
      await controller.initialize();
      controller.updateText('A');
      final firstSave = controller.saveNow();
      await api.entered.future;
      controller.updateText('AB');
      final finish = controller.finish();
      api.release.complete();

      expect(await finish, isTrue);
      expect(await firstSave, isTrue);
      expect(api.calls.map((call) => call.text), ['A', 'AB']);
      expect(api.calls.map((call) => call.revision), [0, 1]);
      expect(api.commits, 1);
      expect((await api.listCaptures()).captures.single.draftText, 'AB');
      controller.dispose();
    },
  );

  test('save retry reuses operationId after a lost acknowledgement', () async {
    final api = LostSaveAcknowledgementApi();
    final controller = CaptureController(api);
    await controller.initialize();
    controller.updateText('这一刻');
    expect(await controller.saveNow(), isFalse);
    expect(controller.text, '这一刻');
    expect(await controller.saveNow(), isTrue);
    expect(api.calls, hasLength(2));
    expect(api.calls[1].operationId, api.calls[0].operationId);
    expect(api.calls[1].revision, api.calls[0].revision);
    expect((await api.listCaptures()).captures.single.revision, 1);
    controller.dispose();
  });

  test(
    'commit retry reuses operationId after a lost acknowledgement',
    () async {
      final api = LostCommitAcknowledgementApi();
      final controller = CaptureController(api);
      await controller.initialize();
      controller.updateText('这一刻');
      expect(await controller.finish(), isFalse);
      expect(controller.text, '这一刻');
      expect(await controller.finish(), isTrue);
      expect(api.operationIds, hasLength(2));
      expect(api.operationIds[1], api.operationIds[0]);
      expect(
        (await api.listCaptures()).captures.single.state,
        CaptureState.committed,
      );
      controller.dispose();
    },
  );

  test('an in-flight refresh can complete after dispose', () async {
    final api = SlowListApi();
    final controller = CaptureController(api);
    final initialization = controller.initialize();
    controller.dispose();
    api.release.complete();
    await initialization;
  });

  test('typing during draft recovery is not overwritten', () async {
    final api = SlowListApi();
    final draft = await api.createDraft(operationId: 'create-existing');
    await api.saveDraft(
      id: draft.id,
      text: '旧草稿',
      expectedRevision: draft.revision,
      operationId: 'save-existing',
    );
    final controller = CaptureController(api);
    final initialization = controller.initialize();
    await api.entered.future;
    controller.updateText('新输入');
    api.release.complete();
    await initialization;
    expect(controller.text, '新输入');
    controller.dispose();
  });
}

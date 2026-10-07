import 'package:diary_api/diary_api.dart';
import 'package:diary_mock/diary_mock.dart';
import 'package:test/test.dart';

void main() {
  final now = DateTime.utc(2026, 9, 30, 13, 5);

  test('保存、提交及重读使用同一条记录', () async {
    final api = MockDiaryApi(clock: () => now);
    final draft = await api.createDraft(operationId: 'create-1');
    final saved = await api.saveDraft(
      id: draft.id,
      text: '今天的微风很好。',
      expectedRevision: draft.revision,
      operationId: 'save-1',
    );
    expect(saved.durable, isTrue);
    final done = await api.commit(
      id: draft.id,
      expectedRevision: saved.revision,
      operationId: 'commit-1',
    );
    expect(done.capture.state, CaptureState.committed);
    expect((await api.getCapture(draft.id)).draftText, '今天的微风很好。');
    expect((await api.listCaptures()).captures.single.id, draft.id);
  });

  test('同一操作幂等，旧修订不可覆盖新内容', () async {
    final api = MockDiaryApi(clock: () => now);
    final draft = await api.createDraft(operationId: 'create-1');
    expect((await api.createDraft(operationId: 'create-1')).id, draft.id);
    final first = await api.saveDraft(
      id: draft.id,
      text: '第一句',
      expectedRevision: 0,
      operationId: 'save-1',
    );
    final repeat = await api.saveDraft(
      id: draft.id,
      text: '第一句',
      expectedRevision: 0,
      operationId: 'save-1',
    );
    expect(repeat.revision, first.revision);
    expect(
      () => api.saveDraft(
        id: draft.id,
        text: '第二句',
        expectedRevision: 0,
        operationId: 'save-2',
      ),
      throwsA(
        isA<DiaryException>().having(
          (error) => error.code,
          'code',
          DiaryErrorCode.revisionConflict,
        ),
      ),
    );
  });

  test('失败的保存可用相同操作号重试', () async {
    final api = MockDiaryApi(clock: () => now)
      ..nextSaveFailure = const DiaryException(
        code: DiaryErrorCode.storageFull,
        message: '空间不足',
      );
    final draft = await api.createDraft(operationId: 'create-1');
    Future<DraftSaveResult> save() => api.saveDraft(
      id: draft.id,
      text: '稍后再试',
      expectedRevision: 0,
      operationId: 'save-1',
    );
    await expectLater(save(), throwsA(isA<DiaryException>()));
    expect((await save()).revision, 1);
  });
}

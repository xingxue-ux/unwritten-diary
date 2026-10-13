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
      expectedRevision: draft.revision,
      operationId: 'save-1',
    );
    final repeat = await api.saveDraft(
      id: draft.id,
      text: '第一句',
      expectedRevision: draft.revision,
      operationId: 'save-1',
    );
    expect(repeat.revision, first.revision);
    expect(
      () => api.saveDraft(
        id: draft.id,
        text: '第二句',
        expectedRevision: draft.revision,
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
      expectedRevision: draft.revision,
      operationId: 'save-1',
    );
    await expectLater(save(), throwsA(isA<DiaryException>()));
    expect((await save()).revision, 2); // 新建就是 1，保存后 +1
  });

  test('升级检索不伪造 hybrid_ready（Mock 没有检索会话）', () async {
    final api = MockDiaryApi(clock: () => now);

    // Mock 只覆盖 F0 记录路径：它没有 `search.start` 建出来的会话，也没有任何
    // 语义/融合能力。所以升级只能如实拒绝——绝不会返回一份编造的 hybrid_ready，
    // 也不会替不存在的会话编一份 keyword_ready 快照。断言必须反映这个真实行为。
    await expectLater(
      api.upgradeSearch(sessionId: 'mock-session'),
      throwsA(isA<UnsupportedError>()),
    );

    // 能力声明同样是诚实的：没接的检索能力不出现在 capabilities 里。
    final capabilities = (await api.snapshot()).coreInfo.capabilities;
    expect(capabilities, isNot(contains('search.upgrade')));
    expect(capabilities, isNot(contains('search.start')));
  });

  test('索引重建在 Mock 里明确拒绝，不伪造成功', () {
    // Mock 没有队列、没有索引，也不写盘。它做不到 `indexes.rebuild` 就如实报未实现，
    // 绝不返回一个假的 jobId 让界面以为排上了。`noSuchMethod` 是同步抛的，所以用
    // 闭包断言，而不是 await 一个永远不会返回的 Future。
    final api = MockDiaryApi(clock: () => now);
    expect(
      () => api.rebuildIndexes(scope: 'all', reason: 'manual', operationId: 'op-1'),
      throwsA(isA<UnsupportedError>()),
    );
    // 契约声明的 scope 编码在 Mock 里也不被「顺便实现」成别的东西。
    expect(
      () => api.rebuildIndexes(
        scope: 'sources:src_0000',
        reason: 'manual',
        operationId: 'op-2',
      ),
      throwsA(isA<UnsupportedError>()),
    );
  });
}

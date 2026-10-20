import 'dart:io';

import 'package:diary_api/diary_api.dart';
// 两个包有同名枚举（CaptureState / AuthorType 等），这里只要适配层的类。
import 'package:diary_bridge/diary_bridge.dart' show BridgeDiaryApi;
import 'package:test/test.dart';

import 'expected_schema.dart';

/// 适配层的端到端测试：**只经契约接口**（`DiaryApi`）走真核心 + 真 SQLite。
///
/// 这条路径就是前端 `CaptureController` 用的那条：`open` → `createDraft` →
/// `saveDraft` → `commit` → `getCapture` → `listCaptures`。测试不碰任何桥接类型，
/// 所以一旦适配层漏映射或映射错字段，这里就会红。
///
/// 运行前先构建宿主产物：
///   cargo build --release            （仓库根目录）
///   dart test                        （本包目录）
void main() {
  late Directory workDir;
  late BridgeDiaryApi api;

  setUpAll(() {
    final path = File(
      Platform.environment['DIARY_BRIDGE_LIB'] ?? _defaultLibraryPath(),
    ).absolute.path;
    expect(
      File(path).existsSync(),
      isTrue,
      reason: '找不到桥接产物：$path。先在仓库根目录跑 cargo build --release',
    );

    workDir = Directory.systemTemp.createTempSync('diary_adapter_test');
    api = BridgeDiaryApi(
      coreLibraryPath: path,
      libraryPath: '${workDir.path}/library.sqlite',
      timeZone: 'Asia/Shanghai',
      utcOffsetMinutes: 480,
    );
  });

  tearDownAll(() async {
    // 先关会话再删目录：Windows 上文件被打开着是删不掉的。
    await api.close();
    try {
      workDir.deleteSync(recursive: true);
    } on FileSystemException {
      // 删除失败不该让测试失败；临时目录留给系统清理。
    }
  });

  test('经契约接口完成一条记录（真核心 + 真 SQLite）', () async {
    final opened = await api.open();
    expect(opened.coreInfo.apiVersion, '1.0');
    // 版本号只在这一处维护（见 expected_schema.dart 的说明）。
    expect(opened.coreInfo.dataSchemaVersion, expectedDataSchemaVersion);
    expect(opened.coreInfo.capabilities, contains('captures.commit'));
    expect(opened.captureCount, 0);
    expect(opened.recovery.notes, isEmpty, reason: '空库里没有需要恢复的东西');

    final draft = await api.createDraft(operationId: 'adapter-create');
    expect(draft.state, CaptureState.draft);
    // 核心的初始 revision 是 1（契约只说它是「乐观锁版本」，没规定起点）。
    // 顺带一提：`diary_mock` 从 0 开始，两者不一致——见 issue 里的记录。
    expect(draft.revision, 1);
    expect(draft.dayKey, isNotEmpty);
    expect(draft.utcOffsetMinutes, 480);

    final saved = await api.saveDraft(
      id: draft.id,
      text: '适配层写进来的第一句。',
      expectedRevision: draft.revision,
      operationId: 'adapter-save',
    );
    expect(saved.durable, isTrue);
    expect(saved.revision, draft.revision + 1);

    final committed = await api.commit(
      id: draft.id,
      expectedRevision: saved.revision,
      operationId: 'adapter-commit',
    );
    expect(committed.capture.state, CaptureState.committed);
    expect(committed.capture.draftText, '适配层写进来的第一句。');
    // 原始文字版本也要翻过来：桥接侧的 AuthorType.import_ 要映射成契约的 import。
    expect(committed.originalTextRevision, isNotNull);
    expect(committed.originalTextRevision!.authorType, AuthorType.user);
    expect(committed.originalTextRevision!.text, '适配层写进来的第一句。');

    final again = await api.getCapture(draft.id);
    expect(again.revision, committed.capture.revision);
    expect(again.state, CaptureState.committed);

    final page = await api.listCaptures(pageSize: 10);
    expect(page.captures.map((capture) => capture.id), contains(draft.id));

    // 快照里的三个数字要真的从库里读出来，而不是默认值。
    final after = await api.snapshot();
    expect(after.captureCount, 1, reason: '记录数必须真的数出来');
    expect(after.lastEventSequence, greaterThan(0), reason: '提交会产生事件');
    expect(after.pendingJobCount, greaterThanOrEqualTo(0));
  });

  test('导入取消与录音恢复交接到真实核心', () async {
    final bridge = BridgeDiaryApi(
      coreLibraryPath: File(
        Platform.environment['DIARY_BRIDGE_LIB'] ?? _defaultLibraryPath(),
      ).absolute.path,
      libraryPath: '${workDir.path}/handoff.sqlite',
      timeZone: 'Asia/Shanghai',
      utcOffsetMinutes: 480,
    );
    try {
      final opened = await bridge.open();
      expect(opened.coreInfo.capabilities, contains('imports.cancel'));
      expect(opened.coreInfo.capabilities, contains('recordings.prepare'));
      final draft = await bridge.createDraft(operationId: 'handoff-draft');
      final import = await bridge.prepareImport(
        captureId: draft.id,
        displayName: 'unfinished.txt',
        operationId: 'handoff-import',
      );
      expect(
        (await bridge.importStatus(import.importId)).state,
        ImportState.prepared,
      );
      await bridge.cancelImport(import.importId);
      await expectLater(
        bridge.importStatus(import.importId),
        throwsA(isA<DiaryException>()),
      );

      final recording = await bridge.prepareRecording(
        captureId: draft.id,
        operationId: 'handoff-recording',
      );
      expect(recording.stagingTicket, isNotEmpty);
      final recovery = await bridge.recoverRecording(
        recordingId: recording.recordingId,
      );
      expect(recovery.recordingId, recording.recordingId);
      expect(recovery.closedSegmentIndexes, isEmpty);
    } finally {
      await bridge.close();
    }
  });

  test('开库失败抛契约异常，且不留下已释放的会话', () async {
    // 回归：审查发现 `open()` 没走 `_translate`，会把原始 BridgeError 泄漏给
    // 只依赖 DiaryApi 的调用方；而且它 dispose 旧会话后没有清空引用，开库失败
    // 时后续方法会拿到一个死句柄。
    final broken = BridgeDiaryApi(
      coreLibraryPath: File(
        Platform.environment['DIARY_BRIDGE_LIB'] ?? _defaultLibraryPath(),
      ).absolute.path,
      // 目录不存在，SQLite 建不了库文件。
      libraryPath: '${workDir.path}/不存在的目录/library.sqlite',
      timeZone: 'Asia/Shanghai',
      utcOffsetMinutes: 480,
    );

    await expectLater(
      broken.open(),
      throwsA(
        isA<DiaryException>(),
        // 明确不是桥接的原始异常类型（isA<DiaryException> 已经蕴含，写出来是为了
        // 让「回归的是什么」在测试里看得见）。
      ),
    );

    // 失败之后这个适配器必须处于「没打开」的状态，而不是拿着死句柄。
    await expectLater(
      broken.createDraft(operationId: 'op-after-failed-open'),
      throwsA(
        isA<DiaryException>().having(
          (error) => error.code,
          'code',
          DiaryErrorCode.invalidState,
        ),
      ),
    );
  });

  test('正文修订经契约适配层保存并只返回当前正文与上一版', () async {
    await api.open();
    final draft = await api.createDraft(operationId: 'adapter-revise-create');
    final saved = await api.saveDraft(
      id: draft.id,
      text: '散步时看见旧路灯。',
      expectedRevision: draft.revision,
      operationId: 'adapter-revise-draft',
    );
    final committed = await api.commit(
      id: draft.id,
      expectedRevision: saved.revision,
      operationId: 'adapter-revise-commit',
    );
    final sourceId = committed.originalTextRevision!.sourceId;
    expect(await api.previousSourceRevision(sourceId), isNull);
    await api.extractSource(sourceId);

    final revision = await api.reviseText(
      sourceId: sourceId,
      text: '散步时看见新路灯。',
      expectedRevision: committed.capture.revision,
      operationId: 'adapter-revise-text',
    );
    expect(revision.text, '散步时看见新路灯。');
    expect((await api.previousSourceRevision(sourceId))?.text, '散步时看见旧路灯。');
    expect((await api.extractedContent(sourceId))?.text, '散步时看见新路灯。');
    expect((await api.getCapture(draft.id)).draftText, '散步时看见新路灯。');
  });

  test('错误码翻成契约枚举，不会变成 unknown', () async {
    await expectLater(
      api.getCapture('cap_不存在'),
      throwsA(
        isA<DiaryException>()
            .having((error) => error.code, 'code', DiaryErrorCode.notFound)
            .having((error) => error.retryable, 'retryable', false),
      ),
    );
  });

  test('检索会话经契约接口完成搜索、翻页、快照与取消', () async {
    // Seed both pages here: this test must also pass when run by --plain-name.
    for (final suffix in ['甲', '乙']) {
      final draft = await api.createDraft(
        operationId: 'adapter-search-create-$suffix',
      );
      final saved = await api.saveDraft(
        id: draft.id,
        text: '独立检索标记$suffix。',
        expectedRevision: draft.revision,
        operationId: 'adapter-search-save-$suffix',
      );
      await api.commit(
        id: draft.id,
        expectedRevision: saved.revision,
        operationId: 'adapter-search-commit-$suffix',
      );
    }

    final first = await api.startSearch(
      request: const SearchRequest(
        query: '独立检索标记',
        mode: SearchMode.keyword,
        pageSize: 1,
      ),
      queryRevision: 42,
    );
    expect(first.queryRevision, 42);
    expect(first.phase, SearchPhase.keywordReady);
    expect(first.results, hasLength(1));
    expect(first.cursor, isNotNull);
    expect(first.results.single.snippet, contains('独立检索标记'));
    expect(first.results.single.matchedBy, contains(MatchedBy.keyword));
    // This hit is text written directly in a capture. It has no original asset
    // or source revision to locate; the adapter must preserve that null value.
    expect(first.results.single.sourceId, isNull);
    expect(first.results.single.locator, isNull);

    await expectLater(
      api.nextPage(sessionId: first.sessionId, cursor: 'invalid-cursor'),
      throwsA(isA<CursorExpiredException>()),
    );

    final second = await api.nextPage(
      sessionId: first.sessionId,
      cursor: first.cursor,
    );
    expect(second.results, hasLength(1));
    expect(second.results.single.hitId, isNot(first.results.single.hitId));
    expect(second.phase, SearchPhase.done);
    expect((await api.searchSnapshot(first.sessionId)).phase, SearchPhase.done);

    final cancellable = await api.startSearch(
      request: const SearchRequest(
        query: '独立检索标记',
        mode: SearchMode.keyword,
        pageSize: 1,
      ),
      queryRevision: 43,
    );
    await api.cancelSearch(cancellable.sessionId);
    expect(
      (await api.searchSnapshot(cancellable.sessionId)).phase,
      SearchPhase.cancelled,
    );
  });

  test('混合会话的升级经适配层转发并如实返回阶段', () async {
    await api.open();
    final draft = await api.createDraft(operationId: 'adapter-upgrade-create');
    final saved = await api.saveDraft(
      id: draft.id,
      text: '升级检索的独立文字。',
      expectedRevision: draft.revision,
      operationId: 'adapter-upgrade-save',
    );
    await api.commit(
      id: draft.id,
      expectedRevision: saved.revision,
      operationId: 'adapter-upgrade-commit',
    );
    final opened = await api.snapshot();
    expect(opened.coreInfo.capabilities, contains('search.upgrade'));
    final keyword = await api.startSearch(
      request: const SearchRequest(query: '升级检索', mode: SearchMode.hybrid),
      queryRevision: 52,
    );
    expect(keyword.phase, SearchPhase.keywordReady);
    final upgraded = await api.upgradeSearch(sessionId: keyword.sessionId);
    expect(upgraded.sessionId, keyword.sessionId);
    expect(upgraded.queryRevision, keyword.queryRevision);
    expect(
      upgraded.phase,
      anyOf(SearchPhase.keywordReady, SearchPhase.hybridReady),
    );
    if (upgraded.phase == SearchPhase.keywordReady) {
      expect(upgraded.warnings, isNotEmpty);
    }
  });

  test('没接的方法明确报错，且不在能力清单里', () async {
    final snapshot = await api.snapshot();
    expect(snapshot.coreInfo.capabilities, contains('captures.list'));
    expect(snapshot.coreInfo.capabilities, contains('search.start'));
    expect(snapshot.coreInfo.capabilities, isNot(contains('indexes.status')));
    // **语义**重建这一条接了（设置页要在换过模型之后触发「重建语义索引」）：
    // 核心入口 → 桥接 → 适配层 → 能力项都在。
    expect(
      snapshot.coreInfo.capabilities,
      contains('indexes.rebuildSemantic'),
    );
    // **关键词**那条照旧没接：核心与桥接都通了（能力项进了 wired_capabilities），
    // 但适配层的 `_wiredMethods` 仍然不含它，所以这里必须是「没有」。
    expect(snapshot.coreInfo.capabilities, isNot(contains('indexes.rebuild')));
  });

  test('到期任务入口经 DiaryApi 返回真实核心摘要', () async {
    final snapshot = await api.snapshot();
    expect(snapshot.coreInfo.capabilities, contains('jobs.nextWakeup'));
    expect(snapshot.coreInfo.capabilities, contains('jobs.runDue'));
    final before = await api.nextWakeup();
    final result = await api.runDueJobs(budgetMs: 0, trigger: 'adapter-wake');
    expect(result.trigger, 'adapter-wake');
    expect(result.processed, 0);
    expect(result.startedAt, isA<DateTime>());
    expect(result.nextWakeup, before);
  });

  test('语义索引重建经契约接口排得上，重放拿到同一个 jobId', () async {
    await api.open();
    final ticket = await api.rebuildSemanticIndex(
      reason: '适配层端到端',
      operationId: 'adapter-semantic-1',
    );
    expect(ticket.jobId, isNotEmpty, reason: '拿到的是真实任务 id，不是编出来的');

    // 同一个 (reason, operationId)：同一条任务。
    final replay = await api.rebuildSemanticIndex(
      reason: '适配层端到端',
      operationId: 'adapter-semantic-1',
    );
    expect(replay.jobId, ticket.jobId);

    // 换一个 operationId：语义重建总是整库，业务键让它命中活着的那一条。
    final other = await api.rebuildSemanticIndex(
      reason: '另一个入口',
      operationId: 'adapter-semantic-2',
    );
    expect(other.jobId, ticket.jobId, reason: '重复排队不产生第二份工作');

    // 同一个 operationId 换了原因：契约 1.2 的幂等冲突，如实翻成契约错误码
    // （不是悄悄返回旧任务，也不是崩掉）。
    await expectLater(
      api.rebuildSemanticIndex(
        reason: '换了原因',
        operationId: 'adapter-semantic-1',
      ),
      throwsA(
        isA<DiaryException>().having(
          (error) => error.code,
          'code',
          DiaryErrorCode.idempotencyConflict,
        ),
      ),
    );
  });

  test('风格 CRUD 与乐观锁经契约接口可用', () async {
    await api.open();

    // #114：默认风格是真实行；保存/读取/乐观锁都经适配层映射。
    final styles = await api.listStyles();
    expect(styles.map((profile) => profile.id), contains('sty_default'));
    final saved = await api.saveStyle(
      StyleProfile(
        id: 'sty_adapter',
        label: '适配层的风格',
        revision: 0,
        updatedAt: DateTime.now().toUtc(),
        lengthPreference: '简短',
        tone: '平静',
        examples: const ['像跟朋友说话'],
        bannedExpressions: const ['治愈一切'],
      ),
    );
    expect(saved.revision, 1);
    expect(saved.id, 'sty_adapter');
    expect(saved.tone, '平静');
    final fetched = await api.getStyle('sty_adapter');
    expect(fetched, saved);

    await expectLater(
      api.saveStyle(
        StyleProfile(
          id: 'sty_adapter',
          label: '适配层的风格',
          revision: 0,
          updatedAt: DateTime.now().toUtc(),
        ),
        expectedRevision: 0,
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
}

String _defaultLibraryPath() {
  if (Platform.isLinux) {
    return '../../target/release/libdiary_bridge.so';
  }
  if (Platform.isWindows) {
    return r'..\..\target\x86_64-pc-windows-msvc\release\diary_bridge.dll';
  }
  if (Platform.isMacOS) {
    return '../../target/release/libdiary_bridge.dylib';
  }
  throw UnsupportedError('未知平台：${Platform.operatingSystem}');
}

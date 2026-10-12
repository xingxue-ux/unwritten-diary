import 'dart:convert';
import 'dart:io';

import 'package:crypto/crypto.dart';
import 'package:diary_bridge/diary_bridge.dart';
import 'package:flutter_rust_bridge/flutter_rust_bridge_for_generated.dart';
import 'package:test/test.dart';

/// issue #70 后半的桥接端到端：Dart → `indexes.rebuild(scope, reason, operationId)`
/// → 真核心 → 队列。
///
/// 覆盖三件在 Dart 侧才看得出来的事：
/// 1. **异步**：入口只排一条 `index` 任务（整库任务的 targetIds 为空），返回 jobId，
///    不做同步重建；
/// 2. **scope 编码**：`all` / `sources:<id>[,<id>]*` 与它们的错误码
///    （`invalid_argument` / `not_found` / `idempotency_conflict`）；
/// 3. **端到端**：排的任务能被 `runDueJobs` 跑完，`indexStatus` 报关键词就绪，
///    重复调用同一个 operationId 不产生第二条任务。
///
/// 每个用例开一个干净的资料库，避免互相干扰。
void main() {
  late Directory root;

  setUpAll(() async {
    final path = File(
      Platform.environment['DIARY_BRIDGE_LIB'] ?? _defaultLibraryPath(),
    ).absolute.path;
    expect(
      File(path).existsSync(),
      isTrue,
      reason: '找不到桥接产物：$path。先在仓库根目录跑 cargo build --release',
    );
    await RustLib.init(externalLibrary: ExternalLibrary.open(path));
    root = Directory.systemTemp.createTempSync('diary_index_rebuild_test');
  });

  tearDownAll(() {
    try {
      root.deleteSync(recursive: true);
    } on FileSystemException {
      // 删不掉不影响结论。
    }
  });

  late Directory workDir;
  late BridgeSession session;

  setUp(() async {
    workDir = Directory.systemTemp.createTempSync('diary_index_rebuild_case');
    session = await BridgeSession.open(
      libraryPath: '${workDir.path}/library.sqlite',
    );
  });

  tearDown(() {
    session.dispose();
    try {
      workDir.deleteSync(recursive: true);
    } on FileSystemException {
      // 删不掉不影响结论。
    }
  });

  /// 导入一段文本，返回来源 id。**不手动提取**：提取要走任务驱动。
  Future<String> addMaterial(String name, String text, String operation) async {
    final draft = await session.createDraft(
      timeZone: 'Asia/Shanghai',
      utcOffsetMinutes: 480,
      operationId: '$operation-create',
    );
    final ticket = await session.prepareImport(
      captureId: draft.id,
      displayName: name,
      mimeHint: 'text/plain',
      origin: ImportOrigin.picker,
      operationId: '$operation-prepare',
    );
    final bytes = utf8.encode(text);
    File(ticket.stagingTicket).writeAsBytesSync(bytes);
    await session.finishImport(
      importId: ticket.importId,
      stagingTicket: ticket.stagingTicket,
      manifest: ImportManifest(
        copiedBytes: bytes.length,
        sha256: sha256.convert(bytes).toString(),
        detectedMime: 'text/plain',
        originalName: name,
      ),
    );
    return (await session.getCapture(captureId: draft.id))
        .orderedSourceIds
        .first;
  }

  Future<int> searchHits(String query) async {
    final snapshot = await session.startSearch(
      request: SearchRequest(
        query: query,
        mode: SearchMode.keyword,
        filters: const SearchFilters(
          kinds: [],
          includeOldDiaryVersions: false,
          includeTrashed: false,
        ),
        pageSize: 20,
      ),
      queryRevision: 1,
    );
    return snapshot.results.length;
  }

  Future<List<String>> indexJobTargets() async {
    final jobs = await session.listJobs(limit: 50);
    return jobs
        .where((job) => job.kind == 'index')
        .map((job) => job.targetIds.join(','))
        .toList();
  }

  /// 跑多轮驱动，直到没有到期任务也没有在等前置的任务。
  Future<int> driveUntilIdle() async {
    for (var round = 1; round <= 8; round++) {
      final summary = await session.runDueJobs(
        budgetMs: 60000,
        trigger: 'manual',
      );
      if (summary.dueBacklog == 0 && summary.waitingDependency == 0) {
        return round;
      }
    }
    fail('驱动跑了 8 轮还有到期或等待的任务，链没走完');
  }

  test('能力清单里有 indexes.rebuild', () async {
    final info = await session.info();
    expect(info.capabilities, contains('indexes.rebuild'));
  });

  test('整库重建只排一条任务，重复调用同一 operationId 不产生第二条', () async {
    await addMaterial('整库.txt', '整库重建的正文', 'op-all');

    final job = await session.rebuildIndexes(
      scope: 'all',
      reason: 'manual',
      operationId: 'op-rebuild-all',
    );
    expect(job.kind, 'index');
    expect(job.targetIds, isEmpty, reason: '整库用空目标表示');
    expect(job.state, JobState.queued);

    final replay = await session.rebuildIndexes(
      scope: 'all',
      reason: 'manual',
      operationId: 'op-rebuild-all',
    );
    expect(replay.id, job.id, reason: '重复调用返回同一条任务');
    expect(
      (await indexJobTargets()).where((targets) => targets.isEmpty),
      hasLength(1),
      reason: '重复调用不产生第二条整库任务',
    );

    // 同一个 operationId 换了 scope 或原因要报幂等冲突（契约 1.2 节）。
    await expectLater(
      session.rebuildIndexes(
        scope: 'all',
        reason: '换了原因',
        operationId: 'op-rebuild-all',
      ),
      throwsA(
        isA<BridgeError>().having(
          (error) => error.code,
          'code',
          'idempotency_conflict',
        ),
      ),
    );
  });

  test('整库重建排的任务被驱动跑完，indexStatus 报关键词就绪', () async {
    const text = '重建前也能搜到的正文\n\n第二段';
    final sourceId = await addMaterial('链条.txt', text, 'op-chain');

    final job = await session.rebuildIndexes(
      scope: 'all',
      reason: '因为改过正文',
      operationId: 'op-rebuild-chain',
    );
    expect(job.state, JobState.queued, reason: '入口只排任务，不同步重建');

    // 整库任务的前置是「没有活着的 extract」：预算 0 的一轮只跑闸门，它落等待态。
    final idle = await session.runDueJobs(budgetMs: 0, trigger: 'manual');
    expect(idle.processed, 0);
    final waiting = await session.getJob(jobId: job.id);
    expect(waiting.state, JobState.waitingDependency);
    expect(waiting.errorCode, 'waiting_extract');
    expect(waiting.attemptCount, 0, reason: '等前置不是失败，不扣额度');

    await driveUntilIdle();

    final done = await session.getJob(jobId: job.id);
    expect(done.state, JobState.succeeded);
    expect(await searchHits('正文'), greaterThan(0));
    final index = await session.indexStatus(sourceScope: [sourceId]);
    expect(index.keywordIndexReady, isTrue);
  });

  test('指定来源的重建：提取未完成时落 waiting_dependency，前置一满足就跑', () async {
    final sourceId = await addMaterial('来源.txt', '指定来源的正文', 'op-src');

    final job = await session.rebuildIndexes(
      scope: 'sources:$sourceId',
      reason: 'manual',
      operationId: 'op-rebuild-src',
    );
    expect(job.targetIds, [sourceId]);

    // 预算 0 的一轮只跑依赖闸门：提取还没跑完 → 落等待态，不消耗额度。
    final idle = await session.runDueJobs(budgetMs: 0, trigger: 'manual');
    expect(idle.processed, 0);
    final waiting = await session.getJob(jobId: job.id);
    expect(waiting.state, JobState.waitingDependency);
    expect(waiting.errorCode, 'waiting_extract');
    expect(waiting.attemptCount, 0, reason: '等前置不是失败，不扣额度');
    expect(waiting.requiresUserAction, isFalse);

    await driveUntilIdle();
    final done = await session.getJob(jobId: job.id);
    expect(done.state, JobState.succeeded);
    expect(
      (await session.indexStatus(sourceScope: [sourceId])).keywordIndexReady,
      isTrue,
    );
  });

  test('scope 编码的错误码：空列表/非法字符 invalid_argument，未知来源 not_found',
      () async {
    for (final scope in [
      '',
      'ALL',
      'all ',
      'sources',
      'sources:',
      'sources:src_a,,src_b',
      'sources:src has space',
      'src_00000000000000000000000000000000',
    ]) {
      await expectLater(
        session.rebuildIndexes(scope: scope, reason: 'manual', operationId: 'op-bad-$scope'),
        throwsA(
          isA<BridgeError>().having(
            (error) => error.code,
            'code',
            'invalid_argument',
          ),
        ),
        reason: 'scope「$scope」应报 invalid_argument',
      );
    }

    await expectLater(
      session.rebuildIndexes(
        scope: 'sources:src_00000000000000000000000000000000',
        reason: 'manual',
        operationId: 'op-missing',
      ),
      throwsA(
        isA<BridgeError>().having(
          (error) => error.code,
          'code',
          'not_found',
        ),
      ),
    );

    // 一条任务都不该被排下去（只有非法输入，没有导入）。
    expect(await indexJobTargets(), isEmpty);
  });

  test('关键字字母表里的来源 id 会排序成规范形，顺序无语义', () async {
    final first = await addMaterial('一.txt', 'one', 'op-one');
    final second = await addMaterial('二.txt', 'two', 'op-two');

    final job = await session.rebuildIndexes(
      scope: 'sources:$second,$first',
      reason: 'manual',
      operationId: 'op-order',
    );
    expect(job.targetIds, [first, second], reason: '解析时排成规范形');

    final replay = await session.rebuildIndexes(
      scope: 'sources:$first,$second',
      reason: 'manual',
      operationId: 'op-order',
    );
    expect(replay.id, job.id, reason: '顺序不同的等价 scope 是同一次操作');
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
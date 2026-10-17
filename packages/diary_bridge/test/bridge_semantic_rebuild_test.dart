import 'dart:io';

import 'package:diary_bridge/diary_bridge.dart';
import 'package:flutter_rust_bridge/flutter_rust_bridge_for_generated.dart';
import 'package:test/test.dart';

/// 语义索引重建的桥接端到端：Dart → `indexes.rebuildSemantic(reason, operationId)`
/// → 真核心 → 队列（契约第 4.4 节）。
///
/// 覆盖四件在 Dart 侧才看得出来的事：
/// 1. **能力项**：`indexes.rebuildSemantic` 进了 `wired_capabilities()`；
/// 2. **异步 + 整库**：只排一条 `semantic_index` 任务（`targetIds` 为空），返回它，
///    不做同步重建；
/// 3. **幂等**：同一个 `(reason, operationId)` 重放同一条；换 operationId 也命中
///    业务键（「队列里至多一条活着的语义重建」）；同一个 operationId 换原因报
///    `idempotency_conflict`；
/// 4. **等模型配置**（本机没配模型时）：跑一轮驱动后任务落 `waiting_configuration`
///    + `errorCode = waiting_model`，**不是失败**、不消耗重试额度，也不把
///    `nextWakeup` 拖成「现在」。
///
/// 「真的建出向量」那一段需要注入嵌入器，Dart 侧没有这个入口：它由核心的
/// `semantic_job_test.rs` 用 stub 嵌入器覆盖。这里只钉「排得上、等得对、不重复」。
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
    root = Directory.systemTemp.createTempSync('diary_semantic_rebuild_test');
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
    workDir = Directory.systemTemp.createTempSync('diary_semantic_rebuild_case');
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

  /// 这个进程配了本地模型吗？配了就跳过「等模型配置」那条断言（与核心测试同一口径）。
  bool modelConfigured() =>
      Platform.environment['DIARY_MODEL_DIR'] != null ||
      Platform.environment['DIARY_ORT_DYLIB'] != null ||
      Platform.environment['ORT_DYLIB_PATH'] != null;

  Future<List<Job>> semanticJobs() async {
    final jobs = await session.listJobs(limit: 50);
    return jobs.where((job) => job.kind == 'semantic_index').toList();
  }

  test('能力清单里有 indexes.rebuildSemantic', () async {
    final info = await session.info();
    expect(info.capabilities, contains('indexes.rebuildSemantic'));
    // 关键词那条照旧在（两条索引路各有各的入口）。
    expect(info.capabilities, contains('indexes.rebuild'));
  });

  test('只排一条整库的 semantic_index 任务，重放与换 operationId 都不产生第二条', () async {
    final job = await session.rebuildSemanticIndex(
      reason: '换模型后重建',
      operationId: 'op-semantic-1',
    );
    expect(job.kind, 'semantic_index');
    expect(job.targetIds, isEmpty, reason: '语义重建总是整库（代次是全局的）');
    expect(job.state, JobState.queued);
    expect(job.nextAttemptAt, isNull, reason: '入队即到期');
    expect(job.progress, isNull, reason: '还没跑过就没有进度可报');

    // 同一个 operationId + 同一个原因：同一条（读的是它此刻的真实状态）。
    final replay = await session.rebuildSemanticIndex(
      reason: '换模型后重建',
      operationId: 'op-semantic-1',
    );
    expect(replay.id, job.id);

    // 换一个 operationId：业务键 `(semantic_index, [], NULL)` 命中活着的那一条。
    final other = await session.rebuildSemanticIndex(
      reason: '设置页另一个入口',
      operationId: 'op-semantic-2',
    );
    expect(other.id, job.id, reason: '重复排队不产生第二份工作');

    // 同一个 operationId 换了原因：契约 1.2 的幂等冲突。
    await expectLater(
      session.rebuildSemanticIndex(
        reason: '换了原因',
        operationId: 'op-semantic-1',
      ),
      throwsA(
        isA<BridgeError>().having(
          (error) => error.code,
          'code',
          'idempotency_conflict',
        ),
      ),
    );

    expect(await semanticJobs(), hasLength(1));
  });

  test('没配模型时：跑一轮落 waiting_configuration，不失败、不拖 nextWakeup', () async {
    final job = await session.rebuildSemanticIndex(
      reason: '换模型后重建',
      operationId: 'op-semantic-model',
    );

    final summary = await session.runDueJobs(budgetMs: 60000, trigger: 'manual');
    final waiting = await session.getJob(jobId: job.id);

    if (modelConfigured()) {
      // 配了模型的机器上跑不到「等配置」这条路：只钉「没有谎报成 waiting 以外的东西」。
      expect(waiting.state, isNot(JobState.failed));
      return;
    }
    expect(waiting.state, JobState.waitingConfiguration);
    expect(waiting.errorCode, 'waiting_model');
    expect(waiting.attemptCount, 0, reason: '等配置不是失败，不扣额度');
    expect(waiting.requiresUserAction, isFalse);
    expect(summary.failed, 0, reason: '等配置不算这一轮的失败');

    // 状态摘要里看得见「等配置」，而且它不把下次唤醒拖成「现在」。
    final status = await session.jobStatus();
    expect(status.waitingConfiguration, 1);
    expect(status.waitingDependency, 0);
    expect(summary.processed, 0, reason: '没有模型就一块都不该算');
    expect(
      summary.nextWakeup,
      isNull,
      reason: '等配置不是时间问题：配置变化不是「到点唤醒」',
    );

    // 索引状态如实说清语义这一路为什么没就绪（不是含糊的「未就绪」）。
    final indexStatus = await session.indexStatus();
    expect(indexStatus.semanticIndexReady, isFalse);
    expect(
      indexStatus.reasons.any((reason) => reason.contains('还没有配置本地模型')),
      isTrue,
      reason: '要说清是「没配」：${indexStatus.reasons}',
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
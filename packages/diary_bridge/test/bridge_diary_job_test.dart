import 'dart:io';

import 'package:diary_bridge/diary_bridge.dart';
import 'package:flutter_rust_bridge/flutter_rust_bridge_for_generated.dart';
import 'package:test/test.dart';

/// B4b 的日记生成：**原始会话**（`BridgeSession`）走真核心，覆盖适配层还没有接的
/// `jobs.*` 那一段——票据冻结输入后，没有配置任何服务时任务停在
/// `waiting_configuration`，不发请求、不消耗尝试、不产出假版本。
///
/// 运行前先构建宿主产物：
///   cargo build --release            （仓库根目录）
///   dart test                        （本包目录）
void main() {
  late Directory workDir;
  late BridgeSession session;

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
    workDir = Directory.systemTemp.createTempSync('diary_diary_job_test');
    session = await BridgeSession.open(
      libraryPath: '${workDir.path}/library.sqlite',
    );
  });

  tearDownAll(() {
    session.dispose();
    try {
      workDir.deleteSync(recursive: true);
    } on FileSystemException {
      // 删除失败不该让测试失败；临时目录留给系统清理。
    }
  });

  test('生成票据冻结输入；没有服务时任务等配置且不产出任何版本', () async {
    final draft = await session.createDraft(
      occurredAt: DateTime.utc(2026, 10, 16, 2),
      timeZone: 'Asia/Shanghai',
      utcOffsetMinutes: 480,
      operationId: 'op-j-create',
    );
    await session.saveDraft(
      captureId: draft.id,
      text: '今天去公园散步，风有点大。',
      expectedRevision: draft.revision,
      operationId: 'op-j-save',
    );
    await session.commit(
      captureId: draft.id,
      expectedRevision: draft.revision + 1,
      operationId: 'op-j-commit',
    );

    final ticket = await session.generateDiary(
      dayKey: '2026-10-16',
      reason: DiaryGenerationReason.manual,
      operationId: 'op-j-gen',
    );
    expect(ticket.jobId, isNotEmpty);
    expect(ticket.sourceSnapshotId, isNotEmpty);

    // 一个服务都没配：领取前的配置闸门挡住它，一条真实请求都不发。
    final summary = await session.runDueJobs(budgetMs: 5000, trigger: 'test');
    expect(summary.processed, 0, reason: '被闸门挡住的任务不算「跑过」');
    final job = await session.getJob(jobId: ticket.jobId);
    expect(job.state, JobState.waitingConfiguration);
    expect(job.errorCode, 'provider_not_configured');
    expect(job.requiresUserAction, isTrue);
    expect(job.attemptCount, 0, reason: '等配置不消耗尝试次数');
    expect(job.inputSnapshotHash, isNotEmpty, reason: '任务的输入快照哈希就是那份冻结输入');

    // 没有版本；没有空正文冒充成功。
    final page = await session.listDiaryVersions(dayKey: '2026-10-16', limit: 20);
    expect(page.versions, isEmpty);

    // 这一天的视图：材料统计是真的，当前/候选都为空。
    final day = await session.getDiaryDay(dayKey: '2026-10-16');
    expect(day.materialCounts.captures, 1);
    expect(day.materialCounts.processed, 1);
    expect(day.materialCounts.pending, 0);
    expect(day.materialCounts.failed, 0);
    expect(day.currentVersionId, isNull);
    expect(day.candidateVersionIds, isEmpty);

    // 关键词检索与既有材料完全不受影响（E23）。
    final hits = await session.startSearch(
      request: SearchRequest(
        query: '公园',
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
    expect(hits.results, isNotEmpty, reason: '用户自己写的文字照常搜得到');

    // 同一条 operationId 重试：原票据，不新建、不重复排。
    final again = await session.generateDiary(
      dayKey: '2026-10-16',
      reason: DiaryGenerationReason.manual,
      operationId: 'op-j-gen',
    );
    expect(again.jobId, ticket.jobId);
    expect(again.sourceSnapshotId, ticket.sourceSnapshotId);
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
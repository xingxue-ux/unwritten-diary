import 'dart:convert';
import 'dart:io';

import 'package:crypto/crypto.dart';
import 'package:diary_bridge/diary_bridge.dart';
import 'package:flutter_rust_bridge/flutter_rust_bridge_for_generated.dart';
import 'package:test/test.dart';

/// issue #70 前半的桥接端到端：Dart → `runDueJobs(budgetMs, trigger)` → 真核心。
///
/// 覆盖两件在 Dart 侧才看得出来的事：
/// 1. **依赖闸门**：导入排下 extract + 等提取的 index，`jobStatus` 与 `runDueJobs`
///    的摘要都如实报「在等」（并且有等待态时 `nextWakeup` 非空），而不是排一个
///    注定失败的任务；
/// 2. **链式推进**：**一次** `runDueJobs` 把 extract → index 推到就绪，关键词全程可搜。
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
    root = Directory.systemTemp.createTempSync('diary_jobs_test');
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
    workDir = Directory.systemTemp.createTempSync('diary_jobs_case');
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

  test('依赖闸门：导入后 index 在等 extract，状态与摘要都看得见', () async {
    final sourceId = await addMaterial('闸门.txt', '还没提取完的材料', 'op-gate');

    // 队列里只有 extract 在排队；index 落在等待态，原因写在 errorCode 上。
    final queued = await session.listJobs(states: [JobState.queued], limit: 20);
    expect(queued.map((job) => job.kind), contains('extract'));
    expect(queued.map((job) => job.kind), isNot(contains('index')));

    final waiting = await session.listJobs(
      states: [JobState.waitingDependency],
      limit: 20,
    );
    expect(waiting, hasLength(1));
    expect(waiting.single.kind, 'index');
    expect(waiting.single.targetIds, [sourceId]);
    expect(waiting.single.errorCode, 'waiting_extract');
    expect(waiting.single.attemptCount, 0, reason: '在等前置不是失败，不扣额度');
    expect(waiting.single.requiresUserAction, isFalse);

    final status = await session.jobStatus();
    expect(status.waitingDependency, 1);
    expect(status.failed, 0, reason: '等前置不能被算成失败');

    // 预算 0 的一轮：一条都不跑，但摘要如实报「还有到期的」与「还有在等的」。
    final idle = await session.runDueJobs(budgetMs: 0, trigger: 'startup');
    expect(idle.trigger, 'startup');
    expect(idle.processed, 0, reason: '预算 0 一条都不跑');
    expect(idle.failed, 0, reason: '没跑过的不能算成失败');
    expect(idle.dueBacklog, 1, reason: 'extract 还到期没跑');
    expect(idle.waitingDependency, 1);
    expect(idle.dependencyFailed, 0);
    expect(
      idle.nextWakeup,
      isNotNull,
      reason: '有等待态就必须给出可用的下次唤醒，不能报「没有待办」',
    );
  });

  test('链式推进：extract → index 一次驱动跑完，关键词全程可搜', () async {
    const text = '第一段：妈妈打电话来。\n\n第二段：晚上吃了面。';
    final sourceId = await addMaterial('链条.txt', text, 'op-chain');

    // **只调一次**驱动：闸门在每次领取前重评估，extract 跑完立刻把 index 拉起并在
    // 同一轮跑掉——不把链的完成寄托在平台层肯不肯再叫一次上（#74 评审）。
    final round = await session.runDueJobs(
      budgetMs: 60000,
      trigger: 'foreground',
    );
    expect(round.processed, 2);
    expect(round.succeeded, 2);
    expect(round.waitingDependency, 0);
    expect(round.dependencyFailed, 0);
    expect(round.dueBacklog, 0);
    expect(round.nextWakeup, isNull, reason: '没有待办了');
    expect(round.trigger, 'foreground');
    expect(
      round.processed,
      round.succeeded + round.failed + round.retrying,
      reason: '跑过的任务都要落在终态里',
    );
    expect(
      round.startedAt.isBefore(
        DateTime.now().toUtc().add(const Duration(minutes: 1)),
      ),
      isTrue,
    );

    // 索引跑完，关键词仍然可搜。
    expect(await searchHits('妈妈'), greaterThan(0));

    // 两条任务都在终态里，来源拿到了关键词索引覆盖。
    final indexJobs = await session.listJobs(states: [JobState.succeeded], limit: 20);
    expect(
      indexJobs.where((job) => job.kind == 'index').map((job) => job.targetIds.single),
      contains(sourceId),
    );
    final index = await session.indexStatus(sourceScope: [sourceId]);
    expect(index.keywordIndexReady, isTrue);
    expect(index.totalSegments, 2);
  });
test('DueRunSummary 与核心摘要字段一一对应（防漂移）', () {
    // 两边都是手写/生成的「同一份东西」：核心 `JobRunSummary`（frb 镜像成
    // `third_party/diary_core/model.dart`）与契约 `DueRunSummary`。曾经就漂过
    // （契约那边留着 `remaining` / `budgetExhausted` 这种核心没有的字段），
    // 所以这里直接比字段名：多一个少一个都算漂移。
    final coreFields = _fieldNames(
      File('lib/src/rust/third_party/diary_core/model.dart').readAsStringSync(),
      'JobRunSummary',
    );
    final contractFields = _fieldNames(
      File('../diary_api/lib/src/models_transport.dart').readAsStringSync(),
      'DueRunSummary',
    );
    expect(coreFields, isNotEmpty);
    expect(
      contractFields,
      coreFields.map(_snakeToCamel).toSet(),
      reason: 'DueRunSummary 的字段必须与 JobRunSummary 一一对应',
    );
  });
}

/// 从一段 Dart 源码里抽出某个类的实例字段名。
Set<String> _fieldNames(String source, String className) {
  final start = source.indexOf('class $className {');
  expect(start, greaterThanOrEqualTo(0), reason: '找不到 $className');
  final end = source.indexOf('\n}', start);
  expect(end, greaterThan(start), reason: '$className 没有正常闭合');
  final body = source.substring(start, end);
  final pattern = RegExp(r'^\s*final\s+[^;=]+\s(\w+);\s*$');
  return body
      .split('\n')
      .map(pattern.firstMatch)
      .whereType<RegExpMatch>()
      .map((match) => match.group(1)!)
      .toSet();
}

String _snakeToCamel(String name) {
  final parts = name.split('_');
  return parts.first +
      parts.skip(1).map((part) => part[0].toUpperCase() + part.substring(1)).join();
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
import 'dart:convert';
import 'dart:io';

import 'package:crypto/crypto.dart';
import 'package:diary_api/diary_api.dart';
import 'package:diary_bridge/diary_bridge.dart'
    show BridgeDiaryApi, BridgeSession;
import 'package:test/test.dart';

import 'expected_schema.dart';

/// 这一轮新接上的契约方法，全部**只经 `DiaryApi`** 走真核心 + 真 SQLite：
/// 事件流、资产租约、来源定位、索引状态/重建、确认提醒与备份检查。
///
/// 运行前先构建宿主产物：
///   cargo build --release            （仓库根目录）
///   dart test                        （本包目录）
void main() {
  late Directory workDir;
  late String libraryPath;
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

    workDir = Directory.systemTemp.createTempSync('diary_coverage_test');
    libraryPath = '${workDir.path}/library.sqlite';
    api = BridgeDiaryApi(
      coreLibraryPath: path,
      libraryPath: libraryPath,
      timeZone: 'Asia/Shanghai',
      utcOffsetMinutes: 480,
      // 轮询测试不该等满一秒；核心仍按真实序号补事件。
      eventPollInterval: const Duration(milliseconds: 30),
      // 2 条一批：库里事件不止 2 条，逼出「满批立即取下一页」的路径。
      eventBatchSize: 2,
    );
  });

  tearDownAll(() async {
    await api.close();
    try {
      workDir.deleteSync(recursive: true);
    } on FileSystemException {
      // 删除失败不该让测试失败；临时目录留给系统清理。
    }
  });

  test('events 从游标补读并轮询到新事件', () async {
    final opened = await api.open();
    expect(opened.coreInfo.capabilities, contains('core.events'));

    final draft = await _newCommittedCapture(api, 'events-initial');
    expect(draft.id, isNotEmpty);

    // 从 0 补读：第一条必须是真事件，序号单调、字段来自持久表。
    final first = await api
        .events(fromSequence: 0)
        .first
        .timeout(const Duration(seconds: 5));
    expect(first.eventId, isNotEmpty);
    expect(first.sequence, greaterThan(0));
    expect(first.entityId, isNotEmpty);
    expect(first.emittedAt.isUtc, isTrue);

    // 从「当前最大序号」订阅，再写一条：轮询到的新事件序号更大。
    final snapshot = await api.snapshot();
    final cursor = snapshot.lastEventSequence;
    final pending = api
        .events(fromSequence: cursor)
        .first
        .timeout(const Duration(seconds: 5));
    await Future<void>.delayed(const Duration(milliseconds: 50));
    final created = await api.createDraft(operationId: 'events-new-draft');

    final event = await pending;
    expect(event.sequence, greaterThan(cursor));
    expect(event.entityId, created.id);
    expect(event.type, DomainEventType.captureChanged);
  });

  test('events 按批读取，跨多批仍有序且不重不漏', () async {
    await api.open();
    // 共用库；这里再造几条，保证 2 条/批时要跨多批才能取满 5 条。
    for (var index = 0; index < 4; index++) {
      await _newCommittedCapture(api, 'events-batch-$index');
    }

    final events = await api
        .events(fromSequence: 0)
        .take(5)
        .toList()
        .timeout(const Duration(seconds: 10));
    expect(events, hasLength(5));
    final sequences = events.map((event) => event.sequence).toList();
    expect(sequences, orderedEquals([...sequences]..sort()));
    expect(sequences.toSet(), hasLength(5), reason: '分页不能重复给出同一序号');
  });

  test('events 的游标指向未来时报 CursorExpiredException，不无限等待', () async {
    await api.open();
    await expectLater(
      api.events(fromSequence: 1 << 40).first,
      throwsA(isA<CursorExpiredException>()),
    );
    await expectLater(
      api.events(fromSequence: -1).first,
      throwsA(
        isA<DiaryException>().having(
          (error) => error.code,
          'code',
          DiaryErrorCode.invalidArgument,
        ),
      ),
    );
  });

  test('导入的原件能通过 assets.open 拿到限域租约并释放', () async {
    await api.open();
    final imported = await _importTextAsset(api, 'assets', 'asset 租约的正文。');
    expect(imported.state, ImportState.ready);
    final assetId = imported.assetId;
    expect(assetId, isNotNull);

    final lease = await api.openAsset(
      assetId: assetId!,
      usage: AssetUsage.preview,
    );
    expect(lease.leaseId, isNotEmpty);
    expect(lease.assetId, assetId);
    expect(lease.usage, AssetUsage.preview);
    expect(lease.byteSize, greaterThan(0));
    expect(File(lease.handle).existsSync(), isTrue);
    // 句柄是资料库私有目录里的路径，不是调用方给的任意路径。
    expect(lease.handle, startsWith(workDir.path));

    await api.releaseAsset(lease.leaseId);
    // 重复释放是真实的 not_found，不静默成功。
    await expectLater(
      api.releaseAsset(lease.leaseId),
      throwsA(
        isA<DiaryException>().having(
          (error) => error.code,
          'code',
          DiaryErrorCode.notFound,
        ),
      ),
    );
  });

  test('locateSource 把来源定位解析成可打开的原件', () async {
    await api.open();
    final imported = await _importTextAsset(api, 'locate', '这一段可以被定位。');
    final sourceRef = await _extractJobTarget(api);
    final content = await api.extractSource(sourceRef);
    expect(content.segments, isNotEmpty);

    final location = await api.locateSource(
      ref: content.sourceId,
      locator: content.segments.first.locator,
    );
    expect(location.sourceRef, content.sourceId);
    expect(location.available, isTrue);
    expect(location.assetId, imported.assetId);
    expect(location.locator.sourceRevisionId, content.sourceRevisionId);

    // 不存在的来源也是真实的「打不开」，不是异常。
    final missing = await api.locateSource(
      ref: 'src_does_not_exist',
      locator: content.segments.first.locator,
    );
    expect(missing.available, isFalse);
    expect(missing.reason, isNotEmpty);
  });

  test('indexesStatus 读真实覆盖，rebuildIndexes 排的是可查询的任务', () async {
    await api.open();
    final status = await api.indexesStatus();
    expect(status.coverage, isA<Coverage>());
    expect(status.reasons, isNotEmpty);

    final ticket = await api.rebuildIndexes(
      scope: 'all',
      reason: '适配层覆盖测试',
      operationId: 'adapter-index-rebuild-1',
    );
    expect(ticket.jobId, isNotEmpty);
    final job = await api.getJob(ticket.jobId);
    expect(job.kind, 'index');
    expect(job.id, ticket.jobId);

    // 幂等：同一个 (scope, reason, operationId) 拿到同一条任务。
    final replay = await api.rebuildIndexes(
      scope: 'all',
      reason: '适配层覆盖测试',
      operationId: 'adapter-index-rebuild-1',
    );
    expect(replay.jobId, ticket.jobId);
  });

  test('acknowledgeAttention 真的写进核心', () async {
    await api.open();
    // 不存在的 key 也允许确认（核心只记录「已看过」）；这里验证的是调用链与幂等。
    await api.acknowledgeAttention('job-config:adapter-test');
    await api.acknowledgeAttention('job-config:adapter-test');
  });

  test('inspectBackup 读真备份包的结构与哈希', () async {
    await api.open();
    final package = '${workDir.path}/inspect.diarybak';
    final session = await BridgeSession.open(libraryPath: libraryPath);
    try {
      final report = await session.createBackup(targetPath: package);
      expect(report.packagePath, package);
      expect(File(package).existsSync(), isTrue);
    } finally {
      session.dispose();
    }

    final inspection = await api.inspectBackup(package);
    expect(inspection.packageTicket, package);
    expect(inspection.schemaVersion, expectedDataSchemaVersion);
    expect(inspection.apiVersion, '1.0');
    expect(inspection.byteSize, greaterThan(0));
    expect(inspection.assetCount, greaterThanOrEqualTo(0));
    expect(inspection.issues, isEmpty);
  });

  test('noSuchMethod 的消息列出仍未实现的方法，不再说过时的「只有记录路径」', () {
    expect(
      () => api.getSettings(),
      throwsA(
        isA<UnsupportedError>()
            .having(
              (error) => error.message,
              'message',
              contains('getSettings'),
            )
            .having(
              (error) => error.message,
              'message',
              contains('backups.create'),
            )
            .having(
              (error) => error.message,
              'message',
              contains('setRuntimeState'),
            )
            .having(
              (error) => error.message,
              'message',
              isNot(contains('当前只有记录路径')),
            ),
      ),
    );
  });

  test('能力清单只列真的接上的方法', () async {
    final snapshot = await api.open();
    final capabilities = snapshot.coreInfo.capabilities;
    for (final capability in [
      'core.events',
      'assets.open',
      'assets.release',
      'sources.locate',
      'indexes.status',
      'indexes.rebuild',
      'jobs.acknowledgeAttention',
      'backups.inspect',
    ]) {
      expect(capabilities, contains(capability), reason: capability);
    }
    for (final capability in [
      'core.setRuntimeState',
      'captures.trash',
      'settings.get',
      'plugins.invoke',
      'backups.create',
      'backups.restore',
    ]) {
      expect(capabilities, isNot(contains(capability)), reason: capability);
    }
  });
}

Future<Capture> _newCommittedCapture(BridgeDiaryApi api, String suffix) async {
  final draft = await api.createDraft(operationId: '$suffix-create');
  final saved = await api.saveDraft(
    id: draft.id,
    text: '$suffix 的正文。',
    expectedRevision: draft.revision,
    operationId: '$suffix-save',
  );
  await api.commit(
    id: draft.id,
    expectedRevision: saved.revision,
    operationId: '$suffix-commit',
  );
  return api.getCapture(draft.id);
}

Future<ImportStatus> _importTextAsset(
  BridgeDiaryApi api,
  String suffix,
  String text,
) async {
  final capture = await _newCommittedCapture(api, suffix);
  final ticket = await api.prepareImport(
    captureId: capture.id,
    displayName: '$suffix.txt',
    mimeHint: 'text/plain',
    operationId: '$suffix-prepare',
  );
  final bytes = utf8.encode(text);
  File(ticket.stagingTicket).writeAsBytesSync(bytes);
  return api.finishImport(
    importId: ticket.importId,
    ticket: ticket.stagingTicket,
    manifest: ImportManifest(
      copiedBytes: bytes.length,
      sha256: sha256.convert(bytes).toString(),
      detectedMime: 'text/plain',
      originalName: '$suffix.txt',
    ),
  );
}

/// 导入完成时核心会排一条 `extract` 任务，目标就是来源 id。
Future<String> _extractJobTarget(BridgeDiaryApi api) async {
  final jobs = await api.listJobs(
    states: const {JobState.queued},
    pageSize: 50,
  );
  final extract = jobs.jobs.lastWhere((job) => job.kind == 'extract');
  return extract.targetIds.single;
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

import 'dart:convert';
import 'dart:io';

import 'package:crypto/crypto.dart';
import 'package:diary_bridge/diary_bridge.dart';
import 'package:flutter_rust_bridge/flutter_rust_bridge_for_generated.dart';
import 'package:test/test.dart';

import 'expected_schema.dart';

/// 端到端：Dart → 桥接 → 真实核心 → SQLite + 文件库。
///
/// 运行前先构建宿主平台产物：
///   cargo build --release            （在仓库根目录）
///   dart test                        （在本包目录）
///
/// 也可以用 `DIARY_BRIDGE_LIB` 指定别的产物路径（Windows 的 dll、Android 上的 so）。
void main() {
  late Directory workDir;
  late BridgeSession session;

  setUpAll(() async {
    final path = File(
      Platform.environment['DIARY_BRIDGE_LIB'] ?? _defaultLibraryPath(),
    ).absolute.path;
    expect(File(path).existsSync(), isTrue,
        reason: '找不到桥接产物：$path。先在仓库根目录跑 cargo build --release');
    await RustLib.init(externalLibrary: ExternalLibrary.open(path));

    workDir = Directory.systemTemp.createTempSync('diary_bridge_test');
    session = await BridgeSession.open(
      libraryPath: '${workDir.path}/library.sqlite',
    );
  });

  tearDownAll(() async {
    // 先关掉会话再删目录：Windows 上文件被打开着是删不掉的。
    session.dispose();
    try {
      workDir.deleteSync(recursive: true);
    } on FileSystemException {
      // 删除失败不该让整个测试变成失败；临时目录留给系统清理。
    }
  });

  test('打开资料库能拿到核心信息与能力清单', () async {
    final info = await session.info();
    expect(info.apiVersion, '1.0');
    expect(
      info.dataSchemaVersion,
      expectedDataSchemaVersion,
      reason: '当前 schema 版本（v6 索引代次、v7 记录文字进索引）',
    );
    expect(info.libraryId, 'library');
    expect(info.capabilities, contains('captures.commit'));
    expect(info.capabilities, contains('imports.finish'));
    expect(info.capabilities, contains('indexes.status'));
    // 检索会话在 B3b 接上了。
    expect(info.capabilities, contains('search.start'));
    // 诚实的能力声明：还没接的不该出现在清单里。
    expect(info.capabilities, isNot(contains('diary.generate')));
    expect(info.recovery.pendingJobs, 0);
  });

  test('创建、保存、提交一条记录，并重新读出来', () async {
    final draft = await session.createDraft(
      occurredAt: DateTime.utc(2026, 9, 27, 9),
      timeZone: 'Asia/Shanghai',
      utcOffsetMinutes: 480,
      operationId: 'op-create',
    );
    expect(draft.state, CaptureState.draft);
    expect(draft.dayKey, '2026-09-27');

    final saved = await session.saveDraft(
      captureId: draft.id,
      text: '今天下午面试完，走出大楼的时候风很大。',
      expectedRevision: draft.revision,
      operationId: 'op-save',
    );
    expect(saved.durable, isTrue);

    final committed = await session.commit(
      captureId: draft.id,
      expectedRevision: saved.revision,
      operationId: 'op-commit',
    );
    expect(committed.capture.state, CaptureState.committed);
    expect(committed.capture.draftText, '今天下午面试完，走出大楼的时候风很大。');
    expect(committed.originalTextRevision, isNotNull);

    final again = await session.getCapture(captureId: draft.id);
    expect(again.revision, committed.capture.revision);
    expect(again.draftText, committed.capture.draftText);

    final page = await session.listCaptures(dayKey: '2026-09-27', limit: 10);
    expect(page.captures.map((capture) => capture.id), contains(draft.id));
  });

  test('导入一个文本文件并提取出正文', () async {
    final draft = await session.createDraft(
      timeZone: 'Asia/Shanghai',
      utcOffsetMinutes: 480,
      operationId: 'op-create-2',
    );

    final ticket = await session.prepareImport(
      captureId: draft.id,
      displayName: '日记.txt',
      mimeHint: 'text/plain',
      origin: ImportOrigin.picker,
      operationId: 'op-import',
    );
    expect(ticket.stagingTicket, isNotEmpty);

    // 平台层负责把字节写到票据指向的位置。
    const text = '第一段：今天风很大。\n\n第二段：晚上吃了面。\n';
    final bytes = utf8.encode(text);
    File(ticket.stagingTicket).writeAsBytesSync(bytes);

    final status = await session.finishImport(
      importId: ticket.importId,
      stagingTicket: ticket.stagingTicket,
      manifest: ImportManifest(
        copiedBytes: bytes.length,
        sha256: sha256.convert(bytes).toString(),
        detectedMime: 'text/plain',
        originalName: '日记.txt',
      ),
    );
    expect(status.state, ImportState.ready);
    final assetId = status.assetId;
    expect(assetId, isNotNull);

    // 导入完成时核心已经排了一个 extract 任务。
    final jobs = await session.listJobs(states: [JobState.queued], limit: 10);
    expect(jobs.map((job) => job.kind), contains('extract'));
    final target =
        jobs.firstWhere((job) => job.kind == 'extract').targetIds.single;

    // 用任务的目标直接提取，证明这个目标真的可提取。
    final content = await session.extractSource(sourceRef: target);
    expect(content.extractorId, 'plain_text');
    expect(content.coverage, Coverage.complete);
    expect(content.segments.length, 2);
    expect(content.segments.first.text, '第一段：今天风很大。');

    // 按来源读派生内容并定位原件。
    final bySource = await session.extractedContent(sourceId: content.sourceId);
    expect(bySource!.text, text);

    final location = await session.locateSource(
      sourceRef: content.sourceId,
      locator: content.segments.first.locator,
    );
    expect(location.available, isTrue);
    expect(location.assetId, assetId);

    // 提取与建索引在同一个事务里：提取完就该是「已索引」。
    final index = await session.indexStatus();
    expect(index.coverage, Coverage.complete);
    expect(index.keywordIndexReady, isTrue);
    expect(index.indexedSegments, 2);
    expect(index.totalSegments, 2);
    expect(index.pendingSegments, 0);
    expect(index.tokenizerVersion, isNotEmpty);
    expect(index.indexRows, greaterThan(0));
    // 语义索引还没接，状态里不能装作就绪。
    expect(index.semanticIndexReady, isFalse);
    expect(index.modelVersion, isNull);
    expect(index.reasons.join(), contains('语义'));
    // 块与向量这一片（B3c-2 前半）只建了存储：`chunkerVersion` 从这一片起不再是
    // null（块真的会进库），但没有向量——`embeddedChunks` 恒为 0。桥接这一片还没有
    // 重建块的入口，所以 `totalChunks` 也是 0；有块时它同样恒为 0 的是 embeddedChunks。
    expect(index.chunkerVersion, isNotNull);
    expect(index.totalChunks, 0);
    expect(index.embeddedChunks, 0);

    // 范围过滤：限定了来源就只看这个来源；空范围是「什么都不看」。
    final scoped = await session.indexStatus(sourceScope: [content.sourceId]);
    expect(scoped.indexedSegments, 2);
    expect(scoped.coverage, Coverage.complete);
    final empty = await session.indexStatus(sourceScope: <String>[]);
    expect(empty.totalSegments, 0);
    expect(empty.coverage, Coverage.unavailable);
  });

  test('待办任务超过 200 时，恢复摘要仍报真实数字', () async {
    // 回归：桥接曾用 `list_jobs(..., 1000).len()` 当计数，而核心的 list 会把
    // limit 夹到 200，所以第 201 个待办任务开始就少报。这里用真实路径堆出
    // 205 个排队中的 extract 任务（每次导入完成时核心排一个），再读恢复摘要。
    const total = 205;
    // 同一个会话被整个文件共享，前面的用例可能已经排过任务，所以按基线增量断言。
    final baseline = (await session.info()).recovery.pendingJobs;
    final draft = await session.createDraft(
      timeZone: 'Asia/Shanghai',
      utcOffsetMinutes: 480,
      operationId: 'op-bulk-create',
    );
    final bytes = utf8.encode('待办任务计数验证\n');

    for (var index = 0; index < total; index++) {
      final ticket = await session.prepareImport(
        captureId: draft.id,
        displayName: 'bulk-$index.txt',
        mimeHint: 'text/plain',
        origin: ImportOrigin.picker,
        operationId: 'op-bulk-$index-prepare',
      );
      File(ticket.stagingTicket).writeAsBytesSync(bytes);
      await session.finishImport(
        importId: ticket.importId,
        stagingTicket: ticket.stagingTicket,
        manifest: ImportManifest(
          copiedBytes: bytes.length,
          sha256: sha256.convert(bytes).toString(),
          detectedMime: 'text/plain',
          originalName: 'bulk-$index.txt',
        ),
      );
    }

    final info = await session.info();
    expect(
      info.recovery.pendingJobs,
      baseline + total,
      reason: '计数必须精确：分页列表的上限是 200，不能拿它的长度当计数',
    );
    expect(
      info.recovery.pendingJobs,
      greaterThan(200),
      reason: '这条用例的意义就在于超过分页上限之后仍然精确',
    );
    // 分页接口本身仍然守自己的上限，两者不冲突。
    final page = await session.listJobs(
      states: [JobState.queued, JobState.retryWait],
      limit: 1000,
    );
    expect(page.length, 200);
  });

  test('核心错误映射成契约错误码', () async {
    await expectLater(
      session.getCapture(captureId: 'cap_不存在'),
      throwsA(
        isA<BridgeError>()
            .having((error) => error.code, 'code', 'not_found')
            .having((error) => error.retryable, 'retryable', false),
      ),
    );
  });

  test('事件按序号递增，可以从游标之后补读', () async {
    final events = await session.eventsSince(fromSequence: 0);
    expect(events, isNotEmpty);
    final sequences = events.map((event) => event.sequence).toList();
    expect(sequences, orderedEquals([...sequences]..sort()));
    // 字段名是 eventType：frb 按 Rust 字段名生成，不读 serde 的重命名。
    // 枚举直接比值：核心给枚举加的 wire() 被 frb 镜像成了 Future<void>，不该用。
    expect(
      events.map((event) => event.eventType),
      contains(EventType.captureChanged),
    );

    final tail = await session.eventsSince(fromSequence: sequences.first + 1);
    expect(tail.length, lessThan(events.length));
  });

  test('中文与 emoji 跨桥往返不走样', () async {
    final draft = await session.createDraft(
      timeZone: 'Asia/Shanghai',
      utcOffsetMinutes: 480,
      operationId: 'op-create-3',
    );
    const text = '妈妈离职了，晚上又觉得还行 🙂 —— 真的吗？';
    final saved = await session.saveDraft(
      captureId: draft.id,
      text: text,
      expectedRevision: draft.revision,
      operationId: 'op-save-3',
    );
    expect(saved.durable, isTrue);
    final again = await session.getCapture(captureId: draft.id);
    expect(again.draftText, text);
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
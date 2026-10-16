import 'dart:convert';
import 'dart:io';

import 'package:crypto/crypto.dart';
import 'package:diary_bridge/diary_bridge.dart';
import 'package:flutter_rust_bridge/flutter_rust_bridge_for_generated.dart';
import 'package:test/test.dart';

/// B3b 的桥接端到端：Dart → `search.*` → 真核心 + 真 SQLite。
///
/// 覆盖分页、取消、游标失效与快照过期这几条**状态机**路径——它们是纯 SQL
/// 测不出来的部分。
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
    workDir = Directory.systemTemp.createTempSync('diary_search_test');
    session = await BridgeSession.open(
      libraryPath: '${workDir.path}/library.sqlite',
    );
  });

  tearDownAll(() {
    session.dispose();
    try {
      workDir.deleteSync(recursive: true);
    } on FileSystemException {
      // 删不掉不影响结论。
    }
  });

  /// 导入一段文本并提取（提取会把正文写进索引）。
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
    final sourceId = (await session.getCapture(captureId: draft.id))
        .orderedSourceIds
        .first;
    await session.extractSource(sourceRef: sourceId);
    return sourceId;
  }

  SearchRequest buildRequest(String query, int pageSize) => SearchRequest(
    query: query,
    mode: SearchMode.keyword,
    filters: const SearchFilters(
      kinds: [],
      includeOldDiaryVersions: false,
      includeTrashed: false,
    ),
    pageSize: pageSize,
  );

  test('分页到底：字段齐全、游标可续、最后一页是 done', () async {
    // 每条材料两段，共 6 条命中。
    await addMaterial(
      '日记.txt',
      List.generate(3, (index) => '第$index段，妈妈打电话来。')
          .join('\n\n'),
      'op-page',
    );
    await addMaterial(
      '另一份.txt',
      List.generate(3, (index) => '另一份第$index段，也提到妈妈。')
          .join('\n\n'),
      'op-page-2',
    );

    final first = await session.startSearch(
      request: buildRequest('妈妈', 4),
      queryRevision: 7,
    );
    expect(first.queryRevision, 7);
    expect(first.phase, SearchPhase.keywordReady);
    expect(first.results.length, 4);
    expect(first.cursor, isNotNull);
    expect(first.indexCoverage, Coverage.complete);

    final hit = first.results.first;
    expect(hit.sourceKind, SourceKind.file);
    expect(hit.matchedBy, [MatchedBy.keyword]);
    expect(hit.hitId, isNotEmpty);
    expect(hit.groupId, isNotEmpty);
    expect(hit.snippet, contains('妈妈'));
    expect(hit.highlights, hasLength(1));
    expect(hit.locator, isNotNull);
    expect(hit.title, isNotNull);

    final second = await session.searchNextPage(
      sessionId: first.sessionId,
      cursor: first.cursor,
    );
    expect(second.results.length, 2);
    expect(second.phase, SearchPhase.done);
    expect(second.cursor, isNull);

    final ids = {...first.results.map((h) => h.hitId), ...second.results.map((h) => h.hitId)};
    expect(ids.length, 6, reason: '翻完应当不重不漏');
  });

  test('取消之后不能再翻页，但快照仍可读', () async {
    final started = await session.startSearch(
      request: buildRequest('妈妈', 2),
      queryRevision: 8,
    );
    final cancelled = await session.cancelSearch(sessionId: started.sessionId);
    expect(cancelled.phase, SearchPhase.cancelled);
    expect(cancelled.cursor, isNull);

    final snapshot = await session.searchSnapshot(sessionId: started.sessionId);
    expect(snapshot.phase, SearchPhase.cancelled);

    await expectLater(
      session.searchNextPage(
        sessionId: started.sessionId,
        cursor: started.cursor,
      ),
      throwsA(
        isA<BridgeError>().having((e) => e.code, 'code', 'invalid_state'),
      ),
    );
  });

  test('别的会话的游标会被拒，索引变了会报 search_expired', () async {
    final one = await session.startSearch(
      request: buildRequest('妈妈', 2),
      queryRevision: 9,
    );
    final other = await session.startSearch(
      request: buildRequest('妈妈', 2),
      queryRevision: 10,
    );
    await expectLater(
      session.searchNextPage(sessionId: one.sessionId, cursor: other.cursor),
      throwsA(
        isA<BridgeError>().having((e) => e.code, 'code', 'cursor_expired'),
      ),
    );

    // 会话期间导入新材料：索引变了，旧快照必须作废。
    await addMaterial(
      '后来的.txt',
      '后来妈妈又打了一次电话。\n',
      'op-after',
    );
    await expectLater(
      session.searchNextPage(sessionId: one.sessionId, cursor: one.cursor),
      throwsA(
        isA<BridgeError>()
            .having((e) => e.code, 'code', 'search_expired')
            .having((e) => e.retryable, 'retryable', true),
      ),
    );
    await expectLater(
      session.searchSnapshot(sessionId: one.sessionId),
      throwsA(
        isA<BridgeError>().having((e) => e.code, 'code', 'search_expired'),
      ),
    );
  });

  test('升级接口在桥上：非混合会话明确拒绝（#69）', () async {
    final started = await session.startSearch(
      request: buildRequest('妈妈', 2),
      queryRevision: 12,
    );
    // 只有 hybrid 会话有「第二阶段」；keyword 会话要求升级是 invalid_state，
    // 不许静默返回一份「融合结果」。
    await expectLater(
      session.upgradeSearch(sessionId: started.sessionId),
      throwsA(
        isA<BridgeError>().having((e) => e.code, 'code', 'invalid_state'),
      ),
    );
  });

  test('用户自己写的文字也能被搜到（记录文字命中）', () async {
    // 这段文字只在这条记录里出现：M2 之前索引只有派生片段，它一条都搜不到。
    final draft = await session.createDraft(
      timeZone: 'Asia/Shanghai',
      utcOffsetMinutes: 480,
      operationId: 'op-capture-text-create',
    );
    final saved = await session.saveDraft(
      captureId: draft.id,
      text: '今天蒸了一锅馒头，味道不错。',
      expectedRevision: draft.revision,
      operationId: 'op-capture-text-save',
    );
    expect(saved.durable, isTrue, reason: '保存与索引写在同一个事务里');

    final snapshot = await session.startSearch(
      request: buildRequest('馒头', 20),
      queryRevision: 11,
    );
    expect(snapshot.results, hasLength(1), reason: '「馒头」只在这条记录的文字里');
    final hit = snapshot.results.single;
    expect(hit.sourceKind, SourceKind.text);
    expect(
      hit.locator,
      isNull,
      reason: '记录文字没有可定位的原件，locator 如实为 null',
    );
    expect(hit.hitId, 'cap_${draft.id}');
    expect(hit.groupId, draft.id);
    expect(hit.coverage, Coverage.complete);
    expect(hit.matchedBy, [MatchedBy.keyword]);
    expect(hit.snippet, contains('馒头'));
    expect(hit.dayKey, isNotNull);
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
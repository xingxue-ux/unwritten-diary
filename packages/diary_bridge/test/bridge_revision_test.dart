import 'dart:convert';
import 'dart:io';

import 'package:crypto/crypto.dart';
import 'package:diary_bridge/diary_bridge.dart';
import 'package:flutter_rust_bridge/flutter_rust_bridge_for_generated.dart';
import 'package:test/test.dart';

/// issue #54 的桥接端到端：**改过的正文当成新的一篇**——新正文进检索、旧正文退出
/// 默认检索、上一版正文可按需读取（`sources.previousRevision`）。
///
/// 语料是**同一话题的改写**（体检结果「不太好」→「还行，只是血脂偏高」），不是换
/// 话题：换话题只能证明机制通不通，测不到「共享词还活着、摘录来自新正文」。
void main() {
  late Directory workDir;
  late BridgeSession session;

  /// 只存在于旧版的措辞。
  const oldOnly = '不太好';
  /// 只存在于新版的措辞。
  const newOnly = '血脂';
  const oldBody = '今天去医院拿报告，体检结果不太好，医生让我下个月复查。';
  const newBody = '今天去医院拿报告，体检结果还行，只是血脂偏高，医生让我下个月复查。';

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
    workDir = Directory.systemTemp.createTempSync('diary_revision_test');
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

  SearchRequest buildRequest(String query) => SearchRequest(
    query: query,
    mode: SearchMode.keyword,
    filters: const SearchFilters(
      kinds: [],
      includeOldDiaryVersions: false,
      includeTrashed: false,
    ),
    pageSize: 20,
  );

  Future<List<SearchHit>> search(String query) async {
    final snapshot = await session.startSearch(
      request: buildRequest(query),
      queryRevision: 1,
    );
    return snapshot.results;
  }

  /// 把一段文字当材料导入（草稿正文留空：命中只可能来自派生片段那一路）。
  Future<String> importMaterial(
    String captureId,
    String text,
    String operation,
  ) async {
    final ticket = await session.prepareImport(
      captureId: captureId,
      displayName: '体检报告.txt',
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
        originalName: '体检报告.txt',
      ),
    );
    final capture = await session.getCapture(captureId: captureId);
    return capture.orderedSourceIds.first;
  }

  /// 改一篇来源的原始文字。乐观锁以**记录**的 revision 为准，所以现读一次。
  Future<SourceRevision> revise(
    String captureId,
    String sourceId,
    String text,
    String operation,
  ) async {
    final capture = await session.getCapture(captureId: captureId);
    return session.reviseText(
      sourceId: sourceId,
      text: text,
      expectedRevision: capture.revision,
      operationId: operation,
    );
  }

  test('导入材料的正文改过之后：新正文进检索、旧正文退出、上一版可读', () async {
    final draft = await session.createDraft(
      timeZone: 'Asia/Shanghai',
      utcOffsetMinutes: 480,
      operationId: 'op-rev-create',
    );
    final sourceId = await importMaterial(draft.id, oldBody, 'op-rev');
    await session.extractSource(sourceRef: sourceId);

    // 改之前：旧措辞在，新措辞不在。用例共用一个资料库，所以一律按本条来源筛，
    // 免得别的用例留下的材料把数字搅乱。
    Future<Iterable<SearchHit>> mine(String query, String source) async {
      return (await search(query)).where((hit) => hit.sourceId == source);
    }

    expect(await mine('体检', sourceId), hasLength(1), reason: '共享词改前命中 1 条');
    expect(await mine(oldOnly, sourceId), hasLength(1), reason: '旧措辞改前命中 1 条');
    expect(await mine(newOnly, sourceId), isEmpty, reason: '新措辞改前不该命中');

    // 导入推进过记录的 revision，提交前现读一次。
    final current = await session.getCapture(captureId: draft.id);
    final committed = await session.commit(
      captureId: draft.id,
      expectedRevision: current.revision,
      operationId: 'op-rev-commit',
    );
    expect(
      committed.originalTextRevision,
      isNull,
      reason: '草稿正文是空的，提交不建文字版本',
    );

    final revised = await revise(draft.id, sourceId, newBody, 'op-rev-revise');
    expect(revised.text, newBody, reason: '修订自己带着正文');

    // 改完必须再提取一次，新正文才进索引。
    final content = await session.extractSource(sourceRef: sourceId);
    expect(content.coverage, Coverage.complete);
    expect(
      content.extractorId,
      'text_direct',
      reason: '纯文本修订走「正文就是内容」那条提取路径',
    );

    // 1) 共享词仍命中，且摘录来自新正文。
    final shared = await mine('体检', sourceId);
    expect(shared, hasLength(1));
    final snippet = shared.single.snippet ?? '';
    expect(snippet, contains('体检结果还行'), reason: '摘录必须来自新正文：$snippet');
    expect(snippet, isNot(contains(oldOnly)), reason: '摘录不该是旧措辞：$snippet');

    // 2) 只存在于旧版的措辞搜不到。
    expect(await mine(oldOnly, sourceId), isEmpty, reason: '旧措辞必须退出默认检索');

    // 3) 只存在于新版的措辞能搜到。
    expect(await mine(newOnly, sourceId), hasLength(1), reason: '新措辞要进索引');

    // 上一版：导入那一版是**原件型**修订（正文要经提取才拿得到，text 为空）。
    final previous = await session.previousSourceRevision(sourceId: sourceId);
    expect(previous, isNotNull, reason: '改过之后应当有上一版');
    expect(previous!.sourceId, sourceId);
    expect(previous.assetId, isNotNull, reason: '导入那一版有原件');
    expect(previous.text, isNull, reason: '原件型修订自己没有正文文字');
  });

  test('自己写的正文改过之后：上一版正文拿得到，派生正文那一路旧词退出', () async {
    final draft = await session.createDraft(
      timeZone: 'Asia/Shanghai',
      utcOffsetMinutes: 480,
      operationId: 'op-text-create',
    );
    final saved = await session.saveDraft(
      captureId: draft.id,
      text: oldBody,
      expectedRevision: draft.revision,
      operationId: 'op-text-save',
    );
    final committed = await session.commit(
      captureId: draft.id,
      expectedRevision: saved.revision,
      operationId: 'op-text-commit',
    );
    final sourceId = committed.originalTextRevision!.sourceId;
    await session.extractSource(sourceRef: sourceId);

    expect(
      await session.previousSourceRevision(sourceId: sourceId),
      isNull,
      reason: '只有一版时没有上一版',
    );

    final revised = await revise(draft.id, sourceId, newBody, 'op-text-revise');
    await session.extractSource(sourceRef: sourceId);

    // 上一版正文：就是提交时那一版的文字。
    final previous = await session.previousSourceRevision(sourceId: sourceId);
    expect(previous, isNotNull);
    expect(previous!.text, oldBody, reason: '拿到的是上一版正文');
    expect(previous.parentRevisionId, isNull, reason: '上一版自己就是第一版');
    expect(previous.revisionId, revised.parentRevisionId);

    // 派生正文那一路：新措辞进索引、旧措辞退出。
    //
    // 按 `sourceId` 筛（来源修订的命中都带 sourceId；记录文字的命中 sourceId 为 null）：
    // 按本片的决策，`captures.draft_text` 是「记录自己的文字」那一套机制（B3d），
    // 不受 `reviseText` 影响——改自己写的正文应当走 `saveDraft`（旧措辞会在索引里被
    // 替换），而不是改来源修订。这处重叠写进了架构文档，前端入口留给 #55。
    Future<Iterable<SearchHit>> bySource(String query, String source) async {
      return (await search(query)).where((hit) => hit.sourceId == source);
    }

    expect(
      await bySource(oldOnly, sourceId),
      isEmpty,
      reason: '旧措辞在来源修订那一路已经退出',
    );
    expect(
      await bySource(newOnly, sourceId),
      hasLength(1),
      reason: '新措辞在来源修订那一路进了索引',
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

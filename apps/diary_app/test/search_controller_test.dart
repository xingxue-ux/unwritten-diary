import 'dart:async';

import 'package:diary_api/diary_api.dart';
import 'package:diary_mock/diary_mock.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:unwritten_diary/src/search_controller.dart';

class DelayedSearchApi extends MockDiaryApi {
  final pending = <String, Completer<SearchSnapshot>>{};
  final cancelled = <String>[];

  @override
  Future<SearchSnapshot> startSearch({
    required SearchRequest request,
    required int queryRevision,
  }) {
    final result = Completer<SearchSnapshot>();
    pending[request.query] = result;
    return result.future;
  }

  @override
  Future<void> cancelSearch(String sessionId) async {
    cancelled.add(sessionId);
  }
}

class ExpiringCursorApi extends MockDiaryApi {
  int searches = 0;
  int pageRequests = 0;

  @override
  Future<SearchSnapshot> startSearch({
    required SearchRequest request,
    required int queryRevision,
  }) async {
    searches++;
    return SearchSnapshot(
      sessionId: 'session-$searches',
      queryRevision: queryRevision,
      phase: SearchPhase.keywordReady,
      results: [
        SearchHit(
          hitId: 'hit-$searches',
          groupId: 'capture-$searches',
          sourceKind: SourceKind.text,
          matchedBy: const {MatchedBy.keyword},
          coverage: Coverage.complete,
          snippet: '第 $searches 次搜索',
        ),
      ],
      cursor: 'cursor-$searches',
    );
  }

  @override
  Future<SearchSnapshot> nextPage({
    required String sessionId,
    String? cursor,
  }) async {
    pageRequests++;
    throw const CursorExpiredException(message: '游标已过期');
  }
}

class HybridUpgradeApi extends MockDiaryApi {
  final upgrades = <String, Completer<SearchSnapshot>>{};
  int searchCount = 0;
  String? requestedCursor;
  SearchMode? requestedMode;
  Completer<SearchSnapshot>? pendingPage;

  @override
  Future<SearchSnapshot> startSearch({
    required SearchRequest request,
    required int queryRevision,
  }) async {
    searchCount++;
    requestedMode = request.mode;
    final session = 'session-${request.query}';
    upgrades[session] = Completer<SearchSnapshot>();
    return SearchSnapshot(
      sessionId: session,
      queryRevision: queryRevision,
      phase: SearchPhase.keywordReady,
      results: [_hit('old', '旧关键词结果'), _hit('same', '两路都会命中')],
      cursor: 'keyword-cursor',
    );
  }

  @override
  Future<SearchSnapshot> upgradeSearch({required String sessionId}) =>
      upgrades[sessionId]!.future;

  @override
  Future<SearchSnapshot> nextPage({
    required String sessionId,
    String? cursor,
  }) async {
    requestedCursor = cursor;
    if (pendingPage != null) return pendingPage!.future;
    return SearchSnapshot(
      sessionId: sessionId,
      queryRevision: 1,
      phase: SearchPhase.done,
      results: [_hit('next', '融合下一页')],
    );
  }
}

SearchHit _hit(String id, String text) => SearchHit(
  hitId: id,
  groupId: id,
  sourceKind: SourceKind.text,
  matchedBy: const {MatchedBy.keyword},
  coverage: Coverage.complete,
  snippet: text,
);

SearchSnapshot result(String session, String text, int revision) =>
    SearchSnapshot(
      sessionId: session,
      queryRevision: revision,
      phase: SearchPhase.done,
      results: [
        SearchHit(
          hitId: session,
          groupId: session,
          sourceKind: SourceKind.text,
          matchedBy: const {MatchedBy.keyword},
          coverage: Coverage.complete,
          snippet: text,
        ),
      ],
    );

void main() {
  test(
    'late result from an older query cannot replace the latest query',
    () async {
      final api = DelayedSearchApi();
      final controller = DiarySearchController(api);
      final oldQuery = controller.search('旧词');
      final newQuery = controller.search('新词');

      api.pending['新词']!.complete(result('new-session', '新结果', 2));
      await newQuery;
      api.pending['旧词']!.complete(result('old-session', '旧结果', 1));
      await oldQuery;
      await Future<void>.delayed(Duration.zero);

      expect(controller.hits.map((hit) => hit.snippet), ['新结果']);
      expect(controller.snapshot?.sessionId, 'new-session');
      expect(api.cancelled, contains('old-session'));
      controller.dispose();
    },
  );

  test(
    'expired cursor restarts the query instead of retrying the same page',
    () async {
      final api = ExpiringCursorApi();
      final controller = DiarySearchController(api);
      await controller.search('独立查询');
      expect(controller.snapshot?.sessionId, 'session-1');
      expect(controller.canLoadMore, isTrue);

      await controller.loadMore();

      expect(api.pageRequests, 1);
      expect(api.searches, 2);
      expect(controller.snapshot?.sessionId, 'session-2');
      expect(controller.hits.map((hit) => hit.hitId), ['hit-2']);
      expect(controller.error, isNull);
      controller.dispose();
    },
  );

  test(
    'hybrid first page replaces keyword order and supplies its cursor',
    () async {
      final api = HybridUpgradeApi();
      final controller = DiarySearchController(
        api,
        supportsHybridUpgrade: true,
      );
      await controller.search('花');
      expect(api.requestedMode, SearchMode.hybrid);
      expect(controller.hits.map((hit) => hit.hitId), ['old', 'same']);
      api.upgrades['session-花']!.complete(
        SearchSnapshot(
          sessionId: 'session-花',
          queryRevision: 1,
          phase: SearchPhase.hybridReady,
          results: [_hit('same', '融合后第一名'), _hit('new', '语义独有')],
          cursor: 'hybrid-cursor',
        ),
      );
      await Future<void>.delayed(Duration.zero);
      expect(controller.hits.map((hit) => hit.hitId), ['same', 'new']);
      expect(controller.snapshot?.cursor, 'hybrid-cursor');
      await controller.loadMore();
      expect(api.requestedCursor, 'hybrid-cursor');
      expect(controller.hits.map((hit) => hit.hitId), ['same', 'new', 'next']);
      controller.dispose();
    },
  );

  test(
    'unavailable upgrade keeps keyword results and explains the state',
    () async {
      final api = HybridUpgradeApi();
      final controller = DiarySearchController(
        api,
        supportsHybridUpgrade: true,
      );
      await controller.search('花');
      api.upgrades['session-花']!.complete(
        SearchSnapshot(
          sessionId: 'session-花',
          queryRevision: 1,
          phase: SearchPhase.keywordReady,
          warnings: const ['没有可融合的语义证据'],
          results: const [],
        ),
      );
      await Future<void>.delayed(Duration.zero);
      expect(controller.hits.map((hit) => hit.hitId), ['old', 'same']);
      expect(controller.snapshot?.phase, SearchPhase.keywordReady);
      expect(controller.upgradeWarning, contains('没有可融合'));
      controller.dispose();
    },
  );

  test('an old upgrade cannot replace a newer query', () async {
    final api = HybridUpgradeApi();
    final controller = DiarySearchController(api, supportsHybridUpgrade: true);
    await controller.search('旧');
    await controller.search('新');
    api.upgrades['session-新']!.complete(
      SearchSnapshot(
        sessionId: 'session-新',
        queryRevision: 2,
        phase: SearchPhase.hybridReady,
        results: [_hit('new', '新查询融合结果')],
      ),
    );
    await Future<void>.delayed(Duration.zero);
    api.upgrades['session-旧']!.complete(
      SearchSnapshot(
        sessionId: 'session-旧',
        queryRevision: 1,
        phase: SearchPhase.hybridReady,
        results: [_hit('old', '旧查询迟到结果')],
      ),
    );
    await Future<void>.delayed(Duration.zero);
    expect(controller.hits.map((hit) => hit.hitId), ['new']);
    controller.dispose();
  });

  test(
    'a keyword page arriving after fusion cannot append stale hits',
    () async {
      final api = HybridUpgradeApi()..pendingPage = Completer<SearchSnapshot>();
      final controller = DiarySearchController(
        api,
        supportsHybridUpgrade: true,
      );
      await controller.search('花');
      final page = controller.loadMore();
      expect(api.requestedCursor, 'keyword-cursor');
      api.upgrades['session-花']!.complete(
        SearchSnapshot(
          sessionId: 'session-花',
          queryRevision: 1,
          phase: SearchPhase.hybridReady,
          results: [_hit('new', '融合第一页')],
          cursor: 'hybrid-cursor',
        ),
      );
      await Future<void>.delayed(Duration.zero);
      api.pendingPage!.complete(
        SearchSnapshot(
          sessionId: 'session-花',
          queryRevision: 1,
          phase: SearchPhase.keywordReady,
          results: [_hit('stale', '旧关键词下一页')],
        ),
      );
      await page;
      expect(controller.hits.map((hit) => hit.hitId), ['new']);
      expect(controller.snapshot?.cursor, 'hybrid-cursor');
      controller.dispose();
    },
  );

  test(
    'an expired keyword cursor after fusion does not restart search',
    () async {
      final api = HybridUpgradeApi()..pendingPage = Completer<SearchSnapshot>();
      final controller = DiarySearchController(
        api,
        supportsHybridUpgrade: true,
      );
      await controller.search('花');
      final page = controller.loadMore();
      api.upgrades['session-花']!.complete(
        SearchSnapshot(
          sessionId: 'session-花',
          queryRevision: 1,
          phase: SearchPhase.hybridReady,
          results: [_hit('new', '融合第一页')],
          cursor: 'hybrid-cursor',
        ),
      );
      await Future<void>.delayed(Duration.zero);
      api.pendingPage!.completeError(
        const CursorExpiredException(message: '旧关键词游标已过期'),
      );
      await page;
      expect(api.searchCount, 1);
      expect(controller.hits.map((hit) => hit.hitId), ['new']);
      expect(controller.snapshot?.cursor, 'hybrid-cursor');
      expect(controller.error, isNull);
      controller.dispose();
    },
  );
}

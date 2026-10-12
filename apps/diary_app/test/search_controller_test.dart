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
}

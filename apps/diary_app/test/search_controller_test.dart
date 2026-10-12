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
}

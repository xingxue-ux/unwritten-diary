import 'dart:async';

import 'package:diary_api/diary_api.dart';
import 'package:flutter/foundation.dart';

/// Owns one active core search session. Older responses never replace a newer
/// query, even when native work finishes after cancellation.
class DiarySearchController extends ChangeNotifier {
  DiarySearchController(this.api, {this.supportsHybridUpgrade = false});

  final DiaryApi api;
  final bool supportsHybridUpgrade;
  int _generation = 0;
  int _revision = 0;
  int _resultSet = 0;
  String _query = '';
  Set<SourceKind> _kinds = const {};
  SearchSnapshot? _snapshot;
  String? _error;
  bool _loading = false;
  bool _loadingMore = false;
  bool _upgrading = false;
  String? _upgradeWarning;
  final List<SearchHit> _hits = [];

  String get query => _query;
  SearchSnapshot? get snapshot => _snapshot;
  String? get error => _error;
  bool get loading => _loading;
  bool get loadingMore => _loadingMore;
  bool get upgrading => _upgrading;
  String? get upgradeWarning => _upgradeWarning;
  List<SearchHit> get hits => List.unmodifiable(_hits);
  bool get canLoadMore =>
      !_loading && !_loadingMore && _snapshot?.cursor != null;

  Future<void> search(String query, {Set<SourceKind> kinds = const {}}) async {
    final generation = ++_generation;
    final oldSession = _snapshot?.sessionId;
    _query = query.trim();
    _kinds = kinds;
    _snapshot = null;
    _hits.clear();
    _error = null;
    _upgradeWarning = null;
    _upgrading = false;
    _loading = _query.isNotEmpty;
    _loadingMore = false;
    notifyListeners();
    if (oldSession != null) unawaited(_cancelQuietly(oldSession));
    if (_query.isEmpty) return;

    try {
      final result = await api.startSearch(
        request: SearchRequest(
          query: _query,
          mode: supportsHybridUpgrade ? SearchMode.hybrid : SearchMode.keyword,
          filters: SearchFilters(kinds: _kinds),
        ),
        queryRevision: ++_revision,
      );
      if (generation != _generation) {
        unawaited(_cancelQuietly(result.sessionId));
        return;
      }
      _accept(result, replace: true);
      if (supportsHybridUpgrade && result.phase == SearchPhase.keywordReady) {
        unawaited(_upgrade(result, generation));
      }
    } on Object catch (error) {
      if (generation != _generation) return;
      _error = error is DiaryException ? error.message : '暂时无法搜索，请稍后重试。';
    } finally {
      if (generation == _generation) {
        _loading = false;
        notifyListeners();
      }
    }
  }

  Future<void> _upgrade(SearchSnapshot keyword, int generation) async {
    _upgrading = true;
    notifyListeners();
    try {
      final result = await api.upgradeSearch(sessionId: keyword.sessionId);
      if (generation != _generation ||
          _snapshot?.sessionId != keyword.sessionId ||
          result.sessionId != keyword.sessionId ||
          result.queryRevision != keyword.queryRevision) {
        return;
      }
      if (result.phase == SearchPhase.hybridReady) {
        // Fusion is a newly ranked first page, not a continuation of the
        // keyword list. Its cursor replaces the keyword cursor as well.
        _accept(result, replace: true);
      } else {
        _upgradeWarning = result.warnings.isNotEmpty
            ? result.warnings.join(' · ')
            : '语义结果暂不可用，继续显示关键词结果。';
      }
    } on SearchExpiredException {
      if (generation == _generation) {
        await search(_query, kinds: _kinds);
      }
    } on Object catch (failure) {
      if (generation == _generation) {
        _upgradeWarning = failure is DiaryException
            ? failure.message
            : '语义补充暂不可用，继续显示关键词结果。';
      }
    } finally {
      if (generation == _generation) {
        _upgrading = false;
        notifyListeners();
      }
    }
  }

  Future<void> loadMore() async {
    if (!canLoadMore) return;
    final generation = _generation;
    final session = _snapshot!;
    final resultSet = _resultSet;
    _loadingMore = true;
    _error = null;
    notifyListeners();
    try {
      final result = await api.nextPage(
        sessionId: session.sessionId,
        cursor: session.cursor,
      );
      if (generation == _generation &&
          resultSet == _resultSet &&
          result.sessionId == session.sessionId &&
          result.queryRevision == session.queryRevision) {
        _accept(result, replace: false);
      }
    } on SearchExpiredException {
      if (generation == _generation && resultSet == _resultSet) {
        // The session changed. Restart rather than retrying its stale cursor.
        await search(_query, kinds: _kinds);
      }
    } on CursorExpiredException {
      if (generation == _generation && resultSet == _resultSet) {
        // A stale cursor cannot recover by requesting the same page again.
        await search(_query, kinds: _kinds);
      }
    } on Object catch (error) {
      if (generation == _generation && resultSet == _resultSet) {
        _error = error is DiaryException ? error.message : '加载下一页失败，请重试。';
      }
    } finally {
      if (generation == _generation) {
        _loadingMore = false;
        notifyListeners();
      }
    }
  }

  void _accept(SearchSnapshot result, {required bool replace}) {
    _snapshot = result;
    if (replace) {
      _resultSet++;
      _hits.clear();
    }
    final existing = _hits.map((hit) => hit.hitId).toSet();
    for (final hit in result.results) {
      if (existing.add(hit.hitId)) _hits.add(hit);
    }
    notifyListeners();
  }

  Future<void> _cancelQuietly(String sessionId) async {
    try {
      await api.cancelSearch(sessionId);
    } on Object {
      // Expired/cancelled sessions are already unusable; generation guards the UI.
    }
  }

  @override
  void dispose() {
    _generation++;
    final sessionId = _snapshot?.sessionId;
    if (sessionId != null) unawaited(_cancelQuietly(sessionId));
    super.dispose();
  }
}

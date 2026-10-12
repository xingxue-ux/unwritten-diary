import 'package:diary_api/diary_api.dart';
import 'package:test/test.dart';

/// 只接得上关键词检索的最小实现者：用 `noSuchMethod` 顶掉其余契约方法，
/// 只覆盖 [DiaryApi.searchSnapshot]。
///
/// 这里刻意用 `extends` 而不是 `implements`：Dart 里只有继承才会拿到接口上的
/// 具体方法体（`implements` 的实现者不继承方法体，会走自己的 `noSuchMethod`），
/// 而 [DiaryApi.upgradeSearch] 的默认实现正是给这类实现者的诚实退化。
class _KeywordOnlyApi extends DiaryApi {
  _KeywordOnlyApi(this._snapshot);

  final SearchSnapshot _snapshot;

  @override
  Future<SearchSnapshot> searchSnapshot(String sessionId) async => _snapshot;

  @override
  dynamic noSuchMethod(Invocation invocation) =>
      throw UnsupportedError('测试桩没有实现 ${invocation.memberName}');
}

void main() {
  test('默认 upgradeSearch 如实退回当前关键词快照，不假装 hybrid_ready', () async {
    const keywordOnly = SearchSnapshot(
      sessionId: 'session-1',
      queryRevision: 3,
      phase: SearchPhase.keywordReady,
      warnings: ['语义检索不可用：没有生效代次；阶段保持 keyword_ready'],
    );
    final api = _KeywordOnlyApi(keywordOnly);

    final upgraded = await api.upgradeSearch(sessionId: 'session-1');

    // 不重算、不新造快照：拿到的就是这条会话当前那一份。
    expect(identical(upgraded, keywordOnly), isTrue);
    // 阶段与结果自洽：没有融合就绝不声称 hybrid_ready。
    expect(upgraded.phase, SearchPhase.keywordReady);
    expect(upgraded.phase, isNot(SearchPhase.hybridReady));
    expect(upgraded.results, keywordOnly.results);
    expect(upgraded.cursor, keywordOnly.cursor);
    expect(upgraded.warnings, keywordOnly.warnings);
  });
}
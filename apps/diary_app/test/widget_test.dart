import 'dart:async';
import 'dart:io';
import 'dart:ui' as ui;

import 'package:diary_api/diary_api.dart';
import 'package:diary_mock/diary_mock.dart';
import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:unwritten_diary/main.dart';
import 'package:unwritten_diary/src/sketch_ui.dart';
import 'package:unwritten_diary/src/ui_strings.dart';

double _contrastRatio(Color foreground, Color background) {
  final a = foreground.computeLuminance();
  final b = background.computeLuminance();
  final lighter = a > b ? a : b;
  final darker = a > b ? b : a;
  return (lighter + 0.05) / (darker + 0.05);
}

class CountingMockDiaryApi extends MockDiaryApi {
  int openCount = 0;

  @override
  Future<CoreSnapshot> open({String? libraryHandle}) {
    openCount++;
    return super.open(libraryHandle: libraryHandle);
  }
}

class IndexSettingsUiApi extends MockDiaryApi {
  int rebuildCount = 0;
  IndexStatus indexStatus = const IndexStatus(
    coverage: Coverage.complete,
    semanticIndexReady: false,
    reasons: ['本地模型已更换，需要重建'],
  );

  @override
  Future<CoreSnapshot> snapshot({Set<String>? scopes}) async {
    final base = await super.snapshot(scopes: scopes);
    return CoreSnapshot(
      coreInfo: CoreInfo(
        apiVersion: base.coreInfo.apiVersion,
        dataSchemaVersion: base.coreInfo.dataSchemaVersion,
        buildVersion: base.coreInfo.buildVersion,
        libraryId: base.coreInfo.libraryId,
        capabilities: {
          ...base.coreInfo.capabilities,
          'indexes.status',
          'indexes.rebuildSemantic',
        },
      ),
      recovery: base.recovery,
      captureCount: base.captureCount,
    );
  }

  @override
  Future<IndexStatus> indexesStatus({Set<String>? sourceScope}) async =>
      indexStatus;

  @override
  Future<JobTicket> rebuildSemanticIndex({
    required String reason,
    required String operationId,
  }) async {
    rebuildCount++;
    return const JobTicket(jobId: 'ui-index-job');
  }
}

class RetryableOpenFailureApi extends MockDiaryApi {
  int attempts = 0;

  @override
  Future<CoreSnapshot> open({String? libraryHandle}) {
    attempts++;
    if (attempts == 1) {
      throw const DiaryException(
        code: DiaryErrorCode.unknown,
        message: '资料库暂时忙，请重试。',
        retryable: true,
      );
    }
    return super.open(libraryHandle: libraryHandle);
  }
}

class IncompatibleOpenApi extends MockDiaryApi {
  @override
  Future<CoreSnapshot> open({String? libraryHandle}) async =>
      const CoreSnapshot(
        coreInfo: CoreInfo(
          apiVersion: '2.0',
          dataSchemaVersion: 0,
          buildVersion: 'incompatible-test',
          libraryId: 'test',
          capabilities: {
            'captures.createDraft',
            'captures.saveDraft',
            'captures.commit',
            'captures.list',
          },
        ),
        recovery: RecoverySummary(),
      );
}

class ConflictSaveApi extends MockDiaryApi {
  @override
  Future<DraftSaveResult> saveDraft({
    required String id,
    required String text,
    required int expectedRevision,
    required String operationId,
  }) async => throw const DiaryException(
    code: DiaryErrorCode.revisionConflict,
    message: '记录版本冲突，输入仍留在页面上。',
  );
}

class HybridPreviewApi extends MockDiaryApi {
  final upgraded = Completer<SearchSnapshot>();

  @override
  Future<CoreSnapshot> snapshot({Set<String>? scopes}) async =>
      const CoreSnapshot(
        coreInfo: CoreInfo(
          apiVersion: kApiVersion,
          dataSchemaVersion: 0,
          buildVersion: 'hybrid-widget-test',
          libraryId: 'hybrid-widget-test',
          capabilities: {
            'search.start',
            'search.upgrade',
            'captures.createDraft',
            'captures.saveDraft',
            'captures.commit',
            'captures.list',
          },
        ),
        recovery: RecoverySummary(),
      );

  @override
  Future<SearchSnapshot> startSearch({
    required SearchRequest request,
    required int queryRevision,
  }) async {
    expect(request.mode, SearchMode.hybrid);
    return SearchSnapshot(
      sessionId: 'hybrid-preview',
      queryRevision: queryRevision,
      phase: SearchPhase.keywordReady,
      results: const [
        SearchHit(
          hitId: 'keyword',
          groupId: 'keyword',
          captureId: 'keyword',
          sourceKind: SourceKind.text,
          matchedBy: {MatchedBy.keyword},
          coverage: Coverage.complete,
          snippet: '关键词阶段的旧片段',
        ),
      ],
    );
  }

  @override
  Future<SearchSnapshot> upgradeSearch({required String sessionId}) =>
      upgraded.future;
}

class LargeCaptureApi extends MockDiaryApi {
  LargeCaptureApi({this.ignorePageSize = false, this.count = 1001});

  final bool ignorePageSize;
  final int count;
  int pageRequests = 0;
  int searchRequests = 0;

  @override
  Future<SearchSnapshot> startSearch({
    required SearchRequest request,
    required int queryRevision,
  }) async {
    searchRequests++;
    final query = request.query.toLowerCase();
    return SearchSnapshot(
      sessionId: 'test-search-$searchRequests',
      queryRevision: queryRevision,
      phase: SearchPhase.done,
      indexCoverage: Coverage.complete,
      results: [
        for (var index = 0; index < count; index++)
          if ('测试片段 $index'.contains(query))
            SearchHit(
              hitId: 'hit-$index',
              groupId: 'capture-$index',
              captureId: 'capture-$index',
              sourceKind: SourceKind.text,
              matchedBy: const {MatchedBy.keyword},
              coverage: Coverage.complete,
              dayKey: '2026-10-12',
              snippet: '测试片段 $index',
            ),
      ],
    );
  }

  @override
  Future<void> cancelSearch(String sessionId) async {}

  @override
  Future<CapturePage> listCaptures({
    String? dayKey,
    String? cursor,
    int pageSize = 20,
  }) async {
    pageRequests++;
    final start = int.tryParse(cursor ?? '0') ?? 0;
    final end = (start + (ignorePageSize ? count : pageSize)).clamp(0, count);
    final when = DateTime.utc(2026, 10, 12);
    return CapturePage(
      captures: [
        for (var index = start; index < end; index++)
          Capture(
            id: 'large-$index',
            revision: 1,
            state: CaptureState.committed,
            occurredAt: when.subtract(Duration(minutes: index)),
            createdAt: when,
            updatedAt: when,
            timeZone: 'Asia/Shanghai',
            utcOffsetMinutes: 480,
            dayKey: '2026-10-12',
            draftText: '测试片段 $index',
          ),
      ],
      nextCursor: end < count ? '$end' : null,
    );
  }
}

Future<void> openApp(WidgetTester tester, MockDiaryApi api) async {
  await tester.pumpWidget(DiaryApp(api: api, demoMode: true));
  await tester.pumpAndSettle();
}

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();
  setUpAll(() async {
    await uiStrings.load();
  });

  testWidgets('timeline builds only visible tiles from 1001 records', (
    tester,
  ) async {
    final api = LargeCaptureApi(ignorePageSize: true);
    final stopwatch = Stopwatch()..start();
    await tester.pumpWidget(DiaryApp(api: api));
    await tester.pumpAndSettle();
    await tester.tap(find.text('片段').last);
    await tester.pumpAndSettle();
    stopwatch.stop();

    final built = find.textContaining('测试片段').evaluate().length;
    expect(built, greaterThan(0));
    expect(built, lessThan(30));
    // Diagnostic numbers make regressions visible without imposing a machine-
    // specific timing or memory threshold on CI.
    // ignore: avoid_print
    print(
      'timeline 1001: ${stopwatch.elapsedMilliseconds} ms, '
      '$built built tiles, ${ProcessInfo.currentRss ~/ (1024 * 1024)} MiB RSS',
    );

    stopwatch
      ..reset()
      ..start();
    await tester.tap(find.text('搜索').last);
    await tester.pumpAndSettle();
    await tester.enterText(find.byKey(const Key('global-search')), '测试片段');
    await tester.pump(const Duration(milliseconds: 250));
    await tester.pumpAndSettle();
    stopwatch.stop();
    final searchBuilt = find.textContaining('测试片段').evaluate().length;
    expect(searchBuilt, greaterThan(0));
    expect(searchBuilt, lessThan(30));
    // ignore: avoid_print
    print(
      'search 1001: ${stopwatch.elapsedMilliseconds} ms, '
      '$searchBuilt built tiles, ${ProcessInfo.currentRss ~/ (1024 * 1024)} MiB RSS',
    );
  });

  testWidgets('timeline loads another page and keeps position after detail', (
    tester,
  ) async {
    final api = LargeCaptureApi(count: 60);
    await tester.pumpWidget(DiaryApp(api: api));
    await tester.pumpAndSettle();
    await tester.tap(find.text('片段').last);
    await tester.pumpAndSettle();
    expect(api.pageRequests, 1);

    await tester.drag(find.byType(CustomScrollView), const Offset(0, -2200));
    await tester.pumpAndSettle();
    expect(api.pageRequests, greaterThan(1));
    final scrollable = find.descendant(
      of: find.byType(CustomScrollView),
      matching: find.byType(Scrollable),
    );
    final before = tester.state<ScrollableState>(scrollable).position.pixels;
    expect(before, greaterThan(0));

    await tester.tap(find.textContaining('测试片段').at(2));
    await tester.pumpAndSettle();
    await tester.tap(find.text('返回'));
    await tester.pumpAndSettle();
    final after = tester.state<ScrollableState>(scrollable).position.pixels;
    expect(after, closeTo(before, 2));
  });

  testWidgets('search waits for IME commit and then debounces once', (
    tester,
  ) async {
    final api = LargeCaptureApi(count: 2);
    await tester.pumpWidget(DiaryApp(api: api));
    await tester.pumpAndSettle();
    await tester.tap(find.text('搜索').last);
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const Key('global-search')));
    tester.testTextInput.updateEditingValue(
      const TextEditingValue(
        text: '测试片段 1',
        selection: TextSelection.collapsed(offset: 6),
        composing: ui.TextRange(start: 0, end: 6),
      ),
    );
    await tester.pump(const Duration(milliseconds: 300));
    expect(api.searchRequests, 0);

    tester.testTextInput.updateEditingValue(
      const TextEditingValue(
        text: '测试片段 1',
        selection: TextSelection.collapsed(offset: 6),
      ),
    );
    await tester.pump(const Duration(milliseconds: 249));
    expect(api.searchRequests, 0);
    await tester.pump(const Duration(milliseconds: 1));
    await tester.pumpAndSettle();
    expect(api.searchRequests, 1);
    expect(find.text('测试片段 1'), findsWidgets);
  });

  testWidgets(
    'core search result opens its own excerpt and returns to search',
    (tester) async {
      final api = LargeCaptureApi(count: 2);
      await tester.pumpWidget(DiaryApp(api: api));
      await tester.pumpAndSettle();
      await tester.tap(find.text('搜索').last);
      await tester.pumpAndSettle();
      await tester.enterText(find.byKey(const Key('global-search')), '测试片段 1');
      await tester.pump(const Duration(milliseconds: 250));
      await tester.pumpAndSettle();
      await tester.tap(find.text('一段文字记录'));
      await tester.pumpAndSettle();
      expect(find.text('返回搜索'), findsOneWidget);
      expect(find.text('测试片段 1'), findsOneWidget);
      await tester.tap(find.text('返回搜索'));
      await tester.pumpAndSettle();
      expect(find.byKey(const Key('global-search')), findsOneWidget);
    },
  );

  testWidgets('search screen replaces keyword cards after hybrid upgrade', (
    tester,
  ) async {
    final api = HybridPreviewApi();
    await tester.pumpWidget(DiaryApp(api: api));
    await tester.pumpAndSettle();
    await tester.tap(find.text('搜索').last);
    await tester.pumpAndSettle();
    await tester.enterText(find.byKey(const Key('global-search')), '花');
    await tester.pump(const Duration(milliseconds: 250));
    await tester.pump();
    expect(find.text('关键词阶段的旧片段'), findsOneWidget);
    expect(find.text('先显示关键词结果，语义补充就绪后会更新排序。'), findsOneWidget);

    api.upgraded.complete(
      const SearchSnapshot(
        sessionId: 'hybrid-preview',
        queryRevision: 1,
        phase: SearchPhase.hybridReady,
        results: [
          SearchHit(
            hitId: 'semantic',
            groupId: 'semantic',
            captureId: 'semantic',
            sourceKind: SourceKind.text,
            matchedBy: {MatchedBy.semantic},
            coverage: Coverage.complete,
            snippet: '融合阶段的新片段',
          ),
        ],
      ),
    );
    await tester.pumpAndSettle();
    expect(find.text('融合阶段的新片段'), findsOneWidget);
    expect(find.text('关键词阶段的旧片段'), findsNothing);
    expect(find.text('已结合关键词和语义结果。'), findsOneWidget);
  });

  testWidgets('record page meets text contrast and Android touch guidelines', (
    tester,
  ) async {
    final semantics = tester.ensureSemantics();
    await openApp(tester, MockDiaryApi());
    // Flutter's pixel guideline mistakes antialiased CJK placeholder glyphs
    // for the foreground in CI. Check the declared colors directly instead.
    for (final label in ['记录', '日记', '搜索', '片段', '我的']) {
      final text = tester.widget<Text>(find.text(label).last);
      final foreground = text.style?.color;
      expect(foreground, isNotNull, reason: label);
      expect(
        _contrastRatio(foreground!, SketchColors.paper),
        greaterThanOrEqualTo(4.5),
        reason: '$label must remain readable on the navigation paper',
      );
    }
    await expectLater(tester, meetsGuideline(androidTapTargetGuideline));
    for (final label in [
      '封面与加载动画',
      '前往记录',
      '前往日记',
      '前往搜索',
      '前往片段',
      '前往我的',
      '添加材料',
    ]) {
      final node = tester.getSemantics(find.bySemanticsLabel(label));
      final data = node.getSemanticsData();
      expect(data.flagsCollection.isButton, isTrue, reason: label);
      expect(data.hasAction(ui.SemanticsAction.tap), isTrue, reason: label);
    }
    final date = tester.getSemantics(find.bySemanticsLabel(RegExp('记录日期')));
    expect(date.getSemanticsData().flagsCollection.isButton, isTrue);
    expect(date.getSemanticsData().hasAction(ui.SemanticsAction.tap), isTrue);
    final disabledFinish = tester.getSemantics(find.bySemanticsLabel('收好这一刻'));
    expect(
      disabledFinish.getSemanticsData().flagsCollection.isEnabled,
      ui.Tristate.isFalse,
    );
    expect(
      disabledFinish.getSemanticsData().hasAction(ui.SemanticsAction.tap),
      isFalse,
    );
    await tester.enterText(find.byKey(const Key('capture-editor')), '今天的一句话');
    await tester.pump();
    final enabledFinish = tester.getSemantics(find.bySemanticsLabel('收好这一刻'));
    expect(
      enabledFinish.getSemanticsData().flagsCollection.isEnabled,
      ui.Tristate.isTrue,
    );
    expect(
      enabledFinish.getSemanticsData().hasAction(ui.SemanticsAction.tap),
      isTrue,
    );
    semantics.dispose();
  });

  testWidgets('reduced motion settles the writing cat', (tester) async {
    await tester.pumpWidget(
      const MaterialApp(
        home: Scaffold(
          body: MediaQuery(
            data: MediaQueryData(disableAnimations: true),
            child: WritingCat(),
          ),
        ),
      ),
    );
    await tester.pumpAndSettle();
    expect(tester.binding.hasScheduledFrame, isFalse);
  });

  testWidgets('system dark mode uses the warm ink and paper treatment', (
    tester,
  ) async {
    tester.binding.platformDispatcher.platformBrightnessTestValue =
        Brightness.dark;
    addTearDown(
      tester.binding.platformDispatcher.clearPlatformBrightnessTestValue,
    );
    await openApp(tester, MockDiaryApi());
    expect(find.byType(ColorFiltered), findsOneWidget);
    expect(
      tester.widget<ColorFiltered>(find.byType(ColorFiltered)).colorFilter,
      const ColorFilter.matrix(<double>[
        0,
        0,
        -.88,
        0,
        245,
        0,
        -.88,
        0,
        0,
        245,
        -.88,
        0,
        0,
        0,
        245,
        0,
        0,
        0,
        1,
        0,
      ]),
    );
    expect(find.text('今天，想留住什么？'), findsOneWidget);
    expect(tester.takeException(), isNull);
  });

  testWidgets('light mode keeps the original paper palette', (tester) async {
    tester.binding.platformDispatcher.platformBrightnessTestValue =
        Brightness.light;
    addTearDown(
      tester.binding.platformDispatcher.clearPlatformBrightnessTestValue,
    );
    await openApp(tester, MockDiaryApi());
    expect(find.byType(ColorFiltered), findsNothing);
  });

  testWidgets('real mode reads saved records and hides preview material', (
    tester,
  ) async {
    final api = MockDiaryApi();
    await api.open();
    final draft = await api.createDraft(operationId: 'seed-create');
    final saved = await api.saveDraft(
      id: draft.id,
      text: '重启后仍能看到的文字。',
      expectedRevision: draft.revision,
      operationId: 'seed-save',
    );
    await api.commit(
      id: draft.id,
      expectedRevision: saved.revision,
      operationId: 'seed-commit',
    );

    await tester.pumpWidget(DiaryApp(api: api));
    await tester.pumpAndSettle();
    await tester.tap(find.text('片段').last);
    await tester.pumpAndSettle();

    expect(find.text('重启后仍能看到的文字。'), findsOneWidget);
    expect(find.text('花店门口的片刻'), findsNothing);
    expect(find.text('傍晚的街角'), findsNothing);
  });

  testWidgets('backgrounding immediately flushes an edited draft', (
    tester,
  ) async {
    final api = MockDiaryApi();
    await tester.pumpWidget(DiaryApp(api: api));
    await tester.pumpAndSettle();
    await tester.enterText(find.byKey(const Key('capture-editor')), '切到后台前写的字');
    tester.binding.handleAppLifecycleStateChanged(AppLifecycleState.paused);
    await tester.pumpAndSettle();

    final captures = (await api.listCaptures()).captures;
    expect(captures.single.draftText, '切到后台前写的字');
  });

  testWidgets(
    'record page opens immediately and committed text appears in timeline',
    (tester) async {
      final api = CountingMockDiaryApi();
      await openApp(tester, api);
      expect(api.openCount, 1);
      expect(find.text('今天，想留住什么？'), findsOneWidget);
      expect(find.textContaining('交互演示'), findsOneWidget);

      await tester.enterText(
        find.byKey(const Key('capture-editor')),
        '今天的风很轻。',
      );
      await tester.pump(const Duration(seconds: 1));
      await tester.pumpAndSettle();
      expect(find.textContaining('已模拟保存'), findsOneWidget);

      await tester.ensureVisible(find.byKey(const Key('finish-capture')));
      await tester.tap(find.byKey(const Key('finish-capture')));
      await tester.pumpAndSettle();
      expect(
        (await api.listCaptures()).captures.single.state,
        CaptureState.committed,
      );

      await tester.tap(find.text('片段').last);
      await tester.pumpAndSettle();
      expect(find.text('今天的风很轻。'), findsOneWidget);
    },
  );

  testWidgets('failed save leaves text editable and can retry', (tester) async {
    final api = MockDiaryApi()
      ..nextSaveFailure = const DiaryException(
        code: DiaryErrorCode.storageFull,
        message: '空间不足，尚未保存。',
      );
    await openApp(tester, api);
    await tester.enterText(find.byKey(const Key('capture-editor')), '第一句');
    await tester.testTextInput.receiveAction(TextInputAction.newline);
    await tester.pump(const Duration(seconds: 1));
    await tester.pumpAndSettle();
    expect(find.textContaining('空间不足'), findsOneWidget);
    expect(find.byKey(const Key('capture-editor')), findsOneWidget);

    await tester.ensureVisible(find.text('重试'));
    await tester.tap(find.text('重试'));
    await tester.pumpAndSettle();
    expect(find.textContaining('已模拟保存'), findsOneWidget);
    expect(
      (await api.listCaptures()).captures.single.state,
      CaptureState.draft,
    );
  });

  testWidgets('diary candidate stays separate until chosen', (tester) async {
    await openApp(tester, MockDiaryApi());
    await tester.tap(find.text('日记').last);
    await tester.pumpAndSettle();
    expect(find.text('普通的一天，也有光'), findsOneWidget);
    await tester.ensureVisible(find.text('模拟重新整理'));
    await tester.tap(find.text('模拟重新整理'));
    await tester.pumpAndSettle();
    expect(find.textContaining('当前阅读版不会自动替换'), findsOneWidget);
    await tester.tap(find.text('查看候选版本'));
    await tester.pumpAndSettle();
    expect(find.text('采用候选版'), findsOneWidget);
  });

  testWidgets('narrow viewport and large text keep primary action reachable', (
    tester,
  ) async {
    tester.view.physicalSize = const Size(320, 700);
    tester.view.devicePixelRatio = 1;
    tester.binding.platformDispatcher.textScaleFactorTestValue = 1.4;
    addTearDown(tester.view.resetPhysicalSize);
    addTearDown(tester.view.resetDevicePixelRatio);
    addTearDown(
      tester.binding.platformDispatcher.clearTextScaleFactorTestValue,
    );

    await openApp(tester, MockDiaryApi());
    await tester.enterText(find.byKey(const Key('capture-editor')), '小小一段。');
    await tester.ensureVisible(find.byKey(const Key('finish-capture')));
    await tester.pumpAndSettle();
    expect(find.byKey(const Key('finish-capture')), findsOneWidget);
    expect(tester.takeException(), isNull);
  });

  testWidgets('retryable open failure shows its message and retry works', (
    tester,
  ) async {
    final api = RetryableOpenFailureApi();
    await tester.pumpWidget(DiaryApp(api: api, demoMode: true));
    await tester.pumpAndSettle();
    expect(find.text('资料库暂时忙，请重试。'), findsOneWidget);
    await tester.tap(find.text('重试'));
    await tester.pumpAndSettle();
    expect(api.attempts, 2);
    expect(find.byKey(const Key('capture-editor')), findsOneWidget);
  });

  testWidgets('incompatible API version cannot enter the editor', (
    tester,
  ) async {
    await tester.pumpWidget(
      DiaryApp(api: IncompatibleOpenApi(), demoMode: true),
    );
    await tester.pumpAndSettle();
    expect(find.textContaining('版本不兼容'), findsOneWidget);
    expect(find.byKey(const Key('capture-editor')), findsNothing);
    expect(find.text('重试'), findsNothing);
  });

  testWidgets('finish failure keeps text and gives explicit feedback', (
    tester,
  ) async {
    await openApp(tester, ConflictSaveApi());
    await tester.enterText(find.byKey(const Key('capture-editor')), '没有丢失');
    await tester.ensureVisible(find.byKey(const Key('finish-capture')));
    await tester.tap(find.byKey(const Key('finish-capture')));
    await tester.pump(const Duration(seconds: 1));
    await tester.pumpAndSettle();
    expect(find.textContaining('内容已被修改'), findsWidgets);
    expect(find.text('没有丢失'), findsOneWidget);
    expect(find.text('复制文字'), findsOneWidget);
    expect(find.text('重试'), findsNothing);
  });

  testWidgets('two times text scale keeps a committed capture reachable', (
    tester,
  ) async {
    tester.view.physicalSize = const Size(320, 640);
    tester.view.devicePixelRatio = 1;
    tester.binding.platformDispatcher.textScaleFactorTestValue = 2;
    addTearDown(tester.view.resetPhysicalSize);
    addTearDown(tester.view.resetDevicePixelRatio);
    addTearDown(
      tester.binding.platformDispatcher.clearTextScaleFactorTestValue,
    );

    await openApp(tester, MockDiaryApi());
    await tester.enterText(find.byKey(const Key('capture-editor')), '放大字体的记录');
    await tester.ensureVisible(find.byKey(const Key('finish-capture')));
    await tester.tap(find.byKey(const Key('finish-capture')));
    await tester.pumpAndSettle();
    await tester.tap(find.text('片段').last);
    await tester.pumpAndSettle();
    await tester.scrollUntilVisible(
      find.text('放大字体的记录'),
      250,
      scrollable: find.descendant(
        of: find.byType(CustomScrollView),
        matching: find.byType(Scrollable),
      ),
    );
    expect(find.text('放大字体的记录'), findsOneWidget);
    expect(tester.takeException(), isNull);
  });

  testWidgets('persistent mode labels a confirmed save accurately', (
    tester,
  ) async {
    await tester.pumpWidget(DiaryApp(api: MockDiaryApi(), demoMode: false));
    await tester.pumpAndSettle();
    await tester.enterText(find.byKey(const Key('capture-editor')), '确认文案');
    await tester.pump(const Duration(seconds: 1));
    await tester.pumpAndSettle();
    expect(find.text('已保存到本地资料库'), findsOneWidget);
    expect(find.textContaining('已模拟保存'), findsNothing);
  });

  testWidgets('attachment and recording entries disclose preview status', (
    tester,
  ) async {
    await openApp(tester, MockDiaryApi());
    await tester.ensureVisible(find.text('添加材料'));
    await tester.tap(find.text('添加材料'));
    await tester.pumpAndSettle();
    expect(find.textContaining('尚未访问设备文件'), findsOneWidget);
    await tester.tap(find.text('模拟添加照片'));
    await tester.pumpAndSettle();
    expect(find.textContaining('照片 · 演示'), findsOneWidget);

    await tester.ensureVisible(find.text('录音').first);
    await tester.tap(find.text('录音').first);
    await tester.pumpAndSettle();
    expect(find.textContaining('不会录音'), findsOneWidget);
  });

  testWidgets('long action wraps at 320 width and two times text scale', (
    tester,
  ) async {
    tester.view.physicalSize = const Size(320, 700);
    tester.view.devicePixelRatio = 1;
    tester.binding.platformDispatcher.textScaleFactorTestValue = 2;
    addTearDown(tester.view.resetPhysicalSize);
    addTearDown(tester.view.resetDevicePixelRatio);
    addTearDown(
      tester.binding.platformDispatcher.clearTextScaleFactorTestValue,
    );

    await tester.pumpWidget(
      MaterialApp(
        home: Scaffold(
          body: SizedBox(
            width: 276,
            child: SketchAction(
              label: '保存为新版（演示）',
              icon: Icons.save_outlined,
              onPressed: () {},
            ),
          ),
        ),
      ),
    );
    expect(find.text('保存为新版（演示）'), findsOneWidget);
    expect(tester.takeException(), isNull);
  });

  testWidgets('demo labels survive scrolling and identify sample cards', (
    tester,
  ) async {
    await openApp(tester, MockDiaryApi());
    await tester.drag(
      find.byType(SingleChildScrollView),
      const Offset(0, -600),
    );
    await tester.pumpAndSettle();
    expect(find.text('演示模式 · 内容仅在本次运行'), findsOneWidget);
    expect(tester.getTopLeft(find.text('演示模式 · 内容仅在本次运行')).dy, lessThan(100));

    await tester.tap(find.text('搜索').last);
    await tester.pumpAndSettle();
    expect(find.text('示例 · 日记 · 无实际日期'), findsOneWidget);
    expect(find.text('示例 · 原始文字 · 无实际日期'), findsOneWidget);
    await tester.tap(find.text('片段').last);
    await tester.pumpAndSettle();
    expect(find.text('示例 · 照片 · 待处理'), findsOneWidget);
    expect(find.text('示例 · 录音 · 待转写'), findsOneWidget);
  });

  testWidgets('preview settings disclose that no core action runs', (
    tester,
  ) async {
    await openApp(tester, MockDiaryApi());
    await tester.tap(find.text('我的').last);
    await tester.pumpAndSettle();
    await tester.ensureVisible(find.text('风格偏好'));
    await tester.tap(find.text('风格偏好'));
    await tester.pumpAndSettle();
    expect(find.textContaining('暂不保存到核心'), findsOneWidget);
    await tester.tap(find.text('返回'));
    await tester.pumpAndSettle();
    await tester.ensureVisible(find.text('整理时机'));
    await tester.tap(find.text('整理时机'));
    await tester.pumpAndSettle();
    expect(find.textContaining('自动整理尚未接入'), findsOneWidget);
  });

  testWidgets('unready semantic index has a visible working settings action', (
    tester,
  ) async {
    final api = IndexSettingsUiApi();
    await tester.pumpWidget(DiaryApp(api: api));
    await tester.pumpAndSettle();
    await tester.tap(find.text('我的').last);
    await tester.pumpAndSettle();
    await tester.ensureVisible(find.text('搜索索引 · 需要处理'));
    await tester.tap(find.text('搜索索引 · 需要处理'));
    await tester.pumpAndSettle();
    expect(find.text('语义检索暂不可用'), findsOneWidget);
    await tester.tap(find.text('重建语义索引'));
    await tester.pumpAndSettle();
    expect(api.rebuildCount, 1);
    expect(find.textContaining('已加入本地队列'), findsOneWidget);
  });

  testWidgets('partial semantic coverage remains visibly searchable', (
    tester,
  ) async {
    final api = IndexSettingsUiApi()
      ..indexStatus = const IndexStatus(
        coverage: Coverage.partial,
        semanticIndexReady: false,
        pendingCount: 3,
      );
    await tester.pumpWidget(DiaryApp(api: api));
    await tester.pumpAndSettle();
    await tester.tap(find.text('我的').last);
    await tester.pumpAndSettle();
    await tester.ensureVisible(find.text('搜索索引 · 需要处理'));
    await tester.tap(find.text('搜索索引 · 需要处理'));
    await tester.pumpAndSettle();
    expect(find.text('语义检索部分可用'), findsOneWidget);
    expect(find.textContaining('已嵌入的内容可以搜到'), findsOneWidget);
    expect(find.text('还有 3 块待嵌入。'), findsOneWidget);
    expect(find.text('语义检索暂不可用'), findsNothing);
  });

  testWidgets('Windows index settings disclose that models are not bundled', (
    tester,
  ) async {
    debugDefaultTargetPlatformOverride = TargetPlatform.windows;
    try {
      final api = IndexSettingsUiApi();
      await tester.pumpWidget(DiaryApp(api: api));
      await tester.pumpAndSettle();
      await tester.tap(find.text('我的').last);
      await tester.pumpAndSettle();
      await tester.ensureVisible(find.text('搜索索引 · 需要处理'));
      await tester.tap(find.text('搜索索引 · 需要处理'));
      await tester.pumpAndSettle();

      expect(find.textContaining('当前 Windows 构建不含任何本地模型文件'), findsOneWidget);
      expect(find.textContaining('语义检索与录音转写需要配置模型目录后才能使用'), findsOneWidget);
      expect(find.textContaining('不会自动下载模型'), findsOneWidget);
      // 有 IndexStatus.reasons 时就原样显示，不另编一句替代。
      expect(find.text('· 本地模型已更换，需要重建'), findsOneWidget);
    } finally {
      debugDefaultTargetPlatformOverride = null;
    }
  });

  testWidgets('main routes fit a narrow screen at two times text scale', (
    tester,
  ) async {
    tester.view.physicalSize = const Size(320, 700);
    tester.view.devicePixelRatio = 1;
    tester.binding.platformDispatcher.textScaleFactorTestValue = 2;
    addTearDown(tester.view.resetPhysicalSize);
    addTearDown(tester.view.resetDevicePixelRatio);
    addTearDown(
      tester.binding.platformDispatcher.clearTextScaleFactorTestValue,
    );

    await openApp(tester, MockDiaryApi());
    for (final label in ['日记', '搜索', '片段', '我的', '记录']) {
      await tester.tap(find.text(label).last);
      await tester.pumpAndSettle();
      expect(tester.takeException(), isNull, reason: '$label route overflowed');
      expect(find.text('演示模式 · 内容仅在本次运行'), findsOneWidget);
    }
  });

  testWidgets('secondary routes fit a narrow screen at two times text scale', (
    tester,
  ) async {
    tester.view.physicalSize = const Size(320, 700);
    tester.view.devicePixelRatio = 1;
    tester.binding.platformDispatcher.textScaleFactorTestValue = 2;
    addTearDown(tester.view.resetPhysicalSize);
    addTearDown(tester.view.resetDevicePixelRatio);
    addTearDown(
      tester.binding.platformDispatcher.clearTextScaleFactorTestValue,
    );
    await openApp(tester, MockDiaryApi());

    for (final label in [
      '搜索索引',
      '风格偏好',
      '模型服务',
      '整理时机',
      '任务中心',
      '插件',
      '备份与恢复',
      '存储与回收站',
      '封面与加载动画',
    ]) {
      await tester.tap(find.text('我的').last);
      await tester.pumpAndSettle();
      await tester.ensureVisible(find.text(label));
      await tester.tap(find.text(label));
      await tester.pumpAndSettle();
      expect(tester.takeException(), isNull, reason: '$label panel overflowed');
    }

    await tester.ensureVisible(find.text('查看加载动画'));
    await tester.tap(find.text('查看加载动画'));
    await tester.pump(const Duration(milliseconds: 300));
    expect(tester.takeException(), isNull, reason: 'loading panel overflowed');

    await tester.tap(find.text('日记').last);
    await tester.pumpAndSettle();
    for (final label in ['版本', '编辑为新版', '主动请求回应']) {
      if (label != '版本') {
        await tester.tap(find.text('日记').last);
        await tester.pumpAndSettle();
      }
      await tester.ensureVisible(find.text(label).last);
      await tester.tap(find.text(label).last);
      await tester.pumpAndSettle();
      expect(tester.takeException(), isNull, reason: '$label panel overflowed');
    }
  });
}

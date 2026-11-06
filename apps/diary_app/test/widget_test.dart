import 'dart:async';
import 'dart:io';
import 'dart:ui' as ui;

import 'package:diary_api/diary_api.dart';
import 'package:diary_mock/diary_mock.dart';
import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:path_provider_platform_interface/path_provider_platform_interface.dart';
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

/// 测试里的 support 目录：日记页要它才能建控制器。
class _FakePathProvider extends PathProviderPlatform {
  _FakePathProvider(this.root);
  final String root;

  @override
  Future<String?> getApplicationSupportPath() async => root;

  @override
  Future<String?> getTemporaryPath() async => root;

  @override
  Future<String?> getApplicationDocumentsPath() async => root;
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


/// 本地文件 I/O 在 widget 测试里不会自己推进：要让出真实事件循环再 pump。
Future<void> settleRealIo(WidgetTester tester) async {
  // 一次授权要连着做几次读写（读表 → 写表 → 再读表），每做一次都要让出事件循环。
  for (var i = 0; i < 8; i++) {
    await tester.runAsync(
      () => Future<void>.delayed(const Duration(milliseconds: 60)),
    );
    await tester.pump();
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
    final mock = MockDiaryApi();
    await openApp(tester, mock);
    await tester.tap(find.text('我的').last);
    await tester.pumpAndSettle();
    await tester.ensureVisible(find.text('风格偏好'));
    await tester.tap(find.text('风格偏好'));
    await tester.pumpAndSettle();
    expect(find.textContaining('暂不保存到核心'), findsOneWidget);
    // 五个维度都能自定义：点「自定义」后出现输入框，写进去的值会原样保存。
    expect(find.text('从一套现成的开始'), findsOneWidget);
    expect(find.byType(TextField), findsAtLeastNWidgets(3));
    await tester.tap(find.text('返回'));
    await tester.pumpAndSettle();
    await tester.ensureVisible(find.text('整理时机'));
    await tester.tap(find.text('整理时机'));
    await tester.pumpAndSettle();
    // 「整理时机」不再是一段只读的说明：三项都走核心真实的 `organize_settings`，
    // 开关点下去会真的写回（这里用 MockDiaryApi 验证调用链）。
    expect(find.text('自动整理'), findsOneWidget);
    expect(find.text('22:30'), findsOneWidget);
    expect((await mock.organizeSchedule()).enabled, isFalse);
    await tester.tap(find.byType(Switch).first);
    await tester.pumpAndSettle();
    expect((await mock.organizeSchedule()).enabled, isTrue);
  });

  testWidgets('unready semantic index has a visible working settings action', (
    tester,
  ) async {
    final api = IndexSettingsUiApi();
    await tester.pumpWidget(DiaryApp(api: api));
    await tester.pumpAndSettle();
    await tester.tap(find.text('我的').last);
    await tester.pumpAndSettle();
    await tester.ensureVisible(find.text('搜索索引 · 未建立'));
    await tester.tap(find.text('搜索索引 · 未建立'));
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
    await tester.ensureVisible(find.text('搜索索引 · 未建立'));
    await tester.tap(find.text('搜索索引 · 未建立'));
    await tester.pumpAndSettle();
    expect(find.text('语义检索部分可用'), findsOneWidget);
    expect(find.textContaining('已嵌入的内容可以搜到'), findsOneWidget);
    expect(find.text('还有 3 块待嵌入。'), findsOneWidget);
    expect(find.text('语义检索暂不可用'), findsNothing);
  });

  testWidgets('「获取模型列表」在添加服务里，点了才拉', (tester) async {
    final api = _ProviderUiApi();
    // demoMode 走的是预览面板；真实安装版用的是 AiProviderPanel，这里要测后者。
    await tester.pumpWidget(DiaryApp(api: api));
    await tester.pumpAndSettle();
    await tester.tap(find.text('我的').last);
    await tester.pumpAndSettle();
    await tester.ensureVisible(find.text('模型服务'));
    await tester.tap(find.text('模型服务'));
    await tester.pumpAndSettle();

    // 打开页面**不该**发请求：核心 provider::models 的约定就是「点了才出网」。
    expect(api.modelCalls, 0, reason: '打开页面不该自动拉模型列表');

    // 没填密钥就问模型列表：先要密钥，不发请求。
    await tester.ensureVisible(find.text('获取模型列表'));
    await tester.tap(find.text('获取模型列表'));
    await tester.pumpAndSettle();
    expect(api.modelCalls, 0, reason: '没有密钥不该出网');
    expect(find.textContaining('先填 API Key'), findsOneWidget);

    // 填上密钥再点：这一次真的去问，候选出现在表单里。
    await tester.enterText(
      find.widgetWithText(TextField, 'API Key'),
      'sk-test',
    );
    await tester.tap(find.text('获取模型列表'));
    await tester.pumpAndSettle();
    expect(api.modelCalls, 1, reason: '点一次按钮拉一次');
    expect(find.text('deepseek-reasoner'), findsOneWidget);
  });

  testWidgets('获取模型列表之后 API Key 还在输入框里', (tester) async {
    final api = _ProviderUiApi();
    await tester.pumpWidget(DiaryApp(api: api));
    await tester.pumpAndSettle();
    await tester.tap(find.text('我的').last);
    await tester.pumpAndSettle();
    await tester.ensureVisible(find.text('模型服务'));
    await tester.tap(find.text('模型服务'));
    await tester.pumpAndSettle();

    final keyField = find.widgetWithText(TextField, 'API Key');
    await tester.enterText(keyField, 'sk-keep-me');
    await tester.pumpAndSettle();

    await tester.ensureVisible(find.text('获取模型列表'));
    await tester.tap(find.text('获取模型列表'));
    await tester.pumpAndSettle();

    // 拉完列表还要接着测试和保存，密钥被清掉就等于让用户重打一遍。
    expect(
      tester.widget<TextField>(keyField).controller?.text,
      'sk-keep-me',
      reason: '获取模型列表不该清空 API Key',
    );

    await tester.ensureVisible(find.text('测试连接').last);
    await tester.tap(find.text('测试连接').last);
    await tester.pumpAndSettle();
    expect(
      tester.widget<TextField>(keyField).controller?.text,
      'sk-keep-me',
      reason: '测试连接也不该清空 API Key',
    );
  });

  testWidgets('添加服务里也能测试连接，结果留在表单里', (tester) async {
    final api = _ProviderUiApi();
    await tester.pumpWidget(DiaryApp(api: api));
    await tester.pumpAndSettle();
    await tester.tap(find.text('我的').last);
    await tester.pumpAndSettle();
    await tester.ensureVisible(find.text('模型服务'));
    await tester.tap(find.text('模型服务'));
    await tester.pumpAndSettle();

    // 表单里那个「测试连接」是第二个（第一个在已配置卡片上）。
    final formTest = find.text('测试连接').last;
    await tester.ensureVisible(formTest);
    await tester.tap(formTest);
    await tester.pumpAndSettle();

    // 没填密钥：先要密钥，不发请求。
    expect(api.testCalls, 0, reason: '没有密钥不该真的去测');
    expect(find.textContaining('先填 API Key'), findsOneWidget);

    await tester.enterText(
      find.widgetWithText(TextField, 'API Key'),
      'sk-test',
    );
    await tester.tap(find.text('测试连接').last);
    await tester.pumpAndSettle();

    expect(api.testCalls, 1, reason: '填好之后点一次测一次');
    expect(find.text('连接正常。'), findsWidgets);
  });

  testWidgets('连接结果写在该服务的卡片里，不跑到页面底部', (tester) async {
    final api = _ProviderUiApi();
    await tester.pumpWidget(DiaryApp(api: api));
    await tester.pumpAndSettle();
    await tester.tap(find.text('我的').last);
    await tester.pumpAndSettle();
    await tester.ensureVisible(find.text('模型服务'));
    await tester.tap(find.text('模型服务'));
    await tester.pumpAndSettle();

    // 「测试连接」现在卡片上和表单里各有一个；这条测的是卡片上那个。
    await tester.ensureVisible(find.text('测试连接').first);
    await tester.tap(find.text('测试连接').first);
    await tester.pumpAndSettle();

    final note = find.text('连接正常。');
    expect(note, findsOneWidget);
    // 位置断言：提示要排在「添加服务」标题**上面**（即在已配置卡片里）。
    final noteY = tester.getTopLeft(note).dy;
    final addCardY = tester.getTopLeft(find.text('添加服务')).dy;
    expect(noteY, lessThan(addCardY), reason: '连接结果不该掉到页面底部');
  });

  testWidgets('「说大话」在编辑卡片的标题行右侧', (tester) async {
    await openApp(tester, MockDiaryApi());
    final button = find.byKey(const Key('enter-big-talk'));
    expect(button, findsOneWidget);

    // 位置断言：和「此刻的记录」同一行，且在它右边。
    final title = tester.getTopLeft(find.text('此刻的记录'));
    final btn = tester.getTopLeft(button);
    expect(
      (btn.dy - title.dy).abs(),
      lessThan(24),
      reason: '按钮该和标题同一行，不该跑到左栏或别处',
    );
    expect(btn.dx, greaterThan(title.dx), reason: '按钮该在标题右边');
  });

  testWidgets('说大话在窄屏 2 倍字号下也不溢出', (tester) async {
    tester.view.physicalSize = const Size(320, 700);
    tester.view.devicePixelRatio = 1;
    tester.binding.platformDispatcher.textScaleFactorTestValue = 2;
    addTearDown(tester.view.resetPhysicalSize);
    addTearDown(tester.view.resetDevicePixelRatio);
    addTearDown(
      tester.binding.platformDispatcher.clearTextScaleFactorTestValue,
    );

    await openApp(tester, MockDiaryApi());
    await tester.ensureVisible(find.byKey(const Key('enter-big-talk')));
    await tester.tap(find.byKey(const Key('enter-big-talk')));
    await tester.pumpAndSettle();

    expect(tester.takeException(), isNull, reason: '全屏写作面溢出了');
    expect(find.byKey(const Key('capture-editor-bigtalk')), findsOneWidget);

    // 写一堆字也不该溢出：输入区是 expands 的，自己会滚。
    await tester.enterText(
      find.byKey(const Key('capture-editor-bigtalk')),
      '很长的一段话。' * 60,
    );
    await tester.pumpAndSettle();
    expect(tester.takeException(), isNull, reason: '长文本把全屏页撑爆了');

    await tester.tap(find.byKey(const Key('exit-big-talk')));
    await tester.pumpAndSettle();
    expect(tester.takeException(), isNull);
  });

  testWidgets('说大话：全屏写作，文字与普通模式是同一条', (tester) async {
    final dir = Directory.systemTemp.createTempSync('diary-ui');
    addTearDown(() => dir.deleteSync(recursive: true));
    PathProviderPlatform.instance = _FakePathProvider(dir.path);

    await openApp(tester, MockDiaryApi());

    // 普通模式写一句。
    await tester.enterText(
      find.byKey(const Key('capture-editor')),
      '今天风很大',
    );
    await tester.pumpAndSettle();

    // 进全屏：页头收起，输入框换成大画布那个。
    await tester.tap(find.byKey(const Key('enter-big-talk')));
    await tester.pumpAndSettle();
    expect(find.byKey(const Key('capture-editor-bigtalk')), findsOneWidget);
    expect(find.byKey(const Key('capture-editor')), findsNothing);
    // 页头（导航）收起来了。
    expect(find.text('我的'), findsNothing);

    // 全屏里接着写：前面那句还在，说明是同一个 controller。
    await tester.enterText(
      find.byKey(const Key('capture-editor-bigtalk')),
      '今天风很大，把伞吹翻了',
    );
    await tester.pumpAndSettle();

    // 说小话退出：文字跟着回来。
    await tester.tap(find.byKey(const Key('exit-big-talk')));
    await tester.pumpAndSettle();
    expect(find.byKey(const Key('capture-editor')), findsOneWidget);
    expect(find.text('我的'), findsWidgets);
    final field = tester.widget<TextField>(
      find.byKey(const Key('capture-editor')),
    );
    expect(field.controller!.text, '今天风很大，把伞吹翻了');
  });

  testWidgets('远端服务在模型服务里授权一次，日记页不再反复打勾', (tester) async {
    final dir = Directory.systemTemp.createTempSync('diary-ui');
    addTearDown(() => dir.deleteSync(recursive: true));
    PathProviderPlatform.instance = _FakePathProvider(dir.path);

    final api = _ProviderUiApi();
    await tester.pumpWidget(DiaryApp(api: api));
    await tester.pumpAndSettle();

    // ① 没授权时：日记页拦住，并指路去「模型服务」。
    await tester.tap(find.text('日记').last);
    // 控制器初始化要读本地 UI 文件，是真实 I/O：让出事件循环才推得动。
    await settleRealIo(tester);
    expect(find.textContaining('还没授权发送材料'), findsOneWidget);
    expect(find.text('去模型服务授权'), findsOneWidget);
    // 老那个「每次都要打勾」的复选框不该再出现。
    expect(find.textContaining('我确认把当天材料发送到上述服务'), findsNothing);

    // ② 去模型服务授权。
    await tester.tap(find.text('去模型服务授权'));
    await tester.pumpAndSettle();
    // 模型服务页也要读授权表（真实 I/O），读完才认得出「授权发送」这个动作。
    await settleRealIo(tester);
    await tester.ensureVisible(find.text('授权发送'));
    await tester.pumpAndSettle();
    await tester.tap(find.text('授权发送'));
    await tester.pump();
    await settleRealIo(tester);
    expect(find.textContaining('已授权发送材料'), findsOneWidget);

    // ③ 回日记页：不再要求授权，也不再出现复选框。
    await tester.tap(find.text('日记').last);
    await settleRealIo(tester);
    expect(find.textContaining('还没授权发送材料'), findsNothing);
    expect(find.text('去模型服务授权'), findsNothing);
    expect(find.textContaining('我确认把当天材料发送到上述服务'), findsNothing);
  });

  testWidgets('日记页的空态是一行字，不是告警框', (tester) async {
    final dir = Directory.systemTemp.createTempSync('diary-ui');
    addTearDown(() => dir.deleteSync(recursive: true));
    PathProviderPlatform.instance = _FakePathProvider(dir.path);

    final api = _ProviderUiApi();
    await tester.pumpWidget(DiaryApp(api: api));
    await tester.pumpAndSettle();
    await tester.tap(find.text('日记').last);
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 300));

    // 空态还在，但**走的是淡字样式**，不是告警框。
    // 两者文字一样，靠 `_hint` 的 key 区分：换回 `_notice` 这个 key 就没了。
    for (final text in ['还没有正文。', '还没有候选。']) {
      expect(find.text(text), findsOneWidget);
      expect(
        find.byKey(Key('hint-$text')),
        findsOneWidget,
        reason: '「$text」该是一行淡字，不该被框起来',
      );
    }
    // 底部那句解释「收起」的框已经删掉。
    expect(find.textContaining('收起只隐藏'), findsNothing);
  });

  testWidgets('片段页不再堆「正在接入」这类说明', (tester) async {
    await openApp(tester, MockDiaryApi());
    await tester.tap(find.text('片段').last);
    await tester.pumpAndSettle();

    // 空态只留一行淡字，不再是一个带图标的框。
    expect(find.text('还没有记录。'), findsOneWidget);
    // 「我们还没做完」属于开发进度，不该出现在界面里。
    expect(find.textContaining('正在接入'), findsNothing);
    expect(find.textContaining('完整时间线'), findsNothing);
    expect(find.textContaining('后续接入'), findsNothing);
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
      await tester.ensureVisible(find.text('搜索索引 · 未建立'));
      await tester.tap(find.text('搜索索引 · 未建立'));
      await tester.pumpAndSettle();

      // 一句话说完：要配模型目录，且不会自动下载（别让人干等）。
      expect(find.textContaining('语义检索与转写要先配好本机模型目录'), findsOneWidget);
      expect(find.textContaining('不会自动下载'), findsOneWidget);
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

  testWidgets('滚到底再点「获取模型列表」，页面不许跳回上面', (tester) async {
    final api = _ProviderUiApi();
    await tester.pumpWidget(DiaryApp(api: api));
    await tester.pumpAndSettle();
    await tester.tap(find.text('我的').last);
    await tester.pumpAndSettle();
    await tester.ensureVisible(find.text('模型服务'));
    await tester.tap(find.text('模型服务'));
    await tester.pumpAndSettle();

    // 先拉一次，让表单里出现候选 chip（内容变高）。
    await tester.enterText(
      find.widgetWithText(TextField, 'API Key'),
      'sk-test',
    );
    await tester.ensureVisible(find.text('获取模型列表'));
    await tester.pumpAndSettle();
    await tester.tap(find.text('获取模型列表'));
    await tester.pumpAndSettle();
    expect(find.text('deepseek-reasoner'), findsOneWidget);

    // 滚到最底下——用户点按钮时通常就在这儿。
    final scrollable = find.byType(Scrollable).first;
    await tester.drag(scrollable, const Offset(0, -5000));
    await tester.pumpAndSettle();
    final before = tester.state<ScrollableState>(scrollable).position.pixels;
    expect(before, greaterThan(0), reason: '这一页要真的能滚，否则测不出问题');

    await tester.ensureVisible(find.text('获取模型列表'));
    await tester.pumpAndSettle();
    await tester.tap(find.text('获取模型列表'));
    await tester.pumpAndSettle();

    final after = tester.state<ScrollableState>(scrollable).position.pixels;
    expect(
      after,
      closeTo(before, 1),
      reason: '点按钮把内容清空会让滚动位置被夹回去，看起来就是「跳回最上面」',
    );
  });
}

/// 只给「模型服务」页用的假 API：已配置一个在线服务，`/models` 返回两个模型。
class _ProviderUiApi extends MockDiaryApi {
  int modelCalls = 0;
  int testCalls = 0;

  @override
  Future<List<Provider>> listProviders() async => const [
    Provider(
      id: 'prv_deepseek',
      label: 'DeepSeek',
      adapterId: 'openai_compatible',
      capabilities: {ProviderCapability.generateText},
      enabled: true,
      endpoint: 'https://api.deepseek.com',
      model: 'deepseek-chat',
      credentialRef: 'secret-1',
      testStatus: ProviderTestStatus.ok,
    ),
  ];

  @override
  Future<List<ProviderModel>> listProviderModels({
    required String providerId,
  }) async {
    modelCalls++;
    return const [
      ProviderModel(id: 'deepseek-chat'),
      ProviderModel(id: 'deepseek-reasoner'),
    ];
  }

  @override
  Future<DiaryDay> getDay(String dayKey) async => DiaryDay(
    dayKey: dayKey,
    timeZone: 'Asia/Shanghai',
    updatedAt: DateTime.utc(2026, 11, 5),
  );

  @override
  Future<Provider> saveProvider(Provider provider) async => provider;

  @override
  Future<Provider> saveProviderCredential({
    required String providerId,
    required String secret,
  }) async => const Provider(
    id: 'prv_deepseek',
    label: 'DeepSeek',
    adapterId: 'openai_compatible',
    capabilities: {ProviderCapability.generateText},
    enabled: true,
    endpoint: 'https://api.deepseek.com',
    model: 'deepseek-chat',
    credentialRef: 'secret-1',
    testStatus: ProviderTestStatus.ok,
  );

  @override
  Future<ProviderTestReport> testProvider({
    required String providerId,
    required Set<ProviderCapability> scope,
  }) async {
    testCalls++;
    return ProviderTestReport(
    providerId: providerId,
      results: const {ProviderCapability.generateText: ProviderTestStatus.ok},
      actualCallMade: true,
    );
  }
}

import 'package:diary_api/diary_api.dart';
import 'package:diary_mock/diary_mock.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:unwritten_diary/src/plugins_panel.dart';
import 'package:unwritten_diary/src/sketch_ui.dart';

/// 插件页以前是预览：一张写死的卡片，开关只改内存变量。现在每个按钮都落到核心。
class _PluginsApi extends MockDiaryApi {
  final List<Plugin> plugins = [];
  final List<String> calls = [];
  final List<Set<String>> grants = [];
  PluginPackageReport? report;
  String? installError;

  @override
  Future<List<Plugin>> listPlugins() async => List.of(plugins);

  @override
  Future<Plugin> enablePlugin(String pluginId) async {
    calls.add('enable:$pluginId');
    final plugin = plugins.firstWhere((item) => item.id == pluginId);
    final updated = plugin.copyWith(state: PluginState.enabled);
    plugins[plugins.indexOf(plugin)] = updated;
    return updated;
  }

  @override
  Future<Plugin> disablePlugin(String pluginId) async {
    calls.add('disable:$pluginId');
    final plugin = plugins.firstWhere((item) => item.id == pluginId);
    final updated = plugin.copyWith(state: PluginState.disabled);
    plugins[plugins.indexOf(plugin)] = updated;
    return updated;
  }

  @override
  Future<void> uninstallPlugin(String pluginId) async {
    calls.add('uninstall:$pluginId');
    plugins.removeWhere((item) => item.id == pluginId);
  }

  PluginRunResult? runResult;
  final List<String> runs = [];
  final List<Map<String, Object?>> invocations = [];
  List<StyleProfile> styles = const [];
  List<DiaryDay> days = const [];

  @override
  Future<PluginInvocationTicket> invokePlugin({
    required String pluginId,
    required String command,
    List<String>? captureIds,
    String? templateId,
    String? format,
    String? inputText,
    List<String>? versionIds,
    String? fromDayKey,
    String? toDayKey,
    StyleProfile? styleProfile,
  }) async {
    runs.add('$pluginId@$command');
    invocations.add({
      'templateId': templateId,
      'styleProfile': styleProfile?.id,
      'fromDayKey': fromDayKey,
      'toDayKey': toDayKey,
      'format': format,
      'versionIds': versionIds,
    });
    return const PluginInvocationTicket(jobId: 'job_plugin_1');
  }

  @override
  Future<PluginRunResult?> pluginRun(String jobId) async => runResult;

  @override
  Future<List<StyleProfile>> listStyles() async => styles;

  /// 按 `pageSize` 切页：用来验「日记落在第二页时也要能找到」。
  int dayPagesRead = 0;

  @override
  Future<DiaryDayPage> listDays({
    String? fromDayKey,
    String? toDayKey,
    String? cursor,
    int pageSize = 20,
  }) async {
    dayPagesRead++;
    final start = cursor == null ? 0 : int.parse(cursor);
    final end = (start + pageSize).clamp(0, days.length);
    final next = end < days.length ? '$end' : null;
    return DiaryDayPage(days: days.sublist(start, end), nextCursor: next);
  }

  @override
  Future<PluginPackageReport> inspectPluginPackage(String ticket) async {
    calls.add('inspect:$ticket');
    return report ??
        const PluginPackageReport(
          packageId: 'demo.export',
          version: '0.1.0',
          apiRange: '>=1.0.0 <2.0.0',
          compatible: true,
        );
  }

  @override
  Future<Plugin> installPlugin({
    required String ticket,
    required Set<String> grantedPermissions,
  }) async {
    calls.add('install:$ticket');
    grants.add(grantedPermissions);
    if (installError != null) {
      throw DiaryException(
        code: DiaryErrorCode.invalidArgument,
        message: installError!,
      );
    }
    final plugin = Plugin(
      id: 'demo.export',
      version: '0.1.0',
      apiRange: '>=1.0.0 <2.0.0',
      entryKind: PluginEntryKind.declarative,
      state: PluginState.disabled,
      grantedPermissions: grantedPermissions,
    );
    plugins.add(plugin);
    return plugin;
  }
}

const _allCapabilities = {
  'plugins.list',
  'plugins.invoke',
  'plugins.inspectPackage',
  'plugins.install',
  'plugins.enable',
  'plugins.disable',
  'plugins.uninstall',
};

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();

  Future<void> pump(
    WidgetTester tester,
    _PluginsApi api, {
    Set<String> capabilities = _allCapabilities,
    String? package,
  }) async {
    tester.view.physicalSize = const Size(1200, 2000);
    tester.view.devicePixelRatio = 1;
    addTearDown(tester.view.resetPhysicalSize);
    addTearDown(tester.view.resetDevicePixelRatio);
    await tester.pumpWidget(
      MaterialApp(
        home: Scaffold(
          body: SingleChildScrollView(
            child: PluginsPanel(
              api: api,
              pickPackage: () async => package,
              capabilities: capabilities,
            ),
          ),
        ),
      ),
    );
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 50));
  }

  testWidgets('列出真实安装的插件，开关落到核心', (tester) async {
    final api = _PluginsApi()
      ..plugins.add(
        const Plugin(
          id: 'demo.export',
          version: '0.1.0',
          apiRange: '>=1.0.0 <2.0.0',
          entryKind: PluginEntryKind.declarative,
          state: PluginState.disabled,
          grantedPermissions: {'export_write'},
        ),
      );
    await pump(tester, api);

    expect(find.text('demo.export'), findsOneWidget);
    expect(find.textContaining('export_write'), findsOneWidget);

    await tester.tap(find.byKey(const ValueKey('plugin-switch-demo.export')));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 50));

    expect(api.calls, contains('enable:demo.export'));
    expect(find.text('已启用'), findsOneWidget);
  });

  testWidgets('卸载要先确认，取消就不动', (tester) async {
    final api = _PluginsApi()
      ..plugins.add(
        const Plugin(
          id: 'demo.export',
          version: '0.1.0',
          apiRange: '>=1.0.0 <2.0.0',
          entryKind: PluginEntryKind.declarative,
          state: PluginState.enabled,
        ),
      );
    await pump(tester, api);

    await tester.tap(find.byKey(const ValueKey('plugin-uninstall-demo.export')));
    await tester.pumpAndSettle();
    await tester.tap(find.text('取消'));
    await tester.pumpAndSettle();
    expect(api.calls, isNot(contains('uninstall:demo.export')));

    await tester.tap(find.byKey(const ValueKey('plugin-uninstall-demo.export')));
    await tester.pumpAndSettle();
    // 卡片上也有一个「卸载」按钮，这里只点对话框里那个。
    await tester.tap(
      find.descendant(
        of: find.byType(AlertDialog),
        matching: find.text('卸载'),
      ),
    );
    await tester.pumpAndSettle();
    expect(api.calls, contains('uninstall:demo.export'));
  });

  testWidgets('安装要经过检查与逐项授权', (tester) async {
    final api = _PluginsApi()
      ..report = const PluginPackageReport(
        packageId: 'demo.export',
        version: '0.2.0',
        apiRange: '>=1.0.0 <2.0.0',
        compatible: true,
        requestedPermissions: {'export_write', 'read_captures'},
      );
    await pump(tester, api, package: '/tmp/demo.zip');

    await tester.tap(find.byKey(const ValueKey('plugin-install')));
    await tester.pumpAndSettle();

    expect(api.calls, contains('inspect:/tmp/demo.zip'));
    // 权限清单里两项都在，默认全勾；取消一项之后只授权剩下的。
    expect(find.text('export_write'), findsOneWidget);
    await tester.tap(find.text('read_captures'));
    await tester.pumpAndSettle();
    await tester.tap(find.text('安装'));
    await tester.pumpAndSettle();

    expect(api.grants.single, {'export_write'});
  });

  testWidgets('检查不通过的包装不上，也不弹授权框', (tester) async {
    final api = _PluginsApi()
      ..report = const PluginPackageReport(
        packageId: 'demo.export',
        version: '0.2.0',
        apiRange: '>=2.0.0',
        compatible: false,
        issues: ['接口版本不兼容'],
      );
    await pump(tester, api, package: '/tmp/demo.zip');

    await tester.tap(find.byKey(const ValueKey('plugin-install')));
    await tester.pumpAndSettle();

    expect(find.textContaining('接口版本不兼容'), findsOneWidget);
    expect(api.calls, isNot(contains('install:/tmp/demo.zip')));
  });

  testWidgets('运行插件：结果读回来，候选内容不自动写进日记', (tester) async {
    final api = _PluginsApi()
      ..plugins.add(
        const Plugin(
          id: 'demo.export',
          version: '0.1.0',
          apiRange: '>=1.0.0 <2.0.0',
          entryKind: PluginEntryKind.declarative,
          state: PluginState.enabled,
          activationEvents: ['export.start'],
        ),
      )
      ..runResult = const PluginRunResult(
        runId: 'run_1',
        pluginId: 'demo.export',
        activation: 'export.start',
        status: 'succeeded',
        output: '# 2026-11-08',
        durationMs: 12,
      );
    await pump(tester, api);

    await tester.tap(
      find.byKey(const ValueKey('plugin-run-demo.export-export.start')),
    );
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 50));

    expect(api.runs, ['demo.export@export.start']);
    expect(find.textContaining('跑完了'), findsOneWidget);
    expect(find.textContaining('# 2026-11-08'), findsOneWidget);
    expect(find.textContaining('不会自动写进日记'), findsOneWidget);
  });

  testWidgets('插件报错时如实显示，不假装成功', (tester) async {
    final api = _PluginsApi()
      ..plugins.add(
        const Plugin(
          id: 'demo.export',
          version: '0.1.0',
          apiRange: '>=1.0.0 <2.0.0',
          entryKind: PluginEntryKind.code,
          state: PluginState.enabled,
          // 代码型导出插件要选一天；这条测的是「失败如实显示」，所以给一天。
          activationEvents: ['export.start'],
        ),
      )
      ..days = [
        DiaryDay(
          dayKey: '2026-11-08',
          timeZone: 'Asia/Shanghai',
          updatedAt: DateTime.utc(2026, 11, 8),
          currentVersionId: 'ver-1',
        ),
      ]
      ..runResult = const PluginRunResult(
        runId: 'run_2',
        pluginId: 'demo.export',
        activation: 'export.start',
        status: 'failed',
        errorCode: 'plugin_failed',
        errorMessage: '指令预算用完',
      );
    await pump(tester, api);

    await tester.tap(
      find.byKey(const ValueKey('plugin-run-demo.export-export.start')),
    );
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 50));

    expect(find.textContaining('没有成功'), findsOneWidget);
    expect(find.textContaining('指令预算用完'), findsWidgets);
  });

  testWidgets('风格模板：选模板 + 选风格，两样都传下去', (tester) async {
    // 以前只传 pluginId + command，按下去只会得到「声明式插件需要 templateId」，
    // 补了模板还会得到「风格模板需要 styleProfile」。
    final api = _PluginsApi()
      ..plugins.add(
        const Plugin(
          id: 'demo.style',
          version: '0.1.0',
          apiRange: '>=1.0.0 <2.0.0',
          entryKind: PluginEntryKind.declarative,
          state: PluginState.enabled,
          activationEvents: ['organize.template.select'],
          templates: [
            PluginTemplate(
              id: 'style.daily-note',
              kind: 'style',
              label: '日常便签',
              format: 'style_instruction',
            ),
          ],
        ),
      )
      ..styles = [
        StyleProfile(
          id: 'style-1',
          label: '我的日记风格',
          revision: 3,
          updatedAt: DateTime.utc(2026, 11, 8),
          tone: '平静',
        ),
      ]
      ..runResult = const PluginRunResult(
        runId: 'run_1',
        pluginId: 'demo.style',
        activation: 'organize.template.select',
        status: 'succeeded',
        output: '篇幅：三百字以内',
      );
    await pump(tester, api);

    expect(find.text('用哪个模板'), findsOneWidget);
    expect(find.text('用哪份风格'), findsOneWidget);

    await tester.tap(
      find.byKey(const ValueKey('plugin-run-demo.style-organize.template.select')),
    );
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 50));

    expect(api.invocations.single['templateId'], 'style.daily-note');
    expect(api.invocations.single['styleProfile'], 'style-1');
    expect(find.textContaining('篇幅：三百字以内'), findsOneWidget);
  });

  testWidgets('没有保存过风格时不假装能跑风格模板', (tester) async {
    final api = _PluginsApi()
      ..plugins.add(
        const Plugin(
          id: 'demo.style',
          version: '0.1.0',
          apiRange: '>=1.0.0 <2.0.0',
          entryKind: PluginEntryKind.declarative,
          state: PluginState.enabled,
          activationEvents: ['organize.template.select'],
          templates: [
            PluginTemplate(
              id: 'style.daily-note',
              kind: 'style',
              label: '日常便签',
              format: 'style_instruction',
            ),
          ],
        ),
      );
    await pump(tester, api);

    await tester.tap(
      find.byKey(const ValueKey('plugin-run-demo.style-organize.template.select')),
    );
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 50));

    expect(api.runs, isEmpty, reason: '没有风格就不该发起调用');
    expect(find.textContaining('还没有保存过风格'), findsWidgets);
  });

  testWidgets('导出模板：把日期范围传下去', (tester) async {
    final api = _PluginsApi()
      ..plugins.add(
        const Plugin(
          id: 'demo.export',
          version: '0.1.0',
          apiRange: '>=1.0.0 <2.0.0',
          entryKind: PluginEntryKind.declarative,
          state: PluginState.enabled,
          activationEvents: ['export.start'],
          templates: [
            PluginTemplate(
              id: 'export.markdown',
              kind: 'export',
              label: '按天导出 Markdown',
              format: 'markdown',
            ),
          ],
        ),
      )
      ..runResult = const PluginRunResult(
        runId: 'run_2',
        pluginId: 'demo.export',
        activation: 'export.start',
        status: 'succeeded',
      );
    await pump(tester, api);

    await tester.tap(
      find.byKey(const ValueKey('plugin-run-demo.export-export.start')),
    );
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 50));

    final invocation = api.invocations.single;
    expect(invocation['templateId'], 'export.markdown');
    expect(invocation['format'], 'markdown');
    expect(invocation['fromDayKey'], isNotNull);
    expect(invocation['toDayKey'], isNotNull);
  });

  testWidgets('代码型导出插件：选一天，把当前版本传下去', (tester) async {
    // 仓库自带的 `example.export-diary-markdown` 是 Wasm 插件，`templates` 是空的。
    // 它要的是「用户选定的日记版本」——不传就返回 2（没选日记）。
    final api = _PluginsApi()
      ..plugins.add(
        const Plugin(
          id: 'example.export-diary-markdown',
          version: '1.0.0',
          apiRange: '>=1.0 <2.0',
          entryKind: PluginEntryKind.code,
          state: PluginState.enabled,
          activationEvents: ['export.start'],
        ),
      )
      ..days = [
        DiaryDay(
          dayKey: '2026-11-08',
          timeZone: 'Asia/Shanghai',
          updatedAt: DateTime.utc(2026, 11, 8),
          currentVersionId: 'ver-2026-11-08',
        ),
      ]
      ..runResult = const PluginRunResult(
        runId: 'run_export',
        pluginId: 'example.export-diary-markdown',
        activation: 'export.start',
        status: 'succeeded',
        output: '# 2026-11-08',
      );
    await pump(tester, api);

    expect(find.text('导出哪一天的日记'), findsOneWidget);
    await tester.tap(
      find.byKey(
        const ValueKey('plugin-run-example.export-diary-markdown-export.start'),
      ),
    );
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 50));

    expect(api.invocations.single['versionIds'], ['ver-2026-11-08']);
    expect(find.textContaining('跑完了'), findsOneWidget);
  });

  testWidgets('日记落在后面的页里也要能找到（不能只看第一页）', (tester) async {
    // 32 天有记录、唯一有日记的那天在第二页：只取第一页会误报「还没有任何一天
    // 生成过日记」——用户明明生成过。
    final api = _PluginsApi()
      ..plugins.add(
        const Plugin(
          id: 'example.export-diary-markdown',
          version: '1.0.0',
          apiRange: '>=1.0 <2.0',
          entryKind: PluginEntryKind.code,
          state: PluginState.enabled,
          activationEvents: ['export.start'],
        ),
      )
      // 250 天有记录（一页 100 天 ⇒ 3 页），唯一有日记的那天在最后一页。
      ..days = [
        for (var index = 0; index < 250; index++)
          DiaryDay(
            dayKey: '2025-01-01+$index',
            timeZone: 'Asia/Shanghai',
            updatedAt: DateTime.utc(2025, 1, 1).add(Duration(days: index)),
          ),
        DiaryDay(
          dayKey: '2026-11-10',
          timeZone: 'Asia/Shanghai',
          updatedAt: DateTime.utc(2026, 11, 10),
          currentVersionId: 'ver-late',
        ),
      ]
      ..runResult = const PluginRunResult(
        runId: 'run_late',
        pluginId: 'example.export-diary-markdown',
        activation: 'export.start',
        status: 'succeeded',
      );
    await pump(tester, api);

    expect(api.dayPagesRead, greaterThan(1), reason: '必须翻页才能找到那天');
    expect(find.textContaining('2026-11-10'), findsOneWidget);

    await tester.tap(
      find.byKey(
        const ValueKey('plugin-run-example.export-diary-markdown-export.start'),
      ),
    );
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 50));
    expect(api.invocations.single['versionIds'], ['ver-late']);
  });

  testWidgets('库里没有生成过日记时，代码型导出插件不假装能跑', (tester) async {
    final api = _PluginsApi()
      ..plugins.add(
        const Plugin(
          id: 'example.export-diary-markdown',
          version: '1.0.0',
          apiRange: '>=1.0 <2.0',
          entryKind: PluginEntryKind.code,
          state: PluginState.enabled,
          activationEvents: ['export.start'],
        ),
      );
    await pump(tester, api);

    await tester.tap(
      find.byKey(
        const ValueKey('plugin-run-example.export-diary-markdown-export.start'),
      ),
    );
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 50));

    expect(api.runs, isEmpty, reason: '没有版本就不该发起调用');
    expect(find.textContaining('还没有任何一天生成过日记'), findsWidgets);
  });

  testWidgets('没有 invoke 能力时不画运行按钮', (tester) async {
    final api = _PluginsApi()
      ..plugins.add(
        const Plugin(
          id: 'demo.export',
          version: '0.1.0',
          apiRange: '>=1.0.0 <2.0.0',
          entryKind: PluginEntryKind.declarative,
          state: PluginState.enabled,
          activationEvents: ['export.start'],
        ),
      );
    await pump(
      tester,
      api,
      capabilities: const {'plugins.list', 'plugins.enable', 'plugins.disable'},
    );
    expect(
      find.byKey(const ValueKey('plugin-run-demo.export-export.start')),
      findsNothing,
    );
  });

  testWidgets('核心没有插件能力时不画能按的入口', (tester) async {
    final api = _PluginsApi();
    await pump(tester, api, capabilities: const {'plugins.list'});

    final install = tester.widget<SketchAction>(
      find.byKey(const ValueKey('plugin-install')),
    );
    expect(install.onPressed, isNull, reason: '没有能力就不该能按');
    expect(find.textContaining('不支持安装插件'), findsOneWidget);
  });
}

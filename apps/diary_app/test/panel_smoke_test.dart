import 'dart:io';

import 'package:diary_api/diary_api.dart';
import 'package:diary_mock/diary_mock.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:path_provider_platform_interface/path_provider_platform_interface.dart';
import 'package:unwritten_diary/main.dart';
import 'package:unwritten_diary/src/ui_strings.dart';

/// 测试里的 support 目录。
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


/// 尽量把面板要用到的读路径都实现掉：只有走到真实分支，按钮才会真的被启用、
/// 才会暴露「点下去就炸」的问题。全用 `MockDiaryApi` 的默认实现时，界面一路显示
/// 「读不到…」，按钮多半是禁用的，点了也看不出问题。
class _RichApi extends MockDiaryApi {
  final List<Provider> providers = [
    const Provider(
      id: 'prv_a',
      label: 'DeepSeek',
      adapterId: 'openai_compatible',
      capabilities: {ProviderCapability.generateText},
      enabled: true,
      endpoint: 'https://api.deepseek.com',
      model: 'deepseek-flash',
      credentialRef: 'secret-1',
      testStatus: ProviderTestStatus.ok,
    ),
    const Provider(
      id: 'prv_b',
      label: '本机 Ollama',
      adapterId: 'openai_compatible',
      capabilities: {ProviderCapability.generateText},
      enabled: false,
      endpoint: 'http://127.0.0.1:11434/v1',
      model: 'qwen',
      testStatus: ProviderTestStatus.untested,
    ),
  ];

  @override
  Future<List<Provider>> listProviders() async => List.of(providers);

  @override
  Future<Provider> saveProvider(Provider provider) async => provider;

  @override
  Future<Provider> saveProviderCredential({
    required String providerId,
    required String secret,
  }) async => providers.firstWhere((item) => item.id == providerId);

  @override
  Future<List<ProviderModel>> listProviderModels({
    required String providerId,
  }) async => const [
    ProviderModel(id: 'deepseek-flash'),
    ProviderModel(id: 'deepseek-v4-pro'),
  ];

  @override
  Future<ProviderTestReport> testProvider({
    required String providerId,
    required Set<ProviderCapability> scope,
  }) async => ProviderTestReport(
    providerId: providerId,
    results: const {ProviderCapability.generateText: ProviderTestStatus.ok},
    actualCallMade: true,
  );

  @override
  Future<Provider> selectGenerationProvider({
    required String providerId,
  }) async => providers.firstWhere((item) => item.id == providerId);

  @override
  Future<DiaryDay> getDay(String dayKey) async => DiaryDay(
    dayKey: dayKey,
    timeZone: 'Asia/Shanghai',
    updatedAt: DateTime.utc(2026, 11, 7),
  );

  @override
  Future<DiaryVersionPage> listVersions(
    String dayKey, {
    String? cursor,
    int pageSize = 20,
  }) async => const DiaryVersionPage(versions: [], nextCursor: null);

  @override
  Future<JobPage> listJobs({
    String? cursor,
    int pageSize = 50,
    Set<JobState>? states,
  }) async => const JobPage(jobs: [], nextCursor: null);

  @override
  Future<DiaryDayPage> listDays({
    String? cursor,
    String? fromDayKey,
    int pageSize = 30,
    String? toDayKey,
  }) async => const DiaryDayPage(days: [], nextCursor: null);
}

/// 把「我的」页里每个面板都打开，**把里面的每个按钮都点一遍**，
/// 任何一次点击抛出异常都算失败。
///
/// 这条测试的意义：之前的几个 bug（`firstWhere` 抛 StateError、`const {}` 不可变）
/// 都不是「页面打不开」，而是「某个按钮点下去才炸」。只渲染不点击的测试抓不到。
void main() {
  TestWidgetsFlutterBinding.ensureInitialized();
  setUpAll(() async => uiStrings.load());

  const panels = [
    '搜索索引',
    '风格偏好',
    '模型服务',
    '整理时机',
    '任务中心',
    '插件',
    '备份与恢复',
    '存储与回收站',
    '封面与加载动画',
  ];

  // 主页面（记录 / 日记 / 搜索 / 片段）里的按钮同样要点一遍。
  for (final tab in ['记录', '日记', '搜索', '片段']) {
    testWidgets('$tab 页里点每个按钮都不炸', (tester) async {
      final dir = Directory.systemTemp.createTempSync('tab-smoke');
      addTearDown(() => dir.deleteSync(recursive: true));
      PathProviderPlatform.instance = _FakePathProvider(dir.path);

      tester.view.physicalSize = const Size(1400, 1000);
      tester.view.devicePixelRatio = 1;
      addTearDown(tester.view.resetPhysicalSize);
      addTearDown(tester.view.resetDevicePixelRatio);

      await tester.pumpWidget(DiaryApp(api: _RichApi()));
      await tester.pumpAndSettle();
      await tester.tap(find.text(tab).last);
      await tester.pumpAndSettle();

      bool isButton(Widget widget) => widget is InkWell && widget.onTap != null;
      final labels = <String>[];
      for (final element in find.byWidgetPredicate(isButton).evaluate()) {
        final texts = find
            .descendant(
              of: find.byWidget(element.widget),
              matching: find.byType(Text),
            )
            .evaluate()
            .map((item) => (item.widget as Text).data)
            .whereType<String>();
        if (texts.isNotEmpty) labels.add(texts.first);
      }
      expect(labels, isNotEmpty, reason: '$tab 页里一个按钮都没找到');

      const nav = {'记录', '日记', '搜索', '片段', '我的', '返回'};
      for (final label in labels.where((item) => !nav.contains(item))) {
        final matches = find.text(label);
        if (matches.evaluate().isEmpty) continue;
        final target = matches.first;
        var tapped = true;
        try {
          await tester.ensureVisible(target);
          await tester.tap(target, warnIfMissed: false);
        } on Object {
          tapped = false;
        }
        if (!tapped) continue;
        await tester.pump(const Duration(milliseconds: 200));
        expect(
          tester.takeException(),
          isNull,
          reason: '$tab 页里点「$label」抛异常了',
        );
      }
    });
  }

  for (final panel in panels) {
    testWidgets('$panel 面板里点每个按钮都不炸', (tester) async {
      final dir = Directory.systemTemp.createTempSync('panel-smoke');
      addTearDown(() => dir.deleteSync(recursive: true));
      PathProviderPlatform.instance = _FakePathProvider(dir.path);

      tester.view.physicalSize = const Size(1400, 1000);
      tester.view.devicePixelRatio = 1;
      addTearDown(tester.view.resetPhysicalSize);
      addTearDown(tester.view.resetDevicePixelRatio);

      await tester.pumpWidget(DiaryApp(api: _RichApi()));
      await tester.pumpAndSettle();
      await tester.tap(find.text('我的').last);
      await tester.pumpAndSettle();
      await tester.ensureVisible(find.text(panel));
      await tester.tap(find.text(panel));
      await tester.pumpAndSettle();

      // 把面板里所有可点的东西点一遍。点完不要求有结果，只要求不抛异常。
      // 先把「当前面板里所有按钮的文字」收集下来，再逐个按文字去点。
      // 按下标遍历不行：点一个按钮会改界面，后面的下标就错位了。
      bool isButton(Widget widget) => widget is InkWell && widget.onTap != null;
      final labels = <String>[];
      for (final element in find.byWidgetPredicate(isButton).evaluate()) {
        final texts = find
            .descendant(
              of: find.byWidget(element.widget),
              matching: find.byType(Text),
            )
            .evaluate()
            .map((item) => (item.widget as Text).data)
            .whereType<String>();
        if (texts.isNotEmpty) labels.add(texts.first);
      }
      expect(labels, isNotEmpty, reason: '$panel 面板里一个按钮都没找到，测试本身失效了');

      // 导航类按钮会把我们带离这个面板，后面的按钮就都找不到了——跳过它们。
      const nav = {'记录', '日记', '搜索', '片段', '我的', '返回'};
      for (final label in labels.where((item) => !nav.contains(item))) {
        final matches = find.text(label);
        if (matches.evaluate().isEmpty) continue;
        final target = matches.first;
        var tapped = true;
        try {
          await tester.ensureVisible(target);
          await tester.tap(target, warnIfMissed: false);
        } on Object {
          // 找不到/点不到（被遮挡、已卸载）就跳过这一条。
          tapped = false;
        }
        if (!tapped) continue;
        // **pump 必须在 try 外面**：widget 里的异常是在下一帧抛的，
        // 把它一起 catch 掉，这个测试就变成了「什么都测不到」。
        await tester.pump(const Duration(milliseconds: 200));
        expect(
          tester.takeException(),
          isNull,
          reason: '$panel 面板里点「$label」抛异常了',
        );
      }
    });
  }
}

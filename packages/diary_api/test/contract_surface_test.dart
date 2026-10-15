import 'dart:convert';
import 'dart:io';

import 'package:diary_api/diary_api.dart';
import 'package:test/test.dart';

/// 契约里的方法名 → Dart 侧方法名。
///
/// 两者刻意不完全同名：契约按能力分组（`assets.open`、`imports.prepare`），
/// Dart 侧用动词开头的可读名字（`openAsset`、`prepareImport`）。这张表就是
/// 两边唯一的对应关系，改动任何一侧都必须同步改这里。
const Map<String, String> kContractToDartMethod = {
  'core.open': 'open',
  'core.snapshot': 'snapshot',
  'core.close': 'close',
  'core.setRuntimeState': 'setRuntimeState',
  'core.events': 'events',
  'captures.createDraft': 'createDraft',
  'captures.saveDraft': 'saveDraft',
  'captures.commit': 'commit',
  'captures.get': 'getCapture',
  'captures.list': 'listCaptures',
  'sources.reviseText': 'reviseText',
  'sources.extractedContent': 'extractedContent',
  'sources.previousRevision': 'previousSourceRevision',
  'sources.extract': 'extractSource',
  'captures.moveDay': 'moveDay',
  'captures.trash': 'trashCapture',
  'captures.restore': 'restoreCapture',
  'captures.previewPurge': 'previewPurge',
  'captures.purge': 'purgeCapture',
  'imports.prepare': 'prepareImport',
  'imports.finish': 'finishImport',
  'imports.status': 'importStatus',
  'imports.cancel': 'cancelImport',
  'recordings.prepare': 'prepareRecording',
  'recordings.registerSegment': 'registerSegment',
  'recordings.updateState': 'updateRecordingState',
  'recordings.finalize': 'finalizeRecording',
  'recordings.recover': 'recoverRecording',
  'assets.open': 'openAsset',
  'assets.release': 'releaseAsset',
  'sources.locate': 'locateSource',
  'diary.getDay': 'getDay',
  'diary.listDays': 'listDays',
  'diary.generate': 'generateDiary',
  'diary.listVersions': 'listVersions',
  'diary.getVersion': 'getVersion',
  'diary.saveEdit': 'saveDiaryEdit',
  'diary.setCurrent': 'setCurrentVersion',
  'diary.merge': 'mergeDiary',
  // 契约写作 styles.get / save「profile 或范围」，按范围列出对应 listStyles。
  'styles.get': 'getStyle',
  'styles.list': 'listStyles',
  'styles.save': 'saveStyle',
  'responses.request': 'requestResponse',
  'responses.get': 'getResponse',
  'search.start': 'startSearch',
  'search.nextPage': 'nextPage',
  'search.snapshot': 'searchSnapshot',
  'search.cancel': 'cancelSearch',
  'indexes.status': 'indexesStatus',
  'indexes.rebuild': 'rebuildIndexes',
  'jobs.list': 'listJobs',
  'jobs.get': 'getJob',
  'jobs.retry': 'retryJob',
  'jobs.cancel': 'cancelJob',
  'jobs.nextWakeup': 'nextWakeup',
  'jobs.runDue': 'runDueJobs',
  'jobs.acknowledgeAttention': 'acknowledgeAttention',
  'providers.list': 'listProviders',
  'providers.save': 'saveProvider',
  'providers.remove': 'removeProvider',
  'providers.test': 'testProvider',
  'settings.get': 'getSettings',
  'settings.update': 'updateSettings',
  'plugins.inspectPackage': 'inspectPluginPackage',
  'plugins.install': 'installPlugin',
  'plugins.enable': 'enablePlugin',
  'plugins.disable': 'disablePlugin',
  'plugins.uninstall': 'uninstallPlugin',
  'plugins.list': 'listPlugins',
  'plugins.invoke': 'invokePlugin',
  'backups.create': 'createBackup',
  'backups.inspect': 'inspectBackup',
  'backups.restore': 'restoreBackup',
  'exports.create': 'createExport',
};

/// 从 `lib/src/diary_api.dart` 里抽出真实存在的 Dart 方法名。
Set<String> _declaredDartMethods() {
  final source = File('lib/src/diary_api.dart').readAsStringSync();
  final pattern = RegExp(
    r'^\s*(?:Future|Stream)<[^;{]*?>\s+([A-Za-z]+)\s*\(',
    multiLine: true,
  );
  return pattern.allMatches(source).map((m) => m.group(1)!).toSet();
}

Map<String, dynamic> _loadScenarios() {
  final file = File('../../tests/fixtures/scenarios/m0-scenarios.json');
  expect(file.existsSync(), isTrue, reason: '共用场景文件不存在：${file.path}');
  return jsonDecode(file.readAsStringSync()) as Map<String, dynamic>;
}

List<Map<String, dynamic>> _scenarios(Map<String, dynamic> root) =>
    (root['scenarios'] as List<dynamic>).cast<Map<String, dynamic>>();

void main() {
  group('共用场景与契约', () {
    late Map<String, dynamic> root;
    late Set<String> declared;

    setUpAll(() {
      root = _loadScenarios();
      declared = _declaredDartMethods();
    });

    test('场景文件的 contractVersion 与包内常量一致', () {
      expect(root['contractVersion'], kContractVersion);
    });

    test('每条场景都有必备字段，且 id 唯一', () {
      final scenarios = _scenarios(root);
      expect(scenarios, isNotEmpty);

      final ids = <String>{};
      for (final scenario in scenarios) {
        for (final field in ['id', 'name', 'goal', 'setup', 'steps', 'expected']) {
          expect(scenario.containsKey(field), isTrue,
              reason: '${scenario['id']} 缺少字段 $field');
        }
        expect(scenario['steps'], isA<List<dynamic>>());
        expect(scenario['expected'], isA<Map<String, dynamic>>());
        expect(scenario['setup'], isA<Map<String, dynamic>>());
        expect(ids.add(scenario['id'] as String), isTrue,
            reason: 'id 重复：${scenario['id']}');
      }
    });

    test('covers 引用的验收案例编号格式正确', () {
      final codePattern = RegExp(r'^E\d{2}$');
      for (final scenario in _scenarios(root)) {
        final covers = (scenario['covers'] as List<dynamic>?) ?? const [];
        for (final code in covers) {
          expect(codePattern.hasMatch(code as String), isTrue,
              reason: '${scenario['id']} 的验收编号不合法：$code');
        }
      }
    });

    test('场景步骤只使用契约里列出的方法名', () {
      final referenced = _referencedContractMethods(root);
      expect(referenced, isNotEmpty);

      final unknown = referenced
          .difference(kContractToDartMethod.keys.toSet())
          .toList()
        ..sort();
      expect(unknown, isEmpty, reason: '场景引用了契约里没有的方法：$unknown');
    });

    test('契约方法在 DiaryApi 上都有对应实现', () {
      expect(declared.length, greaterThanOrEqualTo(60),
          reason: 'DiaryApi 方法数异常，可能解析失败');

      final missing = kContractToDartMethod.entries
          .where((entry) => !declared.contains(entry.value))
          .map((entry) => '${entry.key} -> ${entry.value}')
          .toList()
        ..sort();
      expect(missing, isEmpty, reason: '契约方法没有对应实现：$missing');
    });

    test('DiaryApi 上没有契约之外的多余方法', () {
      final mapped = kContractToDartMethod.values.toSet();
      final extra = declared.difference(mapped).toList()..sort();
      expect(extra, isEmpty,
          reason: '这些方法不在契约里，属于接口漂移：$extra');
    });
  });
}

Set<String> _referencedContractMethods(Map<String, dynamic> root) {
  final stepPattern = RegExp(r'[a-z][a-zA-Z]*\.[a-zA-Z]+');
  final referenced = <String>{};
  for (final scenario in _scenarios(root)) {
    for (final step in scenario['steps'] as List<dynamic>) {
      for (final match in stepPattern.allMatches(step as String)) {
        referenced.add(match.group(0)!);
      }
    }
  }
  return referenced;
}

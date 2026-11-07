import 'dart:io';

import 'package:diary_api/diary_api.dart';
import 'package:diary_bridge/diary_bridge.dart' show BridgeDiaryApi;
import 'package:test/test.dart';
import 'package:path/path.dart' as path;

/// **跨层**验证：用 `api.open()` 返回的**真实能力清单**去构造控制器，
/// 而不是在 Mock 里手写一份「包含 settings.*」的集合。
///
/// 之前就是这么漏的：Rust 有 `get_settings`，Dart 方法表也加了名字，但 Rust 的
/// `wired_capabilities` 没宣告 → 两者的交集里没有 `settings.update` → 界面按钮
/// 一直禁用。只测 Dart 侧，这个洞永远看不见。
void main() {
  final libraryPath = Platform.environment['DIARY_BRIDGE_LIB'];
  final available = libraryPath != null && File(libraryPath).existsSync();

  test('真实能力清单里必须有 settings.get / settings.update', () async {
    final dir = Directory.systemTemp.createTempSync('bridge-settings');
    addTearDown(() => dir.deleteSync(recursive: true));

    final api = BridgeDiaryApi(
      coreLibraryPath: libraryPath!,
      libraryPath: path.join(dir.path, 'library.sqlite'),
      timeZone: 'Asia/Shanghai',
      utcOffsetMinutes: 480,
    );
    addTearDown(api.close);

    final snapshot = await api.open();
    final capabilities = snapshot.coreInfo.capabilities;

    expect(
      capabilities,
      containsAll(<String>['settings.get', 'settings.update']),
      reason: 'Rust 侧没宣告，Dart 适配层的能力交集里就不会有它，界面按钮永远禁用',
    );

    // 真的能读能写。
    final before = await api.getSettings();
    final saved = await api.saveStyle(
      StyleProfile(
        id: 'sty_probe',
        label: '探针风格',
        revision: 0,
        updatedAt: DateTime.now().toUtc(),
        lengthPreference: '跟随原文',
        person: '第一人称',
        tone: '冷静记录',
        emotionalTemperature: '克制',
        rhetoricLevel: '少润色，尽量用原话',
        examples: const [],
        bannedExpressions: const [],
      ),
      expectedRevision: null,
    );
    final updated = await api.updateSettings(
      settings: before.copyWith(defaultStyleId: saved.id),
      expectedRevision: before.revision,
    );
    expect(updated.defaultStyleId, saved.id, reason: '默认风格要真的写进去');
  }, skip: available ? false : '需要 DIARY_BRIDGE_LIB 指向真实的 diary_bridge 动态库');
}

import 'package:diary_api/diary_api.dart';
import 'package:diary_mock/diary_mock.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:unwritten_diary/src/style_settings_controller.dart';

class StyleApi extends MockDiaryApi {
  final now = DateTime.utc(2026, 10, 17);
  late Settings currentSettings = Settings(
    diaryTimeZone: 'Asia/Shanghai',
    revision: 2,
    updatedAt: now,
  );
  final stored = <StyleProfile>[];

  @override
  Future<List<StyleProfile>> listStyles() async => List.unmodifiable(stored);

  @override
  Future<StyleProfile> saveStyle(
    StyleProfile profile, {
    int? expectedRevision,
  }) async {
    final saved = StyleProfile(
      id: profile.id,
      label: profile.label,
      revision: (expectedRevision ?? 0) + 1,
      updatedAt: now,
      lengthPreference: profile.lengthPreference,
      person: profile.person,
      tone: profile.tone,
      emotionalTemperature: profile.emotionalTemperature,
      rhetoricLevel: profile.rhetoricLevel,
      examples: profile.examples,
      bannedExpressions: profile.bannedExpressions,
    );
    stored
      ..removeWhere((item) => item.id == saved.id)
      ..add(saved);
    return saved;
  }

  @override
  Future<Settings> getSettings() async => currentSettings;

  @override
  Future<Settings> updateSettings({
    required Settings settings,
    required int expectedRevision,
  }) async {
    expect(expectedRevision, currentSettings.revision);
    currentSettings = settings.copyWith(
      revision: expectedRevision + 1,
      updatedAt: now,
    );
    return currentSettings;
  }
}

void main() {
  const capabilities = {
    'styles.list',
    'styles.save',
    'settings.get',
    'settings.update',
  };

  test('style is persisted then selected without rewriting history', () async {
    final api = StyleApi();
    final controller = StyleSettingsController(api, capabilities: capabilities);
    addTearDown(controller.dispose);

    await controller.load();
    final saved = await controller.save(
      label: '克制日记',
      lengthPreference: '适中',
      tone: '更克制',
      person: '第一人称',
      emotionalTemperature: '温和',
      rhetoricLevel: '直接',
      examples: const ['风很轻。'],
      bannedExpressions: const ['治愈一切'],
    );

    expect(saved?.revision, 1);
    expect(controller.styles.single.bannedExpressions, const ['治愈一切']);

    // **保存即生效**：还没有默认风格时，保存后自动设为默认。
    // 之前只保存不设默认，用户改完风格、下次整理还是用内置默认。
    expect(controller.selectedStyleId, saved!.id);
    expect(api.currentSettings.revision, 3);

    // 再点一次「设为默认」不该重复推进 revision（已经是它了）。
    expect(await controller.select(saved), isTrue);
    expect(api.currentSettings.revision, 4);
  });

  test('已经有默认风格时，保存新风格不会抢走默认', () async {
    final api = StyleApi();
    final controller = StyleSettingsController(api, capabilities: capabilities);
    addTearDown(controller.dispose);

    await controller.load();
    final first = await controller.save(
      label: '第一套',
      lengthPreference: '适中',
      tone: '更克制',
      person: '第一人称',
      emotionalTemperature: '温和',
      rhetoricLevel: '直接',
      examples: const [],
      bannedExpressions: const [],
    );
    expect(controller.selectedStyleId, first!.id);

    final second = await controller.save(
      label: '第二套',
      lengthPreference: '跟随原文',
      tone: '冷静记录',
      person: '第一人称',
      emotionalTemperature: '克制',
      rhetoricLevel: '少润色，尽量用原话',
      examples: const [],
      bannedExpressions: const [],
    );
    expect(second, isNotNull);
    expect(
      controller.selectedStyleId,
      first.id,
      reason: '用户已经选过默认，保存新风格不该悄悄改掉它',
    );
  });

  test('missing capability keeps save disabled', () async {
    final controller = StyleSettingsController(
      StyleApi(),
      capabilities: const {'styles.list'},
    );
    addTearDown(controller.dispose);

    await controller.load();
    expect(controller.canSave, isFalse);
    expect(controller.message, contains('尚未开放'));
  });
}

import 'package:diary_api/diary_api.dart';
import 'package:flutter/foundation.dart';

class StyleSettingsController extends ChangeNotifier {
  StyleSettingsController(this._api, {required Set<String> capabilities})
    : capabilities = Set.unmodifiable(capabilities);

  final DiaryApi _api;
  final Set<String> capabilities;

  List<StyleProfile> styles = const [];
  Settings? settings;
  bool loading = false;
  bool saving = false;
  String? message;

  bool get canList => capabilities.contains('styles.list');
  bool get canSave => capabilities.contains('styles.save');
  bool get canReadSettings => capabilities.contains('settings.get');
  bool get canSelect =>
      canReadSettings && capabilities.contains('settings.update');
  String? get selectedStyleId => settings?.defaultStyleId;

  Future<void> load() async {
    if (loading) return;
    loading = true;
    message = null;
    notifyListeners();
    try {
      final loadedStyles = canList
          ? await _api.listStyles()
          : const <StyleProfile>[];
      final loadedSettings = canReadSettings ? await _api.getSettings() : null;
      styles = List.unmodifiable(loadedStyles);
      settings = loadedSettings;
      if (!canList || !canSave) {
        message = '当前核心尚未开放完整风格设置能力，所选内容暂不保存到核心；能力就绪后会自动启用。';
      }
    } on DiaryException catch (error) {
      message = error.message;
    } on Object {
      message = '风格偏好暂时没有加载完成，请稍后重试。';
    } finally {
      loading = false;
      notifyListeners();
    }
  }

  Future<StyleProfile?> save({
    StyleProfile? existing,
    required String label,
    required String lengthPreference,
    required String tone,
    required String person,
    required String emotionalTemperature,
    required String rhetoricLevel,
    required List<String> examples,
    required List<String> bannedExpressions,
  }) async {
    if (!canSave || saving) return null;
    saving = true;
    message = '正在保存风格偏好…';
    notifyListeners();
    final profile = StyleProfile(
      id: existing?.id ?? 'style-${DateTime.now().microsecondsSinceEpoch}',
      label: label.trim(),
      revision: existing?.revision ?? 0,
      updatedAt: DateTime.now().toUtc(),
      lengthPreference: lengthPreference.trim(),
      tone: tone.trim(),
      person: person.trim(),
      emotionalTemperature: emotionalTemperature.trim(),
      rhetoricLevel: rhetoricLevel.trim(),
      examples: List.unmodifiable(examples),
      bannedExpressions: List.unmodifiable(bannedExpressions),
    );
    try {
      final saved = await _api.saveStyle(
        profile,
        expectedRevision: existing?.revision,
      );
      final next = [...styles];
      final index = next.indexWhere((item) => item.id == saved.id);
      if (index == -1) {
        next.add(saved);
      } else {
        next[index] = saved;
      }
      styles = List.unmodifiable(next);
      message = '风格偏好已保存；历史日记不会自动重写。';
      return saved;
    } on DiaryException catch (error) {
      message = error.message;
    } on Object {
      message = '风格偏好没有保存，原设置保持不变。';
    } finally {
      saving = false;
      notifyListeners();
    }
    return null;
  }

  Future<bool> select(StyleProfile profile) async {
    final current = settings;
    if (!canSelect || current == null || saving) return false;
    saving = true;
    message = '正在设为默认风格…';
    notifyListeners();
    try {
      settings = await _api.updateSettings(
        settings: current.copyWith(defaultStyleId: profile.id),
        expectedRevision: current.revision,
      );
      message = '“${profile.label}”已用于之后的新整理任务。';
      return true;
    } on DiaryException catch (error) {
      message = error.message;
    } on Object {
      message = '默认风格没有改变，请稍后重试。';
    } finally {
      saving = false;
      notifyListeners();
    }
    return false;
  }
}

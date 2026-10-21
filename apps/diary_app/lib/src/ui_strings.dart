import 'dart:convert';

import 'package:flutter/services.dart';

/// 共享语义文案表的只读读取层。
///
/// 唯一事实源是 `packages/ui_strings/ui_strings.json`。它由一个只含 asset 的
/// Flutter 包带进安装包，key 是 `packages/ui_strings/ui_strings.json`。
///
/// 这里**不**保留任何硬编码文案作为回退：表没加载或 key 不存在时，[t] 返回
/// 显眼的 `⟨...⟩` 占位符，让漏接/打错 key 直接出现在界面上，而不是悄悄变成空白。
class UiStrings {
  UiStrings();

  /// 全应用共用的实例；`main()` 在 `runApp` 前调用一次 [load]。
  static final UiStrings instance = UiStrings();

  /// 由 `packages/ui_strings/pubspec.yaml` 声明的 asset key。
  static const String assetPath = 'packages/ui_strings/ui_strings.json';

  Map<String, String> _values = const {};
  bool _loaded = false;
  Object? _loadError;

  bool get isLoaded => _loaded;

  /// 最近一次 [load] 失败的原因；成功后为 null。
  Object? get loadError => _loadError;

  /// 读取并校验共享文案表。失败时抛出，由调用方决定是否继续启动。
  Future<void> load({AssetBundle? bundle}) async {
    try {
      final raw = await (bundle ?? rootBundle).loadString(assetPath);
      final decoded = jsonDecode(raw);
      if (decoded is! Map<String, dynamic>) {
        throw const FormatException('共享文案表顶层必须是 JSON 对象');
      }
      final values = <String, String>{};
      for (final entry in decoded.entries) {
        final value = entry.value;
        if (value is! String || value.trim().isEmpty) {
          throw FormatException('共享文案 ${entry.key} 不是非空字符串');
        }
        values[entry.key] = value;
      }
      if (values.isEmpty) {
        throw const FormatException('共享文案表是空的');
      }
      _values = Map.unmodifiable(values);
      _loaded = true;
      _loadError = null;
    } on Object catch (error) {
      _loadError = error;
      rethrow;
    }
  }

  /// 读一条文案。
  ///
  /// 表未加载或 key 不存在时返回 `⟨...⟩` 占位符，绝不返回空串或抛异常。
  String t(String key) {
    if (!_loaded) return '⟨文案表未加载: $key⟩';
    final value = _values[key];
    if (value == null || value.isEmpty) return '⟨key 不存在: $key⟩';
    return value;
  }

  bool containsKey(String key) => _values.containsKey(key);

  Iterable<String> get keys => _values.keys;
}

/// 统一的小写单例入口，便于源码按 `uiStrings.t('...')` 引用文案；
/// `tools/check_ui_parity.py` 也按这个写法扫描源码引用。
final UiStrings uiStrings = UiStrings.instance;

import 'dart:convert';
import 'dart:io';

import 'package:path/path.dart' as path;

/// 随包本地模型的**路径登记**。
///
/// 安装包里带了两套模型（和鸿蒙版同一份）：
///
/// ```
/// <exe>/models/asr/sense-voice-int8/      转写（core 按约定自己找）
/// <exe>/models/semantic/bge-small-zh-v1.5/ 语义检索（要写 core-config.json）
/// <exe>/onnxruntime.dll                    两条路共用一份 ORT 1.28.2
/// ```
///
/// 转写那一路 core 认「可执行文件旁边的 models/asr/sense-voice-int8」；
/// 语义那一路只认环境变量或 `core-config.json`，所以这里把路径补进去。
///
/// **只补缺失的字段**：用户自己配过路径就不动，坏掉的配置文件也不重写
/// （宁可让核心如实报错，也不要悄悄改掉用户的选择）。
class BundledModels {
  const BundledModels._();

  static const _semanticRelative = 'models/semantic/bge-small-zh-v1.5';
  static const _ortRelative = 'onnxruntime.dll';
  static const _configName = 'core-config.json';

  /// 把随包路径补进 `core-config.json`。返回是否真的写了文件。
  static Future<bool> ensureConfigured(
    Directory libraryDirectory, {
    Directory? executableDirectory,
  }) async {
    final exeDir = executableDirectory ??
        (Platform.isWindows ? File(Platform.resolvedExecutable).parent : null);
    if (exeDir == null) return false;
    final semanticDir = Directory(path.join(exeDir.path, _semanticRelative));
    final ortDylib = File(path.join(exeDir.path, _ortRelative));
    // 没有随包模型就什么都不做：不写半套配置，让核心照旧报「没配」。
    if (!File(path.join(semanticDir.path, 'model_quantized.onnx')).existsSync() ||
        !ortDylib.existsSync()) {
      return false;
    }

    final configFile = File(path.join(libraryDirectory.path, _configName));
    // 注意：不能用 `const {}`——那是不可变 Map，putIfAbsent 会抛
    // UnsupportedError，整个启动就挂了。
    Map<String, dynamic> config = <String, dynamic>{};
    if (configFile.existsSync()) {
      try {
        final decoded = jsonDecode(await configFile.readAsString());
        if (decoded is! Map) return false; // 坏文件不覆盖
        config = Map<String, dynamic>.from(decoded);
      } on Object {
        return false;
      }
    }

    final hasModelDir = (config['modelDir'] ?? config['model_dir']) != null;
    final hasOrtDylib = (config['ortDylib'] ?? config['ort_dylib']) != null;
    if (hasModelDir && hasOrtDylib) return false;

    config.putIfAbsent('modelDir', () => semanticDir.path);
    config.putIfAbsent('ortDylib', () => ortDylib.path);
    await configFile.writeAsString(
      const JsonEncoder.withIndent('  ').convert(config),
      flush: true,
    );
    return true;
  }
}

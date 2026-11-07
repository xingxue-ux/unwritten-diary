import 'dart:convert';
import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:path/path.dart' as path;
import 'package:unwritten_diary/src/bundled_models.dart';

/// 造一个「安装目录」：只放判定需要的两个文件。
Directory fakeInstall(Directory root) {
  final exe = Directory(path.join(root.path, 'app'))..createSync(recursive: true);
  File(path.join(exe.path, 'onnxruntime.dll')).writeAsBytesSync([0]);
  final semantic = Directory(
    path.join(exe.path, 'models', 'semantic', 'bge-small-zh-v1.5'),
  )..createSync(recursive: true);
  File(path.join(semantic.path, 'model_quantized.onnx')).writeAsBytesSync([0]);
  return exe;
}

void main() {
  late Directory root;
  late Directory library;

  setUp(() {
    root = Directory.systemTemp.createTempSync('bundled-models');
    library = Directory(path.join(root.path, 'data'))..createSync();
  });
  tearDown(() => root.deleteSync(recursive: true));

  test('随包模型存在时把 modelDir / ortDylib 写进 core-config.json', () async {
    final exe = fakeInstall(root);
    final wrote = await BundledModels.ensureConfigured(
      library,
      executableDirectory: exe,
    );
    expect(wrote, isTrue);

    final config = jsonDecode(
      File(path.join(library.path, 'core-config.json')).readAsStringSync(),
    ) as Map<String, dynamic>;
    // 用 path.join 拼期望值：写死 `models/semantic/...` 在 Windows 上会跟
    // `path.join` 产生的反斜杠对不上，测试红得莫名其妙。
    expect(
      config['modelDir'],
      path.join(exe.path, 'models', 'semantic', 'bge-small-zh-v1.5'),
    );
    expect(config['ortDylib'], path.join(exe.path, 'onnxruntime.dll'));
  });

  test('已有配置只补缺失字段，用户自己配的路径不动', () async {
    final exe = fakeInstall(root);
    final file = File(path.join(library.path, 'core-config.json'))
      ..writeAsStringSync(jsonEncode({'modelDir': '/my/own/models', 'asrModelDir': '/my/asr'}));

    await BundledModels.ensureConfigured(library, executableDirectory: exe);

    final config = jsonDecode(file.readAsStringSync()) as Map<String, dynamic>;
    expect(config['modelDir'], '/my/own/models', reason: '用户配过就不能覆盖');
    expect(config['asrModelDir'], '/my/asr', reason: '别的字段要原样保留');
    expect(config['ortDylib'], path.join(exe.path, 'onnxruntime.dll'));
  });

  test('配置文件坏掉时不覆盖', () async {
    final exe = fakeInstall(root);
    final file = File(path.join(library.path, 'core-config.json'))
      ..writeAsStringSync('{ not json');

    final wrote = await BundledModels.ensureConfigured(
      library,
      executableDirectory: exe,
    );
    expect(wrote, isFalse);
    expect(file.readAsStringSync(), '{ not json');
  });

  test('没有随包模型时什么都不写', () async {
    final empty = Directory(path.join(root.path, 'app'))..createSync();
    final wrote = await BundledModels.ensureConfigured(
      library,
      executableDirectory: empty,
    );
    expect(wrote, isFalse);
    expect(File(path.join(library.path, 'core-config.json')).existsSync(), isFalse);
  });
}

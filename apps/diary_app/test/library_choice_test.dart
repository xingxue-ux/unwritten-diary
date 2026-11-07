import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:path/path.dart' as p;
import 'package:path_provider_platform_interface/path_provider_platform_interface.dart';
import 'package:unwritten_diary/src/app_services.dart';

class _FakePathProvider extends PathProviderPlatform {
  _FakePathProvider(this.root);
  final String root;

  @override
  Future<String?> getApplicationSupportPath() async => root;
}

/// 恢复备份会生成一个**新库**，但应用启动默认只打开支持目录下那一个。用户恢复完、
/// 重开应用仍然回到旧库——恢复等于白做。这里钉住「记住选择 + 启动时用它」。
void main() {
  TestWidgetsFlutterBinding.ensureInitialized();

  late Directory root;

  setUp(() {
    root = Directory.systemTemp.createTempSync('library-choice');
    PathProviderPlatform.instance = _FakePathProvider(root.path);
    Directory(p.join(root.path, 'unwritten_diary')).createSync(recursive: true);
  });

  tearDown(() => root.deleteSync(recursive: true));

  test('没有选择时返回空，走默认库', () async {
    expect(await readChosenLibrary(), isNull);
  });

  test('记住的库存在就返回它', () async {
    final restored = File(p.join(root.path, 'restored', 'library.sqlite'));
    restored.parent.createSync(recursive: true);
    restored.writeAsStringSync('x');

    await writeChosenLibrary(restored.path);
    expect(await readChosenLibrary(), restored.path);
  });

  test('记住的库被删掉之后回到默认库，而不是打不开', () async {
    final restored = File(p.join(root.path, 'restored', 'library.sqlite'));
    restored.parent.createSync(recursive: true);
    restored.writeAsStringSync('x');
    await writeChosenLibrary(restored.path);

    restored.deleteSync();
    expect(
      await readChosenLibrary(),
      isNull,
      reason: '库没了就该回到默认路径，不能让应用打不开',
    );
  });

  test('传 null 表示回到默认库', () async {
    final restored = File(p.join(root.path, 'restored', 'library.sqlite'));
    restored.parent.createSync(recursive: true);
    restored.writeAsStringSync('x');
    await writeChosenLibrary(restored.path);

    await writeChosenLibrary(null);
    expect(await readChosenLibrary(), isNull);
  });

  test('选择文件坏掉时当作没选过，不抛异常', () async {
    File(p.join(root.path, 'library-choice.json')).writeAsStringSync('{不是 json');
    expect(await readChosenLibrary(), isNull);
  });

  test('默认资料库目录在应用支持目录下', () async {
    final dir = await defaultLibraryDirectory();
    expect(p.basename(dir.path), 'unwritten_diary');
  });
}

import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:path/path.dart' as p;
import 'package:unwritten_diary/src/recording/playable_audio.dart';

/// 文件库里的 blob 没有扩展名，Windows 的 Media Foundation 打不开（`play()` 会
/// 一直等到 30 秒超时）。这一层负责给它一个带后缀的副本。
void main() {
  test('没有扩展名的原件会被复制成带 .wav 的副本', () async {
    final root = Directory.systemTemp.createTempSync('playable');
    addTearDown(() => root.deleteSync(recursive: true));
    final cache = Directory(p.join(root.path, 'cache'));
    final source = File(p.join(root.path, '13507cd4abcdef'))
      ..writeAsBytesSync(List.filled(64, 7));

    final playable = PlayableAudio(cacheDirectory: cache);
    final resolved = await playable.resolve(source.path);

    expect(p.extension(resolved), '.wav');
    expect(File(resolved).readAsBytesSync(), source.readAsBytesSync());
    expect(File(resolved).path, isNot(source.path));
  });

  test('同一份原件只复制一次（按哈希命名）', () async {
    final root = Directory.systemTemp.createTempSync('playable2');
    addTearDown(() => root.deleteSync(recursive: true));
    final cache = Directory(p.join(root.path, 'cache'));
    final source = File(p.join(root.path, 'deadbeef'))
      ..writeAsBytesSync(List.filled(8, 1));

    final playable = PlayableAudio(cacheDirectory: cache);
    final first = await playable.resolve(source.path);
    final second = await playable.resolve(source.path);
    expect(first, second);
    expect(cache.listSync().whereType<File>().length, 1);
  });

  test('已经有扩展名的路径原样返回，不复制', () async {
    final root = Directory.systemTemp.createTempSync('playable3');
    addTearDown(() => root.deleteSync(recursive: true));
    final source = File(p.join(root.path, 'a.wav'))..writeAsBytesSync([1, 2, 3]);

    final playable = PlayableAudio(
      cacheDirectory: Directory(p.join(root.path, 'cache')),
    );
    expect(await playable.resolve(source.path), source.path);
    expect(Directory(p.join(root.path, 'cache')).existsSync(), isFalse);
  });
}

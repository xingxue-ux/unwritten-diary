import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:path/path.dart' as p;
import 'package:unwritten_diary/src/materialized_file.dart';

/// 文件库里的 blob 没有扩展名：播放器打不开（`play()` 会等到 30 秒超时），
/// 交给系统程序也只会弹「你要如何打开此文件」。这一层负责按内容嗅探出后缀、
/// 复制一份带后缀的副本。
void main() {
  test('没有扩展名的原件会被复制成带后缀的副本', () async {
    final root = Directory.systemTemp.createTempSync('playable');
    addTearDown(() => root.deleteSync(recursive: true));
    final cache = Directory(p.join(root.path, 'cache'));
    final source = File(p.join(root.path, '13507cd4abcdef'))
      ..writeAsBytesSync(List.filled(64, 7));

    final playable = MaterializedFile(cacheDirectory: cache);
    // 录音链知道自己给的是 WAV，直接指定后缀（不靠嗅探）。
    final resolved = await playable.resolve(source.path, extension: 'wav');

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

    final playable = MaterializedFile(cacheDirectory: cache);
    final first = await playable.resolve(source.path);
    final second = await playable.resolve(source.path);
    expect(first, second);
    expect(cache.listSync().whereType<File>().length, 1);
  });

  test('按文件头嗅探类型：PDF / WAV / PNG', () async {
    final root = Directory.systemTemp.createTempSync('sniff');
    addTearDown(() => root.deleteSync(recursive: true));
    final cache = Directory(p.join(root.path, 'cache'));
    final materialized = MaterializedFile(cacheDirectory: cache);

    final pdf = File(p.join(root.path, 'blob_pdf'))
      ..writeAsBytesSync('%PDF-1.7 rest'.codeUnits);
    final wav = File(p.join(root.path, 'blob_wav'))
      ..writeAsBytesSync([
        ...'RIFF'.codeUnits,
        0, 0, 0, 0,
        ...'WAVEfmt '.codeUnits,
      ]);
    final png = File(p.join(root.path, 'blob_png'))
      ..writeAsBytesSync([0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]);

    expect(p.extension(await materialized.resolve(pdf.path)), '.pdf');
    expect(p.extension(await materialized.resolve(wav.path)), '.wav');
    expect(p.extension(await materialized.resolve(png.path)), '.png');
  });

  test('认不出来的内容用 .bin，不猜一个会误导系统的后缀', () async {
    final root = Directory.systemTemp.createTempSync('sniff2');
    addTearDown(() => root.deleteSync(recursive: true));
    final materialized = MaterializedFile(
      cacheDirectory: Directory(p.join(root.path, 'cache')),
    );
    final unknown = File(p.join(root.path, 'blob_unknown'))
      ..writeAsBytesSync([1, 2, 3, 4, 5, 6, 7, 8]);
    expect(p.extension(await materialized.resolve(unknown.path)), '.bin');
  });

  test('已经有扩展名的路径原样返回，不复制', () async {
    final root = Directory.systemTemp.createTempSync('playable3');
    addTearDown(() => root.deleteSync(recursive: true));
    final source = File(p.join(root.path, 'a.wav'))..writeAsBytesSync([1, 2, 3]);

    final playable = MaterializedFile(
      cacheDirectory: Directory(p.join(root.path, 'cache')),
    );
    expect(await playable.resolve(source.path), source.path);
    expect(Directory(p.join(root.path, 'cache')).existsSync(), isFalse);
  });
}

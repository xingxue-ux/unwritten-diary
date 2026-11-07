import 'dart:io';

import 'package:path/path.dart' as p;

/// 把「文件库里的原件路径」变成外部程序真的能打开的路径。
///
/// # 为什么需要这一层
///
/// 文件库里的 blob 用**内容哈希**当文件名（`blobs/13/13507cd4…`），没有扩展名。
/// 两种地方会因此打不开：
///
/// - **播放**：Windows 的 Media Foundation（`audioplayers` 在 Windows 上就是它）
///   虽然带了「内容与扩展名不必匹配」的解析标志，实测仍然打不开这种路径——
///   `play()` 会一直等到 30 秒超时（`TimeoutException: Future not completed`）；
/// - **交给系统程序打开**：没有扩展名，Windows 不知道用什么程序，只会弹「你要
///   如何打开此文件」。
///
/// 所以这里按内容嗅探出真实类型，复制一份带正确后缀的副本到系统临时目录再交出去。
///
/// # 为什么不改文件库的命名
///
/// blob 名就是哈希，改名字会让「同一份内容只存一份」失效，也会让已经存下来的
/// 引用全部对不上。副本按哈希命名并缓存，同一份内容只复制一次。
class MaterializedFile {
  MaterializedFile({Directory? cacheDirectory})
    : _cache = cacheDirectory ??
          Directory(p.join(Directory.systemTemp.path, 'unwritten-diary-open'));

  final Directory _cache;

  /// 返回一个外部程序能打开的路径。
  ///
  /// [extension] 是调用方已经知道的类型（例如录音链一定是 `wav`）；不传就嗅探。
  /// [handle] 本身已经有扩展名时原样返回，不复制。
  Future<String> resolve(String handle, {String? extension}) async {
    if (p.extension(handle).isNotEmpty) return handle;
    final suffix = extension ?? await _sniff(handle);
    final name = p.basename(handle);
    final target = File(p.join(_cache.path, '$name.$suffix'));
    if (await target.exists()) return target.path;
    await _cache.create(recursive: true);
    // 先写临时文件再改名：中途失败不会留下一个「看起来完整」的坏副本。
    final partial = File('${target.path}.part');
    await File(handle).copy(partial.path);
    await partial.rename(target.path);
    return target.path;
  }

  /// 按文件头认类型。认不出来就用 `.bin`——**不猜**一个可能误导系统的后缀。
  Future<String> _sniff(String handle) async {
    RandomAccessFile? file;
    try {
      file = await File(handle).open();
      final head = await file.read(12);
      if (head.length >= 5 &&
          head[0] == 0x25 &&
          head[1] == 0x50 &&
          head[2] == 0x44 &&
          head[3] == 0x46) {
        return 'pdf'; // %PDF-
      }
      if (head.length >= 12 &&
          head[0] == 0x52 && // R
          head[1] == 0x49 && // I
          head[2] == 0x46 && // F
          head[3] == 0x46 && // F
          head[8] == 0x57 && // W
          head[9] == 0x41 && // A
          head[10] == 0x56 && // V
          head[11] == 0x45) {
        return 'wav';
      }
      if (head.length >= 8 &&
          head[0] == 0x89 &&
          head[1] == 0x50 &&
          head[2] == 0x4E &&
          head[3] == 0x47) {
        return 'png';
      }
      if (head.length >= 3 && head[0] == 0xFF && head[1] == 0xD8 && head[2] == 0xFF) {
        return 'jpg';
      }
      if (head.length >= 4 &&
          head[0] == 0x47 &&
          head[1] == 0x49 &&
          head[2] == 0x46 &&
          head[3] == 0x38) {
        return 'gif';
      }
      if (head.length >= 4 &&
          head[0] == 0x25 &&
          head[1] == 0x21 &&
          head[2] == 0x50 &&
          head[3] == 0x53) {
        return 'ps'; // %!PS
      }
      return 'bin';
    } on Object {
      return 'bin';
    } finally {
      await file?.close();
    }
  }

  /// 清掉缓存（退出时调用；失败不影响任何事）。
  Future<void> clear() async {
    try {
      if (await _cache.exists()) await _cache.delete(recursive: true);
    } on Object {
      // 临时目录会被系统清理，删不掉不是错误。
    }
  }
}

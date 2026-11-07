import 'dart:io';

import 'package:path/path.dart' as p;

/// 把「文件库里的原件路径」变成播放器真的能打开的路径。
///
/// # 为什么要绕这一下
///
/// 文件库里的 blob 用**内容哈希**当文件名（`blobs/13/13507cd4…`），没有扩展名。
/// Windows 的 Media Foundation（`audioplayers` 在 Windows 上就是它）虽然带了
/// 「内容与扩展名不必匹配」的解析标志，实测仍然打不开这种路径：`play()` 会一直
/// 等到 30 秒超时（`TimeoutException: Future not completed`），界面上一句话都
/// 播不出来。给它一个 `.wav` 后缀的副本就正常了。
///
/// # 为什么不直接改文件库
///
/// blob 名就是哈希，改名字会让「同一份内容只存一份」这条失效，也会让所有已经
/// 存下来的引用对不上。所以只在**播放这一层**做一次副本，缓存目录放在系统临时
/// 目录下，按哈希命名——同一段音频只复制一次。
class PlayableAudio {
  PlayableAudio({Directory? cacheDirectory})
    : _cache = cacheDirectory ??
          Directory(p.join(Directory.systemTemp.path, 'unwritten-diary-play'));

  final Directory _cache;

  /// 返回一个播放器能打开的路径。[handle] 已经有扩展名时原样返回。
  Future<String> resolve(String handle, {String extension = 'wav'}) async {
    if (p.extension(handle).isNotEmpty) return handle;
    final name = p.basename(handle);
    final target = File(p.join(_cache.path, '$name.$extension'));
    if (await target.exists()) return target.path;
    await _cache.create(recursive: true);
    // 先写临时文件再改名：中途失败不会留下一个「看起来完整」的坏副本。
    final partial = File('${target.path}.part');
    await File(handle).copy(partial.path);
    await partial.rename(target.path);
    return target.path;
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

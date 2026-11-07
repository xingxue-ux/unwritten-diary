import 'dart:io';

/// 把文件路径变成 `file://` URL（可选带页码片段）。
///
/// # 为什么要自己拼
///
/// 文件库给的路径是 Windows 的**长路径**形式：`\\?\E:\…\blobs\58\581049…`。
/// 直接把反斜杠换成斜杠再加 `file:///` 会得到 `file:////?/E:/…`——URI 解析器
/// 会把 `//` 当成 authority，真实路径落到 query 里，浏览器打开的是一个不存在的
/// 位置。所以要先去掉 `\\?\` 前缀，再交给 `Uri.file` 做转义（空格、中文都要编码）。
///
/// [windows] 只是为了让测试在两个平台上都能断言同一套规则；产品路径不传。
String fileUrlFor(String path, {int? page, bool? windows}) {
  final isWindows = windows ?? Platform.isWindows;
  var normalized = path;
  if (isWindows) {
    const uncPrefix = r'\\?\UNC\';
    const longPrefix = r'\\?\';
    if (normalized.startsWith(uncPrefix)) {
      // `\\?\UNC\server\share` 是「长路径形式的 UNC」，还原成 `\\server\share`。
      normalized = '\\\\${normalized.substring(uncPrefix.length)}';
    } else if (normalized.startsWith(longPrefix)) {
      normalized = normalized.substring(longPrefix.length);
    }
  }
  final uri = Uri.file(normalized, windows: isWindows);
  return page == null ? uri.toString() : '${uri.toString()}#page=$page';
}

import 'dart:io';

/// 比较路径时用的规范形。
///
/// Windows 上核心返回的是 `fs::canonicalize` 之后的路径：带 `\\?\` 前缀、分隔符
/// 是反斜杠、大小写按磁盘上的真实写法（`E:\Data\Temp`）。而测试里 `p.join` /
/// `'${dir.path}/x'` 拼的是 `%TEMP%` 那套写法（`E:\data\Temp`）。两者指的是同一个
/// 位置，字符串却不相等——断言前统一规范化，而不是让产品把真实路径改回去。
String normalizePath(String path) {
  var value = path.replaceAll('\\', '/');
  // `//?/E:/…` 是长路径前缀，去掉之后才是普通路径。
  if (value.startsWith('//?/')) value = value.substring(4);
  if (value.startsWith('//./')) value = value.substring(4);
  // Windows 路径大小写不敏感；Linux 上必须保留大小写。
  return Platform.isWindows ? value.toLowerCase() : value;
}

/// 两个路径是否指向同一个位置。
bool samePath(String left, String right) =>
    normalizePath(left) == normalizePath(right);

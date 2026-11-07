import 'package:flutter_test/flutter_test.dart';
import 'package:unwritten_diary/src/file_url.dart';

/// 文件库给的是 Windows 长路径（`\\?\E:\…`），拼 URL 时最容易在这里出错：
/// 直接换斜杠会得到 `file:////?/E:/…`，URI 解析器把 `//` 当 authority，
/// 真实路径落到 query 里，浏览器打开的是一个不存在的位置。
void main() {
  test('去掉长路径前缀，得到正常的 file URL', () {
    expect(
      fileUrlFor(r'\\?\E:\data\library\blobs\58\581049', page: 2, windows: true),
      'file:///E:/data/library/blobs/58/581049#page=2',
    );
  });

  test('没有前缀的 Windows 路径同样处理', () {
    expect(
      fileUrlFor(r'E:\a\b.pdf', page: 3, windows: true),
      'file:///E:/a/b.pdf#page=3',
    );
  });

  test('空格与中文要转义，页码放在 fragment 里', () {
    final url = fileUrlFor(r'\\?\C:\我的 文件\a b.pdf', page: 7, windows: true);
    expect(url, startsWith('file:///C:/'));
    expect(url, isNot(contains(' ')));
    expect(url, endsWith('#page=7'));
    // 解析回来必须还是同一个路径，而不是落到 query 里。
    final uri = Uri.parse(url);
    expect(uri.fragment, 'page=7');
    expect(uri.query, isEmpty);
    expect(Uri.decodeFull(uri.path), '/C:/我的 文件/a b.pdf');
  });

  test('UNC 长路径还原成 \\\\server\\share', () {
    expect(
      fileUrlFor(r'\\?\UNC\server\share\x.pdf', windows: true),
      'file://server/share/x.pdf',
    );
  });

  test('不带页码时不加 fragment', () {
    expect(
      fileUrlFor(r'\\?\E:\a\b.pdf', windows: true),
      'file:///E:/a/b.pdf',
    );
  });

  test('非 Windows 路径按 POSIX 规则拼', () {
    expect(
      fileUrlFor('/home/u/x.pdf', page: 1, windows: false),
      'file:///home/u/x.pdf#page=1',
    );
  });
}

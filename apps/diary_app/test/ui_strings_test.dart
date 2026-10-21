import 'dart:convert';
import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:unwritten_diary/src/ui_strings.dart';

/// 共享文案表在仓库里的唯一事实源（相对 `apps/diary_app` 或仓库根都能找到）。
File _sourceTableFile() {
  var directory = Directory.current;
  for (var depth = 0; depth < 6; depth++) {
    final candidate = File(
      '${directory.path}/packages/ui_strings/ui_strings.json',
    );
    if (candidate.existsSync()) return candidate;
    final parent = directory.parent;
    if (parent.path == directory.path) break;
    directory = parent;
  }
  throw StateError(
    '找不到 packages/ui_strings/ui_strings.json（从 ${Directory.current.path} 向上找了 6 层）',
  );
}

Directory _libDirectory() {
  var directory = Directory.current;
  for (var depth = 0; depth < 6; depth++) {
    final direct = Directory('${directory.path}/lib');
    if (direct.existsSync()) return direct;
    final nested = Directory('${directory.path}/apps/diary_app/lib');
    if (nested.existsSync()) return nested;
    final parent = directory.parent;
    if (parent.path == directory.path) break;
    directory = parent;
  }
  throw StateError('找不到 apps/diary_app/lib（从 ${Directory.current.path} 开始）');
}

Map<String, String> _readSourceTable() {
  final decoded = jsonDecode(_sourceTableFile().readAsStringSync());
  if (decoded is! Map<String, dynamic>) {
    throw StateError('ui_strings.json 顶层不是对象');
  }
  return decoded.cast<String, String>();
}

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();

  group('UiStrings 读取层', () {
    late Map<String, String> table;

    setUpAll(() async {
      table = _readSourceTable();
    });

    test('契约的 30 个 key 全部可读、非空，且与 ui_strings.json 逐字一致', () async {
      final strings = UiStrings();
      await strings.load();

      expect(strings.isLoaded, isTrue);
      expect(
        table,
        hasLength(30),
        reason: '契约 §4.8 固定 30 个 key；新增 key 要同步改契约与两端',
      );
      expect(strings.keys.toSet(), table.keys.toSet());

      for (final entry in table.entries) {
        final loaded = strings.t(entry.key);
        expect(loaded.trim(), isNotEmpty, reason: '${entry.key} 不应是空串');
        expect(loaded, entry.value, reason: '${entry.key} 与共享表不一致');
      }
    });

    test('不存在的 key 返回显眼占位，而不是空串或异常', () async {
      final strings = UiStrings();
      await strings.load();

      expect(strings.t('nav.not-a-real-key'), '⟨key 不存在: nav.not-a-real-key⟩');
      expect(() => strings.t('state.not-a-real-key'), returnsNormally);
      expect(strings.t('state.not-a-real-key').trim(), isNotEmpty);
    });

    test('未加载时也给出可见占位（不退回任何硬编码文案）', () {
      final unloaded = UiStrings();

      final value = unloaded.t('nav.record');
      expect(value, contains('文案表未加载'));
      expect(value, contains('nav.record'));
      expect(value.trim(), isNotEmpty);
    });

    test('asset key 指向共享包，应用没有另抄一份', () {
      expect(UiStrings.assetPath, 'packages/ui_strings/ui_strings.json');

      final packageRoot = _sourceTableFile().parent;
      expect(File('${packageRoot.path}/pubspec.yaml').existsSync(), isTrue);
      expect(
        File('${packageRoot.path}/pubspec.yaml').readAsStringSync(),
        contains('- ui_strings.json'),
      );

      final appPubspec = File('${Directory.current.path}/pubspec.yaml');
      expect(appPubspec.existsSync(), isTrue);
      final appManifest = appPubspec.readAsStringSync();
      expect(appManifest, contains('ui_strings:'));
      expect(appManifest, contains('path: ../../packages/ui_strings'));
    });
  });

  group('防退化：源码里不许再硬编码文案表里的语义文案', () {
    late Map<String, String> table;

    setUpAll(() {
      table = _readSourceTable();
    });

    test('lib 下 Dart 字符串字面量不得等于表里的值（nav.* 除外）', () {
      // nav.* 故意留在字面量里：检查器按 `_navItem(..., '记录')` 解析入口；
      // 等两端都接表后再一起改。其余 25 个语义 key 的值一律不许再抄进源码。
      final banned = <String, String>{
        for (final entry in table.entries)
          if (!entry.key.startsWith('nav.')) entry.value: entry.key,
      };

      final violations = <String>[];
      final files =
          _libDirectory()
              .listSync(recursive: true)
              .whereType<File>()
              .where((file) => file.path.endsWith('.dart'))
              .toList()
            ..sort((a, b) => a.path.compareTo(b.path));

      for (final file in files) {
        for (final literal in _dartStringLiterals(file.readAsStringSync())) {
          final key = banned[literal.value.trim()];
          if (key != null) {
            violations.add(
              '${file.path}:${literal.line} 把 $key 的文案 '
              '“${literal.value.trim()}”硬编码进了源码；请改用 '
              "uiStrings.t('$key')",
            );
          }
        }
      }

      expect(
        violations,
        isEmpty,
        reason:
            '以下字面量已经收进 packages/ui_strings/ui_strings.json，'
            '不应再散落在 Flutter 源码里：\n${violations.join('\n')}',
      );
    });

    test('扫描器本身能抓到植入的硬编码，且不把注释当源码', () {
      const planted = """
const plain = '草稿';
const labelled = '状态：草稿';
// 注释里的 '已提交' 不算
/* 块注释里的 '已移入回收站' 也不算 */
const nested = '\${flag ? '草稿' : '已提交'}';
""";
      final values = _dartStringLiterals(
        planted,
      ).map((literal) => literal.value).toList();

      expect(values, contains('草稿'));
      expect(values, contains('状态：草稿'));
      expect(values, contains('已提交'));
      expect(values, isNot(contains('已移入回收站')));
      expect(values.where((value) => value == '草稿'), hasLength(2));
    });
  });
}

/// 从 Dart 源码里取出字符串字面量（跳过注释，处理插值里的嵌套字符串）。
///
/// 这不是完整的 Dart 词法器；测试只需要足够稳地覆盖状态词/句子的字面量。
/// 不解析插值表达式本身，但会把插值里的嵌套字面量也收进来。
List<_Literal> _dartStringLiterals(String source) {
  final scanner = _DartStringScanner(source);
  scanner.scan();
  return scanner.literals;
}

class _Literal {
  const _Literal(this.value, this.line);

  final String value;
  final int line;
}

class _DartStringScanner {
  _DartStringScanner(this.source);

  final String source;
  final List<_Literal> literals = [];
  int index = 0;
  int line = 1;

  void scan() => _scanCode(stopAtClosingBrace: false);

  void _scanCode({required bool stopAtClosingBrace}) {
    var braces = 0;
    while (index < source.length) {
      final char = source[index];
      if (char == '\n') {
        line++;
        index++;
        continue;
      }
      if (char == '/' && index + 1 < source.length) {
        final next = source[index + 1];
        if (next == '/') {
          final end = source.indexOf('\n', index);
          if (end < 0) {
            index = source.length;
            return;
          }
          index = end;
          continue;
        }
        if (next == '*') {
          _skipBlockComment();
          continue;
        }
      }
      if (char == "'" || char == '"') {
        _scanString();
        continue;
      }
      if (stopAtClosingBrace) {
        if (char == '{') {
          braces++;
        } else if (char == '}') {
          if (braces == 0) {
            index++;
            return;
          }
          braces--;
        }
      }
      index++;
    }
  }

  void _skipBlockComment() {
    var depth = 1;
    index += 2;
    while (index < source.length && depth > 0) {
      if (source.startsWith('/*', index)) {
        depth++;
        index += 2;
      } else if (source.startsWith('*/', index)) {
        depth--;
        index += 2;
      } else {
        if (source[index] == '\n') line++;
        index++;
      }
    }
  }

  void _scanString() {
    final quoteChar = source[index];
    final triple = source.startsWith('$quoteChar$quoteChar$quoteChar', index);
    final quote = triple ? '$quoteChar$quoteChar$quoteChar' : quoteChar;
    final raw =
        index > 0 &&
        source[index - 1] == 'r' &&
        (index < 2 || !_isIdentifier(source.codeUnitAt(index - 2)));

    final startLine = line;
    final buffer = StringBuffer();
    index += quote.length;

    while (index < source.length) {
      if (source.startsWith(quote, index)) {
        index += quote.length;
        literals.add(_Literal(buffer.toString(), startLine));
        return;
      }

      final char = source[index];
      if (char == '\n') {
        if (!triple) {
          // 单行字符串没闭合：多半是注释里的撇号，交给调用方当普通字符跳过。
          return;
        }
        line++;
      }

      if (!raw) {
        if (char == '\\' && index + 1 < source.length) {
          final escaped = source[index + 1];
          buffer.write(_unescape(escaped));
          if (escaped == '\n') line++;
          index += 2;
          continue;
        }
        if (char == r'$') {
          if (index + 1 < source.length && source[index + 1] == '{') {
            buffer.write(r'${');
            index += 2;
            _scanCode(stopAtClosingBrace: true);
            buffer.write('}');
            continue;
          }
          buffer.write(r'$');
          index++;
          while (index < source.length &&
              _isIdentifier(source.codeUnitAt(index))) {
            buffer.write(source[index]);
            index++;
          }
          continue;
        }
      }

      buffer.write(char);
      index++;
    }
  }

  bool _isIdentifier(int codeUnit) =>
      (codeUnit >= 0x30 && codeUnit <= 0x39) ||
      (codeUnit >= 0x41 && codeUnit <= 0x5A) ||
      (codeUnit >= 0x61 && codeUnit <= 0x7A) ||
      codeUnit == 0x5F ||
      codeUnit == 0x24;

  String _unescape(String value) => switch (value) {
    'n' => '\n',
    't' => '\t',
    'r' => '\r',
    '\\' => '\\',
    "'" => "'",
    '"' => '"',
    r'$' => r'$',
    _ => value,
  };
}

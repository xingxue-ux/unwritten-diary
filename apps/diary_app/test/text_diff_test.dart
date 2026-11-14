import 'package:flutter_test/flutter_test.dart';
import 'package:unwritten_diary/src/text_diff.dart';

void main() {
  test('相同的正文没有改动', () {
    final lines = diffLines('第一段\n第二段', '第一段\n第二段');
    expect(hasChanges(lines), isFalse);
    expect(lines.length, 2);
  });

  test('改一段只标出改的那段', () {
    final lines = diffLines('第一段\n第二段\n第三段', '第一段\n第二段改过\n第三段');
    expect(hasChanges(lines), isTrue);
    expect(
      lines.where((line) => line.kind == 'removed').map((line) => line.text),
      ['第二段'],
    );
    expect(
      lines.where((line) => line.kind == 'added').map((line) => line.text),
      ['第二段改过'],
    );
    // 没动的两段仍然是 same。
    expect(lines.where((line) => line.kind == 'same').length, 2);
  });

  test('新增和删除的行都标出来', () {
    final lines = diffLines('A\nB', 'A\nX\nB');
    expect(lines.map((line) => line.kind), ['same', 'added', 'same']);
  });

  test('末尾换行不算差异', () {
    expect(hasChanges(diffLines('一段\n', '一段')), isFalse);
  });
}

/// 逐行对比两段正文。
///
/// 只做**行级** LCS：日记正文是自然段，行级差异已经足够看清「改了哪些字」，
/// 而且不会像字符级那样把一句话切得七零八落。长文本用 O(n·m) 的表，
/// 日记一天几十行，完全够用；真正的超大文本要走别的算法，这里不做。
library;

/// 一行差异。[kind] 是 `same` / `added` / `removed`。
class DiffLine {
  const DiffLine(this.kind, this.text);

  final String kind;
  final String text;

  @override
  String toString() => '$kind: $text';
}

/// 返回从 [before] 到 [after] 的行级差异。
List<DiffLine> diffLines(String before, String after) {
  final left = before.split('\n');
  final right = after.split('\n');
  // 末尾换行会产生一个空行，去掉它，免得每份正文都多一条「空行相同」。
  if (left.isNotEmpty && left.last.isEmpty) left.removeLast();
  if (right.isNotEmpty && right.last.isEmpty) right.removeLast();

  final table = List.generate(
    left.length + 1,
    (_) => List<int>.filled(right.length + 1, 0),
  );
  for (var i = left.length - 1; i >= 0; i--) {
    for (var j = right.length - 1; j >= 0; j--) {
      table[i][j] = left[i] == right[j]
          ? table[i + 1][j + 1] + 1
          : (table[i + 1][j] >= table[i][j + 1]
                ? table[i + 1][j]
                : table[i][j + 1]);
    }
  }

  final lines = <DiffLine>[];
  var i = 0;
  var j = 0;
  while (i < left.length && j < right.length) {
    if (left[i] == right[j]) {
      lines.add(DiffLine('same', left[i]));
      i++;
      j++;
    } else if (table[i + 1][j] >= table[i][j + 1]) {
      lines.add(DiffLine('removed', left[i]));
      i++;
    } else {
      lines.add(DiffLine('added', right[j]));
      j++;
    }
  }
  while (i < left.length) {
    lines.add(DiffLine('removed', left[i]));
    i++;
  }
  while (j < right.length) {
    lines.add(DiffLine('added', right[j]));
    j++;
  }
  return lines;
}

/// 差异里有没有真的改动（全是 `same` 就是没改）。
bool hasChanges(List<DiffLine> lines) =>
    lines.any((line) => line.kind != 'same');

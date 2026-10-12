enum RevisionLineKind { same, added, removed }

class RevisionLine {
  const RevisionLine(this.kind, this.text);
  final RevisionLineKind kind;
  final String text;
}

/// Line diff for a read-only review. Bounds memory for unusually long notes.
List<RevisionLine> revisionDiff(String oldText, String newText) {
  final before = oldText.split('\n');
  final after = newText.split('\n');
  if (before.length * after.length > 160000) {
    return [
      for (final line in before) RevisionLine(RevisionLineKind.removed, line),
      for (final line in after) RevisionLine(RevisionLineKind.added, line),
    ];
  }
  final lengths = List.generate(
    before.length + 1,
    (_) => List<int>.filled(after.length + 1, 0),
  );
  for (var i = before.length - 1; i >= 0; i--) {
    for (var j = after.length - 1; j >= 0; j--) {
      lengths[i][j] = before[i] == after[j]
          ? lengths[i + 1][j + 1] + 1
          : (lengths[i + 1][j] > lengths[i][j + 1]
                ? lengths[i + 1][j]
                : lengths[i][j + 1]);
    }
  }
  final lines = <RevisionLine>[];
  var i = 0;
  var j = 0;
  while (i < before.length || j < after.length) {
    if (i < before.length && j < after.length && before[i] == after[j]) {
      lines.add(RevisionLine(RevisionLineKind.same, before[i++]));
      j++;
    } else if (j < after.length &&
        (i == before.length || lengths[i][j + 1] > lengths[i + 1][j])) {
      lines.add(RevisionLine(RevisionLineKind.added, after[j++]));
    } else {
      lines.add(RevisionLine(RevisionLineKind.removed, before[i++]));
    }
  }
  return lines;
}

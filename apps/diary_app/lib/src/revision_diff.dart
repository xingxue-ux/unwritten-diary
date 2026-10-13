enum RevisionLineKind { same, added, removed }

class RevisionLine {
  const RevisionLine(this.kind, this.text);
  final RevisionLineKind kind;
  final String text;
}

/// Character-level diff so a one-word edit does not mark the whole paragraph.
/// The bounded middle avoids quadratic memory use on unusually long notes.
List<RevisionLine> revisionDiff(String oldText, String newText) {
  final before = oldText.runes.toList(growable: false);
  final after = newText.runes.toList(growable: false);
  var prefix = 0;
  while (prefix < before.length &&
      prefix < after.length &&
      before[prefix] == after[prefix]) {
    prefix++;
  }
  var suffix = 0;
  while (suffix < before.length - prefix &&
      suffix < after.length - prefix &&
      before[before.length - suffix - 1] == after[after.length - suffix - 1]) {
    suffix++;
  }

  final oldMiddle = before.sublist(prefix, before.length - suffix);
  final newMiddle = after.sublist(prefix, after.length - suffix);
  final result = <RevisionLine>[];
  void append(RevisionLineKind kind, Iterable<int> runes) {
    final text = String.fromCharCodes(runes);
    if (text.isEmpty) return;
    if (result.isNotEmpty && result.last.kind == kind) {
      result[result.length - 1] = RevisionLine(kind, result.last.text + text);
    } else {
      result.add(RevisionLine(kind, text));
    }
  }

  append(RevisionLineKind.same, before.take(prefix));
  if (oldMiddle.length * newMiddle.length > 160000) {
    append(RevisionLineKind.removed, oldMiddle);
    append(RevisionLineKind.added, newMiddle);
  } else {
    final lengths = List.generate(
      oldMiddle.length + 1,
      (_) => List<int>.filled(newMiddle.length + 1, 0),
    );
    for (var i = oldMiddle.length - 1; i >= 0; i--) {
      for (var j = newMiddle.length - 1; j >= 0; j--) {
        lengths[i][j] = oldMiddle[i] == newMiddle[j]
            ? lengths[i + 1][j + 1] + 1
            : (lengths[i + 1][j] > lengths[i][j + 1]
                  ? lengths[i + 1][j]
                  : lengths[i][j + 1]);
      }
    }
    var i = 0;
    var j = 0;
    while (i < oldMiddle.length || j < newMiddle.length) {
      if (i < oldMiddle.length &&
          j < newMiddle.length &&
          oldMiddle[i] == newMiddle[j]) {
        append(RevisionLineKind.same, [oldMiddle[i++]]);
        j++;
      } else if (j < newMiddle.length &&
          (i == oldMiddle.length || lengths[i][j + 1] > lengths[i + 1][j])) {
        append(RevisionLineKind.added, [newMiddle[j++]]);
      } else {
        append(RevisionLineKind.removed, [oldMiddle[i++]]);
      }
    }
  }
  append(RevisionLineKind.same, before.skip(before.length - suffix));
  return result;
}

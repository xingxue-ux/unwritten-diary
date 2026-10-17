import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:unwritten_diary/src/file_import_journal.dart';

void main() {
  test('a torn log line cannot hide earlier or later checkpoints', () async {
    final directory = await Directory.systemTemp.createTemp('import-journal-');
    try {
      final journal = FileImportJournal(
        File('${directory.path}/imports.jsonl'),
      );
      await journal.append({'operationId': 'first', 'phase': 'copying'});
      await journal.file.writeAsString('{broken', mode: FileMode.append);
      await journal.append({'operationId': 'first', 'phase': 'verifying'});
      await journal.append({'operationId': 'second', 'phase': 'failed'});
      final entries = await journal.load();
      expect(entries['first']?['phase'], 'verifying');
      expect(entries['second']?['phase'], 'failed');
    } finally {
      await directory.delete(recursive: true);
    }
  });
}

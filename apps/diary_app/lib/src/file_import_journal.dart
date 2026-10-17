import 'dart:convert';
import 'dart:io';

/// A small append-only recovery log. A torn final line is ignored; the next
/// append starts on a new line so earlier checkpoints stay readable.
class FileImportJournal {
  FileImportJournal(this.file);

  final File file;

  Future<Map<String, Map<String, dynamic>>> load() async {
    if (!await file.exists()) return {};
    final latest = <String, Map<String, dynamic>>{};
    await for (final line
        in file
            .openRead()
            .transform(const Utf8Decoder(allowMalformed: true))
            .transform(const LineSplitter())) {
      try {
        final value = jsonDecode(line);
        if (value is Map<String, dynamic> && value['operationId'] is String) {
          latest[value['operationId'] as String] = value;
        }
      } on FormatException {
        // The process may have ended during this append. Older lines remain.
      }
    }
    return latest;
  }

  Future<void> append(Map<String, Object?> checkpoint) async {
    await file.parent.create(recursive: true);
    final output = await file.open(mode: FileMode.append);
    try {
      await output.writeString('\n${jsonEncode(checkpoint)}\n');
      await output.flush();
    } finally {
      await output.close();
    }
  }
}

import 'dart:async';
import 'dart:io';

import 'package:diary_api/diary_api.dart';
import 'package:diary_mock/diary_mock.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:unwritten_diary/src/file_import_controller.dart';
import 'package:unwritten_diary/src/file_import_journal.dart';

class ImportTestApi extends MockDiaryApi {
  ImportTestApi(this.directory);
  final Directory directory;
  ImportState importedState = ImportState.ready;
  int commits = 0;
  int prepared = 0;
  int cancelled = 0;
  bool loseFirstCommitAcknowledgement = false;
  final Map<String, ImportStatus> importStatuses = {};
  @override
  Future<ImportTicket> prepareImport({
    required String captureId,
    required String displayName,
    String? mimeHint,
    int? sizeHint,
    required String operationId,
  }) async {
    prepared++;
    return ImportTicket(
      importId: operationId,
      stagingTicket: '${directory.path}/$operationId',
      maxBytes: 1024,
    );
  }

  @override
  Future<ImportStatus> importStatus(String importId) async =>
      importStatuses[importId] ??
      ImportStatus(
        importId: importId,
        state: ImportState.prepared,
        copiedBytes: 0,
        totalBytes: 0,
      );
  @override
  Future<ImportStatus> finishImport({
    required String importId,
    required String ticket,
    required ImportManifest manifest,
  }) async {
    final status = ImportStatus(
      importId: importId,
      state: importedState,
      copiedBytes: manifest.copiedBytes,
      totalBytes: manifest.copiedBytes,
    );
    importStatuses[importId] = status;
    return status;
  }

  @override
  Future<void> cancelImport(String importId) async {
    cancelled++;
    importStatuses.remove(importId);
  }

  @override
  Future<CommitResult> commit({
    required String id,
    required int expectedRevision,
    required String operationId,
  }) async {
    final result = await super.commit(
      id: id,
      expectedRevision: expectedRevision,
      operationId: operationId,
    );
    commits++;
    if (loseFirstCommitAcknowledgement) {
      loseFirstCommitAcknowledgement = false;
      throw StateError('commit response lost');
    }
    return result;
  }
}

void main() {
  late Directory directory;
  late ImportTestApi api;
  late FileImportController controller;
  setUp(() async {
    directory = await Directory.systemTemp.createTemp('import-ui-');
    api = ImportTestApi(directory);
    await api.open();
    controller = FileImportController(api);
  });
  tearDown(() async {
    await api.close();
    await directory.delete(recursive: true);
  });
  SelectedImportFile file(String name) =>
      SelectedImportFile(name: name, openRead: () => Stream.value([1, 2, 3]));

  test(
    'one file can fail without preventing the next file from saving',
    () async {
      await controller.add([
        SelectedImportFile(
          name: 'bad',
          openRead: () => Stream.error(StateError('read failed')),
        ),
        file('good'),
      ]);
      expect(controller.entries.map((e) => e.phase), [
        FileImportPhase.failed,
        FileImportPhase.saved,
      ]);
      expect(api.commits, 1);
    },
  );
  test('verifying is never displayed as saved or committed', () async {
    api.importedState = ImportState.verifying;
    await controller.add([file('waiting')]);
    expect(controller.entries.single.phase, FileImportPhase.verifying);
    expect(api.commits, 0);
  });
  test('queued cancellation does not create a core import', () async {
    final done = controller.add([file('cancelled')]);
    controller.cancel(controller.entries.single);
    await done;
    expect(controller.entries.single.phase, FileImportPhase.interrupted);
    expect(api.prepared, 0);
  });
  test(
    'retry keeps the capture and operation identity after source failure',
    () async {
      var attempts = 0;
      await controller.add([
        SelectedImportFile(
          name: 'again',
          openRead: () => ++attempts == 1
              ? Stream.error(StateError('source unavailable'))
              : Stream.value([1, 2, 3]),
        ),
      ]);
      final entry = controller.entries.single;
      final captureId = entry.captureId;
      final operationId = entry.operationId;
      expect(entry.phase, FileImportPhase.failed);
      expect(entry.canRetry, isTrue);
      await controller.retry(entry);
      expect(entry.phase, FileImportPhase.saved);
      expect(entry.captureId, captureId);
      expect(entry.operationId, operationId);
      expect(entry.importId, operationId);
      expect(attempts, 2);
      expect(api.commits, 1);
    },
  );
  test(
    'lost commit response recovers without copying the source again',
    () async {
      api.loseFirstCommitAcknowledgement = true;
      var sourceOpens = 0;
      await controller.add([
        SelectedImportFile(
          name: 'lost-ack',
          openRead: () {
            sourceOpens++;
            return Stream.value([1, 2, 3]);
          },
        ),
      ]);
      final entry = controller.entries.single;
      expect(entry.phase, FileImportPhase.failed);
      expect(entry.canRetry, isTrue);
      await controller.retry(entry);
      expect(entry.phase, FileImportPhase.saved);
      expect(sourceOpens, 1);
      expect(api.commits, 1);
    },
  );
  test('stopped copy asks core to cancel the import session', () async {
    final source = StreamController<List<int>>();
    final firstChunk = Completer<void>();
    controller.addListener(() {
      if (controller.entries.single.copiedBytes > 0 &&
          !firstChunk.isCompleted) {
        firstChunk.complete();
      }
    });
    final done = controller.add([
      SelectedImportFile(name: 'stop', openRead: () => source.stream),
    ]);
    source.add([1]);
    await firstChunk.future;
    final entry = controller.entries.single;
    controller.cancel(entry);
    await done;
    expect(entry.phase, FileImportPhase.interrupted);
    expect(entry.canRetry, isFalse);
    expect(api.cancelled, 1);
    await source.close();
  });
  test(
    'restart finds a ready import and commits without reopening the source',
    () async {
      api.importedState = ImportState.verifying;
      final journal = FileImportJournal(
        File('${directory.path}/imports.jsonl'),
      );
      controller.attachJournal(journal);
      var sourceOpens = 0;
      await controller.add([
        SelectedImportFile(
          name: 'restart',
          openRead: () {
            sourceOpens++;
            return Stream.value([1, 2, 3]);
          },
        ),
      ]);
      final previous = controller.entries.single;
      expect(previous.phase, FileImportPhase.verifying);
      api.importStatuses[previous.importId!] = ImportStatus(
        importId: previous.importId!,
        state: ImportState.ready,
        copiedBytes: 3,
        totalBytes: 3,
      );
      final resumed = FileImportController(api)..attachJournal(journal);
      await resumed.restore();
      expect(resumed.entries.single.phase, FileImportPhase.saved);
      expect(resumed.entries.single.captureId, previous.captureId);
      expect(sourceOpens, 1);
      expect(api.commits, 1);
      final reopened = FileImportController(api)..attachJournal(journal);
      await reopened.restore();
      expect(reopened.entries, isEmpty);
    },
  );
  test('restart after a lost commit response does not duplicate the record', () async {
    final journal = FileImportJournal(File('${directory.path}/lost-ack.jsonl'));
    controller.attachJournal(journal);
    api.loseFirstCommitAcknowledgement = true;
    var sourceOpens = 0;
    await controller.add([
      SelectedImportFile(name: 'once', openRead: () {
        sourceOpens++;
        return Stream.value([1, 2, 3]);
      }),
    ]);
    expect(controller.entries.single.phase, FileImportPhase.failed);
    final resumed = FileImportController(api)..attachJournal(journal);
    await resumed.restore();
    expect(resumed.entries.single.phase, FileImportPhase.saved);
    expect(sourceOpens, 1);
    expect(api.commits, 1);
  });
  test(
    'disposing the page listener does not stop the durable handoff',
    () async {
      final source = StreamController<List<int>>();
      final copying = Completer<void>();
      controller.addListener(() {
        if (controller.entries.single.copiedBytes > 0 && !copying.isCompleted) {
          copying.complete();
        }
      });
      final done = controller.add([
        SelectedImportFile(name: 'continued', openRead: () => source.stream),
      ]);
      source.add([1]);
      await copying.future;
      controller.dispose();
      await source.close();
      await done;
      expect(controller.entries.single.phase, FileImportPhase.saved);
      expect(api.commits, 1);
    },
  );
}

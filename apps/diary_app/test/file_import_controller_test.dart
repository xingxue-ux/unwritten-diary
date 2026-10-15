import 'dart:async';
import 'dart:io';

import 'package:diary_api/diary_api.dart';
import 'package:diary_mock/diary_mock.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:unwritten_diary/src/file_import_controller.dart';

class ImportTestApi extends MockDiaryApi {
  ImportTestApi(this.directory);
  final Directory directory;
  ImportState importedState = ImportState.ready;
  int commits = 0;
  int prepared = 0;
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
  Future<ImportStatus> importStatus(String importId) async => ImportStatus(
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
  }) async => ImportStatus(
    importId: importId,
    state: importedState,
    copiedBytes: manifest.copiedBytes,
    totalBytes: manifest.copiedBytes,
  );
  @override
  Future<CommitResult> commit({
    required String id,
    required int expectedRevision,
    required String operationId,
  }) async {
    final capture = await getCapture(id);
    expect(expectedRevision, capture.revision);
    commits++;
    return CommitResult(
      capture: capture.copyWith(state: CaptureState.committed),
    );
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

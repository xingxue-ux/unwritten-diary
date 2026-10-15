import 'dart:io';
import 'package:diary_api/diary_api.dart';
import 'package:platform_host/platform_host.dart';
import 'package:test/test.dart';

class ImportApi implements DiaryApi {
  ImportApi(this.path);
  final String path;
  ImportState state = ImportState.prepared;
  ImportManifest? manifest;
  int finishes = 0;
  @override
  Future<ImportTicket> prepareImport({
    required String captureId,
    required String displayName,
    String? mimeHint,
    int? sizeHint,
    required String operationId,
  }) async =>
      ImportTicket(importId: 'import-1', stagingTicket: path, maxBytes: 4);
  @override
  Future<ImportStatus> importStatus(String importId) async => ImportStatus(
    importId: importId,
    state: state,
    copiedBytes: 0,
    totalBytes: 0,
  );
  @override
  Future<ImportStatus> finishImport({
    required String importId,
    required String ticket,
    required ImportManifest manifest,
  }) async {
    this.manifest = manifest;
    finishes++;
    return ImportStatus(
      importId: importId,
      state: ImportState.verifying,
      copiedBytes: manifest.copiedBytes,
      totalBytes: manifest.copiedBytes,
    );
  }

  @override
  dynamic noSuchMethod(Invocation invocation) => super.noSuchMethod(invocation);
}

void main() {
  late Directory directory;
  late ImportApi api;
  setUp(() async {
    directory = await Directory.systemTemp.createTemp('diary-import-');
    api = ImportApi('${directory.path}/ticket');
  });
  tearDown(() => directory.delete(recursive: true));
  Future<ImportStatus> run({CopyCancellation? cancellation}) =>
      FileImportCoordinator(api).importFile(
        captureId: 'capture',
        displayName: '../untrusted-name.txt',
        operationId: 'op',
        openRead: () => Stream.value([97, 98, 99]),
        cancellation: cancellation ?? CopyCancellation(),
      );

  test(
    'finishes only durable bytes and keeps verifying distinct from ready',
    () async {
      final result = await run();
      expect(result.state, ImportState.verifying);
      expect(await File(api.path).readAsBytes(), [97, 98, 99]);
      expect(api.manifest!.copiedBytes, 3);
      expect(
        api.manifest!.sha256,
        'ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad',
      );
    },
  );
  test('idempotent retry never truncates a ready file', () async {
    api.state = ImportState.ready;
    await File(api.path).writeAsString('kept');
    expect((await run()).state, ImportState.ready);
    expect(await File(api.path).readAsString(), 'kept');
    expect(api.finishes, 0);
  });
  test('source failure retains import identity without finishing', () async {
    await expectLater(
      FileImportCoordinator(api).importFile(
        captureId: 'capture',
        displayName: 'test',
        operationId: 'op',
        openRead: () => Stream.error(const FileSystemException('no space')),
        cancellation: CopyCancellation(),
      ),
      throwsA(
        isA<ImportInterrupted>().having((e) => e.importId, 'id', 'import-1'),
      ),
    );
    expect(api.finishes, 0);
  });
}

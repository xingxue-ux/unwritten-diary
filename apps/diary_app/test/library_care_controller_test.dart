import 'package:diary_api/diary_api.dart';
import 'package:diary_mock/diary_mock.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:unwritten_diary/src/library_care_controller.dart';

class LibraryCareApi extends MockDiaryApi {
  int backupCalls = 0;
  int restoreCalls = 0;
  int trashCalls = 0;
  int purgeCalls = 0;

  final now = DateTime.utc(2026, 10, 17, 12);

  Capture get trashedCapture => Capture(
    id: 'trashed-1',
    revision: 4,
    state: CaptureState.trashed,
    occurredAt: now,
    createdAt: now,
    updatedAt: now,
    timeZone: 'Asia/Shanghai',
    utcOffsetMinutes: 480,
    dayKey: '2026-10-17',
    draftText: '回收站里的记录',
  );

  @override
  Future<JobTicket> createBackup(BackupRequest request) async {
    backupCalls += 1;
    expect(request.targetTicket, 'chosen-target');
    return const JobTicket(jobId: 'backup-job');
  }

  @override
  Future<Job> getJob(String id) async => Job(
    id: id,
    kind: 'backup',
    state: JobState.succeeded,
    createdAt: now,
    updatedAt: now,
    progress: const JobProgress(completed: 1, total: 1),
  );

  @override
  Future<BackupInspection> inspectBackup(String packageTicket) async =>
      BackupInspection(
        packageTicket: packageTicket,
        schemaVersion: 1,
        apiVersion: kApiVersion,
        byteSize: 4096,
        createdAt: now,
        assetCount: 2,
      );

  @override
  Future<JobTicket> restoreBackup({
    required String packageTicket,
    RestoreOptions options = const RestoreOptions(),
  }) async {
    restoreCalls += 1;
    expect(options.restoreAsNewLibrary, isTrue);
    return const JobTicket(jobId: 'restore-job');
  }

  @override
  Future<CapturePage> listCaptures({
    String? dayKey,
    String? cursor,
    int pageSize = 20,
  }) async => CapturePage(captures: [trashedCapture]);

  @override
  Future<Capture> restoreCapture({
    required String id,
    required int expectedRevision,
    required String operationId,
  }) async => trashedCapture.copyWith(
    revision: expectedRevision + 1,
    state: CaptureState.committed,
  );

  @override
  Future<Capture> trashCapture({
    required String id,
    required int expectedRevision,
    required String operationId,
  }) async {
    trashCalls += 1;
    return trashedCapture;
  }

  @override
  Future<PurgePreview> previewPurge(String id) async => PurgePreview(
    captureId: id,
    previewToken: 'single-use-token',
    assetIds: const ['asset-1'],
  );

  @override
  Future<PurgeResult> purgeCapture({
    required String id,
    required String previewToken,
    required String operationId,
  }) async {
    purgeCalls += 1;
    expect(previewToken, 'single-use-token');
    return PurgeResult(captureId: id, purgedAssetIds: const ['asset-1']);
  }
}

void main() {
  test(
    'unsupported backup remains disabled and does not call the API',
    () async {
      final api = LibraryCareApi();
      final controller = LibraryCareController(
        api,
        capabilities: const {'captures.list'},
        captureCount: 3,
      );
      addTearDown(controller.dispose);

      await controller.createBackup('chosen-target');

      expect(api.backupCalls, 0);
      expect(controller.phase, LibraryCarePhase.failed);
      expect(controller.message, contains('没有开放'));
    },
  );

  test('backup and restore expose verified job completion', () async {
    final api = LibraryCareApi();
    final controller = LibraryCareController(
      api,
      capabilities: const {
        'backups.create',
        'backups.inspect',
        'backups.restore',
        'jobs.get',
      },
      captureCount: 3,
    );
    addTearDown(controller.dispose);

    await controller.createBackup('chosen-target');
    expect(api.backupCalls, 1);
    expect(controller.phase, LibraryCarePhase.succeeded);
    expect(controller.progress, 1);

    await controller.inspectBackup('backup-package');
    expect(controller.inspection?.issues, isEmpty);
    await controller.restoreInspectedBackup();
    expect(api.restoreCalls, 1);
    expect(controller.phase, LibraryCarePhase.succeeded);
  });

  test('trash requires preview token before permanent purge', () async {
    final api = LibraryCareApi();
    final controller = LibraryCareController(
      api,
      capabilities: const {
        'captures.list',
        'captures.trash',
        'captures.restore',
        'captures.previewPurge',
        'captures.purge',
      },
      captureCount: 1,
    );
    addTearDown(controller.dispose);

    await controller.loadTrash();
    expect(controller.trashedCaptures, hasLength(1));
    expect(await controller.trashCapture(api.trashedCapture), isTrue);
    expect(api.trashCalls, 1);
    final preview = await controller.previewPurge(
      controller.trashedCaptures.single,
    );
    expect(preview?.assetIds, const ['asset-1']);
    expect(await controller.purge(preview!), isTrue);
    expect(api.purgeCalls, 1);
  });
}

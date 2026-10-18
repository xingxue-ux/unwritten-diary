import 'package:diary_api/diary_api.dart';
import 'package:diary_mock/diary_mock.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:unwritten_diary/src/readable_export_controller.dart';

class ExportApi extends MockDiaryApi {
  final now = DateTime.utc(2026, 10, 17);
  ExportRequest? request;

  @override
  Future<ExportTicket> createExport(ExportRequest request) async {
    this.request = request;
    return const ExportTicket(
      jobId: 'export-job',
      packageTicket: 'export-package',
    );
  }

  @override
  Future<Job> getJob(String id) async => Job(
    id: id,
    kind: 'export',
    state: JobState.succeeded,
    createdAt: now,
    updatedAt: now,
    progress: const JobProgress(completed: 10, total: 10),
  );
}

void main() {
  test(
    'creates an export with explicit range, format and assets choice',
    () async {
      final api = ExportApi();
      final controller = ReadableExportController(
        api,
        capabilities: const {'exports.create', 'jobs.get'},
      );
      addTearDown(controller.dispose);

      await controller.create(
        from: DateTime(2026, 9, 1),
        to: DateTime(2026, 10, 17),
        format: 'markdown',
        includeAssets: true,
      );

      expect(api.request?.fromDayKey, '2026-09-01');
      expect(api.request?.toDayKey, '2026-10-17');
      expect(api.request?.includeAssets, isTrue);
      expect(controller.job?.state, JobState.succeeded);
      expect(controller.ticket?.packageTicket, 'export-package');
      expect(controller.message, contains('可以保存'));
    },
  );

  test('invalid range never calls the API', () async {
    final api = ExportApi();
    final controller = ReadableExportController(
      api,
      capabilities: const {'exports.create'},
    );
    addTearDown(controller.dispose);

    await controller.create(
      from: DateTime(2026, 10, 18),
      to: DateTime(2026, 10, 17),
      format: 'markdown',
      includeAssets: false,
    );

    expect(api.request, isNull);
    expect(controller.message, contains('不能晚于'));
  });
}

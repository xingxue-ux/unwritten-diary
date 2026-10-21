import 'package:diary_api/diary_api.dart';
import 'package:diary_mock/diary_mock.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:unwritten_diary/src/index_settings_controller.dart';

class IndexApi extends MockDiaryApi {
  bool ready = false;
  bool completeOnRun = false;
  int queuedCount = 0;
  Job? currentJob;

  @override
  Future<IndexStatus> indexesStatus({Set<String>? sourceScope}) async =>
      IndexStatus(
        coverage: Coverage.complete,
        keywordIndexReady: true,
        semanticIndexReady: ready,
        reasons: ready ? const [] : const ['本地模型已更换，需要重建语义索引'],
      );

  @override
  Future<JobTicket> rebuildSemanticIndex({
    required String reason,
    required String operationId,
  }) async {
    queuedCount++;
    currentJob ??= Job(
      id: 'semantic-job',
      kind: 'semantic_index',
      state: JobState.queued,
      createdAt: DateTime.utc(2026, 10, 19),
      updatedAt: DateTime.utc(2026, 10, 19),
    );
    return JobTicket(jobId: currentJob!.id);
  }

  @override
  Future<JobPage> listJobs({
    Set<JobState>? states,
    String? cursor,
    int pageSize = 20,
  }) async => JobPage(
    jobs:
        currentJob != null &&
            (states == null || states.contains(currentJob!.state))
        ? [currentJob!]
        : const [],
  );

  @override
  Future<Job> getJob(String id) async => currentJob!;

  @override
  Future<DueRunSummary> runDueJobs({
    required int budgetMs,
    required String trigger,
  }) async {
    if (completeOnRun) {
      ready = true;
      currentJob = currentJob!.copyWith(
        state: JobState.succeeded,
        progress: const JobProgress(completed: 3, total: 3),
      );
    }
    return DueRunSummary(startedAt: DateTime.utc(2026, 10, 19));
  }
}

void main() {
  const capabilities = {
    'indexes.status',
    'indexes.rebuildSemantic',
    'jobs.get',
    'jobs.list',
    'jobs.runDue',
  };

  test(
    'queued rebuild stays visible and is recovered after reopening',
    () async {
      final api = IndexApi();
      final first = IndexSettingsController(api, capabilities: capabilities);
      await first.load();
      expect(first.status?.semanticIndexReady, isFalse);

      await first.rebuild();
      await first.rebuild();
      expect(api.queuedCount, 1);
      expect(first.hasActiveJob, isTrue);
      expect(first.message, isNot(contains('已重建')));
      first.dispose();

      final reopened = IndexSettingsController(api, capabilities: capabilities);
      addTearDown(reopened.dispose);
      await reopened.load();
      expect(reopened.job?.id, 'semantic-job');
      expect(reopened.hasActiveJob, isTrue);
    },
  );

  test(
    'success is shown only after both job and index status are ready',
    () async {
      final api = IndexApi()..completeOnRun = true;
      final controller = IndexSettingsController(
        api,
        capabilities: capabilities,
      );
      addTearDown(controller.dispose);
      await controller.load();
      await controller.rebuild();
      await Future<void>.delayed(const Duration(milliseconds: 20));
      await controller.refresh();
      expect(controller.job?.state, JobState.succeeded);
      expect(controller.status?.semanticIndexReady, isTrue);
      expect(controller.message, contains('可以使用语义检索'));
    },
  );

  test('missing rebuild capability never queues a fake task', () async {
    final api = IndexApi();
    final controller = IndexSettingsController(
      api,
      capabilities: const {'indexes.status'},
    );
    addTearDown(controller.dispose);
    await controller.load();
    await controller.rebuild();
    expect(controller.canRebuild, isFalse);
    expect(api.queuedCount, 0);
  });
}

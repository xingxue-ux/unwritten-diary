import 'dart:async';
import 'dart:io';

import 'package:diary_api/diary_api.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:unwritten_diary/src/ai_diary_controller.dart';

class AiFakeApi implements DiaryApi {
  bool hasProvider = true;
  int generations = 0;
  int cancellations = 0;
  int retries = 0;
  DiaryDay day = DiaryDay(
    dayKey: '2026-10-18',
    timeZone: 'Asia/Shanghai',
    updatedAt: DateTime.utc(2026, 10, 18),
    revision: 1,
    materialCounts: const MaterialCounts(captures: 2, processed: 2),
  );
  final versions = <DiaryVersion>[];
  Job? job;
  Completer<DiaryDay>? delayedDay;
  Completer<void>? dayRequested;

  @override
  Future<List<Provider>> listProviders() async => hasProvider
      ? [
          const Provider(
            id: 'local',
            label: '本机模型',
            adapterId: 'openai_compatible',
            capabilities: {ProviderCapability.generateText},
            enabled: true,
            endpoint: 'http://127.0.0.1:11434/v1',
            testStatus: ProviderTestStatus.ok,
          ),
        ]
      : [];

  @override
  Future<DiaryDay> getDay(String dayKey) async {
    if (dayKey == '2026-10-18' && delayedDay != null) {
      if (dayRequested?.isCompleted == false) dayRequested!.complete();
      return delayedDay!.future;
    }
    return day;
  }

  @override
  Future<DiaryVersionPage> listVersions(
    String dayKey, {
    String? cursor,
    int pageSize = 20,
  }) async => DiaryVersionPage(versions: versions);

  @override
  Future<JobPage> listJobs({
    Set<JobState>? states,
    String? cursor,
    int pageSize = 20,
  }) async => JobPage(jobs: job == null ? [] : [job!]);

  @override
  Future<Job> getJob(String id) async => job!;

  @override
  Future<DiaryGenerationTicket> generateDiary({
    required String dayKey,
    String? styleId,
    DiaryGenerationReason reason = DiaryGenerationReason.manual,
    required String operationId,
  }) async {
    generations++;
    job = Job(
      id: 'job-$generations',
      kind: 'diary_generate',
      state: JobState.queued,
      createdAt: DateTime.utc(2026, 10, 18),
      updatedAt: DateTime.utc(2026, 10, 18),
    );
    return DiaryGenerationTicket(jobId: job!.id, sourceSnapshotId: 'snapshot');
  }

  @override
  Future<DiaryDay> setCurrentVersion({
    required String dayKey,
    required String versionId,
    required int expectedRevision,
  }) async {
    expect(expectedRevision, day.revision);
    day = day.copyWith(currentVersionId: versionId, revision: day.revision + 1);
    return day;
  }

  @override
  Future<Job> cancelJob({
    required String id,
    required String operationId,
  }) async {
    cancellations++;
    job = job!.copyWith(state: JobState.cancelled);
    return job!;
  }

  @override
  Future<Job> retryJob({
    required String id,
    required String operationId,
  }) async {
    retries++;
    job = job!.copyWith(state: JobState.queued);
    return job!;
  }

  @override
  dynamic noSuchMethod(Invocation invocation) => super.noSuchMethod(invocation);
}

void main() {
  test(
    'without a provider, generation gives an actionable next step',
    () async {
      final directory = await Directory.systemTemp.createTemp('ai-diary-');
      addTearDown(() => directory.delete(recursive: true));
      final api = AiFakeApi()..hasProvider = false;
      final controller = AiDiaryController(
        api,
        FileAiLocalStore(File('${directory.path}/state.json')),
      );
      addTearDown(controller.dispose);
      await controller.open('2026-10-18');
      await controller.generate();
      expect(api.generations, 0);
      expect(controller.error, contains('配置'));
    },
  );

  test(
    'job survives reopening and candidate adoption is a core write',
    () async {
      final directory = await Directory.systemTemp.createTemp('ai-diary-');
      addTearDown(() => directory.delete(recursive: true));
      final store = FileAiLocalStore(File('${directory.path}/state.json'));
      final api = AiFakeApi();
      final first = AiDiaryController(api, store);
      await first.open('2026-10-18');
      await first.generate();
      expect(api.generations, 1);
      expect(first.job?.state, JobState.queued);
      first.dispose();

      final candidate = DiaryVersion(
        id: 'candidate-1',
        dayKey: '2026-10-18',
        origin: DiaryVersionOrigin.generated,
        body: '真实候选正文',
        createdAt: DateTime.utc(2026, 10, 18),
      );
      api.versions.add(candidate);
      api.day = api.day.copyWith(candidateVersionIds: [candidate.id]);
      final second = AiDiaryController(api, store);
      addTearDown(second.dispose);
      await second.open('2026-10-18');
      expect(second.job?.id, 'job-1');
      expect(second.candidates.single.id, candidate.id);
      await second.adopt(candidate.id);
      expect(second.day?.currentVersionId, candidate.id);
      expect(second.day?.revision, 2);
      await second.dismissCandidate(candidate.id);
      final third = AiDiaryController(api, store);
      addTearDown(third.dispose);
      await third.open('2026-10-18');
      expect(third.dismissedIds, contains(candidate.id));
    },
  );

  test('queued work can be cancelled, failed work retried', () async {
    final directory = await Directory.systemTemp.createTemp('ai-diary-');
    addTearDown(() => directory.delete(recursive: true));
    final api = AiFakeApi();
    final controller = AiDiaryController(
      api,
      FileAiLocalStore(File('${directory.path}/state.json')),
    );
    addTearDown(controller.dispose);
    await controller.open('2026-10-18');
    await controller.generate();
    await controller.generate();
    expect(api.generations, 1);
    expect(controller.error, contains('仍在处理'));
    await controller.cancel();
    expect(api.cancellations, 1);
    expect(controller.job?.state, JobState.cancelled);
    api.job = api.job!.copyWith(state: JobState.failed);
    await controller.refresh();
    await controller.retry();
    expect(api.retries, 1);
    expect(controller.job?.state, JobState.queued);
  });

  test('a slow previous day cannot overwrite the newly opened day', () async {
    final directory = await Directory.systemTemp.createTemp('ai-diary-');
    addTearDown(() => directory.delete(recursive: true));
    final api = AiFakeApi()
      ..delayedDay = Completer<DiaryDay>()
      ..dayRequested = Completer<void>();
    final controller = AiDiaryController(
      api,
      FileAiLocalStore(File('${directory.path}/state.json')),
    );
    addTearDown(controller.dispose);

    final oldOpen = controller.open('2026-10-18');
    await api.dayRequested!.future;
    api.day = DiaryDay(
      dayKey: '2026-10-19',
      timeZone: 'Asia/Shanghai',
      updatedAt: DateTime.utc(2026, 10, 19),
      revision: 4,
      materialCounts: const MaterialCounts(captures: 1, processed: 1),
    );
    await controller.open('2026-10-19');
    expect(controller.day?.dayKey, '2026-10-19');

    api.delayedDay!.complete(
      DiaryDay(
        dayKey: '2026-10-18',
        timeZone: 'Asia/Shanghai',
        updatedAt: DateTime.utc(2026, 10, 18),
        revision: 1,
        materialCounts: const MaterialCounts(captures: 2, processed: 2),
      ),
    );
    await oldOpen;
    expect(controller.dayKey, '2026-10-19');
    expect(controller.day?.dayKey, '2026-10-19');
    expect(controller.day?.revision, 4);
  });
}

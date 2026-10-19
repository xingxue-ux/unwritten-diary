import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:diary_api/diary_api.dart';
import 'package:flutter/foundation.dart';

/// Only UI bookkeeping lives here. Versions and job outcomes remain in core.
abstract class AiLocalStore {
  Future<String?> jobFor(String dayKey);
  Future<void> setJob(String dayKey, String jobId);
  Future<Set<String>> dismissedFor(String dayKey);
  Future<void> dismiss(String dayKey, String versionId);
}

class FileAiLocalStore implements AiLocalStore {
  FileAiLocalStore(this.file);
  final File file;

  Future<Map<String, dynamic>> _read() async {
    if (!await file.exists()) return {};
    try {
      return Map<String, dynamic>.from(
        jsonDecode(await file.readAsString()) as Map,
      );
    } on Object {
      // A damaged UI hint must never prevent access to core diary versions.
      return {};
    }
  }

  Future<void> _update(void Function(Map<String, dynamic>) change) async {
    final value = await _read();
    change(value);
    await file.parent.create(recursive: true);
    await file.writeAsString(jsonEncode(value), flush: true);
  }

  @override
  Future<String?> jobFor(String dayKey) async =>
      (_read().then((value) => (value['jobs'] as Map?)?[dayKey] as String?));

  @override
  Future<void> setJob(String dayKey, String jobId) => _update((value) {
    final jobs = Map<String, dynamic>.from(value['jobs'] as Map? ?? {});
    jobs[dayKey] = jobId;
    value['jobs'] = jobs;
  });

  @override
  Future<Set<String>> dismissedFor(String dayKey) async {
    final value = await _read();
    return ((value['dismissed'] as Map?)?[dayKey] as List? ?? [])
        .whereType<String>()
        .toSet();
  }

  @override
  Future<void> dismiss(String dayKey, String versionId) => _update((value) {
    final dismissed = Map<String, dynamic>.from(
      value['dismissed'] as Map? ?? {},
    );
    final ids = ((dismissed[dayKey] as List?) ?? []).whereType<String>().toSet()
      ..add(versionId);
    dismissed[dayKey] = ids.toList();
    value['dismissed'] = dismissed;
  });
}

class AiDiaryController extends ChangeNotifier {
  AiDiaryController(this.api, this.store);

  final DiaryApi api;
  final AiLocalStore store;
  String dayKey = '';
  DiaryDay? day;
  List<DiaryVersion> versions = const [];
  List<Provider> providers = const [];
  String? providerProblem;
  List<Job> recentAiJobs = const [];
  Job? job;
  String? error;
  bool loading = false;
  bool busy = false;
  bool showDismissed = false;
  Set<String> dismissedIds = {};
  Timer? _poll;
  final Map<int, Future<void>> _refreshes = {};
  bool _disposed = false;
  int _operation = 0;
  int _viewEpoch = 0;

  bool _isCurrent(int epoch) => !_disposed && epoch == _viewEpoch;

  Provider? get generationProvider {
    for (final provider in providers) {
      if (provider.enabled &&
          provider.testStatus == ProviderTestStatus.ok &&
          provider.capabilities.contains(ProviderCapability.generateText)) {
        return provider;
      }
    }
    for (final provider in providers) {
      if (provider.enabled &&
          provider.capabilities.contains(ProviderCapability.generateText)) {
        return provider;
      }
    }
    return null;
  }

  DiaryVersion? get current {
    final id = day?.currentVersionId;
    for (final version in versions) {
      if (version.id == id) return version;
    }
    return null;
  }

  List<DiaryVersion> get candidates => versions
      .where((version) => day?.candidateVersionIds.contains(version.id) == true)
      .where((version) => showDismissed || !dismissedIds.contains(version.id))
      .toList(growable: false);

  void _notify() {
    if (!_disposed) notifyListeners();
  }

  Future<void> open(String key) async {
    _poll?.cancel();
    final epoch = ++_viewEpoch;
    dayKey = key;
    day = null;
    versions = const [];
    providers = const [];
    providerProblem = null;
    recentAiJobs = const [];
    job = null;
    error = null;
    loading = true;
    busy = false;
    showDismissed = false;
    dismissedIds = {};
    _notify();
    try {
      final savedDismissed = await store.dismissedFor(key);
      if (!_isCurrent(epoch)) return;
      dismissedIds = savedDismissed;
      try {
        final availableProviders = await api.listProviders();
        if (!_isCurrent(epoch)) return;
        providers = availableProviders;
      } on Object {
        if (!_isCurrent(epoch)) return;
        providerProblem = '服务配置暂时无法读取；已保存的日记仍可查看。';
      }
      await refresh();
    } on DiaryException catch (failure) {
      if (_isCurrent(epoch)) error = nextStepFor(failure.code);
    } on Object {
      if (_isCurrent(epoch)) {
        error = '暂时无法读取整理状态。请稍后重试；原始记录仍在本机。';
      }
    } finally {
      if (_isCurrent(epoch)) {
        loading = false;
        _notify();
      }
    }
    if (_isCurrent(epoch)) {
      _poll = Timer.periodic(const Duration(seconds: 3), (_) {
        if (_isCurrent(epoch) && !_refreshes.containsKey(epoch)) {
          unawaited(refresh());
        }
      });
    }
  }

  Future<void> refresh({bool afterInFlight = false}) async {
    if (dayKey.isEmpty) return;
    final epoch = _viewEpoch;
    final inFlight = _refreshes[epoch];
    if (inFlight != null) {
      await inFlight;
      if (afterInFlight && _isCurrent(epoch)) await refresh();
      return;
    }
    final task = _loadDay(dayKey, epoch);
    _refreshes[epoch] = task;
    try {
      await task;
    } finally {
      _refreshes.remove(epoch);
    }
  }

  Future<void> _loadDay(String key, int epoch) async {
    try {
      DiaryDay? loadedDay;
      try {
        loadedDay = await api.getDay(key);
      } on DiaryException catch (failure) {
        if (failure.code != DiaryErrorCode.notFound) rethrow;
      }
      List<DiaryVersion> loadedVersions = const [];
      if (loadedDay != null) {
        final all = <DiaryVersion>[];
        String? cursor;
        do {
          final page = await api.listVersions(
            key,
            cursor: cursor,
            pageSize: 50,
          );
          all.addAll(page.versions);
          cursor = page.nextCursor;
        } while (cursor != null);
        loadedVersions = all;
      }
      final loadedJobs = (await api.listJobs(pageSize: 50)).jobs
          .where(
            (item) =>
                item.kind == 'diary_generate' || item.kind == 'transcribe',
          )
          .take(5)
          .toList(growable: false);
      Job? loadedJob;
      final jobId = await store.jobFor(key);
      if (jobId != null) {
        try {
          loadedJob = await api.getJob(jobId);
        } on DiaryException catch (failure) {
          if (failure.code != DiaryErrorCode.notFound) rethrow;
        }
      }
      if (!_isCurrent(epoch)) return;
      day = loadedDay;
      versions = loadedVersions;
      recentAiJobs = loadedJobs;
      job = loadedJob;
      error = null;
    } on DiaryException catch (failure) {
      if (_isCurrent(epoch)) error = nextStepFor(failure.code);
    } on Object {
      if (_isCurrent(epoch)) error = '状态暂时无法刷新。请稍后重试。';
    } finally {
      if (_isCurrent(epoch)) _notify();
    }
  }

  Future<void> generate({bool regenerate = false}) async {
    if (busy || dayKey.isEmpty) return;
    final epoch = _viewEpoch;
    final key = dayKey;
    if (job case final activeJob? when !activeJob.state.isTerminal) {
      error = '上一次整理仍在处理。可等待完成或先取消，再发起新的一次。';
      _notify();
      return;
    }
    if (generationProvider == null) {
      error = '尚未配置可生成文字的 Provider。请先配置并测试服务，再回来整理。';
      _notify();
      return;
    }
    if (day == null || day!.materialCounts.captures == 0) {
      error = '这一天还没有可整理的记录。先保存一条文字或导入材料。';
      _notify();
      return;
    }
    busy = true;
    error = null;
    _notify();
    try {
      final ticket = await api.generateDiary(
        dayKey: key,
        reason: regenerate
            ? DiaryGenerationReason.regenerate
            : DiaryGenerationReason.manual,
        operationId:
            'ui-generate-${DateTime.now().microsecondsSinceEpoch}-${++_operation}',
      );
      await store.setJob(key, ticket.jobId);
      if (!_isCurrent(epoch)) return;
      final startedJob = await api.getJob(ticket.jobId);
      if (!_isCurrent(epoch)) return;
      job = startedJob;
      await refresh(afterInFlight: true);
    } on DiaryException catch (failure) {
      if (_isCurrent(epoch)) error = nextStepFor(failure.code);
    } on Object {
      if (_isCurrent(epoch)) {
        error = '整理未能启动。请检查本机资料库状态后重试。';
      }
    } finally {
      if (_isCurrent(epoch)) {
        busy = false;
        _notify();
      }
    }
  }

  Future<void> adopt(String versionId) async {
    final currentDay = day;
    if (busy || currentDay == null) return;
    final epoch = _viewEpoch;
    final key = dayKey;
    busy = true;
    error = null;
    _notify();
    try {
      final adoptedDay = await api.setCurrentVersion(
        dayKey: key,
        versionId: versionId,
        expectedRevision: currentDay.revision,
      );
      if (!_isCurrent(epoch)) return;
      day = adoptedDay;
      await refresh(afterInFlight: true);
    } on DiaryException catch (failure) {
      if (_isCurrent(epoch)) error = nextStepFor(failure.code);
      // The next scheduled refresh will reconcile a concurrent edit.
    } on Object {
      if (_isCurrent(epoch)) {
        error = '未能采纳候选。请刷新状态后重试；当前正文未被替换。';
      }
    } finally {
      if (_isCurrent(epoch)) {
        busy = false;
        _notify();
      }
    }
  }

  Future<void> cancel() async => _changeJob(retry: false);
  Future<void> retry() async => _changeJob(retry: true);

  Future<void> _changeJob({required bool retry}) async {
    final currentJob = job;
    if (busy || currentJob == null) return;
    final epoch = _viewEpoch;
    busy = true;
    error = null;
    _notify();
    try {
      final operationId =
          'ui-job-${DateTime.now().microsecondsSinceEpoch}-${++_operation}';
      final changedJob = retry
          ? await api.retryJob(id: currentJob.id, operationId: operationId)
          : await api.cancelJob(id: currentJob.id, operationId: operationId);
      if (_isCurrent(epoch)) {
        job = changedJob;
        await refresh(afterInFlight: true);
      }
    } on DiaryException catch (failure) {
      if (_isCurrent(epoch)) error = nextStepFor(failure.code);
    } on Object {
      if (_isCurrent(epoch)) error = '任务操作暂时不可用，请刷新状态后重试。';
    } finally {
      if (_isCurrent(epoch)) {
        busy = false;
        _notify();
      }
    }
  }

  Future<void> dismissCandidate(String versionId) async {
    if (day?.candidateVersionIds.contains(versionId) != true) return;
    final epoch = _viewEpoch;
    await store.dismiss(dayKey, versionId);
    if (!_isCurrent(epoch)) return;
    dismissedIds = {...dismissedIds, versionId};
    _notify();
  }

  void toggleDismissed() {
    showDismissed = !showDismissed;
    _notify();
  }

  static String nextStepFor(DiaryErrorCode code) => switch (code) {
    DiaryErrorCode.providerNotConfigured ||
    DiaryErrorCode.modelUnavailable => '模型服务尚未就绪。请到「模型服务」配置并测试生成能力。',
    DiaryErrorCode.authenticationFailed => '服务认证失败。请检查密钥后重新测试连接。',
    DiaryErrorCode.networkUnavailable ||
    DiaryErrorCode.providerTimeout => '网络或服务暂不可用。原始记录不受影响；恢复连接后可重试。',
    DiaryErrorCode.rateLimited ||
    DiaryErrorCode.quotaExceeded => '服务配额或速率已达上限。请检查服务商额度，稍后重试。',
    DiaryErrorCode.revisionConflict => '这一天的正文刚刚变化了。已重新读取，请核对后再操作。',
    DiaryErrorCode.permissionDenied => '权限不足。请检查本机设置后重试。',
    _ => '操作没有完成。请刷新状态或稍后重试；原始材料不会被覆盖。',
  };

  @override
  void dispose() {
    _disposed = true;
    _poll?.cancel();
    super.dispose();
  }
}

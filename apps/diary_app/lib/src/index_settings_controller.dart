import 'dart:async';

import 'package:diary_api/diary_api.dart';
import 'package:flutter/foundation.dart';

import 'core_wakeup.dart';

/// The status and job are read from the core; queuing a rebuild is not success.
class IndexSettingsController extends ChangeNotifier {
  IndexSettingsController(this._api, {required Set<String> capabilities})
    : capabilities = Set.unmodifiable(capabilities);

  final DiaryApi _api;
  final Set<String> capabilities;
  IndexStatus? status;
  Job? job;
  String? _queuedJobId;
  bool loading = false;
  bool scheduling = false;
  String? message;
  Timer? _poller;
  bool _refreshing = false;
  bool _driving = false;
  bool _disposed = false;
  int _sequence = 0;

  bool get canReadStatus => capabilities.contains('indexes.status');
  bool get canRebuild => capabilities.contains('indexes.rebuildSemantic');
  bool get canReadJobs => capabilities.contains('jobs.get');
  bool get canListJobs => capabilities.contains('jobs.list');
  bool get canRunDue => capabilities.contains('jobs.runDue');
  bool get hasActiveJob =>
      (job != null && !job!.state.isTerminal) ||
      (canReadJobs && job == null && _queuedJobId != null);
  double? get progress => job?.progress?.fraction;

  Future<void> load() async {
    if (loading || _disposed) return;
    loading = true;
    message = null;
    _notify();
    try {
      if (canReadStatus) {
        status = await _api.indexesStatus();
      } else {
        message = '当前核心不能读取语义索引状态，请更新应用后再试。';
      }
      if (canReadJobs && _queuedJobId != null) {
        job = await _api.getJob(_queuedJobId!);
        if (job!.state.isTerminal) _queuedJobId = null;
      }
      if (canListJobs && canReadJobs && !hasActiveJob) {
        final page = await _api.listJobs(
          states: {
            JobState.queued,
            JobState.running,
            JobState.retryWait,
            JobState.waitingConfiguration,
            JobState.waitingDependency,
            JobState.waitingNetwork,
          },
          pageSize: 200,
        );
        for (final candidate in page.jobs) {
          if (candidate.kind == 'semantic_index') {
            job = candidate;
            _queuedJobId = candidate.id;
            break;
          }
        }
      }
      if (hasActiveJob) _schedulePolling();
    } on DiaryException catch (error) {
      message = error.message;
    } on Object {
      message = '暂时读不到索引状态，请稍后重试。';
    } finally {
      loading = false;
      _notify();
    }
  }

  Future<void> rebuild() async {
    if (!canRebuild || scheduling || hasActiveJob || _disposed) return;
    scheduling = true;
    message = '正在安排语义索引重建…';
    _notify();
    try {
      final ticket = await _api.rebuildSemanticIndex(
        reason: '用户在设置页请求重建语义索引',
        operationId:
            'semantic-rebuild-${DateTime.now().microsecondsSinceEpoch}-${_sequence++}',
      );
      // The durable job ID is the only proof that an operation was queued.
      job = null;
      _queuedJobId = ticket.jobId;
      message = '重建任务已加入本地队列，尚未完成。';
      _notify();
      if (canReadJobs) {
        try {
          job = await _api.getJob(ticket.jobId);
          if (job!.state.isTerminal) _queuedJobId = null;
          _schedulePolling();
          _notify();
        } on Object {
          message = '任务已加入队列，但暂时读不到进度；重新打开页面可以继续查看。';
          _notify();
        }
      }
      await CoreWakeupScheduler.afterCoreMutation(_api);
      if (canRunDue) unawaited(_driveDueJobs());
    } on DiaryException catch (error) {
      message = error.message;
    } on Object {
      message = '没有成功安排重建任务，请检查核心状态后重试。';
    } finally {
      scheduling = false;
      _notify();
    }
  }

  Future<void> refresh() async {
    if (_refreshing || _disposed) return;
    _refreshing = true;
    try {
      if (canReadJobs && (job != null || _queuedJobId != null)) {
        job = await _api.getJob(job?.id ?? _queuedJobId!);
        if (job!.state.isTerminal) _queuedJobId = null;
      }
      if (canReadStatus) status = await _api.indexesStatus();
      if (job?.state.isTerminal ?? false) _poller?.cancel();
      final state = job?.state;
      if (state != null) {
        message = switch (state) {
          JobState.succeeded =>
            status?.semanticIndexReady == true
                ? '语义索引已重建，可以使用语义检索。'
                : '重建任务已结束，但索引仍未就绪；请查看下方原因。',
          JobState.failed => '重建未完成，请查看索引状态并重试。',
          JobState.cancelled => '重建任务已取消。',
          JobState.waitingConfiguration => '正在等待本地模型配置；配置完成后任务会继续。',
          JobState.waitingDependency => '正在等待材料处理完成。',
          JobState.waitingNetwork => '任务正在等待网络恢复。',
          JobState.retryWait => '暂时停下，稍后会自动重试。',
          JobState.running => '正在重建语义索引…',
          JobState.queued => '重建任务正在本地队列中等待。',
        };
      }
      _notify();
    } on DiaryException catch (error) {
      message = error.message;
      _notify();
    } on Object {
      message = '暂时读不到重建进度，任务仍保留在本地队列中。';
      _notify();
    } finally {
      _refreshing = false;
    }
  }

  Future<void> _driveDueJobs() async {
    if (_driving || _disposed || !canRunDue || job?.state != JobState.queued) {
      return;
    }
    _driving = true;
    try {
      await _api.runDueJobs(budgetMs: 500, trigger: 'foreground');
      await CoreWakeupScheduler.afterCoreMutation(_api);
      await refresh();
    } on Object {
      // The durable task remains queued and the platform wakeup can retry it.
    } finally {
      _driving = false;
    }
  }

  void _schedulePolling() {
    _poller?.cancel();
    if (!hasActiveJob || !canReadJobs) return;
    _poller = Timer.periodic(const Duration(seconds: 2), (_) {
      unawaited(refresh().then((_) => _driveDueJobs()));
    });
  }

  void _notify() {
    if (!_disposed) notifyListeners();
  }

  @override
  void dispose() {
    _disposed = true;
    _poller?.cancel();
    super.dispose();
  }
}

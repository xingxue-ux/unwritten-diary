import 'dart:async';

import 'package:diary_api/diary_api.dart';
import 'package:flutter/foundation.dart';

enum LibraryCarePhase { idle, working, queued, succeeded, failed, cancelled }

/// Coordinates backup, restore, storage and trash operations without claiming
/// that an unsupported core action succeeded.
class LibraryCareController extends ChangeNotifier {
  LibraryCareController(
    this._api, {
    required Set<String> capabilities,
    required this.captureCount,
  }) : capabilities = Set.unmodifiable(capabilities);

  final DiaryApi _api;
  final Set<String> capabilities;
  final int captureCount;

  LibraryCarePhase phase = LibraryCarePhase.idle;
  String? message;
  String? activeJobId;
  Job? activeJob;
  BackupInspection? inspection;
  List<Capture> trashedCaptures = const [];
  bool loadingTrash = false;
  Timer? _poller;
  bool _refreshingJob = false;
  int _operationSequence = 0;

  bool get canCreateBackup => capabilities.contains('backups.create');
  bool get canInspectBackup => capabilities.contains('backups.inspect');
  bool get canRestoreBackup => capabilities.contains('backups.restore');
  bool get canReadJobs => capabilities.contains('jobs.get');
  bool get canCancelJobs => capabilities.contains('jobs.cancel');
  bool get canListCaptures => capabilities.contains('captures.list');
  bool get canTrash => capabilities.contains('captures.trash');
  bool get canRestoreTrash => capabilities.contains('captures.restore');
  bool get canPreviewPurge => capabilities.contains('captures.previewPurge');
  bool get canPurge => capabilities.contains('captures.purge');

  double? get progress => activeJob?.progress?.fraction;

  String _operationId(String prefix) {
    _operationSequence += 1;
    return '$prefix-${DateTime.now().microsecondsSinceEpoch}-$_operationSequence';
  }

  Future<void> createBackup(String targetTicket) async {
    if (!canCreateBackup) {
      _unavailable('当前核心还没有开放完整备份创建能力。');
      return;
    }
    await _runQueuedOperation(
      '正在创建完整备份…',
      () async => (await _api.createBackup(
        BackupRequest(targetTicket: targetTicket),
      )).jobId,
    );
  }

  Future<void> inspectBackup(String packageTicket) async {
    if (!canInspectBackup) {
      _unavailable('当前核心还不能检查备份包。');
      return;
    }
    _poller?.cancel();
    inspection = null;
    phase = LibraryCarePhase.working;
    message = '正在检查备份格式、版本和完整性…';
    notifyListeners();
    try {
      inspection = await _api.inspectBackup(packageTicket);
      phase = inspection!.issues.isEmpty
          ? LibraryCarePhase.succeeded
          : LibraryCarePhase.failed;
      message = inspection!.issues.isEmpty
          ? '检查完成，可以安全恢复到新的资料库。'
          : '这个备份存在问题，请先查看检查结果。';
    } on DiaryException catch (error) {
      phase = LibraryCarePhase.failed;
      message = error.message;
    } on Object {
      phase = LibraryCarePhase.failed;
      message = '备份检查没有完成，请确认文件仍可访问后重试。';
    }
    notifyListeners();
  }

  Future<void> restoreInspectedBackup() async {
    final inspected = inspection;
    if (inspected == null || inspected.issues.isNotEmpty) {
      _unavailable('请先选择并通过检查的备份。');
      return;
    }
    if (!canRestoreBackup) {
      _unavailable('当前核心还没有开放备份恢复能力。');
      return;
    }
    await _runQueuedOperation(
      '正在准备新的资料库…',
      () async => (await _api.restoreBackup(
        packageTicket: inspected.packageTicket,
        options: const RestoreOptions(restoreAsNewLibrary: true),
      )).jobId,
    );
  }

  Future<void> _runQueuedOperation(
    String workingMessage,
    Future<String> Function() enqueue,
  ) async {
    _poller?.cancel();
    phase = LibraryCarePhase.working;
    message = workingMessage;
    activeJob = null;
    activeJobId = null;
    notifyListeners();
    try {
      activeJobId = await enqueue();
      phase = LibraryCarePhase.queued;
      message = canReadJobs ? '任务已加入本地队列，正在读取进度。' : '任务已加入本地队列；当前核心没有提供进度读取能力。';
      notifyListeners();
      if (canReadJobs) {
        await refreshActiveJob();
        _schedulePolling();
      }
    } on DiaryException catch (error) {
      phase = LibraryCarePhase.failed;
      message = error.message;
      notifyListeners();
    } on Object {
      phase = LibraryCarePhase.failed;
      message = '操作没有开始，资料与原文件都没有改变。';
      notifyListeners();
    }
  }

  void _schedulePolling() {
    _poller?.cancel();
    if (activeJob?.state.isTerminal ?? false) return;
    _poller = Timer.periodic(const Duration(seconds: 2), (_) {
      unawaited(refreshActiveJob());
    });
  }

  Future<void> refreshActiveJob() async {
    final id = activeJobId;
    if (id == null || !canReadJobs || _refreshingJob) return;
    _refreshingJob = true;
    try {
      final job = await _api.getJob(id);
      activeJob = job;
      if (job.state == JobState.succeeded) {
        phase = LibraryCarePhase.succeeded;
        message = '任务已完成。';
        _poller?.cancel();
      } else if (job.state == JobState.failed) {
        phase = LibraryCarePhase.failed;
        message = '任务未完成，原资料没有被静默覆盖。';
        _poller?.cancel();
      } else if (job.state == JobState.cancelled) {
        phase = LibraryCarePhase.cancelled;
        message = '任务已停止。';
        _poller?.cancel();
      } else if (job.requiresUserAction) {
        phase = LibraryCarePhase.queued;
        message = '任务需要处理后才能继续。';
      } else {
        phase = LibraryCarePhase.queued;
        message = switch (job.state) {
          JobState.running => '正在处理…',
          JobState.waitingConfiguration => '等待完成必要设置。',
          JobState.waitingDependency => '等待前置任务完成。',
          JobState.waitingNetwork => '等待网络恢复。',
          JobState.retryWait => '暂时停下，稍后会自动重试。',
          _ => '任务正在本地队列中等待。',
        };
      }
      notifyListeners();
    } on DiaryException catch (error) {
      message = error.message;
      notifyListeners();
    } on Object {
      message = '暂时读不到任务进度，任务仍保留在本地队列中。';
      notifyListeners();
    } finally {
      _refreshingJob = false;
    }
  }

  Future<void> cancelActiveJob() async {
    final id = activeJobId;
    if (id == null || !canCancelJobs) return;
    try {
      activeJob = await _api.cancelJob(
        id: id,
        operationId: _operationId('cancel-job'),
      );
      phase = LibraryCarePhase.cancelled;
      message = '任务已停止。';
      _poller?.cancel();
    } on DiaryException catch (error) {
      message = error.message;
    } on Object {
      message = '暂时无法停止任务，请稍后再试。';
    }
    notifyListeners();
  }

  Future<void> loadTrash() async {
    if (!canListCaptures || loadingTrash) return;
    loadingTrash = true;
    notifyListeners();
    try {
      final items = <Capture>[];
      String? cursor;
      do {
        final page = await _api.listCaptures(cursor: cursor, pageSize: 100);
        items.addAll(
          page.captures.where(
            (capture) => capture.state == CaptureState.trashed,
          ),
        );
        cursor = page.nextCursor;
      } while (cursor != null);
      trashedCaptures = List.unmodifiable(items);
      message = null;
    } on DiaryException catch (error) {
      message = error.message;
    } on Object {
      message = '回收站暂时没有加载完成，请稍后重试。';
    } finally {
      loadingTrash = false;
      notifyListeners();
    }
  }

  Future<void> restoreCapture(Capture capture) async {
    if (!canRestoreTrash) {
      _unavailable('当前核心还没有开放回收站恢复能力。');
      return;
    }
    try {
      await _api.restoreCapture(
        id: capture.id,
        expectedRevision: capture.revision,
        operationId: _operationId('restore-capture'),
      );
      message = '记录已恢复。';
      await loadTrash();
    } on DiaryException catch (error) {
      message = error.message;
      notifyListeners();
    } on Object {
      message = '恢复没有完成，记录仍在回收站。';
      notifyListeners();
    }
  }

  Future<bool> trashCapture(Capture capture) async {
    if (!canTrash) {
      _unavailable('当前核心还没有开放移入回收站的能力。');
      return false;
    }
    try {
      await _api.trashCapture(
        id: capture.id,
        expectedRevision: capture.revision,
        operationId: _operationId('trash-capture'),
      );
      message = '记录已移入回收站。';
      notifyListeners();
      return true;
    } on DiaryException catch (error) {
      message = error.message;
    } on Object {
      message = '记录没有移入回收站，请稍后重试。';
    }
    notifyListeners();
    return false;
  }

  Future<PurgePreview?> previewPurge(Capture capture) async {
    if (!canPreviewPurge) {
      _unavailable('当前核心还不能预览永久清理的影响。');
      return null;
    }
    try {
      return await _api.previewPurge(capture.id);
    } on DiaryException catch (error) {
      message = error.message;
    } on Object {
      message = '清理影响没有读取完成，记录没有被删除。';
    }
    notifyListeners();
    return null;
  }

  Future<bool> purge(PurgePreview preview) async {
    if (!canPurge) {
      _unavailable('当前核心还没有开放永久清理能力。');
      return false;
    }
    try {
      await _api.purgeCapture(
        id: preview.captureId,
        previewToken: preview.previewToken,
        operationId: _operationId('purge-capture'),
      );
      message = '记录及预览中列出的原件已永久清理。';
      await loadTrash();
      return true;
    } on DiaryException catch (error) {
      message = error.message;
    } on Object {
      message = '永久清理没有完成，请重新预览影响后再试。';
    }
    notifyListeners();
    return false;
  }

  void _unavailable(String value) {
    phase = LibraryCarePhase.failed;
    message = value;
    notifyListeners();
  }

  @override
  void dispose() {
    _poller?.cancel();
    super.dispose();
  }
}

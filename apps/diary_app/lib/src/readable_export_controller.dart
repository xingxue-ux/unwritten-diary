import 'dart:async';

import 'package:diary_api/diary_api.dart';
import 'package:flutter/foundation.dart';

class ReadableExportController extends ChangeNotifier {
  ReadableExportController(this._api, {required Set<String> capabilities})
    : capabilities = Set.unmodifiable(capabilities);

  final DiaryApi _api;
  final Set<String> capabilities;

  bool working = false;
  ExportTicket? ticket;
  Job? job;
  String? message;
  Timer? _poller;
  int _sequence = 0;

  bool get canCreate => capabilities.contains('exports.create');
  bool get canReadJob => capabilities.contains('jobs.get');
  bool get canCancel => capabilities.contains('jobs.cancel');
  double? get progress => job?.progress?.fraction;

  Future<void> create({
    required DateTime from,
    required DateTime to,
    required String format,
    required bool includeAssets,
  }) async {
    if (!canCreate || working) return;
    if (from.isAfter(to)) {
      message = '开始日期不能晚于结束日期。';
      notifyListeners();
      return;
    }
    _poller?.cancel();
    working = true;
    ticket = null;
    job = null;
    message = '正在准备阅读导出…';
    notifyListeners();
    try {
      ticket = await _api.createExport(
        ExportRequest(
          fromDayKey: _dayKey(from),
          toDayKey: _dayKey(to),
          format: format,
          includeAssets: includeAssets,
        ),
      );
      working = false;
      message = canReadJob ? '导出任务已加入本地队列。' : '导出任务已加入本地队列；当前核心没有提供进度读取能力。';
      notifyListeners();
      if (canReadJob) {
        await refresh();
        _schedulePolling();
      }
    } on DiaryException catch (error) {
      working = false;
      message = error.message;
      notifyListeners();
    } on Object {
      working = false;
      message = '阅读导出没有开始，资料库没有变化。';
      notifyListeners();
    }
  }

  Future<void> refresh() async {
    final id = ticket?.jobId;
    if (id == null || !canReadJob) return;
    try {
      job = await _api.getJob(id);
      working = !job!.state.isTerminal;
      message = switch (job!.state) {
        JobState.succeeded =>
          ticket?.packageTicket == null
              ? '导出已完成，等待平台提供保存包。'
              : '导出已完成，可以保存阅读文档。',
        JobState.failed => '导出未完成，请调整范围或格式后重试。',
        JobState.cancelled => '导出已停止。',
        JobState.running => '正在生成阅读文档…',
        JobState.waitingConfiguration => '导出在等待必要设置。',
        JobState.waitingDependency => '导出在等待前置任务完成。',
        JobState.waitingNetwork => '导出在等待网络恢复。',
        JobState.retryWait => '导出稍后会自动重试。',
        JobState.queued => '导出正在本地队列中等待。',
      };
      if (job!.state.isTerminal) _poller?.cancel();
      notifyListeners();
    } on DiaryException catch (error) {
      message = error.message;
      notifyListeners();
    } on Object {
      message = '暂时读不到导出进度，任务仍保留在本地队列中。';
      notifyListeners();
    }
  }

  void _schedulePolling() {
    _poller?.cancel();
    if (job?.state.isTerminal ?? false) return;
    _poller = Timer.periodic(const Duration(seconds: 2), (_) {
      unawaited(refresh());
    });
  }

  Future<void> cancel() async {
    final id = ticket?.jobId;
    if (id == null || !canCancel) return;
    try {
      job = await _api.cancelJob(
        id: id,
        operationId:
            'cancel-export-${DateTime.now().microsecondsSinceEpoch}-${_sequence++}',
      );
      working = false;
      message = '导出已停止。';
      _poller?.cancel();
    } on DiaryException catch (error) {
      message = error.message;
    } on Object {
      message = '暂时无法停止导出。';
    }
    notifyListeners();
  }

  @override
  void dispose() {
    _poller?.cancel();
    super.dispose();
  }
}

String _dayKey(DateTime value) =>
    '${value.year.toString().padLeft(4, '0')}-'
    '${value.month.toString().padLeft(2, '0')}-'
    '${value.day.toString().padLeft(2, '0')}';

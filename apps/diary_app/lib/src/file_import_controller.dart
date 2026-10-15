import 'dart:async';

import 'package:diary_api/diary_api.dart';
import 'package:flutter/foundation.dart';
import 'package:platform_host/platform_host.dart';

import 'core_wakeup.dart';

class SelectedImportFile {
  const SelectedImportFile({
    required this.name,
    required this.openRead,
    this.mimeHint,
    this.size,
  });
  final String name;
  final Stream<List<int>> Function() openRead;
  final String? mimeHint;
  final int? size;
}

enum FileImportPhase { queued, copying, verifying, saved, interrupted, failed }

class FileImportEntry {
  FileImportEntry(this.file, this.operationId);
  final SelectedImportFile file;
  final String operationId;
  final CopyCancellation cancellation = CopyCancellation();
  FileImportPhase phase = FileImportPhase.queued;
  int copiedBytes = 0;
  String? importId;
  String? captureId;
  String? message;
  bool get active =>
      phase == FileImportPhase.queued ||
      phase == FileImportPhase.copying ||
      phase == FileImportPhase.verifying;
}

/// Import actions run independently of the editor draft. Every selected file
/// becomes a separate capture, so typing and auto-save never race import revisions.
/// Disposing listeners does not cancel a running copy or its core handoff.
class FileImportController extends ChangeNotifier {
  FileImportController(this.api);
  final DiaryApi api;
  final List<FileImportEntry> entries = [];
  bool _disposed = false;
  int _sequence = 0;
  Future<void> _tail = Future.value();

  void _notify() {
    if (!_disposed) notifyListeners();
  }

  Future<void> add(List<SelectedImportFile> files) {
    final added = files
        .map(
          (file) => FileImportEntry(
            file,
            'file-${DateTime.now().microsecondsSinceEpoch}-${++_sequence}',
          ),
        )
        .toList();
    entries.addAll(added);
    _notify();
    final task = _tail.then((_) async {
      for (final entry in added) {
        await _run(entry);
      }
    });
    _tail = task;
    return task;
  }

  void cancel(FileImportEntry entry) {
    if (entry.phase == FileImportPhase.queued ||
        entry.phase == FileImportPhase.copying) {
      entry.cancellation.cancel();
    }
  }

  Future<void> _run(FileImportEntry entry) async {
    try {
      entry.cancellation.check();
      entry.phase = FileImportPhase.copying;
      _notify();
      final draft = await api.createDraft(
        operationId: '${entry.operationId}-draft',
      );
      entry.captureId = draft.id;
      final progressClock = Stopwatch()..start();
      var lastNotificationMs = -100;
      final result = await FileImportCoordinator(api).importFile(
        captureId: draft.id,
        displayName: entry.file.name,
        operationId: entry.operationId,
        openRead: entry.file.openRead,
        cancellation: entry.cancellation,
        mimeHint: entry.file.mimeHint,
        sizeHint: entry.file.size,
        onProgress: (bytes) {
          entry.copiedBytes = bytes;
          if (progressClock.elapsedMilliseconds - lastNotificationMs >= 100) {
            lastNotificationMs = progressClock.elapsedMilliseconds;
            _notify();
          }
        },
      );
      entry.importId = result.importId;
      unawaited(CoreWakeupScheduler.afterCoreMutation(api));
      if (result.state != ImportState.ready) {
        entry.phase = result.state == ImportState.verifying
            ? FileImportPhase.verifying
            : FileImportPhase.failed;
        entry.message = result.state == ImportState.verifying
            ? '文件正在校验，尚未确认入库。'
            : '文件尚未入库，原件仍保留。';
        _notify();
        return;
      }
      await _commit(entry);
    } catch (failure) {
      if (failure is ImportInterrupted) entry.importId = failure.importId;
      final cause = failure is ImportInterrupted ? failure.cause : failure;
      entry.phase = cause is CopyCancelled
          ? FileImportPhase.interrupted
          : FileImportPhase.failed;
      entry.message = switch (cause) {
        CopyCancelled() => '已停止复制；暂存材料保留，尚未确认清理。',
        CopyTooLarge() => '文件超过导入上限，未完成入库。',
        DiaryException() => cause.message,
        _ => '导入未完成，原文件未改动，请稍后重试。',
      };
    } finally {
      _notify();
    }
  }

  Future<void> _commit(FileImportEntry entry) async {
    final current = await api.getCapture(entry.captureId!);
    await api.commit(
      id: current.id,
      expectedRevision: current.revision,
      operationId: '${entry.operationId}-commit',
    );
    entry.phase = FileImportPhase.saved;
    entry.message = '已保存为一条独立记录。';
  }

  Future<void> refresh(FileImportEntry entry) async {
    if (entry.importId == null || entry.phase != FileImportPhase.verifying) {
      return;
    }
    try {
      final status = await api.importStatus(entry.importId!);
      if (status.state == ImportState.ready) {
        await _commit(entry);
      } else if (status.state == ImportState.error ||
          status.state == ImportState.recoverable) {
        entry.phase = FileImportPhase.failed;
        entry.message = '校验未完成，材料保留在草稿中。';
      }
    } catch (_) {
      entry.message = '暂时无法确认状态，请稍后再次查看。';
    }
    _notify();
  }

  @override
  void dispose() {
    _disposed = true;
    super.dispose();
  }
}

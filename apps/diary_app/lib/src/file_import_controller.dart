import 'dart:async';

import 'package:diary_api/diary_api.dart';
import 'package:flutter/foundation.dart';
import 'package:platform_host/platform_host.dart';

import 'core_wakeup.dart';
import 'file_import_journal.dart';

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
  SelectedImportFile file;
  final String operationId;
  CopyCancellation cancellation = CopyCancellation();
  bool _busy = false;
  bool _retryAllowed = true;
  bool _sourceAvailable = true;
  bool _terminal = false;
  int? _commitRevision;
  FileImportPhase phase = FileImportPhase.queued;
  int copiedBytes = 0;
  String? importId;
  String? captureId;
  String? message;
  bool get canRetry =>
      !_busy &&
      _retryAllowed &&
      _sourceAvailable &&
      (phase == FileImportPhase.failed || phase == FileImportPhase.interrupted);
  bool get needsReselection =>
      !_sourceAvailable &&
      _retryAllowed &&
      (phase == FileImportPhase.failed || phase == FileImportPhase.interrupted);
  bool get canRefresh =>
      !_busy && phase == FileImportPhase.verifying && importId != null;
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
  Future<void> _journalTail = Future.value();
  FileImportJournal? _journal;

  void attachJournal(FileImportJournal journal) => _journal = journal;

  Future<void> _persist(FileImportEntry entry) {
    final journal = _journal;
    if (journal == null) return Future.value();
    final checkpoint = <String, Object?>{
      'operationId': entry.operationId,
      'captureId': entry.captureId,
      'importId': entry.importId,
      'name': entry.file.name,
      'phase': entry.phase.name,
      'commitRevision': entry._commitRevision,
      'terminal': entry._terminal,
    };
    final next = _journalTail
        .catchError((Object _) {})
        .then((_) => journal.append(checkpoint));
    _journalTail = next;
    return next;
  }

  /// Restore identities before any new picker operation starts. A picker stream
  /// is not durable, so interrupted copies require the user to reselect a file.
  Future<void> restore() async {
    final checkpoints = await _journal?.load();
    if (checkpoints == null) return;
    for (final value in checkpoints.values) {
      if (value['terminal'] == true || value['name'] is! String) continue;
      final operationId = value['operationId'] as String;
      final entry =
          FileImportEntry(
              SelectedImportFile(
                name: value['name'] as String,
                openRead: () =>
                    Stream.error(StateError('Reselect the source file first.')),
              ),
              operationId,
            )
            ..captureId = value['captureId'] as String?
            ..importId = value['importId'] as String?
            .._commitRevision = value['commitRevision'] as int?
            .._sourceAvailable = false
            ..phase = FileImportPhase.failed
            ..message = '上次导入未完成，请重新选择同一文件。';
      entries.add(entry);
      if (entry.importId == null) continue;
      try {
        final status = await api.importStatus(entry.importId!);
        if (status.state == ImportState.ready && entry.captureId != null) {
          // A ready asset needs no picker handle for a commit retry.
          entry._sourceAvailable = true;
          try {
            await _commit(entry);
          } catch (_) {
            entry.phase = FileImportPhase.failed;
            entry.message = '原件已入库，但记录提交未确认；可直接重试保存。';
          }
        } else if (status.state == ImportState.verifying) {
          entry.phase = FileImportPhase.verifying;
          entry.message = '资料库仍在校验，点击刷新查看结果。';
        } else if (status.state == ImportState.error) {
          entry._retryAllowed = false;
          entry.message = '资料库校验失败；请使用“导入文件”开始新导入。';
        }
      } catch (_) {
        entry.message = '暂时无法确认上次导入，材料状态未改动。';
      }
      await _persist(entry);
    }
    _notify();
  }

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
        try {
          await _persist(entry);
        } catch (_) {
          entry.phase = FileImportPhase.failed;
          entry._retryAllowed = false;
          entry.message = '无法写入导入恢复记录；文件尚未开始复制。';
          _notify();
          continue;
        }
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

  Future<void> retry(FileImportEntry entry) {
    if (!entries.contains(entry) || !entry.canRetry) return Future.value();
    entry.cancellation = CopyCancellation();
    entry.phase = FileImportPhase.queued;
    entry.message = null;
    entry.copiedBytes = 0;
    _notify();
    final task = _tail.then((_) => _run(entry));
    _tail = task;
    return task;
  }

  Future<void> retryWithFile(FileImportEntry entry, SelectedImportFile file) {
    if (!entries.contains(entry) ||
        !entry.needsReselection ||
        file.name != entry.file.name) {
      return Future.value();
    }
    entry.file = file;
    entry._sourceAvailable = true;
    return retry(entry);
  }

  Future<void> _run(FileImportEntry entry) async {
    if (entry._busy) return;
    entry._busy = true;
    try {
      entry.cancellation.check();
      entry.phase = FileImportPhase.copying;
      _notify();
      entry.captureId ??= (await api.createDraft(
        operationId: '${entry.operationId}-draft',
      )).id;
      await _persist(entry);
      final progressClock = Stopwatch()..start();
      var lastNotificationMs = -100;
      final result = await FileImportCoordinator(api).importFile(
        captureId: entry.captureId!,
        displayName: entry.file.name,
        operationId: entry.operationId,
        openRead: entry.file.openRead,
        cancellation: entry.cancellation,
        mimeHint: entry.file.mimeHint,
        sizeHint: entry.file.size,
        onPrepared: (ticket) async {
          entry.importId = ticket.importId;
          await _persist(entry);
        },
        onCopyComplete: () {
          entry.phase = FileImportPhase.verifying;
          entry.message = '复制完成，等待资料库校验。';
          _notify();
        },
        onProgress: (bytes) {
          entry.copiedBytes = bytes;
          if (progressClock.elapsedMilliseconds - lastNotificationMs >= 100) {
            lastNotificationMs = progressClock.elapsedMilliseconds;
            _notify();
          }
        },
      );
      entry.importId = result.importId;
      await _persist(entry);
      unawaited(CoreWakeupScheduler.afterCoreMutation(api));
      if (result.state != ImportState.ready) {
        entry.phase = result.state == ImportState.verifying
            ? FileImportPhase.verifying
            : FileImportPhase.failed;
        entry.message = result.state == ImportState.verifying
            ? '文件正在校验，尚未确认入库。'
            : '文件尚未入库，原件仍保留。';
        entry._retryAllowed = result.state != ImportState.error;
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
        CopyCancelled() => '已停止复制；正在向资料库确认取消。',
        CopyTooLarge() => '文件超过导入上限，未完成入库。',
        DiaryException() => cause.message,
        _ => '导入未完成，原文件未改动，请稍后重试。',
      };
      if (cause is CopyCancelled && entry.importId != null) {
        try {
          await api.cancelImport(entry.importId!);
          entry._retryAllowed = false;
          entry._terminal = true;
          entry.message = '已取消导入；资料库会话已移除。可重新选择文件。';
        } catch (_) {
          entry.message = '复制已停止，但资料库取消尚未确认；请稍后重试。';
        }
      }
    } finally {
      entry._busy = false;
      try {
        await _persist(entry);
      } catch (_) {
        entry.message = '恢复日志暂时无法写入；资料库状态请重新打开后核对。';
      }
      _notify();
    }
  }

  Future<void> _commit(FileImportEntry entry) async {
    final current = await api.getCapture(entry.captureId!);
    if (current.state != CaptureState.committed) {
      entry._commitRevision ??= current.revision;
      await _persist(entry);
      await api.commit(
        id: current.id,
        expectedRevision: entry._commitRevision!,
        operationId: '${entry.operationId}-commit',
      );
    }
    entry.phase = FileImportPhase.saved;
    entry._terminal = true;
    entry.message = '已保存为一条独立记录。';
    await _persist(entry);
  }

  Future<void> refresh(FileImportEntry entry) async {
    if (entry._busy ||
        entry.importId == null ||
        entry.phase != FileImportPhase.verifying) {
      return;
    }
    entry._busy = true;
    try {
      final status = await api.importStatus(entry.importId!);
      if (status.state == ImportState.ready) {
        await _commit(entry);
      } else if (status.state == ImportState.error ||
          status.state == ImportState.recoverable) {
        entry.phase = FileImportPhase.failed;
        entry._retryAllowed = status.state != ImportState.error;
        entry.message = '校验未完成，材料保留在草稿中。';
      }
    } catch (_) {
      entry.message = '暂时无法确认状态，请稍后再次查看。';
    } finally {
      entry._busy = false;
    }
    try {
      await _persist(entry);
    } catch (_) {
      entry.message = '恢复日志暂时无法写入；资料库状态请重新打开后核对。';
    }
    _notify();
  }

  @override
  void dispose() {
    _disposed = true;
    super.dispose();
  }
}

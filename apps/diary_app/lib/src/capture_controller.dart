import 'dart:async';

import 'package:diary_api/diary_api.dart';
import 'package:flutter/foundation.dart';

enum SavePhase { ready, editing, saving, saved, failed }

/// Text capture state. A pending write keeps its operationId until the core
/// confirms it, including when a timeout happens after the write succeeded.
class CaptureController extends ChangeNotifier {
  CaptureController(
    this.api, {
    this.autoSaveDelay = const Duration(milliseconds: 450),
  });

  final DiaryApi api;
  final Duration autoSaveDelay;
  Timer? _debounce;
  Capture? _draft;
  Future<bool>? _saving;
  Future<bool>? _finishing;
  _PendingSave? _pendingSave;
  String? _createOperationId;
  String? _commitOperationId;
  int _operation = 0;
  bool _disposed = false;

  bool loading = true;
  String text = '';
  DateTime? occurredAt;
  String _savedText = '';
  String? error;
  SavePhase phase = SavePhase.ready;
  List<Capture> recent = const [];

  bool get canChangeDate =>
      _draft == null && _createOperationId == null && _finishing == null;
  bool get finishing => _finishing != null;

  void _notify() {
    if (!_disposed) notifyListeners();
  }

  void chooseDate(DateTime date) {
    if (!canChangeDate || _disposed) return;
    occurredAt = date;
    _notify();
  }

  String _newOperation() =>
      'f0-${++_operation}-${DateTime.now().microsecondsSinceEpoch}';

  Future<void> initialize({bool opened = false}) async {
    try {
      if (!opened) await api.open();
      await refresh();
      if (_disposed) return;
      for (final capture in recent) {
        if (capture.state == CaptureState.draft) {
          _draft = capture;
          _savedText = capture.draftText;
          if (text.isEmpty) {
            text = capture.draftText;
            phase = SavePhase.saved;
          } else {
            phase = SavePhase.editing;
            _debounce?.cancel();
            _debounce = Timer(autoSaveDelay, saveNow);
          }
          break;
        }
      }
    } catch (failure) {
      if (_disposed) return;
      error = _message(failure);
      phase = SavePhase.failed;
    } finally {
      if (!_disposed) {
        loading = false;
        _notify();
      }
    }
  }

  Future<void> refresh() async {
    final page = await api.listCaptures(pageSize: 20);
    if (_disposed) return;
    recent = page.captures;
    _notify();
  }

  void updateText(String value) {
    if (_disposed || finishing) return;
    text = value;
    error = null;
    phase = value == _savedText && _pendingSave == null
        ? SavePhase.saved
        : SavePhase.editing;
    _debounce?.cancel();
    if ((value.trim().isNotEmpty || _draft != null) &&
        (value != _savedText || _pendingSave != null)) {
      _debounce = Timer(autoSaveDelay, saveNow);
    }
    _notify();
  }

  Future<bool> saveNow() async {
    _debounce?.cancel();
    if (_disposed || (text.trim().isEmpty && _draft == null)) return false;
    while (!_disposed) {
      final current = _saving;
      if (current != null) {
        if (!await current) return false;
      } else {
        if (_pendingSave == null && text == _savedText) return true;
        final task = _saveOnce();
        late final Future<bool> shared;
        shared = task.then((success) {
          if (identical(_saving, shared)) _saving = null;
          if (success && !_disposed) {
            phase = _pendingSave == null && text == _savedText
                ? SavePhase.saved
                : SavePhase.editing;
          }
          _notify();
          return success;
        });
        _saving = shared;
        _notify();
        if (!await shared) return false;
      }
      if (_pendingSave == null && text == _savedText) return true;
    }
    return false;
  }

  Future<bool> _saveOnce() async {
    phase = SavePhase.saving;
    error = null;
    _notify();
    try {
      if (_draft == null) {
        _createOperationId ??= _newOperation();
        final draft = await api.createDraft(
          occurredAt: occurredAt?.toUtc(),
          operationId: _createOperationId!,
        );
        if (_disposed) return false;
        _draft = draft;
        _createOperationId = null;
      }
      _pendingSave ??= _PendingSave(
        id: _draft!.id,
        text: text,
        expectedRevision: _draft!.revision,
        operationId: _newOperation(),
      );
      final pending = _pendingSave!;
      final result = await api.saveDraft(
        id: pending.id,
        text: pending.text,
        expectedRevision: pending.expectedRevision,
        operationId: pending.operationId,
      );
      if (_disposed) return false;
      if (!result.durable) {
        throw const DiaryException(
          code: DiaryErrorCode.unknown,
          message: '尚未收到保存确认。',
          retryable: true,
        );
      }
      _draft = _draft!.copyWith(
        revision: result.revision,
        draftText: pending.text,
        updatedAt: result.savedAt,
      );
      _savedText = pending.text;
      _pendingSave = null;
      recent = [
        _draft!,
        ...recent.where((capture) => capture.id != _draft!.id),
      ];
      return true;
    } catch (failure) {
      if (_disposed) return false;
      phase = SavePhase.failed;
      error = _message(failure);
      _notify();
      return false;
    }
  }

  Future<bool> finish() {
    if (_disposed) return Future.value(false);
    if (_finishing != null) return _finishing!;
    late final Future<bool> shared;
    shared = _finishOnce().then((success) {
      if (identical(_finishing, shared)) _finishing = null;
      _notify();
      return success;
    });
    _finishing = shared;
    _notify();
    return shared;
  }

  Future<bool> _finishOnce() async {
    _debounce?.cancel();
    if (text.trim().isEmpty) return false;
    if (!await saveNow()) {
      if (!_disposed) {
        error ??= '记录还没有保存成功，请重试。';
        phase = SavePhase.failed;
        _notify();
      }
      return false;
    }
    if (_disposed) return false;
    try {
      _commitOperationId ??= _newOperation();
      final done = await api.commit(
        id: _draft!.id,
        expectedRevision: _draft!.revision,
        operationId: _commitOperationId!,
      );
      if (_disposed) return false;
      _draft = null;
      _pendingSave = null;
      _createOperationId = null;
      _commitOperationId = null;
      occurredAt = null;
      text = '';
      _savedText = '';
      phase = SavePhase.ready;
      error = null;
      recent = [
        done.capture,
        ...recent.where((capture) => capture.id != done.capture.id),
      ];
      _notify();
      return true;
    } catch (failure) {
      if (_disposed) return false;
      phase = SavePhase.failed;
      error = _message(failure);
      _notify();
      return false;
    }
  }

  String _message(Object failure) =>
      failure is DiaryException ? failure.message : '这次操作没有完成，请稍后重试。';

  @override
  void dispose() {
    _disposed = true;
    _debounce?.cancel();
    super.dispose();
  }
}

class _PendingSave {
  const _PendingSave({
    required this.id,
    required this.text,
    required this.expectedRevision,
    required this.operationId,
  });

  final String id;
  final String text;
  final int expectedRevision;
  final String operationId;
}

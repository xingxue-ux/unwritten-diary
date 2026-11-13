import 'dart:async';

import 'package:diary_api/diary_api.dart';
import 'package:flutter/foundation.dart';

import 'ui_strings.dart';

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
  DiaryErrorCode? _conflictCode;
  int _operation = 0;
  bool _disposed = false;

  bool loading = true;
  String text = '';
  DateTime? occurredAt;
  String _savedText = '';
  String? error;
  SavePhase phase = SavePhase.ready;
  List<Capture> recent = const [];
  String? _nextCursor;
  bool loadingMore = false;
  String? loadMoreError;

  bool get hasMore => _nextCursor != null;

  /// 时间线上能看到的记录：**不含回收站里的**。
  ///
  /// 核心的 `captures.list` 会把 trashed 也带回来（回收站页要用同一份数据），
  /// 但「片段」是普通时间线——把已移入回收站的记录画在这里，等于告诉用户
  /// 「删除没成功」，而它其实在回收站里好好的。
  List<Capture> get visible => recent
      .where((capture) => capture.state != CaptureState.trashed)
      .toList(growable: false);

  bool get canChangeDate =>
      _draft == null && _createOperationId == null && _finishing == null;
  bool get finishing => _finishing != null;

  /// 这一条记录上有没有材料（录音、导入的文件）。
  ///
  /// 「不写日记」允许**只录一段音就收好**，所以「有没有正文」不能当成能不能
  /// 收好的判据——那会把「只录音不写字」这条路堵死。
  bool get hasSources => _draft?.orderedSourceIds.isNotEmpty ?? false;

  /// 现在能不能「收好这一刻」：有正文，或者有材料。
  bool get canFinish => text.trim().isNotEmpty || hasSources;
  bool get needsManualRecovery => _conflictCode != null;

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
          // **连日期一起恢复**。以前只恢复正文，日期 UI 回退成「今天」，
          // 于是「昨天写了一半的草稿」在界面上看着是今天的，提交后却写进昨天：
          // 用户去今天的日记里找，找不到刚提交的材料。
          occurredAt = capture.occurredAt.toLocal();
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

  /// 重新读列表，并且**把当前草稿也读回来**。
  ///
  /// 只刷列表是不够的：录音最终化会推进记录的 revision（往记录上挂一个音频
  /// 来源）。手里那份草稿的 revision 就旧了，下一次 `saveDraft` 会被核心判成
  /// `revision_conflict`——用户看到的是「内容已被修改」，其实是他自己刚录的音。
  Future<void> refresh() async {
    final page = await api.listCaptures(pageSize: 20);
    if (_disposed) return;
    recent = page.captures;
    _nextCursor = page.nextCursor;
    loadMoreError = null;
    final draft = _draft;
    if (draft != null) {
      try {
        final fresh = await api.getCapture(draft.id);
        if (_disposed) return;
        _draft = fresh;
        _savedText = fresh.draftText;
      } on Object {
        // 读不回来就保持原样：下一次保存会如实报冲突，不会悄悄写坏。
      }
    }
    _notify();
  }

  Future<void> loadMore() async {
    final cursor = _nextCursor;
    if (_disposed || loadingMore || cursor == null) return;
    loadingMore = true;
    loadMoreError = null;
    _notify();
    try {
      final page = await api.listCaptures(cursor: cursor, pageSize: 20);
      if (_disposed) return;
      final seen = recent.map((capture) => capture.id).toSet();
      recent = [
        ...recent,
        ...page.captures.where((capture) => seen.add(capture.id)),
      ];
      _nextCursor = page.nextCursor;
    } catch (failure) {
      if (_disposed) return;
      loadMoreError = _message(failure);
    } finally {
      if (!_disposed) {
        loadingMore = false;
        _notify();
      }
    }
  }

  void updateText(String value) {
    if (_disposed || finishing) return;
    text = value;
    // A conflicting write must not silently retry with a stale revision or
    // operation ID. Keep the user's text available to copy.
    if (needsManualRecovery) {
      _notify();
      return;
    }
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
    if (_disposed ||
        needsManualRecovery ||
        (text.trim().isEmpty && _draft == null)) {
      return false;
    }
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
      _blockOnConflict(failure);
      phase = SavePhase.failed;
      error = _message(failure);
      _notify();
      return false;
    }
  }

  /// 录音要挂到「这一条」记录上，所以录音前先把它落成草稿。
  ///
  /// 复用 [_createOperationId]：录音和打字抢同一个草稿时，重试仍然只建一条。
  /// 失败返回 `null`，调用方据此告诉用户「现在还不能录」——不要在这里抛，
  /// 录音面板自己会显示原因。
  Future<String?> ensureDraftId() async {
    if (_disposed) return null;
    final existing = _draft;
    if (existing != null) return existing.id;
    _createOperationId ??= _newOperation();
    try {
      final draft = await api.createDraft(
        occurredAt: occurredAt?.toUtc(),
        operationId: _createOperationId!,
      );
      if (_disposed) return null;
      _draft = draft;
      _createOperationId = null;
      recent = [
        draft,
        ...recent.where((capture) => capture.id != draft.id),
      ];
      _notify();
      return draft.id;
    } on Object catch (failure) {
      if (_disposed) return null;
      _blockOnConflict(failure);
      error = _message(failure);
      _notify();
      return null;
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
    if (!canFinish || needsManualRecovery) return false;
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
      _blockOnConflict(failure);
      phase = SavePhase.failed;
      error = _message(failure);
      _notify();
      return false;
    }
  }

  void _blockOnConflict(Object failure) {
    if (failure is! DiaryException) return;
    if (failure.code == DiaryErrorCode.revisionConflict ||
        failure.code == DiaryErrorCode.idempotencyConflict ||
        failure.code == DiaryErrorCode.invalidState) {
      _conflictCode = failure.code;
    }
  }

  String _message(Object failure) {
    if (failure is! DiaryException) return '这次操作没有完成，请稍后重试。';
    return switch (failure.code) {
      DiaryErrorCode.revisionConflict =>
        '${uiStrings.t('error.revision_conflict')}。文字还在这里，请先复制，再重新打开应用。',
      DiaryErrorCode.idempotencyConflict => '重复操作发生冲突。文字还在这里，请先复制，再重新打开应用。',
      DiaryErrorCode.invalidState =>
        '${uiStrings.t('error.invalid_state')}。文字还在这里，请先复制，再重新打开应用。',
      _ => failure.message,
    };
  }

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

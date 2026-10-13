import 'package:diary_api/diary_api.dart';
import 'package:flutter/foundation.dart';

enum RevisionSaveResult { saved, unchanged, failed }

/// Keeps the durable revision and its parent together while a source is open.
class RevisionController extends ChangeNotifier {
  RevisionController(this.api);

  final DiaryApi api;
  String? sourceId;
  String? captureId;
  int? captureRevision;
  String? currentText;
  String? previousText;
  bool hasPrevious = false;
  bool previousIsAsset = false;
  bool previousLoading = false;
  String? previousError;
  bool loading = false;
  bool saving = false;
  String? error;
  String? indexWarning;
  int _request = 0;
  int _operation = 0;
  String? _pendingText;
  String? _pendingOperationId;
  SourceRevision? _previousRevision;

  bool get canEdit =>
      captureId != null &&
      captureRevision != null &&
      currentText != null &&
      !saving;
  Future<void> open({
    required String sourceId,
    String? captureId,
    int? captureRevision,
    String? initialText,
  }) async {
    final request = ++_request;
    if (this.sourceId != sourceId) {
      _pendingText = null;
      _pendingOperationId = null;
    }
    this.sourceId = sourceId;
    this.captureId = captureId;
    this.captureRevision = captureRevision;
    saving = false;
    currentText = null;
    previousText = null;
    hasPrevious = false;
    previousIsAsset = false;
    previousLoading = false;
    previousError = null;
    _previousRevision = null;
    error = null;
    indexWarning = null;
    loading = true;
    notifyListeners();
    try {
      final content = await api.extractedContent(sourceId);
      final previous = await api.previousSourceRevision(sourceId);
      if (request != _request) return;
      // A committed first revision may still be waiting for extraction. Its
      // capture text is safe to use only when there is no earlier revision.
      currentText = content?.text ?? (previous == null ? initialText : null);
      previousText = previous?.text;
      hasPrevious = previous != null;
      previousIsAsset = previous?.assetId != null && previous?.text == null;
      _previousRevision = previous;
      if (currentText == null) {
        indexWarning = '当前正文还未完成提取，请稍后重试。';
      }
    } catch (failure) {
      if (request != _request) return;
      error = failure.toString();
    } finally {
      if (request == _request) {
        loading = false;
        notifyListeners();
      }
    }
  }

  /// Imported originals keep their text in extracted content, not in the
  /// SourceRevision row. Load it only when the user asks to see the old text.
  Future<void> loadPreviousText() async {
    final previous = _previousRevision;
    if (previous == null ||
        previousText != null ||
        previousLoading ||
        previous.assetId == null) {
      return;
    }
    final request = _request;
    previousLoading = true;
    previousError = null;
    notifyListeners();
    try {
      final content = await api.extractSource(previous.revisionId);
      if (request != _request) return;
      previousText = content.text;
      if (previousText == null) {
        previousError = '修改前的原件没有可读取的文字。';
      }
    } catch (_) {
      if (request == _request) {
        previousError = '暂时无法读取修改前的原件，请稍后重试。';
      }
    } finally {
      if (request == _request) {
        previousLoading = false;
        notifyListeners();
      }
    }
  }

  Future<RevisionSaveResult> save(String text) async {
    final source = sourceId;
    final capture = captureId;
    final expectedRevision = captureRevision;
    final oldText = currentText;
    if (saving ||
        source == null ||
        capture == null ||
        expectedRevision == null ||
        oldText == null) {
      return RevisionSaveResult.failed;
    }
    if (text == oldText) return RevisionSaveResult.unchanged;
    final request = _request;
    saving = true;
    error = null;
    indexWarning = null;
    notifyListeners();
    try {
      if (_pendingText != text) {
        _pendingText = text;
        _pendingOperationId =
            'rev-${DateTime.now().microsecondsSinceEpoch}-${++_operation}';
      }
      final operationId = _pendingOperationId!;
      final revision = await api.reviseText(
        sourceId: source,
        text: text,
        expectedRevision: expectedRevision,
        operationId: operationId,
      );
      if (request != _request) return RevisionSaveResult.saved;
      _pendingText = null;
      _pendingOperationId = null;
      currentText = revision.text ?? text;
      previousText = oldText;
      hasPrevious = true;
      previousIsAsset = false;
      captureRevision = expectedRevision + 1;
      notifyListeners();
      // The core extracts the new revision during reviseText. A second extract
      // call is redundant and could misleadingly report a confirmed edit as
      // failed.
      return RevisionSaveResult.saved;
    } catch (failure) {
      if (request == _request) {
        error = '保存尚未确认，请重试。若内容已写入，重试会沿用同一次操作。';
      }
      return RevisionSaveResult.failed;
    } finally {
      if (request == _request) {
        saving = false;
        notifyListeners();
      }
    }
  }

  @override
  void dispose() {
    _request++;
    super.dispose();
  }
}

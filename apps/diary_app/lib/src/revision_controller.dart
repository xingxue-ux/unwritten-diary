import 'package:diary_api/diary_api.dart';
import 'package:flutter/foundation.dart';

/// Keeps the durable revision and its parent together while a source is open.
class RevisionController extends ChangeNotifier {
  RevisionController(this.api);

  final DiaryApi api;
  String? sourceId;
  String? captureId;
  int? captureRevision;
  String? currentText;
  String? previousText;
  bool loading = false;
  bool saving = false;
  String? error;
  String? indexWarning;
  int _request = 0;
  int _operation = 0;
  String? _pendingText;
  String? _pendingOperationId;

  bool get canEdit =>
      captureId != null &&
      captureRevision != null &&
      currentText != null &&
      !saving;
  bool get hasPrevious => previousText != null;

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
    currentText = null;
    previousText = null;
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

  Future<bool> save(String text) async {
    final source = sourceId;
    final capture = captureId;
    final expectedRevision = captureRevision;
    final oldText = currentText;
    if (saving ||
        source == null ||
        capture == null ||
        expectedRevision == null ||
        oldText == null) {
      return false;
    }
    if (text == oldText) return true;
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
      final revision = await api.reviseText(
        sourceId: source,
        text: text,
        expectedRevision: expectedRevision,
        operationId: _pendingOperationId!,
      );
      _pendingText = null;
      _pendingOperationId = null;
      currentText = revision.text ?? text;
      previousText = oldText;
      captureRevision = expectedRevision + 1;
      notifyListeners();
      try {
        await api.extractSource(source);
      } catch (_) {
        // The revision is durable even if indexing fails. Do not ask the user
        // to save again; that would create another revision.
        indexWarning = '新版已保存，搜索索引暂未更新。稍后可重新处理。';
      }
      return true;
    } catch (failure) {
      error = failure.toString();
      return false;
    } finally {
      saving = false;
      notifyListeners();
    }
  }

  @override
  void dispose() {
    _request++;
    super.dispose();
  }
}

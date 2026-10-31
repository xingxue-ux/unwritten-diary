import 'dart:async';

import 'package:diary_api/diary_api.dart' as api;
import 'package:diary_api/diary_api.dart';

/// 只覆盖 F0 的记录路径；其余契约方法在对应任务实现前会明确报错。
///
/// 数据只在这个对象存活期间存在，绝不用于验证真实持久化。
class MockDiaryApi implements DiaryApi, OrganizeSchedulePort {
  MockDiaryApi({
    DateTime Function()? clock,
    this.timeZone = 'Asia/Shanghai',
    this.utcOffsetMinutes = 480,
  }) : _clock = clock ?? DateTime.now;

  final DateTime Function() _clock;
  final String timeZone;
  final int utcOffsetMinutes;
  final Map<String, Capture> _captures = {};
  final Map<String, _Receipt> _receipts = {};
  int _nextId = 1;

  /// 用于预览保存失败状态；失败不占用 operationId，可原样重试。
  DiaryException? nextSaveFailure;

  DateTime get _now => _clock().toUtc();

  String _dayKey(DateTime time) {
    final local = time.toUtc().add(Duration(minutes: utcOffsetMinutes));
    return '${local.year.toString().padLeft(4, '0')}-'
        '${local.month.toString().padLeft(2, '0')}-'
        '${local.day.toString().padLeft(2, '0')}';
  }

  T _once<T>(String operationId, String fingerprint, T Function() action) {
    final previous = _receipts[operationId];
    if (previous != null) {
      if (previous.fingerprint != fingerprint) {
        throw const DiaryException(
          code: DiaryErrorCode.idempotencyConflict,
          message: '这次操作的内容与上次不同，请重新尝试。',
        );
      }
      return previous.value as T;
    }
    final result = action();
    _receipts[operationId] = _Receipt(fingerprint, result as Object);
    return result;
  }

  Capture _capture(String id) {
    final capture = _captures[id];
    if (capture == null) {
      throw const DiaryException(
        code: DiaryErrorCode.notFound,
        message: '这条记录不存在。',
      );
    }
    return capture;
  }

  void _expectDraft(Capture capture, int expectedRevision) {
    if (capture.state != CaptureState.draft) {
      throw const DiaryException(
        code: DiaryErrorCode.invalidState,
        message: '这条记录已经结束，不能再修改草稿。',
      );
    }
    if (capture.revision != expectedRevision) {
      throw const DiaryException(
        code: DiaryErrorCode.revisionConflict,
        message: '记录已经发生变化，输入仍留在页面上。',
      );
    }
  }

  @override
  Future<CoreSnapshot> open({String? libraryHandle}) async => snapshot();

  @override
  Future<CoreSnapshot> snapshot({Set<String>? scopes}) async => CoreSnapshot(
    coreInfo: const CoreInfo(
      apiVersion: kApiVersion,
      dataSchemaVersion: 0,
      buildVersion: 'f0-mock',
      libraryId: 'mock-session',
      capabilities: {
        'captures.createDraft',
        'captures.saveDraft',
        'captures.commit',
        'captures.get',
        'captures.list',
      },
    ),
    recovery: const RecoverySummary(),
    captureCount: _captures.length,
  );

  @override
  Future<void> close() async {}

  @override
  Future<void> setRuntimeState(RuntimeState state) async {}

  @override
  Stream<DomainEvent> events({int? fromSequence}) => const Stream.empty();

  @override
  Future<Capture> createDraft({
    DateTime? occurredAt,
    required String operationId,
  }) async =>
      _once(operationId, 'create:${occurredAt?.toUtc().toIso8601String()}', () {
        final now = _now;
        final time = occurredAt?.toUtc() ?? now;
        final capture = Capture(
          id: 'mock-${_nextId++}',
          revision: 1, // 与真核心一致：新建记录的 revision 从 1 开始（契约）
          state: CaptureState.draft,
          occurredAt: time,
          createdAt: now,
          updatedAt: now,
          timeZone: timeZone,
          utcOffsetMinutes: utcOffsetMinutes,
          dayKey: _dayKey(time),
        );
        _captures[capture.id] = capture;
        return capture;
      });

  @override
  Future<DraftSaveResult> saveDraft({
    required String id,
    required String text,
    required int expectedRevision,
    required String operationId,
  }) async {
    final fingerprint = 'save:$id:$expectedRevision:$text';
    return _once(operationId, fingerprint, () {
      final failure = nextSaveFailure;
      nextSaveFailure = null;
      if (failure != null) throw failure;
      final current = _capture(id);
      _expectDraft(current, expectedRevision);
      final now = _now;
      final updated = current.copyWith(
        revision: current.revision + 1,
        draftText: text,
        updatedAt: now,
      );
      _captures[id] = updated;
      // durable 是接口语义；这个 Mock 不写磁盘，UI 必须明确说明。
      return DraftSaveResult(
        revision: updated.revision,
        durable: true,
        savedAt: now,
      );
    });
  }

  @override
  Future<CommitResult> commit({
    required String id,
    required int expectedRevision,
    required String operationId,
  }) async => _once(operationId, 'commit:$id:$expectedRevision', () {
    final current = _capture(id);
    _expectDraft(current, expectedRevision);
    final committed = current.copyWith(
      state: CaptureState.committed,
      revision: current.revision + 1,
      updatedAt: _now,
    );
    _captures[id] = committed;
    return CommitResult(capture: committed);
  });

  @override
  Future<Capture> getCapture(String id) async => _capture(id);

  @override
  Future<CapturePage> listCaptures({
    String? dayKey,
    String? cursor,
    int pageSize = 20,
  }) async {
    final items =
        _captures.values
            .where((capture) => dayKey == null || capture.dayKey == dayKey)
            .toList()
          ..sort((a, b) {
            final byTime = b.occurredAt.compareTo(a.occurredAt);
            return byTime != 0 ? byTime : b.id.compareTo(a.id);
          });
    final start = cursor == null
        ? 0
        : int.tryParse(cursor.replaceFirst('mock:', '')) ?? 0;
    final end = (start + pageSize.clamp(1, 100)).clamp(0, items.length);
    return CapturePage(
      captures: items.sublist(start.clamp(0, items.length), end),
      nextCursor: end < items.length ? 'mock:$end' : null,
    );
  }

  /// Mock **没有**检索会话，也**没有**任何语义/融合能力（本类只覆盖 F0 记录路径，
  /// 连 `search.start` 都还没接）。所以这里明确拒绝，而不是照
  /// [DiaryApi.upgradeSearch] 的默认实现去读会话快照——`searchSnapshot` 在 Mock
  /// 里同样没有实现，没有会话可以交出来。
  ///
  /// 关键取舍：宁可报错，也**绝不**返回一份编造的 `hybrid_ready`，或者替一个不
  /// 存在的会话编一份 `keyword_ready` 快照。能力清单（`capabilities`）同样不列
  /// `search.upgrade`：没接的能力不声明。
  @override
  Future<SearchSnapshot> upgradeSearch({required String sessionId}) async {
    throw UnsupportedError(
      'MockDiaryApi 没有检索会话（search.start 尚未在 Mock 中实现），'
      'upgradeSearch 交不出真实快照，也不会伪造 hybrid_ready。',
    );
  }

  api.OrganizeSchedule _organizeSchedule = const api.OrganizeSchedule(
    enabled: false,
    templateId: 'merge_by_theme',
    autoOrganizeTime: '22:30',
    supplementOnNewMaterials: true,
    revision: 1,
  );

  @override
  Future<api.OrganizeSchedule> organizeSchedule() async => _organizeSchedule;

  @override
  Future<api.OrganizeSchedule> updateOrganizeSchedule({
    required api.OrganizeSchedule schedule,
    required int expectedRevision,
  }) async {
    if (expectedRevision != _organizeSchedule.revision) {
      throw const DiaryException(
        code: DiaryErrorCode.revisionConflict,
        message: '设置已被改动，请重新读取后再试。',
      );
    }
    _organizeSchedule = api.OrganizeSchedule(
      enabled: schedule.enabled,
      templateId: schedule.templateId,
      autoOrganizeTime: schedule.autoOrganizeTime,
      supplementOnNewMaterials: schedule.supplementOnNewMaterials,
      revision: _organizeSchedule.revision + 1,
    );
    return _organizeSchedule;
  }

  @override
  dynamic noSuchMethod(Invocation invocation) => throw UnsupportedError(
    '${invocation.memberName} 尚未在 F0 Mock 中实现，请按任务书扩展。',
  );
}

class _Receipt {
  const _Receipt(this.fingerprint, this.value);
  final String fingerprint;
  final Object value;
}

// 这个文件用前缀区分两个包：两边有一批**同名不同类型**的对象
// （Capture、CaptureState、DraftSaveResult…），不加前缀很容易把桥接的类型
// 递给契约的构造函数，而那种错误恰恰是编译器能拦住的。
import 'dart:async';

import 'package:diary_api/diary_api.dart' as api;
import 'package:flutter_rust_bridge/flutter_rust_bridge_for_generated.dart';

import '../diary_bridge.dart' as bridge;

/// 把桥接的 `BridgeSession` 适配成契约的 [api.DiaryApi]。
///
/// **覆盖记录与检索路径**：`open` / `snapshot` / `close` / 记录 CRUD /
/// `startSearch` / `nextPage` / `searchSnapshot` / `cancelSearch`。其余契约方法一律抛
/// [UnsupportedError]（和 `diary_mock` 的做法一致），并且**不出现在
/// [api.CoreInfo.capabilities] 里**——能力清单是窄化过的，界面据此决定显示哪些入口。
///
/// 设计取舍：
/// - **不另造 DTO**：桥接的字段名已经和契约对得上，这里只做映射与改名，
///   不做语义转换；
/// - **错误翻译**：桥接抛 `BridgeError`（字符串码），这里翻成
///   [api.DiaryException]（枚举码），未知码落到 `unknown` 而不是崩掉；
/// - **能力窄化**：核心报的是「桥接接了多少」，这里只报「适配层能真的用的」，
///   两者取交集。
class BridgeDiaryApi implements api.DiaryApi {
  BridgeDiaryApi({
    required this.coreLibraryPath,
    required this.libraryPath,
    required this.timeZone,
    required this.utcOffsetMinutes,
  });

  /// 桥接产物（Windows 的 `.dll`、Android 的 `.so`）路径，用来初始化 frb。
  final String coreLibraryPath;

  /// 资料库文件路径。契约里这是平台层的活（`libraryHandle` 就是不透明句柄），
  /// 平台层（F1）到位之前由调用方直接注入。
  final String libraryPath;

  /// 契约把时区与偏移放在平台层，而核心的 `createDraft` 需要它们。
  final String timeZone;
  final int utcOffsetMinutes;

  /// frb 的初始化在一个进程里只能做一次。
  static Future<void>? _rustLibReady;

  bridge.BridgeSession? _session;

  /// 适配层真正接上的契约方法。核心报的能力清单是它的上限，这里是实际能力。
  static const Set<String> _wiredMethods = {
    'core.open',
    'core.snapshot',
    'captures.createDraft',
    'captures.saveDraft',
    'captures.commit',
    'captures.get',
    'captures.list',
    'search.start',
    'search.nextPage',
    'search.snapshot',
    'search.cancel',
    'jobs.nextWakeup',
    'jobs.runDue',
    'imports.prepare',
    'imports.finish',
    'imports.status',
    'imports.cancel',
    'recordings.prepare',
    'recordings.registerSegment',
    'recordings.updateState',
    'recordings.finalize',
    'recordings.recover',
    'search.upgrade',
  };

  /// 幂等地初始化 frb 运行时。
  static Future<void> ensureRuntime(String coreLibraryPath) {
    return _rustLibReady ??= bridge.RustLib.init(
      externalLibrary: ExternalLibrary.open(coreLibraryPath),
    );
  }

  bridge.BridgeSession get _open {
    final session = _session;
    if (session == null) {
      throw const api.DiaryException(
        code: api.DiaryErrorCode.invalidState,
        message: '资料库还没有打开，请先调用 open()。',
      );
    }
    return session;
  }

  @override
  Future<api.CoreSnapshot> open({String? libraryHandle}) async {
    // 先断开旧会话并**立刻清空引用**：开库可能失败（路径不存在、没有权限、
    // 核心拒绝打开），那时若 `_session` 还指向已经释放的句柄，后面每个方法都会
    // 拿着一个死句柄去用，报出来的错也跟真正的原因无关。
    _session?.dispose();
    _session = null;

    // 加载桥接产物失败是**部署错误**（.so/.dll 不在或架构不对），不是资料库错误，
    // 所以故意让它原样抛：换成 DiaryException 会把它伪装成「资料库有问题」。
    await ensureRuntime(coreLibraryPath);

    // 开库失败要按契约抛 DiaryException，不能把 BridgeError 泄漏给只依赖
    // DiaryApi 的调用方——其他记录方法都翻过了，这里漏一个就会出现「同一个
    // 失败，取决于发生在哪一步，异常类型不同」。
    final session = await _translate(
      () =>
          bridge.BridgeSession.open(libraryPath: libraryHandle ?? libraryPath),
    );
    _session = session;
    return await _snapshot(session);
  }

  @override
  Future<api.CoreSnapshot> snapshot({Set<String>? scopes}) async =>
      _snapshot(_open);

  @override
  Future<void> close() async {
    _session?.dispose();
    _session = null;
  }

  @override
  Future<api.Capture> createDraft({
    DateTime? occurredAt,
    required String operationId,
  }) async {
    final session = _open;
    final capture = await _translate(
      () => session.createDraft(
        occurredAt: occurredAt,
        timeZone: timeZone,
        utcOffsetMinutes: utcOffsetMinutes,
        operationId: operationId,
      ),
    );
    return _capture(capture);
  }

  @override
  Future<api.DraftSaveResult> saveDraft({
    required String id,
    required String text,
    required int expectedRevision,
    required String operationId,
  }) async {
    final session = _open;
    final result = await _translate(
      () => session.saveDraft(
        captureId: id,
        text: text,
        expectedRevision: expectedRevision,
        operationId: operationId,
      ),
    );
    return api.DraftSaveResult(
      revision: result.revision.toInt(),
      durable: result.durable,
      savedAt: result.savedAt,
    );
  }

  @override
  Future<api.CommitResult> commit({
    required String id,
    required int expectedRevision,
    required String operationId,
  }) async {
    final session = _open;
    final result = await _translate(
      () => session.commit(
        captureId: id,
        expectedRevision: expectedRevision,
        operationId: operationId,
      ),
    );
    final revision = result.originalTextRevision;
    return api.CommitResult(
      capture: _capture(result.capture),
      originalTextRevision: revision == null ? null : _sourceRevision(revision),
    );
  }

  @override
  Future<api.Capture> getCapture(String id) async {
    final session = _open;
    final capture = await _translate(() => session.getCapture(captureId: id));
    return _capture(capture);
  }

  @override
  Future<api.CapturePage> listCaptures({
    String? dayKey,
    String? cursor,
    int pageSize = 20,
  }) async {
    final session = _open;
    final page = await _translate(
      () =>
          session.listCaptures(dayKey: dayKey, cursor: cursor, limit: pageSize),
    );
    return api.CapturePage(
      captures: page.captures.map(_capture).toList(growable: false),
      nextCursor: page.nextCursor,
    );
  }

  @override
  Future<api.SearchSnapshot> startSearch({
    required api.SearchRequest request,
    required int queryRevision,
  }) async {
    final result = await _translate(
      () => _open.startSearch(
        request: bridge.SearchRequest(
          query: request.query,
          mode: bridge.SearchMode.values.byName(request.mode.name),
          filters: bridge.SearchFilters(
            fromDayKey: request.filters.fromDayKey,
            toDayKey: request.filters.toDayKey,
            kinds: request.filters.kinds
                .map((kind) => bridge.SourceKind.values.byName(kind.name))
                .toList(growable: false),
            sourceScope: request.filters.sourceScope?.toList(growable: false),
            includeOldDiaryVersions: request.filters.includeOldDiaryVersions,
            includeTrashed: request.filters.includeTrashed,
          ),
          pageSize: request.pageSize,
        ),
        queryRevision: queryRevision,
      ),
    );
    return _searchSnapshot(result);
  }

  @override
  Future<api.SearchSnapshot> upgradeSearch({required String sessionId}) async =>
      _searchSnapshot(
        await _translate(() => _open.upgradeSearch(sessionId: sessionId)),
      );

  @override
  Future<api.SearchSnapshot> nextPage({
    required String sessionId,
    String? cursor,
  }) async => _searchSnapshot(
    await _translate(
      () => _open.searchNextPage(sessionId: sessionId, cursor: cursor),
    ),
  );

  @override
  Future<api.SearchSnapshot> searchSnapshot(String sessionId) async =>
      _searchSnapshot(
        await _translate(() => _open.searchSnapshot(sessionId: sessionId)),
      );

  @override
  Future<void> cancelSearch(String sessionId) async {
    await _translate(() => _open.cancelSearch(sessionId: sessionId));
  }

  @override
  Future<DateTime?> nextWakeup() => _translate(() => _open.nextWakeup());

  @override
  Future<api.ImportTicket> prepareImport({
    required String captureId,
    required String displayName,
    String? mimeHint,
    int? sizeHint,
    required String operationId,
  }) async {
    final ticket = await _translate(
      () => _open.prepareImport(
        captureId: captureId,
        displayName: displayName,
        mimeHint: mimeHint,
        sizeHint: sizeHint,
        origin: bridge.ImportOrigin.picker,
        operationId: operationId,
      ),
    );
    return api.ImportTicket(
      importId: ticket.importId,
      stagingTicket: ticket.stagingTicket,
      maxBytes: ticket.maxBytes,
    );
  }

  @override
  Future<api.ImportStatus> finishImport({
    required String importId,
    required String ticket,
    required api.ImportManifest manifest,
  }) async => _importStatus(
    await _translate(
      () => _open.finishImport(
        importId: importId,
        stagingTicket: ticket,
        manifest: bridge.ImportManifest(
          copiedBytes: manifest.copiedBytes,
          sha256: manifest.sha256,
          detectedMime: manifest.detectedMime,
          originalName: manifest.originalName,
        ),
      ),
    ),
  );

  @override
  Future<api.ImportStatus> importStatus(String importId) async => _importStatus(
    await _translate(() => _open.importStatus(importId: importId)),
  );

  @override
  Future<void> cancelImport(String importId) =>
      _translate(() => _open.cancelImport(importId: importId));

  static api.ImportStatus _importStatus(bridge.ImportStatus status) =>
      api.ImportStatus(
        importId: status.importId,
        state: api.ImportState.values.byName(status.state.name),
        copiedBytes: status.copiedBytes,
        totalBytes: status.totalBytes,
        assetId: status.assetId,
        errorCode: status.errorCode == null
            ? null
            : _errorCode(status.errorCode!),
        message: status.message,
      );

  @override
  Future<api.RecordingTicket> prepareRecording({
    required String captureId,
    String? formatPreference,
    required String operationId,
  }) async {
    final ticket = await _translate(
      () => _open.prepareRecording(
        captureId: captureId,
        operationId: operationId,
      ),
    );
    return api.RecordingTicket(
      recordingId: ticket.recordingId,
      assetId: ticket.assetId,
      stagingTicket: ticket.stagingTicket,
    );
  }

  @override
  Future<api.SegmentReceipt> registerSegment({
    required String recordingId,
    required int segmentIndex,
    required api.SegmentManifest segment,
  }) async {
    final receipt = await _translate(
      () => _open.registerSegment(
        recordingId: recordingId,
        segmentIndex: segmentIndex,
        segment: bridge.SegmentManifest(
          segmentId: segment.segmentId,
          relativeFileName: segment.relativeFileName,
          durationMs: segment.durationMs,
          sha256: segment.sha256,
          byteSize: segment.byteSize,
          closedAt: segment.closedAt,
        ),
      ),
    );
    return api.SegmentReceipt(
      segmentId: receipt.segmentId,
      segmentIndex: receipt.segmentIndex,
      durable: receipt.durable,
      recordedAt: receipt.recordedAt,
    );
  }

  @override
  Future<api.RecordingSession> updateRecordingState(
    api.NativeRecordingStatus status,
  ) async {
    final session = await _translate(
      () => _open.updateRecordingState(
        status: bridge.NativeRecordingStatus(
          recordingId: status.recordingId,
          state: bridge.RecordingState.values.byName(status.state.name),
          wallClock: status.wallClock,
          elapsedMs: status.elapsedMs,
          persistedThroughMs: status.persistedThroughMs,
          inputDeviceChanged: status.inputDeviceChanged,
          message: status.message,
        ),
      ),
    );
    return _recordingSession(session);
  }

  static api.RecordingSession _recordingSession(
    bridge.RecordingSession session,
  ) => api.RecordingSession(
    recordingId: session.recordingId,
    assetId: session.assetId,
    state: api.RecordingState.values.byName(session.state.name),
    elapsedMs: session.elapsedMs,
    durableThroughMs: session.durableThroughMs,
    segmentCount: session.segmentCount,
    lastError: session.lastError,
  );

  @override
  Future<api.RecordingFinalizeResult> finalizeRecording({
    required String recordingId,
    required int lastSegmentIndex,
    required String endReason,
  }) async {
    final result = await _translate(
      () => _open.finalizeRecording(
        recordingId: recordingId,
        lastSegmentIndex: lastSegmentIndex,
        endReason: endReason,
      ),
    );
    return api.RecordingFinalizeResult(
      recordingId: result.recordingId,
      assetId: result.assetId,
      state: api.RecordingState.values.byName(result.state.name),
      segmentCount: result.segmentCount,
      transcriptionQueued: result.transcriptionQueued,
      logicalDurationMs: result.logicalDurationMs,
    );
  }

  @override
  Future<api.RecordingRecovery> recoverRecording({String? recordingId}) async {
    final recovery = await _translate(
      () => _open.recoverRecording(recordingId: recordingId),
    );
    return api.RecordingRecovery(
      recordingId: recovery.recordingId,
      state: api.RecordingState.values.byName(recovery.state.name),
      closedSegmentIndexes: recovery.closedSegmentIndexes
          .map((index) => index.toInt())
          .toList(),
      gapMs: recovery.gapMs,
      notes: recovery.notes,
    );
  }

  @override
  Future<api.DueRunSummary> runDueJobs({
    required int budgetMs,
    required String trigger,
  }) async {
    final result = await _translate(
      () => _open.runDueJobs(budgetMs: budgetMs, trigger: trigger),
    );
    return api.DueRunSummary(
      startedAt: result.startedAt,
      trigger: result.trigger,
      processed: result.processed,
      succeeded: result.succeeded,
      failed: result.failed,
      retrying: result.retrying,
      backfilled: result.backfilled,
      recovered: result.recovered,
      dueBacklog: result.dueBacklog,
      waitingDependency: result.waitingDependency,
      dependencyFailed: result.dependencyFailed,
      nextWakeup: result.nextWakeup,
    );
  }

  /// 没接的方法一律明确报错——照 `diary_mock` 的做法，避免悄悄返回空数据。
  @override
  dynamic noSuchMethod(Invocation invocation) => throw UnsupportedError(
    '${invocation.memberName} 还没有接到适配层（当前只有记录路径）。'
    '对应的契约方法也 deliberately 不出现在 capabilities 里。',
  );

  // ------------------------------------------------------------ 映射

  /// 统一把桥接的错误翻成契约的异常。**未知错误码落到 `unknown`**，
  /// 不能让它变成一个没人认识的枚举值或者直接崩。
  Future<T> _translate<T>(Future<T> Function() action) async {
    try {
      return await action();
    } on bridge.BridgeError catch (error) {
      if (error.code == api.DiaryErrorCode.searchExpired.wire) {
        throw api.SearchExpiredException(message: error.message);
      }
      if (error.code == api.DiaryErrorCode.cursorExpired.wire) {
        throw api.CursorExpiredException(message: error.message);
      }
      throw api.DiaryException(
        code: _errorCode(error.code),
        message: error.message,
        retryable: error.retryable,
      );
    }
  }

  static api.DiaryErrorCode _errorCode(String wire) {
    try {
      return api.DiaryErrorCode.fromWire(wire);
    } on Object {
      return api.DiaryErrorCode.unknown;
    }
  }

  Future<api.CoreSnapshot> _snapshot(bridge.BridgeSession session) async {
    final info = await _translate(() => session.info());
    return api.CoreSnapshot(
      coreInfo: api.CoreInfo(
        apiVersion: info.apiVersion,
        dataSchemaVersion: info.dataSchemaVersion.toInt(),
        buildVersion: info.buildVersion,
        libraryId: info.libraryId,
        // 窄化：核心报的是「桥接接了多少」，这里只报适配层真的能用的。
        capabilities: info.capabilities.where(_wiredMethods.contains).toSet(),
      ),
      recovery: api.RecoverySummary(
        // 核心不区分「恢复的草稿」：草稿本来就持久存在（B1a），重启后仍是草稿，
        // 没有一个单独标记成「已恢复」的状态。所以这里恒为 0，而不是「读不到」。
        recoveredDraftCount: 0,
        orphanedImportCount: info.recovery.recoverableImports,
        unrecoveredRecordingCount: info.recovery.openRecordings,
        notes: info.recovery.notes,
      ),
      pendingJobCount: info.recovery.pendingJobs,
      lastEventSequence: info.lastEventSequence.toInt(),
      captureCount: info.captureCount.toInt(),
    );
  }

  api.Capture _capture(bridge.Capture value) => api.Capture(
    id: value.id,
    revision: value.revision.toInt(),
    state: _captureState(value.state),
    occurredAt: value.occurredAt,
    createdAt: value.createdAt,
    updatedAt: value.updatedAt,
    timeZone: value.timeZone,
    utcOffsetMinutes: value.utcOffsetMinutes,
    dayKey: value.dayKey,
    orderedSourceIds: value.orderedSourceIds,
    draftText: value.draftText,
    processingSummary: api.ProcessingSummary(
      unprocessed: value.processingSummary.unprocessed,
      processing: value.processingSummary.processing,
      searchable: value.processingSummary.searchable,
      needsAttention: value.processingSummary.needsAttention,
    ),
  );

  static api.CaptureState _captureState(bridge.CaptureState value) =>
      switch (value) {
        bridge.CaptureState.draft => api.CaptureState.draft,
        bridge.CaptureState.committed => api.CaptureState.committed,
        bridge.CaptureState.trashed => api.CaptureState.trashed,
      };

  api.SourceRevision _sourceRevision(bridge.SourceRevision value) =>
      api.SourceRevision(
        revisionId: value.revisionId,
        sourceId: value.sourceId,
        parentRevisionId: value.parentRevisionId,
        text: value.text,
        assetId: value.assetId,
        authorType: switch (value.authorType) {
          bridge.AuthorType.user => api.AuthorType.user,
          bridge.AuthorType.import_ => api.AuthorType.import,
        },
        occurredAt: value.occurredAt,
        // 桥接侧目前不提供 provenance（契约里它是可空字段），如实留空。
      );

  api.SearchSnapshot _searchSnapshot(bridge.SearchSnapshot value) =>
      api.SearchSnapshot(
        sessionId: value.sessionId,
        queryRevision: value.queryRevision.toInt(),
        phase: api.SearchPhase.values.byName(value.phase.name),
        results: value.results
            .map(
              (hit) => api.SearchHit(
                hitId: hit.hitId,
                groupId: hit.groupId,
                sourceKind: api.SourceKind.values.byName(hit.sourceKind.name),
                matchedBy: hit.matchedBy
                    .map((kind) => api.MatchedBy.values.byName(kind.name))
                    .toSet(),
                coverage: api.Coverage.values.byName(hit.coverage.name),
                sourceId: hit.sourceId,
                revisionId: hit.revisionId,
                dayKey: hit.dayKey,
                title: hit.title,
                snippet: hit.snippet,
                highlights: hit.highlights
                    .map(
                      (range) => api.TextRange(
                        start: range.start.toInt(),
                        end: range.end.toInt(),
                      ),
                    )
                    .toList(growable: false),
                locator: hit.locator == null ? null : _locator(hit.locator!),
              ),
            )
            .toList(growable: false),
        cursor: value.cursor,
        indexCoverage: api.Coverage.values.byName(value.indexCoverage.name),
        warnings: value.warnings,
      );

  api.SourceLocator _locator(bridge.SourceLocator value) => api.SourceLocator(
    type: api.LocatorType.values.byName(value.locatorType.name),
    sourceRevisionId: value.sourceRevisionId,
    textRange: value.textStart == null || value.textEnd == null
        ? null
        : api.TextRange(
            start: value.textStart!.toInt(),
            end: value.textEnd!.toInt(),
          ),
    startMs: value.startMs?.toInt(),
    endMs: value.endMs?.toInt(),
    pageNumber: value.pageNumber?.toInt(),
    blockId: value.blockId,
    rect: value.rect == null
        ? null
        : api.NormalizedRect(
            left: value.rect![0],
            top: value.rect![1],
            right: value.rect![2],
            bottom: value.rect![3],
          ),
    assetId: value.assetId,
  );
}

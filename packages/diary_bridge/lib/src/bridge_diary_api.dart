// 这个文件用前缀区分两个包：两边有一批**同名不同类型**的对象
// （Capture、CaptureState、DraftSaveResult…），不加前缀很容易把桥接的类型
// 递给契约的构造函数，而那种错误恰恰是编译器能拦住的。
import 'dart:async';

import 'package:diary_api/diary_api.dart' as api;
import 'package:flutter_rust_bridge/flutter_rust_bridge_for_generated.dart';

import '../diary_bridge.dart' as bridge;

/// 把桥接的 `BridgeSession` 适配成契约的 [api.DiaryApi]。
///
/// **覆盖记录、检索、远端服务与日记生成路径**：`open` / `snapshot` / `close` /
/// 记录 CRUD / `startSearch` / `nextPage` / `searchSnapshot` / `cancelSearch` /
/// `listProviders` / `saveProvider` / `removeProvider` / `testProvider` /
/// `requestResponse` / `getResponse` /
/// `getDay` / `generateDiary` / `listVersions` / `getVersion` /
/// `saveDiaryEdit` / `setCurrentVersion` / `mergeDiary`。其余契约方法一律抛
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
    'search.upgrade',
    'providers.list',
    'providers.save',
    'providers.remove',
    'providers.test',
    'responses.request',
    'responses.get',
    'diary.getDay',
    'diary.generate',
    'diary.listVersions',
    'diary.getVersion',
    'diary.saveEdit',
    'diary.setCurrent',
    'diary.merge',
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

  // ------------------------------------------------------------ 服务配置与主动回应

  @override
  Future<List<api.Provider>> listProviders() async {
    final session = _open;
    final providers = await _translate(() => session.listProviders());
    return providers.map(_provider).toList(growable: false);
  }

  /// 保存脱敏配置。契约的签名里没有密钥，所以这里只透传 `credentialRef`。
  ///
  /// 从设置页输入一个新密钥要走 [saveProviderCredential]（本片的过渡入口，
  /// 见它的文档）。
  @override
  Future<api.Provider> saveProvider(api.Provider provider) async {
    final session = _open;
    final saved = await _translate(
      () => session.saveProvider(config: _bridgeProvider(provider)),
    );
    return _provider(saved);
  }

  /// **过渡入口**：把明文密钥交给核心的密钥托管，并返回脱敏后的配置。
  ///
  /// 契约最终的口径是「平台层用系统凭据接口存好、核心只拿 `credentialRef`」
  /// （`PlatformHost.storeCredential` / `readCredential`）。平台层的凭据桥接还没接上
  /// （F1/F2），所以本片给设置页一条能用的路：明文只在这一次调用里经过桥，核心写进
  /// 密钥托管后只留引用——配置表、返回值与错误消息里都不会有明文。
  ///
  /// 宿主凭据接上之后这个方法会被移除（或改成只传 `credentialRef`），调用方不该把它
  /// 当成长期接口。
  Future<api.Provider> saveProviderCredential({
    required String providerId,
    required String secret,
  }) async {
    final session = _open;
    final current = await _translate(() => session.listProviders());
    final target = current.firstWhere(
      (item) => item.id == providerId,
      orElse: () => throw api.DiaryException(
        code: api.DiaryErrorCode.notFound,
        message: '没有这个服务配置：$providerId',
      ),
    );
    final saved = await _translate(
      () => session.saveProvider(config: target, secret: secret),
    );
    return _provider(saved);
  }

  @override
  Future<void> removeProvider(String providerId) async {
    final session = _open;
    await _translate(() => session.removeProvider(providerId: providerId));
  }

  @override
  Future<api.ProviderTestReport> testProvider({
    required String providerId,
    required Set<api.ProviderCapability> scope,
  }) async {
    final session = _open;
    final report = await _translate(
      () => session.testProvider(
        providerId: providerId,
        scope: scope.map(_bridgeCapability).toList(growable: false),
      ),
    );
    return api.ProviderTestReport(
      providerId: report.providerId,
      results: report.results.map(
        (capability, status) => MapEntry(
          api.ProviderCapability.values.byName(capability.name),
          api.ProviderTestStatus.values.byName(status.name),
        ),
      ),
      actualCallMade: report.actualCallMade,
      message: report.message,
    );
  }

  @override
  Future<api.ResponseTicket> requestResponse({
    required List<String> captureIds,
    required String requestText,
    String? providerId,
  }) async {
    final session = _open;
    final ticket = await _translate(
      () => session.requestResponse(
        captureIds: captureIds,
        requestText: requestText,
        providerId: providerId,
      ),
    );
    return api.ResponseTicket(
      jobId: ticket.jobId,
      responseId: ticket.responseId,
    );
  }

  @override
  Future<api.AiResponse> getResponse(String responseId) async {
    final session = _open;
    final response = await _translate(
      () => session.getResponse(responseId: responseId),
    );
    return api.AiResponse(
      id: response.id,
      requestText: response.requestText,
      body: response.body,
      createdAt: response.createdAt,
      captureIds: response.captureIds,
      providerId: response.providerId,
      model: response.model,
    );
  }

  // ------------------------------------------------------- 日记生成与版本（B4b）

  @override
  Future<api.DiaryDay> getDay(String dayKey) async {
    final session = _open;
    final day = await _translate(() => session.getDiaryDay(dayKey: dayKey));
    return _diaryDay(day);
  }

  /// 生成一篇日记。**输入在返回前已冻结**；真正的调用由 `runDueJobs` 执行。
  ///
  /// 首次生成可成为当前版本，之后一律候选（`regenerate` 是用户明确再要一版的例外）。
  /// 没有配好服务也返回票据：任务会停在 `waiting_configuration`，用 `getJob` 看状态。
  @override
  Future<api.DiaryGenerationTicket> generateDiary({
    required String dayKey,
    String? styleId,
    api.DiaryGenerationReason reason = api.DiaryGenerationReason.manual,
    required String operationId,
  }) async {
    final session = _open;
    final ticket = await _translate(
      () => session.generateDiary(
        dayKey: dayKey,
        styleId: styleId,
        reason: bridge.DiaryGenerationReason.values.byName(reason.name),
        operationId: operationId,
      ),
    );
    return api.DiaryGenerationTicket(
      jobId: ticket.jobId,
      sourceSnapshotId: ticket.sourceSnapshotId,
    );
  }

  @override
  Future<api.DiaryVersionPage> listVersions(
    String dayKey, {
    String? cursor,
    int pageSize = 20,
  }) async {
    final session = _open;
    final page = await _translate(
      () => session.listDiaryVersions(
        dayKey: dayKey,
        cursor: cursor,
        limit: pageSize,
      ),
    );
    return api.DiaryVersionPage(
      versions: page.versions.map(_diaryVersion).toList(growable: false),
      nextCursor: page.nextCursor,
    );
  }

  @override
  Future<api.DiaryVersion> getVersion(String versionId) async {
    final session = _open;
    final version = await _translate(
      () => session.getDiaryVersion(versionId: versionId),
    );
    return _diaryVersion(version);
  }

  // ------------------------------------------------- 编辑 / 指定当前 / 合并（B4c）

  /// 保存人工编辑：**新写一条 `user_edit` 版本**（基版一个字节都不动），并让它成为当前。
  ///
  /// `expectedRevision` 是这一天的 revision（`getDay().revision`）。
  /// 不符 → `revisionConflict`（核心零改动）；同一条 `operationId` 重试返回原版本，
  /// 换了内容 → `idempotencyConflict`。
  @override
  Future<api.DiaryVersion> saveDiaryEdit({
    required String baseVersionId,
    required String body,
    required int expectedRevision,
    required String operationId,
  }) async {
    final session = _open;
    final version = await _translate(
      () => session.saveDiaryEdit(
        baseVersionId: baseVersionId,
        body: body,
        expectedRevision: expectedRevision,
        operationId: operationId,
      ),
    );
    return _diaryVersion(version);
  }

  /// 指定某一天的当前版本（用户想退回上一版）。只允许这一天自己的版本。
  ///
  /// 旧当前版变成候选、不删除；切到已经是当前的版本是无操作。
  /// **换当前版本也会推进这一天的 revision**：两个窗口拿着同一个
  /// `expectedRevision` 时，后一个会被 `revisionConflict` 拒绝，不会静默覆盖前一个。
  @override
  Future<api.DiaryDay> setCurrentVersion({
    required String dayKey,
    required String versionId,
    required int expectedRevision,
  }) async {
    final session = _open;
    final day = await _translate(
      () => session.setDiaryCurrent(
        dayKey: dayKey,
        versionId: versionId,
        expectedRevision: expectedRevision,
      ),
    );
    return _diaryDay(day);
  }

  /// 把两版（或多版）合成一条新的 `merge` 版本，`body` 是用户确认后的正文。
  ///
  /// 来源记在新版本的 `generationInfo.generationOptions.mergeSourceVersionIds`；
  /// 参与合并的版本全部保留、随时可取回原文；新版本成为当前。
  @override
  Future<api.DiaryVersion> mergeDiary({
    required String baseVersionId,
    required List<String> candidateVersionIds,
    required String body,
    required int expectedRevision,
    required String operationId,
  }) async {
    final session = _open;
    final version = await _translate(
      () => session.mergeDiaryVersions(
        baseVersionId: baseVersionId,
        candidateVersionIds: candidateVersionIds,
        body: body,
        expectedRevision: expectedRevision,
        operationId: operationId,
      ),
    );
    return _diaryVersion(version);
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

  api.Provider _provider(bridge.Provider value) => api.Provider(
    id: value.id,
    label: value.label,
    adapterId: value.adapterId,
    capabilities: value.capabilities
        .map((capability) => api.ProviderCapability.values.byName(capability.name))
        .toSet(),
    enabled: value.enabled,
    endpoint: value.endpoint,
    model: value.model,
    credentialRef: value.credentialRef,
    testStatus: api.ProviderTestStatus.values.byName(value.testStatus.name),
  );

  /// 契约对象 → 桥接对象。密钥不在这里：它只经 `saveProvider` 的 `secret` 参数。
  bridge.Provider _bridgeProvider(api.Provider value) => bridge.Provider(
    id: value.id,
    label: value.label,
    adapterId: value.adapterId,
    capabilities: value.capabilities
        .map(_bridgeCapability)
        .toSet(),
    enabled: value.enabled,
    endpoint: value.endpoint,
    model: value.model,
    credentialRef: value.credentialRef,
    testStatus: bridge.ProviderTestStatus.values.byName(value.testStatus.name),
  );

  static bridge.ProviderCapability _bridgeCapability(
    api.ProviderCapability capability,
  ) => bridge.ProviderCapability.values.byName(capability.name);

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

  api.DiaryDay _diaryDay(bridge.DiaryDay value) => api.DiaryDay(
    dayKey: value.dayKey,
    timeZone: value.timeZone,
    updatedAt: value.updatedAt,
    currentVersionId: value.currentVersionId,
    candidateVersionIds: value.candidateVersionIds,
    revision: value.revision.toInt(),
    materialCounts: api.MaterialCounts(
      captures: value.materialCounts.captures.toInt(),
      processed: value.materialCounts.processed.toInt(),
      pending: value.materialCounts.pending.toInt(),
      failed: value.materialCounts.failed.toInt(),
    ),
    coverage: api.Coverage.values.byName(value.coverage.name),
  );

  api.DiaryVersion _diaryVersion(bridge.DiaryVersion value) => api.DiaryVersion(
    id: value.id,
    dayKey: value.dayKey,
    parentVersionId: value.parentVersionId,
    origin: switch (value.origin) {
      bridge.DiaryVersionOrigin.generated => api.DiaryVersionOrigin.generated,
      bridge.DiaryVersionOrigin.userEdit => api.DiaryVersionOrigin.userEdit,
      bridge.DiaryVersionOrigin.merge => api.DiaryVersionOrigin.merge,
    },
    body: value.body,
    createdAt: value.createdAt,
    sourceSnapshotId: value.sourceSnapshotId,
    styleSnapshotId: value.styleSnapshotId,
    generationInfo: value.generationInfo == null
        ? null
        : api.GenerationInfo(
            promptVersion: value.generationInfo!.promptVersion,
            providerId: value.generationInfo!.providerId,
            model: value.generationInfo!.model,
            generationOptions: value.generationInfo!.generationOptions,
            coveredSourceCount: value.generationInfo!.coveredSourceCount.toInt(),
            unprocessedSourceCount: value
                .generationInfo!
                .unprocessedSourceCount
                .toInt(),
          ),
    provenanceLinks: value.provenanceLinks
        .map(
          (link) => api.ProvenanceLink(
            sourceId: link.sourceId,
            sourceRevisionId: link.sourceRevisionId,
            locator: _locator(link.locator),
            bodyRange: link.bodyRange == null
                ? null
                : api.TextRange(
                    start: link.bodyRange!.start.toInt(),
                    end: link.bodyRange!.end.toInt(),
                  ),
            note: link.note,
          ),
        )
        .toList(growable: false),
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

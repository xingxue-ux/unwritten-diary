// 这个文件用前缀区分两个包：两边有一批**同名不同类型**的对象
// （Capture、CaptureState、DraftSaveResult…），不加前缀很容易把桥接的类型
// 递给契约的构造函数，而那种错误恰恰是编译器能拦住的。
import 'dart:async';

import 'package:diary_api/diary_api.dart' as api;
import 'package:flutter_rust_bridge/flutter_rust_bridge_for_generated.dart';

import '../diary_bridge.dart' as bridge;

/// 把桥接的 `BridgeSession` 适配成契约的 [api.DiaryApi]。
///
/// **覆盖记录、来源与资产、检索、远端服务、索引、任务、日记、插件与备份检查**：
/// `open` / `snapshot` / `close` / `events` / 记录 CRUD / 导入与录音交接 /
/// `openAsset` / `releaseAsset` / `locateSource` / `startSearch` / `nextPage` /
/// `searchSnapshot` / `cancelSearch` / `upgradeSearch` / `listProviders` /
/// `saveProvider` / `removeProvider` / `testProvider` / `requestResponse` /
/// `getResponse` / `indexesStatus` / `rebuildIndexes` / `rebuildSemanticIndex` /
/// 任务读取、取消、重试、到期执行与 `acknowledgeAttention` / `getDay` /
/// `generateDiary` / `listVersions` / `getVersion` / `saveDiaryEdit` /
/// `setCurrentVersion` / `mergeDiary` / 插件管理与 `inspectBackup`。
///
/// **备份的同步任务取舍**：`backups.create` / `backups.restore` 的核心入口是同步的，
/// 适配层在同步完成之后写一条 `succeeded` 的 `backup` / `restore` 任务记录，返回的
/// `jobId` 指向这条**已经完成**的记录，不是排队任务。任务的 `targetIds` 记真实产物
/// 路径（备份包 / 恢复出来的库），`getJob` 读得到。代码注释、README 与 m5 文档都写明
/// 这一点，避免调用方把 `jobId` 当成进度句柄。
///
/// **仍没接的**（都会走 [noSuchMethod] 并在异常消息里点名）：
/// - 回收站五条（`moveDay` / `trashCapture` / `restoreCapture` / `previewPurge` /
///   `purgeCapture`）：核心实现还在 PR #155，main 上没有对应入口；
/// - 天列表与风格/导出（`listDays` / `listStyles` / `getStyle` / `saveStyle` /
///   `createExport`）：核心实现还在 PR #159；
/// - `setRuntimeState`：核心没有资源调度入口，不造一个只记不做的假实现；
/// - `getSettings` / `updateSettings`：由设置那条线负责，本分支不碰；
/// - `invokePlugin`：契约是票据式（返回 `jobId`），核心 `run_plugin` 同步返回完整
///   结果且没有结果读取入口；硬套 jobId 会造出「有 id 没结果」的假票据，保持不接。
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
    this.eventPollInterval = const Duration(seconds: 1),
    this.eventBatchSize = 256,
  });

  /// 桥接产物（Windows 的 `.dll`、Android 的 `.so`）路径，用来初始化 frb。
  final String coreLibraryPath;

  /// 资料库文件路径。契约里这是平台层的活（`libraryHandle` 就是不透明句柄），
  /// 平台层（F1）到位之前由调用方直接注入。
  final String libraryPath;

  /// 契约把时区与偏移放在平台层，而核心的 `createDraft` 需要它们。
  final String timeZone;
  final int utcOffsetMinutes;

  /// [events] 轮询持久事件表的间隔。
  ///
  /// 核心没有推送通道，事件是**持久表**，所以订阅只能是按序号轮询；这个参数
  /// 只影响延迟，不影响「不漏事件」——每次轮询都从上一次的最大序号继续。
  /// 测试可以把它调小，产品默认 1 秒。
  final Duration eventPollInterval;

  /// [events] 每次向核心取多少条事件（1..=1000）。
  ///
  /// 订阅一个历史很长的库时，不能把整张事件表一次读进内存：适配层每批最多取
  /// 这么多条，积压没读完就立刻取下一批，只有读到空批才等 [eventPollInterval]。
  /// 测试可以调小来逼出分页路径；产品默认 256。
  final int eventBatchSize;

  /// frb 的初始化在一个进程里只能做一次。
  static Future<void>? _rustLibReady;

  bridge.BridgeSession? _session;

  /// 适配层真正接上的契约方法。核心报的能力清单是它的上限，这里是实际能力。
  static const Set<String> _wiredMethods = {
    'core.open',
    'core.snapshot',
    'core.close',
    'core.events',
    'captures.createDraft',
    'captures.saveDraft',
    'captures.commit',
    'captures.get',
    'captures.list',
    'sources.reviseText',
    'sources.previousRevision',
    'sources.extractedContent',
    'sources.extract',
    'sources.locate',
    'assets.open',
    'assets.release',
    'search.start',
    'search.nextPage',
    'search.snapshot',
    'search.cancel',
    'jobs.nextWakeup',
    'jobs.runDue',
    'jobs.get',
    'jobs.list',
    'jobs.cancel',
    'jobs.retry',
    'jobs.acknowledgeAttention',
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
    'indexes.status',
    'indexes.rebuild',
    'indexes.rebuildSemantic',
    'backups.create',
    'backups.inspect',
    'backups.restore',
    'plugins.inspectPackage',
    'plugins.install',
    'plugins.enable',
    'plugins.disable',
    'plugins.uninstall',
    'plugins.list',
  };

  /// 契约 `DiaryApi` 的全部方法名（按能力分组）。
  ///
  /// 这张表只用于 [noSuchMethod] 的诚实报错：它能说出「还差哪些」，而不是一句
  /// 过时的「当前只有记录路径」。契约改动时这里要和
  /// `packages/diary_api/lib/src/diary_api.dart` 一起改；
  /// `test/bridge_diary_api_test.dart` 会检查 `_wiredMethods` 是它的子集、且
  /// `noSuchMethod` 的消息确实列出了差集。
  static const Set<String> _contractMethods = {
    'core.open',
    'core.snapshot',
    'core.close',
    'core.setRuntimeState',
    'core.events',
    'captures.createDraft',
    'captures.saveDraft',
    'captures.commit',
    'captures.get',
    'captures.list',
    'sources.reviseText',
    'sources.extractedContent',
    'sources.previousRevision',
    'sources.extract',
    'captures.moveDay',
    'captures.trash',
    'captures.restore',
    'captures.previewPurge',
    'captures.purge',
    'imports.prepare',
    'imports.finish',
    'imports.status',
    'imports.cancel',
    'recordings.prepare',
    'recordings.registerSegment',
    'recordings.updateState',
    'recordings.finalize',
    'recordings.recover',
    'assets.open',
    'assets.release',
    'sources.locate',
    'diary.getDay',
    'diary.listDays',
    'diary.generate',
    'diary.listVersions',
    'diary.getVersion',
    'diary.saveEdit',
    'diary.setCurrent',
    'diary.merge',
    'styles.list',
    'styles.get',
    'styles.save',
    'responses.request',
    'responses.get',
    'search.start',
    'search.upgrade',
    'search.nextPage',
    'search.snapshot',
    'search.cancel',
    'indexes.status',
    'indexes.rebuild',
    'indexes.rebuildSemantic',
    'jobs.list',
    'jobs.get',
    'jobs.retry',
    'jobs.cancel',
    'jobs.nextWakeup',
    'jobs.runDue',
    'jobs.acknowledgeAttention',
    'providers.list',
    'providers.save',
    'providers.remove',
    'providers.test',
    'settings.get',
    'settings.update',
    'plugins.inspectPackage',
    'plugins.install',
    'plugins.enable',
    'plugins.disable',
    'plugins.uninstall',
    'plugins.list',
    'plugins.invoke',
    'backups.create',
    'backups.inspect',
    'backups.restore',
    'exports.create',
  };

  /// 当前还没实现的契约方法名（`noSuchMethod` 如实报出来，不静默）。
  static List<String> get _unimplementedMethods =>
      _contractMethods.difference(_wiredMethods).toList()..sort();

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

  /// 订阅持久业务事件：按 `sequence` 分页轮询核心的事件表。
  ///
  /// 语义与契约第 6 节一致：**先取快照，再从快照的 `lastEventSequence` 续读**；
  /// 不传 [fromSequence] 时从 0 开始补全部还能读到的事件。核心没有推送通道，
  /// 所以这里以 [eventPollInterval] 轮询，每次都用上一批的最大序号作为下一次的
  /// 游标——不会重复发同一条，也不会跳过两批之间的新事件。
  ///
  /// **分页**：每批最多 [eventBatchSize] 条；积压还没读完就立刻取下一批，只有
  /// 读到空批才等下一轮。因此从 0 订阅一个很大的库也不会把整张事件表一次读进内存。
  ///
  /// 游标过期：核心目前**不裁剪**事件表，所以「太老」不会发生；能判定的过期有两种：
  /// **游标指向未来**（例如资料库被换成了另一份、序号回退），以及**首批事件与游标
  /// 之间有缺口**（事件表被裁剪过）。两种都在流上发 [api.CursorExpiredException]
  /// 并结束，调用方应重新读快照而不是无限等待。其他桥接错误同样以
  /// [api.DiaryException] 形式发到流上。
  @override
  Stream<api.DomainEvent> events({int? fromSequence}) async* {
    final session = _open;
    if (eventBatchSize < 1 || eventBatchSize > 1000) {
      throw ArgumentError.value(
        eventBatchSize,
        'eventBatchSize',
        '事件批大小必须在 1..=1000（核心同样按这个上限夹取）',
      );
    }
    if (fromSequence != null && fromSequence < 0) {
      throw const api.DiaryException(
        code: api.DiaryErrorCode.invalidArgument,
        message: '事件游标不能是负数。',
      );
    }
    var cursor = fromSequence ?? 0;
    final info = await _translate(() => session.info());
    final last = info.lastEventSequence.toInt();
    if (cursor > last) {
      throw api.CursorExpiredException(
        message: '事件游标 $cursor 超过当前最大序号 $last；资料库可能被替换过，请重新读取快照。',
      );
    }
    while (true) {
      final batch = await _translate(
        () => session.eventsSincePage(
          fromSequence: cursor,
          limit: eventBatchSize,
        ),
      );
      if (batch.isEmpty) {
        await Future<void>.delayed(eventPollInterval);
        continue;
      }
      final first = batch.first.sequence.toInt();
      if (first > cursor + 1) {
        throw api.CursorExpiredException(
          message:
              '事件游标 $cursor 与最早可读事件 $first 之间有缺口；事件表可能被裁剪过，请重新读取快照。',
        );
      }
      for (final event in batch) {
        final sequence = event.sequence.toInt();
        cursor = sequence;
        yield _domainEvent(event);
      }
      // 这一批是满的，说明可能还有积压：立刻取下一批，不白等一个轮询间隔。
      if (batch.length < eventBatchSize) {
        await Future<void>.delayed(eventPollInterval);
      }
    }
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
  Future<api.SourceRevision> reviseText({
    required String sourceId,
    required String text,
    required int expectedRevision,
    required String operationId,
  }) async => _sourceRevision(
    await _translate(
      () => _open.reviseText(
        sourceId: sourceId,
        text: text,
        expectedRevision: expectedRevision,
        operationId: operationId,
      ),
    ),
  );

  @override
  Future<api.SourceRevision?> previousSourceRevision(String sourceId) async {
    final revision = await _translate(
      () => _open.previousSourceRevision(sourceId: sourceId),
    );
    return revision == null ? null : _sourceRevision(revision);
  }

  @override
  Future<api.ExtractedContent?> extractedContent(String sourceId) async {
    final content = await _translate(
      () => _open.extractedContent(sourceId: sourceId),
    );
    return content == null ? null : _extractedContent(content);
  }

  @override
  Future<api.ExtractedContent> extractSource(String sourceRef) async =>
      _extractedContent(
        await _translate(() => _open.extractSource(sourceRef: sourceRef)),
      );

  @override
  Future<api.SourceLocation> locateSource({
    required String ref,
    required api.SourceLocator locator,
  }) async {
    // 契约的 `ref` 就是核心的 `sourceRef`：来源 id 或来源修订 id。
    final location = await _translate(
      () =>
          _open.locateSource(sourceRef: ref, locator: _bridgeLocator(locator)),
    );
    return api.SourceLocation(
      sourceRef: location.sourceRef,
      locator: _locator(location.locator),
      available: location.available,
      assetId: location.assetId,
      reason: location.reason,
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

  @override
  Future<api.Job> getJob(String id) async =>
      _job(await _translate(() => _open.getJob(jobId: id)));

  @override
  Future<api.JobPage> listJobs({
    Set<api.JobState>? states,
    String? cursor,
    int pageSize = 20,
  }) async {
    // The current bridge exposes a bounded recent list, without pagination.
    if (cursor != null) {
      throw const api.DiaryException(
        code: api.DiaryErrorCode.invalidArgument,
        message: '当前核心暂不支持继续翻页任务列表。',
      );
    }
    final rows = await _translate(
      () => _open.listJobs(
        states: states
            ?.map((state) => bridge.JobState.values.byName(state.name))
            .toList(),
        limit: pageSize,
      ),
    );
    return api.JobPage(jobs: rows.map(_job).toList(growable: false));
  }

  @override
  Future<api.Job> cancelJob({
    required String id,
    required String operationId,
  }) async => _job(
    await _translate(
      () => _open.cancelJob(jobId: id, operationId: operationId),
    ),
  );

  @override
  Future<api.Job> retryJob({
    required String id,
    required String operationId,
  }) async => _job(
    await _translate(() => _open.retryJob(jobId: id, operationId: operationId)),
  );

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
  Future<api.AssetLease> openAsset({
    required String assetId,
    required api.AssetUsage usage,
  }) async {
    final lease = await _translate(
      () => _open.openAsset(assetId: assetId, usage: usage.wire),
    );
    return api.AssetLease(
      leaseId: lease.leaseId,
      assetId: lease.assetId,
      usage: api.AssetUsage.fromWire(lease.usage),
      handle: lease.handle,
      expiresAt: lease.expiresAt,
      byteSize: lease.byteSize.toInt(),
    );
  }

  @override
  Future<void> releaseAsset(String leaseId) =>
      _translate(() => _open.releaseAsset(leaseId: leaseId));

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

  @override
  Future<void> acknowledgeAttention(String attentionKey) =>
      _translate(() => _open.acknowledgeAttention(attentionKey: attentionKey));

  // ------------------------------------------------------------ 索引

  /// 关键词索引与语义索引的覆盖状态，契约 `indexes.status`。
  ///
  /// 契约的 `pendingCount` / `failedCount` 没有单位字段；核心把「待重建片段数」
  /// 与「正文解析失败的材料数」分开报，这里就按这两个真实数字映射，不另算一个
  /// 好看的数。`sourceScope` 为空是整库，空集合是「什么都不看」——与核心口径一致。
  @override
  Future<api.IndexStatus> indexesStatus({Set<String>? sourceScope}) async {
    final status = await _translate(
      () =>
          _open.indexStatus(sourceScope: sourceScope?.toList(growable: false)),
    );
    return api.IndexStatus(
      coverage: api.Coverage.values.byName(status.coverage.name),
      keywordIndexReady: status.keywordIndexReady,
      semanticIndexReady: status.semanticIndexReady,
      modelVersion: status.modelVersion,
      chunkerVersion: status.chunkerVersion,
      pendingCount: status.pendingSegments.toInt(),
      failedCount: status.failedSources.toInt(),
      reasons: status.reasons,
    );
  }

  /// 排一条关键词索引重建任务，契约 `indexes.rebuild`。
  ///
  /// **只排任务**：真正的重建由 `index` 执行者在 [runDueJobs] 里跑，界面不阻塞。
  /// `scope` 的编码（`all` / `sources:<id>[,<id>]*`）由核心校验，非法输入按契约
  /// 报 `invalid_argument` / `not_found`，适配层不改写、不吞错。
  @override
  Future<api.JobTicket> rebuildIndexes({
    required String scope,
    required String reason,
    required String operationId,
  }) async {
    final job = await _translate(
      () => _open.rebuildIndexes(
        scope: scope,
        reason: reason,
        operationId: operationId,
      ),
    );
    return api.JobTicket(jobId: job.id);
  }

  /// 排一条**语义索引**重建任务，契约第 4.4 节 `indexes.rebuildSemantic`。
  ///
  /// **异步**：只排任务并返回它的 `jobId`（这里交出契约的 [api.JobTicket]），真正的
  /// 重建由 `semantic_index` 执行者在 `runDueJobs` 里跑。它**等模型配置**（没配好时
  /// 任务落 `waiting_configuration`，不是失败），并且**可分段让出**：预算到点就回到
  /// `queued`、进度写在 `Job.progress` 上，下一轮接着算——所以「换过模型、重建整库」
  /// 这件事不会把一次唤醒拖死，也不会假装已经建好。
  ///
  /// 幂等：同一个 `(reason, operationId)` 重复调用返回同一条任务（拿到的 `jobId`
  /// 相同）；同一个 operationId 换了原因报 `idempotency_conflict`；任务跑完后再请求
  /// 会建新的一条。
  @override
  Future<api.JobTicket> rebuildSemanticIndex({
    required String reason,
    required String operationId,
  }) async {
    final job = await _translate(
      () =>
          _open.rebuildSemanticIndex(reason: reason, operationId: operationId),
    );
    return api.JobTicket(jobId: job.id);
  }

  // ------------------------------------------------------------ 插件（B6，#88）

  /// 检查插件包（目录或 zip），**不执行包内容**。
  ///
  /// 契约的 `ticket` 在这里就是用户选中的包路径（平台票据层未接入前由调用方给出）。
  /// 平台由核心按宿主自动判定（桥接传空串时用 `host_platform()`）。
  @override
  Future<api.PluginPackageReport> inspectPluginPackage(String ticket) async {
    final report = await _translate(
      () => _open.inspectPluginPackage(packagePath: ticket, platform: ''),
    );
    return api.PluginPackageReport(
      packageId: report.manifest.id,
      version: report.manifest.version,
      apiRange: report.manifest.apiRange,
      compatible: report.compatible,
      supportedPlatforms: report.manifest.supportedPlatforms,
      requestedPermissions: report.manifest.permissions.toSet(),
      entryKind: _pluginEntryKind(report.manifest.entryKind),
      issues: report.issues,
    );
  }

  @override
  Future<api.Plugin> installPlugin({
    required String ticket,
    required Set<String> grantedPermissions,
  }) async {
    final installed = await _translate(
      () => _open.installPlugin(
        packagePath: ticket,
        platform: '',
        grants: grantedPermissions.toList(growable: false),
      ),
    );
    return _plugin(installed);
  }

  @override
  Future<api.Plugin> enablePlugin(String pluginId) async {
    final installed = await _translate(
      () => _open.setPluginEnabled(pluginId: pluginId, enabled: true),
    );
    return _plugin(installed);
  }

  @override
  Future<api.Plugin> disablePlugin(String pluginId) async {
    final installed = await _translate(
      () => _open.setPluginEnabled(pluginId: pluginId, enabled: false),
    );
    return _plugin(installed);
  }

  @override
  Future<void> uninstallPlugin(String pluginId) async {
    // 私有状态就在安装行里，卸载必然清理；没有额外缓存可留，故传 false。
    await _translate(
      () =>
          _open.uninstallPlugin(pluginId: pluginId, removePrivateState: false),
    );
  }

  @override
  Future<List<api.Plugin>> listPlugins() async {
    final plugins = await _translate(() => _open.listPlugins());
    return plugins.map(_plugin).toList(growable: false);
  }

  // ------------------------------------------------------------ 备份

  /// 检查备份包，契约 `backups.inspect`。
  ///
  /// 契约的 `packageTicket` 在这里就是用户选中的包路径（平台票据层未接入前由
  /// 调用方给出，与 `inspectPluginPackage` 同一个口径）。核心**不抛错**报坏包：
  /// 哈希、路径、版本问题都进 `issues`，界面先展示再决定要不要恢复。
  @override
  Future<api.BackupInspection> inspectBackup(String packageTicket) async {
    final inspection = await _translate(
      () => _open.inspectBackup(packagePath: packageTicket),
    );
    return api.BackupInspection(
      packageTicket: packageTicket,
      schemaVersion: inspection.schemaVersion.toInt(),
      apiVersion: inspection.apiVersion,
      byteSize: inspection.byteSize.toInt(),
      createdAt: inspection.createdAt,
      assetCount: inspection.assetCount.toInt(),
      issues: inspection.issues,
    );
  }

  /// 创建完整备份，契约 `backups.create`。
  ///
  /// **同步完成 + 已完成任务记录**：核心生成并自校验备份包之后，写一条
  /// `state = succeeded` 的 `backup` 任务记录；返回的 `jobId` 指向这条**已经完成**
  /// 的记录，不是排队任务。`getJob(jobId).targetIds` 是备份包路径（单个）。
  /// 大库上这个调用会占住线程，界面应显示「正在备份」而不是进度轮询。
  ///
  /// `targetTicket` 在平台票据层接入前就是包文件的完整路径（与 `inspectBackup`
  /// 同一口径）。`includeAssets = false` 明确拒绝：完整备份必须含原件，否则产出的
  /// 包不能恢复资料库。
  @override
  Future<api.JobTicket> createBackup(api.BackupRequest request) async {
    if (request.targetTicket.trim().isEmpty) {
      throw const api.DiaryException(
        code: api.DiaryErrorCode.invalidArgument,
        message: '备份目标不能为空。',
      );
    }
    if (!request.includeAssets) {
      throw const api.DiaryException(
        code: api.DiaryErrorCode.invalidArgument,
        message: '完整备份必须包含原件；includeAssets = false 会产出不可恢复的包，已拒绝。',
      );
    }
    final job = await _translate(
      () => _open.createBackupJob(targetPath: request.targetTicket),
    );
    return _completedJobTicket(job, 'backups.create');
  }

  /// 恢复备份，契约 `backups.restore`。
  ///
  /// **同步完成 + 已完成任务记录**：恢复成功后写一条 `state = succeeded` 的
  /// `restore` 任务记录，`jobId` 指向已完成记录，不是排队任务。`getJob(jobId)`
  /// 的 `targetIds` 是 `[恢复出来的 libraryPath, rootDir]`，调用方据此打开新库。
  ///
  /// 契约的这个方法没有目标目录参数（平台票据层也未接），所以默认恢复到当前库
  /// 同级的 `<库名>-restored-<微秒>`；**目标必须是新目录/空目录**，非空会被核心
  /// 拒绝（`invalid_state`），当前库一字不改。
  ///
  /// `options.restoreAsNewLibrary = false` 明确拒绝：核心只恢复到新库，绝不覆盖
  /// 当前库。`includeIndexes` 不改变核心的整库快照：false 不会剔除索引表，true 也
  /// 不会额外重建；索引始终可以在恢复后重建。
  @override
  Future<api.JobTicket> restoreBackup({
    required String packageTicket,
    api.RestoreOptions options = const api.RestoreOptions(),
  }) {
    return restoreBackupTo(
      packageTicket: packageTicket,
      targetDirectory: _defaultRestoreDirectory(),
      options: options,
    );
  }

  /// 恢复到指定目录（平台层/测试入口，**不是契约方法**）。
  ///
  /// `targetDirectory` 必须是新目录或空目录，且不能是当前资料库目录；否则核心在
  /// 写任何东西之前报 `invalid_state`。恢复成功后 `getJob(jobId).targetIds` 是
  /// `[libraryPath, rootDir]`。
  Future<api.JobTicket> restoreBackupTo({
    required String packageTicket,
    required String targetDirectory,
    api.RestoreOptions options = const api.RestoreOptions(),
  }) async {
    if (!options.restoreAsNewLibrary) {
      throw const api.DiaryException(
        code: api.DiaryErrorCode.invalidArgument,
        message: '核心只恢复到新资料库，绝不覆盖当前库；restoreAsNewLibrary = false 已拒绝。',
      );
    }
    final job = await _translate(
      () => _open.restoreBackupJob(
        packagePath: packageTicket,
        targetDir: targetDirectory,
      ),
    );
    return _completedJobTicket(job, 'backups.restore');
  }

  /// 同步完成的任务记录必须真的是 `succeeded`；不是就如实报错，不把排队/失败任务
  /// 当票据发出去。
  api.JobTicket _completedJobTicket(bridge.Job job, String operation) {
    if (job.state != bridge.JobState.succeeded) {
      throw api.DiaryException(
        code: api.DiaryErrorCode.invalidState,
        message: '$operation 没有落成 succeeded 任务（实际 ${job.state.name}）；请重试或查看任务面板。',
      );
    }
    return api.JobTicket(jobId: job.id);
  }

  /// 契约没有目标目录参数：默认恢复到当前库同级的 `<库名>-restored-<微秒>`。
  ///
  /// 不用 `dart:io`：适配层还要能在 web 目标上分析/编译；这里只做字符串路径处理，
  /// 目录是否存在交给核心校验（非空/已存在会被拒绝，绝不覆盖）。
  String _defaultRestoreDirectory() {
    final separator = libraryPath.contains('\\') ? '\\' : '/';
    final cut = libraryPath.lastIndexOf(separator);
    final directory = cut >= 0 ? libraryPath.substring(0, cut) : '.';
    final fileName = cut >= 0 ? libraryPath.substring(cut + 1) : libraryPath;
    final stem = fileName.endsWith('.sqlite')
        ? fileName.substring(0, fileName.length - '.sqlite'.length)
        : fileName;
    final prefix = directory.endsWith(separator)
        ? directory
        : '$directory$separator';
    final stamp = DateTime.now().toUtc().microsecondsSinceEpoch;
    return '${prefix}$stem-restored-$stamp';
  }

  /// 没接的方法一律明确报错——照 `diary_mock` 的做法，避免悄悄返回空数据。
  ///
  /// 消息里带上**仍未实现**的契约方法名，且这份清单从
  /// [_contractMethods] 与 [_wiredMethods] 实时算出来，不会像旧文案那样过时。
  @override
  dynamic noSuchMethod(Invocation invocation) {
    final symbol = invocation.memberName.toString();
    final match = RegExp(r'^Symbol\("(.*)"\)$').firstMatch(symbol);
    final name = match?.group(1) ?? symbol;
    final missing = _unimplementedMethods;
    throw UnsupportedError(
      'BridgeDiaryApi 还没有实现契约方法 `$name`。'
      '仍未实现（${missing.length}/${_contractMethods.length}）：'
      '${missing.join('、')}。'
      '这些方法不会出现在 capabilities 里；调用方应按能力清单降级，'
      '不要把它们当成可用入口。',
    );
  }

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

  api.Job _job(bridge.Job value) => api.Job(
    id: value.id,
    kind: value.kind,
    state: api.JobState.values.byName(value.state.name),
    targetIds: value.targetIds,
    inputSnapshotHash: value.inputSnapshotHash,
    progress: value.progress == null
        ? null
        : api.JobProgress(
            completed: value.progress!.completed.toInt(),
            total: value.progress!.total.toInt(),
          ),
    createdAt: value.createdAt,
    updatedAt: value.updatedAt,
    attemptCount: value.attemptCount.toInt(),
    nextAttemptAt: value.nextAttemptAt,
    errorCode: value.errorCode == null ? null : _errorCode(value.errorCode!),
    requiresUserAction: value.requiresUserAction,
    attentionKey: value.attentionKey,
  );

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
        .map(
          (capability) => api.ProviderCapability.values.byName(capability.name),
        )
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
    capabilities: value.capabilities.map(_bridgeCapability).toSet(),
    enabled: value.enabled,
    endpoint: value.endpoint,
    model: value.model,
    credentialRef: value.credentialRef,
    testStatus: bridge.ProviderTestStatus.values.byName(value.testStatus.name),
  );

  static bridge.ProviderCapability _bridgeCapability(
    api.ProviderCapability capability,
  ) => bridge.ProviderCapability.values.byName(capability.name);

  api.PluginEntryKind _pluginEntryKind(bridge.PluginEntryKind value) =>
      value == bridge.PluginEntryKind.wasm
      ? api.PluginEntryKind.code
      : api.PluginEntryKind.declarative;

  api.Plugin _plugin(bridge.PluginInstallation value) => api.Plugin(
    id: value.pluginId,
    version: value.version,
    apiRange: value.apiRange,
    entryKind: _pluginEntryKind(value.entryKind),
    state: value.enabled ? api.PluginState.enabled : api.PluginState.disabled,
    supportedPlatforms: value.supportedPlatforms,
    activationEvents: value.activationEvents,
    requestedPermissions: value.requestedPermissions.toSet(),
    grantedPermissions: value.permissions.toSet(),
    lastError: value.lastError,
  );

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

  api.ExtractedContent _extractedContent(bridge.ExtractedContent value) =>
      api.ExtractedContent(
        id: value.id,
        sourceId: value.sourceId,
        sourceRevisionId: value.sourceRevisionId,
        extractorId: value.extractorId,
        extractorVersion: value.extractorVersion,
        status: api.ProcessingStatus.values.byName(value.status.name),
        coverage: api.Coverage.values.byName(value.coverage.name),
        text: value.text,
        segments: value.segments
            .map(
              (segment) => api.ExtractedSegment(
                text: segment.text,
                locator: _locator(segment.locator),
                coverage: api.Coverage.values.byName(value.coverage.name),
              ),
            )
            .toList(growable: false),
        errorCode: value.errorCode == null
            ? null
            : _errorCode(value.errorCode!),
        coverageReason: value.coverageReason,
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
                captureId: hit.captureId,
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
            coveredSourceCount: value.generationInfo!.coveredSourceCount
                .toInt(),
            unprocessedSourceCount: value.generationInfo!.unprocessedSourceCount
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

  api.DomainEvent _domainEvent(bridge.DomainEvent event) => api.DomainEvent(
    eventId: event.eventId,
    sequence: event.sequence.toInt(),
    type: api.DomainEventType.values.byName(event.eventType.name),
    entityId: event.entityId,
    revision: event.revision.toInt(),
    emittedAt: event.emittedAt,
  );

  /// 契约定位对象 → 桥接定位对象。字段名不同（`type` / `textRange` 对
  /// `locatorType` / `textStart`+`textEnd`），语义一一对应。
  bridge.SourceLocator _bridgeLocator(api.SourceLocator value) =>
      bridge.SourceLocator(
        locatorType: bridge.LocatorType.values.byName(value.type.name),
        sourceRevisionId: value.sourceRevisionId,
        textStart: value.textRange?.start,
        textEnd: value.textRange?.end,
        startMs: value.startMs,
        endMs: value.endMs,
        pageNumber: value.pageNumber,
        blockId: value.blockId,
        rect: value.rect == null
            ? null
            : bridge.F64Array4(
                Float64List.fromList([
                  value.rect!.left,
                  value.rect!.top,
                  value.rect!.right,
                  value.rect!.bottom,
                ]),
              ),
        assetId: value.assetId,
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

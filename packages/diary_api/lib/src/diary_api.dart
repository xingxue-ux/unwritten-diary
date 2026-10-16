/// DiaryApi：Flutter 到本地业务核心的异步接口。契约第 4 节。
///
/// 页面只依赖这个抽象接口。实际产品使用 `RustBridgeDiaryApi`，页面测试使用
/// `MockDiaryApi`；两者必须实现同一组方法、错误与事件语义。
///
/// 通用约定（契约第 1.2 节）：
/// - ID 是核心生成的不透明字符串，调用方不得解析或拼接。
/// - 时间使用 UTC 语义的 [DateTime]；本地归属另看 `timeZone` 与 `dayKey`。
/// - 可重试写入必须带 `operationId`；先查已完成操作再检查 revision。
/// - 修改并发写入带 `expectedRevision`，不匹配抛 `revision_conflict`。
/// - 只有核心完成持久化才返回 `durable: true`，UI 的「已保存」以此为准。
library;

import 'dart:async';

import 'events.dart';
import 'models_content.dart';
import 'models_diary.dart';
import 'models_transport.dart';
import 'values.dart';

/// 本地业务核心的完整接口。
abstract class DiaryApi {
  // ---------------------------------------------------------------- 核心

  /// 打开或创建资料库。空值打开默认库，只初始化记录必要能力。
  ///
  /// 不等待远端服务、不全盘扫描媒体、不实例化所有插件、不加载向量模型。
  Future<CoreSnapshot> open({String? libraryHandle});

  /// 轻量启动快照，不返回整库正文或媒体。
  Future<CoreSnapshot> snapshot({Set<String>? scopes});

  /// 完成必要提交后关闭当前会话；不等待远端模型任务完成。
  Future<void> close();

  /// 上报前后台、录音活跃与压力情况，供核心调整资源调度建议。
  Future<void> setRuntimeState(RuntimeState state);

  /// 订阅持久业务事件。先取快照，再从可用游标补事件。
  ///
  /// 游标过期时流上会发出 [CursorExpiredException]，调用方应重新读快照。
  Stream<DomainEvent> events({int? fromSequence});

  // ------------------------------------------------------- 记录与来源

  /// 创建草稿。`occurredAt` 为空时由核心按当前时间填充。
  Future<Capture> createDraft({DateTime? occurredAt, required String operationId});

  /// 保存草稿正文。频繁保存只更新草稿与恢复日志，不制造永久版本。
  Future<DraftSaveResult> saveDraft({
    required String id,
    required String text,
    required int expectedRevision,
    required String operationId,
  });

  /// 提交记录：创建原始文字版本。附件未完成时保留其真实状态。
  Future<CommitResult> commit({
    required String id,
    required int expectedRevision,
    required String operationId,
  });

  Future<Capture> getCapture(String id);

  Future<CapturePage> listCaptures({
    String? dayKey,
    String? cursor,
    int pageSize = 20,
  });

  /// 修改原始文字，创建新的 SourceRevision，不覆盖旧内容。
  Future<SourceRevision> reviseText({
    required String sourceId,
    required String text,
    required int expectedRevision,
    required String operationId,
  });

  /// 调整日记归属日期。调整不改录音时间戳，也不静默搬动历史。
  Future<Capture> moveDay({
    required String id,
    required String targetDayKey,
    required int expectedRevision,
  });

  Future<Capture> trashCapture({
    required String id,
    required int expectedRevision,
    required String operationId,
  });

  Future<Capture> restoreCapture({
    required String id,
    required int expectedRevision,
    required String operationId,
  });

  /// 预览永久清理的影响：将删除的原件、派生内容，以及仍引用它的日记版本。
  Future<PurgePreview> previewPurge(String id);

  /// 永久清理，只由明确的用户操作发起。
  Future<PurgeResult> purgeCapture({
    required String id,
    required String previewToken,
    required String operationId,
  });

  // ---------------------------------------------------- 文件与录音交接

  /// 申请导入暂存票据。平台层只能在这个限域范围内写入。
  Future<ImportTicket> prepareImport({
    required String captureId,
    required String displayName,
    String? mimeHint,
    int? sizeHint,
    required String operationId,
  });

  /// 声明复制完成，核心开始校验；ready 之后才算完整入库。
  Future<ImportStatus> finishImport({
    required String importId,
    required String ticket,
    required ImportManifest manifest,
  });

  Future<ImportStatus> importStatus(String importId);

  /// 取消导入不影响其他已导入材料。
  Future<void> cancelImport(String importId);

  /// 申请录音票据与限域暂存目录。
  Future<RecordingTicket> prepareRecording({
    required String captureId,
    String? formatPreference,
    required String operationId,
  });

  /// 登记一个已封闭片段。重复登记同一片段返回原结果，内容不一致报冲突。
  Future<SegmentReceipt> registerSegment({
    required String recordingId,
    required int segmentIndex,
    required SegmentManifest segment,
  });

  /// 上报原生录音真实状态。后端不虚构麦克风状态。
  Future<RecordingSession> updateRecordingState(NativeRecordingStatus status);

  /// 最终化逻辑录音。转写不阻塞完成。
  Future<RecordingFinalizeResult> finalizeRecording({
    required String recordingId,
    required int lastSegmentIndex,
    required String endReason,
  });

  /// 恢复录音：不假设最后一段完好，明确报告末尾缺口。
  Future<RecordingRecovery> recoverRecording({String? recordingId});

  /// 打开只读句柄或限域租约。
  Future<AssetLease> openAsset({
    required String assetId,
    required AssetUsage usage,
  });

  Future<void> releaseAsset(String leaseId);

  /// 把定位信息解析成前端可打开的原件位置。
  Future<SourceLocation> locateSource({
    required String ref,
    required SourceLocator locator,
  });

  // ---------------------------------------------------- 日记、版本、回应

  Future<DiaryDay> getDay(String dayKey);

  Future<DiaryDayPage> listDays({
    String? fromDayKey,
    String? toDayKey,
    String? cursor,
    int pageSize = 20,
  });

  /// 冻结输入快照后生成日记。首次生成可成为当前版本。
  Future<DiaryGenerationTicket> generateDiary({
    required String dayKey,
    String? styleId,
    DiaryGenerationReason reason = DiaryGenerationReason.manual,
    required String operationId,
  });

  Future<DiaryVersionPage> listVersions(
    String dayKey, {
    String? cursor,
    int pageSize = 20,
  });

  Future<DiaryVersion> getVersion(String versionId);

  /// 保存人工编辑，创建新的 user_edit 版本，不覆盖基版。
  Future<DiaryVersion> saveDiaryEdit({
    required String baseVersionId,
    required String body,
    required int expectedRevision,
    required String operationId,
  });

  Future<DiaryDay> setCurrentVersion({
    required String dayKey,
    required String versionId,
    required int expectedRevision,
  });

  Future<DiaryVersion> mergeDiary({
    required String baseVersionId,
    required List<String> candidateVersionIds,
    required String body,
    required String operationId,
  });

  // ------------------------------------------------------------ 风格

  Future<List<StyleProfile>> listStyles();

  Future<StyleProfile> getStyle(String styleId);

  /// 保存风格偏好。偏好变更不自动重写历史日记。
  Future<StyleProfile> saveStyle(StyleProfile profile, {int? expectedRevision});

  // ------------------------------------------------------------ 主动回应

  /// 主动请求一次回应。只有用户主动调用才运行，默认不自动回应。
  Future<ResponseTicket> requestResponse({
    required List<String> captureIds,
    required String requestText,
    String? providerId,
  });

  Future<AiResponse> getResponse(String responseId);

  // -------------------------------------------------------------- 搜索

  /// 开始检索。返回初始快照，后续经事件更新。
  ///
  /// `hybrid` 模式的初始阶段是 [SearchPhase.keywordReady]：先只算关键词那一路，
  /// 首屏不等向量模型；要语义补充得再调 [upgradeSearch]。
  Future<SearchSnapshot> startSearch({
    required SearchRequest request,
    required int queryRevision,
  });

  /// 把已经给了关键词结果的会话升级到融合阶段（契约 4.4）。
  ///
  /// 语义与核心一致：
  /// - **同一会话**再取一份快照，阶段从 [SearchPhase.keywordReady] 走到
  ///   [SearchPhase.hybridReady]，结果是两路 RRF 融合并重排后的；
  /// - **幂等**：重复调用（含已经确认升不了级的情形）返回同一份，不重复计算；
  /// - **游标重置**：升级后的游标是融合结果第一页的游标（核心内部会递增结果
  ///   代次）；升级前发出的**所有**游标（含第一页那个与融合后第一页偏移相同的）
  ///   翻页报 `cursor_expired`，不按新顺序返回一段对不上的结果。游标对外仍是不
  ///   透明字符串，调用方照常原样回传即可。幂等升级不递增代次，升级后发出的游标
  ///   继续可用；
  /// - **语义不可用时绝不假装**：没有模型、没有生效代次或没有可用块时不融合，
  ///   阶段如实停在 [SearchPhase.keywordReady] 并在 `warnings` 里说明；
  /// - **错误**：会话过期抛 [SearchExpiredException]；非 `hybrid` 会话抛
  ///   `invalid_state`；已取消的会话返回 `cancelled` 快照、不融合。
  ///
  /// **默认实现是诚实的退化**：只实现本接口、接不上混合检索的实现者，拿到的
  /// 是这条会话**当前那一份**快照——也就是仍然 [SearchPhase.keywordReady] 的
  /// 关键词结果。这样做的原因：接口新增方法不能让已有实现者编译不过，而这里
  /// 能给出的最诚实的答案就是「我没有融合，阶段如实停在关键词阶段」。它**不抛
  /// 异常**（调用方在 keyword_ready 上应当继续可用），也**不伪造** hybrid_ready。
  /// 桥接（`BridgeDiaryApi`）等真正支持混合检索的实现会覆盖它。
  Future<SearchSnapshot> upgradeSearch({required String sessionId}) =>
      searchSnapshot(sessionId);

  /// 翻页绑定原会话；快照失效时抛 [SearchExpiredException]。
  Future<SearchSnapshot> nextPage({required String sessionId, String? cursor});

  Future<SearchSnapshot> searchSnapshot(String sessionId);

  /// 取消后续处理，不删除已经保存的原件。
  Future<void> cancelSearch(String sessionId);

  // ------------------------------------------------------------ 索引

  Future<IndexStatus> indexesStatus({Set<String>? sourceScope});

  /// 排一条关键词索引重建任务，返回它的 `jobId`。契约第 4.4 节 `indexes.rebuild`。
  ///
  /// **异步**：只排任务，不做同步重建——同步重建会阻塞界面，真正重建由 `index`
  /// 执行者在 [runDueJobs] 里跑（队列的退避、补做与依赖闸门已经就绪）。
  ///
  /// [scope] 的编码（契约第 4.4 节，v1）只有两种合法写法：
  /// - `all`：整库，等价于 [indexesStatus] 不传来源范围；
  /// - `sources:<id>[,<id>]*`：只重建这些来源。来源 id 是不透明字符串，`,` 是
  ///   分隔符，id 记号只允许 `[A-Za-z0-9_]`（不定义转义）。顺序没有语义，解析时
  ///   排序成规范形；空列表、空记号、重复 id、越界字符都是 `invalid_argument`，
  ///   不存在的来源是 `not_found`。
  ///
  /// [reason] 是给用户/排查看的原因文字；同一个 `(scope, reason, operationId)`
  /// 重复调用返回同一条任务，同一个 [operationId] 换了内容报
  /// `idempotency_conflict`。
  Future<JobTicket> rebuildIndexes({
    required String scope,
    required String reason,
    required String operationId,
  });

  /// 排一条**语义索引**重建任务，返回它的 `jobId`。契约第 4.4 节
  /// `indexes.rebuildSemantic`。
  ///
  /// **异步**：只排任务，不做同步重建。与 [rebuildIndexes]（关键词那一路）的区别：
  ///
  /// - **整库**：没有 `scope`。语义的代次与向量是全局的，一次重建总是把块表里所有块
  ///   在新代次里补齐，所以任务总是整库；业务键让「队列里至多一条活着的语义重建」，
  ///   重复排队不产生第二份工作；
  /// - **等模型配置**：本地模型没配好时任务落在 `waiting_configuration`
  ///   （`errorCode = waiting_model`），**不是失败**、不消耗重试额度，也不出现在
  ///   [nextWakeup] 里——配置变化不是时间问题，下一次 [runDueJobs] 的前置评估会把它
  ///   拉起。提取还没收尾时落 `waiting_dependency`；
  /// - **可分段让出**：整库重建会远超一次唤醒的预算，所以它在块与块之间检查预算，
  ///   到点就回到 `queued`（进度写在 `Job.progress` 上、`attemptCount` 不增加），
  ///   下一轮接着算没算完的块。**让出不是失败、也不算「已完成」**：代次切换仍是原子的，
  ///   [DueRunSummary.succeeded] 只在整代切换之后才发。
  ///
  /// 它**不会**顺手重建关键词索引，[rebuildIndexes] 也不会顺手建语义索引：两条独立的
  /// 索引路，各有各的入口与前置。就绪与覆盖照旧看 [indexesStatus]
  /// （`semanticIndexReady` / `modelVersion` / `reasons`）。
  ///
  /// 幂等：同一个 `(reason, operationId)` 重复调用返回同一条任务（拿到的 `jobId`
  /// 相同）；同一个 [operationId] 换了原因报 `idempotency_conflict`；任务跑完后再请求
  /// 会建新的一条（按现状增量重算：口径一致的向量整批复用）。
  Future<JobTicket> rebuildSemanticIndex({
    required String reason,
    required String operationId,
  });

  // -------------------------------------------------------------- 任务

  Future<JobPage> listJobs({
    Set<JobState>? states,
    String? cursor,
    int pageSize = 20,
  });

  Future<Job> getJob(String id);

  Future<Job> retryJob({required String id, required String operationId});

  Future<Job> cancelJob({required String id, required String operationId});

  /// 建议的下次唤醒时刻；为空表示没有待办。实际调度由平台决定。
  Future<DateTime?> nextWakeup();

  /// 在平台授予的运行预算内执行到期任务，支持取消与租约恢复。
  ///
  /// [budgetMs] 是**时间预算**（毫秒），`0` 表示这一轮一条都不跑。预算只在**任务
  /// 之间**判断：已经领取、正在跑的任务不打断（核心不做抢占），所以一条任务本身
  /// 可以比预算还长。[trigger] 是唤醒来源（startup / foreground / wake / manual …），
  /// 如实记进摘要、不改变行为。没跑上的到期任务留在队列里，数量在
  /// [DueRunSummary.dueBacklog] 里如实报出来。
  ///
  /// 依赖闸门在**每次领取前**重评估，所以「导入 → 提取 → 索引」这样的链在**一次**
  /// 调用里就能走完，不依赖平台层再叫一次；一轮结束后若仍有等待前置的任务，
  /// [DueRunSummary.nextWakeup] 必定非空。
  Future<DueRunSummary> runDueJobs({
    required int budgetMs,
    required String trigger,
  });

  /// 记录该问题已展示或已处理，避免每次重开重复通知。
  Future<void> acknowledgeAttention(String attentionKey);

  // ------------------------------------------------------------ 服务配置

  Future<List<Provider>> listProviders();

  Future<Provider> saveProvider(Provider provider);

  /// 移除配置不删除日记。
  Future<void> removeProvider(String providerId);

  /// 测试能力。报告必须说明是否发生了实际模型调用。
  Future<ProviderTestReport> testProvider({
    required String providerId,
    required Set<ProviderCapability> scope,
  });

  // -------------------------------------------------------------- 设置

  Future<Settings> getSettings();

  Future<Settings> updateSettings({
    required Settings settings,
    required int expectedRevision,
  });

  // -------------------------------------------------------------- 插件

  /// 检查插件包，不执行插件。
  Future<PluginPackageReport> inspectPluginPackage(String ticket);

  Future<Plugin> installPlugin({
    required String ticket,
    required Set<String> grantedPermissions,
  });

  Future<Plugin> enablePlugin(String pluginId);

  Future<Plugin> disablePlugin(String pluginId);

  /// 卸载不抹除历史日记。
  Future<void> uninstallPlugin(String pluginId);

  Future<List<Plugin>> listPlugins();

  /// 调用插件，受授权与执行预算约束。
  Future<PluginInvocationTicket> invokePlugin({
    required String pluginId,
    required String command,
    List<String>? captureIds,
  });

  // ------------------------------------------------------- 备份与导出

  Future<JobTicket> createBackup(BackupRequest request);

  Future<BackupInspection> inspectBackup(String packageTicket);

  /// 恢复默认新建资料库，不直接覆盖现库。
  Future<JobTicket> restoreBackup({
    required String packageTicket,
    RestoreOptions options = const RestoreOptions(),
  });

  /// 普通资料导出，与可完整恢复的备份是不同入口。
  Future<ExportTicket> createExport(ExportRequest request);
}
// 「不写日记」核心的 ArkTS 薄封装（#108 第二块）。
//
// 分层（从下到上）：
//
//   libdiary_capi.so   Rust：句柄 + JSON 进 / JSON 出（crates/diary_capi）
//   libdiary_napi.so   C++：dlopen 上面那个，7 个同步函数（entry/src/main/cpp/napi_shim.cpp）
//   DiaryCore（本文件）ArkTS：拼请求、拆信封、**把 ok:false 变成 DiaryError 异常**、给方法起名字
//
// 本层**不重写任何业务规则**：
// - 方法名用契约里的名字（`core.info` / `captures.saveDraft` …），只把点号换成驼峰方法名；
// - 参数字段名用 snake_case（核心 Rust 参数名），不做 camelCase 转换；
// - 返回值就是信封里的 `data`，字段名与核心 serde 输出一致（见 Models.ets）。
//
// ⚠️ 同步阻塞：这些方法直接在调用线程上跑核心（SQLite + 可能的重活）。**不要在主线程跑大库
// 上的 `search()` / `runDueJobs()` / `rebuildIndex()`**——要么放到 TaskPool 的 worker，
// 要么等后面的异步包装。本块没做异步，是因为先把「能调到核心」跑通（见文档 §8 未验证清单）。

import diaryNapi from 'libdiary_napi.so';
import { DiaryError, NATIVE_UNAVAILABLE } from './DiaryError';
import { buildRequest, unwrapEnvelope } from './Envelope';
import {
  AcknowledgeAttentionParams,
  BackupInspection,
  BackupReport,
  CancelJobParams,
  Capture,
  CaptureIdParams,
  CapturePage,
  CommitParams,
  CommitResult,
  CreateBackupParams,
  CreateDraftParams,
  CreateExportParams,
  DiaryDay,
  DiaryDayPage,
  DiaryDayParams,
  DiaryGenerationTicket,
  DiaryVersion,
  DiaryVersionPage,
  DiaryVersionParams,
  DomainEventPage,
  DraftSaveResult,
  EventsParams,
  ExportTicket,
  ExtractedContent,
  ExtractParams,
  FinalizeRecordingParams,
  GenerateDiaryParams,
  IndexStatus,
  IndexStatusParams,
  InspectBackupParams,
  Job,
  JobIdParams,
  JobPage,
  JobRunSummary,
  JobStatus,
  LibraryInfo,
  ListCapturesParams,
  ListDiaryDaysParams,
  ListDiaryVersionsParams,
  ListJobsParams,
  LocateSourceParams,
  MergeDiaryParams,
  MoveDayParams,
  NativeRecordingStatusParams,
  PrepareRecordingParams,
  ProviderConfig,
  ProviderIdParams,
  ProviderModelInfo,
  ProviderPreset,
  ProviderTestReport,
  PurgeCaptureParams,
  PurgePreview,
  PurgeResult,
  RebuildIndexParams,
  RebuildSemanticParams,
  RecoverRecordingParams,
  RecordingFinalizeResult,
  RecordingRecovery,
  RecordingSession,
  RecordingTicket,
  RegisterSegmentParams,
  RestoreBackupParams,
  RestoreReport,
  RetryJobParams,
  ReviseTextParams,
  RunDueParams,
  SaveDiaryEditParams,
  SaveDraftParams,
  SaveProviderParams,
  SaveStyleParams,
  SearchNextPageParams,
  SearchParams,
  SearchSessionParams,
  SearchSnapshot,
  SegmentReceipt,
  SetDiaryCurrentParams,
  Settings,
  SourceIdParams,
  SourceLocation,
  SourceRevision,
  StyleIdParams,
  StyleProfile,
  TestProviderParams,
  TrashCaptureParams,
  UpdateSettingsParams,
} from './Models';

/** 默认资料库文件名；与 Flutter 侧 `apps/diary_app/lib/src/app_services.dart` 用的是同一个。 */
const DEFAULT_LIBRARY_FILE: string = 'library.sqlite';

/**
 * 一个已打开的资料库。
 *
 * ```ts
 * const core = DiaryCore.openInDirectory(context.filesDir);
 * try {
 *   const info = core.coreInfo();
 * } finally {
 *   core.close();
 * }
 * ```
 */
export class DiaryCore {
  private handle: number = 0;
  private closed: boolean = false;
  private readonly libraryPath: string;

  private constructor(handle: number, libraryPath: string) {
    this.handle = handle;
    this.libraryPath = libraryPath;
  }

  // ---------------------------------------------------------------- 生命周期

  /**
   * 打开（或创建）资料库。`libraryPath` 是**文件路径**，不是目录
   * （例如 `<filesDir>/library.sqlite`）。
   *
   * 失败时抛 `DiaryError`：`invalid_argument`（路径为空/非法）、`permission_denied`、
   * `storage_full` …；原生库没打进包时是 `native_unavailable`。
   */
  static open(libraryPath: string): DiaryCore {
    if (libraryPath.length === 0) {
      throw new DiaryError('invalid_argument', '资料库路径为空');
    }
    let handle = 0;
    try {
      handle = diaryNapi.open(libraryPath);
    } catch (e) {
      throw DiaryError.fromThrown(e as Object);
    }
    return new DiaryCore(handle, libraryPath);
  }

  /** 在沙箱目录下打开默认资料库（`<dir>/library.sqlite`）。 */
  static openInDirectory(directory: string): DiaryCore {
    const base = directory.endsWith('/') ? directory.substring(0, directory.length - 1) : directory;
    return DiaryCore.open(`${base}/${DEFAULT_LIBRARY_FILE}`);
  }

  /**
   * 关闭。重复调用是 no-op（不会抛）；关闭后任何 `call` 都抛 `invalid_state`。
   *
   * 同一个路径被打开多次时，核心是共享的：**最后一个句柄关闭**才真正释放 SQLite 连接
   * （与 `diary_capi` 的登记表口径一致）。
   */
  close(): void {
    if (this.closed) {
      return;
    }
    const handle = this.handle;
    // 先标记再调原生：即使原生 close 报错，这个对象也不该再被使用。
    this.closed = true;
    this.handle = 0;
    try {
      diaryNapi.close(handle);
    } catch (e) {
      throw DiaryError.fromThrown(e as Object);
    }
  }

  /** 这个对象是否还持有打开的句柄。 */
  isOpen(): boolean {
    return !this.closed;
  }

  /** 打开时用的资料库文件路径。 */
  path(): string {
    return this.libraryPath;
  }

  // ---------------------------------------------------------------- 静态诊断

  /** 核心 C ABI 的版本戳（`diary_capi_version()`），例如 `diary_capi/0.1.0 apiVersion=1.1`。 */
  static version(): string {
    try {
      return diaryNapi.version();
    } catch (e) {
      throw DiaryError.fromThrown(e as Object);
    }
  }

  /** NAPI shim 自己被加载的目录（排障用；库没打进包时先看它）。 */
  static nativeLibraryDir(): string {
    return diaryNapi.selfDir();
  }

  /** 还没关闭的句柄数（泄漏自检；所有 `DiaryCore` 都 close 之后应为 0）。 */
  static openHandleCount(): number {
    return diaryNapi.openHandleCount();
  }

  // ---------------------------------------------------------------- 契约方法（50 条）
  //
  // 一一对应 `crates/diary_capi/src/methods.rs` 的 `WIRED_METHODS`。方法名与方法体里的
  // 字符串必须一致；`tools/check_arkts_capi_contract.py` 会核对。

  /** `core.info`：版本、能力清单、记录数、恢复摘要。 */
  coreInfo(): LibraryInfo {
    return this.invokeData('core.info', null) as LibraryInfo;
  }

  /**
   * `captures.createDraft`：建一条草稿。
   *
   * 注意：**正文不在这里给**（核心的 `createDraft` 只接收时间与时区），
   * 先 `createDraft()` 拿到 `capture_id` 与 `revision`，再 `saveDraft()` 写正文。
   */
  createDraft(params: CreateDraftParams): Capture {
    return this.invokeData('captures.createDraft', params) as Capture;
  }

  /** `captures.saveDraft`：写草稿正文；`durable === true` 才算「已保存」。 */
  saveDraft(params: SaveDraftParams): DraftSaveResult {
    return this.invokeData('captures.saveDraft', params) as DraftSaveResult;
  }

  /**
   * `captures.commit`：草稿转正式记录（`state='committed'`），有正文时生成原文修订。
   *
   * **不产生来源、也不产生检索内容**：文字来源是 commit 从 `draft_text` 复制出来的；
   * 录音的音频来源在 `recordings.finalize` 时就已经建好，转写/关键词索引也在那时
   * 或由 finalize 排的任务负责。commit 对录音只做「记录成篇」这一步，不会重复排队。
   */
  commit(params: CommitParams): CommitResult {
    return this.invokeData('captures.commit', params) as CommitResult;
  }

  /** `captures.get`：条目详情（含 `revision` / `state` / `processing_summary`）。 */
  getCapture(params: CaptureIdParams): Capture {
    return this.invokeData('captures.get', params) as Capture;
  }

  /**
   * `captures.list`：「看今天」与历史列表的数据源。
   *
   * 翻页：把上一页的 `next_cursor` 原样放进 `cursor`；`null` 表示没有下一页。
   * 「看今天」传 `day_key`（当天的 `YYYY-MM-DD`）；历史列表省略它。
   */
  listCaptures(params: ListCapturesParams | null = null): CapturePage {
    return this.invokeData('captures.list', params) as CapturePage;
  }

  // ---------------------------------------------------------------- 回收站（契约 4.1 / §8）
  //
  // 四条只做转接：状态机（含「还有在途录音/导入就拒绝」）、幂等回执、预览令牌的
  // 绑定/一次性/过期、以及日记版本的三种分类全在核心 `recycle.rs`。
  //
  // **可见性口径**（契约 §8）：回收站里的记录立即从**默认检索与后续 AI 输入**里排除；
  // `captures.list` 不按 `state` 过滤（回收站页面自己按 `state === 'trashed'` 筛），
  // 所以「我的」页不要指望列表帮它隐藏行。

  /**
   * `captures.trash`：移入回收站（`state = 'trashed'`，`revision` +1）。
   *
   * 只推进状态，正文/来源/转写都还在（恢复后照旧可读）。已经在回收站里报
   * `invalid_state`；还有没收尾的录音/导入时拒绝，要求先结束或取消。
   */
  trashCapture(params: TrashCaptureParams): Capture {
    return this.invokeData('captures.trash', params) as Capture;
  }

  /**
   * `captures.restore`：从回收站恢复回**移入之前**的状态（`draft` / `committed`）。
   *
   * 不在回收站里报 `invalid_state`；v16 之前没有回收站入口，旧行的「原状态」按
   * `committed` 解释（不会把已提交的记录悄悄变成草稿）。
   */
  restoreCapture(params: TrashCaptureParams): Capture {
    return this.invokeData('captures.restore', params) as Capture;
  }

  /**
   * `captures.previewPurge`：预览永久清理会删掉什么、会如实保留什么。
   *
   * 只接受回收站里的记录（不在回收站里是 `invalid_state`——清理不可逆，产品路径是
   * 先 `trashCapture` 再决定）。返回的 `preview_token` 是 `purgeCapture` 的**必填**参数：
   * 一次性、默认 10 分钟过期、绑定这条记录。
   */
  previewPurge(params: CaptureIdParams): PurgePreview {
    return this.invokeData('captures.previewPurge', params) as PurgePreview;
  }

  /**
   * `captures.purge`：按预览永久清理（不可逆）。
   *
   * `delete_diary_versions` 默认 `false`：要删的是 `generated` 版本，而一次生成可能
   * 覆盖同一天多条记录，删 A 会连带删掉含 B 内容的日记，所以默认保留。`true` 也只删
   * 「有可判定关联」的那些；`derived_diary_version_ids`（用户确认过的编辑/合并）与
   * `undetermined_diary_version_ids`（归属判不出来）**一个都不删**，结果里如实回填。
   *
   * 预览之后内容变了会拒绝并要求重新预览；同一个 `operation_id` 换选择报
   * `idempotency_conflict`。
   */
  purgeCapture(params: PurgeCaptureParams): PurgeResult {
    return this.invokeData('captures.purge', params) as PurgeResult;
  }

  /**
   * `captures.moveDay`：把一条记录挪到另一天（只改归属日）。
   *
   * `occurred_at` 一个字节都不动（调整不改录音时间戳），但旧日与新日的文本块会在核心的
   * 同一个事务里重打包，所以 `listDiaryDays` 与检索立刻按新的一天命中。
   *
   * 同一天再调是**无操作**（不推进 revision、不发事件）；在回收站里是 `invalid_state`
   * （先 `restoreCapture` 再改）；`expected_revision` 不符报 `revision_conflict`、零改动。
   */
  moveDay(params: MoveDayParams): Capture {
    return this.invokeData('captures.moveDay', params) as Capture;
  }

  // ---------------------------------------------------------------- 录音（契约 4.2）
  //
  // 这里只做「参数/回执的类型化 + 信封拆封」；**收音、分片、前台服务都在
  // `recording/VoiceRecorder.ets`**，核心只负责登记与最终化，不虚构麦克风状态。

  /** `recordings.prepare`：申请录音票据与限域暂存目录（只能往 `staging_ticket` 里写）。 */
  prepareRecording(params: PrepareRecordingParams): RecordingTicket {
    return this.invokeData('recordings.prepare', params) as RecordingTicket;
  }

  /** `recordings.registerSegment`：登记一个已封闭片段；核心会重算文件哈希与字节数。 */
  registerSegment(params: RegisterSegmentParams): SegmentReceipt {
    return this.invokeData('recordings.registerSegment', params) as SegmentReceipt;
  }

  /** `recordings.updateState`：上报原生录音状态（进度、暂停、错误都由平台层给）。 */
  updateRecordingState(params: NativeRecordingStatusParams): RecordingSession {
    return this.invokeData('recordings.updateState', params) as RecordingSession;
  }

  /**
   * `recordings.finalize`：把已封闭片段拼成逻辑音频、建音频资产，
   * **同一个事务里**排一条转写任务。转写不阻塞完成。
   */
  finalizeRecording(params: FinalizeRecordingParams): RecordingFinalizeResult {
    return this.invokeData('recordings.finalize', params) as RecordingFinalizeResult;
  }

  /**
   * `recordings.recover`：恢复最近一条（或指定）未收尾的录音。
   *
   * 不假设最后一段完好：已封闭序号、缺口估算、尾巴未知都在返回里如实写。
   */
  recoverRecording(params: RecoverRecordingParams): RecordingRecovery {
    return this.invokeData('recordings.recover', params) as RecordingRecovery;
  }

  /** `sources.extract`：抽取正文；`source_ref` 可以是来源 id 或修订 id。 */
  extract(params: ExtractParams): ExtractedContent {
    return this.invokeData('sources.extract', params) as ExtractedContent;
  }

  /**
   * `sources.extractedContent`：当前修订的提取结果；**还没提取过时返回 null**
   * （正常结果，不是错误）。
   *
   * 这是可空返回，所以走 `invokeNullable`——不能用 `invokeData`（它把 null 当信封坏了）。
   */
  extractedContent(params: SourceIdParams): ExtractedContent | null {
    return this.invokeNullable('sources.extractedContent', params) as ExtractedContent | null;
  }

  /**
   * `sources.previousRevision`：当前修订的上一版正文，供「看原正文/对比」用。
   *
   * 只有一版时返回 null（首次提交没有上一版）。
   */
  previousRevision(params: SourceIdParams): SourceRevision | null {
    return this.invokeNullable('sources.previousRevision', params) as SourceRevision | null;
  }

  /**
   * `sources.reviseText`：改原始正文，创建**新修订**，不覆盖旧内容。
   *
   * `expected_revision` 不符抛 `revision_conflict`；同一个 `operation_id` 重放返回
   * 同一条修订。
   */
  reviseText(params: ReviseTextParams): SourceRevision {
    return this.invokeData('sources.reviseText', params) as SourceRevision;
  }

  /**
   * `sources.locate`：把定位信息解析成前端可打开的原件位置。
   *
   * 纯文字/原件不在文件库时 `available === false` 并在 `reason` 里说明原因——
   * 这不是异常，界面应展示原因而不是报「出错了」。
   */
  locateSource(params: LocateSourceParams): SourceLocation {
    return this.invokeData('sources.locate', params) as SourceLocation;
  }

  /** `search.start`：发起一次检索，返回**当前页的完整快照**。 */
  search(params: SearchParams): SearchSnapshot {
    return this.invokeData('search.start', params) as SearchSnapshot;
  }

  /**
   * `search.nextPage`：翻下一页。
   *
   * `cursor` 省略时用会话自己记着的下一页；显式给时必须是同一会话、同一结果代次的
   * 游标。翻到头时 `cursor === null`、`phase === 'done'`；游标过期报 `cursor_expired`。
   */
  nextPage(params: SearchNextPageParams): SearchSnapshot {
    return this.invokeData('search.nextPage', params) as SearchSnapshot;
  }

  /**
   * `search.upgrade`：把 `mode = 'hybrid'` 的会话从关键词阶段升级到融合阶段。
   *
   * 语义拿不出证据时不假装融合：仍返回 `keyword_ready` 快照，原因在 `warnings` 里。
   * 幂等；升级成功后旧游标一律失效（`cursor_expired`）。
   */
  upgradeSearch(params: SearchSessionParams): SearchSnapshot {
    return this.invokeData('search.upgrade', params) as SearchSnapshot;
  }

  /**
   * `search.cancel`：取消后续处理，返回 `cancelled` 快照。不删除任何原件。
   *
   * 重复取消是幂等的；取消后 `nextPage` 报 `invalid_state`（不是静默给空页）。
   */
  cancelSearch(params: SearchSessionParams): SearchSnapshot {
    return this.invokeData('search.cancel', params) as SearchSnapshot;
  }

  /** `indexes.rebuild`：只排任务，不同步建索引（真正跑要 `runDueJobs()`）。 */
  rebuildIndex(params: RebuildIndexParams): Job {
    return this.invokeData('indexes.rebuild', params) as Job;
  }

  /**
   * `indexes.rebuildSemantic`：只排一条**整库**语义重建任务（契约 4.4）。
   *
   * 与 `rebuildIndex` 是两条独立的索引路：这条建向量（要模型与 ORT），那条建关键词。
   * 同样不同步重建——真正跑要 `runDueJobs()`。
   */
  rebuildSemanticIndex(params: RebuildSemanticParams): Job {
    return this.invokeData('indexes.rebuildSemantic', params) as Job;
  }

  /**
   * `indexes.status`：索引就绪与覆盖（`semantic_index_ready` / `model_version` /
   * `reasons` / 块与片段计数）。
   *
   * `source_scope` 省略 = 整库；空数组 = 「什么都不看」（两者刻意不同）。
   */
  indexStatus(params: IndexStatusParams | null = null): IndexStatus {
    return this.invokeData('indexes.status', params) as IndexStatus;
  }

  /** `jobs.runDue`：把到期的任务跑一轮（预算 + 条数上限，见 C ABI 文档 §6.1）。 */
  runDueJobs(params: RunDueParams): JobRunSummary {
    return this.invokeData('jobs.runDue', params) as JobRunSummary;
  }

  /** `jobs.status`：队列计数、到期积压、最近错误。 */
  jobsStatus(): JobStatus {
    return this.invokeData('jobs.status', null) as JobStatus;
  }

  /**
   * `jobs.list`：任务中心逐条（含 `attention_key` / `progress`）。
   *
   * `states` 省略或空数组都是「不限状态」；`cursor` 原样回传 `next_cursor`。
   */
  listJobs(params: ListJobsParams | null = null): JobPage {
    return this.invokeData('jobs.list', params) as JobPage;
  }

  /** `jobs.get`：单条任务（`progress` / `attention_key` 都在这）。 */
  getJob(params: JobIdParams): Job {
    return this.invokeData('jobs.get', params) as Job;
  }

  /**
   * `jobs.retry`：人工重试一条 `failed` 的任务（清零 `attempt_count` 重跑）。
   *
   * 只接受 `failed`；别的状态报 `invalid_state`。给了 `operation_id` 时可安全重放。
   */
  retryJob(params: RetryJobParams): Job {
    return this.invokeData('jobs.retry', params) as Job;
  }

  /** `jobs.cancel`：取消一条未终结的任务；不删除任何原件。 */
  cancelJob(params: CancelJobParams): Job {
    return this.invokeData('jobs.cancel', params) as Job;
  }

  /**
   * `jobs.acknowledgeAttention`：记录这个待处理问题已经展示过（跨重启去重）。
   *
   * 核心返回 `null`，所以走 `rawCall`；同一个 `attention_key` 重复调用是幂等的。
   */
  acknowledgeAttention(params: AcknowledgeAttentionParams): void {
    this.rawCall('jobs.acknowledgeAttention', params);
  }

  // ---------------------------------------------------------------- 本机服务配置

  /**
   * `providers.save`：保存一条服务配置（本机引擎的 `model` 是模型目录）。
   *
   * `test_status` 由核心维护：保存后它回到 `untested`，不能由调用方写成「已验证」。
   */
  saveProvider(params: SaveProviderParams): ProviderConfig {
    return this.invokeData('providers.save', params) as ProviderConfig;
  }

  /** `providers.list`：配置行（不包含核心自带的本机引擎内建行）。 */
  listProviders(): ProviderConfig[] {
    return this.invokeData('providers.list', null) as ProviderConfig[];
  }

  /**
   * `providers.presets`：内置预设表（静态数据，不读库、不出网、不含密钥）。
   *
   * 预设只是「填表草稿」：endpoint / model 可能是空串（表示要用户自己填），
   * `verified` 只认实测。保存仍然走 `saveProvider`，核心不会因为选了预设就建配置。
   */
  listProviderPresets(): ProviderPreset[] {
    return this.invokeData('providers.presets', null) as ProviderPreset[];
  }

  /**
   * `providers.models`：检测一条服务可用的模型列表（`GET {endpoint}/models`）。
   *
   * 只有这次显式调用才出网。本机（loopback）服务没有凭据也能检测；服务器返回
   * 401 / 超时仍按真实错误抛出，不把失败装成成功。
   */
  listProviderModels(params: ProviderIdParams): ProviderModelInfo[] {
    return this.invokeData('providers.models', params) as ProviderModelInfo[];
  }

  /** `providers.test`：对一条配置跑连接测试；本机引擎会真的加载模型并转写静音。 */
  testProvider(params: TestProviderParams): ProviderTestReport {
    return this.invokeData('providers.test', params) as ProviderTestReport;
  }

  /** `providers.remove`：只删配置行，不删任何日记/派生内容（核心返回 `null`）。 */
  removeProvider(params: ProviderIdParams): void {
    this.rawCall('providers.remove', params);
  }

  // ---------------------------------------------------------------- 设置（契约 4.4）

  /**
   * `settings.get`：读完整设置对象。
   *
   * 与 `organizeSettings()`（Flutter 桥接入口）是**同一行**：开关与计划时间的变化
   * 两个入口都看得到。新库的 `diary_time_zone` 是 `UTC`，平台层首次启动时应把设备
   * 时区（`i18n.System.getTimeZone()`）通过 `updateSettings()` 写进去。
   */
  getSettings(): Settings {
    return this.invokeData('settings.get', null) as Settings;
  }

  /**
   * `settings.update`：更新完整设置对象；`expected_revision` 是乐观锁。
   *
   * `settings.revision` / `settings.updated_at` 是只读结论，核心会忽略；冲突返回
   * `revision_conflict`，一个字节不改。
   */
  updateSettings(params: UpdateSettingsParams): Settings {
    return this.invokeData('settings.update', params) as Settings;
  }

  // ---------------------------------------------------------------- 备份与恢复（契约 4.5）
  //
  // 核心这三个是**同步**入口（契约那三行返回 jobId 的任务化是 #70，不在本块）。
  // `target_path` / `package_path` / `target_dir` 在 OHOS 上必须是应用沙箱内路径，
  // 否则 C ABI 报 `invalid_argument`（见 `crates/diary_capi/src/methods.rs`）。

  /** `backups.create`：生成可完整恢复的备份包（含原件，自校验通过才落地）。 */
  createBackup(params: CreateBackupParams): BackupReport {
    return this.invokeData('backups.create', params) as BackupReport;
  }

  /** `backups.inspect`：逐条目校验备份包；坏在哪就列在 `issues`，不抛错。 */
  inspectBackup(params: InspectBackupParams): BackupInspection {
    return this.invokeData('backups.inspect', params) as BackupInspection;
  }

  /**
   * `backups.restore`：恢复到**新的空目录**，绝不覆盖当前库。
   *
   * 返回的 `library_path` 直接交给 `DiaryCore.open()` 打开新库。
   */
  restoreBackup(params: RestoreBackupParams): RestoreReport {
    return this.invokeData('backups.restore', params) as RestoreReport;
  }

  // ---------------------------------------------------------------- 事件（契约第 6 节）

  /**
   * `core.events`：按序号**轮询**持久业务事件（C ABI 不是推送流）。
   *
   * 用法：省略 `from_sequence` 先拿到当前 `next_sequence`，之后每轮把上次的
   * `next_sequence` 原样传回来；`truncated === true` 时立刻再拉一页。
   *
   * 核心在每次写事务里就持久化了事件，所以**不需要**先订阅；本方法不建会话、
   * 不缓存，读到的就是库里真实的事件。
   */
  events(params: EventsParams | null = null): DomainEventPage {
    return this.invokeData('core.events', params) as DomainEventPage;
  }
  // ---------------------------------------------------------------- 日记（契约 4.3 / 2.5）
  //
  // 「日记」一级入口的数据源。这一组只透传核心的 `DiaryDay` / `DiaryVersion`：
  // 哪一版是当前、哪些是候选看 `DiaryDay`；版本本身没有状态字段。
  //
  // 空的一天核心报 `not_found`（这一天既没有材料也没有版本）——那是**空态**，
  // 界面显示「还没有成篇日记」，不要把它当读取失败。

  /**
   * `diary.getDay`：某一天的当前/候选与材料覆盖。
   *
   * 返回值的 `revision` 是 `saveDiaryEdit` / `setDiaryCurrent` / `mergeDiary` 的
   * `expected_revision`（原样回传，不猜）。
   */
  getDiaryDay(params: DiaryDayParams): DiaryDay {
    return this.invokeData('diary.getDay', params) as DiaryDay;
  }

  /**
   * `diary.listDays`：有哪些天、每天多少条材料（回收站不算），按 dayKey 倒序。
   *
   * 四个参数全可省（省略 = 全库第一页）。`cursor` 原样回传上一页的 `nextCursor`
   * （不透明、不要解析）；坏游标报 `cursor_expired`。注意响应外层的游标是
   * **`nextCursor`**（core 的 `DiaryDayPage` 是 camelCase），`days` 里的天仍是 snake_case。
   */
  listDiaryDays(params: ListDiaryDaysParams | null = null): DiaryDayPage {
    return this.invokeData('diary.listDays', params) as DiaryDayPage;
  }

  /**
   * `diary.listVersions`：分页读某一天的版本，最新在前。
   *
   * `cursor` 原样回传上一页的 `next_cursor`（不透明）；没有版本时是空页，不是错误。
   */
  listDiaryVersions(params: ListDiaryVersionsParams): DiaryVersionPage {
    return this.invokeData('diary.listVersions', params) as DiaryVersionPage;
  }

  /** `diary.getVersion`：读一条完整版本（含正文、生成信息与来源关联）。 */
  getDiaryVersion(params: DiaryVersionParams): DiaryVersion {
    return this.invokeData('diary.getVersion', params) as DiaryVersion;
  }

  /**
   * `diary.saveEdit`：人工编辑写成**新版本**（基版一个字节都不动），新版本成为当前。
   *
   * `expected_revision` 不符报 `revision_conflict`，核心零改动；空正文报
   * `invalid_argument`。同一条 `operation_id` 重放返回原版本。
   */
  saveDiaryEdit(params: SaveDiaryEditParams): DiaryVersion {
    return this.invokeData('diary.saveEdit', params) as DiaryVersion;
  }

  /**
   * `diary.setCurrent`：在一天的版本之间指定当前版本（例如退回上一版）。
   *
   * 旧当前版变候选、不删除；切到已经是当前的版本是无操作。`expected_revision`
   * 不符报 `revision_conflict`。
   */
  setDiaryCurrent(params: SetDiaryCurrentParams): DiaryDay {
    return this.invokeData('diary.setCurrent', params) as DiaryDay;
  }

  /**
   * `diary.merge`：把两版（或多版）合成一条新的 `merge` 版本。
   *
   * `body` 是**用户确认后的正文**：核心不替用户合并、不调模型（`modelCalls = 0`）。
   * 参与合并的版本全部保留，随时可取回。
   */
  mergeDiary(params: MergeDiaryParams): DiaryVersion {
    return this.invokeData('diary.merge', params) as DiaryVersion;
  }

  /**
   * `diary.generate`：受理一次生成——冻结这一天的输入、排一条 `diary_generate` 任务，
   * 返回 `DiaryGenerationTicket`（`job_id` / `source_snapshot_id`）。
   *
   * **返回票据 ≠ 已生成**：真正的模型调用在 `jobs.runDue` 里。没有可用服务时也照样
   * 返回票据，任务会停在 `waiting_configuration` 等配置（与 #66 的能力闸门同一口径），
   * 这不是一次调用失败，也不会写一版空正文冒充成功。
   *
   * **输入在返回前已冻结**：快照行不可变，事后改材料不影响这一版。候选/当前：首次
   * （当天没有当前版本、也没有用户自己的版本）生成的结果成为当前版本，其余一律候选；
   * 同快照哈希 + 同 `operation_id` 不重复产版本（`reason = 'regenerate'` 是例外）。
   */
  generateDiary(params: GenerateDiaryParams): DiaryGenerationTicket {
    return this.invokeData('diary.generate', params) as DiaryGenerationTicket;
  }

  // ---------------------------------------------------------------- 风格与阅读导出（契约 4.3 / 4.4）

  /**
   * `styles.list`：全部风格的**当前版本**，按展示名、id 固定排序。
   *
   * 默认风格 `sty_default` 也在里面（内容全空、`revision = 1`）——它是「没有偏好」的
   * 真实配置，不是查不出来的魔法值，也不是「界面该藏起来」的那一条。
   */
  listStyles(): StyleProfile[] {
    return this.invokeData('styles.list', null) as StyleProfile[];
  }

  /**
   * `styles.get`：读一条风格的当前版本。
   *
   * 不存在报 `not_found`——**不会静默回退默认风格**（回退会让界面以为用户设过偏好）。
   */
  getStyle(params: StyleIdParams): StyleProfile {
    return this.invokeData('styles.get', params) as StyleProfile;
  }

  /**
   * `styles.save`：新建/更新一条风格，**每次保存落一个新的不可变版本**（`revision` +1）。
   *
   * 带 `expected_revision` 时是乐观锁：不符报 `revision_conflict` 且零改动；省略表示
   * 明确接受覆盖当前版本。`revision` / `updatedAt` 由核心盖章（传了也不生效）。
   * 偏好变更**不自动重写历史日记**：历史版本的 `styleSnapshotId` 仍指向当时那一版。
   */
  saveStyle(params: SaveStyleParams): StyleProfile {
    return this.invokeData('styles.save', params) as StyleProfile;
  }

  /**
   * `exports.create`：把日期范围内的**当前日记正文**导成 Markdown / 纯文本。
   *
   * 同步完成：返回的 `jobId` 指向一条**已经 `succeeded`** 的任务记录，不是排队中的任务。
   * 产物落在资料库自己的 `exports/` 下（契约不给路径、这一层也不接受路径），重名自动加
   * 后缀、绝不覆盖。`restorable` 恒为 `false`——**这不是备份**，不能用于恢复资料库。
   *
   * `includeAssets = true` 会被核心明确拒绝（本构建只写正文，不随附原件）。
   */
  createExport(params: CreateExportParams): ExportTicket {
    return this.invokeData('exports.create', params) as ExportTicket;
  }

  // ---------------------------------------------------------------- 逃生口

  /**
   * 直接调一个 C ABI 方法（不校验方法名、不做类型声明），返回 `data`；`ok:false` 照样抛。
   *
   * 给「C ABI 已经接、但 ArkTS 侧还没包方法」的场合用，也方便在真机上手工试参数。
   */
  rawCall(method: string, params: Object | null = null): Object | null {
    this.ensureOpen();
    const request = buildRequest(method, params);
    let envelope = '';
    try {
      envelope = diaryNapi.call(this.handle, request);
    } catch (e) {
      throw DiaryError.fromThrown(e as Object);
    }
    return unwrapEnvelope(envelope);
  }

  /**
   * 只发请求、拿**原始信封字符串**（不解析、不抛业务错误）。
   *
   * 日志、回放、离线比对用；业务代码应该用上面的类型化方法。
   */
  rawEnvelope(method: string, params: Object | null = null): string {
    this.ensureOpen();
    const request = buildRequest(method, params);
    try {
      return diaryNapi.call(this.handle, request);
    } catch (e) {
      throw DiaryError.fromThrown(e as Object);
    }
  }

  // ---------------------------------------------------------------- 内部

  private ensureOpen(): void {
    if (this.closed) {
      throw new DiaryError('invalid_state', '资料库已关闭（DiaryCore.close 之后不能再调用）');
    }
  }

  /**
   * 可空返回（`Option<T>` → `null`）用：`ok:true` + `data:null` 是正常结果，
   * 不能像 [`invokeData`] 那样当成「信封形状变了」。
   */
  private invokeNullable(method: string, params: Object | null): Object | null {
    return this.rawCall(method, params);
  }

  private invokeData(method: string, params: Object | null): Object {
    const data = this.rawCall(method, params);
    if (data === null) {
      // 走 invokeData 的方法都有返回值（返回 null 的 acknowledgeAttention 走 rawCall）；
      // 走到这里说明 C ABI 的信封形状变了。
      throw new DiaryError(NATIVE_UNAVAILABLE, `核心返回 ok:true 但没有 data（${method}）`);
    }
    return data;
  }
}

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
  CancelJobParams,
  Capture,
  CaptureIdParams,
  CapturePage,
  CommitParams,
  CommitResult,
  CreateDraftParams,
  DraftSaveResult,
  ExtractedContent,
  ExtractParams,
  FinalizeRecordingParams,
  IndexStatus,
  IndexStatusParams,
  Job,
  JobIdParams,
  JobPage,
  JobRunSummary,
  JobStatus,
  LibraryInfo,
  ListCapturesParams,
  ListJobsParams,
  NativeRecordingStatusParams,
  PrepareRecordingParams,
  ProviderConfig,
  ProviderIdParams,
  ProviderTestReport,
  RebuildIndexParams,
  RebuildSemanticParams,
  RecoverRecordingParams,
  RecordingFinalizeResult,
  RecordingRecovery,
  RecordingSession,
  RecordingTicket,
  RegisterSegmentParams,
  RetryJobParams,
  RunDueParams,
  SaveDraftParams,
  SaveProviderParams,
  SearchNextPageParams,
  SearchParams,
  SearchSessionParams,
  SearchSnapshot,
  SegmentReceipt,
  TestProviderParams,
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

  // ---------------------------------------------------------------- 契约方法（30 条）
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

  /** `captures.commit`：草稿转正式记录，并生成原文修订（之后才能 `extract` / 检索到）。 */
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

  /** `providers.test`：对一条配置跑连接测试；本机引擎会真的加载模型并转写静音。 */
  testProvider(params: TestProviderParams): ProviderTestReport {
    return this.invokeData('providers.test', params) as ProviderTestReport;
  }

  /** `providers.remove`：只删配置行，不删任何日记/派生内容（核心返回 `null`）。 */
  removeProvider(params: ProviderIdParams): void {
    this.rawCall('providers.remove', params);
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

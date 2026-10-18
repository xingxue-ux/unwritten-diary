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
  Capture,
  CommitParams,
  CommitResult,
  CreateDraftParams,
  DraftSaveResult,
  ExtractedContent,
  ExtractParams,
  Job,
  JobRunSummary,
  JobStatus,
  LibraryInfo,
  RebuildIndexParams,
  RunDueParams,
  SaveDraftParams,
  SearchParams,
  SearchSnapshot,
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

  /** 核心 C ABI 的版本戳（`diary_capi_version()`），例如 `diary_capi/0.1.0 apiVersion=1.0`。 */
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

  // ---------------------------------------------------------------- 契约方法（9 条）
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

  /** `sources.extract`：抽取正文；`source_ref` 可以是来源 id 或修订 id。 */
  extract(params: ExtractParams): ExtractedContent {
    return this.invokeData('sources.extract', params) as ExtractedContent;
  }

  /** `search.start`：发起一次检索，返回**当前页的完整快照**。 */
  search(params: SearchParams): SearchSnapshot {
    return this.invokeData('search.start', params) as SearchSnapshot;
  }

  /** `indexes.rebuild`：只排任务，不同步建索引（真正跑要 `runDueJobs()`）。 */
  rebuildIndex(params: RebuildIndexParams): Job {
    return this.invokeData('indexes.rebuild', params) as Job;
  }

  /** `jobs.runDue`：把到期的任务跑一轮（预算 + 条数上限，见 C ABI 文档 §6.1）。 */
  runDueJobs(params: RunDueParams): JobRunSummary {
    return this.invokeData('jobs.runDue', params) as JobRunSummary;
  }

  /** `jobs.status`：队列计数、到期积压、最近错误。 */
  jobsStatus(): JobStatus {
    return this.invokeData('jobs.status', null) as JobStatus;
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
      // 9 条已接方法都有返回值；走到这里说明 C ABI 的信封形状变了。
      throw new DiaryError(NATIVE_UNAVAILABLE, `核心返回 ok:true 但没有 data（${method}）`);
    }
    return data;
  }
}

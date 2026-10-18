// 真机验证用的探针（#122 未验清单第 1–14 项）。
//
// 这个文件**只用于验证**，不是产品 API：
//   - bench       性能数字（open / coreInfo / JSON 往返 / search / jobs）；
//   - errors      错误路径（不存在路径、同路径两次、关闭后调用、非法 JSON、未知方法、
//                 revision_conflict、损坏库文件）；
//   - concurrency 两个 TaskPool worker + 主线程同时 call 同一个句柄；
//   - persist     升级/回滚后读回数据；
//   - diag        库文件/原生库大小、沙箱落盘自检。
//
// 所有日志的 tag 都是 `DiaryCore`，机器可读前缀 `DIARY_VERIFY`；长 JSON 按 900 字符分片，
// 与 Smoke.ets 的 `DIARY_SMOKE JSON[...]` 同一口径（拼接分片即可还原）。

import { hilog } from '@kit.PerformanceAnalysisKit';
import { systemDateTime } from '@kit.BasicServicesKit';
import { fileIo as fs } from '@kit.CoreFileKit';
import { process, taskpool } from '@kit.ArkTS';
import diaryNapi from 'libdiary_napi.so';
import { DiaryCore } from '../core/DiaryCore';
import { DiaryError } from '../core/DiaryError';
import { Capture, CommitResult, DraftSaveResult, ExtractParams, ExtractedContent, JobRunSummary, LibraryInfo, SearchSnapshot } from '../core/Models';
import { callSameHandle, openAndCall } from './ConcurrentWorker';

const DOMAIN: number = 0x0000;
const TAG: string = 'DiaryCore';
const MARK: string = 'DIARY_VERIFY';
const CHUNK: number = 900;

const INFO_REQUEST: string = '{"method":"core.info","params":null}';
const SEARCH_REQUEST: string =
  '{"method":"search.start","params":{"request":{"query":"benchneedle","page_size":10},"query_revision":1}}';

interface EnvelopeView {
  ok?: boolean;
  apiVersion?: string;
  error?: ErrorView;
}

interface ErrorView {
  code?: string;
  message?: string;
  retryable?: boolean;
}

interface BenchMetrics {
  db: string;
  capi_load_ms: number;
  open_cold_ms: number;
  open_warm_median_ms: number;
  open_warm_p95_ms: number;
  schema_version: number;
  core_info_n: number;
  core_info_median_ms: number;
  core_info_p95_ms: number;
  core_info_chars: number;
  json_small_chars: number;
  json_small_n: number;
  json_small_median_ms: number;
  json_medium_chars: number;
  json_medium_n: number;
  json_medium_median_ms: number;
  json_large_chars: number;
  json_large_n: number;
  json_large_median_ms: number;
  search_create_n: number;
  search_create_ms: number;
  search_first_ms: number;
  search_warm_median_ms: number;
  search_hits: number;
  jobs_first_ms: number;
  jobs_median_ms: number;
  jobs_processed: number;
  db_bytes: number;
  wal_bytes: number;
}

export class DeviceVerify {
  // ---------------------------------------------------------------- 入口

  static run(mode: string, filesDir: string): void {
    hilog.info(DOMAIN, TAG, '%{public}s MODE mode=%{public}s pid=%{public}d',
      MARK, mode, process.pid);
    try {
      if (mode === 'bench') {
        DeviceVerify.runBench(filesDir);
      } else if (mode === 'errors') {
        DeviceVerify.runErrors(filesDir);
      } else if (mode === 'concurrency') {
        DeviceVerify.runConcurrency(filesDir).catch((e: Object) => {
          DeviceVerify.logError('concurrency', e);
        });
      } else if (mode === 'persist') {
        DeviceVerify.runPersist(filesDir);
      } else if (mode === 'diag') {
        DeviceVerify.runDiag(filesDir);
      } else {
        hilog.error(DOMAIN, TAG, '%{public}s UNKNOWN_MODE mode=%{public}s', MARK, mode);
      }
    } catch (e) {
      DeviceVerify.logError(mode, e as Object);
    }
  }

  // ---------------------------------------------------------------- 性能

  static runBench(filesDir: string): void {
    const dbPath = `${filesDir}/bench-${Date.now()}.sqlite`;
    hilog.info(DOMAIN, TAG, '%{public}s BENCH START db=%{public}s', MARK, dbPath);
    DeviceVerify.logLibSizes();

    // 第一次调 shim 才会 dlopen libdiary_capi.so；version() 就是「加载 + dlsym」的计时点。
    const loadStart = DeviceVerify.nowMs();
    const version = DiaryCore.version();
    const capiLoadMs = DeviceVerify.nowMs() - loadStart;
    hilog.info(DOMAIN, TAG, '%{public}s BENCH capiLoad ms=%{public}s version=%{public}s',
      MARK, DeviceVerify.fmt(capiLoadMs), version);

    const openStart = DeviceVerify.nowMs();
    const core = DiaryCore.open(dbPath);
    const openColdMs = DeviceVerify.nowMs() - openStart;
    const info: LibraryInfo = core.coreInfo();
    hilog.info(DOMAIN, TAG, '%{public}s BENCH openCold ms=%{public}s schema=%{public}d',
      MARK, DeviceVerify.fmt(openColdMs), info.data_schema_version);

    // coreInfo：200 次，看中位数/95 分位。
    const infoTimes: number[] = [];
    let infoChars = 0;
    for (let i = 0; i < 200; i++) {
      const t = DeviceVerify.nowMs();
      const one: LibraryInfo = core.coreInfo();
      infoTimes.push(DeviceVerify.nowMs() - t);
      infoChars = JSON.stringify(one).length;
    }

    // 小 payload：core.info 原始信封（不经过 ArkTS 解析）。
    const smallTimes: number[] = [];
    let smallChars = 0;
    for (let i = 0; i < 200; i++) {
      const t = DeviceVerify.nowMs();
      const envelope = core.rawEnvelope('core.info', null);
      smallTimes.push(DeviceVerify.nowMs() - t);
      smallChars = envelope.length;
    }

    // 中 payload：8 KiB 正文的 extract，响应里带全文。
    const mediumRevision = DeviceVerify.makeCapture(core, 8 * 1024, 'bench-medium');
    const mediumParams: ExtractParams = { source_ref: mediumRevision };
    const mediumTimes: number[] = [];
    let mediumChars = 0;
    for (let i = 0; i < 100; i++) {
      const t = DeviceVerify.nowMs();
      const envelope = core.rawEnvelope('sources.extract', mediumParams);
      mediumTimes.push(DeviceVerify.nowMs() - t);
      mediumChars = envelope.length;
    }

    // 大 payload：512 KiB 正文的 extract。
    const largeRevision = DeviceVerify.makeCapture(core, 512 * 1024, 'bench-large');
    const largeParams: ExtractParams = { source_ref: largeRevision };
    const largeTimes: number[] = [];
    let largeChars = 0;
    for (let i = 0; i < 30; i++) {
      const t = DeviceVerify.nowMs();
      const envelope = core.rawEnvelope('sources.extract', largeParams);
      largeTimes.push(DeviceVerify.nowMs() - t);
      largeChars = envelope.length;
    }

    // 检索数据集：40 条带同一关键词的记录；先建索引，再量首屏与热态。
    const createStart = DeviceVerify.nowMs();
    const searchN = 40;
    for (let i = 0; i < searchN; i++) {
      DeviceVerify.makeCapture(core, 320, `bench-search-${i}`);
    }
    const searchCreateMs = DeviceVerify.nowMs() - createStart;
    const rebuild: string = DeviceVerify.scheduleRebuild(core, 'bench-search-pre');
    const preRun: JobRunSummary = core.runDueJobs({ budget_ms: 30000, trigger: 'manual' });
    hilog.info(DOMAIN, TAG, '%{public}s BENCH searchIndex job=%{public}s processed=%{public}d',
      MARK, rebuild, preRun.processed);

    const searchStart = DeviceVerify.nowMs();
    const firstSearch: SearchSnapshot = core.search({
      request: { query: 'benchneedle', page_size: 20 },
      query_revision: 1
    });
    const searchFirstMs = DeviceVerify.nowMs() - searchStart;
    const searchTimes: number[] = [];
    let searchHits = firstSearch.results.length;
    for (let i = 0; i < 30; i++) {
      const t = DeviceVerify.nowMs();
      const snap: SearchSnapshot = core.search({
        request: { query: 'benchneedle', page_size: 20 },
        query_revision: 1
      });
      searchTimes.push(DeviceVerify.nowMs() - t);
      searchHits = snap.results.length;
    }

    // jobs.runDue：每次先排一条重建任务，量「一轮驱动」的耗时（5 轮）。
    const jobTimes: number[] = [];
    let jobProcessed = 0;
    for (let i = 0; i < 5; i++) {
      DeviceVerify.scheduleRebuild(core, `bench-jobs-${i}`);
      const t = DeviceVerify.nowMs();
      const summary: JobRunSummary = core.runDueJobs({ budget_ms: 30000, trigger: 'manual' });
      jobTimes.push(DeviceVerify.nowMs() - t);
      jobProcessed = summary.processed;
    }
    const jobsFirstMs = jobTimes.length > 0 ? jobTimes[0] : 0;

    core.close();
    hilog.info(DOMAIN, TAG, '%{public}s BENCH CLOSED handles=%{public}d',
      MARK, DiaryCore.openHandleCount());

    // 热 open：同一个库反复 open/close（已存在，不再建表/迁移）。
    const warmOpenTimes: number[] = [];
    for (let i = 0; i < 20; i++) {
      const t = DeviceVerify.nowMs();
      const one = DiaryCore.open(dbPath);
      one.close();
      warmOpenTimes.push(DeviceVerify.nowMs() - t);
    }

    const dbBytes = DeviceVerify.fileBytes(dbPath);
    const walBytes = DeviceVerify.fileBytes(`${dbPath}-wal`);
    const metrics: BenchMetrics = {
      db: dbPath,
      capi_load_ms: DeviceVerify.round(capiLoadMs),
      open_cold_ms: DeviceVerify.round(openColdMs),
      open_warm_median_ms: DeviceVerify.round(DeviceVerify.median(warmOpenTimes)),
      open_warm_p95_ms: DeviceVerify.round(DeviceVerify.percentile(warmOpenTimes, 95)),
      schema_version: info.data_schema_version,
      core_info_n: infoTimes.length,
      core_info_median_ms: DeviceVerify.round(DeviceVerify.median(infoTimes)),
      core_info_p95_ms: DeviceVerify.round(DeviceVerify.percentile(infoTimes, 95)),
      core_info_chars: infoChars,
      json_small_chars: smallChars,
      json_small_n: smallTimes.length,
      json_small_median_ms: DeviceVerify.round(DeviceVerify.median(smallTimes)),
      json_medium_chars: mediumChars,
      json_medium_n: mediumTimes.length,
      json_medium_median_ms: DeviceVerify.round(DeviceVerify.median(mediumTimes)),
      json_large_chars: largeChars,
      json_large_n: largeTimes.length,
      json_large_median_ms: DeviceVerify.round(DeviceVerify.median(largeTimes)),
      search_create_n: searchN,
      search_create_ms: DeviceVerify.round(searchCreateMs),
      search_first_ms: DeviceVerify.round(searchFirstMs),
      search_warm_median_ms: DeviceVerify.round(DeviceVerify.median(searchTimes)),
      search_hits: searchHits,
      jobs_first_ms: DeviceVerify.round(jobsFirstMs),
      jobs_median_ms: DeviceVerify.round(DeviceVerify.median(jobTimes)),
      jobs_processed: jobProcessed,
      db_bytes: dbBytes,
      wal_bytes: walBytes
    };
    DeviceVerify.logJson('bench', metrics);
    DeviceVerify.logMetrics(metrics);
    DeviceVerify.reportDbStats(dbPath);
    hilog.info(DOMAIN, TAG, '%{public}s BENCH DONE', MARK);
  }

  // ---------------------------------------------------------------- 错误路径

  static runErrors(filesDir: string): void {
    const dbPath = `${filesDir}/library.sqlite`;
    hilog.info(DOMAIN, TAG, '%{public}s ERRORS START db=%{public}s handles=%{public}d',
      MARK, dbPath, DiaryCore.openHandleCount());

    DeviceVerify.probe('open-empty-path', () => {
      DiaryCore.open('');
    });

    DeviceVerify.probe('open-missing-parent', () => {
      DiaryCore.open(`${filesDir}/no-such-dir-${Date.now()}/library.sqlite`);
    });

    // 同路径打开两次：C ABI 共享同一个 Core；关掉一个，另一个还能用。
    let a: DiaryCore | null = null;
    let b: DiaryCore | null = null;
    try {
      a = DiaryCore.open(dbPath);
      b = DiaryCore.open(dbPath);
      hilog.info(DOMAIN, TAG, '%{public}s ERRCASE open-same-path-ok handles=%{public}d',
        MARK, DiaryCore.openHandleCount());
      const ia: LibraryInfo = a.coreInfo();
      const ib: LibraryInfo = b.coreInfo();
      hilog.info(DOMAIN, TAG, '%{public}s ERRCASE same-path library_id=%{public}s/%{public}s schema=%{public}d/%{public}d',
        MARK, ia.library_id, ib.library_id, ia.data_schema_version, ib.data_schema_version);
      a.close();
      a = null;
      const afterClose: LibraryInfo = b.coreInfo();
      hilog.info(DOMAIN, TAG, '%{public}s ERRCASE sibling-after-close schema=%{public}d handles=%{public}d',
        MARK, afterClose.data_schema_version, DiaryCore.openHandleCount());
      b.close();
      b = null;
      hilog.info(DOMAIN, TAG, '%{public}s ERRCASE both-closed handles=%{public}d',
        MARK, DiaryCore.openHandleCount());
    } finally {
      if (a !== null && a.isOpen()) {
        a.close();
      }
      if (b !== null && b.isOpen()) {
        b.close();
      }
    }

    // DiaryCore 关闭后再调：会被 ensureOpen() 拦成核心码 invalid_state。
    const closed = DiaryCore.open(dbPath);
    closed.close();
    DeviceVerify.probe('close-then-call', () => {
      closed.coreInfo();
    });

    // 直接走 shim：同一个已关闭的 id 再 call / 再 close。
    const rawHandle = diaryNapi.open(dbPath);
    diaryNapi.close(rawHandle);
    DeviceVerify.probe('native-call-closed-handle', () => {
      diaryNapi.call(rawHandle, INFO_REQUEST);
    });
    DeviceVerify.probe('native-close-twice', () => {
      diaryNapi.close(rawHandle);
    });

    // 非法 JSON / 未知方法 / 缺参数：业务错误不抛，返回 ok:false 信封。
    const raw = diaryNapi.open(dbPath);
    try {
      DeviceVerify.logEnvelope('invalid-json', diaryNapi.call(raw, '{not valid json'));
      DeviceVerify.logEnvelope('wrong-shape', diaryNapi.call(raw, '{"method":123}'));
      DeviceVerify.logEnvelope('unknown-method', diaryNapi.call(raw, '{"method":"no.such.method","params":null}'));
      DeviceVerify.logEnvelope('missing-params', diaryNapi.call(raw,
        '{"method":"captures.saveDraft","params":{"capture_id":"x"}}'));
    } finally {
      diaryNapi.close(raw);
    }

    // revision_conflict：第一次 save 后 revision=2，第二次仍拿 1 当 expected。
    const c = DiaryCore.open(dbPath);
    try {
      const created: Capture = c.createDraft({
        time_zone: 'Asia/Shanghai',
        utc_offset_minutes: 480,
        operation_id: `op-err-create-${Date.now()}`
      });
      const saved: DraftSaveResult = c.saveDraft({
        capture_id: created.id,
        text: '错误路径测试：revision_conflict',
        expected_revision: created.revision,
        operation_id: `op-err-save-${Date.now()}`
      });
      DeviceVerify.probe('revision-conflict', () => {
        c.saveDraft({
          capture_id: created.id,
          text: '第二次写，期望旧的 revision',
          expected_revision: created.revision,
          operation_id: `op-err-save2-${Date.now()}`
        });
      });
      hilog.info(DOMAIN, TAG, '%{public}s ERRCASE revision-conflict-context savedRevision=%{public}d durable=%{public}s',
        MARK, saved.revision, saved.durable ? 'true' : 'false');
    } finally {
      c.close();
    }

    // 损坏的库文件：写一段非 SQLite 文本，再 open。
    const corruptPath = `${filesDir}/corrupt-${Date.now()}.sqlite`;
    try {
      const file = fs.openSync(corruptPath, fs.OpenMode.CREATE | fs.OpenMode.READ_WRITE);
      fs.writeSync(file.fd, 'this file is definitely not a sqlite database');
      fs.closeSync(file);
      DeviceVerify.probe('open-corrupt-file', () => {
        const bad = DiaryCore.open(corruptPath);
        bad.close();
      });
    } catch (e) {
      DeviceVerify.logError('corrupt-setup', e as Object);
    }

    hilog.info(DOMAIN, TAG, '%{public}s ERRORS DONE handles=%{public}d',
      MARK, DiaryCore.openHandleCount());
  }

  // ---------------------------------------------------------------- 并发

  static async runConcurrency(filesDir: string): Promise<void> {
    const dbPath = `${filesDir}/library.sqlite`;
    hilog.info(DOMAIN, TAG, '%{public}s CONC START pid=%{public}d handles=%{public}d',
      MARK, process.pid, DiaryCore.openHandleCount());

    // 阶段 1：同一个句柄，两个 worker + 主线程同时 call。
    const handle = diaryNapi.open(dbPath);
    try {
      hilog.info(DOMAIN, TAG, '%{public}s CONC same-handle open id=%{public}d handles=%{public}d',
        MARK, handle, DiaryCore.openHandleCount());
      const t1 = new taskpool.Task(callSameHandle, handle, 'info', 800);
      const t2 = new taskpool.Task(callSameHandle, handle, 'search', 800);
      const p1 = taskpool.execute(t1);
      const p2 = taskpool.execute(t2);

      // 主线程也压 500 次（同一句柄），与两个 worker 重叠。
      const mainStart = Date.now();
      let mainOk = 0;
      let mainErr = 0;
      let mainLast = '';
      for (let i = 0; i < 500; i++) {
        try {
          const envelope = diaryNapi.call(handle, i % 2 === 0 ? INFO_REQUEST : SEARCH_REQUEST);
          if (envelope.indexOf('"ok":true') >= 0) {
            mainOk++;
          } else {
            mainErr++;
            if (mainLast.length < 200) {
              mainLast = envelope.substring(0, 200);
            }
          }
        } catch (e) {
          mainErr++;
          const err = e as Error;
          if (mainLast.length < 200) {
            mainLast = err.message;
          }
        }
      }
      const mainEnd = Date.now();
      const results: Object[] = await Promise.all([p1, p2]);
      hilog.info(DOMAIN, TAG, '%{public}s CONC same-handle main ok=%{public}d err=%{public}d spanMs=%{public}d last=%{public}s',
        MARK, mainOk, mainErr, mainEnd - mainStart, mainLast);
      for (let i = 0; i < results.length; i++) {
        hilog.info(DOMAIN, TAG, '%{public}s CONC same-handle worker[%{public}d] %{public}s',
          MARK, i, results[i] as string);
      }
    } finally {
      diaryNapi.close(handle);
      hilog.info(DOMAIN, TAG, '%{public}s CONC same-handle closed handles=%{public}d',
        MARK, DiaryCore.openHandleCount());
    }

    // 阶段 2：两个 worker 各自 open 同一路径（共享同一个 Core），轮询观察句柄数。
    hilog.info(DOMAIN, TAG, '%{public}s CONC own-handle start handles=%{public}d',
      MARK, DiaryCore.openHandleCount());
    const t3 = new taskpool.Task(openAndCall, dbPath, 'info', 1200);
    const t4 = new taskpool.Task(openAndCall, dbPath, 'search', 1200);
    const p3 = taskpool.execute(t3);
    const p4 = taskpool.execute(t4);
    let maxHandles = 0;
    for (let i = 0; i < 25; i++) {
      await DeviceVerify.sleep(20);
      maxHandles = Math.max(maxHandles, DiaryCore.openHandleCount());
    }
    const results2: Object[] = await Promise.all([p3, p4]);
    hilog.info(DOMAIN, TAG, '%{public}s CONC own-handle maxObservedHandles=%{public}d finalHandles=%{public}d',
      MARK, maxHandles, DiaryCore.openHandleCount());
    for (let i = 0; i < results2.length; i++) {
      hilog.info(DOMAIN, TAG, '%{public}s CONC own-handle worker[%{public}d] %{public}s',
        MARK, i, results2[i] as string);
    }
    hilog.info(DOMAIN, TAG, '%{public}s CONC DONE handles=%{public}d',
      MARK, DiaryCore.openHandleCount());
  }

  // ---------------------------------------------------------------- 升级后读回

  static runPersist(filesDir: string): void {
    const dbPath = `${filesDir}/library.sqlite`;
    const core = DiaryCore.open(dbPath);
    try {
      const info: LibraryInfo = core.coreInfo();
      const snap: SearchSnapshot = core.search({
        request: { query: '鹅掌楸', page_size: 10 },
        query_revision: 1
      });
      hilog.info(DOMAIN, TAG, '%{public}s PERSIST schema=%{public}d captures=%{public}d library=%{public}s hits=%{public}d dbBytes=%{public}d',
        MARK, info.data_schema_version, info.capture_count, info.library_id,
        snap.results.length, DeviceVerify.fileBytes(dbPath));
      DeviceVerify.logJson('persist.coreInfo', info);
      DeviceVerify.logJson('persist.search', snap);
      DeviceVerify.reportDbStats(dbPath);
    } finally {
      core.close();
      hilog.info(DOMAIN, TAG, '%{public}s PERSIST CLOSED handles=%{public}d',
        MARK, DiaryCore.openHandleCount());
    }
  }

  static runDiag(filesDir: string): void {
    DeviceVerify.logLibSizes();
    const dbPath = `${filesDir}/library.sqlite`;
    const core = DiaryCore.open(dbPath);
    try {
      const info: LibraryInfo = core.coreInfo();
      hilog.info(DOMAIN, TAG, '%{public}s DIAG schema=%{public}d captures=%{public}d pid=%{public}d nativeDir=%{public}s',
        MARK, info.data_schema_version, info.capture_count, process.pid, DiaryCore.nativeLibraryDir());
      DeviceVerify.reportDbStats(dbPath);
    } finally {
      core.close();
      hilog.info(DOMAIN, TAG, '%{public}s DIAG CLOSED handles=%{public}d',
        MARK, DiaryCore.openHandleCount());
    }
  }

  // ---------------------------------------------------------------- 小工具

  static logLibSizes(): void {
    const dir = DiaryCore.nativeLibraryDir();
    const names: string[] = ['libdiary_capi.so', 'libdiary_napi.so', 'libc++_shared.so'];
    for (let i = 0; i < names.length; i++) {
      const path = dir.endsWith('/') ? `${dir}${names[i]}` : `${dir}/${names[i]}`;
      try {
        const size = DeviceVerify.fileBytes(path);
        hilog.info(DOMAIN, TAG, '%{public}s LIBS %{public}s bytes=%{public}d',
          MARK, names[i], size);
      } catch (e) {
        DeviceVerify.logError(`stat-${names[i]}`, e as Object);
      }
    }
  }

  static reportDbStats(dbPath: string): void {
    const db = DeviceVerify.fileBytes(dbPath);
    const wal = DeviceVerify.fileBytes(`${dbPath}-wal`);
    const shm = DeviceVerify.fileBytes(`${dbPath}-shm`);
    hilog.info(DOMAIN, TAG, '%{public}s DBSTAT path=%{public}s bytes=%{public}d wal=%{public}d shm=%{public}d',
      MARK, dbPath, db, wal, shm);
  }

  private static makeCapture(core: DiaryCore, size: number, tag: string): string {
    const text = DeviceVerify.filler(size, tag);
    const created: Capture = core.createDraft({
      time_zone: 'Asia/Shanghai',
      utc_offset_minutes: 480,
      operation_id: `op-${tag}-create`
    });
    const saved: DraftSaveResult = core.saveDraft({
      capture_id: created.id,
      text: text,
      expected_revision: created.revision,
      operation_id: `op-${tag}-save`
    });
    const committed: CommitResult = core.commit({
      capture_id: created.id,
      expected_revision: saved.revision,
      operation_id: `op-${tag}-commit`
    });
    if (committed.original_text_revision === null) {
      throw new DiaryError('invalid_state', `commit 后没有原文修订（${tag}）`);
    }
    return committed.original_text_revision.revision_id;
  }

  private static scheduleRebuild(core: DiaryCore, tag: string): string {
    const job = core.rebuildIndex({
      scope: 'all',
      reason: 'device verify',
      operation_id: `op-${tag}`
    });
    return job.id;
  }

  private static filler(size: number, tag: string): string {
    const chunk = `benchneedle ${tag} 鹅掌楸 benchmark payload. `;
    let text = '';
    while (text.length < size) {
      text += chunk;
    }
    return text.substring(0, size);
  }

  private static probe(label: string, action: () => void): void {
    try {
      action();
      hilog.error(DOMAIN, TAG, '%{public}s ERRCASE %{public}s UNEXPECTED_SUCCESS', MARK, label);
    } catch (e) {
      hilog.info(DOMAIN, TAG, '%{public}s ERRCASE %{public}s %{public}s',
        MARK, label, DeviceVerify.describeError(e as Object));
    }
  }

  private static logEnvelope(label: string, envelope: string): void {
    try {
      const parsed = JSON.parse(envelope) as EnvelopeView;
      const code = parsed.error !== undefined && parsed.error !== null && parsed.error.code !== undefined
        ? parsed.error.code : '(none)';
      hilog.info(DOMAIN, TAG, '%{public}s ERRCASE %{public}s envelope ok=%{public}s code=%{public}s chars=%{public}d',
        MARK, label, parsed.ok === true ? 'true' : 'false', code, envelope.length);
    } catch (e) {
      hilog.error(DOMAIN, TAG, '%{public}s ERRCASE %{public}s BAD_ENVELOPE %{public}s',
        MARK, label, envelope.substring(0, 200));
    }
  }

  private static describeError(raw: Object): string {
    const err = raw instanceof DiaryError ? raw : DiaryError.fromThrown(raw);
    return `code=${err.code} retryable=${err.retryable ? 'true' : 'false'} message=${err.message}`;
  }

  private static logError(where: string, raw: Object): void {
    const err = raw instanceof DiaryError ? raw : DiaryError.fromThrown(raw);
    hilog.error(DOMAIN, TAG, '%{public}s ERROR[%{public}s] %{public}s',
      MARK, where, err.toLogLine());
  }

  private static logJson(label: string, value: Object): void {
    const text = JSON.stringify(value);
    const chunks = Math.max(1, Math.ceil(text.length / CHUNK));
    for (let i = 0; i < chunks; i++) {
      hilog.info(DOMAIN, TAG, '%{public}s JSON[%{public}s][%{public}d/%{public}d] %{public}s',
        MARK, label, i + 1, chunks, text.substring(i * CHUNK, (i + 1) * CHUNK));
    }
  }

  private static logMetrics(m: BenchMetrics): void {
    hilog.info(DOMAIN, TAG, '%{public}s BENCH capiLoadMs=%{public}s', MARK, DeviceVerify.fmt(m.capi_load_ms));
    hilog.info(DOMAIN, TAG, '%{public}s BENCH openColdMs=%{public}s openWarmMedianMs=%{public}s openWarmP95Ms=%{public}s',
      MARK, DeviceVerify.fmt(m.open_cold_ms), DeviceVerify.fmt(m.open_warm_median_ms),
      DeviceVerify.fmt(m.open_warm_p95_ms));
    hilog.info(DOMAIN, TAG, '%{public}s BENCH coreInfo n=%{public}d medianMs=%{public}s p95Ms=%{public}s chars=%{public}d',
      MARK, m.core_info_n, DeviceVerify.fmt(m.core_info_median_ms),
      DeviceVerify.fmt(m.core_info_p95_ms), m.core_info_chars);
    hilog.info(DOMAIN, TAG, '%{public}s BENCH jsonSmall chars=%{public}d n=%{public}d medianMs=%{public}s',
      MARK, m.json_small_chars, m.json_small_n, DeviceVerify.fmt(m.json_small_median_ms));
    hilog.info(DOMAIN, TAG, '%{public}s BENCH jsonMedium chars=%{public}d n=%{public}d medianMs=%{public}s',
      MARK, m.json_medium_chars, m.json_medium_n, DeviceVerify.fmt(m.json_medium_median_ms));
    hilog.info(DOMAIN, TAG, '%{public}s BENCH jsonLarge chars=%{public}d n=%{public}d medianMs=%{public}s',
      MARK, m.json_large_chars, m.json_large_n, DeviceVerify.fmt(m.json_large_median_ms));
    hilog.info(DOMAIN, TAG, '%{public}s BENCH search firstMs=%{public}s warmMedianMs=%{public}s hits=%{public}d createMs=%{public}s n=%{public}d',
      MARK, DeviceVerify.fmt(m.search_first_ms), DeviceVerify.fmt(m.search_warm_median_ms),
      m.search_hits, DeviceVerify.fmt(m.search_create_ms), m.search_create_n);
    hilog.info(DOMAIN, TAG, '%{public}s BENCH jobs firstMs=%{public}s medianMs=%{public}s processed=%{public}d',
      MARK, DeviceVerify.fmt(m.jobs_first_ms), DeviceVerify.fmt(m.jobs_median_ms), m.jobs_processed);
  }

  private static fileBytes(path: string): number {
    try {
      const st = fs.statSync(path);
      return st.size;
    } catch (e) {
      return 0;
    }
  }

  private static nowMs(): number {
    return systemDateTime.getUptime(systemDateTime.TimeType.STARTUP, true) / 1000000;
  }

  private static round(value: number): number {
    return Math.round(value * 1000) / 1000;
  }

  private static fmt(value: number): string {
    return value.toFixed(3);
  }

  private static median(values: number[]): number {
    if (values.length === 0) {
      return 0;
    }
    const sorted = values.slice();
    sorted.sort((a: number, b: number): number => a - b);
    const mid = Math.floor(sorted.length / 2);
    if (sorted.length % 2 === 0) {
      return (sorted[mid - 1] + sorted[mid]) / 2;
    }
    return sorted[mid];
  }

  private static percentile(values: number[], p: number): number {
    if (values.length === 0) {
      return 0;
    }
    const sorted = values.slice();
    sorted.sort((a: number, b: number): number => a - b);
    const index = Math.min(sorted.length - 1, Math.ceil((p / 100) * sorted.length) - 1);
    return sorted[Math.max(0, index)];
  }

  private static sleep(ms: number): Promise<void> {
    return new Promise<void>((resolve: () => void) => {
      setTimeout(() => {
        resolve();
      }, ms);
    });
  }
}

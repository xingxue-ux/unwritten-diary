// 日记生成真机探针的 TaskPool worker：把「造等待任务 → 保存服务 → 真出网测试 → 跑队列」
// 全部搬离主线程。
//
// 为什么要 worker：`providers.test` 会真的发一次 HTTPS 请求（DeepSeek），
// `jobs.runDue` 里的 `diary_generate` 会同步调用模型（C ABI 目前只有同步接口），
// 两条都可能阻塞数秒到数分钟。放在 UI 主线程上会触发 AppFreeze；TaskPool 的另一个
// 线程重新 `DiaryCore.open` 同一个库即可——C ABI 的句柄是进程级共享的
// （`Arc<Mutex<Core>>` + 句柄表），与 `AsrWorker` 同一条路。
//
// 为什么分两轮 runDue：
//   设备上原来那两条 `diary_generate` 任务在第一次尝试时被产品界面的前台驱动
//   （`pages/Index.onPageShow` 的 `kickDueJobs`）抢先跑掉了，探针自己那一轮
//   `processed=0`。为了留下「waiting_configuration → succeeded」的干净证据，
//   这里用产品自己的自动整理开关造一条**新的**等待任务：先在没有生成服务时
//   `runDue`（任务落成 waiting_configuration），再保存/测试服务，第二次 `runDue`
//   领取并生成。自动整理设置随后**恢复原值**（默认关、22:30）。
//
// 安全：`secret` 只作为参数在本进程内传给核心的 `providers.save`，worker **不打日志**，
// 返回值里也只有 `credential_ref`（核心的引用），没有明文。

import { hidebug } from '@kit.PerformanceAnalysisKit';
import { DiaryCore } from '../core/DiaryCore';
import { DiaryError } from '../core/DiaryError';
import {
  Job,
  JobRunSummary,
  JobStatus,
  ProviderConfig,
  ProviderTestReport,
  Settings,
} from '../core/Models';
import {
  diaryGenJobs,
  removeProviderQuietly,
  restoreOrganizeSettings,
  writeOrganizeSettings,
} from './DiaryGenSupport';

/** 一份远端服务配置（由 `deepseek.json` 下载后解析，字段见探针说明）。 */
export interface GenRemoteConfig {
  endpoint: string;
  model: string;
  secret: string;
  provider_id: string;
}

/** worker 回给主线程的测量结果；字段名与 hilog 里的 JSON 一致。 */
export interface GenRemoteReport {
  /** 走到哪一步：open / remove / settings / schedule / save / test / jobs / done。 */
  phase: string;
  /** 失败原文（成功时为空串）。 */
  error: string;
  /** 失败的核心错误码（成功时为空串）。 */
  error_code: string;
  open_ms: number;
  settings_ms: number;
  save_ms: number;
  test_ms: number;
  run1_ms: number;
  run2_ms: number;
  pss_before_kb: number;
  pss_after_kb: number;
  provider: ProviderConfig | null;
  test: ProviderTestReport | null;
  settings_original: Settings | null;
  settings_restored: boolean;
  summary1: JobRunSummary | null;
  summary2: JobRunSummary | null;
  status: JobStatus | null;
  /** 第一轮之后（应当有一条 waiting_configuration 的新任务）。 */
  jobs_before: Job[];
  /** 第二轮之后（同一条任务应当 succeeded）。 */
  jobs_after: Job[];
}

@Concurrent
export function runGenRemote(
  dbPath: string,
  providerId: string,
  endpoint: string,
  model: string,
  secret: string,
  budgetMs: number,
  trigger: string,
): string {
  const report: GenRemoteReport = {
    phase: 'open',
    error: '',
    error_code: '',
    open_ms: 0,
    settings_ms: 0,
    save_ms: 0,
    test_ms: 0,
    run1_ms: 0,
    run2_ms: 0,
    pss_before_kb: Number(hidebug.getPss()),
    pss_after_kb: 0,
    provider: null,
    test: null,
    settings_original: null,
    settings_restored: false,
    summary1: null,
    summary2: null,
    status: null,
    jobs_before: [],
    jobs_after: [],
  };

  let core: DiaryCore | null = null;
  let settingsTouched = false;
  const openStart = Date.now();
  try {
    core = DiaryCore.open(dbPath);
    report.open_ms = Date.now() - openStart;

    // 1. 清掉上一次探针留下的服务行：没有生成服务，新任务才会 waiting_configuration。
    report.phase = 'remove';
    removeProviderQuietly(core, providerId);

    // 2. 临时打开自动整理、把计划时刻放到今天 00:00（只用来造一条等待任务）。
    report.phase = 'settings';
    const settingsStart = Date.now();
    report.settings_original = core.getSettings();
    writeOrganizeSettings(core, report.settings_original, true, '00:00');
    settingsTouched = true;
    report.settings_ms = Date.now() - settingsStart;

    // 3. 第一轮：排期 + 配置闸门把新任务停在 waiting_configuration。
    report.phase = 'schedule';
    const run1Start = Date.now();
    report.summary1 = core.runDueJobs({ budget_ms: 30000, trigger: 'gen_schedule' });
    report.run1_ms = Date.now() - run1Start;
    report.jobs_before = diaryGenJobs(core);

    // 4. 马上恢复自动整理，后续队列不再排新任务（恢复失败会写进 report）。
    report.settings_restored = restoreOrganizeSettings(core, report.settings_original);
    settingsTouched = !report.settings_restored;

    // 5. 保存服务 + 真出网连接测试。
    report.phase = 'save';
    const saveStart = Date.now();
    report.provider = core.saveProvider({
      config: {
        id: providerId,
        label: 'DeepSeek 生成（真机探针）',
        adapter_id: 'openai_compatible',
        endpoint: endpoint,
        model: model,
        capabilities: ['generate_text'],
        // 明文只走 `secret`；配置里的引用由核心在 save 时生成并返回。
        credential_ref: null,
        enabled: true,
        test_status: 'untested',
      },
      secret: secret,
    });
    report.save_ms = Date.now() - saveStart;

    report.phase = 'test';
    const testStart = Date.now();
    report.test = core.testProvider({
      provider_id: providerId,
      scope: ['generate_text'],
    });
    report.test_ms = Date.now() - testStart;

    // 6. 第二轮：领取 waiting_configuration 的任务并真的生成。
    report.phase = 'jobs';
    const run2Start = Date.now();
    try {
      report.summary2 = core.runDueJobs({ budget_ms: budgetMs, trigger: trigger });
    } finally {
      report.run2_ms = Date.now() - run2Start;
    }
    report.status = core.jobsStatus();
    report.jobs_after = diaryGenJobs(core);
    report.phase = 'done';
  } catch (e) {
    const err = e as DiaryError;
    report.error = err.message !== undefined ? err.message : JSON.stringify(e as Object);
    report.error_code = err.code !== undefined ? err.code : '';
  } finally {
    // 设置被改过就一定要恢复：默认关是产品口径，不能把设备留在「开着自动整理」。
    if (core !== null && settingsTouched && report.settings_original !== null) {
      try {
        report.settings_restored = restoreOrganizeSettings(core, report.settings_original);
      } catch (e) {
        report.settings_restored = false;
      }
    }
    if (core !== null) {
      try {
        core.close();
      } catch (e) {
        if (report.error.length === 0) {
          report.error = `close: ${(e as Error).message}`;
        }
      }
    }
  }

  report.pss_after_kb = Number(hidebug.getPss());
  return JSON.stringify(report);
}

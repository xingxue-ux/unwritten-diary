// 转写验证的 TaskPool worker：把「跑一轮任务」搬离主线程。
//
// 为什么需要它：`jobs.runDue` 在真机上要**同步**加载 228 MiB 的 ONNX 模型并跑
// SenseVoice（OHOS 的 C API 目前只有同步接口）。放在主线程上就是一次几秒到几十秒的
// 阻塞，会触发 AppFreeze/ANR。DeviceVerify 的并发验证已经证明 C ABI 的句柄可以跨
// 线程调用（`Arc<Mutex<Core>>` + 进程内句柄表），所以 worker 重新 `open` 同一个库
// 文件即可——拿到的是同一个 Core，事件顺序由核心的互斥锁保证。
//
// 返回一条 JSON（TaskPool 只回传可序列化的值）：耗时、内存、任务摘要与队列状态。
// 日志仍由主线程打印，worker 不直接写 hilog（避免两个线程的日志交织）。

import { hidebug } from '@kit.PerformanceAnalysisKit';
import { DiaryCore } from '../core/DiaryCore';
import { JobRunSummary, JobStatus } from '../core/Models';

/** 一次转写运行的测量结果；字段名与 hilog 里的 JSON 一致。 */
export interface AsrJobsReport {
  /** 从 open 到拿到句柄的毫秒数（冷/热打开）。 */
  open_ms: number;
  /** `runDueJobs` 本身的毫秒数（含首次模型加载）。 */
  run_ms: number;
  /** 进程 PSS（KiB）；前后各一次，用来量模型/推理占用的增量。 */
  pss_before_kb: number;
  pss_after_kb: number;
  /** Private_Dirty（KiB）：比 PSS 更能说明「这块内存是本进程独有的」。 */
  private_before_kb: number;
  private_after_kb: number;
  /** 任务摘要；`runDueJobs` 失败时为空。 */
  summary: JobRunSummary | null;
  /** 运行后的队列计数。 */
  status: JobStatus | null;
  /** 失败原文（成功时为空串）。 */
  error: string;
}

@Concurrent
export function runAsrJobs(dbPath: string, budgetMs: number, trigger: string): string {
  const report: AsrJobsReport = {
    open_ms: 0,
    run_ms: 0,
    pss_before_kb: Number(hidebug.getPss()),
    pss_after_kb: 0,
    private_before_kb: Number(hidebug.getPrivateDirty()),
    private_after_kb: 0,
    summary: null,
    status: null,
    error: '',
  };

  let core: DiaryCore | null = null;
  const openStart = Date.now();
  try {
    core = DiaryCore.open(dbPath);
    report.open_ms = Date.now() - openStart;

    const runStart = Date.now();
    report.summary = core.runDueJobs({ budget_ms: budgetMs, trigger: trigger });
    report.run_ms = Date.now() - runStart;
    report.status = core.jobsStatus();
  } catch (e) {
    report.error = (e as Error).message;
    report.run_ms = Date.now() - openStart;
  } finally {
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
  report.private_after_kb = Number(hidebug.getPrivateDirty());
  return JSON.stringify(report);
}

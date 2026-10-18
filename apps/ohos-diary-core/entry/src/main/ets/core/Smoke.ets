// 最小示例：打开库 → coreInfo → 建草稿 → 保存 → 提交 → 抽取 → 检索 → 排索引任务 →
// 跑一轮任务 → 关闭，每一步的结果打到 hilog。
//
// 为什么把步骤放在独立的类里而不是塞进 EntryAbility：示例要能同时被「Ability 启动时跑一次」
// 和「页面按钮点一下再跑」调用，Ability 只负责触发。
//
// ⚠️ 这段是**同步**的，会阻塞调用线程（见 DiaryCore.ets 顶部）。示例里步骤很少、库是新建的，
// 放到 Ability 启动时跑可以接受；产品里应当挪到 TaskPool worker。
//
// ⚠️ 真机没连着（hdc list targets 为空），本示例**没有在设备上运行过**——
// 它只做到「能编、逻辑与 C ABI 的 9 条方法对得上」。真机验证清单见
// docs/architecture/m6-ArkTS-NAPI.md §9。

import { hilog } from '@kit.PerformanceAnalysisKit';
import { DiaryCore } from './DiaryCore';
import { DiaryError } from './DiaryError';
import {
  Capture,
  CommitResult,
  DraftSaveResult,
  ExtractedContent,
  Job,
  JobRunSummary,
  JobStatus,
  LibraryInfo,
  SearchSnapshot,
} from './Models';

const DOMAIN: number = 0x0000;
/** 真机抓法：`hdc shell "hilog -x" | grep DiaryCore`（或 `hilog -T DiaryCore`）。 */
const TAG: string = 'DiaryCore';
/** 机器可读标记，便于 grep / CI 断言。 */
const MARK: string = 'DIARY_SMOKE';
/** hilog 单行长度有限，长 JSON 分片打，前缀 `JSON[label][i/n]`，拼接即可还原。 */
const CHUNK: number = 900;

export class DiaryCoreSmoke {
  /**
   * 跑一遍完整链路。抛出的 `DiaryError` 由本方法自己打日志，不向外抛——
   * 示例挂掉不应该把 Ability 一起带崩。
   */
  static run(filesDir: string): void {
    const dbPath = `${filesDir}/library.sqlite`;
    hilog.info(DOMAIN, TAG, '%{public}s START db=%{public}s', MARK, dbPath);

    // ① 先证明「原生层本身」是通的：库在哪个目录、C ABI 版本戳是什么。
    //    这两句失败（native_unavailable）说明 HAP 里少了 .so 或 libc++_shared.so。
    try {
      hilog.info(DOMAIN, TAG, '%{public}s nativeLibraryDir=%{public}s',
        MARK, DiaryCore.nativeLibraryDir());
      hilog.info(DOMAIN, TAG, '%{public}s capiVersion=%{public}s', MARK, DiaryCore.version());
    } catch (e) {
      DiaryCoreSmoke.logError('native', e as Object);
    }

    let core: DiaryCore | null = null;
    try {
      // ② 打开（不存在就创建 + 迁移）。
      core = DiaryCore.open(dbPath);
      hilog.info(DOMAIN, TAG, '%{public}s open OK handles=%{public}d',
        MARK, DiaryCore.openHandleCount());

      // ③ 核心信息 + 能力清单：`capabilities` 就是这一版 C ABI 真正接了的方法。
      const info: LibraryInfo = core.coreInfo();
      DiaryCoreSmoke.logJson('core.info', info);
      hilog.info(DOMAIN, TAG, '%{public}s capabilities=%{public}s',
        MARK, info.capabilities.join(','));

      // ④ 建草稿。核心的 createDraft 只接收时间/时区，正文在 saveDraft 里给。
      const created: Capture = core.createDraft({
        time_zone: 'Asia/Shanghai',
        utc_offset_minutes: 480,
        operation_id: 'op-arkts-draft',
      });
      DiaryCoreSmoke.logJson('createDraft', created);

      // ⑤ 写正文。durable=true 才表示落盘（WAL + 事务）。
      const saved: DraftSaveResult = core.saveDraft({
        capture_id: created.id,
        text: '在鸿蒙上通过 ArkTS 调到了核心：关键词是鹅掌楸与检索。',
        expected_revision: created.revision,
        operation_id: 'op-arkts-save',
      });
      DiaryCoreSmoke.logJson('saveDraft', saved);

      // ⑥ 提交：草稿没有来源，不提交就没有 revision_id 可抽取（C ABI 文档 §6.1 的理由）。
      const committed: CommitResult = core.commit({
        capture_id: created.id,
        expected_revision: saved.revision,
        operation_id: 'op-arkts-commit',
      });
      DiaryCoreSmoke.logJson('commit', committed);

      // ⑦ 抽取正文。
      const revision = committed.original_text_revision;
      if (revision === null) {
        throw new DiaryError('invalid_state', '提交后没有原文修订，无法抽取');
      }
      const extracted: ExtractedContent = core.extract({ source_ref: revision.revision_id });
      DiaryCoreSmoke.logJson('extract', extracted);

      // ⑧ 检索（keyword 模式，默认分页 20）。
      const snapshot: SearchSnapshot = core.search({
        request: { query: '鹅掌楸', page_size: 10 },
        query_revision: 1,
      });
      DiaryCoreSmoke.logJson('search', snapshot);
      hilog.info(DOMAIN, TAG, '%{public}s search hits=%{public}d phase=%{public}s',
        MARK, snapshot.results.length, snapshot.phase);

      // ⑨ 排一个索引重建任务，再跑一轮到期任务。
      const job: Job = core.rebuildIndex({
        scope: 'all',
        reason: 'arkts smoke',
        operation_id: 'op-arkts-index',
      });
      DiaryCoreSmoke.logJson('rebuildIndex', job);

      const before: JobStatus = core.jobsStatus();
      DiaryCoreSmoke.logJson('jobs.status.before', before);
      const run: JobRunSummary = core.runDueJobs({ budget_ms: 30000, trigger: 'manual' });
      DiaryCoreSmoke.logJson('jobs.runDue', run);
      const after: JobStatus = core.jobsStatus();
      DiaryCoreSmoke.logJson('jobs.status.after', after);

      hilog.info(DOMAIN, TAG, '%{public}s DONE processed=%{public}d succeeded=%{public}d',
        MARK, run.processed, run.succeeded);
    } catch (e) {
      DiaryCoreSmoke.logError('run', e as Object);
    } finally {
      // ⑩ 无论成败都关掉；同路径的其它句柄不受影响。
      if (core !== null) {
        try {
          core.close();
          hilog.info(DOMAIN, TAG, '%{public}s CLOSED handles=%{public}d',
            MARK, DiaryCore.openHandleCount());
        } catch (e) {
          DiaryCoreSmoke.logError('close', e as Object);
        }
      }
    }
  }

  /** 把一条 JSON 结果分片打到 hilog。 */
  private static logJson(label: string, value: Object): void {
    const text = JSON.stringify(value);
    const chunks = Math.max(1, Math.ceil(text.length / CHUNK));
    for (let i = 0; i < chunks; i++) {
      hilog.info(DOMAIN, TAG, '%{public}s JSON[%{public}s][%{public}d/%{public}d] %{public}s',
        MARK, label, i + 1, chunks, text.substring(i * CHUNK, (i + 1) * CHUNK));
    }
  }

  /**
   * 错误日志：`DiaryError` 打出 code/retryable/message；别的异常按原生层失败记。
   *
   * 真机上最常见的三条：
   * - `native_unavailable` + `ERR_DIARY_LIB`：`.so` 没打进包 / ABI 目录不对；
   * - `native_unavailable` + `ERR_DIARY_HANDLE`：句柄已关闭还在用（本示例不会）；
   * - `invalid_argument`：参数拼错（C ABI 会列出可用字段名）。
   */
  private static logError(where: string, raw: Object): void {
    if (raw instanceof DiaryError) {
      hilog.error(DOMAIN, TAG, '%{public}s ERROR[%{public}s] %{public}s',
        MARK, where, raw.toLogLine());
      return;
    }
    const err = DiaryError.fromThrown(raw);
    hilog.error(DOMAIN, TAG, '%{public}s ERROR[%{public}s] %{public}s',
      MARK, where, err.toLogLine());
  }
}

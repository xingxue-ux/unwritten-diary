// 转写链真机探针（#108 转写未验项）。
//
// 只用于验证，不是产品 API。入口（`aa start ... --ps verify_mode <mode>`）：
//
//   asr_setup   建沙箱模型目录（0777，便于 hdc 推模型）、保存本机引擎的 provider 配置
//   asr_test    对该 provider 跑连接测试（会真的加载模型并转写 0.05 秒静音，不联网）
//   asr_jobs    在 worker 里跑一轮 jobs.runDue，打印耗时/内存/摘要/队列
//   asr_search  以 `--ps query=<词>` 跑 search.start，打印命中（验证转写文本可检索）
//   asr_remove  删掉本探针保存的 provider 行（连接测试失败后重新配置用）
//
// 为什么要有 `asr_setup` 这一步：设备的应用沙箱 `/data/app/el2/.../files` 是
// `drwxrwx--- <app uid>`，`hdc file send`（shell uid）写不进去。让应用自己建一个
// 0777 的子目录，模型就能用 `hdc file send` 直接推到大文件该在的位置——不必把
// 239 MB 打进 HAP，也不必让 shell 去猜应用沙箱的权限。
//
// 日志 tag 是 `DiaryAsr`，机器可读前缀 `DIARY_ASR`；长 JSON 与录音探针同口径按
// 900 字符分片。

import { common } from '@kit.AbilityKit';
import { fileIo as fs } from '@kit.CoreFileKit';
import fileIO from '@ohos.fileio';
import { hilog } from '@kit.PerformanceAnalysisKit';
import { taskpool } from '@kit.ArkTS';
import { DiaryCore } from '../core/DiaryCore';
import { SearchParams, SearchSnapshot } from '../core/Models';
import { AsrJobsReport, runAsrJobs } from './AsrWorker';

const DOMAIN: number = 0x0000;
const TAG: string = 'DiaryAsr';
const MARK: string = 'DIARY_ASR';
const CHUNK: number = 900;

/** 探针保存的 provider id：与产品内置的 `builtin_local_asr` 分开，免得删配置时误删内置行。 */
const PROVIDER_ID: string = 'local_asr_ohos';
const PROVIDER_LABEL: string = '本机转写（OHOS 沙箱模型）';

/** `providers.save` 的 `config`：字段名是核心 `Provider` 的 snake_case。 */
interface AsrProviderConfig {
  id: string;
  label: string;
  adapter_id: string;
  endpoint: string | null;
  model: string | null;
  capabilities: string[];
  credential_ref: string | null;
  enabled: boolean;
  test_status: string;
}

interface SaveProviderParams {
  config: AsrProviderConfig;
  secret: string | null;
}

interface TestProviderParams {
  provider_id: string;
  scope: string[];
}

interface ProviderIdParams {
  provider_id: string;
}

export class TranscriptionVerify {
  static async run(
    mode: string,
    context: common.UIAbilityContext,
    filesDir: string,
    query: string,
  ): Promise<void> {
    hilog.info(DOMAIN, TAG, '%{public}s MODE mode=%{public}s query=%{public}s filesDir=%{public}s',
      MARK, mode, query, filesDir);
    try {
      if (mode === 'asr_setup') {
        TranscriptionVerify.setup(filesDir);
      } else if (mode === 'asr_test') {
        TranscriptionVerify.testProvider(filesDir);
      } else if (mode === 'asr_jobs') {
        await TranscriptionVerify.runJobs(filesDir);
      } else if (mode === 'asr_search') {
        TranscriptionVerify.search(filesDir, query);
      } else if (mode === 'asr_remove') {
        TranscriptionVerify.removeProvider(filesDir);
      } else {
        hilog.error(DOMAIN, TAG, '%{public}s UNKNOWN_MODE mode=%{public}s', MARK, mode);
      }
    } catch (e) {
      hilog.error(DOMAIN, TAG, '%{public}s FAILED mode=%{public}s error=%{public}s',
        MARK, mode, describeError(e as Object));
    }
  }

  // ---------------------------------------------------------------- 准备

  /**
   * 建模型目录 + 保存 provider。
   *
   * 目录用 0777 是为了让 `hdc file send`（shell uid）能把 228 MiB 的模型直接写进去；
   * 这是**验证期**的取舍，产品随包分发时应把模型放进 HAP 资源 / 安装包，而不是
   * 依赖一个所有人可写的目录。
   */
  static setup(filesDir: string): void {
    const base = `${filesDir}/asr`;
    const modelDir = `${base}/sense-voice`;
    fs.mkdirSync(base, true);
    fs.mkdirSync(modelDir, true);
    fileIO.chmodSync(base, 0o777);
    fileIO.chmodSync(modelDir, 0o777);

    const mode = TranscriptionVerify.statMode(modelDir);
    hilog.info(DOMAIN, TAG, '%{public}s DIR model=%{public}s mode=%{public}s',
      MARK, modelDir, mode);

    // 应用自己写一次：证明目录对应用可写（对 shell 可写要靠下面那次 hdc 推送来验）。
    let selfWrite = false;
    const probe = `${modelDir}/.write-probe`;
    try {
      const file = fs.openSync(probe, fs.OpenMode.CREATE | fs.OpenMode.READ_WRITE);
      fs.writeSync(file.fd, 'ok');
      fs.closeSync(file);
      fs.unlinkSync(probe);
      selfWrite = true;
    } catch (e) {
      hilog.warn(DOMAIN, TAG, '%{public}s DIR self-write failed %{public}s',
        MARK, describeError(e as Object));
    }
    hilog.info(DOMAIN, TAG, '%{public}s DIR model=%{public}s appWritable=%{public}s',
      MARK, modelDir, selfWrite ? 'true' : 'false');

    // hdc 的推模型路线也可以先放 `/data/local/tmp` 再由应用拷进来；先如实测一次
    // 应用进程能不能读那个目录，免得真到那一步才发现要走另一条路。
    const localTmp = '/data/local/tmp/asr-probe.txt';
    let localTmpReadable = false;
    let localTmpText = '';
    try {
      localTmpText = fs.readTextSync(localTmp);
      localTmpReadable = true;
    } catch (e) {
      localTmpText = describeError(e as Object);
    }
    hilog.info(DOMAIN, TAG, '%{public}s LOCAL_TMP readable=%{public}s path=%{public}s text=%{public}s',
      MARK, localTmpReadable ? 'true' : 'false', localTmp, localTmpText);

    const core = DiaryCore.openInDirectory(filesDir);
    try {
      const params: SaveProviderParams = {
        config: {
          id: PROVIDER_ID,
          label: PROVIDER_LABEL,
          adapter_id: 'sherpa_onnx',
          // 本机引擎不用 endpoint / 密钥；核心会拒绝非空 endpoint。
          endpoint: null,
          model: modelDir,
          capabilities: ['transcribe_audio'],
          credential_ref: null,
          enabled: true,
          test_status: 'untested',
        },
        secret: null,
      };
      const saved = core.rawCall('providers.save', params);
      logJson('providers.save', saved);
      logJson('providers.list', core.rawCall('providers.list', null));
      logJson('core.info', core.coreInfo());
    } finally {
      core.close();
    }
    hilog.info(DOMAIN, TAG, '%{public}s SETUP DONE modelDir=%{public}s', MARK, modelDir);
  }

  /** 连接测试：真的加载模型 + 转写 0.05 秒静音，不联网。 */
  static testProvider(filesDir: string): void {
    const core = DiaryCore.openInDirectory(filesDir);
    try {
      const params: TestProviderParams = {
        provider_id: PROVIDER_ID,
        scope: ['transcribe_audio'],
      };
      const report = core.rawCall('providers.test', params);
      logJson('providers.test', report);
    } finally {
      core.close();
    }
  }

  /** 删配置行（连接测试失败、要改模型目录时用）。 */
  static removeProvider(filesDir: string): void {
    const core = DiaryCore.openInDirectory(filesDir);
    try {
      const params: ProviderIdParams = { provider_id: PROVIDER_ID };
      const removed = core.rawCall('providers.remove', params);
      logJson('providers.remove', removed);
      logJson('providers.list', core.rawCall('providers.list', null));
    } finally {
      core.close();
    }
  }

  // ---------------------------------------------------------------- 跑任务

  /**
   * 在 TaskPool 里跑 `jobs.runDue`。
   *
   * 预算给 180 秒：首次要加载 228 MiB 模型（SenseVoice + ORT），之后每条 5 秒音频
   * 的推理在设备上是秒级；旧队列里有 9 条等待配置的历史任务，也由这一轮一并补做。
   */
  static async runJobs(filesDir: string): Promise<void> {
    const dbPath = `${filesDir}/library.sqlite`;
    const started = Date.now();
    const raw = await taskpool.execute(runAsrJobs, dbPath, 180000, 'asr_verify');
    const report = JSON.parse(raw as string) as AsrJobsReport;
    logJson('jobs.report', report);
    hilog.info(DOMAIN, TAG, '%{public}s JOBS wallMs=%{public}d openMs=%{public}d runMs=%{public}d pssBeforeKb=%{public}d pssAfterKb=%{public}d pssDeltaKb=%{public}d privateBeforeKb=%{public}d privateAfterKb=%{public}d privateDeltaKb=%{public}d error=%{public}s',
      MARK, Date.now() - started, report.open_ms, report.run_ms,
      report.pss_before_kb, report.pss_after_kb, report.pss_after_kb - report.pss_before_kb,
      report.private_before_kb, report.private_after_kb,
      report.private_after_kb - report.private_before_kb, report.error);
  }

  // ---------------------------------------------------------------- 检索

  static search(filesDir: string, query: string): void {
    if (query.length === 0) {
      hilog.error(DOMAIN, TAG, '%{public}s SEARCH missing query (--ps query=...)', MARK);
      return;
    }
    const core = DiaryCore.openInDirectory(filesDir);
    try {
      const params: SearchParams = {
        request: { query: query, page_size: 20 },
        query_revision: 1,
      };
      const started = Date.now();
      const snapshot: SearchSnapshot = core.search(params);
      const elapsed = Date.now() - started;
      logJson('search', snapshot);
      hilog.info(DOMAIN, TAG, '%{public}s SEARCH query=%{public}s hits=%{public}d elapsedMs=%{public}d coverage=%{public}s',
        MARK, query, snapshot.results.length, elapsed, snapshot.index_coverage);
      for (let i = 0; i < snapshot.results.length && i < 5; i++) {
        const hit = snapshot.results[i];
        hilog.info(DOMAIN, TAG, '%{public}s HIT[%{public}d] kind=%{public}s matchedBy=%{public}s sourceId=%{public}s snippet=%{public}s',
          MARK, i, hit.source_kind, hit.matched_by.join(','), hit.source_id, hit.snippet);
      }
    } finally {
      core.close();
    }
  }

  // ---------------------------------------------------------------- 小工具

  private static statMode(path: string): string {
    try {
      return (fs.statSync(path).mode & 0o777).toString(8);
    } catch (e) {
      return `statFailed:${describeError(e as Object)}`;
    }
  }
}

function logJson(label: string, value: Object | null): void {
  const text = JSON.stringify(value);
  const chunks = Math.max(1, Math.ceil(text.length / CHUNK));
  for (let i = 0; i < chunks; i++) {
    hilog.info(DOMAIN, TAG, '%{public}s JSON[%{public}s][%{public}d/%{public}d] %{public}s',
      MARK, label, i + 1, chunks, text.substring(i * CHUNK, (i + 1) * CHUNK));
  }
}

function describeError(error: Object): string {
  const err = error as Error;
  if (err !== undefined && err !== null && err.message !== undefined) {
    return err.message;
  }
  return JSON.stringify(error);
}

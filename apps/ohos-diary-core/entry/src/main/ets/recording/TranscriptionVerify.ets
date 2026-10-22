// 转写链真机探针（#108 转写未验项）。
//
// 只用于验证，不是产品 API。入口（`aa start ... --ps verify_mode <mode>`）：
//
//   asr_setup   建沙箱模型目录（应用默认权限）、保存本机引擎的 provider 配置
//   asr_test    对该 provider 跑连接测试（会真的加载模型并转写 0.05 秒静音，不联网）
//   asr_jobs    在 worker 里跑一轮 jobs.runDue，打印耗时/内存/摘要/队列
//   asr_warm    先跑一次连接测试（把 228 MiB 模型加载进本进程），再跑一轮 jobs.runDue
//               ⇒ 量「热态」转写耗时（不含模型加载）
//   asr_search  以 `--ps query=<词>` 跑 search.start，打印命中（验证转写文本可检索）
//   asr_remove  删掉本探针保存的 provider 行（连接测试失败后重新配置用）
//   asr_download 从 `--ps base=<url>` 下载模型/词表（rport 到宿主机）
//   asr_record_play 设备扬声器播 zh.wav、麦克风录回来（**受系统 AEC 影响，不能量准确率**）
//   asr_inject  把 zh.wav 按平台层录音约定直接登记 → 跑转写 → 与已知答案比对
//               输出 `DIARY_ASR INJECT_TEXT` / `INJECT_RESULT`（debug-only；
//               `--ps base=<url>` 只用于缺 zh.wav 时下载，179 KB）
//   gen_remote  从 `--ps base=<url>` 下载 deepseek.json → 造一条 waiting_configuration
//               的 diary_generate（自动整理开关临时打开、随即恢复）→ providers.save/test
//               （真出网）→ 第二轮 jobs.runDue 领取并生成 → diary.setCurrent 确认候选
//               → diary.getDay/getVersion，打印生成正文。只用于真机验证，不是产品路径。
//   gen_remove  删掉 `deepseek_gen`（清理探针保存的服务配置；不删任何日记）
//
// 为什么要有 `asr_setup` 这一步：设备的应用沙箱 `/data/app/el2/.../files` 是
// `drwxrwx--- <app uid>`，`hdc file send`（shell uid）写不进去。模型只能由**应用
// 自己**通过 rport 流式写进沙箱（见 `downloadModels`）。目录用应用默认权限
// （不再 chmod 0777——shell 本来也写不进去，放宽权限没有收益）。
//
// 日志 tag 是 `DiaryAsr`，机器可读前缀 `DIARY_ASR`；长 JSON 与录音探针同口径按
// 900 字符分片。

import { common } from '@kit.AbilityKit';
import { fileIo as fs } from '@kit.CoreFileKit';
import { hilog } from '@kit.PerformanceAnalysisKit';
import { audio } from '@kit.AudioKit';
import { taskpool } from '@kit.ArkTS';
import { DiaryCore } from '../core/DiaryCore';
import {
  Capture,
  DiaryDay,
  DiaryDayPage,
  DiaryVersion,
  DiaryVersionPage,
  ExtractedContent,
  Job,
  JobPage,
  JobRunSummary,
  SearchParams,
  SearchSnapshot,
  SetDiaryCurrentParams,
} from '../core/Models';
import { AsrJobsReport, runAsrJobs } from './AsrWorker';
import { GenRemoteConfig, GenRemoteReport, runGenRemote } from './DiaryGenWorker';
import { pcmPeak, pcmRms, readWavPcm } from './HapBothWav';
import { downloadToFile, ExpectedFile } from './ModelDownload';
import { injectWavAsRecording } from './RecordingInject';
import { RecordingVerify } from './RecordingVerify';

const DOMAIN: number = 0x0000;
const TAG: string = 'DiaryAsr';
const MARK: string = 'DIARY_ASR';
/** 生成链的机器可读前缀：判据按 `DIARY_GEN` 取日志。 */
const GEN_MARK: string = 'DIARY_GEN';
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

/**
 * SenseVoice-Small int8（2024-07-17）的两份随包文件。
 *
 * 数字与 sha256 的唯一事实来源是
 * `models/manifest/sense-voice-small-int8-2024-07-17.json` 的 `files` 数组；
 * 改模型必须同时改这里。下载器（`ModelDownload.ets`）要求两者都给。
 */
const EXPECTED_MODEL_INT8: ExpectedFile = {
  bytes: 239233841,
  sha256: 'c71f0ce00bec95b07744e116345e33d8cbbe08cef896382cf907bf4b51a2cd51',
};

const EXPECTED_TOKENS: ExpectedFile = {
  bytes: 315894,
  sha256: 'f449eb28dc567533d7fa59be34e2abca8784f771850c78a47fb731a31429a1dc',
};

/** 自播放回放用的验证素材（sherpa 的 zh.wav，5.59 s）：固定来源、固定哈希。 */
const EXPECTED_ZH_WAV: ExpectedFile = {
  bytes: 178988,
  sha256: 'b77f1794fe374a0ba1ee1dc458bfaf9349496cbbfc32780c50ba3c5a7ad8e373',
};

/**
 * `gen_remote` 从宿主机下载的远端服务配置（**只在探针期存在**）。
 *
 * 文件在 `/tmp/gen-test/deepseek.json`，由宿主 HTTP 服务经 rport 提供；内容含
 * DeepSeek 密钥，所以**不提交**、测完即删。这里只钉住字节数与 sha256，
 * 让 `ModelDownload` 的完整性校验照常生效（不是「大小未知就放行」）。
 */
const EXPECTED_DEEPSEEK_JSON: ExpectedFile = {
  bytes: 139,
  sha256: 'da23b80e23dc09da0eca5ab799b0ceba3b9b7d1cbb0bd72118438f822027e51c',
};

/**
 * `asr_inject` 的已知答案：README 里 2026-10-17 真机 `DIARY_HAP TRANSCRIPT` 的原文。
 *
 * 注入的 PCM 与那次完全相同的 `zh.wav`（同一份 178,988 B / sha256 钉死），所以
 * 「同一段音频、同一个引擎，应该转出同一句话」。比对是宽松的：去标点/空白后
 * 字符级相似度 ≥ 0.8 就算通过（数字写成「九/五」等常见差异不算失败）。
 */
const EXPECTED_ZH_TEXT: string = '开饭时间早上9点至下午5点。';
const SIMILARITY_THRESHOLD: number = 0.8;

/** `asr_inject` 从 `extracted_contents` 读到的正文与元数据。 */
interface InjectedText {
  found: boolean;
  sourceId: string;
  extractorId: string;
  status: string;
  coverage: string;
  errorCode: string;
  text: string;
  /** 排障：逐个来源的 extractor/status/coverage/长度，用分号连起来。 */
  detail: string;
}

export class TranscriptionVerify {
  static async run(
    mode: string,
    context: common.UIAbilityContext,
    filesDir: string,
    query: string,
    baseUrl: string,
  ): Promise<void> {
    hilog.info(DOMAIN, TAG, '%{public}s MODE mode=%{public}s query=%{public}s filesDir=%{public}s base=%{public}s',
      MARK, mode, query, filesDir, baseUrl);
    try {
      if (mode === 'asr_setup') {
        TranscriptionVerify.setup(filesDir);
      } else if (mode === 'asr_test') {
        TranscriptionVerify.testProvider(filesDir);
      } else if (mode === 'asr_jobs') {
        await TranscriptionVerify.runJobs(filesDir);
      } else if (mode === 'asr_warm') {
        await TranscriptionVerify.testThenJobs(filesDir);
      } else if (mode === 'asr_search') {
        TranscriptionVerify.search(filesDir, query);
      } else if (mode === 'asr_remove') {
        TranscriptionVerify.removeProvider(filesDir);
      } else if (mode === 'asr_download') {
        await TranscriptionVerify.downloadModels(filesDir, baseUrl);
      } else if (mode === 'asr_record_play') {
        await TranscriptionVerify.recordWithPlayback(context, filesDir, baseUrl);
      } else if (mode === 'gen_remote') {
        await TranscriptionVerify.generateRemote(filesDir, baseUrl);
      } else if (mode === 'gen_remove') {
        TranscriptionVerify.removeGenProvider(filesDir);
      } else if (mode === 'asr_inject') {
        await TranscriptionVerify.injectKnownWavAndCheck(filesDir, baseUrl);
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
   * 目录用应用默认权限（最小权限）：shell 本来就不能写应用沙箱，放宽权限
   * 没有收益。模型由应用自己通过 rport 下载并原子落盘（见 `downloadModels`）。
   */
  static setup(filesDir: string): void {
    const base = `${filesDir}/asr`;
    const modelDir = `${base}/sense-voice`;
    ensureDir(base);
    ensureDir(modelDir);

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

  /**
   * 冷/热两次量测：先连接测试（把模型加载进本进程的引擎缓存），再跑 jobs.runDue。
   *
   * 两段在**同一个进程**里：Rust 侧的识别器缓存是进程级的，所以第二段拿到的是
   * 已经加载好的模型；两段耗时之差就是「模型加载 + 建 ORT session」的成本。
   * 任务由 taskpool 的另一个线程跑，但同一个进程 ⇒ 缓存仍命中。
   */
  static async testThenJobs(filesDir: string): Promise<void> {
    const t0 = Date.now();
    TranscriptionVerify.testProvider(filesDir);
    const testMs = Date.now() - t0;
    hilog.info(DOMAIN, TAG, '%{public}s WARM testMs=%{public}d phase=load_model_and_probe',
      MARK, testMs);
    await TranscriptionVerify.runJobs(filesDir);
    hilog.info(DOMAIN, TAG, '%{public}s WARM totalMs=%{public}d', MARK, Date.now() - t0);
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

  // ---------------------------------------------------------------- 远端生成

  /**
   * `gen_remote`：远端生成链真机探针（转写材料 → provider → 队列生成 → 读回正文）。
   *
   * `diary.generate` 已经接进 C ABI；本探针要验的是**自动整理**那条队列路径
   * （provider → 队列 → 生成），所以不走它，而是用产品自己的队列造等待任务并跑掉它：
   *   1. 删掉旧的 `deepseek_gen`，临时打开自动整理（计划时刻 00:00），`runDue`
   *      把「有材料、这份输入没冻结过」的日子排成 `waiting_configuration`；
   *      随后立刻把自动整理恢复原值（默认关、22:30）；
   *   2. `providers.save` 把密钥换成核心的 `credential_ref`，`providers.test`
   *      真的发一次 HTTPS 请求（`max_tokens=1` 的 ping）；
   *   3. 第二轮 `runDue` 领取等待任务并生成；自动整理的结果按 B4e 一律是候选，
   *      这里再走产品的 `diary.setCurrent`（用户确认）把最新一版设为当前，然后
   *      用 `diary.getDay` / `diary.getVersion` 读回 currentVersionId 与正文。
   *
   * 密钥只在内存里过一手：下载到的 JSON 不打印、worker 不打印、日志只出现
   * `secret=<len=…>` 与 `credential_ref=<len=…>`。测完由 `gen_remove` 删 provider
   * 和沙箱里的 `deepseek.json`。
   */
  static async generateRemote(filesDir: string, baseUrl: string): Promise<void> {
    const dest = `${filesDir}/deepseek.json`;
    await downloadToFile(`${baseUrl}/deepseek.json`, dest, EXPECTED_DEEPSEEK_JSON);
    const text = fs.readTextSync(dest);
    const config = JSON.parse(text) as GenRemoteConfig;
    // 只打「密钥有多长」，绝不打内容（与核心/宿主的日志口径一致）。
    hilog.info(DOMAIN, TAG,
      '%{public}s CONFIG provider=%{public}s endpoint=%{public}s model=%{public}s secret=<len=%{public}d>',
      MARK, config.provider_id, config.endpoint, config.model, config.secret.length);

    const raw = await taskpool.execute(runGenRemote, `${filesDir}/library.sqlite`,
      config.provider_id, config.endpoint, config.model, config.secret, 180000, 'gen_verify');
    const report = JSON.parse(raw as string) as GenRemoteReport;

    if (report.provider !== null) {
      const ref = report.provider.credential_ref;
      hilog.info(DOMAIN, TAG,
        '%{public}s PROVIDER_SAVED id=%{public}s credential_ref=<len=%{public}d> enabled=%{public}s testStatus=%{public}s saveMs=%{public}d',
        GEN_MARK, report.provider.id, ref === null ? 0 : ref.length,
        report.provider.enabled ? 'true' : 'false', report.provider.test_status, report.save_ms);
    } else {
      hilog.error(DOMAIN, TAG,
        '%{public}s PROVIDER_SAVED id=%{public}s error_code=%{public}s phase=%{public}s error=%{public}s',
        GEN_MARK, config.provider_id, report.error_code, report.phase, report.error);
    }

    if (report.test !== null) {
      hilog.info(DOMAIN, TAG,
        '%{public}s TEST actual_call_made=%{public}s results=%{public}s testMs=%{public}d message=%{public}s',
        GEN_MARK, report.test.actual_call_made ? 'true' : 'false',
        JSON.stringify(report.test.results), report.test_ms,
        report.test.message === null ? '' : report.test.message);
    } else {
      hilog.error(DOMAIN, TAG,
        '%{public}s TEST actual_call_made=false results={} error_code=%{public}s phase=%{public}s error=%{public}s',
        GEN_MARK, report.error_code, report.phase, report.error);
    }

    if (report.settings_original !== null) {
      hilog.info(DOMAIN, TAG,
        '%{public}s SETTINGS originalAutoOrganize=%{public}s originalTime=%{public}s restored=%{public}s settingsMs=%{public}d',
        GEN_MARK, report.settings_original.auto_organize_enabled ? 'true' : 'false',
        report.settings_original.auto_organize_time,
        report.settings_restored ? 'true' : 'false', report.settings_ms);
    }

    logGenSummary('schedule', report.summary1, report.run1_ms, report);
    for (let i = 0; i < report.jobs_before.length; i++) {
      logGenJob('JOB_BEFORE', report.jobs_before[i]);
    }
    logGenSummary('generate', report.summary2, report.run2_ms, report);
    for (let i = 0; i < report.jobs_after.length; i++) {
      logGenJob('JOB', report.jobs_after[i]);
    }
    if (report.error.length > 0) {
      hilog.error(DOMAIN, TAG,
        '%{public}s FAILED phase=%{public}s error_code=%{public}s error=%{public}s',
        GEN_MARK, report.phase, report.error_code, report.error);
    }

    // 读回结果：优先选「最新生成的版本」所在的那一天；若它还不是当前版本，
    // 走产品的 `diary.setCurrent`（自动整理的结果按 B4e 一律是候选，用户确认才成为当前）。
    const core = DiaryCore.openInDirectory(filesDir);
    try {
      const page: DiaryDayPage = core.listDiaryDays(null);
      let targetDay = '';
      let targetVersionId = '';
      let targetCreated = '';
      for (let i = 0; i < page.days.length; i++) {
        const key = page.days[i].day_key;
        const versions: DiaryVersionPage = core.listDiaryVersions({ day_key: key, limit: 5 });
        if (versions.versions.length === 0) {
          continue;
        }
        const latest = versions.versions[0];
        if (latest.created_at > targetCreated) {
          targetCreated = latest.created_at;
          targetDay = key;
          targetVersionId = latest.id;
        }
      }

      if (targetDay.length === 0) {
        hilog.error(DOMAIN, TAG, '%{public}s BODY_MISSING 库里没有任何日记版本', GEN_MARK);
        return;
      }

      let day: DiaryDay = core.getDiaryDay({ day_key: targetDay });
      if (day.current_version_id === null) {
        const candidateId = day.candidate_version_ids.length > 0
          ? day.candidate_version_ids[0] : targetVersionId;
        const promote: SetDiaryCurrentParams = {
          day_key: targetDay,
          version_id: candidateId,
          expected_revision: day.revision,
        };
        hilog.info(DOMAIN, TAG,
          '%{public}s PROMOTE day_key=%{public}s version_id=%{public}s expectedRevision=%{public}d reason=自动整理结果按 B4e 一律候选，这里走 diary.setCurrent（用户确认）',
          GEN_MARK, targetDay, candidateId, day.revision);
        day = core.setDiaryCurrent(promote);
        hilog.info(DOMAIN, TAG,
          '%{public}s PROMOTE_DONE day_key=%{public}s currentVersionId=%{public}s revision=%{public}d',
          GEN_MARK, day.day_key, day.current_version_id === null ? '' : day.current_version_id,
          day.revision);
      }

      const currentId = day.current_version_id;
      if (currentId === null) {
        hilog.error(DOMAIN, TAG,
          '%{public}s BODY_MISSING day_key=%{public}s 没有当前版本', GEN_MARK, targetDay);
        return;
      }
      hilog.info(DOMAIN, TAG,
        '%{public}s DAY day_key=%{public}s currentVersionId=%{public}s candidates=%{public}d captures=%{public}d processed=%{public}d pending=%{public}d',
        GEN_MARK, day.day_key, currentId, day.candidate_version_ids.length,
        day.material_counts.captures, day.material_counts.processed, day.material_counts.pending);
      const current: DiaryVersion = core.getDiaryVersion({ version_id: currentId });
      logGenBody('BODY', day.day_key, current);

      // 其它天若有生成出来的候选版本，也把正文如实打出来（不设当前，只作证据）。
      for (let i = 0; i < page.days.length; i++) {
        const key = page.days[i].day_key;
        if (key === targetDay) {
          continue;
        }
        const other: DiaryDay = core.getDiaryDay({ day_key: key });
        if (other.candidate_version_ids.length === 0) {
          continue;
        }
        const candidate: DiaryVersion = core.getDiaryVersion({
          version_id: other.candidate_version_ids[0],
        });
        hilog.info(DOMAIN, TAG,
          '%{public}s CANDIDATE day_key=%{public}s versionId=%{public}s chars=%{public}d origin=%{public}s',
          GEN_MARK, other.day_key, candidate.id, candidate.body.length, candidate.origin);
        logGenBody('CANDIDATE_BODY', other.day_key, candidate);
      }
    } finally {
      core.close();
    }
  }

  /**
   * `gen_remove`：删掉探针保存的远端服务行，并把下载到沙箱的明文配置删掉。
   *
   * 两件事必须一起做：`providers.remove` 让核心删掉 `credential_ref` 指向的密钥文件；
   * `deepseek.json` 是**密钥明文的另一份拷贝**，不删就等于没清理。这里顺带把
   * `secrets/` 目录的剩余文件数打出来，作为「密钥托管没有残留」的可核对证据。
   */
  static removeGenProvider(filesDir: string): void {
    const core = DiaryCore.openInDirectory(filesDir);
    try {
      const params: ProviderIdParams = { provider_id: 'deepseek_gen' };
      const removed = core.rawCall('providers.remove', params);
      logJson('providers.remove', removed);
      logJson('providers.list', core.rawCall('providers.list', null));
      hilog.info(DOMAIN, TAG, '%{public}s GEN_PROVIDER_REMOVED id=deepseek_gen', GEN_MARK);
    } finally {
      core.close();
    }

    const keyFile = `${filesDir}/deepseek.json`;
    removeFileIfExists(keyFile);
    removeFileIfExists(`${keyFile}.part`);
    hilog.info(DOMAIN, TAG, '%{public}s KEY_FILE_REMOVED path=%{public}s exists=%{public}s',
      GEN_MARK, keyFile, fs.accessSync(keyFile) ? 'true' : 'false');

    const secretsDir = `${filesDir}/secrets`;
    let count = 0;
    let names = '';
    try {
      const entries = fs.listFileSync(secretsDir);
      count = entries.length;
      names = entries.join(',');
    } catch (e) {
      count = 0;
    }
    hilog.info(DOMAIN, TAG, '%{public}s SECRETS_LEFT dir=%{public}s count=%{public}d names=%{public}s',
      GEN_MARK, secretsDir, count, names);
  }

  // ---------------------------------------------------------------- 模型传输

  /**
   * 从本机 HTTP 服务把模型与词表下到沙箱。
   *
   * 为什么不是 `hdc file send`：应用沙箱目录是 `drwxrwx--- <app uid>`，shell 的
   * SELinux 域不允许写应用数据（实测 `touch` 与 `hdc file send` 都是 EACCES），
   * shell 本来就不能写应用数据。`/data/local/tmp` 应用进程又看不到（ENOENT）。
   * 所以走 `hdc rport` 把宿主机的 HTTP 服务映射到设备 127.0.0.1，由**应用自己**
   * 流式写进沙箱，并且走统一的完整性校验（`.part` → 2xx/Content-Length/sha256 →
   * 原子 rename，见 `ModelDownload.ets`）。这是验证期的传输通道，不是产品行为。
   */
  static async downloadModels(filesDir: string, baseUrl: string): Promise<void> {
    const dir = `${filesDir}/asr/sense-voice`;
    ensureDir(dir);
    await downloadToFile(`${baseUrl}/model.int8.onnx`,
      `${dir}/model.int8.onnx`, EXPECTED_MODEL_INT8);
    await downloadToFile(`${baseUrl}/tokens.txt`,
      `${dir}/tokens.txt`, EXPECTED_TOKENS);
    const modelBytes = fs.statSync(`${dir}/model.int8.onnx`).size;
    const tokenBytes = fs.statSync(`${dir}/tokens.txt`).size;
    hilog.info(DOMAIN, TAG, '%{public}s MODEL_DOWNLOAD_OK modelBytes=%{public}d tokensBytes=%{public}d',
      MARK, modelBytes, tokenBytes);
  }

  // ---------------------------------------------------------------- 声学回放

  /**
   * 用设备扬声器播一段中文语音，同时录 5 秒。
   *
   * 为什么不用宿主扬声器：实测宿主播放时设备麦克风只收到 ~100–200 RMS 的环境声，
   * 转写结果是「。」（没识别出文字）。设备自己的扬声器离麦克风足够近，且
   * `SOURCE_TYPE_MIC` 不做回声消除，能量能进采集链路。
   *
   * 播放走 `AudioRenderer` 的 `writeData` 回调（PCM 循环写），录音走现有的
   * `RecordingVerify.run('record')`：两条链路互不依赖，录完照常 finalize + 转写。
   */
  static async recordWithPlayback(
    context: common.UIAbilityContext,
    filesDir: string,
    baseUrl: string,
  ): Promise<void> {
    const dir = `${filesDir}/asr/sense-voice`;
    ensureDir(dir);
    const wavPath = `${dir}/zh.wav`;
    if (!fs.accessSync(wavPath) || fs.statSync(wavPath).size < 1000) {
      // zh.wav 是验证素材、不随产品分发；固定来源，所以也要求 bytes + sha256。
      await downloadToFile(`${baseUrl}/zh.wav`, wavPath, EXPECTED_ZH_WAV);
    }
    const pcm = readWavPcm(wavPath);
    // 播的是什么：幅度也打出来，才能和「麦克风收到多少」对上。播放音量受系统媒体
    // 音量与安全音量保护（长时间大音量会自动降档）影响，只记「播了」不够解释结果。
    hilog.info(DOMAIN, TAG, '%{public}s PLAY wav=%{public}s pcmBytes=%{public}d rms=%{public}d peak=%{public}d',
      MARK, wavPath, pcm.length, pcmRms(pcm), pcmPeak(pcm));

    const renderer = await audio.createAudioRenderer({
      streamInfo: {
        samplingRate: audio.AudioSamplingRate.SAMPLE_RATE_16000,
        channels: audio.AudioChannel.CHANNEL_1,
        sampleFormat: audio.AudioSampleFormat.SAMPLE_FORMAT_S16LE,
        encodingType: audio.AudioEncodingType.ENCODING_TYPE_RAW,
      },
      rendererInfo: {
        usage: audio.StreamUsage.STREAM_USAGE_MUSIC,
        rendererFlags: 0,
      },
    });
    let offset = 0;
    const onWrite = (data: ArrayBuffer): audio.AudioDataCallbackResult => {
      const out = new Uint8Array(data);
      for (let i = 0; i < out.length; i++) {
        out[i] = pcm[offset];
        offset = (offset + 1) % pcm.length;
      }
      return audio.AudioDataCallbackResult.VALID;
    };
    renderer.on('writeData', onWrite);
    await renderer.setVolume(1.0);

    // **先开录、再出声**。反过来的话，`record` 模式固定 5 秒，先播 400 ms 会把句首
    // 吃掉——实测第一版三次里三次都把「开饭」的「开」听成「快/派」；而同一段音频
    // 在 21:29 那次（句首没被吃）转出的是正确的「开饭时间早上9点至下午5点。」。
    // 现在录音先跑 450 ms 再 start()，句首落在采集窗口里；末尾丢 0.2 秒无伤。
    const recording = RecordingVerify.run('record', context, filesDir);
    try {
      await sleep(450);
      await renderer.start();
      hilog.info(DOMAIN, TAG, '%{public}s PLAY started renderer=%{public}d', MARK, renderer.state);
      await recording;
    } finally {
      renderer.off('writeData', onWrite);
      await renderer.stop();
      await renderer.release();
      hilog.info(DOMAIN, TAG, '%{public}s PLAY stopped', MARK);
    }
  }

  // ---------------------------------------------------------------- 已知 PCM 注入

  /**
   * `asr_inject`：把一段已知的 `zh.wav` 注入成录音 → 跑转写 → 与标准答案比对。
   *
   * 为什么需要它：`asr_record_play` 是「扬声器播、麦克风录」，系统 AEC/降噪会把
   * **自己正在播的声音**从采集里减掉（这正是 AEC 的目的），所以那条路径证明不了
   * 转写准不准——它测到的是「回声消除有多彻底」。这里绕开声学路径，把已知 PCM
   * 按平台层录音约定直接登记，变量只剩「引擎 + 模型 + 解码链」。
   *
   * 只下载 `zh.wav`（179 KB，本地 rport）；模型/tokens 已在沙箱里，不重下。
   */
  static async injectKnownWavAndCheck(filesDir: string, baseUrl: string): Promise<void> {
    const wavPath = `${filesDir}/zh.wav`;
    if (!fs.accessSync(wavPath) || fs.statSync(wavPath).size !== EXPECTED_ZH_WAV.bytes) {
      await downloadToFile(`${baseUrl}/zh.wav`, wavPath, EXPECTED_ZH_WAV);
    }
    const core = DiaryCore.openInDirectory(filesDir);
    try {
      const injected = injectWavAsRecording(core, wavPath, 'op-asr-inject', 'asr_inject');
      hilog.info(DOMAIN, TAG,
        '%{public}s INJECT wav=%{public}s wavBytes=%{public}d pcmBytes=%{public}d pcmMs=%{public}d rms=%{public}d peak=%{public}d',
        MARK, injected.wavPath, injected.segmentByteSize, injected.pcmBytes,
        injected.pcmMs, injected.rms, injected.peak);
      hilog.info(DOMAIN, TAG,
        '%{public}s INJECT SEG index=%{public}d file=%{public}s bytes=%{public}d pcmMs=%{public}d sha256=%{public}s durable=%{public}s',
        MARK, injected.segmentIndex, injected.segmentFileName, injected.segmentByteSize,
        injected.segmentDurationMs, injected.segmentSha256.substring(0, 16),
        injected.durable ? 'true' : 'false');
      hilog.info(DOMAIN, TAG,
        '%{public}s INJECT FINALIZE capture=%{public}s asset=%{public}s audioMs=%{public}d segments=%{public}d state=%{public}s queued=%{public}s',
        MARK, injected.captureId, injected.assetId, injected.finalize.logical_duration_ms,
        injected.finalize.segment_count, injected.finalize.state,
        injected.finalize.transcription_queued ? 'true' : 'false');

      const started = Date.now();
      const raw = await taskpool.execute(
        runAsrJobs, `${filesDir}/library.sqlite`, 180000, 'asr_inject');
      const report = JSON.parse(raw as string) as AsrJobsReport;
      logJson('jobs.report', report);
      hilog.info(DOMAIN, TAG,
        '%{public}s INJECT JOBS wallMs=%{public}d openMs=%{public}d runMs=%{public}d pssDeltaKb=%{public}d error=%{public}s',
        MARK, Date.now() - started, report.open_ms, report.run_ms,
        report.pss_after_kb - report.pss_before_kb, report.error);
      if (report.summary !== null) {
        hilog.info(DOMAIN, TAG,
          '%{public}s INJECT JOBS processed=%{public}d succeeded=%{public}d failed=%{public}d retrying=%{public}d dueBacklog=%{public}d',
          MARK, report.summary.processed, report.summary.succeeded, report.summary.failed,
          report.summary.retrying, report.summary.due_backlog);
      }

      TranscriptionVerify.logTranscribeJobs(core, injected.assetId);
      const extracted = TranscriptionVerify.readInjectedText(
        core, injected.captureId, injected.assetId);
      hilog.info(DOMAIN, TAG,
        '%{public}s INJECT EXTRACT found=%{public}s source=%{public}s extractor=%{public}s status=%{public}s coverage=%{public}s error=%{public}s detail=%{public}s',
        MARK, extracted.found ? 'true' : 'false', extracted.sourceId, extracted.extractorId,
        extracted.status, extracted.coverage, extracted.errorCode, extracted.detail);

      const expectedNormalized = normalizeForCompare(EXPECTED_ZH_TEXT);
      const actualNormalized = normalizeForCompare(extracted.text);
      const distance = editDistance(expectedNormalized, actualNormalized);
      const denominator = Math.max(expectedNormalized.length, actualNormalized.length);
      const similarity = denominator === 0 ? 1 : 1 - distance / denominator;
      const ok = similarity >= SIMILARITY_THRESHOLD;
      hilog.info(DOMAIN, TAG,
        '%{public}s INJECT_SIMILARITY expected=%{public}s actual=%{public}s distance=%{public}d length=%{public}d similarity=%{public}s threshold=%{public}s',
        MARK, expectedNormalized, actualNormalized, distance, denominator,
        similarity.toFixed(4), SIMILARITY_THRESHOLD.toFixed(2));
      hilog.info(DOMAIN, TAG, '%{public}s INJECT_TEXT text=%{public}s', MARK, extracted.text);
      hilog.info(DOMAIN, TAG,
        '%{public}s INJECT_RESULT ok=%{public}s similarity=%{public}s text=%{public}s',
        MARK, ok ? 'true' : 'false', similarity.toFixed(2), extracted.text);
    } catch (e) {
      const message = describeError(e as Object);
      hilog.error(DOMAIN, TAG,
        '%{public}s INJECT_RESULT ok=false similarity=0.00 text= error=%{public}s',
        MARK, message);
      throw new Error(message);
    } finally {
      core.close();
    }
  }

  /** 只打本条注入对应的转写任务：`attempt_count` / `error_code` 是失败排查的第一现场。 */
  private static logTranscribeJobs(core: DiaryCore, assetId: string): void {
    const page: JobPage = core.listJobs({ limit: 50 });
    let matched = 0;
    for (let i = 0; i < page.jobs.length; i++) {
      const job: Job = page.jobs[i];
      if (job.kind !== 'transcribe') {
        continue;
      }
      let targetsThisAsset = false;
      for (let j = 0; j < job.target_ids.length; j++) {
        if (job.target_ids[j] === assetId) {
          targetsThisAsset = true;
        }
      }
      if (!targetsThisAsset) {
        continue;
      }
      matched++;
      const progress = job.progress === null
        ? '-' : `${job.progress.completed}/${job.progress.total}`;
      hilog.info(DOMAIN, TAG,
        '%{public}s INJECT JOB id=%{public}s state=%{public}s attempt=%{public}d/%{public}d error=%{public}s progress=%{public}s next=%{public}s',
        MARK, job.id, job.state, job.attempt_count, job.max_attempts,
        job.error_code === null ? '' : job.error_code, progress,
        job.next_attempt_at === null ? '' : job.next_attempt_at);
    }
    hilog.info(DOMAIN, TAG, '%{public}s INJECT JOBS matched=%{public}d scanned=%{public}d',
      MARK, matched, page.jobs.length);
  }

  /**
   * 从 `extracted_contents` 读注入录音的正文。
   *
   * 先用 `captures.get(...).ordered_source_ids` 精确定位这条录音的来源（不能取「第一篇
   * 音频」：沙箱里有历史录音）；读不到时再退回关键词检索，按 `locator.asset_id` 匹配。
   */
  private static readInjectedText(
    core: DiaryCore,
    captureId: string,
    assetId: string,
  ): InjectedText {
    const out: InjectedText = {
      found: false,
      sourceId: '',
      extractorId: '',
      status: '',
      coverage: '',
      errorCode: '',
      text: '',
      detail: '',
    };
    const capture: Capture = core.getCapture({ capture_id: captureId });
    for (let i = 0; i < capture.ordered_source_ids.length; i++) {
      const sourceId = capture.ordered_source_ids[i];
      const content: ExtractedContent | null = core.extractedContent({ source_id: sourceId });
      if (content === null) {
        out.detail += `${i}:${sourceId}:null;`;
        continue;
      }
      out.detail += `${i}:${sourceId}:${content.extractor_id}/${content.status}/${content.coverage}/len=${content.text.length};`;
      if (TranscriptionVerify.takeTranscription(out, content)) {
        return out;
      }
    }

    // 兜底：capture 的来源列表没带音频（或顺序变了）时，按 asset_id 在检索命中里找。
    const snapshot: SearchSnapshot = core.search({
      request: { query: '开饭', page_size: 50 },
      query_revision: 1,
    });
    for (let i = 0; i < snapshot.results.length; i++) {
      const hit = snapshot.results[i];
      if (hit.source_kind !== 'audio' || hit.source_id === null ||
        hit.locator === null || hit.locator.asset_id !== assetId) {
        continue;
      }
      const content: ExtractedContent | null = core.extractedContent({ source_id: hit.source_id });
      if (content !== null && TranscriptionVerify.takeTranscription(out, content)) {
        out.detail += `fallback:${hit.source_id};`;
        return out;
      }
    }
    return out;
  }

  /** 只认 `local_transcription`（本机引擎）的提取结果；别的抽取器不算转写证据。 */
  private static takeTranscription(out: InjectedText, content: ExtractedContent): boolean {
    if (content.extractor_id !== 'local_transcription') {
      return false;
    }
    out.found = true;
    out.sourceId = content.source_id;
    out.extractorId = content.extractor_id;
    out.status = content.status;
    out.coverage = content.coverage;
    out.errorCode = content.error_code === null ? '' : content.error_code;
    out.text = content.text;
    return true;
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

/**
 * 比对前归一化：去掉标点、空白与全角空格，英文转小写。
 *
 * 只做「字符级」比较，不做同音/数字改写：相似度用编辑距离算，阈值 0.8 已经容许
 * 少量差异（例如把 9 写成「九」、多一个「了」）。
 */
function normalizeForCompare(text: string): string {
  let out = '';
  for (let i = 0; i < text.length; i++) {
    const code = text.charCodeAt(i);
    if (code <= 0x20 || code === 0x3000) {
      continue;
    }
    // ASCII 标点/符号区间：!-/ : @ [ - ` { ~
    if ((code >= 0x21 && code <= 0x2F) || (code >= 0x3A && code <= 0x40) ||
      (code >= 0x5B && code <= 0x60) || (code >= 0x7B && code <= 0x7E)) {
      continue;
    }
    // 全角标点（中文正文里最常见的一批；不用 Unicode 属性转义，ArkTS 各版本都稳）。
    if ('，。！？、；：（）《》【】…—·“”‘’'.indexOf(text.charAt(i)) >= 0) {
      continue;
    }
    out += text.charAt(i);
  }
  return out.toLowerCase();
}

/** 字符级编辑距离（Levenshtein）；用于宽松判定，不做分词。 */
function editDistance(a: string, b: string): number {
  const n = b.length;
  const prev: number[] = [];
  const curr: number[] = [];
  for (let j = 0; j <= n; j++) {
    prev.push(j);
    curr.push(0);
  }
  for (let i = 1; i <= a.length; i++) {
    curr[0] = i;
    for (let j = 1; j <= n; j++) {
      const substitution = prev[j - 1] + (a.charCodeAt(i - 1) === b.charCodeAt(j - 1) ? 0 : 1);
      const deletion = prev[j] + 1;
      const insertion = curr[j - 1] + 1;
      curr[j] = Math.min(substitution, Math.min(deletion, insertion));
    }
    // 交换两行，避免每次分配新数组（两行都只依赖上一行与当前行左侧）。
    for (let j = 0; j <= n; j++) {
      const value = curr[j];
      curr[j] = prev[j];
      prev[j] = value;
    }
  }
  return prev[n];
}

function sleep(ms: number): Promise<void> {
  return new Promise<void>((resolve) => {
    setTimeout(() => resolve(), ms);
  });
}

/** 建目录；已存在时不算错（`mkdirSync(.., true)` 在已存在时也会抛 File exists）。 */
function logGenSummary(
  phase: string,
  summary: JobRunSummary | null,
  runMs: number,
  report: GenRemoteReport,
): void {
  if (summary === null) {
    hilog.error(DOMAIN, TAG,
      '%{public}s JOBS phase=%{public}s processed=0 succeeded=0 failed=0 run_ms=%{public}d error_code=%{public}s phase_failed=%{public}s error=%{public}s',
      GEN_MARK, phase, runMs, report.error_code, report.phase, report.error);
    return;
  }
  hilog.info(DOMAIN, TAG,
    '%{public}s JOBS phase=%{public}s processed=%{public}d succeeded=%{public}d failed=%{public}d retrying=%{public}d backfilled=%{public}d recovered=%{public}d backlog=%{public}d waitingDependency=%{public}d dependencyFailed=%{public}d run_ms=%{public}d pssBeforeKb=%{public}d pssAfterKb=%{public}d pssDeltaKb=%{public}d',
    GEN_MARK, phase, summary.processed, summary.succeeded, summary.failed, summary.retrying,
    summary.backfilled, summary.recovered, summary.due_backlog, summary.waiting_dependency,
    summary.dependency_failed, runMs, report.pss_before_kb, report.pss_after_kb,
    report.pss_after_kb - report.pss_before_kb);
}

function logGenJob(label: string, job: Job): void {
  hilog.info(DOMAIN, TAG,
    '%{public}s %{public}s id=%{public}s kind=%{public}s state=%{public}s attempt_count=%{public}d maxAttempts=%{public}d error_code=%{public}s target=%{public}s createdAt=%{public}s updatedAt=%{public}s',
    GEN_MARK, label, job.id, job.kind, job.state, job.attempt_count, job.max_attempts,
    job.error_code === null ? '' : job.error_code, job.target_ids.join(','),
    job.created_at, job.updated_at);
}

function logGenBody(label: string, dayKey: string, version: DiaryVersion): void {
  hilog.info(DOMAIN, TAG, '%{public}s %{public}s day_key=%{public}s chars=%{public}d origin=%{public}s',
    GEN_MARK, label, dayKey, version.body.length, version.origin);
  hilog.info(DOMAIN, TAG, '%{public}s %{public}s %{public}s', GEN_MARK, label, version.body);
  // hilog 单条有长度上限：再按 900 字符分片打一遍，保证「全文」可被完整取回。
  const chunks = Math.max(1, Math.ceil(version.body.length / CHUNK));
  for (let c = 0; c < chunks; c++) {
    hilog.info(DOMAIN, TAG, '%{public}s %{public}s_CHUNK[%{public}d/%{public}d] %{public}s',
      GEN_MARK, label, c + 1, chunks, version.body.substring(c * CHUNK, (c + 1) * CHUNK));
  }
}

function removeFileIfExists(path: string): void {
  try {
    if (fs.accessSync(path)) {
      fs.unlinkSync(path);
    }
  } catch (e) {
    // 删不掉也要让调用方看到 exists=true 的日志，不静默吞掉「密钥还在」这件事。
  }
}

function ensureDir(path: string): void {
  if (fs.accessSync(path)) {
    return;
  }
  fs.mkdirSync(path, true);
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

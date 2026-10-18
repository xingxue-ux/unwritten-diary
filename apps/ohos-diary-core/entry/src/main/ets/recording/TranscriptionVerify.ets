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
import { http } from '@kit.NetworkKit';
import { audio } from '@kit.AudioKit';
import { taskpool } from '@kit.ArkTS';
import { DiaryCore } from '../core/DiaryCore';
import { SearchParams, SearchSnapshot } from '../core/Models';
import { AsrJobsReport, runAsrJobs } from './AsrWorker';
import { RecordingVerify } from './RecordingVerify';

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
      } else if (mode === 'asr_download') {
        await TranscriptionVerify.downloadModels(filesDir);
      } else if (mode === 'asr_record_play') {
        await TranscriptionVerify.recordWithPlayback(context, filesDir);
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
    ensureDir(base);
    ensureDir(modelDir);
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

  // ---------------------------------------------------------------- 模型传输

  /**
   * 从本机 HTTP 服务把模型与词表下到沙箱。
   *
   * 为什么不是 `hdc file send`：应用沙箱目录是 `drwxrwx--- <app uid>`，而且即使
   * 把子目录改成 0777，shell 的 SELinux 域仍不允许写应用数据（实测 `touch` 与
   * `hdc file send` 都是 EACCES）。`/data/local/tmp` 应用进程又看不到（ENOENT）。
   * 所以走 `hdc rport tcp:8000 tcp:8000` 把宿主机的 HTTP 服务映射到设备
   * 127.0.0.1:8000，由**应用自己**流式写进沙箱：不经过 shell，也不把 239 MB
   * 打进 HAP。这是验证期的传输通道，不是产品行为。
   */
  static async downloadModels(filesDir: string): Promise<void> {
    const dir = `${filesDir}/asr/sense-voice`;
    ensureDir(dir);
    fileIO.chmodSync(dir, 0o777);
    await TranscriptionVerify.downloadOne('http://127.0.0.1:8000/model.int8.onnx', `${dir}/model.int8.onnx`);
    await TranscriptionVerify.downloadOne('http://127.0.0.1:8000/tokens.txt', `${dir}/tokens.txt`);
    try {
      hilog.info(DOMAIN, TAG, '%{public}s MODEL size=%{public}d tokens=%{public}d',
        MARK, fs.statSync(`${dir}/model.int8.onnx`).size, fs.statSync(`${dir}/tokens.txt`).size);
    } catch (e) {
      hilog.error(DOMAIN, TAG, '%{public}s MODEL stat failed %{public}s', MARK, describeError(e as Object));
    }
  }

  /** 流式下载一个文件：边收边写，避免把 239 MB 全装进内存。 */
  private static async downloadOne(url: string, dest: string): Promise<void> {
    const request = http.createHttp();
    const file = fs.openSync(dest, fs.OpenMode.CREATE | fs.OpenMode.READ_WRITE | fs.OpenMode.TRUNC);
    let bytes = 0;
    const onData = (chunk: ArrayBuffer): void => {
      fs.writeSync(file.fd, chunk);
      bytes += chunk.byteLength;
    };
    request.on('dataReceive', onData);
    const started = Date.now();
    let status = -1;
    let failure = '';
    try {
      status = await request.requestInStream(url, {
        method: http.RequestMethod.GET,
        connectTimeout: 60000,
        readTimeout: 600000,
      });
    } catch (e) {
      failure = describeError(e as Object);
    } finally {
      request.off('dataReceive', onData);
      request.destroy();
      fs.closeSync(file);
    }
    const elapsed = Date.now() - started;
    hilog.info(DOMAIN, TAG, '%{public}s DOWNLOAD url=%{public}s status=%{public}d bytes=%{public}d elapsedMs=%{public}d mbPerSec=%{public}s error=%{public}s',
      MARK, url, status, bytes, elapsed,
      elapsed > 0 ? (bytes / 1048576 / (elapsed / 1000)).toFixed(2) : '0', failure);
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
  ): Promise<void> {
    const dir = `${filesDir}/asr/sense-voice`;
    ensureDir(dir);
    const wavPath = `${dir}/zh.wav`;
    if (!fs.accessSync(wavPath) || fs.statSync(wavPath).size < 1000) {
      await TranscriptionVerify.downloadOne('http://127.0.0.1:8000/zh.wav', wavPath);
    }
    const pcm = readWavPcm(wavPath);
    hilog.info(DOMAIN, TAG, '%{public}s PLAY wav=%{public}s pcmBytes=%{public}d',
      MARK, wavPath, pcm.length);

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
    await renderer.start();
    hilog.info(DOMAIN, TAG, '%{public}s PLAY started renderer=%{public}d', MARK, renderer.state);
    await sleep(400);

    try {
      await RecordingVerify.run('record', context, filesDir);
    } finally {
      renderer.off('writeData', onWrite);
      await renderer.stop();
      await renderer.release();
      hilog.info(DOMAIN, TAG, '%{public}s PLAY stopped', MARK);
    }
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

/** 读 WAV 的 PCM 数据段（跳过 RIFF 头；找不到 `data` 就按 44 字节头处理）。 */
function readWavPcm(path: string): Uint8Array {
  const size = fs.statSync(path).size;
  const buffer = new ArrayBuffer(size);
  const file = fs.openSync(path, fs.OpenMode.READ_ONLY);
  try {
    fs.readSync(file.fd, buffer);
  } finally {
    fs.closeSync(file);
  }
  const bytes = new Uint8Array(buffer);
  let pos = 12;
  while (pos + 8 <= bytes.length) {
    const id = String.fromCharCode(bytes[pos], bytes[pos + 1], bytes[pos + 2], bytes[pos + 3]);
    const size = bytes[pos + 4] | (bytes[pos + 5] << 8) | (bytes[pos + 6] << 16) | (bytes[pos + 7] << 24);
    if (id === 'data') {
      const start = pos + 8;
      const end = Math.min(start + size, bytes.length);
      return bytes.slice(start, end);
    }
    pos += 8 + size + (size % 2);
  }
  return bytes.slice(44);
}

function sleep(ms: number): Promise<void> {
  return new Promise<void>((resolve) => {
    setTimeout(() => resolve(), ms);
  });
}

/** 建目录；已存在时不算错（`mkdirSync(.., true)` 在已存在时也会抛 File exists）。 */
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

// 鸿蒙产品 HAP 的「配置 + 语义检索 + 转写」同一次启动验证（#108 收尾片）。
//
// 只用于验证，不是产品 API。入口：
//
//   aa start -a EntryAbility -b com.lingxing.unwrittendiary -m entry \
//     --ps verify_mode hap_both --ps query=母亲体检的结果怎么样
//
// 这次要回答的问题（与任务书一一对应）：
//
// 1. `core-config.json` 真的生效：`core.info.runtime_config` 里两条能力都在，
//    并如实报出 `configSource` 与每个字段的来源；
// 2. 同一个 HAP 里的**两个 ORT** 能共存：`libonnxruntime.so`（sherpa 自带 1.16.3）
//    与 `libonnxruntime-diary-1.29.1.so`（自编 1.29.1，`ort` 按绝对路径加载）；
// 3. 同一次启动里 **先语义、再转写、再语义** 都正常（两个方向都不互相踩）。
//
// 时序（见 `HapBothWorker` 的注释）：
//
//   [主线程] 由探针**自己生成** `core-config.json`（不经 HTTP）；bge 模型仍经
//            rport→127.0.0.1:8000 下载（统一下载器，bytes+sha256 校验）
//   [主线程] 开一个锚句柄（整段验证期间不关，Core 与两个 ORT 的会话都留在进程里）
//   [worker] 阶段一：建日记 → 排关键词/语义索引 → 跑任务 → 语义检索（加载 1.29.1）
//   [主线程] 把一段**已知**的 5.59 s 中文 PCM（沙箱里的 zh.wav）按平台层约定登记成
//            录音片段 → finalize（排一条转写任务）。这一步替代麦克风采集：上一轮
//            已证明「扬声器→麦克风→文本」，而声学路径本身不稳定（本轮第一次真录
//            得到 "The."，上一轮也出现过「快饭/派饭」）；这一轮要验的是**双 ORT
//            进程里转写引擎仍然出文本**，用已知 PCM 才是确定的。
//   [worker] 阶段二：跑任务（sherpa + 1.16.3 首次加载，转写这一段）→ 重建索引
//            （把转写文本切块、用 1.29.1 算向量）→ 按 asset_id 语义检索到它 →
//            抽正文 → 再跑一次阶段一的查询（1.29.1 仍正常）
//   [主线程] 关锚句柄 → 打印总耗时/内存
//
// 日志 tag 是 `DiaryHap`，机器可读前缀 `DIARY_HAP`；长 JSON 与其它探针同口径按
// 900 字符分片。

import { common } from '@kit.AbilityKit';
import { fileIo as fs } from '@kit.CoreFileKit';
import { hilog } from '@kit.PerformanceAnalysisKit';
import { taskpool } from '@kit.ArkTS';
import { DiaryCore } from '../core/DiaryCore';
import {
  ExtractedContent,
  JobRunSummary,
  JobStatus,
  LibraryInfo,
  RecordingFinalizeResult,
  SearchHit,
  SearchSnapshot,
} from '../core/Models';
import { HapPhaseReport } from '../recording/HapBothOps';
import { runJobsOnly, runSemanticAfterAsr, runSemanticPhase } from '../recording/HapBothWorker';
import {
  downloadToFile,
  ExpectedFile,
  sha256OfFile,
} from '../recording/ModelDownload';
import { PcmSegmentWriter } from '../recording/PcmSegmentWriter';
import { readWavPcm } from '../recording/HapBothWav';

const DOMAIN: number = 0x0000;
const TAG: string = 'DiaryHap';
const MARK: string = 'DIARY_HAP';
const CHUNK: number = 900;

/** bge-small-zh-v1.5（我们自导出的量化版）在沙箱里的位置（相对 filesDir）。 */
const BGE_REL: string = 'models/bge-small-zh-v1.5-ours';
// 数值唯一事实来源：models/manifest/bge-small-zh-v1.5.json 的 files 数组。
const BGE_MODEL: ExpectedFile = {
  bytes: 23830209,
  sha256: 'd44978e451a0b4e0c561b8c6bb2cfaf5aac734f2729359aae46e1db893499f43',
};
const BGE_TOKENIZER: ExpectedFile = {
  bytes: 439125,
  sha256: '48cea5d44424912a6fd1ea647bf4fe50b55ab8b1e5879c3275f80e339e8fae26',
};

/** 探针生成的 `core-config.json` 形状（camelCase，与核心 `CoreConfigFile` 同形）。 */
interface HapConfig {
  modelDir: string;
  ortDylib: string;
  asrEngineLib: string;
  asrModelDir: string;
}

/** 转写文本的语义查询词：与常见识别结果（开饭/快饭/派饭…）语义最近。 */
const ASR_SEMANTIC_QUERY: string = '开饭时间';
/** 转写文本的关键词查询词（上一轮验过的旧路径，作为对照）。 */
const ASR_KEYWORD_QUERY: string = '开饭';

export class HapBothVerify {
  static async run(
    _mode: string,
    _context: common.UIAbilityContext,
    filesDir: string,
    query: string,
    order: string,
  ): Promise<void> {
    const totalStart = Date.now();
    // **临时库**：验证数据不往当前资料库写。放在 filesDir 下的独立子目录，
    // 配置也必须与库同目录（Core 从库文件所在目录读 core-config.json）。
    const tmpDir = `${filesDir}/hap-both-tmp`;
    cleanupTempDir(tmpDir);
    ensureDir(tmpDir);
    const dbPath = `${tmpDir}/library.sqlite`;
    // 查询词缺省时给一个与目标日记**没有共同词**的句子：命中只可能来自语义那一路。
    const semanticQuery = query.length > 0 ? query : '母亲体检的结果怎么样';
    hilog.info(DOMAIN, TAG, '%{public}s START filesDir=%{public}s db=%{public}s query=%{public}s nativeLibDir=%{public}s',
      MARK, filesDir, dbPath, semanticQuery, DiaryCore.nativeLibraryDir());

    // ── 1. 配置 + 模型进沙箱
    //    配置**由探针自己在沙箱里生成**（不经 HTTP）：它是产品真正会读的运行期配置，
    //    不能「下载即信任」。模型仍走 rport + 应用内下载（带 bytes+sha256 校验）。
    const configStart = Date.now();
    const configPath = `${tmpDir}/core-config.json`;
    const nativeDir = DiaryCore.nativeLibraryDir();
    const config: HapConfig = {
      modelDir: `${filesDir}/${BGE_REL}`,
      ortDylib: `${nativeDir}/libonnxruntime-diary-1.29.1.so`,
      asrEngineLib: `${nativeDir}/libsherpa-onnx-c-api.so`,
      asrModelDir: `${filesDir}/asr/sense-voice`,
    };
    writeText(configPath, `${JSON.stringify(config, null, 2)}\n`);
    hilog.info(DOMAIN, TAG, '%{public}s CONFIG_WRITE path=%{public}s source=probe（不走 HTTP）',
      MARK, configPath);
    HapBothVerify.logConfig(configPath);
    await HapBothVerify.ensureModel(filesDir);
    hilog.info(DOMAIN, TAG, '%{public}s STAGE wallMs=%{public}d', MARK, Date.now() - configStart);

    // ── 2. 锚句柄：整段验证期间不关，Core（与两个 ORT 的会话）留在进程里。
    //    worker 用同一个库路径 open，拿到的是同一个 Core（C ABI 的进程内登记表）。
    const anchor = DiaryCore.open(dbPath);
    try {
      HapBothVerify.logConfigReport('info.anchor', anchor.coreInfo());

      // ── 2b. 顺序开关：`--ps order asr_first` ⇒ 先把转写跑了（加载 sherpa + 1.16.3），
      //    再进阶段一加载 1.29.1。默认（空）是「语义先跑」。
      if (order === 'asr_first') {
        const firstStart = Date.now();
        const firstFinalize = HapBothVerify.injectKnownWav(anchor, filesDir);
        const firstRaw = await taskpool.execute(runJobsOnly, dbPath);
        const firstReport = JSON.parse(firstRaw as string) as HapPhaseReport;
        hilog.info(DOMAIN, TAG,
          '%{public}s ORDER asr_first asset=%{public}s wallMs=%{public}d runMs=%{public}d pssAfterKb=%{public}d error=%{public}s',
          MARK, firstFinalize.asset_id, Date.now() - firstStart, firstReport.run_ms,
          firstReport.pss_after_kb, firstReport.error);
        logJson('jobs.first', firstReport.summary);
      }

      // ── 3. 阶段一：语义先跑（加载自编 ORT 1.29.1）
      const suffix1 = `${Date.now()}-a`;
      const phaseStart1 = Date.now();
      const raw1 = await taskpool.execute(runSemanticPhase, dbPath, semanticQuery, suffix1);
      const phase1 = JSON.parse(raw1 as string) as HapPhaseReport;
      hilog.info(DOMAIN, TAG, '%{public}s PHASE phase=semantic_first wallMs=%{public}d',
        MARK, Date.now() - phaseStart1);
      HapBothVerify.logPhase(phase1);

      // ── 4. 转写：把已知 PCM 按平台层约定登记成录音片段，finalize 排转写任务。
      //    真麦克风采集上一轮验过；这里要的是**确定的**音频，才量得准「引擎在双 ORT
      //    进程里还出不出文本」。
      const asrStart = Date.now();
      const finalized = HapBothVerify.injectKnownWav(anchor, filesDir);
      hilog.info(DOMAIN, TAG,
        '%{public}s ASR_PHASE injectMs=%{public}d asset=%{public}s recording=%{public}s durationMs=%{public}d segments=%{public}d queued=%{public}s',
        MARK, Date.now() - asrStart, finalized.asset_id, finalized.recording_id,
        finalized.logical_duration_ms, finalized.segment_count,
        finalized.transcription_queued ? 'true' : 'false');

      // ── 5. 阶段二：转写之后再验语义（两个 ORT 同时在进程里）
      const suffix2 = `${Date.now()}-b`;
      const phaseStart2 = Date.now();
      const raw2 = await taskpool.execute(
        runSemanticAfterAsr, dbPath, finalized.asset_id, semanticQuery, ASR_SEMANTIC_QUERY,
        ASR_KEYWORD_QUERY, suffix2);
      const phase2 = JSON.parse(raw2 as string) as HapPhaseReport;
      hilog.info(DOMAIN, TAG, '%{public}s PHASE phase=semantic_after_asr wallMs=%{public}d',
        MARK, Date.now() - phaseStart2);
      HapBothVerify.logPhase(phase2);

      HapBothVerify.logConfigReport('info.final', anchor.coreInfo());
    } finally {
      anchor.close();
      hilog.info(DOMAIN, TAG, '%{public}s CLOSED handles=%{public}d',
        MARK, DiaryCore.openHandleCount());
      // 跑完清理临时库与配置（模型留在沙箱，避免下次重下 24 MB）。
      cleanupTempDir(tmpDir);
      hilog.info(DOMAIN, TAG, '%{public}s TEMP_CLEANED dir=%{public}s', MARK, tmpDir);
    }

    hilog.info(DOMAIN, TAG, '%{public}s DONE totalMs=%{public}d', MARK, Date.now() - totalStart);
  }

  // ---------------------------------------------------------------- 转写（已知 PCM）

  /**
   * 把沙箱里的 zh.wav（16 kHz / 单声道 / 16 bit PCM，5.59 s）登记成一条录音。
   *
   * 走的是**和麦克风完全一样**的平台层链路：`recordings.prepare` → 往 staging 目录
   * 写片段（第 0 段带 44 字节 WAV 头）→ `registerSegment`（核心重算哈希/字节数）→
   * `recordings.finalize`（同事务排 `transcribe` 任务）。
   *
   * 为什么不用麦克风：这一轮要量的是「双 ORT 共存」，不是声学路径。上一轮的
   * 「扬声器→麦克风」已经证明过链路，但同一条素材四次录出过「开饭/快饭/派饭」三种
   * 结果（本轮第一次真录甚至是 "The."）——拿它当转写证据，失败时说不清是引擎坏了
   * 还是没录到声音。已知 PCM 把变量收敛成一个：引擎有没有出文本。
   */
  private static injectKnownWav(core: DiaryCore, filesDir: string): RecordingFinalizeResult {
    const wavPath = `${filesDir}/asr/sense-voice/zh.wav`;
    const pcm = readWavPcm(wavPath);
    hilog.info(DOMAIN, TAG, '%{public}s INJECT wav=%{public}s pcmBytes=%{public}d',
      MARK, wavPath, pcm.length);

    const nonce = `${Date.now()}`;
    const capture = core.createDraft({
      time_zone: 'Asia/Shanghai',
      utc_offset_minutes: 480,
      operation_id: `op-hapcfg-inject-draft-${nonce}`,
    });
    const ticket = core.prepareRecording({
      capture_id: capture.id,
      operation_id: `op-hapcfg-inject-prepare-${nonce}`,
    });
    const writer = new PcmSegmentWriter(ticket.staging_ticket, 0);
    writer.open();
    writer.writePcm(pcm.buffer as ArrayBuffer);
    const file = writer.finish();
    const receipt = core.registerSegment({
      recording_id: ticket.recording_id,
      segment_index: file.index,
      segment: {
        segment_id: file.segmentId,
        relative_file_name: file.fileName,
        duration_ms: file.durationMs,
        sha256: file.sha256,
        byte_size: file.byteSize,
      },
    });
    hilog.info(DOMAIN, TAG,
      '%{public}s INJECT SEG index=%{public}d file=%{public}s bytes=%{public}d pcmMs=%{public}d sha256=%{public}s durable=%{public}s',
      MARK, file.index, file.fileName, file.byteSize, file.durationMs,
      file.sha256.substring(0, 16), receipt.durable ? 'true' : 'false');

    const result = core.finalizeRecording({
      recording_id: ticket.recording_id,
      last_segment_index: file.index,
      end_reason: 'hap_both_injected',
    });
    hilog.info(DOMAIN, TAG,
      '%{public}s INJECT FINALIZE asset=%{public}s audioMs=%{public}d segments=%{public}d state=%{public}s queued=%{public}s',
      MARK, result.asset_id, result.logical_duration_ms, result.segment_count,
      result.state, result.transcription_queued ? 'true' : 'false');
    return result;
  }

  // ---------------------------------------------------------------- 报告

  /** `core.info` 的配置部分单独打一行，便于 grep；完整 JSON 另按分片打。 */
  private static logConfigReport(label: string, info: LibraryInfo): void {
    logJson(label, info);
    const config = info.runtime_config;
    if (config === undefined) {
      hilog.warn(DOMAIN, TAG, '%{public}s CONFIG label=%{public}s missing runtime_config（C ABI 没带这一格）',
        MARK, label);
      return;
    }
    hilog.info(DOMAIN, TAG,
      '%{public}s CONFIG label=%{public}s filePresent=%{public}s fileError=%{public}s ignored=%{public}s',
      MARK, label, config.file.present ? 'true' : 'false',
      config.file.error === null ? '' : config.file.error,
      config.file.ignoredFields.join(','));
    hilog.info(DOMAIN, TAG,
      '%{public}s CONFIG label=%{public}s semantic.state=%{public}s semantic.source=%{public}s modelDirSource=%{public}s ortDylibSource=%{public}s modelDir=%{public}s ortDylib=%{public}s version=%{public}s reason=%{public}s',
      MARK, label, config.semantic.state, config.semantic.configSource,
      config.semantic.modelDirSource, config.semantic.ortDylibSource,
      config.semantic.modelDir === null ? '' : config.semantic.modelDir,
      config.semantic.ortDylib === null ? '' : config.semantic.ortDylib,
      config.semantic.modelVersion === null ? '' : config.semantic.modelVersion,
      config.semantic.reason === null ? '' : config.semantic.reason);
    hilog.info(DOMAIN, TAG,
      '%{public}s CONFIG label=%{public}s transcription.state=%{public}s transcription.source=%{public}s modelDirSource=%{public}s engineLibSource=%{public}s providerId=%{public}s modelDir=%{public}s engineLib=%{public}s reason=%{public}s',
      MARK, label, config.transcription.state, config.transcription.configSource,
      config.transcription.modelDirSource, config.transcription.engineLibSource,
      config.transcription.providerId === null ? '' : config.transcription.providerId,
      config.transcription.modelDir === null ? '' : config.transcription.modelDir,
      config.transcription.engineLib === null ? '' : config.transcription.engineLib,
      config.transcription.reason === null ? '' : config.transcription.reason);
  }

  private static logPhase(report: HapPhaseReport): void {
    hilog.info(DOMAIN, TAG,
      '%{public}s PHASE_REPORT phase=%{public}s openMs=%{public}d rebuildMs=%{public}d runMs=%{public}d pssBeforeKb=%{public}d pssAfterKb=%{public}d pssDeltaKb=%{public}d privateDeltaKb=%{public}d error=%{public}s',
      MARK, report.phase, report.open_ms, report.rebuild_ms, report.run_ms,
      report.pss_before_kb, report.pss_after_kb, report.pss_after_kb - report.pss_before_kb,
      report.private_after_kb - report.private_before_kb, report.error);
    if (report.debug.length > 0) {
      hilog.info(DOMAIN, TAG, '%{public}s DEBUG phase=%{public}s %{public}s',
        MARK, report.phase, report.debug);
    }
    hilog.info(DOMAIN, TAG, '%{public}s PHASE phase=%{public}s entries=%{public}s keywordJob=%{public}s semanticJob=%{public}s',
      MARK, report.phase, report.entries.join(','), report.keyword_job, report.semantic_job);

    logJson(`info.before.${report.phase}`, report.info_before);
    logJson(`jobs.summary.${report.phase}`, report.summary);
    logJson(`jobs.status.${report.phase}`, report.status);
    logJson(`info.after.${report.phase}`, report.info_after);
    HapBothVerify.logHits(`search.semantic.${report.phase}`, report.search_semantic);
    if (report.search_keyword.length > 0) {
      HapBothVerify.logHits(`search.keyword.${report.phase}`, report.search_keyword);
    }
    if (report.search_diary_after_asr.length > 0) {
      HapBothVerify.logHits(`search.diary.afterAsr.${report.phase}`, report.search_diary_after_asr);
    }
    if (report.extract_audio.length > 0) {
      const content = JSON.parse(report.extract_audio) as ExtractedContent;
      hilog.info(DOMAIN, TAG,
        '%{public}s TRANSCRIPT phase=%{public}s extractor=%{public}s coverage=%{public}s status=%{public}s text=%{public}s',
        MARK, report.phase, content.extractor_id, content.coverage, content.status, content.text);
      logJson(`extract.audio.${report.phase}`, content);
    } else {
      hilog.warn(DOMAIN, TAG, '%{public}s TRANSCRIPT phase=%{public}s 没有拿到音频来源（语义检索没命中转写文本）',
        MARK, report.phase);
    }
    HapBothVerify.logJobsSummary(report);
  }

  private static logJobsSummary(report: HapPhaseReport): void {
    if (report.summary.length === 0) {
      return;
    }
    const summary = JSON.parse(report.summary) as JobRunSummary;
    hilog.info(DOMAIN, TAG, '%{public}s JOBS phase=%{public}s processed=%{public}d succeeded=%{public}d failed=%{public}d waitingDependency=%{public}d dependencyFailed=%{public}d dueBacklog=%{public}d',
      MARK, report.phase, summary.processed, summary.succeeded, summary.failed,
      summary.waiting_dependency, summary.dependency_failed, summary.due_backlog);
    if (report.status.length > 0) {
      const status = JSON.parse(report.status) as JobStatus;
      hilog.info(DOMAIN, TAG, '%{public}s STATUS phase=%{public}s queued=%{public}d running=%{public}d succeeded=%{public}d failed=%{public}d waitingConfiguration=%{public}d waitingDependency=%{public}d',
        MARK, report.phase, status.queued, status.running, status.succeeded, status.failed,
        status.waiting_configuration, status.waiting_dependency);
    }
  }

  /** 命中的摘要（每条一行）+ 完整 JSON。`matchedBy` 是这次要看的字段。 */
  private static logHits(label: string, raw: string): void {
    if (raw.length === 0) {
      return;
    }
    const snapshot = JSON.parse(raw) as SearchSnapshot;
    hilog.info(DOMAIN, TAG, '%{public}s SEARCH label=%{public}s hits=%{public}d phase=%{public}s coverage=%{public}s warnings=%{public}s',
      MARK, label, snapshot.results.length, snapshot.phase, snapshot.index_coverage,
      snapshot.warnings.join(' | '));
    for (let i = 0; i < snapshot.results.length && i < 5; i++) {
      const hit: SearchHit = snapshot.results[i];
      hilog.info(DOMAIN, TAG,
        '%{public}s HIT label=%{public}s i=%{public}d kind=%{public}s matchedBy=%{public}s sourceId=%{public}s dayKey=%{public}s snippet=%{public}s',
        MARK, label, i, hit.source_kind, hit.matched_by.join(','),
        hit.source_id === null ? '' : hit.source_id,
        hit.day_key === null ? '' : hit.day_key,
        hit.snippet === null ? '' : hit.snippet);
    }
    logJson(label, snapshot);
  }

  // ---------------------------------------------------------------- 文件

  private static logConfig(configPath: string): void {
    try {
      const text = fs.readTextSync(configPath);
      hilog.info(DOMAIN, TAG, '%{public}s CONFIG_FILE path=%{public}s bytes=%{public}d text=%{public}s',
        MARK, configPath, text.length, text);
    } catch (e) {
      hilog.error(DOMAIN, TAG, '%{public}s CONFIG_FILE 读不到 %{public}s：%{public}s',
        MARK, configPath, JSON.stringify(e));
    }
  }

  /**
   * bge 模型进沙箱：**大小与 sha256 都对**才跳过；否则重下（下载器保证原子替换）。
   *
   * 不再只比字节数：24 MB 的文件「大小对、内容错」是可能的（代理错误页、半截文件
   * 恰好补齐）。哈希清单来自 `models/manifest/bge-small-zh-v1.5.json`。
   */
  private static async ensureModel(filesDir: string): Promise<void> {
    const dir = `${filesDir}/${BGE_REL}`;
    ensureDir(dir);
    await HapBothVerify.ensureFile(
      'http://127.0.0.1:8000/model_quantized.onnx', `${dir}/model_quantized.onnx`, BGE_MODEL);
    await HapBothVerify.ensureFile(
      'http://127.0.0.1:8000/tokenizer.json', `${dir}/tokenizer.json`, BGE_TOKENIZER);
  }

  private static async ensureFile(url: string, dest: string, expected: ExpectedFile): Promise<void> {
    let size = -1;
    try {
      size = fs.statSync(dest).size;
    } catch (e) {
      size = -1;
    }
    if (size === expected.bytes) {
      try {
        const actual = await sha256OfFile(dest);
        if (actual === expected.sha256) {
          hilog.info(DOMAIN, TAG, '%{public}s MODEL skip=%{public}s bytes=%{public}d sha256=%{public}s（已存在且校验一致）',
            MARK, dest, size, actual);
          return;
        }
        hilog.warn(DOMAIN, TAG, '%{public}s MODEL hash-mismatch=%{public}s actual=%{public}s expected=%{public}s（重下）',
          MARK, dest, actual, expected.sha256);
      } catch (e) {
        hilog.warn(DOMAIN, TAG, '%{public}s MODEL hash-failed=%{public}s %{public}s（重下）',
          MARK, dest, JSON.stringify(e));
      }
    }
    await downloadToFile(url, dest, expected);
    try {
      hilog.info(DOMAIN, TAG, '%{public}s MODEL file=%{public}s bytes=%{public}d expected=%{public}d',
        MARK, dest, fs.statSync(dest).size, expected.bytes);
    } catch (e) {
      hilog.error(DOMAIN, TAG, '%{public}s MODEL stat failed %{public}s：%{public}s',
        MARK, dest, JSON.stringify(e));
    }
  }
}

function logJson(label: string, value: Object | string): void {
  const text = typeof value === 'string' ? value : JSON.stringify(value);
  const chunks = Math.max(1, Math.ceil(text.length / CHUNK));
  for (let i = 0; i < chunks; i++) {
    hilog.info(DOMAIN, TAG, '%{public}s JSON[%{public}s][%{public}d/%{public}d] %{public}s',
      MARK, label, i + 1, chunks, text.substring(i * CHUNK, (i + 1) * CHUNK));
  }
}

/** 建目录；已存在时不算错（`mkdirSync(.., true)` 在已存在时也会抛 File exists）。 */
function ensureDir(path: string): void {
  if (fs.accessSync(path)) {
    return;
  }
  fs.mkdirSync(path, true);
}

/** 覆盖写一个文本文件（探针生成的 `core-config.json` 用；内容来自代码，不来自网络）。 */
function writeText(path: string, text: string): void {
  const file = fs.openSync(path, fs.OpenMode.CREATE | fs.OpenMode.READ_WRITE | fs.OpenMode.TRUNC);
  try {
    fs.writeSync(file.fd, text);
  } finally {
    fs.closeSync(file);
  }
}

/** 清掉临时库/配置目录（只删这个探针自己的文件；不动当前资料库）。 */
function cleanupTempDir(dir: string): void {
  const names = ['library.sqlite', 'library.sqlite-wal', 'library.sqlite-shm', 'core-config.json'];
  for (let i = 0; i < names.length; i++) {
    try {
      fs.unlinkSync(`${dir}/${names[i]}`);
    } catch (e) {
      // 不存在是正常情况。
    }
  }
  try {
    fs.rmdirSync(dir);
  } catch (e) {
    // 目录不存在或非空都无所谓；下次运行开头还会再清一次。
  }
}

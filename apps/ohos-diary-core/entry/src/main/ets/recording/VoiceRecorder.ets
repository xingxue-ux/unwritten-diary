// 语音日记的**采集 → 分片 → 落核心**（#108 ⑦ 第一片）。
//
// 一次录音的完整链路：
//
//   AudioCapturer（PCM 16 kHz / 单声道 / 16 bit）
//     → PcmSegmentWriter（每 N 秒封闭一段，写进核心给的 staging 目录）
//     → core.registerSegment（核心重算哈希/字节数，落 recording_segments）
//     → core.updateState（进度，供恢复判断缺口）
//     → 停止时 core.finalize（拼逻辑音频 + 建 Asset + 同事务排转写任务）
//
// 失败与中断的口径：
// - 采集出错**不**把会话标成 error（那样 `recordings.recover` 反而找不到它）；
//   会话保持 `recording`，已封闭片段照常登记，进程被杀后下次启动会变成 `recoverable`；
// - 停止时若一个片段都没有，不调用 finalize（核心会报「没有可用的已封闭片段」），
//   由调用方把错误原样带出去；
// - 长时任务失败不阻断前台录音，但结果会如实暴露（`taskResult()`）。
//
// 同步阻塞：`registerSegment`/`updateState`/`finalize` 都是同步核心调用（SQLite）。
// 片段封闭时登记一次（毫秒级），不在每个 `readData` 回调里做；进度一秒一次。
//
// **两个时长必须分开**（PR #127 评审抓到的语义 bug）：
//
//   receivedMs   = 采集器**实际收到**的 PCM 总时长（只增，含还没封闭的尾巴）
//   persistedMs  = **已成功 registerSegment** 的片段时长之和（真正落盘的音频）
//
// `recordings.updateState` 的第二个参数是核心的 `persisted_through_ms`（落库列
// `durable_through_ms`），恢复时核心用它判断「音频到哪了」。所以那里**只能**用
// `persistedMs`；用 `receivedMs` 会把两次登记之间（最多一个片段）还在内存/未封闭
// 文件里的尾巴谎报成「已持久化」——进程崩溃后用户看到 3.5 秒、实际只有 3 秒，
// 而且没有任何提示。`receivedMs - persistedMs` 就是「可能丢失的尾巴」，要如实暴露。

import { audio } from '@kit.AudioKit';
import { fileIo as fs } from '@kit.CoreFileKit';
import { common } from '@kit.AbilityKit';
import { systemDateTime } from '@kit.BasicServicesKit';
import { hilog } from '@kit.PerformanceAnalysisKit';
import { DiaryCore } from '../core/DiaryCore';
import { DiaryError } from '../core/DiaryError';
import {
  NativeRecordingStatusParams,
  RecordingFinalizeResult,
  RecordingTicket,
  SegmentManifest,
} from '../core/Models';
import { ContinuousTaskController, ContinuousTaskResult } from './ContinuousTask';
import { PcmSegmentWriter, SegmentFile } from './PcmSegmentWriter';
import { KeepScreenOnResult, ScreenKeepOn } from './ScreenKeepOn';
import {
  SAMPLE_RATE,
  CHANNEL_COUNT,
  BITS_PER_SAMPLE,
  msToPcmBytes,
  pcmBytesToMs,
} from './RecordingFormat';

const DOMAIN: number = 0x0000;
const TAG: string = 'DiaryRec';
const MARK: string = 'DIARY_REC';

/** 一次录音的配置。默认 2 秒一段：5 秒的验证会真实轮转出 3 段。 */
export class RecorderOptions {
  segmentMs: number = 2000;
  /** 记录在核心 `end_reason` 里的停止原因。 */
  endReason: string = 'user_stop';
}

/** 采集器封装：一个实例只录一次（下一次要 new）。 */
export class VoiceRecorder {
  private readonly core: DiaryCore;
  private readonly context: common.UIAbilityContext;
  private readonly options: RecorderOptions;
  private readonly continuous: ContinuousTaskController = new ContinuousTaskController();
  private readonly screen: ScreenKeepOn = new ScreenKeepOn();

  private ticket: RecordingTicket | null = null;
  private capturer: audio.AudioCapturer | null = null;
  private writer: PcmSegmentWriter | null = null;
  private bgTask: ContinuousTaskResult = new ContinuousTaskResult();
  private keepScreen: KeepScreenOnResult = new KeepScreenOnResult();

  private segmentIndex: number = 0;
  private lastRegistered: number = -1;
  /** 采集器实际收到的 PCM 字节数（只增；换算成 `receivedMs`）。 */
  private totalPcmBytes: number = 0;
  /** 已成功登记的片段时长之和（只有它算「已持久化」）。 */
  private persistedPcmMs: number = 0;
  private phase: string = 'idle';
  private errorText: string = '';
  /** 采集器被系统中断（锁屏/来电/抢占）时置位；停止时写进 end_reason，不假装没发生。 */
  private interrupted: boolean = false;

  private startRequestedMs: number = 0;
  private capturerStartRequestedMs: number = 0;
  private firstDataUptimeMs: number = -1;
  private lastProgressUptimeMs: number = 0;
  private stopRequestedMs: number = 0;

  constructor(core: DiaryCore, context: common.UIAbilityContext, options: RecorderOptions) {
    this.core = core;
    this.context = context;
    this.options = options;
  }

  // ---------------------------------------------------------------- 状态

  currentPhase(): string {
    return this.phase;
  }

  recordingId(): string {
    return this.ticket !== null ? this.ticket.recording_id : '';
  }

  /** 已登记的片段数。 */
  segmentCount(): number {
    return this.lastRegistered + 1;
  }

  /**
   * 采集器**实际收到**的 PCM 总时长（只增；按字节算，不按墙上时间）。
   *
   * 含还没封闭、没登记的尾巴，所以**不能**当 `persisted_through_ms` 上报。
   */
  receivedMs(): number {
    return pcmBytesToMs(this.totalPcmBytes);
  }

  /** **已成功登记**片段时长之和：真正落盘、`finalize` 能拼出来的那部分音频。 */
  persistedMs(): number {
    return this.persistedPcmMs;
  }

  /** `receivedMs - persistedMs`：还没登记、进程崩溃就会丢的尾巴（下界为 0）。 */
  unregisteredTailMs(): number {
    return Math.max(0, this.receivedMs() - this.persistedMs());
  }

  taskResult(): ContinuousTaskResult {
    return this.bgTask;
  }

  /** 录音期间「保持屏幕常亮」的设置结果（失败也要能看见）。 */
  keepScreenResult(): KeepScreenOnResult {
    return this.keepScreen;
  }

  firstDataLatencyMs(): number {
    if (this.firstDataUptimeMs < 0 || this.capturerStartRequestedMs <= 0) {
      return -1;
    }
    return this.firstDataUptimeMs - this.capturerStartRequestedMs;
  }

  error(): string {
    return this.errorText;
  }

  /** 这次录音是否被系统中断过（锁屏、来电、焦点被抢）。 */
  wasInterrupted(): boolean {
    return this.interrupted;
  }

  // ---------------------------------------------------------------- 开始

  /**
   * 建草稿 → 申请票据 → 长时任务 → AudioCapturer → 上报 recording。
   *
   * 任一步失败都会清理已经拿到的资源（采集器/长时任务/半截片段），并抛 `DiaryError`
   * 或系统错误；**不会**留下「用户以为在录、其实没录」的状态。
   */
  async start(): Promise<void> {
    if (this.phase !== 'idle') {
      throw new DiaryError('invalid_state', `录音器已经是 ${this.phase}，不能重复开始`);
    }
    this.startRequestedMs = nowUptimeMs();
    const now = new Date();
    const timeZone = readTimeZone();
    const offsetMinutes = -now.getTimezoneOffset();
    const nonce = `${now.getTime()}`;

    try {
      const capture = this.core.createDraft({
        occurred_at: now.toISOString(),
        time_zone: timeZone,
        utc_offset_minutes: offsetMinutes,
        operation_id: `op-ohos-draft-${nonce}`,
      });
      this.phase = 'preparing';
      hilog.info(DOMAIN, TAG, '%{public}s CAPTURE draft id=%{public}s tz=%{public}s offset=%{public}d',
        MARK, capture.id, timeZone, offsetMinutes);

      this.ticket = this.core.prepareRecording({
        capture_id: capture.id,
        operation_id: `op-ohos-prepare-${nonce}`,
      });
      hilog.info(DOMAIN, TAG, '%{public}s PREPARE recording=%{public}s asset=%{public}s staging=%{public}s',
        MARK, this.ticket.recording_id, this.ticket.asset_id, this.ticket.staging_ticket);

      // 长时任务先申请（这时 Ability 还在前台）；失败不阻断前台录音，但如实记录。
      this.bgTask = await this.continuous.start(this.context);

      // 录音期间保持屏幕常亮：熄屏会被系统强制 STOP 采集（见文档 §7.3）。
      // 失败不阻断录音，但结果如实暴露；停止/异常/abort 都会关掉。
      this.keepScreen = await this.screen.enable(this.context);
      hilog.info(DOMAIN, TAG, '%{public}s KEEP_SCREEN_ON ok=%{public}s code=%{public}d message=%{public}s',
        MARK, this.keepScreen.ok ? 'true' : 'false',
        this.keepScreen.error_code, this.keepScreen.error_message);

      // 第 0 段先建文件、写好 WAV 头；这样第一块 PCM 到达时不会因为建文件抖动。
      this.openSegment(0);

      const streamInfo: audio.AudioStreamInfo = {
        samplingRate: audio.AudioSamplingRate.SAMPLE_RATE_16000,
        channels: audio.AudioChannel.CHANNEL_1,
        sampleFormat: audio.AudioSampleFormat.SAMPLE_FORMAT_S16LE,
        encodingType: audio.AudioEncodingType.ENCODING_TYPE_RAW,
      };
      const capturerInfo: audio.AudioCapturerInfo = {
        source: audio.SourceType.SOURCE_TYPE_MIC,
        capturerFlags: 0,
      };
      const capturerOptions: audio.AudioCapturerOptions = {
        streamInfo: streamInfo,
        capturerInfo: capturerInfo,
      };
      this.capturer = await audio.createAudioCapturer(capturerOptions);
      this.capturer.on('readData', (buffer: ArrayBuffer) => {
        this.handleReadData(buffer);
      });
      // 采集器的状态/焦点变化：锁屏、来电、别的应用抢麦克风时，系统会改状态或发中断事件。
      // 这里只如实记录（不假装恢复）；排查「为什么没声音」时这两条日志是入口。
      this.capturer.on('stateChange', (state: audio.AudioState) => {
        if (this.phase === 'recording' && state !== audio.AudioState.STATE_RUNNING) {
          this.interrupted = true;
          hilog.warn(DOMAIN, TAG, '%{public}s CAPTURE INTERRUPTED state=%{public}d phase=%{public}s',
            MARK, state, this.phase);
        } else {
          hilog.info(DOMAIN, TAG, '%{public}s CAPTURE state=%{public}d phase=%{public}s',
            MARK, state, this.phase);
        }
      });
      this.capturer.on('audioInterrupt', (event: audio.InterruptEvent) => {
        hilog.warn(DOMAIN, TAG, '%{public}s CAPTURE interrupt eventType=%{public}d forceType=%{public}d hintType=%{public}d phase=%{public}s',
          MARK, event.eventType, event.forceType, event.hintType, this.phase);
      });

      this.reportState('recording', 0, 0, '');
      this.capturerStartRequestedMs = nowUptimeMs();
      await this.capturer.start();
      this.phase = 'recording';
      this.lastProgressUptimeMs = nowUptimeMs();
      hilog.info(DOMAIN, TAG, '%{public}s START recording=%{public}s sampleRate=%{public}d channels=%{public}d bits=%{public}d segmentMs=%{public}d bgTask=%{public}s',
        MARK, this.ticket.recording_id, SAMPLE_RATE, CHANNEL_COUNT, BITS_PER_SAMPLE,
        this.options.segmentMs, this.bgTask.ok ? 'ok' : `failed(${this.bgTask.error_code})`);
    } catch (e) {
      hilog.error(DOMAIN, TAG, '%{public}s START failed %{public}s',
        MARK, describeError(e as Object));
      this.phase = 'error';
      this.errorText = describeError(e as Object);
      this.cleanupAfterFailure();
      throw DiaryError.fromThrown(e as Object);
    }
  }

  // ---------------------------------------------------------------- 停止

  /**
   * 停止采集 → 封闭最后一段并登记 → 上报 stopping → finalize。
   *
   * `phase === 'error'` 时也允许调用：把已经录到的片段救回来（`end_reason` 用调用方给的值，
   * 建议是 `capture_error`）。
   */
  async stop(reason: string): Promise<RecordingFinalizeResult> {
    if (this.phase !== 'recording' && this.phase !== 'error') {
      throw new DiaryError('invalid_state', `录音器是 ${this.phase}，不能停止`);
    }
    const ticket = this.ticket;
    if (ticket === null) {
      throw new DiaryError('invalid_state', '没有申请到录音票据，无法停止');
    }
    this.phase = 'stopping';
    this.stopRequestedMs = nowUptimeMs();

    // **整个收尾都在这层 try 里**：`writer.finish()`（封闭尾段）与 `reportState()`
    // 都在原来的 try/finally 之外；它们一抛错，phase 就停在 `stopping`，而屏幕常亮
    // 与长时任务只会在 finalize 的 finally / 无片段分支里释放 ⇒ 永远不释放。
    // 现在无论哪一步抛错都先走 finally；异常本身照常往上抛（不吞）。
    // 用 `primaryError` 记下「已经有异常在飞」，避免 finally 里的释放失败
    // 覆盖掉真正的原因（finally 抛错会替换原异常）。
    let primaryError: Object | null = null;
    try {
      // 1. 停采集器。`stop()` 失败不该阻止后续收尾（可能本来就没跑起来）。
      await this.stopCapturer();

      // 2. 封闭最后一段；没有 PCM 的尾巴直接删掉，不登记空片段。
      //    只有登记**成功**才把它算进 persistedMs；失败就保持旧值，并写进 message，
      //    让核心的 `error_message` 与恢复日志都能看见「最后一段没登记」。
      let tailMessage = '';
      if (this.writer !== null) {
        const file = this.writer.finish();
        this.writer = null;
        if (file.pcmBytes > 0) {
          try {
            this.register(file);
          } catch (e) {
            tailMessage = `最后一段（序号 ${file.index}，${file.durationMs} ms）登记失败：${describeError(e as Object)}`;
            hilog.error(DOMAIN, TAG, '%{public}s SEG tail register failed %{public}s',
              MARK, tailMessage);
          }
        } else {
          hilog.info(DOMAIN, TAG, '%{public}s SEG drop index=%{public}d reason=empty bytes=%{public}d',
            MARK, file.index, file.byteSize);
          try {
            fs_unlink(file.path);
          } catch (e) {
            // 删不掉也不影响核心：没有登记就不会进最终化。
          }
        }
      }

      // 3. 先上报 stopping（核心不虚构麦克风状态），再最终化。
      //    第二个参数**只用 persistedMs**：尾段登记失败时它还是旧值，绝不虚报「已持久化」。
      this.reportState('stopping', this.receivedMs(), this.persistedMs(), tailMessage);
      hilog.info(DOMAIN, TAG, '%{public}s STOP_PROGRESS receivedMs=%{public}d persistedMs=%{public}d unregisteredTailMs=%{public}d equal=%{public}s',
        MARK, this.receivedMs(), this.persistedMs(), this.unregisteredTailMs(),
        this.receivedMs() === this.persistedMs() ? 'true' : 'false');

      if (this.lastRegistered < 0) {
        // 外层 finally 会释放 hold；这里不重复调，异常照常抛给调用方。
        throw new DiaryError('invalid_state', '没有可用的已封闭片段，核心会拒绝最终化（先检查麦克风权限/采集回调）');
      }

      try {
        const effectiveReason = this.interrupted ? `${reason}_interrupted` : reason;
        if (this.interrupted) {
          hilog.warn(DOMAIN, TAG, '%{public}s STOP interrupted=true endReason=%{public}s',
            MARK, effectiveReason);
        }
        const result = this.core.finalizeRecording({
          recording_id: ticket.recording_id,
          last_segment_index: this.lastRegistered,
          end_reason: effectiveReason,
        });
        this.phase = result.state;
        hilog.info(DOMAIN, TAG, '%{public}s FINALIZE %{public}s',
          MARK, JSON.stringify(result));
        hilog.info(DOMAIN, TAG, '%{public}s STOP recording=%{public}s segments=%{public}d receivedMs=%{public}d persistedMs=%{public}d audioMs=%{public}d stopLatencyMs=%{public}d',
          MARK, ticket.recording_id, result.segment_count, this.receivedMs(),
          this.persistedMs(), result.logical_duration_ms,
          nowUptimeMs() - this.stopRequestedMs);
        return result;
      } catch (e) {
        this.phase = 'error';
        this.errorText = describeError(e as Object);
        hilog.error(DOMAIN, TAG, '%{public}s FINALIZE failed %{public}s',
          MARK, this.errorText);
        throw DiaryError.fromThrown(e as Object);
      }
    } catch (e) {
      primaryError = e as Object;
      throw e;
    } finally {
      // 无条件释放「保持屏幕常亮」与长时任务。`releaseHold()` 自身幂等。
      // 已经有异常时：先释放，释放再失败只记日志，别覆盖真正的故障；
      // 一切正常时：释放失败照常抛给调用方（不能假装已经收掉）。
      if (primaryError === null) {
        await this.releaseHold();
      } else {
        try {
          await this.releaseHold();
        } catch (e) {
          hilog.warn(DOMAIN, TAG, '%{public}s RELEASE failed while propagating %{public}s: %{public}s',
            MARK, describeError(primaryError), describeError(e as Object));
        }
      }
    }
  }

  /** 放弃：停采集、删掉当前半截片段、停长时任务；**不**最终化（留给下次启动 recover）。 */
  async abort(): Promise<void> {
    hilog.warn(DOMAIN, TAG, '%{public}s ABORT phase=%{public}s segments=%{public}d',
      MARK, this.phase, this.segmentCount());
    this.phase = 'aborted';
    await this.stopCapturer();
    if (this.writer !== null) {
      this.writer.abort();
      this.writer = null;
    }
    await this.releaseHold();
  }

  // ---------------------------------------------------------------- 采集回调

  private handleReadData(buffer: ArrayBuffer): void {
    if (this.phase !== 'recording' || this.writer === null) {
      return;
    }
    try {
      this.writer.writePcm(buffer);
      this.totalPcmBytes += buffer.byteLength;
      const now = nowUptimeMs();
      if (this.firstDataUptimeMs < 0) {
        this.firstDataUptimeMs = now;
        hilog.info(DOMAIN, TAG, '%{public}s FIRST_DATA bytes=%{public}d captureLatencyMs=%{public}d totalLatencyMs=%{public}d',
          MARK, buffer.byteLength, this.firstDataUptimeMs - this.capturerStartRequestedMs,
          this.firstDataUptimeMs - this.startRequestedMs);
      }

      // 分片轮转：按**实际收到的 PCM 字节数**，不按墙上时间（卡顿/停顿不会把段拉长）。
      if (this.writer.pcmBytes() >= msToPcmBytes(this.options.segmentMs)) {
        this.rotateSegment();
      }

      // 进度一秒一次：`elapsed_ms` 用收到的量，`durable_through_ms` 只用已登记的片段。
      if (now - this.lastProgressUptimeMs >= 1000) {
        this.lastProgressUptimeMs = now;
        this.reportState('recording', this.receivedMs(), this.persistedMs(), '');
        hilog.info(DOMAIN, TAG, '%{public}s PROGRESS receivedMs=%{public}d persistedMs=%{public}d unregisteredTailMs=%{public}d segments=%{public}d',
          MARK, this.receivedMs(), this.persistedMs(), this.unregisteredTailMs(),
          this.segmentCount());
      }
    } catch (e) {
      this.fail('readData', e as Object);
    }
  }

  // ---------------------------------------------------------------- 内部

  private openSegment(index: number): void {
    const ticket = this.ticket;
    if (ticket === null) {
      throw new DiaryError('invalid_state', '没有录音票据，不能建片段文件');
    }
    this.segmentIndex = index;
    const writer = new PcmSegmentWriter(ticket.staging_ticket, index);
    writer.open();
    this.writer = writer;
    hilog.info(DOMAIN, TAG, '%{public}s SEG open index=%{public}d bytes=%{public}d',
      MARK, index, writer.byteSize());
  }

  private rotateSegment(): void {
    const writer = this.writer;
    if (writer === null) {
      return;
    }
    const file = writer.finish();
    this.writer = null;
    this.register(file);
    this.openSegment(file.index + 1);
  }

  /** 登记一个已封闭片段。核心会自己重算哈希/字节数；失败时把错误抛给调用方。 */
  private register(file: SegmentFile): void {
    const ticket = this.ticket;
    if (ticket === null) {
      throw new DiaryError('invalid_state', '没有录音票据，不能登记片段');
    }
    const manifest: SegmentManifest = {
      segment_id: file.segmentId,
      relative_file_name: file.fileName,
      duration_ms: file.durationMs,
      sha256: file.sha256,
      byte_size: file.byteSize,
    };
    const receipt = this.core.registerSegment({
      recording_id: ticket.recording_id,
      segment_index: file.index,
      segment: manifest,
    });
    this.lastRegistered = file.index;
    // **只有登记成功**才把这段的时长算进「已持久化」（抛错时走不到这里）。
    this.persistedPcmMs += file.durationMs;
    hilog.info(DOMAIN, TAG, '%{public}s SEG index=%{public}d file=%{public}s bytes=%{public}d pcmMs=%{public}d persistedMs=%{public}d sha256=%{public}s durable=%{public}s',
      MARK, file.index, file.fileName, file.byteSize, file.durationMs,
      this.persistedPcmMs, file.sha256.substring(0, 16),
      receipt.durable ? 'true' : 'false');
  }

  /**
   * 立刻上报一次进度（不等一秒的节拍）。
   *
   * 崩溃用例在 `process.abort()` 前调用它：让核心里的「收到量」在崩溃瞬间是**准确值**，
   * 这样恢复时 `received - persisted` 是真实丢掉的尾巴，而不是「上一拍到现在」的时间差。
   */
  flushProgress(label: string): void {
    if (this.phase !== 'recording') {
      return;
    }
    this.lastProgressUptimeMs = nowUptimeMs();
    this.reportState('recording', this.receivedMs(), this.persistedMs(), '');
    hilog.info(DOMAIN, TAG, '%{public}s FLUSH_PROGRESS label=%{public}s receivedMs=%{public}d persistedMs=%{public}d unregisteredTailMs=%{public}d segments=%{public}d',
      MARK, label, this.receivedMs(), this.persistedMs(), this.unregisteredTailMs(),
      this.segmentCount());
  }

  private reportState(state: string, receivedMs: number, persistedMs: number, message: string): void {
    const ticket = this.ticket;
    if (ticket === null) {
      return;
    }
    const status: NativeRecordingStatusParams = {
      recording_id: ticket.recording_id,
      state: state,
      wall_clock: new Date().toISOString(),
      elapsed_ms: receivedMs,
      persisted_through_ms: persistedMs,
      input_device_changed: false,
      message: message.length > 0 ? message : null,
    };
    this.core.updateRecordingState(status);
  }

  /** 收掉「保持屏幕常亮」与长时任务；重复调用安全。 */
  private async releaseHold(): Promise<void> {
    await this.screen.disable();
    await this.continuous.stop(this.context);
  }

  private async stopCapturer(): Promise<void> {
    const capturer = this.capturer;
    if (capturer === null) {
      return;
    }
    this.capturer = null;
    try {
      await capturer.stop();
    } catch (e) {
      hilog.warn(DOMAIN, TAG, '%{public}s CAPTURE stop failed %{public}s',
        MARK, describeError(e as Object));
    }
    try {
      await capturer.release();
    } catch (e) {
      hilog.warn(DOMAIN, TAG, '%{public}s CAPTURE release failed %{public}s',
        MARK, describeError(e as Object));
    }
  }

  private fail(where: string, raw: Object): void {
    this.phase = 'error';
    this.errorText = describeError(raw);
    hilog.error(DOMAIN, TAG, '%{public}s ERROR[%{public}s] %{public}s',
      MARK, where, this.errorText);
    // 会话故意保持 `recording`：进程真被杀时，下次打开会由核心标成 recoverable。
    const capturer = this.capturer;
    this.capturer = null;
    if (capturer !== null) {
      capturer.stop().catch((e: Object) => {
        hilog.warn(DOMAIN, TAG, '%{public}s ERROR stop capturer failed %{public}s',
          MARK, describeError(e));
      });
      capturer.release().catch((e: Object) => {
        hilog.warn(DOMAIN, TAG, '%{public}s ERROR release capturer failed %{public}s',
          MARK, describeError(e));
      });
    }
    this.releaseHold().catch((e: Object) => {
      hilog.warn(DOMAIN, TAG, '%{public}s ERROR release hold failed %{public}s',
        MARK, describeError(e));
    });
  }

  private cleanupAfterFailure(): void {
    const capturer = this.capturer;
    this.capturer = null;
    if (capturer !== null) {
      capturer.stop().catch((e: Object) => {
        hilog.warn(DOMAIN, TAG, '%{public}s CLEANUP stop failed %{public}s',
          MARK, describeError(e));
      });
      capturer.release().catch((e: Object) => {
        hilog.warn(DOMAIN, TAG, '%{public}s CLEANUP release failed %{public}s',
          MARK, describeError(e));
      });
    }
    if (this.writer !== null) {
      this.writer.abort();
      this.writer = null;
    }
    this.releaseHold().catch((e: Object) => {
      hilog.warn(DOMAIN, TAG, '%{public}s CLEANUP release hold failed %{public}s',
        MARK, describeError(e));
    });
  }
}

/** 单调毫秒（微秒级精度；不用墙上时间，避免改时间影响时长）。 */
export function nowUptimeMs(): number {
  return systemDateTime.getUptime(systemDateTime.TimeType.STARTUP, true) / 1000000;
}

/** 系统时区名（例如 `Asia/Shanghai`）；失败时退回 UTC。 */
export function readTimeZone(): string {
  try {
    const zone = systemDateTime.getTimezoneSync();
    return zone.length > 0 ? zone : 'UTC';
  } catch (e) {
    return 'UTC';
  }
}

/** 把任意异常统一成 `code=… message=…` 的日志行（与 DiaryError.toLogLine 同口径）。 */
export function describeError(raw: Object): string {
  const err = raw instanceof DiaryError ? raw : DiaryError.fromThrown(raw);
  return err.toLogLine();
}

/** 删除文件的小工具（没有 PCM 的尾巴不进核心）。 */
function fs_unlink(path: string): void {
  fs.unlinkSync(path);
}

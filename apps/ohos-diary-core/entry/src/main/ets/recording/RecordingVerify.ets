// 真机验证探针：录音采集 → 分片 → 落核心（#108 ⑦ 第一片）。
//
// 只用于验证，不是产品 API。入口：
//   aa start -a EntryAbility -b <bundle> -m entry --ps verify_mode record       # 5 秒
//   aa start -a EntryAbility -b <bundle> -m entry --ps verify_mode record_bg    # 8 秒（配合 Home 键）
//   aa start -a EntryAbility -b <bundle> -m entry --ps verify_mode record_hold  # 一直录，等外部 kill
//   aa start -a EntryAbility -b <bundle> -m entry --ps verify_mode record_crash_tail # 3.5 秒后崩溃（尾段未登记）
//   aa start -a EntryAbility -b <bundle> -m entry --ps verify_mode recover      # 只查恢复
//
// 日志 tag 是 `DiaryRec`，机器可读前缀 `DIARY_REC`；长 JSON 按 900 字符分片，
// 与 `DIARY_SMOKE JSON[...]` 同口径（拼起来是合法 JSON）。

import { common } from '@kit.AbilityKit';
import { fileIo as fs } from '@kit.CoreFileKit';
import { settings } from '@kit.BasicServicesKit';
import { process } from '@kit.ArkTS';
import { hilog } from '@kit.PerformanceAnalysisKit';
import { DiaryCore } from '../core/DiaryCore';
import { RecordingFinalizeResult, RecordingRecovery } from '../core/Models';
import { ensureMicrophonePermission } from './MicPermission';
import { RecorderOptions, VoiceRecorder, describeError, nowUptimeMs } from './VoiceRecorder';

const DOMAIN: number = 0x0000;
const TAG: string = 'DiaryRec';
const MARK: string = 'DIARY_REC';
const CHUNK: number = 900;

/** 验证模式的时长（毫秒）。hold 只用于「外部 kill 后 recover」的用例。 */
function durationFor(mode: string): number {
  if (mode === 'record_bg') {
    return 8000;
  }
  if (mode === 'record_crash') {
    return 6000;
  }
  if (mode === 'record_crash_tail') {
    // 1.5 秒一段 ⇒ 3.5 秒时只有第 0/1 段（共 3.0 秒）登记成功，序号 2 那段还开着。
    return 3500;
  }
  if (mode === 'record_hold') {
    return 0;
  }
  return 5000;
}

export class RecordingVerify {
  /** hold 模式要一直持有，否则函数返回后 core 会被 close。 */
  private static heldCore: DiaryCore | null = null;
  private static heldRecorder: VoiceRecorder | null = null;
  /** 最近一次 `recover` 的结果：页面/调用方要能显示「可能丢失的尾巴」。 */
  private static lastRecovery: RecordingRecovery | null = null;

  /**
   * 上次恢复的「已保存 / 可能丢失」摘要（给界面直接显示，不隐瞒尾巴）。
   *
   * 可用时长只有 `persisted_ms`（已登记片段之和）；`received_ms - persisted_ms`
   * 是崩溃时还没登记、已经丢掉的尾巴（`received_ms` 只是最后一次上报值，故说「约」）。
   */
  static recoveryHint(): string {
    const item = RecordingVerify.lastRecovery;
    if (item === null || item.recording_id === null) {
      return '没有未收尾的录音';
    }
    const seconds = (ms: number): string => (ms / 1000).toFixed(1);
    const tail = Math.max(0, item.received_ms - item.persisted_ms);
    if (tail > 0) {
      return `上次录音：已保存 ${seconds(item.persisted_ms)} 秒；崩溃前还有约 ${seconds(tail)} 秒未保存（不完整）`;
    }
    return `上次录音：已保存 ${seconds(item.persisted_ms)} 秒，没有可见的未登记尾巴`;
  }

  static async run(mode: string, context: common.UIAbilityContext, filesDir: string): Promise<void> {
    hilog.info(DOMAIN, TAG, '%{public}s VERIFY mode=%{public}s pid=%{public}d filesDir=%{public}s',
      MARK, mode, process.pid, filesDir);
    logScreenTimeout(context);

    const core = DiaryCore.openInDirectory(filesDir);
    try {
      // 每次启动都先看「上次有没有没走完的录音」——这就是产品启动时该做的事。
      const recovery = core.recoverRecording({});
      RecordingVerify.lastRecovery = recovery;
      logJson('recover.before', recovery);
      if (recovery.recording_id !== null) {
        // 「已收到」与「已登记」的差必须显式打出来：它就是可能丢掉的尾巴。
        hilog.info(DOMAIN, TAG, '%{public}s RECOVER recording=%{public}s state=%{public}s receivedMs=%{public}d persistedMs=%{public}d unregisteredTailMs=%{public}d segments=%{public}d',
          MARK, recovery.recording_id, recovery.state, recovery.received_ms,
          recovery.persisted_ms, Math.max(0, recovery.received_ms - recovery.persisted_ms),
          recovery.closed_segment_indexes.length);
      }

      if (mode === 'recover') {
        const info = core.coreInfo();
        logJson('core.info', info);
        return;
      }

      if (mode === 'recover_finalize') {
        // 把「上次没走完」的会话按已封闭序号救回来（产品上是否自动救由界面决定；
        // 这里证明核心的 finalize 对恢复出来的会话同样可用）。
        let finalized = 0;
        for (let i = 0; i < 5; i++) {
          const item = core.recoverRecording({});
          if (item.recording_id === null) {
            break;
          }
          if (item.closed_segment_indexes.length === 0) {
            logJson('recover.no_segments', item);
            break;
          }
          const last = item.closed_segment_indexes[item.closed_segment_indexes.length - 1];
          const result = core.finalizeRecording({
            recording_id: item.recording_id,
            last_segment_index: last,
            end_reason: 'recovered_on_startup',
          });
          logJson('recover.finalize', result);
          // 恢复出的音频时长必须等于「已登记片段之和」，不是「已收到」。
          hilog.info(DOMAIN, TAG, '%{public}s RECOVER_FINALIZE audioMs=%{public}d receivedMs=%{public}d persistedMs=%{public}d unregisteredTailMs=%{public}d segments=%{public}d',
            MARK, result.logical_duration_ms, item.received_ms, item.persisted_ms,
            Math.max(0, item.received_ms - item.persisted_ms), result.segment_count);
          finalized++;
        }
        hilog.info(DOMAIN, TAG, '%{public}s RECOVER_FINALIZE count=%{public}d', MARK, finalized);
        logJson('core.info', core.coreInfo());
        return;
      }

      const permission = await ensureMicrophonePermission(context);
      if (!permission.granted) {
        hilog.error(DOMAIN, TAG, '%{public}s PERM DENIED reason=%{public}s authResultsLen=%{public}d',
          MARK, permission.message, permission.auth_results.length);
        hilog.error(DOMAIN, TAG, '%{public}s VERIFY ABORT permission_not_granted', MARK);
        return;
      }

      const options = new RecorderOptions();
      // 崩溃尾段用例用 1.5 秒一段：3.5 秒时正好只剩最后 0.5 秒没封闭。
      options.segmentMs = mode === 'record_crash_tail' ? 1500 : 2000;
      options.endReason = mode === 'record_bg' ? 'background_stop' : 'verify_stop';
      const recorder = new VoiceRecorder(core, context, options);

      const cpuBefore = process.getPastCpuTime();
      const wallBefore = nowUptimeMs();
      await recorder.start();
      // 登记活动录音：Ability 被销毁/进程退出时 shutdown() 也能把屏幕常亮与长时任务收掉。
      RecordingVerify.heldCore = core;
      RecordingVerify.heldRecorder = recorder;
      hilog.info(DOMAIN, TAG, '%{public}s VERIFY STARTED recording=%{public}s',
        MARK, recorder.recordingId());

      if (mode === 'record_hold') {
        // 保持引用，不 stop；外部 `aa force-stop` 或 `process.abort()` 后由下次启动 recover。
        hilog.info(DOMAIN, TAG, '%{public}s HOLD pid=%{public}d recording=%{public}s',
          MARK, process.pid, recorder.recordingId());
        return;
      }

      if (mode === 'record_crash') {
        // 真·进程崩溃：不走 onDestroy / abort，模拟被系统回收或 SIGKILL。
        // 关闭的片段已经 register 过，当前半截片段不登记——恢复时「尾巴未知」。
        await sleep(durationFor(mode));
        hilog.warn(DOMAIN, TAG, '%{public}s CRASH pid=%{public}d recording=%{public}s segments=%{public}d',
          MARK, process.pid, recorder.recordingId(), recorder.segmentCount());
        process.abort();
        return;
      }

      if (mode === 'record_crash_tail') {
        // 录音**中途**崩溃、尾段没登记：3.5 秒（1.5 秒一段）时第 0/1 段登记成功
        // （共 3.0 秒），第 2 段还开着；进程在这里死掉，那 0.5 秒永远丢了。
        await sleep(durationFor(mode));
        // 崩溃前补一次进度上报：让核心里的「收到量」在崩溃瞬间是准确值。
        // 不补的话它落后一拍，尾巴会被「上报滞后」掩盖，看不出这个 bug。
        recorder.flushProgress('before_crash');
        hilog.warn(DOMAIN, TAG, '%{public}s CRASH_TAIL pid=%{public}d recording=%{public}s receivedMs=%{public}d persistedMs=%{public}d unregisteredTailMs=%{public}d segments=%{public}d',
          MARK, process.pid, recorder.recordingId(), recorder.receivedMs(),
          recorder.persistedMs(), recorder.unregisteredTailMs(), recorder.segmentCount());
        process.abort();
        return;
      }

      const duration = durationFor(mode);
      hilog.info(DOMAIN, TAG, '%{public}s VERIFY SLEEP ms=%{public}d', MARK, duration);
      await sleep(duration);

      let result: RecordingFinalizeResult | null = null;
      try {
        result = await recorder.stop(options.endReason);
      } catch (e) {
        hilog.error(DOMAIN, TAG, '%{public}s VERIFY STOP_FAILED %{public}s',
          MARK, describeError(e as Object));
      }

      const cpuAfter = process.getPastCpuTime();
      const wallAfter = nowUptimeMs();
      hilog.info(DOMAIN, TAG, '%{public}s MEASURE wallMs=%{public}d cpuMs=%{public}d cpuPercent=%{public}s captureLatencyMs=%{public}d',
        MARK, Math.round(wallAfter - wallBefore), cpuAfter - cpuBefore,
        percent(cpuAfter - cpuBefore, wallAfter - wallBefore),
        recorder.firstDataLatencyMs());
      hilog.info(DOMAIN, TAG, '%{public}s BG_TASK ok=%{public}s code=%{public}d message=%{public}s',
        MARK, recorder.taskResult().ok ? 'true' : 'false',
        recorder.taskResult().error_code, recorder.taskResult().error_message);

      if (result !== null) {
        logJson('finalize', result);
        logFiles(filesDir);
        const summary = core.runDueJobs({ budget_ms: 30000, trigger: 'verify' });
        logJson('jobs.runDue', summary);
        const status = core.jobsStatus();
        logJson('jobs.status', status);
        hilog.info(DOMAIN, TAG, '%{public}s TRANSCRIBE queued=%{public}s waitingConfiguration=%{public}d succeeded=%{public}d failed=%{public}d',
          MARK, result.transcription_queued ? 'true' : 'false',
          status.waiting_configuration, status.succeeded, status.failed);
      }

      const after = core.coreInfo();
      logJson('core.info.after', after);
      hilog.info(DOMAIN, TAG, '%{public}s VERIFY DONE segments=%{public}d segmentCount=%{public}d',
        MARK, recorder.segmentCount(), recorder.segmentCount());
    } catch (e) {
      hilog.error(DOMAIN, TAG, '%{public}s VERIFY ERROR %{public}s',
        MARK, describeError(e as Object));
    } finally {
      // hold 模式不能关：下一次启动（recover 模式）要看库里留下的会话。
      if (mode !== 'record_hold') {
        const active = RecordingVerify.heldRecorder;
        if (active !== null &&
          (active.currentPhase() === 'recording' || active.currentPhase() === 'error')) {
          try {
            await active.abort();
          } catch (err) {
            hilog.warn(DOMAIN, TAG, '%{public}s SHUTDOWN active abort failed %{public}s',
              MARK, describeError(err as Object));
          }
        }
        RecordingVerify.heldRecorder = null;
        RecordingVerify.heldCore = null;
        core.close();
      }
    }
  }

  /** 正常退出时清理 hold 的录音器（进程被 kill 时不会走到）。 */
  static async shutdown(): Promise<void> {
    const recorder = RecordingVerify.heldRecorder;
    const core = RecordingVerify.heldCore;
    RecordingVerify.heldRecorder = null;
    RecordingVerify.heldCore = null;
    if (recorder !== null) {
      try {
        await recorder.abort();
      } catch (e) {
        hilog.warn(DOMAIN, TAG, '%{public}s SHUTDOWN abort failed %{public}s',
          MARK, describeError(e as Object));
      }
    }
    if (core !== null) {
      core.close();
    }
  }
}

// ---------------------------------------------------------------- 日志 / 文件

/** 只读：把系统「息屏超时」现值打进日志（不修改设置）。 */
function logScreenTimeout(context: common.UIAbilityContext): void {
  try {
    const name = settings.display.SCREEN_OFF_TIMEOUT;
    const value = settings.getValueSync(context, name, 'unknown');
    hilog.info(DOMAIN, TAG, '%{public}s SCREEN_TIMEOUT name=%{public}s value=%{public}s',
      MARK, name, value);
  } catch (e) {
    hilog.warn(DOMAIN, TAG, '%{public}s SCREEN_TIMEOUT read failed %{public}s',
      MARK, describeError(e as Object));
  }
}

function logJson(label: string, value: Object): void {
  const text = JSON.stringify(value);
  const chunks = Math.max(1, Math.ceil(text.length / CHUNK));
  for (let i = 0; i < chunks; i++) {
    hilog.info(DOMAIN, TAG, '%{public}s JSON[%{public}s][%{public}d/%{public}d] %{public}s',
      MARK, label, i + 1, chunks, text.substring(i * CHUNK, (i + 1) * CHUNK));
  }
}

/** 列出 filesDir 下 blobs/ 与根目录的文件（大小），证明片段/资产真的落盘。 */
function logFiles(filesDir: string): void {
  const blobs = `${filesDir}/blobs`;
  logDir('blobs', blobs);
  logDir('files', filesDir);
  const db = `${filesDir}/library.sqlite`;
  try {
    const st = fs.statSync(db);
    hilog.info(DOMAIN, TAG, '%{public}s FILE db=%{public}s bytes=%{public}d',
      MARK, db, st.size);
  } catch (e) {
    hilog.warn(DOMAIN, TAG, '%{public}s FILE db missing %{public}s', MARK, describeError(e as Object));
  }
  const staging = `${filesDir}/staging`;
  try {
    const entries = fs.listFileSync(staging);
    hilog.info(DOMAIN, TAG, '%{public}s STAGING remaining=%{public}d names=%{public}s',
      MARK, entries.length, entries.join(','));
  } catch (e) {
    hilog.info(DOMAIN, TAG, '%{public}s STAGING gone (finalize 已清理)', MARK);
  }
}

function logDir(label: string, dir: string): void {
  try {
    const names = fs.listFileSync(dir, { recursion: true });
    hilog.info(DOMAIN, TAG, '%{public}s DIR[%{public}s] path=%{public}s files=%{public}d',
      MARK, label, dir, names.length);
    const limit = Math.min(names.length, 20);
    for (let i = 0; i < limit; i++) {
      const rel = names[i];
      const full = `${dir}${rel.startsWith('/') ? rel : `/${rel}`}`;
      try {
        hilog.info(DOMAIN, TAG, '%{public}s FILE[%{public}s] %{public}s bytes=%{public}d',
          MARK, label, rel, fs.statSync(full).size);
        if (label === 'blobs') {
          logWavHeader(full);
        }
      } catch (e) {
        hilog.warn(DOMAIN, TAG, '%{public}s FILE[%{public}s] %{public}s statFailed',
          MARK, label, rel);
      }
    }
  } catch (e) {
    hilog.warn(DOMAIN, TAG, '%{public}s DIR[%{public}s] missing %{public}s',
      MARK, label, describeError(e as Object));
  }
}

/**
 * 读 blob 开头的 44 字节并解析 WAV 头。
 *
 * 这是「核心最终化产出的是合法 PCM WAV」的直接证据；长度字段是平台层写的流式占位
 * （0xFFFFFFFF），真实长度由文件大小决定（见 RecordingFormat.ets）。
 */
function logWavHeader(path: string): void {
  let file: fs.File | null = null;
  try {
    file = fs.openSync(path, fs.OpenMode.READ_ONLY);
    const buffer = new ArrayBuffer(44);
    const read = fs.readSync(file.fd, buffer);
    if (read < 44) {
      hilog.warn(DOMAIN, TAG, '%{public}s WAV path=%{public}s read=%{public}d tooShort', MARK, path, read);
      return;
    }
    const view = new DataView(buffer);
    const riff = String.fromCharCode(view.getUint8(0), view.getUint8(1), view.getUint8(2), view.getUint8(3));
    const wave = String.fromCharCode(view.getUint8(8), view.getUint8(9), view.getUint8(10), view.getUint8(11));
    const format = view.getUint16(20, true);
    const channels = view.getUint16(22, true);
    const rate = view.getUint32(24, true);
    const bits = view.getUint16(34, true);
    const dataSize = view.getUint32(40, true);
    hilog.info(DOMAIN, TAG, '%{public}s WAV path=%{public}s riff=%{public}s wave=%{public}s format=%{public}d channels=%{public}d rate=%{public}d bits=%{public}d dataSizeField=%{public}d',
      MARK, path, riff, wave, format, channels, rate, bits, dataSize);
  } catch (e) {
    hilog.warn(DOMAIN, TAG, '%{public}s WAV path=%{public}s readFailed %{public}s',
      MARK, path, describeError(e as Object));
  } finally {
    if (file !== null) {
      fs.closeSync(file);
    }
  }
}

function percent(partMs: number, totalMs: number): string {
  if (totalMs <= 0) {
    return 'n/a';
  }
  return ((partMs * 100) / totalMs).toFixed(2);
}

function sleep(ms: number): Promise<void> {
  return new Promise<void>((resolve: () => void) => {
    setTimeout(() => {
      resolve();
    }, ms);
  });
}

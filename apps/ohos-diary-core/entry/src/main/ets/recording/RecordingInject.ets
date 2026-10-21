// 把一段**已知的** WAV 按平台层录音约定登记成一条录音。
//
// 为什么单独一个模块：`hap_both` 与 `asr_inject` 两个探针都要走同一条链路——
// `captures.createDraft` → `recordings.prepare` → 往 staging 目录写片段
// （第 0 段带 44 字节 WAV 头）→ `recordings.registerSegment`（核心重算哈希/字节数）
// → `recordings.finalize`（同事务排 `transcribe` 任务）。
//
// 复制两份的话，任何一边改了片段格式或登记顺序，另一边的证据就不再能互相对照；
// 而且「已知 PCM 转不出已知文本」时会分不清是注入错了还是引擎坏了。这里只做登记，
// **不打印日志**：调用方按自己的 tag（DIARY_HAP / DIARY_ASR）打印，返回的事实足够
// 把 wav/pcm/时长/片段/RMS 都写清楚。

import { DiaryCore } from '../core/DiaryCore';
import { RecordingFinalizeResult } from '../core/Models';
import { pcmPeak, pcmRms, readWavPcm } from './HapBothWav';
import { PcmSegmentWriter } from './PcmSegmentWriter';
import { pcmBytesToMs } from './RecordingFormat';

/** 一次注入的全部事实（调用方据此打日志；`finalize` 是核心的原始回执）。 */
export interface InjectedRecording {
  wavPath: string;
  /** 纯 PCM 字节数（不含 WAV 头）。 */
  pcmBytes: number;
  /** 按 16 kHz / 单声道 / 16 bit 算出的时长；应与 finalize 的 logical_duration_ms 一致。 */
  pcmMs: number;
  rms: number;
  peak: number;
  captureId: string;
  recordingId: string;
  assetId: string;
  segmentIndex: number;
  segmentFileName: string;
  /** 片段文件字节数（第 0 段含 44 字节头）。 */
  segmentByteSize: number;
  /** 片段时长（由纯 PCM 字节数算出，不含头部）。 */
  segmentDurationMs: number;
  segmentSha256: string;
  durable: boolean;
  finalize: RecordingFinalizeResult;
}

/**
 * 读 `wavPath` 的 PCM，登记成一个已收尾的录音，返回可直接打日志/断言的事实。
 *
 * `operationPrefix` 用于幂等键（`<prefix>-draft-<nonce>`）；`endReason` 原样落进
 * `recording_sessions.end_reason`，便于事后在库里区分是哪个探针注入的。
 */
export function injectWavAsRecording(
  core: DiaryCore,
  wavPath: string,
  operationPrefix: string,
  endReason: string,
): InjectedRecording {
  const pcm = readWavPcm(wavPath);

  const nonce = `${Date.now()}`;
  const capture = core.createDraft({
    time_zone: 'Asia/Shanghai',
    utc_offset_minutes: 480,
    operation_id: `${operationPrefix}-draft-${nonce}`,
  });
  const ticket = core.prepareRecording({
    capture_id: capture.id,
    operation_id: `${operationPrefix}-prepare-${nonce}`,
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

  const result = core.finalizeRecording({
    recording_id: ticket.recording_id,
    last_segment_index: file.index,
    end_reason: endReason,
  });

  return {
    wavPath: wavPath,
    pcmBytes: pcm.length,
    pcmMs: pcmBytesToMs(pcm.length),
    rms: pcmRms(pcm),
    peak: pcmPeak(pcm),
    captureId: capture.id,
    recordingId: ticket.recording_id,
    assetId: result.asset_id,
    segmentIndex: file.index,
    segmentFileName: file.fileName,
    segmentByteSize: file.byteSize,
    segmentDurationMs: file.durationMs,
    segmentSha256: file.sha256,
    durable: receipt.durable,
    finalize: result,
  };
}
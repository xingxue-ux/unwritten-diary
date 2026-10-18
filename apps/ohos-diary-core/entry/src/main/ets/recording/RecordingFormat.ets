// 录音的**平台层格式约定**（#108 ⑦ 第一片）。
//
// 核心的转写链只解码 PCM WAV（`crates/diary_core/src/transcription/wav.rs`），而
// `recordings.finalize` 是把片段文件**按序直接拼接**成一个 blob。两件事加起来决定了
// 平台层必须这样写片段：
//
//   第 0 段 = 44 字节 WAV 头（PCM / 16 kHz / 单声道 / 16 bit） + PCM 数据
//   第 N 段 = 裸 PCM 数据（N ≥ 1）
//
// 这样拼出来是**一条**头 + 全部 PCM 的合法 WAV；如果每段都各带 44 字节头，拼接后
// 第二个头会被当成采样读（`wav.rs` 顶部注释里记的已知坑），而且第一个头声明的
// data 长度只会覆盖第 0 段，转写链只能看到开头几秒。
//
// WAV 头的 RIFF size / data size 写 `0xFFFFFFFF`（流式 WAV 的「长度未知」约定）：
// 总长度要到停止时才确定，而片段在录的时候就要登记、不能再改内容（核心会重算哈希）。
// 核心解析时取 `min(声明长度, 文件剩余长度)`，所以 0xFFFFFFFF 正好等于「读到 blob 末尾」。
// 代价：单看第 0 段文件，头里的长度是占位的；最终化后的 blob 是完整音频，但头部长度
// 仍是占位值，导出/播放若严格校验长度可能显示 4 GiB。这一条写在文档的已知限制里。

/** 采样参数：与核心转写链的默认形态一致（16 kHz / 单声道 / 16 bit PCM）。 */
export const SAMPLE_RATE: number = 16000;
export const CHANNEL_COUNT: number = 1;
export const BITS_PER_SAMPLE: number = 16;
/** 每秒 PCM 字节数：16000 × 1 × 2 = 32,000。 */
export const PCM_BYTES_PER_SECOND: number = SAMPLE_RATE * CHANNEL_COUNT * (BITS_PER_SAMPLE / 8);
/** WAV 头长度（只有第 0 段带）。 */
export const WAV_HEADER_BYTES: number = 44;

/** 片段序号 → 稳定文件名。第 0 段是 `.wav`（带头），其余是 `.pcm`（裸数据）。 */
export function segmentFileName(index: number): string {
  const padded = index.toString().padStart(6, '0');
  return index === 0 ? `segment-${padded}.wav` : `segment-${padded}.pcm`;
}

/** 片段序号 → 稳定 segment_id（同一录音内唯一；核心只要求它进指纹）。 */
export function segmentId(index: number): string {
  return `seg-${index.toString().padStart(6, '0')}`;
}

/** PCM 字节数 → 毫秒（按本文件的采样参数）。 */
export function pcmBytesToMs(bytes: number): number {
  return Math.round((bytes * 1000) / PCM_BYTES_PER_SECOND);
}

/** 毫秒 → 该时长需要的 PCM 字节数。 */
export function msToPcmBytes(ms: number): number {
  return Math.round((ms * PCM_BYTES_PER_SECOND) / 1000);
}

/**
 * 44 字节 PCM WAV 头。长度字段用流式占位（见文件顶部说明）。
 *
 * 布局：RIFF / size / WAVE / fmt  (16, PCM, ch, rate, byteRate, blockAlign, bits) /
 * data / size。全部小端。
 */
export function buildWavHeader(): ArrayBuffer {
  const buffer = new ArrayBuffer(WAV_HEADER_BYTES);
  const view = new DataView(buffer);
  writeAscii(view, 0, 'RIFF');
  view.setUint32(4, 0xFFFFFFFF, true);
  writeAscii(view, 8, 'WAVE');
  writeAscii(view, 12, 'fmt ');
  view.setUint32(16, 16, true); // fmt 块长度
  view.setUint16(20, 1, true); // PCM
  view.setUint16(22, CHANNEL_COUNT, true);
  view.setUint32(24, SAMPLE_RATE, true);
  view.setUint32(28, PCM_BYTES_PER_SECOND, true); // byte rate
  view.setUint16(32, CHANNEL_COUNT * (BITS_PER_SAMPLE / 8), true); // block align
  view.setUint16(34, BITS_PER_SAMPLE, true);
  writeAscii(view, 36, 'data');
  view.setUint32(40, 0xFFFFFFFF, true);
  return buffer;
}

function writeAscii(view: DataView, offset: number, text: string): void {
  for (let i = 0; i < text.length; i++) {
    view.setUint8(offset + i, text.charCodeAt(i));
  }
}

// 单个录音片段的落盘：**顺序写、增量算哈希、停止即封闭**。
//
// 为什么不用 `@ohos.file.hash.hash(path)`：那是「文件写完之后再读一遍」的异步接口，
// 录音回调里没有等待点；而且核心登记时自己会重算哈希，平台层只需要给出**同一份内容**
// 的哈希。这里用 `cryptoFramework.Md('SHA256')` 在写的同时 `updateSync`，封闭时
// `digestSync()`，不额外读盘。
//
// 文件格式见 `RecordingFormat.ets`：第 0 段带 44 字节 WAV 头，其余段是裸 PCM。

import { fileIo as fs } from '@kit.CoreFileKit';
import { cryptoFramework } from '@kit.CryptoArchitectureKit';
import {
  WAV_HEADER_BYTES,
  buildWavHeader,
  pcmBytesToMs,
  segmentFileName,
  segmentId,
} from './RecordingFormat';

/** 一个已封闭片段的事实（交给核心登记，也用来打日志）。 */
export class SegmentFile {
  index: number = 0;
  segmentId: string = '';
  fileName: string = '';
  path: string = '';
  /** 文件总字节数（第 0 段含 44 字节头）。 */
  byteSize: number = 0;
  /** 纯 PCM 字节数（不含头）；时长按它算。 */
  pcmBytes: number = 0;
  durationMs: number = 0;
  /** 小写十六进制 SHA-256。 */
  sha256: string = '';
}

/** 一个正在写的片段文件。 */
export class PcmSegmentWriter {
  private readonly directory: string;
  private readonly index: number;
  private file: fs.File | null = null;
  private md: cryptoFramework.Md | null = null;
  private bytes: number = 0;
  private pcm: number = 0;
  private closed: boolean = false;

  constructor(directory: string, index: number) {
    this.directory = directory.endsWith('/') ? directory.substring(0, directory.length - 1) : directory;
    this.index = index;
  }

  /** 创建文件（TRUNC：同名文件视为上一次没封闭完的残留），第 0 段先写 WAV 头。 */
  open(): void {
    const name = segmentFileName(this.index);
    const path = `${this.directory}/${name}`;
    this.file = fs.openSync(path, fs.OpenMode.CREATE | fs.OpenMode.READ_WRITE | fs.OpenMode.TRUNC);
    this.md = cryptoFramework.createMd('SHA256');
    this.bytes = 0;
    this.pcm = 0;
    this.closed = false;
    if (this.index === 0) {
      this.writeRaw(buildWavHeader());
    }
  }

  /** 写一块 PCM（`readData` 给的 ArrayBuffer，直接写、不复制）。 */
  writePcm(buffer: ArrayBuffer): void {
    if (this.closed || this.file === null) {
      throw new Error(`片段 ${this.index} 已封闭，不能再写`);
    }
    this.writeRaw(buffer);
    this.pcm += buffer.byteLength;
  }

  /** 当前纯 PCM 字节数（分片轮转的判据）。 */
  pcmBytes(): number {
    return this.pcm;
  }

  /** 当前文件总字节数（排障用）。 */
  byteSize(): number {
    return this.bytes;
  }

  /** 封闭：算哈希、关文件，返回可登记的事实。 */
  finish(): SegmentFile {
    if (this.closed || this.file === null || this.md === null) {
      throw new Error(`片段 ${this.index} 已经封闭`);
    }
    const digest = this.md.digestSync();
    const sha256 = toHex(digest.data);
    const file = this.file;
    this.file = null;
    this.md = null;
    this.closed = true;
    fs.closeSync(file);

    const result = new SegmentFile();
    result.index = this.index;
    result.segmentId = segmentId(this.index);
    result.fileName = segmentFileName(this.index);
    result.path = `${this.directory}/${result.fileName}`;
    result.byteSize = this.bytes;
    result.pcmBytes = this.pcm;
    result.durationMs = pcmBytesToMs(this.pcm);
    result.sha256 = sha256;
    return result;
  }

  /** 放弃这个片段：关文件并删掉（没有 PCM 的尾巴不该进核心）。 */
  abort(): void {
    if (this.file !== null) {
      const file = this.file;
      this.file = null;
      try {
        fs.closeSync(file);
      } catch (e) {
        // 关闭失败也不能让清理本身抛出去。
      }
    }
    this.md = null;
    this.closed = true;
    try {
      fs.unlinkSync(`${this.directory}/${segmentFileName(this.index)}`);
    } catch (e) {
      // 文件可能本来就不存在。
    }
  }

  private writeRaw(buffer: ArrayBuffer): void {
    if (this.closed || this.file === null || this.md === null) {
      throw new Error(`片段 ${this.index} 没有打开`);
    }
    const written = fs.writeSync(this.file.fd, buffer);
    if (written !== buffer.byteLength) {
      throw new Error(`片段 ${this.index} 写入不完整：${written}/${buffer.byteLength}`);
    }
    this.md.updateSync({ data: new Uint8Array(buffer) });
    this.bytes += buffer.byteLength;
  }
}

/** 字节数组 → 小写十六进制（核心比较时忽略大小写，这里统一小写便于日志对照）。 */
function toHex(bytes: Uint8Array): string {
  let text = '';
  for (let i = 0; i < bytes.length; i++) {
    text += bytes[i].toString(16).padStart(2, '0');
  }
  return text;
}

/** 头长度常量导出给验证代码算「合理大小」用。 */
export const WAV_HEADER_SIZE: number = WAV_HEADER_BYTES;

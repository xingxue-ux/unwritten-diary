// `hap_both` 探针的 WAV 读取工具（**只管读**；下载统一走 `ModelDownload.ets`）。
//
// 原来这里还有一份「`.part` + 2xx + 只比字节数」的下载实现；它与
// `TranscriptionVerify` 的实现重复且更弱（没有 Content-Length/sha256，`-1` 时连
// 字节都不查）。现在下载只有一份（`ModelDownload.downloadToFile`），本文件只保留
// 读已知 PCM 需要的 RIFF 解析。

import { fileIo as fs } from '@kit.CoreFileKit';

/** 读 WAV 的 PCM 数据段（跳过 RIFF 头；找不到 `data` 就按 44 字节头处理）。 */
export function readWavPcm(path: string): Uint8Array {
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

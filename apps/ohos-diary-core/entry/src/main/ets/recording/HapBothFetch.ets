// `hap_both` 探针自己的取文件工具（配置 + bge 模型）。
//
// 为什么单独一份、不直接调 `TranscriptionVerify.downloadOne`：#130 把模型下载重写成了
// 「`.part` → 2xx/Content-Length/sha256 → 原子 rename + 重试」，但那个函数是
// **private**、并且绑定它自己的 `EXPECTED_MODEL_INT8` / `EXPECTED_TOKENS` manifest。
// 本探针要下的两类文件（小的 `core-config.json`、24 MB 的 bge 模型）不在那份 manifest 里；
// 为了不把 #130 的下载实现再复制一遍、也不改它的可见性，这里只留**最小**的一份：
//
//   写 `<dest>.part` → 要求 HTTP 2xx → 收完核对字节数（给了 expected 时）→
//   原子 rename 到 `<dest>`；失败删 `.part`、**旧文件一个字节都不动**。
//
// sha256 的最终核对不在这里做：设备侧文件对 shell 可读，宿主用 `hdc shell sha256sum`
// 直接与源文件比对（证据里就是这么做的），比在 ArkTS 里再算一遍 24 MB 更直接。

import { fileIo as fs } from '@kit.CoreFileKit';
import { http } from '@kit.NetworkKit';
import { hilog } from '@kit.PerformanceAnalysisKit';

const DOMAIN: number = 0x0000;
const TAG: string = 'DiaryHap';
const MARK: string = 'DIARY_HAP';

/**
 * 把 `url` 流式下到 `dest`，返回实收字节数。
 *
 * `expectedBytes >= 0` 时要求实收字节数完全相等；`-1` 表示「大小未知」（例如
 * `core-config.json` 由宿主编排，长度会随运行变化）。
 */
export async function downloadToFile(
  url: string,
  dest: string,
  expectedBytes: number,
): Promise<number> {
  const part = `${dest}.part`;
  removeIfExists(part);
  const request = http.createHttp();
  const file = fs.openSync(part, fs.OpenMode.CREATE | fs.OpenMode.READ_WRITE | fs.OpenMode.TRUNC);
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
  let problem = '';
  if (failure.length > 0) {
    problem = `请求失败：${failure}`;
  } else if (status < 200 || status >= 300) {
    problem = `HTTP ${status}`;
  } else if (expectedBytes >= 0 && bytes !== expectedBytes) {
    problem = `字节数不符：收到 ${bytes}，期望 ${expectedBytes}`;
  }
  if (problem.length > 0) {
    removeIfExists(part);
    throw new Error(`下载 ${url} 失败（${problem}）；原文件保持不变`);
  }
  try {
    fs.renameSync(part, dest);
  } catch (e) {
    removeIfExists(part);
    throw new Error(`下载 ${url} 后无法原子替换 ${dest}：${describeError(e as Object)}`);
  }
  hilog.info(DOMAIN, TAG, '%{public}s FETCH url=%{public}s dest=%{public}s bytes=%{public}d elapsedMs=%{public}d mbPerSec=%{public}s',
    MARK, url, dest, bytes, elapsed,
    elapsed > 0 ? (bytes / 1048576 / (elapsed / 1000)).toFixed(2) : '0');
  return bytes;
}

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

function removeIfExists(path: string): void {
  try {
    fs.unlinkSync(path);
  } catch (e) {
    // 不存在是正常情况。
  }
}

function describeError(error: Object): string {
  const err = error as Error;
  if (err !== undefined && err !== null && err.message !== undefined) {
    return err.message;
  }
  return JSON.stringify(error);
}

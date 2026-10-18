// **唯一**的模型/ORT 下载器（模型、词表、bge；探针配置不在这里，见下）。
//
// 为什么只有这一份：#130 之前的 `TranscriptionVerify` 已经有一份「`.part` → 2xx/
// Content-Length/sha256 → 原子 rename + 重试」的实现；#134 的 `hap_both` 探针又抄了
// 一份更弱的（只比字节数、`-1` 时连字节都不查、无 Content-Length/sha256）。
// 这里把两份合并成一份，所有下载都必须给 `{bytes, sha256}`，没有「大小未知就放行」
// 这条路径。
//
// 顺序（任何一步失败都只删 `.part`，旧 `dest` 一个字节都不动）：
//   写 `<dest>.part` → HTTP 2xx → Content-Length == expected.bytes →
//   实收字节数 == expected.bytes → sha256 == expected.sha256 →
//   同目录原子 rename 到 `dest`。
//
// `core-config.json` **不走网络**：探针直接按沙箱路径生成它（见 `HapBothVerify`）。
// 运行期配置是产品真正会读的东西，不能从 HTTP「下载即信任」。

import { fileIo as fs, hash } from '@kit.CoreFileKit';
import { http } from '@kit.NetworkKit';
import { hilog } from '@kit.PerformanceAnalysisKit';

const DOMAIN: number = 0x0000;
const TAG: string = 'DiaryModel';
const MARK: string = 'DIARY_MODEL';

/** 下载一个文件的完整性期望。两个字段都必须给：没有「大小未知」的弱路径。 */
export interface ExpectedFile {
  bytes: number;
  sha256: string;
}

/** 下载最多试几次（网络/HTTP/校验失败都重试；每次从 `.part` 重新收）。 */
const DOWNLOAD_ATTEMPTS: number = 3;

interface DownloadAttemptResult {
  ok: boolean;
  error: string;
}

/** 算一个文件的 sha256（小写十六进制）。用于「已存在就跳过」前先验内容。 */
export async function sha256OfFile(path: string): Promise<string> {
  return (await hash.hash(path, 'sha256')).toLowerCase();
}

/**
 * 把 `url` 流式下到 `dest`，返回实收字节数。语义见文件头。
 *
 * 失败时抛 `Error`（调用方/`TranscriptionVerify.run` 会把它打到 mode 级）。
 */
export async function downloadToFile(
  url: string,
  dest: string,
  expected: ExpectedFile,
): Promise<number> {
  const started = Date.now();
  let lastError = '';
  for (let attempt = 1; attempt <= DOWNLOAD_ATTEMPTS; attempt++) {
    const result = await downloadAttempt(url, dest, expected, attempt);
    if (result.ok) {
      hilog.info(DOMAIN, TAG, '%{public}s DOWNLOAD_OK url=%{public}s dest=%{public}s attempt=%{public}d elapsedMs=%{public}d',
        MARK, url, dest, attempt, Date.now() - started);
      return expected.bytes;
    }
    lastError = result.error;
    hilog.error(DOMAIN, TAG,
      '%{public}s DOWNLOAD_ATTEMPT_FAILED url=%{public}s attempt=%{public}d/%{public}d error=%{public}s',
      MARK, url, attempt, DOWNLOAD_ATTEMPTS, lastError);
    if (attempt < DOWNLOAD_ATTEMPTS) {
      await sleep(1000 * attempt);
    }
  }
  throw new Error(`下载 ${url} 失败（${DOWNLOAD_ATTEMPTS} 次；原文件保持不变）：${lastError}`);
}

async function downloadAttempt(
  url: string,
  dest: string,
  expected: ExpectedFile,
  attempt: number,
): Promise<DownloadAttemptResult> {
  const part = `${dest}.part`;
  removeIfExists(part);
  const request = http.createHttp();
  const file = fs.openSync(part, fs.OpenMode.CREATE | fs.OpenMode.READ_WRITE | fs.OpenMode.TRUNC);
  let bytes = 0;
  let contentLength = -1;
  const onData = (chunk: ArrayBuffer): void => {
    fs.writeSync(file.fd, chunk);
    bytes += chunk.byteLength;
  };
  // 流式响应没有 `HttpResponse.headers`：头从 `headersReceive` 事件拿。不假设键名
  // 大小写（不同版本/网关会变），用 JSON 文本做一次大小写不敏感匹配。
  const onHeaders = (headers: Object): void => {
    try {
      const text = JSON.stringify(headers);
      const found = text.match(/"content-length"\s*:\s*"?([0-9]+)"?/i);
      if (found !== null) {
        contentLength = Number.parseInt(found[1], 10);
      }
    } catch (e) {
      // 没有头就当没有 Content-Length；下面按「缺长度」失败，不猜。
    }
  };
  request.on('dataReceive', onData);
  request.on('headersReceive', onHeaders);
  let status = -1;
  let failure = '';
  const started = Date.now();
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
    request.off('headersReceive', onHeaders);
    request.destroy();
    fs.closeSync(file);
  }
  const elapsed = Date.now() - started;
  const mbPerSec = elapsed > 0 ? (bytes / 1048576 / (elapsed / 1000)).toFixed(2) : '0';
  hilog.info(DOMAIN, TAG,
    '%{public}s DOWNLOAD url=%{public}s attempt=%{public}d status=%{public}d bytes=%{public}d contentLength=%{public}d expectedBytes=%{public}d elapsedMs=%{public}d mbPerSec=%{public}s error=%{public}s',
    MARK, url, attempt, status, bytes, contentLength, expected.bytes, elapsed, mbPerSec, failure);

  const fail = (why: string): DownloadAttemptResult => {
    removeIfExists(part);
    hilog.error(DOMAIN, TAG,
      '%{public}s DOWNLOAD_VERIFY_FAILED url=%{public}s bytes=%{public}d contentLength=%{public}d expectedBytes=%{public}d sha256Expected=%{public}s reason=%{public}s',
      MARK, url, bytes, contentLength, expected.bytes, expected.sha256, why);
    return { ok: false, error: why };
  };

  if (failure.length > 0) {
    return fail(`请求失败：${failure}`);
  }
  if (status < 200 || status >= 300) {
    return fail(`HTTP ${status}（只接受 2xx）`);
  }
  if (contentLength < 0) {
    return fail('响应没有 Content-Length，无法按清单校验');
  }
  if (contentLength !== expected.bytes) {
    return fail(`Content-Length=${contentLength} 与清单的 ${expected.bytes} 不一致`);
  }
  if (bytes !== expected.bytes) {
    return fail(`实收 ${bytes} 字节与清单的 ${expected.bytes} 不一致`);
  }
  let actual = '';
  try {
    actual = await sha256OfFile(part);
  } catch (e) {
    return fail(`计算 sha256 失败：${describeError(e as Object)}`);
  }
  if (actual !== expected.sha256) {
    return fail(`sha256=${actual} 与清单的 ${expected.sha256} 不一致`);
  }
  try {
    fs.renameSync(part, dest);
  } catch (e) {
    return fail(`校验通过但原子 rename 失败：${describeError(e as Object)}`);
  }
  hilog.info(DOMAIN, TAG, '%{public}s DOWNLOAD_VERIFIED url=%{public}s bytes=%{public}d sha256=%{public}s',
    MARK, url, bytes, actual);
  return { ok: true, error: '' };
}

function removeIfExists(path: string): void {
  try {
    if (fs.accessSync(path)) {
      fs.unlinkSync(path);
    }
  } catch (e) {
    // 删不掉也继续：下一次 open(CREATE|TRUNC) 会覆盖它，旧目标文件仍不受影响。
  }
}

function sleep(ms: number): Promise<void> {
  return new Promise<void>((resolve) => {
    setTimeout(() => resolve(), ms);
  });
}

function describeError(error: Object): string {
  const err = error as Error;
  if (err !== undefined && err !== null && err.message !== undefined) {
    return err.message;
  }
  return JSON.stringify(error);
}

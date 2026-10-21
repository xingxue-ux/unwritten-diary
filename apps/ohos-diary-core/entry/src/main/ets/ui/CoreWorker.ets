// 核心调用的 TaskPool worker（**所有页面共用的唯一通道**）。
//
// 为什么需要：
// - `DiaryCore` 的 30 个方法都是**同步阻塞**的（SQLite / 模型加载 / 推理），
//   在主线程调会卡帧、甚至 AppFreeze；
// - `DiaryCore` 句柄不能跨线程传（私有字段、TaskPool 只传可序列化值），
//   所以 worker 里按同一个资料库路径重新 `open`，拿到的是同一个核心
//   （真机并发验证见 `verify/ConcurrentWorker.ets` 与 README 第 8 项）。
//
// 约定：
// - 入参是 JSON 字符串（方法名 + 参数 JSON），出参也是 JSON 字符串；
// - 每条请求单独 try/catch：一条失败不会让整页失败；
// - 错误在这里被扁平化成 `{code,message,retryable}`，主线程不接触 `DiaryError`。

import { DiaryCore } from '../core/DiaryCore';
import { DiaryError } from '../core/DiaryError';
import { CoreErrorInfo } from './Errors';

/** 一条请求：`params` 是参数对象的 JSON 字符串；`''` 表示没有参数（null）。 */
interface WireRequest {
  method: string;
  params: string;
}

/** 一条响应：`data` 是 data 的 JSON 字符串；无返回值 / 失败时为 null。 */
interface WireResponse {
  ok: boolean;
  data: string | null;
  error: CoreErrorInfo | null;
}

/**
 * 在一个 worker 里打开核心、顺序执行一批请求、关闭。
 *
 * 用 `rawCall` 而不是 `DiaryCore` 的具名方法：worker 只负责「调用 + 序列化」，
 * 参数形状由主线程的页面负责；这样加页面不需要再加一个 @Concurrent 函数。
 */
@Concurrent
export function runCoreBatch(dbPath: string, requestsJson: string): string {
  const toInfo = (raw: Object): CoreErrorInfo => {
    let err: DiaryError;
    if (raw instanceof DiaryError) {
      err = raw;
    } else {
      err = DiaryError.fromThrown(raw);
    }
    return {
      code: err.code,
      message: err.message,
      retryable: err.retryable,
    };
  };
  let requests: WireRequest[] = [];
  try {
    requests = JSON.parse(requestsJson) as WireRequest[];
  } catch (e) {
    const bad: CoreErrorInfo = {
      code: 'native_unavailable',
      message: '界面生成的请求不是合法 JSON',
      retryable: false,
    };
    return JSON.stringify([{
      ok: false,
      data: null,
      error: bad,
    } as WireResponse]);
  }

  const responses: WireResponse[] = [];
  let opened: DiaryCore | null = null;
  try {
    opened = DiaryCore.open(dbPath);
  } catch (e) {
    const info: CoreErrorInfo = toInfo(e as Object);
    for (let i = 0; i < requests.length; i++) {
      responses.push({ ok: false, data: null, error: info });
    }
    return JSON.stringify(responses);
  }

  const core: DiaryCore = opened as DiaryCore;
  try {
    for (let i = 0; i < requests.length; i++) {
      const request: WireRequest = requests[i];
      try {
        const params: Object | null = request.params.length === 0
          ? null
          : JSON.parse(request.params) as Object;
        const data: Object | null = core.rawCall(request.method, params);
        responses.push({
          ok: true,
          data: data === null ? null : JSON.stringify(data),
          error: null,
        });
      } catch (e) {
        responses.push({
          ok: false,
          data: null,
          error: toInfo(e as Object),
        });
      }
    }
  } finally {
    try {
      core.close();
    } catch (e) {
      // 关闭失败不该覆盖已经拿到的结果；句柄泄漏由诊断页的 openHandleCount 兜底。
    }
  }
  return JSON.stringify(responses);
}

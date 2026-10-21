// 页面访问核心的唯一入口。
//
// - 所有调用都走 `taskpool.execute(runCoreBatch, ...)`：不在 UI 线程碰 SQLite / 模型；
// - `call` / `callMany` **不抛异常**，返回 `CoreResult`；页面只用 `ok` + `error`；
// - 一次 `callMany` 只 open/close 一次核心，适合「列表 + 每条的抽取状态」这种
//   一次页面加载要发很多条请求的场景。
//
// 这不是异步化的全部答案：核心仍然是同步的，只是被搬到了 worker。真正的
// 事件/回调通道还没有（见 PR 的「没做的/缺口」）。

import { taskpool } from '@kit.ArkTS';
import { CoreErrorInfo, EMPTY_UI_ERROR, gatewayError, toUiError, UiError } from './Errors';
import { runCoreBatch } from './CoreWorker';

/** 一批请求里的单条结果。`data` 已经解析成对象。 */
export interface CoreResult {
  ok: boolean;
  data: Object | null;
  error: CoreErrorInfo | null;
}

/** 页面构造的一条请求。`params` 必须是带类型的对象或 null。 */
export interface CoreRequest {
  method: string;
  params: Object | null;
}

interface WireRequest {
  method: string;
  params: string;
}

interface WireResponse {
  ok: boolean;
  data: string | null;
  error: CoreErrorInfo | null;
}

export class CoreGateway {
  /** 单条调用。 */
  static async call(
    dbPath: string,
    method: string,
    params: Object | null,
  ): Promise<CoreResult> {
    const results: CoreResult[] = await CoreGateway.callMany(dbPath, [{
      method: method,
      params: params,
    }]);
    return results.length > 0 ? results[0] : CoreGateway.failed(gatewayError('核心没有返回结果'));
  }

  /** 一批调用；返回顺序与请求一致。 */
  static async callMany(dbPath: string, requests: CoreRequest[]): Promise<CoreResult[]> {
    if (requests.length === 0) {
      return [];
    }
    const wire: WireRequest[] = [];
    for (let i = 0; i < requests.length; i++) {
      const request: CoreRequest = requests[i];
      wire.push({
        method: request.method,
        params: request.params === null ? '' : JSON.stringify(request.params),
      });
    }

    let parsed: WireResponse[] = [];
    try {
      const raw: Object = await taskpool.execute(runCoreBatch, dbPath, JSON.stringify(wire));
      parsed = JSON.parse(raw as string) as WireResponse[];
    } catch (e) {
      const message: string = (e as Error).message;
      const fallback: UiError = gatewayError(`核心调用没有完成：${message}`);
      const failed: CoreResult[] = [];
      for (let i = 0; i < requests.length; i++) {
        failed.push(CoreGateway.failed(fallback));
      }
      return failed;
    }

    const results: CoreResult[] = [];
    for (let i = 0; i < parsed.length; i++) {
      const item: WireResponse = parsed[i];
      let data: Object | null = null;
      if (item.data !== null && item.data !== undefined) {
        try {
          data = JSON.parse(item.data) as Object;
        } catch (e) {
          data = null;
        }
      }
      results.push({
        ok: item.ok,
        data: data,
        error: item.error,
      });
    }
    // 防御：worker 少返回时补失败，页面拿到的长度永远与请求一致。
    while (results.length < requests.length) {
      results.push(CoreGateway.failed(gatewayError('核心返回的结果数量不足')));
    }
    return results;
  }

  private static failed(error: UiError): CoreResult {
    return {
      ok: false,
      data: null,
      error: {
        code: error.code,
        message: error.message,
        retryable: error.retryable,
      },
    };
  }
}

/** `CoreResult` → `UiError`；成功时返回空哨兵。 */
export function uiErrorOf(result: CoreResult): UiError {
  if (result.ok) {
    return EMPTY_UI_ERROR;
  }
  if (result.error === null) {
    return gatewayError('核心返回 ok:false，但没有错误信息');
  }
  return toUiError(result.error);
}

// 核心调用的 TaskPool worker（**所有页面共用的唯一通道**）。
//
// 为什么需要：
// - `DiaryCore` 的 30 个方法都是**同步阻塞**的（SQLite / 模型加载 / 推理），
//   在主线程调会卡帧、甚至 AppFreeze；
// - `DiaryCore` 句柄不能跨线程传（私有字段、TaskPool 只传可序列化值），
//   所以 worker 里按同一个资料库路径重新 `open`，拿到的是同一个核心
//   （真机并发验证见 `verify/ConcurrentWorker.ets` 与 README 第 8 项）。
//
// **会话跨调用存活**（2026-10-22 修）：
// 以前每次 `runCoreBatch` 都是 open → 调用 → close，核心的**内存态**（最典型的是
// `captures.previewPurge` 的 `preview_token`，见 `crates/diary_core/src/recycle.rs`）
// 在两次 `CoreGateway.call` 之间就没了：回收站页预览成功、点「确认永久删除」必报
// `invalid_state`。现在句柄按 `dbPath` 缓存在 `CoreSessions` 模块里，只有
// `closeAll()`（App 退出 / 测试）或同一路径的缓存句柄失效被替换时才 close。
//
// 约定：
// - 入参是 JSON 字符串（方法名 + 参数 JSON），出参也是 JSON 字符串；
// - 每条请求单独 try/catch：一条失败不会让整页失败；
// - 错误在这里被扁平化成 `{code,message,retryable}`，主线程不接触 `DiaryError`。
//
// ⚠️ ArkTS 的 `@Concurrent` 只能用 **import 进来的**或本地的变量/函数；同文件的
// 顶层 helper / 全局变量会被 es2abc 拒绝。所以会话缓存放在 `CoreSessions.ets`，
// 这里只 import。

import { hilog } from '@kit.PerformanceAnalysisKit';
import { process } from '@kit.ArkTS';
import { DiaryCore } from '../core/DiaryCore';
import { DiaryError } from '../core/DiaryError';
import { acquireCore, closeAll } from './CoreSessions';
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
 * `closeAll()` 的 TaskPool 入口：由网关在主线程发起，实际在某个 worker 线程上
 * 清掉**那一份**模块缓存（线程模型见 `CoreSessions.ets`）。返回执行后进程内剩余
 * 句柄数。
 *
 * 为什么不让主线程直接 import `closeAll`：主线程与 worker 各有自己的模块实例，
 * 主线程那份 Map 永远是空的；必须把收尾动作也发给 worker 才有意义。
 */
@Concurrent
export function closeCoreSessions(): number {
  closeAll();
  return DiaryCore.openHandleCount();
}

/**
 * 在一个 worker 里取（或复用）核心、顺序执行一批请求。
 *
 * 用 `rawCall` 而不是 `DiaryCore` 的具名方法：worker 只负责「调用 + 序列化」，
 * 参数形状由主线程的页面负责；这样加页面不需要再加一个 @Concurrent 函数。
 *
 * **不在批次结束时 close**：句柄留在 `CoreSessions` 的缓存里给下一次调用复用。
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
    opened = acquireCore(dbPath);
  } catch (e) {
    const info: CoreErrorInfo = toInfo(e as Object);
    for (let i = 0; i < requests.length; i++) {
      responses.push({ ok: false, data: null, error: info });
    }
    return JSON.stringify(responses);
  }

  // acquireCore 只在返回前抛错；走到这里一定拿到了句柄。
  const core: DiaryCore = opened as DiaryCore;
  let okCount: number = 0;
  let firstError: string = '';
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
      okCount++;
    } catch (e) {
      const info: CoreErrorInfo = toInfo(e as Object);
      if (firstError.length === 0) {
        firstError = `${request.method}:${info.code}`;
      }
      responses.push({
        ok: false,
        data: null,
        error: info,
      });
    }
  }
  // 排障用：一行看清「哪条线程、调了什么、成功几条、进程里还有几个句柄」。
  // 跨调用会话出问题时，这条日志能直接区分「复用了缓存」与「重新 open 了新会话」。
  hilog.debug(0x0000, 'DiaryCore',
    'DIARY_CORE_SESSION BATCH tid=%{public}d n=%{public}d first=%{public}s ok=%{public}d err=%{public}s handles=%{public}d',
    process.tid, requests.length, requests[0].method, okCount, firstError,
    DiaryCore.openHandleCount());
  return JSON.stringify(responses);
}

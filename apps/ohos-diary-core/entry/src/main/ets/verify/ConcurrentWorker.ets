// 真机并发验证用的 TaskPool worker（#122 未验清单第 8 项）。
//
// 两个 worker 分别对**同一个句柄**高频调用 `core.info` 与 `search.start`，
// 同时主线程也参与调用；用来观察 shim 的句柄表 + C ABI 的 Arc<Mutex<Core>>
// 在多线程下会不会 panic / 死锁 / 返回错乱。
//
// 注意：这里刻意直接走 `libdiary_napi.so` 的 `call(handle, json)`，不经过
// `DiaryCore` 对象——`DiaryCore` 的 handle 字段是私有的，而 TaskPool 只能传
// 可序列化的 number。句柄 id 本来就是进程内全局的，所以 worker 拿 id 调用
// 与主线程拿 id 调用是同一张句柄表、同一个 Core。

import { process } from '@kit.ArkTS';
import diaryNapi from 'libdiary_napi.so';
import { DiaryCore } from '../core/DiaryCore';

/**
 * 同一个句柄上并发调用。`handle` 由主线程 `diaryNapi.open()` 创建。
 * 返回一条 JSON 摘要（TaskPool 只回传可序列化的值）。
 */
@Concurrent
export function callSameHandle(handle: number, method: string, iterations: number): string {
  const started = Date.now();
  let ok = 0;
  let errors = 0;
  let last = '';
  const request = method === 'search'
    ? '{"method":"search.start","params":{"request":{"query":"benchneedle","page_size":10},"query_revision":1}}'
    : '{"method":"core.info","params":null}';
  for (let i = 0; i < iterations; i++) {
    try {
      const envelope = diaryNapi.call(handle, request);
      if (envelope.indexOf('"ok":true') >= 0) {
        ok++;
      } else {
        errors++;
        if (last.length < 240) {
          last = envelope.substring(0, 240);
        }
      }
    } catch (e) {
      errors++;
      const err = e as Error;
      if (last.length < 240) {
        last = err.message;
      }
    }
  }
  return JSON.stringify({
    mode: 'same-handle',
    pid: process.pid,
    tid: process.tid,
    method: method,
    iterations: iterations,
    ok: ok,
    errors: errors,
    last: last,
    startMs: started,
    endMs: Date.now()
  });
}

/**
 * 每个 worker 自己 open 同一个路径（C ABI 会复用同一个 Arc<Mutex<Core>>），
 * 跑完自己 close。用来观察句柄数与「同路径共享核心」。
 */
@Concurrent
export function openAndCall(dbPath: string, method: string, iterations: number): string {
  const started = Date.now();
  let ok = 0;
  let errors = 0;
  let last = '';
  let opened = false;
  let core: DiaryCore | null = null;
  try {
    core = DiaryCore.open(dbPath);
    opened = true;
    for (let i = 0; i < iterations; i++) {
      try {
        if (method === 'search') {
          core.search({ request: { query: 'benchneedle', page_size: 10 }, query_revision: 1 });
        } else {
          core.coreInfo();
        }
        ok++;
      } catch (e) {
        errors++;
        const err = e as Error;
        if (last.length < 240) {
          last = err.message;
        }
      }
    }
  } catch (e) {
    errors++;
    const err = e as Error;
    last = err.message;
  } finally {
    if (core !== null) {
      try {
        core.close();
      } catch (e) {
        const err = e as Error;
        if (last.length < 240) {
          last = `close: ${err.message}`;
        }
      }
    }
  }
  return JSON.stringify({
    mode: 'own-handle',
    pid: process.pid,
    tid: process.tid,
    method: method,
    iterations: iterations,
    opened: opened,
    ok: ok,
    errors: errors,
    last: last,
    startMs: started,
    endMs: Date.now()
  });
}

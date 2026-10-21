// `CoreWorker` 的会话缓存：按资料库路径缓存打开的核心句柄。
//
// 为什么单独一个模块：ArkTS 的 `@Concurrent` 函数只能用 **import 进来的**或本地的
// 变量/函数，同文件里未导出的顶层 helper / 全局变量会被 es2abc 拒绝
// （`Concurrent function should only use import variable or local variable`）。
// 会话缓存必须跨 `taskpool.execute` 调用存活，所以把它放在独立模块里、由 worker
// `import`；这也和 `recording/DiaryGenSupport.ets` / `HapBothOps.ets` 的做法一致。
//
// 为什么需要缓存（2026-10-22 修）：
// 以前每次 `runCoreBatch` 都是 open → 调用 → close。核心的若干状态只存在 `Core`
// 会话的内存里、**不落库**（最典型的是 `captures.previewPurge` 的 `preview_token`，
// 见 `crates/diary_core/src/recycle.rs`）。最后一次 close 会让核心析构，下一次调用
// 重新 open 出来的是新会话，令牌随之消失 ⇒ 回收站页「预览成功、点确认永久删除必报
// invalid_state」。现在同一路径的下一次调用复用同一个 `DiaryCore`，核心不会被关掉，
// 内存态（预览令牌、检索会话…）在两次 `CoreGateway.call` 之间保持。
//
// 线程模型（**必读，这里没有真正的跨线程互斥**）：
// - ArkTS 在每个 worker 线程里各有一份模块实例，所以这个 Map 实际是
//   **每个 taskpool 线程一份**，不是全进程一张表。同一线程内的任务复用同一个
//   `DiaryCore`；不同线程各持自己的句柄（同一个路径最多每线程一个）。
// - 句柄不同**不等于**核心不同：`diary_capi` 的 `open_libraries()` 用
//   `Weak<Mutex<Core>>` 按 canonical 路径复用同一个 `Core`，并且**跨** `Core::open`
//   持有登记表锁。因此两条线程同时首次打开同一路径时，capi 只会建一个核心；
//   最坏情况是多出一个句柄令牌（多一条登记表记录），不会出现两个核心。
// - 「先查缓存、没有再 open」这个窗口在跨线程时确实存在：两个线程可能都 miss、
//   都 open。后果仅限于多一个**持久的**句柄（我们无法从另一条线程的 Map 里找到它
//   来 close），不会丢数据、不会让令牌绑到另一个核心。要彻底消掉这个窗口，需要把
//   核心调用固定到单一线程（专用 Worker）或把会话登记挪到原生层；本 PR 没做，属于
//   已知残余。
// - `closeAll()` 只能关掉**当前线程**这一份 Map 里的句柄；别的 worker 线程缓存的
//   句柄碰不到。App 退出时进程整体回收，这只是尽力而为；测试要断言句柄归零，应在
//   持有缓存的那个 worker 任务里调 `closeAll()`。

import { hilog } from '@kit.PerformanceAnalysisKit';
import { DiaryCore } from '../core/DiaryCore';

const DOMAIN: number = 0x0000;
const TAG: string = 'DiaryCore';
const MARK: string = 'DIARY_CORE_SESSION';

/** 按资料库路径缓存的核心会话（每个 worker 线程一份，原因见文件头）。 */
const SESSIONS: Map<string, DiaryCore> = new Map<string, DiaryCore>();

/**
 * 取这个路径的会话：有打开着的缓存就复用，否则新开一个并记下来。
 *
 * `dbPath` 用调用方原样传进来的字符串做 key（网关传的是同一个
 * `<filesDir>/library.sqlite`）。不同写法的同一路径（相对/绝对、软链）在这里是
 * 两个 key，会各留一个句柄；底层仍是同一个 `Core`（capi 会 canonicalize）。
 */
export function acquireCore(dbPath: string): DiaryCore {
  const cached: DiaryCore | undefined = SESSIONS.get(dbPath);
  if (cached !== undefined) {
    if (cached.isOpen()) {
      return cached;
    }
    // 缓存里的对象已经 close 过（只可能是同线程测试/诊断代码显式关的）：
    // 丢掉外壳，重新 open；capi 若还有别的句柄就复用同一个核心。
    SESSIONS.delete(dbPath);
  }
  const opened: DiaryCore = DiaryCore.open(dbPath);
  SESSIONS.set(dbPath, opened);
  let handles: number = -1;
  try {
    handles = DiaryCore.openHandleCount();
  } catch (e) {
    // 诊断数字拿不到不影响会话本身。
    handles = -1;
  }
  hilog.debug(DOMAIN, TAG, '%{public}s OPEN db=%{public}s handles=%{public}d',
    MARK, dbPath, handles);
  return opened;
}

/**
 * 关掉**当前线程**缓存里所有核心句柄，并清空缓存。
 *
 * 用途：App 退出、测试/探针收尾。重复调用安全；关不掉的句柄只记日志，不抛
 * （进程退出时系统会回收）。
 *
 * ⚠️ 它**不是**全进程的 close-all：别的 worker 线程缓存里的句柄从这条线程根本
 * 看不到，所以调用之后 `openHandleCount()` 通常仍 > 0（实测一个 4 线程的
 * taskpool 会常驻最多 4 个句柄）。测试若要断言「归零」，只能在持有缓存的那个
 * worker 任务里调；App 退出时剩下的句柄由进程回收。
 */
export function closeAll(): void {
  SESSIONS.forEach((core: DiaryCore, dbPath: string) => {
    try {
      core.close();
    } catch (e) {
      // 关不掉不该让收尾流程失败；泄漏由诊断页的 openHandleCount 体现。
      hilog.warn(DOMAIN, TAG, '%{public}s CLOSE_FAILED db=%{public}s',
        MARK, dbPath);
    }
  });
  SESSIONS.clear();
  let handles: number = -1;
  try {
    handles = DiaryCore.openHandleCount();
  } catch (e) {
    // 诊断数字拿不到不影响收尾。
    handles = -1;
  }
  hilog.info(DOMAIN, TAG, '%{public}s CLOSE_ALL handles=%{public}d',
    MARK, handles);
}
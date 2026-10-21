// 应用级 DiaryCore 句柄（只给 **VoiceRecorder** 这种必须在主线程调核心的组件用）。
//
// 页面/列表/搜索/设置都走 `CoreGateway`（TaskPool），不持有这个句柄。
// 录音链路本身就是按「主线程同步调核心」设计的（2 秒一次 registerSegment、
// 一秒一次 updateState，都是毫秒级），所以需要**一个打开一次**的句柄；
// 不要每次录音都 open/close（关闭后再开有额外成本，也容易和 worker 抢初始化）。

import { DiaryCore } from '../core/DiaryCore';
import { EMPTY_UI_ERROR, UiError, uiErrorFromThrown } from './Errors';

/** 打开结果：失败时 `core === null`，错误已经是界面可渲染的 `UiError`。 */
export interface CoreOpenResult {
  core: DiaryCore | null;
  error: UiError;
}

export class CoreSession {
  private static instance: DiaryCore | null = null;
  private static directory: string = '';

  /** 幂等打开：同一目录复用；目录变了先关旧的。 */
  static open(filesDir: string): CoreOpenResult {
    const current: DiaryCore | null = CoreSession.instance;
    if (current !== null && current.isOpen() && CoreSession.directory === filesDir) {
      return { core: current, error: EMPTY_UI_ERROR };
    }
    try {
      if (current !== null && current.isOpen()) {
        current.close();
      }
    } catch (e) {
      // 旧句柄关闭失败不阻止新打开；真出问题会在诊断页的 openHandleCount 上体现。
    }
    CoreSession.instance = null;
    try {
      const opened: DiaryCore = DiaryCore.openInDirectory(filesDir);
      CoreSession.instance = opened;
      CoreSession.directory = filesDir;
      return { core: opened, error: EMPTY_UI_ERROR };
    } catch (e) {
      return { core: null, error: uiErrorFromThrown(e as Object) };
    }
  }

  static current(): DiaryCore | null {
    return CoreSession.instance;
  }

  /** 退出应用/销毁 Ability 时调用；重复调用安全。 */
  static close(): void {
    const current: DiaryCore | null = CoreSession.instance;
    CoreSession.instance = null;
    CoreSession.directory = '';
    if (current === null) {
      return;
    }
    try {
      current.close();
    } catch (e) {
      // 关闭失败无法补救；进程退出时系统会回收。
    }
  }
}

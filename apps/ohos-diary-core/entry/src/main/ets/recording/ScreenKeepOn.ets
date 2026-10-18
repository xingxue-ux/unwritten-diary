// 录音期间保持屏幕常亮（产品级兜底）。
//
// 背景（本片实测）：这台 2in1 在 Power 熄屏时会**系统强制 STOP** AudioCapturer
// （InterruptHint=STOP），亮屏也不恢复。应用层绕不过这条平台策略，所以录音时把屏幕
// 按住不让它自己息屏——「放着不动就息屏」这条路就不会杀死录音。
//
// 硬口径：
// - 录音开始时 `setWindowKeepScreenOn(true)`；停止/异常/Ability 销毁时**必须**
//   `setWindowKeepScreenOn(false)`，不能让用户设备一直亮着；
// - 拿不到窗口/系统拒绝时**不阻断录音**，只把错误码写进日志与 `KeepScreenOnResult`；
// - 不做电源状态操作（不按 Power、不 suspend、不改息屏超时）——那是用户设备上的副作用。
//
// 注意系统文档的提醒：`setWindowKeepScreenOn(true)` 只应在必要场景（导航/视频/绘图/
// 游戏等）使用，非标准使用可能被系统判定并自动熄屏。录音属于「用户明确发起、屏幕上有
// 录音界面」的场景；真机实测见 docs/architecture/m6-平板与PC平台能力.md。

import { window } from '@kit.ArkUI';
import { common } from '@kit.AbilityKit';
import { BusinessError } from '@kit.BasicServicesKit';
import { hilog } from '@kit.PerformanceAnalysisKit';

const DOMAIN: number = 0x0000;
const TAG: string = 'DiaryRec';
const MARK: string = 'DIARY_REC';

/** 设置结果：失败也把系统错误码带出去。 */
export class KeepScreenOnResult {
  ok: boolean = false;
  error_code: number = 0;
  error_message: string = '';
}

/** 一次录音对应一个实例；`enable` 成功后 `disable` 才会真正调系统。 */
export class ScreenKeepOn {
  private win: window.Window | null = null;
  private enabled: boolean = false;

  isEnabled(): boolean {
    return this.enabled;
  }

  /** 拿主窗口并打开「保持屏幕常亮」。失败只返回结果，不抛。 */
  async enable(context: common.UIAbilityContext): Promise<KeepScreenOnResult> {
    const result = new KeepScreenOnResult();
    if (this.enabled) {
      result.ok = true;
      return result;
    }
    try {
      this.win = await window.getLastWindow(context);
      await this.win.setWindowKeepScreenOn(true);
      this.enabled = true;
      result.ok = true;
      hilog.info(DOMAIN, TAG, '%{public}s KEEP_SCREEN_ON ok=true', MARK);
    } catch (e) {
      const be = e as BusinessError;
      result.ok = false;
      result.error_code = be.code;
      result.error_message = be.message;
      hilog.error(DOMAIN, TAG, '%{public}s KEEP_SCREEN_ON ok=false code=%{public}d message=%{public}s',
        MARK, be.code, be.message);
    }
    return result;
  }

  /** 恢复系统默认（允许自动熄屏）。重复调用是 no-op；失败只记日志。 */
  async disable(): Promise<void> {
    if (!this.enabled || this.win === null) {
      return;
    }
    this.enabled = false;
    const win = this.win;
    this.win = null;
    try {
      await win.setWindowKeepScreenOn(false);
      hilog.info(DOMAIN, TAG, '%{public}s KEEP_SCREEN_ON released', MARK);
    } catch (e) {
      const be = e as BusinessError;
      hilog.warn(DOMAIN, TAG, '%{public}s KEEP_SCREEN_ON release failed code=%{public}d message=%{public}s',
        MARK, be.code, be.message);
    }
  }
}

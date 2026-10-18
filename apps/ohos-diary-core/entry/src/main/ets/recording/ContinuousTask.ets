// 长时任务（ContinuousTask）：让「正在录音」在退到后台/锁屏后不被系统回收。
//
// 用的是 `backgroundTaskManager.startBackgroundRunning(context, AUDIO_RECORDING, wantAgent)`
// ——鸿蒙的长时任务，会在通知栏挂一条「正在录音」的通知（系统生成，不需要应用自己发）。
//
// 权限与声明（缺一不可）：
//   - module.json5 的 `requestPermissions` 里声明 `ohos.permission.KEEP_BACKGROUND_RUNNING`；
//   - EntryAbility 的 `backgroundModes` 里声明 `audioRecording`；
//   - 运行时还必须已经拿到 `ohos.permission.MICROPHONE`（否则长时任务会被系统判定
//     「没有在录音」并取消：`SYSTEM_CANCEL_AUDIO_RECORDING_NOT_RUNNING`）。
//
// 这台 2in1 上到底能不能锁屏继续录，见 `docs/architecture/m6-平板与PC平台能力.md`
// 的实测表；代码不假设成功：失败会如实返回错误码，录音本身仍在前台继续。

import { backgroundTaskManager } from '@kit.BackgroundTasksKit';
import { common, wantAgent, WantAgent, Want } from '@kit.AbilityKit';
import { BusinessError } from '@kit.BasicServicesKit';
import { hilog } from '@kit.PerformanceAnalysisKit';

const DOMAIN: number = 0x0000;
const TAG: string = 'DiaryRec';
const MARK: string = 'DIARY_REC';

/** 长时任务的结果：失败也要把系统错误码带出去，不能假装成功。 */
export class ContinuousTaskResult {
  ok: boolean = false;
  error_code: number = 0;
  error_message: string = '';
}

/** 长时任务控制器：一次录音一个实例。 */
export class ContinuousTaskController {
  private running: boolean = false;

  isRunning(): boolean {
    return this.running;
  }

  /** 申请 AUDIO_RECORDING 长时任务。必须在 UIAbility 还处于前台时调用。 */
  async start(context: common.UIAbilityContext): Promise<ContinuousTaskResult> {
    const result = new ContinuousTaskResult();
    try {
      const want: Want = {
        bundleName: context.abilityInfo.bundleName,
        abilityName: context.abilityInfo.name,
      };
      const info: wantAgent.WantAgentInfo = {
        wants: [want],
        actionType: wantAgent.OperationType.START_ABILITY,
        requestCode: 20261018,
        actionFlags: [wantAgent.WantAgentFlags.UPDATE_PRESENT_FLAG],
      };
      const agent: WantAgent = await wantAgent.getWantAgent(info);
      await backgroundTaskManager.startBackgroundRunning(
        context,
        backgroundTaskManager.BackgroundMode.AUDIO_RECORDING,
        agent,
      );
      this.running = true;
      result.ok = true;
      hilog.info(DOMAIN, TAG, '%{public}s TASK start ok mode=audioRecording',
        MARK);
      return result;
    } catch (e) {
      const be = e as BusinessError;
      result.ok = false;
      result.error_code = be.code;
      result.error_message = be.message;
      hilog.error(DOMAIN, TAG, '%{public}s TASK start failed code=%{public}d message=%{public}s',
        MARK, be.code, be.message);
      return result;
    }
  }

  /** 停止长时任务。失败只记日志：此时录音已经停了，清理失败不该掩盖主流程。 */
  async stop(context: common.UIAbilityContext): Promise<void> {
    if (!this.running) {
      return;
    }
    this.running = false;
    try {
      await backgroundTaskManager.stopBackgroundRunning(context);
      hilog.info(DOMAIN, TAG, '%{public}s TASK stop ok', MARK);
    } catch (e) {
      const be = e as BusinessError;
      hilog.warn(DOMAIN, TAG, '%{public}s TASK stop failed code=%{public}d message=%{public}s',
        MARK, be.code, be.message);
    }
  }
}

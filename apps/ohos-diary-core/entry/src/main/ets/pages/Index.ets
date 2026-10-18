/**
 * 示例页面：录音能力的**手动入口**（真机验证也可以用 hdc 的 verify_mode）。
 *
 * 点「录 5 秒」会走完整链路：申请麦克风权限 → AudioCapturer → 2 秒分片 → 核心
 * registerSegment → finalize（拼音频 + 建 Asset + 排转写任务）。结果在 hilog：
 *
 *   hdc shell "hilog -x -T DiaryRec" | grep DIARY_REC
 *
 * 页面不自己实现录音逻辑：产品代码应该直接用 `recording/VoiceRecorder.ets`。
 */
import { common } from '@kit.AbilityKit';
import { hilog } from '@kit.PerformanceAnalysisKit';
import { RecordingVerify } from '../recording/RecordingVerify';

const DOMAIN: number = 0x0000;
const TAG: string = 'DiaryRec';

@Entry
@Component
struct Index {
  @State message: string = 'Diary Core · 录音平台能力';
  @State status: string = '未开始';
  @State busy: boolean = false;

  private runMode(mode: string, label: string): void {
    if (this.busy) {
      return;
    }
    this.busy = true;
    this.status = `${label}中…（权限弹框请允许麦克风）`;
    const context = getContext(this) as common.UIAbilityContext;
    RecordingVerify.run(mode, context, context.filesDir)
      .then(() => {
        this.busy = false;
        this.status = `${label}结束，结果见 hilog（tag DiaryRec）`;
      })
      .catch((err: Object) => {
        this.busy = false;
        this.status = `${label}失败，见 hilog`;
        hilog.error(DOMAIN, TAG, 'INDEX %{public}s failed %{public}s', mode, JSON.stringify(err));
      });
  }

  build() {
    Column({ space: 12 }) {
      Text(this.message)
        .fontSize(24)
        .fontWeight(FontWeight.Bold)
      Text(this.status)
        .fontSize(14)
        .margin({ top: 8 })
      Button('录 5 秒（2 秒一段）')
        .enabled(!this.busy)
        .onClick(() => {
          this.runMode('record', '录音 5 秒');
        })
      Button('查看上次未收尾的录音')
        .enabled(!this.busy)
        .onClick(() => {
          this.runMode('recover', '查询恢复');
        })
      Text('日志：hdc shell "hilog -x -T DiaryRec" | grep DIARY_REC')
        .fontSize(12)
        .margin({ top: 12 })
    }
    .width('100%')
    .height('100%')
    .justifyContent(FlexAlign.Center)
  }
}

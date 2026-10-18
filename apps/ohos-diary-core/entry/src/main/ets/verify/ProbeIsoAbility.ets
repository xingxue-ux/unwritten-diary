// 多进程验证 Ability（#122 未验清单第 9 项）。
//
// module.json5 里给它配了 `"process": ":probe"`，所以它**不与 EntryAbility 同进程**。
// 启动后打开同一个 library.sqlite 并持有句柄：
//   - 两个进程各自打印自己的 pid 与 handles=1；
//   - EntryAbility 那侧的 handles 不受影响（进程内的 shim 句柄表是各自独立的）；
//   - 同一路径在两个进程各开一个 Core（C ABI 的 OPEN_LIBRARIES 是进程内静态）。

import { AbilityConstant, UIAbility, Want } from '@kit.AbilityKit';
import { hilog } from '@kit.PerformanceAnalysisKit';
import { window } from '@kit.ArkUI';
import { process } from '@kit.ArkTS';
import { DiaryCore } from '../core/DiaryCore';

const DOMAIN: number = 0x0000;
const TAG: string = 'DiaryCore';
const MARK: string = 'DIARY_VERIFY';

export default class ProbeIsoAbility extends UIAbility {
  private core: DiaryCore | null = null;

  onCreate(want: Want, launchParam: AbilityConstant.LaunchParam): void {
    hilog.info(DOMAIN, TAG, '%{public}s ISO onCreate pid=%{public}d', MARK, process.pid);
  }

  onWindowStageCreate(windowStage: window.WindowStage): void {
    try {
      this.core = DiaryCore.open(`${this.context.filesDir}/library.sqlite`);
      hilog.info(DOMAIN, TAG, '%{public}s ISO HOLD open pid=%{public}d handles=%{public}d',
        MARK, process.pid, DiaryCore.openHandleCount());
    } catch (e) {
      hilog.error(DOMAIN, TAG, '%{public}s ISO HOLD failed %{public}s',
        MARK, (e as Error).message);
    }
    windowStage.loadContent('pages/Index', (err) => {
      if (err.code) {
        hilog.error(DOMAIN, TAG, '%{public}s ISO loadContent failed %{public}s',
          MARK, JSON.stringify(err));
      }
    });
  }

  onDestroy(): void {
    if (this.core !== null) {
      try {
        this.core.close();
        hilog.info(DOMAIN, TAG, '%{public}s ISO CLOSED pid=%{public}d handles=%{public}d',
          MARK, process.pid, DiaryCore.openHandleCount());
      } catch (e) {
        hilog.error(DOMAIN, TAG, '%{public}s ISO CLOSE failed %{public}s',
          MARK, (e as Error).message);
      }
      this.core = null;
    }
  }
}

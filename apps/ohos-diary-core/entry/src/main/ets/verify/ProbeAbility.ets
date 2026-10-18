// 真机验证辅助 Ability（#122 未验清单第 9 项：多 Ability / 多进程）。
//
// 用途：
//   aa start -a ProbeAbility -b com.example.myapplication -m entry --ps verify_mode multi_hold
//
// `multi_hold` 打开 library.sqlite 后**不关**，等另一个 Ability（EntryAbility）启动并跑
// Smoke；如果两个 Ability 在同一个进程里，EntryAbility 的 `open OK handles=2` 就证明
// 它们共享同一张 shim 句柄表 / 同一个 Core。日志里 pid 也用来判断是否同进程。

import { AbilityConstant, UIAbility, Want } from '@kit.AbilityKit';
import { hilog } from '@kit.PerformanceAnalysisKit';
import { window } from '@kit.ArkUI';
import { process } from '@kit.ArkTS';
import { DiaryCore } from '../core/DiaryCore';
import { DeviceVerify } from './DeviceVerify';

const DOMAIN: number = 0x0000;
const TAG: string = 'DiaryCore';
const MARK: string = 'DIARY_VERIFY';

export default class ProbeAbility extends UIAbility {
  private mode: string = 'multi_hold';
  private core: DiaryCore | null = null;
  private timer: number = -1;

  onCreate(want: Want, launchParam: AbilityConstant.LaunchParam): void {
    const params = want.parameters;
    if (params !== undefined && params !== null) {
      const value = params['verify_mode'];
      if (typeof value === 'string') {
        this.mode = value;
      }
    }
    hilog.info(DOMAIN, TAG, '%{public}s PROBE onCreate mode=%{public}s pid=%{public}d',
      MARK, this.mode, process.pid);
  }

  onWindowStageCreate(windowStage: window.WindowStage): void {
    const filesDir = this.context.filesDir;
    if (this.mode === 'multi_hold') {
      this.hold(filesDir);
    } else {
      DeviceVerify.run(this.mode, filesDir);
    }
    windowStage.loadContent('pages/Index', (err) => {
      if (err.code) {
        hilog.error(DOMAIN, TAG, '%{public}s PROBE loadContent failed %{public}s',
          MARK, JSON.stringify(err));
      }
    });
  }

  private hold(filesDir: string): void {
    try {
      this.core = DiaryCore.open(`${filesDir}/library.sqlite`);
      hilog.info(DOMAIN, TAG, '%{public}s PROBE HOLD open pid=%{public}d handles=%{public}d',
        MARK, process.pid, DiaryCore.openHandleCount());
      // 后台 timer 可能被系统挂起；onDestroy 里也会关，这里只是尽力而为。
      this.timer = setTimeout(() => {
        this.closeHeld('timer');
      }, 60000);
    } catch (e) {
      DeviceVerify.run('diag', filesDir);
      hilog.error(DOMAIN, TAG, '%{public}s PROBE HOLD failed %{public}s',
        MARK, (e as Error).message);
    }
  }

  private closeHeld(reason: string): void {
    if (this.timer !== -1) {
      clearTimeout(this.timer);
      this.timer = -1;
    }
    if (this.core !== null) {
      try {
        this.core.close();
        this.core = null;
        hilog.info(DOMAIN, TAG, '%{public}s PROBE CLOSED reason=%{public}s pid=%{public}d handles=%{public}d',
          MARK, reason, process.pid, DiaryCore.openHandleCount());
      } catch (e) {
        hilog.error(DOMAIN, TAG, '%{public}s PROBE CLOSE failed %{public}s',
          MARK, (e as Error).message);
      }
    }
  }

  onDestroy(): void {
    this.closeHeld('destroy');
    hilog.info(DOMAIN, TAG, '%{public}s PROBE onDestroy pid=%{public}d', MARK, process.pid);
  }
}

import { AbilityConstant, UIAbility, Want } from '@kit.AbilityKit';
import { hilog } from '@kit.PerformanceAnalysisKit';
import { window } from '@kit.ArkUI';
import { DiaryCoreSmoke } from '../core/Smoke';

const DOMAIN: number = 0x0000;
const TAG: string = 'DiaryCore';
const MARK: string = 'DIARY_SMOKE';

/**
 * 最小示例 Ability：应用启动时跑一遍「打开库 → coreInfo → 建草稿 → 保存 → 提交 → 抽取 →
 * 检索 → 排索引任务 → 跑任务 → 关闭」，结果全部打到 hilog（tag `DiaryCore`）。
 *
 * ⚠️ 真机没连着，本 Ability **没有在设备上运行过**；它只保证「工程结构完整、ArkTS 能编」。
 * 设备上的验证清单见 docs/architecture/m6-ArkTS-NAPI.md §9。
 */
export default class EntryAbility extends UIAbility {
  onCreate(want: Want, launchParam: AbilityConstant.LaunchParam): void {
    hilog.info(DOMAIN, TAG, '%{public}s onCreate', MARK);
  }

  onWindowStageCreate(windowStage: window.WindowStage): void {
    // 同步跑（示例刻意简单）：产品里应当挪到 TaskPool worker，避免阻塞 UI 线程。
    DiaryCoreSmoke.run(this.context.filesDir);

    const t0 = Date.now();
    windowStage.loadContent('pages/Index', (err) => {
      if (err.code) {
        hilog.error(DOMAIN, TAG, '%{public}s loadContent failed %{public}s',
          MARK, JSON.stringify(err));
        return;
      }
      hilog.info(DOMAIN, TAG, '%{public}s loadContent ok contentMs=%{public}d',
        MARK, Date.now() - t0);
    });
  }

  onDestroy(): void {
    hilog.info(DOMAIN, TAG, '%{public}s onDestroy', MARK);
  }
}

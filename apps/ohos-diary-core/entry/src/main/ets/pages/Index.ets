// 产品界面根页面：底部 Tabs（今日 / 搜索 / 任务 / 设置）+ Navigation 二级页。
//
// 页面：
//   pages/TodayPage              今日（captures.list）
//   pages/CaptureDetailPage      记录详情（captures.get / sources.extract / captures.commit）
//   pages/RecordingPage          录音（VoiceRecorder / finalize / recover）
//   pages/SearchPage             搜索（search.start/nextPage/upgrade/cancel）
//   pages/JobsPage               任务中心（jobs.list/get/retry/cancel/runDue）
//   pages/SettingsPage           设置（core.info / indexes.status / indexes.rebuildSemantic）
//   pages/TranscriptionConfigPage 转写 provider（providers.list/save/test）
//
// 所有核心调用都在 `ui/CoreGateway.ets` 里经 TaskPool 执行；本文件不直接调核心。

import { common } from '@kit.AbilityKit';
import { CoreGateway } from '../ui/CoreGateway';
import { AppState } from '../ui/AppState';
import { databasePath } from '../ui/AppRuntime';
import { RunDueParams } from '../core/Models';
import { TodayPage } from './TodayPage';
import { CaptureDetailPage } from './CaptureDetailPage';
import { RecordingPage } from './RecordingPage';
import { SearchPage } from './SearchPage';
import { JobsPage } from './JobsPage';
import { SettingsPage } from './SettingsPage';
import { TranscriptionConfigPage } from './TranscriptionConfigPage';

@Entry
@Component
struct Index {
  private pathStack: NavPathStack = new NavPathStack();
  @State currentTab: number = 0;
  private lastForegroundKickMs: number = 0;

  aboutToAppear(): void {
    AppState.init();
  }

  onPageShow(): void {
    // 回到前台：让今日/任务重读；顺手推一轮到期任务（OHOS 没有后台唤醒）。
    AppState.bumpDataVersion();
    this.kickDueJobs();
  }

  private kickDueJobs(): void {
    const now: number = Date.now();
    if (now - this.lastForegroundKickMs < 30000) {
      return;
    }
    this.lastForegroundKickMs = now;
    const context = getContext(this) as common.UIAbilityContext;
    const params: RunDueParams = {
      budget_ms: 1500,
      trigger: 'foreground',
    };
    CoreGateway.call(databasePath(context), 'jobs.runDue', params).then((result) => {
      if (result.ok) {
        AppState.bumpDataVersion();
      }
    });
  }

  @Builder
  PageMap(name: string, param: string) {
    if (name === 'capture') {
      CaptureDetailPage({ pathStack: this.pathStack, captureId: param as string })
    } else if (name === 'recording') {
      RecordingPage({ pathStack: this.pathStack })
    } else if (name === 'transcription') {
      TranscriptionConfigPage({ pathStack: this.pathStack })
    } else if (name === 'settings') {
      NavDestination() {
        SettingsPage({ pathStack: this.pathStack })
      }
      .title('设置')
    } else if (name === 'jobs') {
      NavDestination() {
        JobsPage({ pathStack: this.pathStack })
      }
      .title('任务中心')
    }
  }

  build() {
    Navigation(this.pathStack) {
      Tabs({ barPosition: BarPosition.End, index: this.currentTab }) {
        TabContent() {
          TodayPage({ pathStack: this.pathStack })
        }
        .tabBar('今日')

        TabContent() {
          SearchPage({ pathStack: this.pathStack })
        }
        .tabBar('搜索')

        TabContent() {
          JobsPage({ pathStack: this.pathStack })
        }
        .tabBar('任务')

        TabContent() {
          SettingsPage({ pathStack: this.pathStack })
        }
        .tabBar('设置')
      }
      .barMode(BarMode.Fixed)
      .width('100%')
      .height('100%')
      .onChange((index: number) => {
        this.currentTab = index;
      })
    }
    .navDestination(this.PageMap)
    .hideTitleBar(true)
    .mode(NavigationMode.Stack)
    .width('100%')
    .height('100%')
  }
}

// 产品界面根页面：底部 Tabs（记录 / 日记 / 搜索 / 片段 / 我的）+ Navigation 二级页。
//
// 一级入口是契约 §2 冻结的 5 个、顺序固定（`tools/check_ui_parity.py` 逐字比对）：
//   记录 → 日记 → 搜索 → 片段 → 我的
// 任务中心**不是**一级入口：它从全局 header 的「任务」按钮、以及「我的 → 创作与整理 →
// 任务中心」进（契约 §0.2 / §3）。
//
// 页面：
//   pages/RecordPage             记录（文字新建 + 录音入口）
//   pages/DiaryPage              日记（按天倒序的成篇正文列表；diary.getDay/listVersions）
//   pages/DiaryDayPage           日记某一天（正文 + 版本 + 切换当前版本；diary.*）
//   pages/SearchPage             搜索（search.start/nextPage/upgrade/cancel）
//   pages/SnippetsPage           片段（captures.list；原始记录与转写状态）
//   pages/ProfilePage            我的（设置分组 + 任务中心入口）
//   pages/CaptureDetailPage      记录详情（captures.get / sources.extract / captures.commit）
//   pages/RecordingPage          录音（VoiceRecorder / finalize / recover）
//   pages/JobsPage               任务中心（jobs.list/get/retry/cancel/runDue）
//   pages/TranscriptionConfigPage 转写 provider（providers.list/save/test）
//
// 所有核心调用都在 `ui/CoreGateway.ets` 里经 TaskPool 执行；本文件不直接调核心。

import { common } from '@kit.AbilityKit';
import { CoreGateway } from '../ui/CoreGateway';
import { AppState } from '../ui/AppState';
import { databasePath } from '../ui/AppRuntime';
import { RunDueParams } from '../core/Models';
import { RecordPage } from './RecordPage';
import { DiaryPage } from './DiaryPage';
import { DiaryDayPage } from './DiaryDayPage';
import { SnippetsPage } from './SnippetsPage';
import { CaptureDetailPage } from './CaptureDetailPage';
import { RecordingPage } from './RecordingPage';
import { SearchPage } from './SearchPage';
import { JobsPage } from './JobsPage';
import { ProfilePage } from './ProfilePage';
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
    // 回到前台：让记录/日记/片段重读；顺手推一轮到期任务（OHOS 没有后台唤醒）。
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
    } else if (name === 'diaryDay') {
      NavDestination() {
        DiaryDayPage({ pathStack: this.pathStack, dayKey: param as string })
      }
      .title('日记')
    } else if (name === 'settings') {
      // 路由名保留 `settings`：SearchPage / JobsPage 已经在用它跳「我的」，
      // 改路由名只会制造无谓的跨文件改动（本 PR 不动 SearchPage）。
      NavDestination() {
        ProfilePage({ pathStack: this.pathStack })
      }
      .title('我的')
    } else if (name === 'jobs') {
      NavDestination() {
        JobsPage({ pathStack: this.pathStack })
      }
      .title('任务中心')
    }
  }

  build() {
    Navigation(this.pathStack) {
      Column() {
        // 全局 header：任务中心是二级页，这里只放一个入口（另一个在「我的」）。
        Row({ space: 8 }) {
          Text('不写日记')
            .fontSize(17)
            .fontWeight(FontWeight.Medium)
            .fontColor($r('sys.color.ohos_id_color_text_primary'))
          Blank()
          Button('任务')
            .fontSize(13)
            .height(32)
            .backgroundColor($r('sys.color.ohos_id_color_sub_background'))
            .fontColor($r('sys.color.ohos_id_color_text_primary'))
            .onClick(() => {
              this.pathStack.pushPathByName('jobs', '');
            })
        }
        .width('100%')
        .height(48)
        .padding({ left: 16, right: 16 })

        Tabs({ barPosition: BarPosition.End, index: this.currentTab }) {
          TabContent() {
            RecordPage({ pathStack: this.pathStack })
          }
          .tabBar('记录')

          TabContent() {
            DiaryPage({ pathStack: this.pathStack })
          }
          .tabBar('日记')

          TabContent() {
            SearchPage({ pathStack: this.pathStack })
          }
          .tabBar('搜索')

          TabContent() {
            SnippetsPage({ pathStack: this.pathStack })
          }
          .tabBar('片段')

          TabContent() {
            ProfilePage({ pathStack: this.pathStack })
          }
          .tabBar('我的')
        }
        .barMode(BarMode.Fixed)
        .width('100%')
        .layoutWeight(1)
        .onChange((index: number) => {
          if (this.currentTab === index) {
            return;
          }
          this.currentTab = index;
          // TabContent 不会因为切换重新 `aboutToAppear`：在「记录」里刚写完，切到
          // 「片段/日记/我的」时那些页面还是旧数据。切 tab 主动让它们重读一次
          // （各页用 `@StorageLink('ohosDataVersion')` + `@Watch` 接这一下）。
          AppState.bumpDataVersion();
        })
      }
      .width('100%')
      .height('100%')
    }
    .navDestination(this.PageMap)
    .hideTitleBar(true)
    .mode(NavigationMode.Stack)
    .width('100%')
    .height('100%')
  }
}
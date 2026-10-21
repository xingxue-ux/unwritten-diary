// 产品界面根页面：**顶部导航条**（对齐 Windows 桌面版）+ Navigation 二级页。
//
// 一级入口是契约 §2 冻结的 5 个、顺序固定（本文件逐字写死；检查器现状见下）：
//   记录 → 日记 → 搜索 → 片段 → 我的
// 任务中心**不是**一级入口：它从全局 header 的「任务」按钮、以及「我的 → 创作与整理 →
// 任务中心」进（契约 §0.2 / §3）。
//
// 壳与 Windows 的对齐口径（`apps/diary_app/lib/src/experience_shell.dart:588-612`）：
//   - 导航从底部 Tabs 移到**顶部横排文字按钮**：当前项 `ink` + 加粗，其余 `muted`，
//     项间距 6（Windows 每项 `padding: EdgeInsets.only(right: 6)`）；
//   - 最左标题「不写日记」33 / ink / 加粗；最右标语「给今天留一页」13 / muted；
//   - 顶部条底色 `canvas`，底边 1vp `line` 分隔线；根背景 `canvas`；
//   - 颜色 / 字号全部来自 `ui/Sketch.ets`，不再取系统色。
//
// **检查器现状**：`tools/check_ui_parity.py` 目前只解析旧的底部页签结构；壳搬到
// 顶部后它会打印 `[跳过]`（exit 0，不等于入口一致）。本 PR 的范围只动 Index/Sketch，
// 检查器适配留给后续工具线；这里的 5 个 NavItem 调用需人工核对顺序/文案与契约 §2 一致。
//
// **已知差异**：Windows 最左还有猫头标记（CatMark），任务中心是右上角铃铛图标
// （`Icons.notifications_none`，tooltip「任务中心」）。鸿蒙没有 Material Icons，
// 也没有随包图标资源，所以保留文字按钮「任务」，改成 ink 色 + 无背景。
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
import { SketchColors, SketchType } from '../ui/Sketch';
import { RecordPage } from './RecordPage';
import { DiaryPage } from './DiaryPage';
import { DiaryDayPage } from './DiaryDayPage';
import { SnippetsPage } from './SnippetsPage';
import { CaptureDetailPage } from './CaptureDetailPage';
import { RecordingPage } from './RecordingPage';
import { SearchPage } from './SearchPage';
import { JobsPage } from './JobsPage';
import { ProfilePage } from './ProfilePage';
import { RecyclePage } from './RecyclePage';
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

  /**
   * 顶部导航项：当前项 ink + 加粗，其余 muted + 常规（Windows `_header` 同款）。
   * 间距 6 由 `margin` 给；不设背景，保持「文字按钮」而不是系统 Button。
   */
  @Builder
  NavItem(label: string, index: number) {
    Text(label)
      .fontSize(SketchType.section)
      .fontWeight(this.currentTab === index ? FontWeight.Bold : FontWeight.Normal)
      .fontColor(this.currentTab === index ? SketchColors.ink : SketchColors.muted)
      .padding({ left: 12, right: 12, top: 8, bottom: 8 })
      .margin({ right: 6 })
      .onClick(() => {
        this.switchTo(index);
      })
  }

  /**
   * 切一级入口。
   *
   * 内容区用 Stack 常驻 5 个页面（见 `build()`），切走再切回来不会重建，所以
   * **不能**指望 `aboutToAppear` 重读：仍然沿用原来的机制，切 tab 时 bump
   * `ohosDataVersion`，各页用 `@StorageLink('ohosDataVersion')` + `@Watch` 接这一下。
   */
  private switchTo(index: number): void {
    if (this.currentTab === index) {
      return;
    }
    this.currentTab = index;
    // 页签 / Stack 都**不会**因为切换重新 `aboutToAppear`：在「记录」里刚写完，
    // 切到「片段/日记/我的」时那些页面还是旧数据。切 tab 主动让它们重读一次。
    AppState.bumpDataVersion();
  }

  /** 二级页占位：只立路由骨架，页面内容由对应线填充。 */
  @Builder
  PlaceholderPage(title: string, note: string) {
    Column({ space: 8 }) {
      Text(title)
        .fontSize(SketchType.pageTitle)
        .fontWeight(FontWeight.Medium)
        .fontColor(SketchColors.ink)
      Text(note)
        .fontSize(SketchType.body)
        .fontColor(SketchColors.muted)
    }
    .width('100%')
    .height('100%')
    .padding(24)
    .alignItems(HorizontalAlign.Start)
    .backgroundColor(SketchColors.canvas)
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
    } else if (name === 'recycle') {
      NavDestination() {
        RecyclePage()
      }
      .title('回收站')
    } else if (name === 'styles') {
      // 路由骨架：风格线填充。
      NavDestination() {
        this.PlaceholderPage('风格', '占位页：由风格线填充（路由名 styles）。')
      }
      .title('风格')
    } else if (name === 'export') {
      // 路由骨架：导出线填充。
      NavDestination() {
        this.PlaceholderPage('导出', '占位页：由导出线填充（路由名 export）。')
      }
      .title('导出')
    }
  }

  build() {
    Navigation(this.pathStack) {
      Column() {
        // ── 顶部条：与 Windows 桌面版 `_header(desktop: true)` 同构 ──────────────
        // 标题 → 5 个文字入口 → Blank 推开 → 标语 → 任务中心。
        Row() {
          Text('不写日记')
            .fontSize(SketchType.display)
            .fontWeight(FontWeight.Bold)
            .fontColor(SketchColors.ink)
            .maxLines(1)
            .margin({ right: 40 })

          this.NavItem('记录', 0)
          this.NavItem('日记', 1)
          this.NavItem('搜索', 2)
          this.NavItem('片段', 3)
          this.NavItem('我的', 4)

          Blank()

          Text('给今天留一页')
            .fontSize(SketchType.body)
            .fontColor(SketchColors.muted)
            .margin({ right: 16 })

          // Windows 这里是铃铛图标（`Icons.notifications_none` + tooltip「任务中心」）；
          // 鸿蒙没有 Material Icons、也没有随包图标资源，保留文字「任务」，
          // 但改成 ink 色、无背景（原来是系统 `sub_background` 的 Button）。
          Text('任务')
            .fontSize(SketchType.body)
            .fontColor(SketchColors.ink)
            .padding({ left: 12, right: 12, top: 8, bottom: 8 })
            .onClick(() => {
              this.pathStack.pushPathByName('jobs', '');
            })
        }
        .width('100%')
        .height(76)
        .padding({ left: 46, right: 46 })
        .alignItems(VerticalAlign.Center)
        .backgroundColor(SketchColors.canvas)
        .border({ width: { bottom: 1 }, color: SketchColors.line })

        // ── 内容区：5 个页面常驻，按 currentTab 显隐 ──────────────────────────
        // 用 Stack + visibility 而不是 `if`：页面只创建一次，切走再切回来保留滚动位置
        // 和未提交的输入（与原来的页签行为一致）。隐藏页不占布局、不参与命中测试。
        // 代价是 5 个页面在启动时都会 aboutToAppear；数据量小，换来的是切页不闪。
        Stack({ alignContent: Alignment.TopStart }) {
          Column() {
            RecordPage({ pathStack: this.pathStack })
          }
          .width('100%')
          .height('100%')
          .visibility(this.currentTab === 0 ? Visibility.Visible : Visibility.None)

          Column() {
            DiaryPage({ pathStack: this.pathStack })
          }
          .width('100%')
          .height('100%')
          .visibility(this.currentTab === 1 ? Visibility.Visible : Visibility.None)

          Column() {
            SearchPage({ pathStack: this.pathStack })
          }
          .width('100%')
          .height('100%')
          .visibility(this.currentTab === 2 ? Visibility.Visible : Visibility.None)

          Column() {
            SnippetsPage({ pathStack: this.pathStack })
          }
          .width('100%')
          .height('100%')
          .visibility(this.currentTab === 3 ? Visibility.Visible : Visibility.None)

          Column() {
            ProfilePage({ pathStack: this.pathStack })
          }
          .width('100%')
          .height('100%')
          .visibility(this.currentTab === 4 ? Visibility.Visible : Visibility.None)
        }
        .width('100%')
        .layoutWeight(1)
        .backgroundColor(SketchColors.canvas)
      }
      .width('100%')
      .height('100%')
      .backgroundColor(SketchColors.canvas)
    }
    .navDestination(this.PageMap)
    .hideTitleBar(true)
    .mode(NavigationMode.Stack)
    .width('100%')
    .height('100%')
    .backgroundColor(SketchColors.canvas)
  }
}
// 与 Windows 共用视觉基线；850vp 以上顶栏，窄屏底栏。页面常驻以保留输入和滚动位置。
import { common } from '@kit.AbilityKit';
import { CoreGateway } from '../ui/CoreGateway';
import { AppState } from '../ui/AppState';
import { databasePath } from '../ui/AppRuntime';
import { RunDueParams } from '../core/Models';
import { CatMark, SketchColors, SketchFonts, sketchOutline } from '../ui/Sketch';
import { StylesPage } from './StylesPage';
import { ExportPage } from './ExportPage';
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
import { LlmConfigPage } from './LlmConfigPage';

@Entry
@Component
struct Index {
  private pathStack: NavPathStack = new NavPathStack();
  @State currentTab: number = 0;
  @State desktop: boolean = false;
  @State availableWidth: number = 0;
  private lastForegroundKickMs: number = 0;

  aboutToAppear(): void {
    AppState.init();
    AppStorage.setOrCreate('ohosWideLayout', false);
    this.getUIContext().getFont().registerFont({
      familyName: SketchFonts.display,
      familySrc: $rawfile('fonts/DiaryDisplay-AppSubset.ttf'),
    });
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
    Column({ space: 3 }) {
      if (!this.desktop) {
        Image(this.navIcon(index)).width(21).height(21)
          .opacity(this.currentTab === index ? 1 : 0.65)
      }
      Text(label)
        .fontSize(this.desktop ? 14 : 11)
        .fontWeight(this.currentTab === index ? FontWeight.Bold : FontWeight.Normal)
        .fontColor(this.currentTab === index ? SketchColors.ink : SketchColors.muted)
    }
      .padding({ left: this.desktop ? 12 : 6, right: this.desktop ? 12 : 6, top: 10, bottom: 10 })
      .margin({ right: this.desktop ? 6 : 0 })
      .constraintSize({ minHeight: 48 })
      .layoutWeight(this.desktop ? 0 : 1)
      .accessibilitySelected(this.currentTab === index)
      .accessibilityLevel('yes')
      .onClick(() => {
        this.switchTo(index);
      })
  }

  private navIcon(index: number): Resource {
    if (index === 0) { return $r('app.media.sketch_record'); }
    if (index === 1) { return $r('app.media.sketch_diary'); }
    if (index === 2) { return $r('app.media.sketch_search'); }
    if (index === 3) { return $r('app.media.sketch_snippets'); }
    return $r('app.media.sketch_profile');
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

  /** 五个入口仅声明一次，宽屏顶栏和窄屏底栏使用相同顺序。 */
  @Builder
  PrimaryNavigation() {
    this.NavItem('记录', 0)
    this.NavItem('日记', 1)
    this.NavItem('搜索', 2)
    this.NavItem('片段', 3)
    this.NavItem('我的', 4)
  }

  @Builder
  PageMap(name: string, param: string) {
    if (name === 'capture') {
      CaptureDetailPage({ pathStack: this.pathStack, captureId: param as string })
    } else if (name === 'recording') {
      RecordingPage({ pathStack: this.pathStack })
    } else if (name === 'transcription') {
      TranscriptionConfigPage({ pathStack: this.pathStack })
    } else if (name === 'llm') {
      LlmConfigPage({ pathStack: this.pathStack })
    } else if (name === 'diaryDay') {
      NavDestination() {
        DiaryDayPage({ pathStack: this.pathStack, dayKey: param as string })
      }
      .title('日记', { backgroundColor: SketchColors.canvas })
    } else if (name === 'settings' || name === 'diagnostics') {
      // 旧 settings 入口保留；索引入口直接打开诊断面板，避免在「我的」中再次查找。
      NavDestination() {
        ProfilePage({ pathStack: this.pathStack, diagnosticsOnly: name === 'diagnostics' })
      }
      .title(name === 'diagnostics' ? '搜索索引' : '我的', { backgroundColor: SketchColors.canvas })
    } else if (name === 'jobs') {
      NavDestination() {
        JobsPage({ pathStack: this.pathStack })
      }
      .title('任务中心', { backgroundColor: SketchColors.canvas })
    } else if (name === 'recycle') {
      NavDestination() {
        RecyclePage()
      }
      .title('回收站', { backgroundColor: SketchColors.canvas })
    } else if (name === 'styles') {
      NavDestination() {
        StylesPage()
      }
      .title('风格偏好', { backgroundColor: SketchColors.canvas })
    } else if (name === 'export') {
      NavDestination() {
        ExportPage()
      }
      .title('阅读导出', { backgroundColor: SketchColors.canvas })
    }
  }

  build() {
    Navigation(this.pathStack) {
      Stack({ alignContent: Alignment.Center }) {
        Column() {
          Column() {
            Row({ space: 15 }) {
              Stack({ alignContent: Alignment.Center }) {
                CatMark()
              }.width(48).height(48).padding(5)
                .backgroundColor(SketchColors.paper).drawModifier(sketchOutline())
              Text('不写日记')
                .fontFamily(SketchFonts.display)
                .fontSize(this.desktop ? 33 : 29)
                .fontColor(SketchColors.ink)
                .maxLines(1)
                .flexShrink(1)
              if (this.desktop) {
                Row() {
                  this.PrimaryNavigation()
                }
                .margin({ left: 25 })
              }
              Blank()
              if (this.desktop && this.availableWidth >= 1100) {
                Text('给今天留一页')
                  .fontSize(13)
                  .fontColor(SketchColors.muted)
              }
              Button({ type: ButtonType.Normal }) {
                Image($r('app.media.sketch_bell'))
                  .width(24)
                  .height(24)
              }
              .width(48)
              .height(48)
              .backgroundColor(Color.Transparent)
              .accessibilityText('任务中心')
              .onClick(() => this.pathStack.pushPathByName('jobs', ''))
            }
            .width('100%')
            .padding({ left: this.desktop ? 46 : 22, right: this.desktop ? 46 : 22,
              top: this.desktop ? 19 : 14, bottom: this.desktop ? 19 : 14 })

            Divider()
              .color(SketchColors.ink)
              .strokeWidth(1)
              .margin({ left: 24, right: 24 })

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
                SearchPage({ pathStack: this.pathStack, visible: this.currentTab === 2 })
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
            .constraintSize({ maxWidth: 1332 })
            .padding({ left: this.desktop ? 30 : 6, right: this.desktop ? 30 : 6,
              top: this.desktop ? 23 : 13 })
            .backgroundColor(SketchColors.canvas)

            if (!this.desktop) {
              Row() {
                this.PrimaryNavigation()
              }
              .width('100%')
              .padding({ left: 12, right: 12 })
              .border({ width: { top: 1 }, color: SketchColors.ink })
              .backgroundColor(SketchColors.paper)
            }
          }
          .width('100%')
          .height('100%')
          .backgroundColor(SketchColors.canvas)
          .drawModifier(sketchOutline())
        }
        .width('100%').height('100%')
        .constraintSize({ maxWidth: 1500 })
        .padding(this.desktop ? 22 : 0)
      }.width('100%').height('100%')
    }
    .navDestination(this.PageMap)
    .onAreaChange((_old: Area, area: Area) => {
      this.availableWidth = Number(area.width);
      this.desktop = Number(area.width) >= 850;
      AppStorage.set('ohosWideLayout', this.desktop);
    })
    .hideTitleBar(true)
    .mode(NavigationMode.Stack)
    .width('100%')
    .height('100%')
    .backgroundColor(SketchColors.canvas)
  }
}

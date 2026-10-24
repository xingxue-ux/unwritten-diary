import { SketchColors, SketchFonts, SketchType, SketchCard, sketchButton, sketchOutline } from '../ui/Sketch';
// 我的：沿用 Windows 的创作与整理、资料与应用分组；设备诊断按需展开。
import { common } from '@kit.AbilityKit';
import { DiaryCore } from '../core/DiaryCore';
import {
  ConfigFileReport,
  IndexStatus,
  Job,
  JobRunSummary,
  JobStatus,
  LibraryInfo,
  RebuildSemanticParams,
  RunDueParams,
  RuntimeConfig,
  Settings,
} from '../core/Models';
import { CoreGateway, CoreRequest, uiErrorOf } from '../ui/CoreGateway';
import { EMPTY_UI_ERROR, UiError } from '../ui/Errors';
import { AppState } from '../ui/AppState';
import { runSummary, timeZoneLabel } from '../ui/UserText';
import { SettingsStore } from '../state/SettingsStore';
import {
  databasePath,
  formatBytes,
  formatDateTime,
  jobStateLabel,
  newOperationId,
} from '../ui/AppRuntime';
import { ErrorBanner, InfoRow, LoadingRow, PageTitle, SectionTitle, StatePill } from '../ui/Common';

@Component
export struct ProfilePage {
  pathStack: NavPathStack = new NavPathStack();
  @Prop diagnosticsOnly: boolean = false;
  @State info: LibraryInfo | null = null;
  @State indexStatus: IndexStatus | null = null;
  @State jobStatus: JobStatus | null = null;
  @State loading: boolean = true;
  @State error: UiError = EMPTY_UI_ERROR;
  @State indexError: UiError = EMPTY_UI_ERROR;
  @State rebuilding: boolean = false;
  @State rebuildMessage: string = '';
  @State rebuildError: UiError = EMPTY_UI_ERROR;
  @State showCapabilities: boolean = false;
  @State showDiagnostics: boolean = false;
  @State showSettings: boolean = false;
  @State hasBacklog: boolean = false;
  /** 设置块：store 的字段镜像（数据与并发都在 store 里，页面只负责显示）。 */
  @State settings: Settings | null = null;
  @State settingsError: UiError = EMPTY_UI_ERROR;
  @State settingsMessage: string = '';
  @State settingsBusy: boolean = false;
  @StorageLink('ohosDataVersion') @Watch('onDataVersionChanged') dataVersion: number = 0;

  private dbPath: string = '';
  /** 设置的数据层（契约 4.4）；页面只读它的字段、调它的方法。 */
  private settingsStore: SettingsStore | null = null;

  aboutToAppear(): void {
    const context = getContext(this) as common.UIAbilityContext;
    this.dbPath = databasePath(context);
    this.settingsStore = new SettingsStore(this.dbPath);
    this.load();
  }

  onDataVersionChanged(): void {
    if (this.dbPath.length > 0) {
      this.load();
    }
  }

  private async load(): Promise<void> {
    this.loading = true;
    this.error = EMPTY_UI_ERROR;
    this.indexError = EMPTY_UI_ERROR;
    this.settingsError = EMPTY_UI_ERROR;
    const requests: CoreRequest[] = [
      { method: 'core.info', params: null },
      { method: 'indexes.status', params: null },
      { method: 'jobs.status', params: null },
    ];
    const results = await CoreGateway.callMany(this.dbPath, requests);
    this.loading = false;
    if (results.length > 0) {
      if (results[0].ok) {
        this.info = results[0].data as LibraryInfo;
      } else {
        this.error = uiErrorOf(results[0]);
      }
    }
    if (results.length > 1) {
      if (results[1].ok) {
        this.indexStatus = results[1].data as IndexStatus;
      } else {
        this.indexError = uiErrorOf(results[1]);
      }
    }
    if (results.length > 2 && results[2].ok) {
      this.jobStatus = results[2].data as JobStatus;
    }
    await this.loadSettings();
  }

  /**
   * 读设置：走 `SettingsStore.refresh()`（`settings.get`）。
   *
   * 读失败**不**让整页失败：这一块单独如实报错，其余区块照常显示。
   */
  private async loadSettings(): Promise<void> {
    const store: SettingsStore | null = this.settingsStore;
    if (store === null) {
      return;
    }
    this.settingsBusy = true;
    await store.refresh();
    this.settingsBusy = false;
    this.syncSettings(store);
  }

  /** store → 页面 `@State` 的唯一同步点（store 不依赖 ArkUI，页面自己镜像）。 */
  private syncSettings(store: SettingsStore): void {
    this.settings = store.settings;
    this.settingsError = store.error;
    this.settingsMessage = store.message;
  }

  private runtimeConfig(): RuntimeConfig | null {
    if (this.info === null) {
      return null;
    }
    const config: RuntimeConfig | undefined = this.info.runtime_config;
    return config === undefined ? null : config;
  }

  private semanticState(): string {
    const config: RuntimeConfig | null = this.runtimeConfig();
    return config === null ? 'unknown' : config.semantic.state;
  }

  private semanticStateLabel(): string {
    const state: string = this.semanticState();
    if (state === 'ready') {
      return '已准备好';
    }
    if (state === 'configured') {
      return '已准备，待检查';
    }
    if (state === 'absent') {
      return '未配置';
    }
    if (state === 'invalid') {
      return '配置无效';
    }
    return '状态未知';
  }

  private semanticStateTone(): string {
    const state: string = this.semanticState();
    if (state === 'ready') {
      return 'ok';
    }
    if (state === 'configured') {
      return 'warn';
    }
    if (state === 'invalid') {
      return 'alert';
    }
    return 'muted';
  }

  private transcriptionState(): string {
    const config: RuntimeConfig | null = this.runtimeConfig();
    return config === null ? 'unknown' : config.transcription.state;
  }

  private transcriptionStateLabel(): string {
    const state: string = this.transcriptionState();
    if (state === 'ready') {
      return '已准备，待检查';
    }
    if (state === 'configured') {
      return '已配置但不完整';
    }
    if (state === 'absent') {
      return '未配置';
    }
    return '状态未知';
  }

  private transcriptionStateTone(): string {
    const state: string = this.transcriptionState();
    if (state === 'ready') {
      return 'ok';
    }
    if (state === 'configured') {
      return 'warn';
    }
    return 'muted';
  }

  private canRebuildSemantic(): boolean {
    const state: string = this.semanticState();
    return state === 'configured' || state === 'ready';
  }

  private async rebuildSemantic(): Promise<void> {
    if (this.rebuilding || !this.canRebuildSemantic()) {
      return;
    }
    this.rebuilding = true;
    this.rebuildMessage = '';
    this.rebuildError = EMPTY_UI_ERROR;
    const params: RebuildSemanticParams = {
      reason: 'user_rebuild',
      operation_id: newOperationId('op-sem'),
    };
    const jobResult = await CoreGateway.call(this.dbPath, 'indexes.rebuildSemantic', params);
    if (!jobResult.ok) {
      this.rebuilding = false;
      this.rebuildError = uiErrorOf(jobResult);
      return;
    }
    const job: Job = jobResult.data as Job;
    this.rebuildMessage = '正在整理搜索内容…';
    await this.runDueOnce();
  }

  private async runDueOnce(): Promise<void> {
    const params: RunDueParams = {
      budget_ms: 30000,
      trigger: 'manual',
    };
    const runResult = await CoreGateway.call(this.dbPath, 'jobs.runDue', params);
    this.rebuilding = false;
    if (!runResult.ok) {
      this.rebuildMessage = '整理未完成，请稍后重试。';
      this.rebuildError = uiErrorOf(runResult);
    } else {
      const summary = runResult.data as JobRunSummary;
      this.rebuildMessage = runSummary(summary);
      this.hasBacklog = summary.due_backlog > 0;
    }
    AppState.bumpDataVersion();
    await this.load();
  }

  private async continueRunDue(): Promise<void> {
    if (this.rebuilding) {
      return;
    }
    this.rebuilding = true;
    this.runDueOnce();
  }

  // ---------------------------------------------------------------- 设置（契约 4.4）
  //
  // 逻辑全在 `state/SettingsStore.ets`：三态、`expected_revision`、冲突重读都在那儿。
  // 这里只做两件事：把 store 的字段镜像到 @State，和把用户动作转成 store 调用。

  /** `build()` 里已经判过非空；这里只做类型收窄，避免每行都写 `as Settings`。 */
  private currentSettings(): Settings {
    return this.settings as Settings;
  }

  private defaultStyleLabel(): string {
    const styleId: string | null = this.currentSettings().default_style_id;
    return styleId === null ? '未设置' : '已设置';
  }

  private async toggleAutoOrganize(isOn: boolean): Promise<void> {
    const store: SettingsStore | null = this.settingsStore;
    if (store === null || this.settingsBusy) {
      return;
    }
    this.settingsBusy = true;
    // 只改开关：store 生成的入参**不含** default_style_id（省略 = 不改）。
    await store.setAutoOrganizeEnabled(isOn);
    this.settingsBusy = false;
    this.syncSettings(store);
  }

  private async clearDefaultStyle(): Promise<void> {
    const store: SettingsStore | null = this.settingsStore;
    if (store === null || this.settingsBusy) {
      return;
    }
    this.settingsBusy = true;
    // 三态里的「显式 null = 清空」；全页只有这一个按钮走这条路。
    await store.clearDefaultStyle();
    this.settingsBusy = false;
    this.syncSettings(store);
  }

  private async reloadSettings(): Promise<void> {
    await this.loadSettings();
  }

  private taskSummary(): string {
    if (this.jobStatus === null) {
      return '还没有读到队列计数';
    }
    const needsAttention: number =
      this.jobStatus.waiting_configuration + this.jobStatus.failed;
    return `${this.jobStatus.queued} 排队 · ${this.jobStatus.running} 运行 · ` +
      `${needsAttention} 需要处理`;
  }

  @Builder
  section(title: string) {
    SectionTitle({ text: title })
  }

  @Builder
  entry(label: string, description: string, route: string) {
    Row({ space: 14 }) {
      Column({ space: 5 }) {
        Text(label).fontSize(16).fontColor(SketchColors.ink)
        Text(route === 'jobs' ? this.taskSummary() : description).fontSize(13).fontColor(SketchColors.muted)
      }.layoutWeight(1).alignItems(HorizontalAlign.Start)
      Text('›').fontSize(24).fontColor(SketchColors.muted)
    }
    .width('100%').padding({ top: 14, bottom: 14 })
    .constraintSize({ minHeight: 64 }).accessibilityLevel('yes')
    .accessibilityText(`${label}，${description}`)
    .onClick(() => {
      if (route === 'schedule') { this.showSettings = !this.showSettings; }
      else { this.pathStack.pushPathByName(route, ''); }
    })
    Divider().color(SketchColors.line)
  }

  @Builder
  settingsPanel() {
        SectionTitle({ text: '设置' })

        Column({ space: 4 }) {
          this.section('整理偏好')
          if (this.settingsError.code.length > 0) {
            ErrorBanner({
              error: this.settingsError,
              showReload: true,
              onReload: () => {
                this.reloadSettings();
              },
            })
          }
          if (this.settings === null) {
            if (this.loading || this.settingsBusy) {
              LoadingRow({ text: '读取设置…' })
            } else {
              Text('设置还没有读到，上面的错误说明了原因；这一块不做本地猜测。')
                .fontSize(12)
                .fontColor(SketchColors.muted)
                .width('100%')
            }
          } else {
            Row({ space: 8 }) {
              Text('自动整理')
                .fontSize(13)
                .fontColor(SketchColors.muted)
                .width(92)
                .flexShrink(0)
              StatePill({
                text: this.currentSettings().auto_organize_enabled ? '已开启' : '已关闭',
                tone: this.currentSettings().auto_organize_enabled ? 'ok' : 'muted',
              })
              Blank()
              Toggle({ type: ToggleType.Switch, isOn: this.currentSettings().auto_organize_enabled })
                .selectedColor(SketchColors.ink)
                .enabled(!this.settingsBusy)
                .onChange((isOn: boolean) => {
                  this.toggleAutoOrganize(isOn);
                })
            }
            .width('100%')
            InfoRow({
              label: '计划时间',
              value: `${this.currentSettings().auto_organize_time}（本地时间）`,
            })
            InfoRow({ label: '日记时区', value: timeZoneLabel(this.currentSettings().diary_time_zone) })
            InfoRow({ label: '默认风格', value: this.defaultStyleLabel() })
            if (this.currentSettings().default_style_id !== null) {
              Row({ space: 8 }) {
                Button('清空默认风格')
                  .attributeModifier(sketchButton).drawModifier(sketchOutline())
                  .fontSize(13)
                  .enabled(!this.settingsBusy)
                  .backgroundColor(SketchColors.soft)
                  .fontColor(SketchColors.text)
                  .onClick(() => {
                    this.clearDefaultStyle();
                  })
                Blank()
              }
              .width('100%')
            }
            Text('可在风格偏好中选择日记的篇幅和语气。')
              .fontSize(12)
              .fontColor(SketchColors.muted)
              .width('100%')
            InfoRow({
              label: '设置版本',
              value: `revision ${this.currentSettings().revision} · ${formatDateTime(this.currentSettings().updated_at)}`,
            })
            if (this.settingsBusy) {
              LoadingRow({ text: '处理设置…' })
            }
            if (this.settingsMessage.length > 0) {
              Text(this.settingsMessage)
                .fontSize(12)
                .fontColor(SketchColors.muted)
                .width('100%')
            }
          }
        }
        .width('100%')
        .padding(14)
        .backgroundColor(SketchColors.paper)
        .drawModifier(sketchOutline())
        .alignItems(HorizontalAlign.Start)

  }

  @Builder
  diagnosticsPanel() {
    Column({ space: 16 }) {
      SketchCard() {
        Column({ space: 12 }) {
          SectionTitle({ text: '语义搜索' })
          ErrorBanner({ error: this.indexError, showReload: true, onReload: () => { this.load(); } })
          if (this.indexStatus === null) {
            LoadingRow({ text: '正在读取搜索状态…' })
          } else {
            StatePill({ text: this.indexStatus.semantic_index_ready ? '已准备好' : '需要整理内容',
              tone: this.indexStatus.semantic_index_ready ? 'ok' : 'warn' })
            Text(`已整理 ${this.indexStatus.embedded_chunks} 段，共 ${this.indexStatus.total_chunks} 段`)
              .fontSize(13).fontColor(SketchColors.muted)
            Text('新增记录后，可在这里更新搜索内容。').fontSize(13).fontColor(SketchColors.muted)
            Button(this.rebuilding ? '整理中…' : '整理搜索内容').attributeModifier(sketchButton).drawModifier(sketchOutline())
              .enabled(!this.rebuilding && this.canRebuildSemantic()).onClick(() => { this.rebuildSemantic(); })
            if (this.hasBacklog) {
              Button('继续整理').attributeModifier(sketchButton).drawModifier(sketchOutline())
                .enabled(!this.rebuilding).onClick(() => { this.continueRunDue(); })
            }
            if (!this.canRebuildSemantic()) { Text('本地模型尚未就绪，请重新打开应用。').fontSize(13).fontColor(SketchColors.warning) }
            ErrorBanner({ error: this.rebuildError })
            if (this.rebuildMessage.length > 0) { Text(this.rebuildMessage).fontSize(13).fontColor(SketchColors.text) }
          }
        }.width('100%').alignItems(HorizontalAlign.Start)
      }
      SketchCard() {
        Column({ space: 12 }) {
          SectionTitle({ text: '本地功能' })
          InfoRow({ label: '语义搜索模型', value: this.semanticStateLabel() })
          InfoRow({ label: '语音转写模型', value: this.transcriptionStateLabel() })
          Text('本地模型随应用提供，无需另外下载。').fontSize(13).fontColor(SketchColors.muted)
        }.width('100%').alignItems(HorizontalAlign.Start)
      }
    }.width('100%')
  }

  build() {
    Scroll() {
      Column({ space: 20 }) {
        if (this.diagnosticsOnly) {
          PageTitle({ title: '搜索索引', subtitle: '准备本地语义搜索，查看索引覆盖。' })
          ErrorBanner({ error: this.error, showReload: true, onReload: () => { this.load(); } })
          this.diagnosticsPanel()
        } else {
          PageTitle({ title: '我的小天地', subtitle: '你的记录安静地留在这台设备上。' })
          Text('日记默认只保存在这台设备上；只有你主动配置的模型服务会收到交给它处理的材料。')
            .fontSize(13).fontColor(SketchColors.text).padding(20)
            .width('100%').backgroundColor(SketchColors.soft).drawModifier(sketchOutline())
          ErrorBanner({ error: this.error, showReload: true, onReload: () => { this.load(); } })
          if (this.loading && this.info === null) { LoadingRow() }
          SketchCard() {
            Text(this.info === null ? '最近记录 · 尚未读取' : `最近记录 · ${(this.info as LibraryInfo).capture_count} 条`)
              .fontSize(17).fontColor(SketchColors.ink)
          }
          SketchCard() {
            Column() {
              SectionTitle({ text: '创作与整理' })
              this.entry('风格偏好', '篇幅、语气与叙述方式', 'styles')
              this.entry('模型服务', '设置智能整理服务', 'llm')
              this.entry('本地语音转写', '离线把录音转成文字', 'transcription')
              this.entry('整理时机', '安静地安排每天的整理', 'schedule')
              this.entry('任务中心', '查看处理进度', 'jobs')
            }.width('100%').alignItems(HorizontalAlign.Start)
          }
          if (this.showSettings) { this.settingsPanel() }
          SketchCard() {
            Column() {
              SectionTitle({ text: '资料与应用' })
              this.entry('搜索索引', '让记录更容易找到', 'diagnostics')
              this.entry('阅读导出', '按日期生成便于阅读的副本', 'export')
              this.entry('存储与回收站', '查看和恢复移除的记录', 'recycle')
            }.width('100%').alignItems(HorizontalAlign.Start)
          }
          Button(this.showDiagnostics ? '收起本地功能' : '本地功能')
            .attributeModifier(sketchButton).drawModifier(sketchOutline())
            .onClick(() => { this.showDiagnostics = !this.showDiagnostics; })
          if (this.showDiagnostics) { this.diagnosticsPanel() }
        }
      }
      .width('100%').constraintSize({ maxWidth: 1240 })
      .padding({ left: 16, right: 16, top: 12, bottom: 30 }).alignItems(HorizontalAlign.Start)
    }
    .width('100%').height('100%').backgroundColor(SketchColors.canvas)
  }
}

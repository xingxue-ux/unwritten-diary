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

  private configFile(): ConfigFileReport | null {
    const config: RuntimeConfig | null = this.runtimeConfig();
    return config === null ? null : config.file;
  }

  private semanticState(): string {
    const config: RuntimeConfig | null = this.runtimeConfig();
    return config === null ? 'unknown' : config.semantic.state;
  }

  private semanticStateLabel(): string {
    const state: string = this.semanticState();
    if (state === 'ready') {
      return '模型已加载';
    }
    if (state === 'configured') {
      return '已配置，待验证';
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

  private semanticReason(): string {
    const config: RuntimeConfig | null = this.runtimeConfig();
    if (config === null || config.semantic.reason === null) {
      return '—';
    }
    return config.semantic.reason;
  }

  private semanticModelDir(): string {
    const config: RuntimeConfig | null = this.runtimeConfig();
    if (config === null || config.semantic.modelDir === null) {
      return '—';
    }
    return config.semantic.modelDir;
  }

  private semanticOrt(): string {
    const config: RuntimeConfig | null = this.runtimeConfig();
    if (config === null || config.semantic.ortDylib === null) {
      return '—';
    }
    return config.semantic.ortDylib;
  }

  private semanticModelVersion(): string {
    const config: RuntimeConfig | null = this.runtimeConfig();
    if (config === null || config.semantic.modelVersion === null) {
      return '—';
    }
    return config.semantic.modelVersion;
  }

  private transcriptionState(): string {
    const config: RuntimeConfig | null = this.runtimeConfig();
    return config === null ? 'unknown' : config.transcription.state;
  }

  private transcriptionStateLabel(): string {
    const state: string = this.transcriptionState();
    if (state === 'ready') {
      return '文件齐全（待测试）';
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

  private transcriptionReason(): string {
    const config: RuntimeConfig | null = this.runtimeConfig();
    if (config === null || config.transcription.reason === null) {
      return '—';
    }
    return config.transcription.reason;
  }

  private transcriptionConfigSource(): string {
    const config: RuntimeConfig | null = this.runtimeConfig();
    if (config === null) {
      return '—';
    }
    return this.sourceLabel(config.transcription.configSource);
  }

  private transcriptionEngineLibSource(): string {
    const config: RuntimeConfig | null = this.runtimeConfig();
    if (config === null) {
      return '—';
    }
    return this.sourceLabel(config.transcription.engineLibSource);
  }

  private transcriptionModelDir(): string {
    const config: RuntimeConfig | null = this.runtimeConfig();
    if (config === null || config.transcription.modelDir === null) {
      return '—';
    }
    return config.transcription.modelDir;
  }

  private transcriptionEngineLib(): string {
    const config: RuntimeConfig | null = this.runtimeConfig();
    if (config === null || config.transcription.engineLib === null) {
      return '—';
    }
    return config.transcription.engineLib;
  }

  private sourceLabel(source: string): string {
    if (source === 'env') {
      return '环境变量';
    }
    if (source === 'file') {
      return 'core-config.json';
    }
    if (source === 'provider') {
      return '转写 provider';
    }
    if (source === 'bundled') {
      return '随包';
    }
    if (source === 'platform') {
      return '平台按库名';
    }
    if (source === 'absent') {
      return '未找到';
    }
    return source;
  }

  private configSourceLabel(): string {
    const config: RuntimeConfig | null = this.runtimeConfig();
    return config === null ? '—' : this.sourceLabel(config.semantic.configSource);
  }

  private configFileName(): string {
    const file: ConfigFileReport | null = this.configFile();
    if (file === null) {
      return '—';
    }
    if (file.path === null) {
      return '内存库（没有配置文件）';
    }
    return file.path;
  }

  private configFileState(): string {
    const file: ConfigFileReport | null = this.configFile();
    if (file === null) {
      return '—';
    }
    if (!file.present) {
      return file.error === null ? '没有配置文件（正常）' : `存在读取问题：${file.error}`;
    }
    if (file.error !== null) {
      return `已读到，但解析有问题：${file.error}`;
    }
    return '已读取';
  }

  private ignoredFields(): string {
    const file: ConfigFileReport | null = this.configFile();
    if (file === null || file.ignoredFields.length === 0) {
      return '—';
    }
    return file.ignoredFields.join('、');
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
    this.rebuildMessage = `已排队：${job.kind}（state=${jobStateLabel(job.state)}）；正在运行到期任务…`;
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
      this.rebuildMessage = `${this.rebuildMessage}\n到期任务运行失败。`;
      this.rebuildError = uiErrorOf(runResult);
    } else {
      const summary = runResult.data as JobRunSummary;
      this.rebuildMessage = `本轮：处理 ${summary.processed}，成功 ${summary.succeeded}，` +
        `失败 ${summary.failed}，重试 ${summary.retrying}，积压 ${summary.due_backlog}。` +
        (summary.due_backlog > 0 ? '还有积压，可以再跑一轮。' : '');
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
    return styleId === null ? '未设置（新建日记跟随界面默认）' : styleId;
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

  private nativeVersion(): string {
    try {
      return DiaryCore.version();
    } catch (e) {
      return '读取失败（核心库可能没有加载）';
    }
  }

  private nativeLibraryDir(): string {
    try {
      return DiaryCore.nativeLibraryDir();
    } catch (e) {
      return '读取失败';
    }
  }

  private openHandleCount(): string {
    try {
      return `${DiaryCore.openHandleCount()}`;
    } catch (e) {
      return '—';
    }
  }

  /** 任务中心入口的一行摘要（不做状态判断，只是把 `jobs.status` 的数字摆出来）。 */
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
          this.section('整理与默认风格（settings.get / update）')
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
              value: `${this.currentSettings().auto_organize_time}（本地时间；只是检查时机，不是完成承诺）`,
            })
            InfoRow({ label: '日记时区', value: this.currentSettings().diary_time_zone })
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
            Text('风格接口已就绪（styles.list / get / save），风格选择器界面还没做；这里只提供「清空」，不伪造选择器。' +
              '改开关不会碰这个字段（省略 = 不改，只有「清空」才发显式 null）。')
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
    Column({ space: 12 }) {
        SectionTitle({ text: '设备与诊断' })

        Column({ space: 4 }) {
          this.section('语义索引（indexes.status）')
          if (this.indexError.code.length > 0) {
            ErrorBanner({
              error: this.indexError,
              showReload: true,
              onReload: () => {
                this.load();
              },
            })
          } else if (this.indexStatus === null) {
            LoadingRow({ text: '读取索引状态…' })
          } else {
            Row({ space: 8 }) {
              StatePill({
                text: this.indexStatus.semantic_index_ready ? '语义索引就绪' : '语义索引未就绪',
                tone: this.indexStatus.semantic_index_ready ? 'ok' : 'warn',
              })
              Text(`已嵌入 ${this.indexStatus.embedded_chunks} / ${this.indexStatus.total_chunks} 块`)
                .fontSize(12)
                .fontColor(SketchColors.muted)
            }
            .width('100%')
            InfoRow({ label: '关键词覆盖', value: `${this.indexStatus.coverage}` })
            InfoRow({ label: '索引模型', value: this.indexStatus.model_version === null ? '—' : this.indexStatus.model_version })
            InfoRow({ label: '索引占用', value: formatBytes(this.indexStatus.index_bytes) })
            InfoRow({
              label: '片段',
              value: `已索引 ${this.indexStatus.indexed_segments} / ${this.indexStatus.total_segments}，待处理 ${this.indexStatus.pending_segments}`,
            })
            if (this.indexStatus.reasons.length > 0) {
              Column({ space: 4 }) {
                Text('核心给出的原因（原样）：')
                  .fontSize(12)
                  .fontColor(SketchColors.muted)
                  .width('100%')
                ForEach(this.indexStatus.reasons, (reason: string, index: number) => {
                  Text(`· ${reason}`)
                    .fontSize(12)
                    .fontColor(SketchColors.text)
                    .width('100%')
                }, (reason: string, index: number) => `${index}-${reason}`)
              }
              .width('100%')
              .padding(10)
              .backgroundColor(SketchColors.soft)
              .borderRadius(0)
            }
            Row({ space: 8 }) {
              Button(this.rebuilding ? '重建中…' : '一键重建语义索引')
                .attributeModifier(sketchButton).drawModifier(sketchOutline())
                .fontSize(13)
                .enabled(!this.rebuilding && this.canRebuildSemantic())
                .backgroundColor(SketchColors.button)
                .fontColor(SketchColors.ink)
                .onClick(() => {
                  this.rebuildSemantic();
                })
              if (this.hasBacklog) {
                Button('再跑一轮')
                  .attributeModifier(sketchButton).drawModifier(sketchOutline())
                  .fontSize(13)
                  .enabled(!this.rebuilding)
                  .backgroundColor(SketchColors.soft)
                  .fontColor(SketchColors.text)
                  .onClick(() => {
                    this.continueRunDue();
                  })
              }
            }
            .width('100%')
            if (!this.canRebuildSemantic()) {
              Text('模型未配置或配置无效，不能重建。先按上面的原因修复 core-config.json。')
                .fontSize(12)
                .fontColor(SketchColors.warning)
                .width('100%')
            }
            if (this.rebuildMessage.length > 0) {
              Text(this.rebuildMessage)
                .fontSize(12)
                .fontColor(SketchColors.muted)
                .width('100%')
            }
            if (this.rebuildError.code.length > 0) {
              ErrorBanner({ error: this.rebuildError, showReload: false, onReload: () => {
              } })
            }
          }
        }
        .width('100%')
        .padding(14)
        .backgroundColor(SketchColors.paper)
        .drawModifier(sketchOutline())
        .alignItems(HorizontalAlign.Start)

        // ---------------------------------------------------------- 诊断
        Column({ space: 4 }) {
          this.section('诊断')
          if (this.info !== null) {
            InfoRow({ label: 'API 版本', value: (this.info as LibraryInfo).api_version })
            InfoRow({ label: '数据 schema', value: `${(this.info as LibraryInfo).data_schema_version}` })
            InfoRow({ label: '构建版本', value: (this.info as LibraryInfo).build_version })
            InfoRow({ label: '记录数', value: `${(this.info as LibraryInfo).capture_count}` })
            InfoRow({ label: '资料库 ID', value: (this.info as LibraryInfo).library_id })
            InfoRow({ label: '事件序号', value: `${(this.info as LibraryInfo).last_event_sequence}` })
            InfoRow({ label: '恢复摘要', value: `待办任务 ${(this.info as LibraryInfo).recovery.pending_jobs}，未收尾录音 ${(this.info as LibraryInfo).recovery.open_recordings}` })
            InfoRow({ label: '能力数', value: `${(this.info as LibraryInfo).capabilities.length} 项（core.info.capabilities）` })
            Button(this.showCapabilities ? '收起能力清单' : '查看能力清单')
              .attributeModifier(sketchButton).drawModifier(sketchOutline())
              .fontSize(12)
              .backgroundColor(SketchColors.soft)
              .fontColor(SketchColors.text)
              .onClick(() => {
                this.showCapabilities = !this.showCapabilities;
              })
            if (this.showCapabilities) {
              Column({ space: 2 }) {
                ForEach((this.info as LibraryInfo).capabilities, (name: string, index: number) => {
                  Text(name)
                    .fontSize(11)
                    .fontColor(SketchColors.muted)
                    .width('100%')
                }, (name: string, index: number) => `${index}-${name}`)
              }
              .width('100%')
              .padding(8)
              .backgroundColor(SketchColors.soft)
              .borderRadius(0)
            }
            InfoRow({ label: '配置文件', value: this.configFileName() })
            InfoRow({ label: '配置状态', value: this.configFileState() })
            InfoRow({ label: '忽略字段', value: this.ignoredFields() })
          } else {
            Text('核心信息还没有读到。')
              .fontSize(12)
              .fontColor(SketchColors.muted)
              .width('100%')
          }
          InfoRow({ label: 'C ABI 版本', value: this.nativeVersion() })
          InfoRow({ label: '原生库目录', value: this.nativeLibraryDir() })
          InfoRow({ label: '未关闭句柄', value: this.openHandleCount() })
          Text('真未接的只有文件导入（imports.*）与插件（plugins.*）；风格（styles.*）、事件轮询（core.events）、' +
            '备份恢复（backups.*）、记录移动/删除（captures.moveDay/trash/restore）的 C ABI 都已就绪，' +
            '缺的是界面入口。本页不伪造这些入口。')
            .fontSize(12)
            .fontColor(SketchColors.muted)
            .width('100%')
            .padding({ top: 6 })
        }
        .width('100%')
        .padding(14)
        .backgroundColor(SketchColors.paper)
        .drawModifier(sketchOutline())
        .alignItems(HorizontalAlign.Start)

        Text('状态说明：语义 state=ready 只表示模型加载过；索引是否覆盖全部块看本页的「语义索引」。')
          .fontSize(11)
          .fontColor(SketchColors.muted)
          .width('100%')
          .padding({ bottom: 24 })
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
              this.entry('模型服务', '配置 AI、模型和连接测试', 'llm')
              this.entry('本地语音转写', '录音转文字的引擎与模型', 'transcription')
              this.entry('整理时机', '安静地安排每天的整理', 'schedule')
              this.entry('任务中心', '等待、运行与需要处理', 'jobs')
            }.width('100%').alignItems(HorizontalAlign.Start)
          }
          if (this.showSettings) { this.settingsPanel() }
          SketchCard() {
            Column() {
              SectionTitle({ text: '资料与应用' })
              this.entry('搜索索引', '状态与语义索引重建', 'diagnostics')
              this.entry('阅读导出', '按日期生成便于阅读的副本', 'export')
              this.entry('存储与回收站', '原件、缓存、索引的边界', 'recycle')
              Text('插件、完整备份与恢复、封面与加载动画仍在接入。')
                .fontSize(13).fontColor(SketchColors.muted).padding({ top: 16 })
            }.width('100%').alignItems(HorizontalAlign.Start)
          }
          Button(this.showDiagnostics ? '收起设备与诊断' : '设备与诊断')
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

// 设置：模型状态、语义索引状态 + 一键重建、转写配置入口、诊断信息。
//
// 状态如实（不拿「模型加载过」冒充「索引就绪」）：
// - `runtime_config.semantic.state` 只表示模型/配置的状态；
// - 语义索引是否覆盖全部块，看 `indexes.status.semantic_index_ready` 与 `reasons`；
// - `reasons[]` 是核心写的中文整句，**原样展示**，不改写成自己的文案；
// - 配置只在 `Core::open` 时读一次；当前没有写 `core-config.json` 的 C ABI，
//   所以「去配置」只能如实说是缺口（不伪造一个保存按钮）。

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
} from '../core/Models';
import { CoreGateway, CoreRequest, uiErrorOf } from '../ui/CoreGateway';
import { EMPTY_UI_ERROR, UiError } from '../ui/Errors';
import { AppState } from '../ui/AppState';
import {
  databasePath,
  formatBytes,
  formatDateTime,
  jobStateLabel,
  newOperationId,
} from '../ui/AppRuntime';
import { ErrorBanner, InfoRow, LoadingRow, PageTitle, SectionTitle, StatePill } from '../ui/Common';

@Component
export struct SettingsPage {
  pathStack: NavPathStack = new NavPathStack();
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
  @State hasBacklog: boolean = false;
  @StorageLink('ohosDataVersion') @Watch('onDataVersionChanged') dataVersion: number = 0;

  private dbPath: string = '';

  aboutToAppear(): void {
    const context = getContext(this) as common.UIAbilityContext;
    this.dbPath = databasePath(context);
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

  @Builder
  section(title: string) {
    SectionTitle({ text: title })
  }

  build() {
    Scroll() {
      Column({ space: 10 }) {
        Row({ space: 8 }) {
          Column() {
            PageTitle({ title: '设置', subtitle: '模型、索引与诊断；状态如实展示' })
          }
          .layoutWeight(1)
          .alignItems(HorizontalAlign.Start)
          Button('刷新')
            .fontSize(13)
            .height(34)
            .backgroundColor($r('sys.color.ohos_id_color_sub_background'))
            .fontColor($r('sys.color.ohos_id_color_text_primary'))
            .onClick(() => {
              this.load();
            })
        }
        .width('100%')
        .padding({ top: 12 })

        if (this.loading && this.info === null) {
          LoadingRow({ text: '读取设置与索引状态…' })
        }

        if (this.error.code.length > 0) {
          ErrorBanner({
            error: this.error,
            showReload: true,
            onReload: () => {
              this.load();
            },
          })
        }

        // ---------------------------------------------------------- 模型状态
        Column({ space: 4 }) {
          this.section('模型状态（core.info.runtime_config）')
          Row({ space: 8 }) {
            Text('语义')
              .fontSize(13)
              .fontColor($r('sys.color.ohos_id_color_text_secondary'))
              .width(92)
            StatePill({ text: this.semanticStateLabel(), tone: this.semanticStateTone() })
            Text(`${this.configSourceLabel()} · ${this.semanticModelVersion()}`)
              .fontSize(12)
              .fontColor($r('sys.color.ohos_id_color_text_tertiary'))
              .layoutWeight(1)
              .maxLines(2)
              .textOverflow({ overflow: TextOverflow.Ellipsis })
          }
          .width('100%')
          InfoRow({ label: '原因', value: this.semanticReason() })
          InfoRow({ label: '模型目录', value: this.semanticModelDir() })
          InfoRow({ label: 'ORT 库', value: this.semanticOrt() })
          Row({ space: 8 }) {
            Text('转写')
              .fontSize(13)
              .fontColor($r('sys.color.ohos_id_color_text_secondary'))
              .width(92)
            StatePill({ text: this.transcriptionStateLabel(), tone: this.transcriptionStateTone() })
            Text(`${this.transcriptionConfigSource()} · ${this.transcriptionModelDir()}`)
              .fontSize(12)
              .fontColor($r('sys.color.ohos_id_color_text_tertiary'))
              .layoutWeight(1)
              .maxLines(2)
              .textOverflow({ overflow: TextOverflow.Ellipsis })
          }
          .width('100%')
          InfoRow({ label: '原因', value: this.transcriptionReason() })
          InfoRow({ label: '引擎库', value: this.transcriptionEngineLib() })
          InfoRow({ label: '引擎来源', value: this.transcriptionEngineLibSource() })
          Button('打开转写引擎配置')
            .fontSize(13)
            .height(34)
            .backgroundColor($r('sys.color.ohos_id_color_sub_background'))
            .fontColor($r('sys.color.ohos_id_color_text_primary'))
            .onClick(() => {
              this.pathStack.pushPathByName('transcription', '');
            })
          if (this.semanticState() === 'absent' || this.semanticState() === 'invalid') {
            Text('语义模型要在 core-config.json 里配置；当前 C ABI 没有写配置文件的接口，' +
              '所以这里只能显示状态，不能伪造「一键配置」。')
              .fontSize(12)
              .fontColor($r('sys.color.ohos_id_color_warning'))
              .width('100%')
          }
        }
        .width('100%')
        .padding(14)
        .backgroundColor($r('sys.color.ohos_id_color_card_bg'))
        .borderRadius(14)
        .alignItems(HorizontalAlign.Start)

        // ---------------------------------------------------------- 语义索引
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
                .fontColor($r('sys.color.ohos_id_color_text_secondary'))
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
                  .fontColor($r('sys.color.ohos_id_color_text_secondary'))
                  .width('100%')
                ForEach(this.indexStatus.reasons, (reason: string, index: number) => {
                  Text(`· ${reason}`)
                    .fontSize(12)
                    .fontColor($r('sys.color.ohos_id_color_text_primary'))
                    .width('100%')
                }, (reason: string, index: number) => `${index}-${reason}`)
              }
              .width('100%')
              .padding(10)
              .backgroundColor($r('sys.color.ohos_id_color_sub_background'))
              .borderRadius(10)
            }
            Row({ space: 8 }) {
              Button(this.rebuilding ? '重建中…' : '一键重建语义索引')
                .fontSize(13)
                .height(36)
                .enabled(!this.rebuilding && this.canRebuildSemantic())
                .backgroundColor($r('sys.color.ohos_id_color_emphasize'))
                .fontColor($r('sys.color.ohos_id_color_text_primary_contrary'))
                .onClick(() => {
                  this.rebuildSemantic();
                })
              if (this.hasBacklog) {
                Button('再跑一轮')
                  .fontSize(13)
                  .height(36)
                  .enabled(!this.rebuilding)
                  .backgroundColor($r('sys.color.ohos_id_color_sub_background'))
                  .fontColor($r('sys.color.ohos_id_color_text_primary'))
                  .onClick(() => {
                    this.continueRunDue();
                  })
              }
            }
            .width('100%')
            if (!this.canRebuildSemantic()) {
              Text('模型未配置或配置无效，不能重建。先按上面的原因修复 core-config.json。')
                .fontSize(12)
                .fontColor($r('sys.color.ohos_id_color_warning'))
                .width('100%')
            }
            if (this.rebuildMessage.length > 0) {
              Text(this.rebuildMessage)
                .fontSize(12)
                .fontColor($r('sys.color.ohos_id_color_text_secondary'))
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
        .backgroundColor($r('sys.color.ohos_id_color_card_bg'))
        .borderRadius(14)
        .alignItems(HorizontalAlign.Start)

        // ---------------------------------------------------------- 任务概览
        Column({ space: 4 }) {
          this.section('任务（jobs.status）')
          if (this.jobStatus === null) {
            Text('还没有读到队列计数。')
              .fontSize(12)
              .fontColor($r('sys.color.ohos_id_color_text_secondary'))
              .width('100%')
          } else {
            InfoRow({
              label: '处理中',
              value: `${this.jobStatus.queued} 排队 / ${this.jobStatus.running} 运行`,
            })
            InfoRow({
              label: '等待',
              value: `${this.jobStatus.retry_wait} 自动重试 / ${this.jobStatus.waiting_dependency} 等前置`,
            })
            InfoRow({
              label: '需要处理',
              value: `${this.jobStatus.waiting_configuration} 等配置 / ${this.jobStatus.failed} 失败`,
            })
            InfoRow({ label: '完成', value: `${this.jobStatus.succeeded} 成功 / ${this.jobStatus.cancelled} 取消` })
            InfoRow({ label: '到期积压', value: `${this.jobStatus.due_backlog}` })
            InfoRow({
              label: '下次唤醒',
              value: this.jobStatus.next_wakeup === null ? '—' : formatDateTime(this.jobStatus.next_wakeup),
            })
          }
          Button('打开任务中心')
            .fontSize(13)
            .height(34)
            .backgroundColor($r('sys.color.ohos_id_color_sub_background'))
            .fontColor($r('sys.color.ohos_id_color_text_primary'))
            .onClick(() => {
              this.pathStack.pushPathByName('jobs', '');
            })
        }
        .width('100%')
        .padding(14)
        .backgroundColor($r('sys.color.ohos_id_color_card_bg'))
        .borderRadius(14)
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
              .fontSize(12)
              .height(30)
              .backgroundColor($r('sys.color.ohos_id_color_sub_background'))
              .fontColor($r('sys.color.ohos_id_color_text_primary'))
              .onClick(() => {
                this.showCapabilities = !this.showCapabilities;
              })
            if (this.showCapabilities) {
              Column({ space: 2 }) {
                ForEach((this.info as LibraryInfo).capabilities, (name: string, index: number) => {
                  Text(name)
                    .fontSize(11)
                    .fontColor($r('sys.color.ohos_id_color_text_secondary'))
                    .width('100%')
                }, (name: string, index: number) => `${index}-${name}`)
              }
              .width('100%')
              .padding(8)
              .backgroundColor($r('sys.color.ohos_id_color_sub_background'))
              .borderRadius(8)
            }
            InfoRow({ label: '配置文件', value: this.configFileName() })
            InfoRow({ label: '配置状态', value: this.configFileState() })
            InfoRow({ label: '忽略字段', value: this.ignoredFields() })
          } else {
            Text('核心信息还没有读到。')
              .fontSize(12)
              .fontColor($r('sys.color.ohos_id_color_text_secondary'))
              .width('100%')
          }
          InfoRow({ label: 'C ABI 版本', value: this.nativeVersion() })
          InfoRow({ label: '原生库目录', value: this.nativeLibraryDir() })
          InfoRow({ label: '未关闭句柄', value: this.openHandleCount() })
          Text('未接的能力：文件导入（imports.*）、styles.*、通知/事件（core.events）、' +
            '备份恢复（backups.*）、插件（plugins.*）、记录移动/删除（captures.moveDay/trash/restore）。' +
            '本页不伪造这些入口。')
            .fontSize(12)
            .fontColor($r('sys.color.ohos_id_color_text_tertiary'))
            .width('100%')
            .padding({ top: 6 })
        }
        .width('100%')
        .padding(14)
        .backgroundColor($r('sys.color.ohos_id_color_card_bg'))
        .borderRadius(14)
        .alignItems(HorizontalAlign.Start)

        Text('状态说明：语义 state=ready 只表示模型加载过；索引是否覆盖全部块看本页的「语义索引」。')
          .fontSize(11)
          .fontColor($r('sys.color.ohos_id_color_text_tertiary'))
          .width('100%')
          .padding({ bottom: 24 })
      }
      .width('100%')
      .padding({ left: 16, right: 16 })
    }
    .width('100%')
    .height('100%')
    .backgroundColor($r('sys.color.ohos_id_color_background'))
  }
}

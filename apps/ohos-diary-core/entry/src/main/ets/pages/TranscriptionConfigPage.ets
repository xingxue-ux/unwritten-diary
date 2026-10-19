// 转写引擎配置入口：本机 sherpa_onnx provider 的保存与连接测试。
//
// 事实（文档 §5.5）：
// - `adapter_id = sherpa_onnx`；`model` 是**模型目录**，不是远端模型名；
// - `endpoint` 必须为 null；
// - `test_status` 由核心维护，保存后回到 `untested`，调用方写 `ok` 会被忽略；
// - `providers.test` 会**真的加载模型并转写静音**（几秒），放在 TaskPool，不在 UI 线程。
//
// 当前没有「模型下载/导入」的产品接口：ModelDownload 需要调用方给 URL + bytes + sha256，
// 而下载源不在仓库里。本页不伪造下载按钮，只显示模型目录状态。

import { common } from '@kit.AbilityKit';
import {
  JobRunSummary,
  LibraryInfo,
  ProviderConfig,
  ProviderTestReport,
  RunDueParams,
  SaveProviderParams,
  TestProviderParams,
} from '../core/Models';
import { CoreGateway, CoreRequest, uiErrorOf } from '../ui/CoreGateway';
import { EMPTY_UI_ERROR, UiError } from '../ui/Errors';
import { AppState } from '../ui/AppState';
import { databasePath, toast } from '../ui/AppRuntime';
import { ErrorBanner, InfoRow, LoadingRow, SectionTitle, StatePill } from '../ui/Common';

@Component
export struct TranscriptionConfigPage {
  pathStack: NavPathStack = new NavPathStack();
  @State info: LibraryInfo | null = null;
  @State providers: ProviderConfig[] = [];
  @State modelDir: string = '';
  @State loading: boolean = true;
  @State error: UiError = EMPTY_UI_ERROR;
  @State saveBusy: boolean = false;
  @State saveError: UiError = EMPTY_UI_ERROR;
  @State testBusy: boolean = false;
  @State testError: UiError = EMPTY_UI_ERROR;
  @State testReport: ProviderTestReport | null = null;
  @State runBusy: boolean = false;
  @State runMessage: string = '';

  private dbPath: string = '';
  private filesDir: string = '';

  aboutToAppear(): void {
    const context = getContext(this) as common.UIAbilityContext;
    this.dbPath = databasePath(context);
    this.filesDir = context.filesDir;
    this.load();
  }

  private async load(): Promise<void> {
    this.loading = true;
    this.error = EMPTY_UI_ERROR;
    const requests: CoreRequest[] = [
      { method: 'core.info', params: null },
      { method: 'providers.list', params: null },
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
        this.providers = results[1].data as ProviderConfig[];
      } else if (this.error.code.length === 0) {
        this.error = uiErrorOf(results[1]);
      }
    }
    if (this.modelDir.length === 0) {
      const configured: string = this.configuredModelDir();
      this.modelDir = configured.length > 0 ? configured : `${this.filesDir}/asr/sense-voice`;
    }
  }

  private configuredModelDir(): string {
    if (this.info === null) {
      return '';
    }
    const config = this.info.runtime_config;
    if (config === undefined || config.transcription.modelDir === null) {
      return '';
    }
    return config.transcription.modelDir;
  }

  private transcriptionState(): string {
    if (this.info === null) {
      return 'unknown';
    }
    const config = this.info.runtime_config;
    if (config === undefined) {
      return 'unknown';
    }
    return config.transcription.state;
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
    if (this.info === null) {
      return '—';
    }
    const config = this.info.runtime_config;
    if (config === undefined || config.transcription.reason === null) {
      return '—';
    }
    return config.transcription.reason;
  }

  private transcriptionConfigSource(): string {
    if (this.info === null) {
      return '—';
    }
    const config = this.info.runtime_config;
    if (config === undefined) {
      return '—';
    }
    const source: string = config.transcription.configSource;
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
    if (source === 'absent') {
      return '未找到';
    }
    return source;
  }

  private transcriptionEngineLib(): string {
    if (this.info === null) {
      return '—';
    }
    const config = this.info.runtime_config;
    if (config === undefined || config.transcription.engineLib === null) {
      return '—';
    }
    return config.transcription.engineLib;
  }

  private transcriptionProviderId(): string {
    if (this.info === null) {
      return '—';
    }
    const config = this.info.runtime_config;
    if (config === undefined || config.transcription.providerId === null) {
      return '—';
    }
    return config.transcription.providerId;
  }

  private providerId(): string {
    for (let i = 0; i < this.providers.length; i++) {
      if (this.providers[i].adapter_id === 'sherpa_onnx') {
        return this.providers[i].id;
      }
    }
    return 'local_asr_ohos';
  }

  private async save(): Promise<void> {
    if (this.saveBusy) {
      return;
    }
    if (this.modelDir.trim().length === 0) {
      this.saveError = {
        code: 'invalid_argument',
        title: '模型目录为空',
        message: '本机引擎的 model 是模型目录，不能为空。',
        hint: '填 SenseVoice 模型所在目录（例如 <filesDir>/asr/sense-voice）。',
        retryable: false,
        native: false,
      };
      return;
    }
    this.saveBusy = true;
    this.saveError = EMPTY_UI_ERROR;
    const config: ProviderConfig = {
      id: this.providerId(),
      label: '本机转写（OHOS 沙箱模型）',
      adapter_id: 'sherpa_onnx',
      endpoint: null,
      model: this.modelDir.trim(),
      capabilities: ['transcribe_audio'],
      credential_ref: null,
      enabled: true,
      test_status: 'untested',
    };
    const params: SaveProviderParams = {
      config: config,
      secret: null,
    };
    const result = await CoreGateway.call(this.dbPath, 'providers.save', params);
    this.saveBusy = false;
    if (!result.ok) {
      this.saveError = uiErrorOf(result);
      return;
    }
    toast('已保存转写配置；test_status 已回到 untested（要真加载一次才算验证）');
    await this.load();
  }

  private async test(): Promise<void> {
    if (this.testBusy) {
      return;
    }
    this.testBusy = true;
    this.testError = EMPTY_UI_ERROR;
    this.testReport = null;
    const params: TestProviderParams = {
      provider_id: this.providerId(),
      scope: ['transcribe_audio'],
    };
    const result = await CoreGateway.call(this.dbPath, 'providers.test', params);
    this.testBusy = false;
    if (!result.ok) {
      this.testError = uiErrorOf(result);
      return;
    }
    this.testReport = result.data as ProviderTestReport;
    await this.load();
  }

  private async runDue(): Promise<void> {
    if (this.runBusy) {
      return;
    }
    this.runBusy = true;
    this.runMessage = '正在运行到期任务（会真的加载模型，最长 30 秒）…';
    const params: RunDueParams = {
      budget_ms: 30000,
      trigger: 'manual',
    };
    const result = await CoreGateway.call(this.dbPath, 'jobs.runDue', params);
    this.runBusy = false;
    if (!result.ok) {
      const mapped: UiError = uiErrorOf(result);
      this.runMessage = `运行失败：${mapped.title}（${mapped.message}）`;
      return;
    }
    const summary: JobRunSummary = result.data as JobRunSummary;
    this.runMessage = `本轮：处理 ${summary.processed}，成功 ${summary.succeeded}，失败 ${summary.failed}，` +
      `积压 ${summary.due_backlog}`;
    AppState.bumpDataVersion();
  }

  @Builder
  providerRow(provider: ProviderConfig) {
    Column({ space: 4 }) {
      Row({ space: 8 }) {
        Text(provider.label.length > 0 ? provider.label : provider.id)
          .fontSize(14)
          .fontWeight(FontWeight.Medium)
          .fontColor($r('sys.color.ohos_id_color_text_primary'))
        StatePill({
          text: provider.test_status,
          tone: provider.test_status === 'ok' ? 'ok' : (provider.test_status === 'failed' ? 'alert' : 'muted'),
        })
        Blank()
        Text(provider.id)
          .fontSize(10)
          .fontColor($r('sys.color.ohos_id_color_text_tertiary'))
      }
      .width('100%')
      InfoRow({ label: 'adapter', value: provider.adapter_id })
      InfoRow({ label: '模型目录', value: provider.model === null ? '—（留空 = 随包/环境变量默认）' : provider.model })
      InfoRow({ label: 'endpoint', value: provider.endpoint === null ? '—' : provider.endpoint })
      InfoRow({ label: '能力', value: provider.capabilities.join('、') })
    }
    .width('100%')
    .padding(12)
    .backgroundColor($r('sys.color.ohos_id_color_sub_background'))
    .borderRadius(10)
    .alignItems(HorizontalAlign.Start)
  }

  build() {
    NavDestination() {
      Scroll() {
        Column({ space: 10 }) {
          if (this.loading) {
            LoadingRow({ text: '读取转写配置…' })
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

          Column({ space: 4 }) {
            SectionTitle({ text: '当前状态（core.info.runtime_config.transcription）' })
            Row({ space: 8 }) {
              StatePill({ text: this.transcriptionStateLabel(), tone: this.transcriptionStateTone() })
              Text(this.transcriptionProviderId())
                .fontSize(12)
                .fontColor($r('sys.color.ohos_id_color_text_secondary'))
            }
            .width('100%')
            InfoRow({ label: '原因', value: this.transcriptionReason() })
            InfoRow({ label: '模型来源', value: this.transcriptionConfigSource() })
            InfoRow({ label: '引擎库', value: this.transcriptionEngineLib() })
            Text('state=configured/ready 只说明路径/文件解析结果；真正能不能转写，要跑一次「连接测试」。')
              .fontSize(12)
              .fontColor($r('sys.color.ohos_id_color_text_secondary'))
              .width('100%')
          }
          .width('100%')
          .padding(14)
          .backgroundColor($r('sys.color.ohos_id_color_card_bg'))
          .borderRadius(14)
          .alignItems(HorizontalAlign.Start)

          Column({ space: 8 }) {
            SectionTitle({ text: '本机转写（sherpa_onnx）' })
            Text('模型目录（model 是目录，不是文件名）')
              .fontSize(12)
              .fontColor($r('sys.color.ohos_id_color_text_secondary'))
              .width('100%')
            TextInput({ text: this.modelDir, placeholder: '<filesDir>/asr/sense-voice' })
              .height(42)
              .fontSize(13)
              .backgroundColor($r('sys.color.ohos_id_color_sub_background'))
              .onChange((value: string) => {
                this.modelDir = value;
              })
            Text('SenseVoice 模型不在 HAP 里，需要另外下载/导入到应用沙箱；当前没有产品下载源，' +
              '本页不伪造「一键下载」。')
              .fontSize(11)
              .fontColor($r('sys.color.ohos_id_color_text_tertiary'))
              .width('100%')
            Row({ space: 8 }) {
              Button(this.saveBusy ? '保存中…' : '保存配置')
                .fontSize(13)
                .height(36)
                .enabled(!this.saveBusy)
                .backgroundColor($r('sys.color.ohos_id_color_emphasize'))
                .fontColor($r('sys.color.ohos_id_color_text_primary_contrary'))
                .onClick(() => {
                  this.save();
                })
              Button(this.testBusy ? '测试中…' : '连接测试（真加载模型）')
                .fontSize(13)
                .height(36)
                .enabled(!this.testBusy)
                .backgroundColor($r('sys.color.ohos_id_color_sub_background'))
                .fontColor($r('sys.color.ohos_id_color_text_primary'))
                .onClick(() => {
                  this.test();
                })
            }
            .width('100%')
            if (this.saveError.code.length > 0) {
              ErrorBanner({ error: this.saveError, showReload: false, onReload: () => {
              } })
            }
            if (this.testError.code.length > 0) {
              ErrorBanner({ error: this.testError, showReload: false, onReload: () => {
              } })
            }
            if (this.testReport !== null) {
              Column({ space: 4 }) {
                SectionTitle({ text: '测试结果（providers.test）' })
                InfoRow({ label: 'provider', value: (this.testReport as ProviderTestReport).provider_id })
                InfoRow({
                  label: '真实调用',
                  value: (this.testReport as ProviderTestReport).actual_call_made ? '是（真的跑了一次推理）' : '否（能力/请求没有发出）',
                })
                InfoRow({
                  label: '说明',
                  value: (this.testReport as ProviderTestReport).message === null
                    ? '—'
                    : (this.testReport as ProviderTestReport).message as string,
                })
                Text(JSON.stringify((this.testReport as ProviderTestReport).results))
                  .fontSize(11)
                  .fontColor($r('sys.color.ohos_id_color_text_secondary'))
                  .width('100%')
              }
              .width('100%')
              .padding(10)
              .backgroundColor($r('sys.color.ohos_id_color_sub_background'))
              .borderRadius(10)
              .alignItems(HorizontalAlign.Start)
            }
          }
          .width('100%')
          .padding(14)
          .backgroundColor($r('sys.color.ohos_id_color_card_bg'))
          .borderRadius(14)
          .alignItems(HorizontalAlign.Start)

          Column({ space: 6 }) {
            SectionTitle({ text: '已保存的 provider（providers.list）' })
            if (this.providers.length === 0) {
              Text('没有配置行。注意：providers.list 不包含内建引擎行，没有配置行不等于不能转写' +
                '（可能走随包/环境变量默认）。')
                .fontSize(12)
                .fontColor($r('sys.color.ohos_id_color_text_secondary'))
                .width('100%')
            } else {
              ForEach(this.providers, (provider: ProviderConfig) => {
                this.providerRow(provider)
              }, (provider: ProviderConfig) => `${provider.id}-${provider.test_status}`)
            }
            Button(this.runBusy ? '运行中…' : '立即运行待办任务')
              .fontSize(13)
              .height(36)
              .enabled(!this.runBusy)
              .backgroundColor($r('sys.color.ohos_id_color_sub_background'))
              .fontColor($r('sys.color.ohos_id_color_text_primary'))
              .onClick(() => {
                this.runDue();
              })
            if (this.runMessage.length > 0) {
              Text(this.runMessage)
                .fontSize(12)
                .fontColor($r('sys.color.ohos_id_color_text_secondary'))
                .width('100%')
            }
          }
          .width('100%')
          .padding(14)
          .backgroundColor($r('sys.color.ohos_id_color_card_bg'))
          .borderRadius(14)
          .alignItems(HorizontalAlign.Start)

          Text('说明：保存后核心会把 test_status 置回 untested；「连接测试」会真加载模型并转写 0.05 秒静音，' +
            '耗时几秒，放在 TaskPool 里跑。')
            .fontSize(11)
            .fontColor($r('sys.color.ohos_id_color_text_tertiary'))
            .width('100%')
            .padding({ bottom: 24 })
        }
        .width('100%')
        .padding({ left: 16, right: 16, top: 10 })
      }
      .width('100%')
      .height('100%')
      // 同 CaptureDetailPage：Scroll 的 align 默认 Center，内容不足一屏会垂直居中。
      .align(Alignment.Top)
      .backgroundColor($r('sys.color.ohos_id_color_background'))
    }
    .title('转写引擎')
  }
}

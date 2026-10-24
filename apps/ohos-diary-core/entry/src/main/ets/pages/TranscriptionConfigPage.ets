import { SketchColors, SketchFonts, SketchType, SketchCard, sketchButton, sketchInput, sketchArea, sketchOutline } from '../ui/Sketch';
// 转写引擎配置入口：本机 sherpa_onnx provider 的保存与连接测试。
//
// 事实（文档 §5.5）：
// - `adapter_id = sherpa_onnx`；`model` 是**模型目录**，不是远端模型名；
// - `endpoint` 必须为 null；
// - `test_status` 由核心维护，保存后回到 `untested`，调用方写 `ok` 会被忽略；
// - `providers.test` 会**真的加载模型并转写静音**（几秒），放在 TaskPool，不在 UI 线程。
//
// 产品 HAP 随包带 SenseVoice，首次启动部署并校验；仍允许保留/指定自己的模型目录。

import { common } from '@kit.AbilityKit';
import { appFilesDir } from '../session/AppDirectory';
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
import { runSummary, testStatusLabel } from '../ui/UserText';
import { databasePath, toast } from '../ui/AppRuntime';
import { ErrorBanner, InfoRow, LoadingRow, SectionTitle, StatePill } from '../ui/Common';

@Component
export struct TranscriptionConfigPage {
  pathStack: NavPathStack = new NavPathStack();
  @State info: LibraryInfo | null = null;
  @State providers: ProviderConfig[] = [];
  /** provider 列表是否读成功过。false + error 时不能说「没有配置行」。 */
  @State providersLoaded: boolean = false;
  @State modelDir: string = '';
  @State editingModel: boolean = false;
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
    this.filesDir = appFilesDir(context);
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
        this.providersLoaded = true;
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
    for (const provider of this.providers) {
      if (provider.adapter_id === 'sherpa_onnx' && provider.enabled) {
        return provider.test_status === 'ok' ? '可以使用' : testStatusLabel(provider.test_status);
      }
    }
    return this.transcriptionState() === 'ready' ? '模型已准备' : '需要配置';
  }

  private transcriptionStateTone(): string {
    return this.transcriptionStateLabel() === '可以使用' ? 'ok' : 'warn';
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
        message: '请填写模型存放位置。',
        hint: '',
        retryable: false,
        native: false,
      };
      return;
    }
    this.saveBusy = true;
    this.saveError = EMPTY_UI_ERROR;
    const config: ProviderConfig = {
      id: this.providerId(),
      label: '本地语音转写',
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
    this.editingModel = false;
    toast('已保存，请检查转写功能。');
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
    this.runMessage = '正在处理待办任务…';
    const params: RunDueParams = {
      budget_ms: 30000,
      trigger: 'manual',
    };
    const result = await CoreGateway.call(this.dbPath, 'jobs.runDue', params);
    this.runBusy = false;
    if (!result.ok) {
      const mapped: UiError = uiErrorOf(result);
      this.runMessage = mapped.title;
      return;
    }
    const summary: JobRunSummary = result.data as JobRunSummary;
    this.runMessage = runSummary(summary);
    AppState.bumpDataVersion();
  }

  build() {
    NavDestination() {
      Scroll() {
        Column({ space: 20 }) {
          Text('把说过的话留下来').fontFamily(SketchFonts.display).fontSize(28).fontColor(SketchColors.ink)
          Text('录音在本机转成文字，无需联网。').fontSize(14).fontColor(SketchColors.muted)
          if (this.loading) { LoadingRow({ text: '正在读取转写设置…' }) }
          ErrorBanner({ error: this.error, showReload: true, onReload: () => { this.load(); } })
          Column({ space: 14 }) {
            SectionTitle({ text: '本地语音转写' })
            StatePill({ text: this.transcriptionStateLabel(), tone: this.transcriptionStateTone() })
            Text(this.modelDir === `${this.filesDir}/bundled-models/v1/asr/sense-voice-int8`
              ? '使用应用自带模型，无需下载。' : '使用自选模型。').fontSize(14).fontColor(SketchColors.text)
            Button(this.testBusy ? '检查中…' : '检查转写功能').attributeModifier(sketchButton).drawModifier(sketchOutline())
              .enabled(!this.testBusy && !this.saveBusy && !this.loading).onClick(() => { this.test(); })
            ErrorBanner({ error: this.testError })
            if (this.testReport !== null) {
              Text(this.transcriptionStateLabel() === '可以使用' ? '检查通过，可以开始录音。' : '检查未通过，请检查模型设置。')
                .fontSize(14).fontColor(SketchColors.text)
            }
            Button(this.editingModel ? '收起模型设置' : '更换模型').attributeModifier(sketchButton).drawModifier(sketchOutline())
              .enabled(!this.saveBusy && !this.testBusy).onClick(() => { this.editingModel = !this.editingModel; })
            if (this.editingModel) {
              Text('模型存放位置').fontSize(13).fontColor(SketchColors.muted)
              TextInput({ text: this.modelDir, placeholder: '填写模型存放位置' }).attributeModifier(sketchInput)
                .enabled(!this.saveBusy).onChange((value: string) => { this.modelDir = value; })
              Button(this.saveBusy ? '保存中…' : '保存').attributeModifier(sketchButton).drawModifier(sketchOutline())
                .enabled(!this.saveBusy).onClick(() => { this.save(); })
            }
            ErrorBanner({ error: this.saveError })
          }.width('100%').padding(16).alignItems(HorizontalAlign.Start)
            .backgroundColor(SketchColors.paper).drawModifier(sketchOutline())
          Button(this.runBusy ? '处理中…' : '处理待办任务').attributeModifier(sketchButton).drawModifier(sketchOutline())
            .enabled(!this.runBusy).onClick(() => { this.runDue(); })
          if (this.runMessage.length > 0) { Text(this.runMessage).fontSize(14).fontColor(SketchColors.text) }
        }.width('100%').padding(20).alignItems(HorizontalAlign.Start)
      }.width('100%').height('100%').align(Alignment.Top).backgroundColor(SketchColors.canvas)
    }.title('语音转写', { backgroundColor: SketchColors.canvas })
  }
}

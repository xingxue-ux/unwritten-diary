import { common } from '@kit.AbilityKit';
import { ProviderConfig, TestProviderParams } from '../core/Models';
import { CoreGateway, uiErrorOf } from '../ui/CoreGateway';
import { EMPTY_UI_ERROR, UiError } from '../ui/Errors';
import { databasePath } from '../ui/AppRuntime';
import { ErrorBanner, StatePill, LoadingRow } from '../ui/Common';
import { SketchColors, SketchFonts, sketchButton, sketchOutline } from '../ui/Sketch';

@Component
export struct LlmConfigPage {
  pathStack: NavPathStack = new NavPathStack();
  @State ready: boolean = false;
  @State verified: boolean = false;
  @State loading: boolean = true;
  @State error: UiError = EMPTY_UI_ERROR;

  aboutToAppear(): void { this.load(); }

  private async load(): Promise<void> {
    this.loading = true;
    this.error = EMPTY_UI_ERROR;
    const result = await CoreGateway.call(databasePath(getContext(this) as common.UIAbilityContext),
      'providers.list', null);
    this.loading = false;
    if (!result.ok) {
      this.ready = false;
      this.verified = false;
      this.error = uiErrorOf(result);
      return;
    }
    const provider = (result.data as ProviderConfig[]).find((p: ProviderConfig) => p.id === 'builtin_local_llm');
    this.ready = provider !== undefined && provider.adapter_id === 'llama_cpp' && provider.enabled &&
      provider.capabilities.includes('generate_text') && provider.model !== null && provider.model.trim().length > 0;
    this.verified = provider !== undefined && this.ready && provider.test_status === 'ok';
  }

  private async check(): Promise<void> {
    if (this.loading || !this.ready) { return; }
    this.loading = true;
    this.verified = false;
    this.error = EMPTY_UI_ERROR;
    const params: TestProviderParams = { provider_id: 'builtin_local_llm', scope: ['generate_text'] };
    const result = await CoreGateway.call(databasePath(getContext(this) as common.UIAbilityContext),
      'providers.test', params);
    if (!result.ok) { this.loading = false; this.error = uiErrorOf(result); return; }
    await this.load();
  }

  build() {
    NavDestination() {
      Column({ space: 16 }) {
        Text('本机整理').fontFamily(SketchFonts.display).fontSize(24).fontColor(SketchColors.ink)
        Text('整理模型已随应用安装，无需配置账号或密钥。')
          .fontSize(16).fontColor(SketchColors.text)
        Text('录音转写、搜索和日记整理均在本机完成。')
          .fontSize(14).fontColor(SketchColors.muted)
        if (this.loading) { LoadingRow({ text: '正在检查本机模型…' }) }
        else { StatePill({ text: this.verified ? '已检查可用' : this.ready ? '模型已就位' : '尚未准备好', tone: this.ready ? 'ok' : 'warn' }) }
        Text('整理后的日记会标明“人工智能生成”，原始记录会保留。')
          .fontSize(14).fontColor(SketchColors.muted)
        if (this.error.code.length > 0) { ErrorBanner({ error: this.error, showReload: true, onReload: () => { this.load(); } }) }
        Button('检查本机整理').attributeModifier(sketchButton).drawModifier(sketchOutline())
          .enabled(!this.loading && this.ready).onClick(() => { this.check(); })
      }.width('100%').padding(24).alignItems(HorizontalAlign.Start)
    }.title('本机整理', { backgroundColor: SketchColors.canvas })
  }
}

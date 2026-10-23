import { common } from '@kit.AbilityKit';
import { ProviderConfig, ProviderPreset, ProviderModelInfo, ProviderIdParams,
  SaveProviderParams, TestProviderParams, ProviderTestReport } from '../core/Models';
import { CoreGateway, uiErrorOf } from '../ui/CoreGateway';
import { EMPTY_UI_ERROR, UiError } from '../ui/Errors';
import { databasePath, confirmAction } from '../ui/AppRuntime';
import { AppState } from '../ui/AppState';
import { ErrorBanner, SectionTitle, StatePill, LoadingRow } from '../ui/Common';
import { SketchColors, sketchInput, sketchButton, sketchOutline } from '../ui/Sketch';

/** Remote LLM configuration. Opening/editing/saving never probes the network. */
@Component
export struct LlmConfigPage {
  pathStack: NavPathStack = new NavPathStack();
  @State providers: ProviderConfig[] = [];
  @State presets: ProviderPreset[] = [];
  @State models: ProviderModelInfo[] = [];
  @State selected: ProviderConfig | null = null;
  @State label: string = '';
  @State endpoint: string = '';
  @State model: string = '';
  @State secret: string = '';
  @State dirty: boolean = false;
  @State loading: boolean = false;
  @State busy: boolean = false;
  @State loaded: boolean = false;
  @State uncertain: boolean = false;
  @State error: UiError = EMPTY_UI_ERROR;
  @State message: string = '';
  private dbPath: string = '';
  private active: boolean = true;

  aboutToAppear(): void {
    this.active = true;
    this.dbPath = databasePath(getContext(this) as common.UIAbilityContext);
    this.load();
  }

  aboutToDisappear(): void {
    this.active = false;
    this.secret = '';
  }

  private async load(): Promise<void> {
    this.loading = true;
    const result = await CoreGateway.call(this.dbPath, 'providers.list', null);
    if (!this.active) { return; }
    this.loading = false;
    if (!result.ok) { this.error = uiErrorOf(result); this.uncertain = true; return; }
    this.providers = (result.data as ProviderConfig[]).filter((p: ProviderConfig) =>
      p.adapter_id === 'openai_compatible' && p.capabilities.includes('generate_text'));
    if (this.selected !== null && !this.dirty) {
      const id: string = (this.selected as ProviderConfig).id;
      let found: boolean = false;
      for (let i = 0; i < this.providers.length; i++) {
        if (this.providers[i].id === id) { this.adopt(this.providers[i]); found = true; break; }
      }
      if (!found) { this.selected = null; this.label = ''; this.endpoint = ''; this.model = ''; this.secret = ''; }
    }
    this.uncertain = false;
    this.error = EMPTY_UI_ERROR;
    this.loaded = true;
    const presets = await CoreGateway.call(this.dbPath, 'providers.presets', null);
    if (!this.active) { return; }
    if (presets.ok) {
      this.presets = (presets.data as ProviderPreset[]).filter((p: ProviderPreset) =>
        p.adapter_id === 'openai_compatible' && p.capabilities.includes('generate_text'));
    } else { this.error = uiErrorOf(presets); }
  }

  private adopt(provider: ProviderConfig): void {
    this.selected = provider;
    this.label = provider.label;
    this.endpoint = provider.endpoint === null ? '' : provider.endpoint;
    this.model = provider.model === null ? '' : provider.model;
    this.secret = '';
    this.models = [];
    this.dirty = false;
  }

  private async mayDiscard(): Promise<boolean> {
    return !this.dirty || await confirmAction('放弃未保存的配置？', '切换后，本次填写的内容和密钥输入会清空。', '放弃修改');
  }

  private async edit(provider: ProviderConfig): Promise<void> {
    if (this.busy || this.loading || !await this.mayDiscard() || !this.active) { return; }
    this.adopt(provider);
    this.error = EMPTY_UI_ERROR;
    this.message = '';
  }

  private async newProvider(preset: ProviderPreset | null): Promise<void> {
    if (this.busy || this.loading || !await this.mayDiscard() || !this.active) { return; }
    this.selected = null;
    this.label = preset === null ? '' : preset.label;
    this.endpoint = preset === null ? '' : preset.endpoint;
    this.model = preset === null ? '' : preset.model;
    this.secret = '';
    this.models = [];
    this.dirty = preset !== null;
    this.error = EMPTY_UI_ERROR;
    this.message = preset === null ? '' : '已填入预设，请核对服务地址和模型，再填写你自己的 API Key。';
  }

  private validation(message: string): void {
    this.error = { code: 'invalid_argument', title: '请检查填写内容', message: message,
      hint: '', retryable: false, native: false };
  }

  private async save(): Promise<void> {
    if (this.busy || this.loading || !this.loaded) { return; }
    if (this.selected !== null && !this.dirty) { return; }
    const endpoint: string = this.endpoint.trim().replace(/\/+$/, '');
    if (this.label.trim().length === 0 || endpoint.length === 0) {
      this.validation('服务名称和 API 根地址不能为空。'); return;
    }
    if (!/^https?:\/\/[^\s/?#]+(?:\/[^\s?#]*)?$/i.test(endpoint) ||
      endpoint.includes('@') || /\/(chat\/completions|models|responses)$/i.test(endpoint)) {
      this.validation('请填写 API 根地址，不带账号密码、查询参数或 /chat/completions、/models、/responses。'); return;
    }
    if (!/^https:\/\//i.test(endpoint) &&
      !/^http:\/\/(localhost|127(?:\.\d{1,3}){3}|\[::1\])(?::\d+)?(?:\/|$)/i.test(endpoint)) {
      this.validation('远程服务请使用 HTTPS；HTTP 仅允许本机回环地址。'); return;
    }
    this.busy = true;
    this.error = EMPTY_UI_ERROR;
    // A functional edit is saved disabled until explicitly tested and enabled.
    const prior: ProviderConfig | null = this.selected;
    const config: ProviderConfig = {
      id: prior === null ? '' : prior.id, label: this.label.trim(),
      adapter_id: 'openai_compatible', endpoint: endpoint,
      model: this.model.trim().length === 0 ? null : this.model.trim(),
      capabilities: prior === null ? ['generate_text'] : prior.capabilities,
      credential_ref: prior === null ? null : prior.credential_ref,
      enabled: false, test_status: 'untested',
    };
    const params: SaveProviderParams = { config: config,
      secret: this.secret.trim().length === 0 ? null : this.secret.trim() };
    this.secret = '';
    const result = await CoreGateway.call(this.dbPath, 'providers.save', params);
    params.secret = null;
    this.busy = false;
    if (!this.active) { return; }
    if (!result.ok) {
      this.error = uiErrorOf(result);
      this.uncertain = true;
      this.message = '保存未确认成功；如本次更换了 API Key，请重新填写后保存。'; return;
    }
    this.adopt(result.data as ProviderConfig);
    this.message = '已保存，当前未启用。可检测模型列表，填写模型后测试连接，再启用。';
    AppState.bumpDataVersion();
    await this.load();
  }

  private canProbe(): boolean {
    return !this.busy && !this.loading && !this.dirty && !this.uncertain && this.selected !== null;
  }

  private async detectModels(): Promise<void> {
    if (!this.canProbe()) { return; }
    this.busy = true;
    this.error = EMPTY_UI_ERROR;
    const params: ProviderIdParams = { provider_id: (this.selected as ProviderConfig).id };
    const result = await CoreGateway.call(this.dbPath, 'providers.models', params);
    this.busy = false;
    if (!this.active) { return; }
    if (!result.ok) { this.error = uiErrorOf(result); return; }
    this.models = result.data as ProviderModelInfo[];
    this.message = this.models.length === 0 ? '服务返回空模型列表，可手动填写准确模型 ID。' : '点击一个模型填入，保存后再测试连接。';
  }

  private async test(): Promise<void> {
    if (!this.canProbe()) { return; }
    if (this.model.trim().length === 0) { this.validation('请先填写模型 ID 并保存。'); return; }
    this.busy = true;
    this.error = EMPTY_UI_ERROR;
    const id: string = (this.selected as ProviderConfig).id;
    const params: TestProviderParams = { provider_id: id, scope: ['generate_text'] };
    const result = await CoreGateway.call(this.dbPath, 'providers.test', params);
    if (!this.active) { this.busy = false; return; }
    // Always refresh persisted test_status, including partial/failed reports.
    const refreshed = await CoreGateway.call(this.dbPath, 'providers.list', null);
    this.busy = false;
    if (!this.active) { return; }
    if (refreshed.ok) {
      const all: ProviderConfig[] = refreshed.data as ProviderConfig[];
      let found: boolean = false;
      for (let i = 0; i < all.length; i++) {
        if (all[i].id === id) { this.adopt(all[i]); found = true; break; }
      }
      if (!found) { this.uncertain = true; this.message = '服务已变更或移除，请刷新配置。'; return; }
    } else {
      this.uncertain = true;
      this.error = uiErrorOf(refreshed);
      this.message = '无法确认最新测试状态，请重新打开本页。'; return;
    }
    if (!result.ok) { this.error = uiErrorOf(result); this.message = '连接测试未通过，请检查配置后重试。'; }
    else {
      const report: ProviderTestReport = result.data as ProviderTestReport;
      this.message = (this.selected as ProviderConfig).test_status === 'ok'
        ? '文本生成测试通过，可启用服务。' : '测试未全部通过，请检查服务和模型。';
      if (report.message !== null) { this.message += ' ' + report.message; }
    }
    AppState.bumpDataVersion();
    await this.load();
  }

  private async toggleEnabled(): Promise<void> {
    if (!this.canProbe()) { return; }
    const provider: ProviderConfig = this.selected as ProviderConfig;
    if (!provider.enabled && provider.test_status !== 'ok') {
      this.validation('请先通过文本生成连接测试，再启用服务。'); return;
    }
    this.busy = true;
    this.error = EMPTY_UI_ERROR;
    const config: ProviderConfig = { id: provider.id, label: provider.label,
      adapter_id: provider.adapter_id, endpoint: provider.endpoint, model: provider.model,
      capabilities: provider.capabilities, credential_ref: provider.credential_ref,
      enabled: !provider.enabled, test_status: provider.test_status };
    const params: SaveProviderParams = { config: config, secret: null };
    const result = await CoreGateway.call(this.dbPath, 'providers.save', params);
    this.busy = false;
    if (!this.active) { return; }
    if (!result.ok) { this.error = uiErrorOf(result); return; }
    this.adopt(result.data as ProviderConfig);
    this.message = config.enabled ? '已启用。日记生成可以使用此服务；已有等待任务可到任务中心继续。' : '已停用；配置和密钥仍保留。';
    AppState.bumpDataVersion();
    await this.load();
  }

  private async remove(): Promise<void> {
    if (!this.canProbe()) { return; }
    const id: string = (this.selected as ProviderConfig).id;
    if (!await confirmAction('删除模型服务？', '移除配置及不再使用的凭据，不会删除日记。', '删除服务') || !this.active) { return; }
    if (!this.canProbe() || (this.selected as ProviderConfig).id !== id) { return; }
    this.busy = true;
    const params: ProviderIdParams = { provider_id: id };
    const result = await CoreGateway.call(this.dbPath, 'providers.remove', params);
    this.busy = false;
    if (!this.active) { return; }
    if (!result.ok) { this.error = uiErrorOf(result); return; }
    this.dirty = false;
    await this.newProvider(null);
    this.message = '已删除服务，日记内容保持不变。';
    AppState.bumpDataVersion();
    await this.load();
  }

  @Builder
  form() {
    Column({ space: 12 }) {
      SectionTitle({ text: this.selected === null ? '添加模型服务' : '编辑模型服务' })
      TextInput({ text: this.label, placeholder: '服务名称，例如：我的 AI 服务' }).attributeModifier(sketchInput)
        .enabled(!this.busy && !this.loading).onChange((v: string) => { this.label = v; this.dirty = true; })
      TextInput({ text: this.endpoint, placeholder: 'API 根地址，例如 https://服务地址/v1' }).attributeModifier(sketchInput)
        .enabled(!this.busy && !this.loading).onChange((v: string) => { this.endpoint = v; this.models = []; this.dirty = true; })
      TextInput({ text: this.model, placeholder: '模型 ID（可手填，或保存后检测模型）' }).attributeModifier(sketchInput)
        .enabled(!this.busy && !this.loading).onChange((v: string) => { this.model = v; this.dirty = true; })
      TextInput({ text: this.secret, placeholder: this.selected !== null && (this.selected as ProviderConfig).credential_ref !== null
        ? 'API Key 已保存，留空保留；输入新值替换' : 'API Key（本机回环服务可留空）' })
        .attributeModifier(sketchInput).type(InputType.Password).showPasswordIcon(false)
        .enableAutoFill(false).copyOption(CopyOptions.None)
        .enabled(!this.busy && !this.loading).onChange((v: string) => { this.secret = v; this.dirty = true; })
      Text('API Key 保存在应用沙箱独立凭据文件中，目前未接入系统密钥库。界面不回显密钥。手机上的 localhost 指手机自身。')
        .fontSize(12).fontColor(SketchColors.muted)
      Button(this.busy ? '处理中…' : '保存配置').attributeModifier(sketchButton).drawModifier(sketchOutline())
        .enabled(!this.busy && !this.loading && this.loaded && (this.dirty || this.selected === null)).onClick(() => { this.save(); })
      if (this.selected !== null) {
        Text(this.dirty ? '有未保存修改，请先保存再测试。' : '检测模型会访问服务；连接测试会发送一段测试文字，可能计费，不发送你的日记。')
          .fontSize(12).fontColor(SketchColors.muted)
        Button('检测可用模型').attributeModifier(sketchButton).enabled(this.canProbe()).onClick(() => { this.detectModels(); })
        Button('测试文本生成连接').attributeModifier(sketchButton).enabled(this.canProbe()).onClick(() => { this.test(); })
        Button((this.selected as ProviderConfig).enabled ? '停用服务' : '启用服务')
          .attributeModifier(sketchButton).enabled(this.canProbe()).onClick(() => { this.toggleEnabled(); })
        Button('删除此服务及凭据').attributeModifier(sketchButton).enabled(this.canProbe()).onClick(() => { this.remove(); })
      }
      ForEach(this.models, (item: ProviderModelInfo) => {
        Button(item.id).attributeModifier(sketchButton).enabled(!this.busy && !this.loading)
          .onClick(() => { this.model = item.id; this.dirty = true; })
      }, (item: ProviderModelInfo) => item.id)
    }.width('100%').padding(16).backgroundColor(SketchColors.paper).drawModifier(sketchOutline())
  }

  build() {
    NavDestination() {
      Scroll() {
        Column({ space: 16 }) {
          Text('让 AI 帮你整理日记').fontSize(24).fontColor(SketchColors.ink)
          Text('只有启用的服务可接收你主动交给 AI 处理的材料。保存配置不会发起模型请求；测试通过后请再启用。')
            .fontSize(14).fontColor(SketchColors.muted)
          if (this.loading) { LoadingRow({ text: '读取模型服务…' }) }
          if (this.error.code.length > 0) {
            ErrorBanner({ error: this.error, showReload: !this.busy && !this.loading,
              onReload: () => { this.load(); } })
          }
          if (this.message.length > 0) { Text(this.message).fontColor(SketchColors.text).fontSize(14) }
          ForEach(this.providers, (provider: ProviderConfig) => {
            Column({ space: 6 }) {
              Text(provider.label).fontSize(18).fontColor(SketchColors.ink)
              Text(provider.model === null ? '未指定模型' : provider.model).fontSize(13).fontColor(SketchColors.muted)
              StatePill({ text: provider.enabled ? '已启用' : '未启用', tone: provider.enabled ? 'ok' : 'muted' })
              Text(provider.test_status === 'ok' ? '连接测试通过' : provider.test_status === 'failed' ? '连接测试失败' : '尚未验证文本生成')
                .fontSize(13).fontColor(SketchColors.muted)
              Button('编辑配置').attributeModifier(sketchButton).enabled(!this.busy && !this.loading)
                .onClick(() => { this.edit(provider); })
            }.width('100%').padding(14).backgroundColor(SketchColors.paper).drawModifier(sketchOutline())
          }, (provider: ProviderConfig) => provider.id)
          Button('添加自定义服务').attributeModifier(sketchButton).enabled(!this.busy && !this.loading)
            .onClick(() => { this.newProvider(null); })
          ForEach(this.presets, (preset: ProviderPreset) => {
            Button('使用预设 · ' + preset.label).attributeModifier(sketchButton).enabled(!this.busy && !this.loading)
              .onClick(() => { this.newProvider(preset); })
          }, (preset: ProviderPreset) => preset.id)
          this.form()
          Button('查看任务，继续等待中的生成').attributeModifier(sketchButton)
            .onClick(() => { this.pathStack.pushPathByName('jobs', ''); })
          Button('配置本地语音转写').attributeModifier(sketchButton)
            .onClick(() => { this.pathStack.pushPathByName('transcription', ''); })
        }.width('100%').padding(16).alignItems(HorizontalAlign.Start)
      }.width('100%').height('100%').align(Alignment.Top).backgroundColor(SketchColors.canvas)
    }.title('模型服务', { backgroundColor: SketchColors.canvas })
  }
}

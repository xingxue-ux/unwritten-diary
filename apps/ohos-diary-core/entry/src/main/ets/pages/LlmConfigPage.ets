import { common } from '@kit.AbilityKit';
import { LengthMetrics } from '@kit.ArkUI';
import { ProviderConfig, ProviderPreset, ProviderModelInfo, ProviderIdParams,
  SaveProviderParams, TestProviderParams, ProviderTestReport } from '../core/Models';
import { CoreGateway, uiErrorOf } from '../ui/CoreGateway';
import { EMPTY_UI_ERROR, UiError } from '../ui/Errors';
import { databasePath, confirmAction } from '../ui/AppRuntime';
import { AppState } from '../ui/AppState';
import { modelLabel, presetLabel } from '../ui/UserText';
import { ErrorBanner, SectionTitle, StatePill, LoadingRow } from '../ui/Common';
import { SketchColors, SketchFonts, sketchInput, sketchButton, sketchOutline } from '../ui/Sketch';

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
  @State editingAddress: boolean = false;
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
    this.editingAddress = false;
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
    this.label = preset === null ? '' : presetLabel(preset.label);
    this.endpoint = preset === null ? '' : preset.endpoint;
    this.model = preset === null ? '' : preset.model;
    this.secret = '';
    this.models = [];
    this.dirty = preset !== null;
    this.editingAddress = preset === null;
    this.error = EMPTY_UI_ERROR;
    this.message = preset === null ? '' : '请填写访问密钥，然后保存。';
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
      this.validation('请填写服务名称和访问地址。'); return;
    }
    if (!/^https?:\/\/[^\s/?#]+(?:\/[^\s?#]*)?$/i.test(endpoint) ||
      endpoint.includes('@') || /\/(chat\/completions|models|responses)$/i.test(endpoint)) {
      this.validation('地址格式不正确，请填写服务方提供的访问地址。'); return;
    }
    if (!/^https:\/\//i.test(endpoint) &&
      !/^http:\/\/(localhost|127(?:\.\d{1,3}){3}|\[::1\])(?::\d+)?(?:\/|$)/i.test(endpoint)) {
      this.validation('远程服务需要使用加密连接地址。'); return;
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
      this.message = '保存未完成，请重新填写密钥后重试。'; return;
    }
    this.adopt(result.data as ProviderConfig);
    this.message = '已保存，请检查连接。';
    AppState.bumpDataVersion();
    await this.load();
  }

  private canProbe(): boolean {
    return !this.busy && !this.loading && !this.dirty && !this.uncertain && this.selected !== null;
  }

  private selectedInUse(): boolean {
    if (this.selected === null) { return false; }
    const id: string = (this.selected as ProviderConfig).id;
    return this.providers.some((p: ProviderConfig) => p.id === id && p.enabled);
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
    this.message = this.models.length === 0 ? '没有找到可用模型，请手动填写模型名称。' : '请选择模型，保存后检查连接。';
  }

  private async test(): Promise<void> {
    if (!this.canProbe()) { return; }
    if (this.model.trim().length === 0) { this.validation('请先填写模型名称并保存。'); return; }
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
    if (!result.ok) { this.error = uiErrorOf(result); this.message = ''; }
    else {
      const report: ProviderTestReport = result.data as ProviderTestReport;
      this.message = (this.selected as ProviderConfig).test_status === 'ok'
        ? '连接成功，可以使用。' : '连接未通过，请检查配置。';

    }
    AppState.bumpDataVersion();
    await this.load();
    if (!result.ok && this.active && !this.uncertain) { this.error = uiErrorOf(result); }
  }

  private async useOnline(provider: ProviderConfig): Promise<void> {
    if (this.busy || this.loading || !this.loaded || this.uncertain || provider.enabled) { return; }
    if (provider.test_status !== 'ok') {
      this.validation('请先检查连接。'); return;
    }
    this.busy = true;
    this.error = EMPTY_UI_ERROR;
    const params: ProviderIdParams = { provider_id: provider.id };
    const result = await CoreGateway.call(this.dbPath, 'providers.selectGeneration', params);
    this.busy = false;
    if (!this.active) { return; }
    if (!result.ok) { await this.load(); this.error = uiErrorOf(result); return; }
    this.message = '已切换整理服务。';
    AppState.bumpDataVersion();
    await this.load();
  }

  private async toggleEnabled(): Promise<void> {
    if (!this.canProbe()) { return; }
    if (!this.selectedInUse()) { await this.useOnline(this.selected as ProviderConfig); return; }
    const provider: ProviderConfig = this.selected as ProviderConfig;
    this.busy = true;
    this.error = EMPTY_UI_ERROR;
    const config: ProviderConfig = { id: provider.id, label: provider.label,
      adapter_id: provider.adapter_id, endpoint: provider.endpoint, model: provider.model,
      capabilities: provider.capabilities, credential_ref: provider.credential_ref,
      enabled: false, test_status: provider.test_status };
    const params: SaveProviderParams = { config: config, secret: null };
    const result = await CoreGateway.call(this.dbPath, 'providers.save', params);
    this.busy = false;
    if (!this.active) { return; }
    if (!result.ok) { await this.load(); this.error = uiErrorOf(result); return; }
    this.message = '已停用，配置和密钥已保留。';
    AppState.bumpDataVersion();
    await this.load();
  }

  private async remove(): Promise<void> {
    if (!this.canProbe()) { return; }
    const id: string = (this.selected as ProviderConfig).id;
    if (!await confirmAction('删除模型服务？', '将删除此服务和密钥，日记会保留。', '删除服务') || !this.active) { return; }
    if (!this.canProbe() || (this.selected as ProviderConfig).id !== id) { return; }
    this.busy = true;
    const params: ProviderIdParams = { provider_id: id };
    const result = await CoreGateway.call(this.dbPath, 'providers.remove', params);
    this.busy = false;
    if (!this.active) { return; }
    if (!result.ok) { this.error = uiErrorOf(result); return; }
    this.dirty = false;
    await this.newProvider(null);
    this.message = '服务已删除，日记已保留。';
    AppState.bumpDataVersion();
    await this.load();
  }

  @Builder
  form() {
    Column({ space: 12 }) {
      SectionTitle({ text: this.selected === null ? '添加模型服务' : '编辑模型服务' })
      TextInput({ text: this.label, placeholder: '服务名称' }).attributeModifier(sketchInput)
        .enabled(!this.busy && !this.loading).onChange((v: string) => {
          if (this.label !== v) { this.label = v; this.dirty = true; }
        })
      Text('已选模型：' + modelLabel(this.model)).fontSize(13).fontColor(SketchColors.muted)
      Button(this.editingAddress ? '收起地址和模型' : '修改地址和模型').attributeModifier(sketchButton).drawModifier(sketchOutline())
        .enabled(!this.busy && !this.loading).onClick(() => { this.editingAddress = !this.editingAddress; })
      if (this.editingAddress) {
        TextInput({ text: this.endpoint, placeholder: '服务方提供的访问地址' }).attributeModifier(sketchInput)
        .enabled(!this.busy && !this.loading).onChange((v: string) => {
          if (this.endpoint !== v) { this.endpoint = v; this.models = []; this.dirty = true; }
        })
      TextInput({ text: this.model, placeholder: '模型名称，也可保存后选择' }).attributeModifier(sketchInput)
        .enabled(!this.busy && !this.loading).onChange((v: string) => {
          if (this.model !== v) { this.model = v; this.dirty = true; }
        })
      }
      TextInput({ text: this.secret, placeholder: this.selected !== null && (this.selected as ProviderConfig).credential_ref !== null
        ? '密钥已保存，留空保留' : '访问密钥' })
        .attributeModifier(sketchInput).type(InputType.Password).showPasswordIcon(false)
        .enableAutoFill(false).copyOption(CopyOptions.None)
        .enabled(!this.busy && !this.loading).onChange((v: string) => {
          if (this.secret !== v) { this.secret = v; this.dirty = true; }
        })
      Text('密钥仅保存在本机，保存后不会显示。')
        .fontSize(12).fontColor(SketchColors.muted)
      if (this.selected !== null) {
        Text(this.dirty ? '有未保存修改，请先保存再测试。' : '检查连接会发送测试文字，可能计费，不发送日记。')
          .fontSize(12).fontColor(SketchColors.muted)
      }
      Flex({ wrap: FlexWrap.Wrap, space: { main: LengthMetrics.vp(8), cross: LengthMetrics.vp(8) } }) {
        Button(this.busy ? '处理中…' : '保存配置').attributeModifier(sketchButton).drawModifier(sketchOutline())
          .enabled(!this.busy && !this.loading && this.loaded && (this.dirty || this.selected === null)).onClick(() => { this.save(); })
        if (this.selected !== null) {
          Button('查找可用模型').attributeModifier(sketchButton).drawModifier(sketchOutline()).enabled(this.canProbe()).onClick(() => { this.detectModels(); })
          Button('检查连接').attributeModifier(sketchButton).drawModifier(sketchOutline()).enabled(this.canProbe()).onClick(() => { this.test(); })
          Button(this.selectedInUse() ? '停用服务' : '使用此服务')
            .attributeModifier(sketchButton).drawModifier(sketchOutline()).enabled(this.canProbe()).onClick(() => { this.toggleEnabled(); })
          Button('删除服务').attributeModifier(sketchButton).drawModifier(sketchOutline()).enabled(this.canProbe()).onClick(() => { this.remove(); })
        }
      }.width('100%')
      if (this.error.code.length > 0) {
        ErrorBanner({ error: this.error, showReload: !this.busy && !this.loading,
          onReload: () => { this.load(); } })
      }
      if (this.message.length > 0) { Text(this.message).fontColor(SketchColors.text).fontSize(14) }
      ForEach(this.models, (item: ProviderModelInfo) => {
        Button(modelLabel(item.id)).attributeModifier(sketchButton).drawModifier(sketchOutline()).enabled(!this.busy && !this.loading)
          .onClick(() => { this.model = item.id; this.dirty = true; })
      }, (item: ProviderModelInfo) => item.id)
    }.width('100%').alignItems(HorizontalAlign.Start).padding(16)
      .backgroundColor(SketchColors.paper).drawModifier(sketchOutline())
  }

  build() {
    NavDestination() {
      Scroll() {
        Column({ space: 16 }) {
          Text('让智能助手整理日记').fontFamily(SketchFonts.display).fontSize(24).fontColor(SketchColors.ink)
          Text('每次只使用一个整理服务，切换后保留配置。')
            .fontSize(14).fontColor(SketchColors.muted)
          if (this.loading) { LoadingRow({ text: '读取模型服务…' }) }
          Flex({ wrap: FlexWrap.Wrap, space: { main: LengthMetrics.vp(8), cross: LengthMetrics.vp(8) } }) {
            Button('添加自定义服务').attributeModifier(sketchButton).drawModifier(sketchOutline()).enabled(!this.busy && !this.loading)
              .onClick(() => { this.newProvider(null); })
            ForEach(this.presets, (preset: ProviderPreset) => {
              Button(presetLabel(preset.label)).attributeModifier(sketchButton).drawModifier(sketchOutline()).enabled(!this.busy && !this.loading)
                .onClick(() => { this.newProvider(preset); })
            }, (preset: ProviderPreset) => preset.id)
          }.width('100%')
          ForEach(this.providers, (provider: ProviderConfig) => {
            Column({ space: 6 }) {
              Flex({ wrap: FlexWrap.Wrap, alignItems: ItemAlign.Center,
                space: { main: LengthMetrics.vp(12), cross: LengthMetrics.vp(8) } }) {
                Text(provider.label).fontSize(18).fontColor(SketchColors.ink)
                StatePill({ text: provider.enabled ? '当前使用' : '未使用', tone: provider.enabled ? 'ok' : 'muted' })
                Text(provider.test_status === 'ok' ? '连接已通过' : provider.test_status === 'failed' ? '连接未通过' : '请先检查连接')
                  .fontSize(13).fontColor(SketchColors.muted)
                Button('编辑配置').attributeModifier(sketchButton).drawModifier(sketchOutline()).enabled(!this.busy && !this.loading)
                  .onClick(() => { this.edit(provider); })
                Button(provider.enabled ? '正在使用' : '使用此服务').attributeModifier(sketchButton).drawModifier(sketchOutline())
                  .enabled(!this.busy && !this.loading && !this.uncertain && !provider.enabled && provider.test_status === 'ok')
                  .onClick(() => { this.useOnline(provider); })
              }.width('100%')
              Text(modelLabel(provider.model)).fontSize(13).fontColor(SketchColors.muted)
            }.width('100%').alignItems(HorizontalAlign.Start).padding(14)
              .backgroundColor(SketchColors.paper).drawModifier(sketchOutline())
          }, (provider: ProviderConfig) => `${provider.id}-${provider.test_status}-${provider.enabled}-${provider.label}-${provider.model}`)
          this.form()
          Button('查看待办任务').attributeModifier(sketchButton).drawModifier(sketchOutline())
            .onClick(() => { this.pathStack.pushPathByName('jobs', ''); })
          Button('配置本地语音转写').attributeModifier(sketchButton).drawModifier(sketchOutline())
            .onClick(() => { this.pathStack.pushPathByName('transcription', ''); })
        }.width('100%').padding(16).alignItems(HorizontalAlign.Start)
      }.width('100%').height('100%').align(Alignment.Top).backgroundColor(SketchColors.canvas)
    }.title('模型服务', { backgroundColor: SketchColors.canvas })
  }
}

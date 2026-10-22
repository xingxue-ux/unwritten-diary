import { common } from '@kit.AbilityKit';
import { StyleProfile, StyleProfileInput } from '../core/Models';
import { SettingsStore } from '../state/SettingsStore';
import { StyleStore } from '../state/StyleStore';
import { AppState } from '../ui/AppState';
import { databasePath, newOperationId } from '../ui/AppRuntime';
import { ErrorBanner, LoadingRow, PageTitle } from '../ui/Common';
import { EMPTY_UI_ERROR, UiError } from '../ui/Errors';
import { SketchCard, SketchColors, sketchButton, sketchInput, sketchArea, sketchOutline } from '../ui/Sketch';

@Component
export struct StylesPage {
  @State styles: StyleProfile[] = [];
  @State busy: boolean = true;
  @State error: UiError = EMPTY_UI_ERROR;
  @State message: string = '';
  @State selectedId: string = '';
  @State revision: number = 0;
  @State label: string = '';
  @State lengthPreference: string = '';
  @State person: string = '';
  @State tone: string = '';
  @State temperature: string = '';
  @State rhetoric: string = '';
  @State examples: string = '';
  @State banned: string = '';
  @State defaultId: string = '';
  private store: StyleStore | null = null;
  private settings: SettingsStore | null = null;

  aboutToAppear(): void {
    const context = getContext(this) as common.UIAbilityContext;
    this.store = new StyleStore(databasePath(context));
    this.settings = new SettingsStore(databasePath(context));
    this.load();
  }

  private async load(): Promise<void> {
    if (this.store === null || this.settings === null) { return; }
    this.busy = true;
    await this.store.refresh();
    await this.settings.refresh();
    this.styles = this.store.styles.slice();
    this.error = this.store.error.code.length > 0 ? this.store.error : this.settings.error;
    this.defaultId = this.settings.settings === null ? '' : (this.settings.settings.default_style_id ?? '');
    if (this.styles.length > 0) { this.selectStyle(this.styles[0]); }
    this.busy = false;
  }

  private selectStyle(profile: StyleProfile): void {
    this.selectedId = profile.id;
    this.revision = profile.revision;
    this.label = profile.label;
    this.lengthPreference = profile.lengthPreference ?? '';
    this.person = profile.person ?? '';
    this.tone = profile.tone ?? '';
    this.temperature = profile.emotionalTemperature ?? '';
    this.rhetoric = profile.rhetoricLevel ?? '';
    this.examples = profile.examples.join('\n');
    this.banned = profile.bannedExpressions.join('\n');
  }

  private createStyle(): void {
    this.selectedId = newOperationId('sty-ohos');
    this.revision = 0;
    this.label = '';
    this.lengthPreference = '';
    this.person = '';
    this.tone = '';
    this.temperature = '';
    this.rhetoric = '';
    this.examples = '';
    this.banned = '';
    this.message = '填写新风格；空白项使用默认设置。';
  }

  private lines(value: string): string[] {
    return value.split('\n').map((line: string) => line.trim()).filter((line: string) => line.length > 0);
  }

  private async save(): Promise<void> {
    if (this.busy || this.store === null) { return; }
    this.busy = true;
    const profile: StyleProfileInput = {
      id: this.selectedId, label: this.label, lengthPreference: this.lengthPreference || null,
      person: this.person || null, tone: this.tone || null,
      emotionalTemperature: this.temperature || null, rhetoricLevel: this.rhetoric || null,
      examples: this.lines(this.examples), bannedExpressions: this.lines(this.banned),
    };
    const saved: StyleProfile | null = await this.store.saveStyle(profile, this.revision);
    this.styles = this.store.styles.slice();
    this.error = this.store.error;
    this.message = this.store.message;
    if (saved !== null) {
      this.selectStyle(saved);
      AppState.bumpDataVersion();
    } else if (this.store.conflicted && this.store.current !== null) {
      this.selectStyle(this.store.current);
    }
    this.busy = false;
  }

  private async useDefault(): Promise<void> {
    if (this.busy || this.settings === null) { return; }
    this.busy = true;
    const ok: boolean = await this.settings.setDefaultStyle(this.selectedId);
    this.error = this.settings.error;
    this.message = this.settings.message;
    this.defaultId = this.settings.settings === null ? '' : (this.settings.settings.default_style_id ?? '');
    if (ok) { AppState.bumpDataVersion(); }
    this.busy = false;
  }

  @Builder
  field(title: string, value: string, change: (text: string) => void) {
    Text(title).fontSize(13).fontColor(SketchColors.muted).width('100%')
    TextInput({ text: value, placeholder: '不指定' }).attributeModifier(sketchInput)
      .fontSize(15).fontColor(SketchColors.text)
      .backgroundColor(SketchColors.soft).borderRadius(0)
      .constraintSize({ minHeight: 48 }).enabled(!this.busy).onChange(change)
  }

  build() {
    Scroll() {
      Column({ space: 20 }) {
        PageTitle({ title: '风格偏好', subtitle: '篇幅、语气与叙述方式。历史日记保留当时的风格。' })
        if (this.busy) { LoadingRow() }
        ErrorBanner({ error: this.error, showReload: true, onReload: () => { this.load(); } })
        Flex({ wrap: FlexWrap.Wrap }) {
          ForEach(this.styles, (profile: StyleProfile) => {
            Button(`${profile.label}${profile.id === this.defaultId ? ' · 默认' : ''}`)
              .attributeModifier(sketchButton).drawModifier(sketchOutline()).margin({ right: 8, bottom: 8 })
              .backgroundColor(profile.id === this.selectedId ? SketchColors.button : SketchColors.paper)
              .enabled(!this.busy).onClick(() => { this.selectStyle(profile); })
          }, (profile: StyleProfile) => `${profile.id}-${profile.revision}`)
          Button('新建风格').attributeModifier(sketchButton).drawModifier(sketchOutline()).enabled(!this.busy).onClick(() => { this.createStyle(); })
        }
        if (this.selectedId.length > 0) {
          SketchCard() {
            Column({ space: 12 }) {
              this.field('名称', this.label, (text: string) => { this.label = text; })
              this.field('篇幅', this.lengthPreference, (text: string) => { this.lengthPreference = text; })
              this.field('叙述人称', this.person, (text: string) => { this.person = text; })
              this.field('语气', this.tone, (text: string) => { this.tone = text; })
              this.field('情绪温度', this.temperature, (text: string) => { this.temperature = text; })
              this.field('修辞程度', this.rhetoric, (text: string) => { this.rhetoric = text; })
              Text('参考示例（每行一条）').fontSize(13).fontColor(SketchColors.muted)
              TextArea({ text: this.examples }).attributeModifier(sketchArea).height(120).borderRadius(0)
                .fontColor(SketchColors.text).backgroundColor(SketchColors.soft).enabled(!this.busy)
                .onChange((text: string) => { this.examples = text; })
              Text('禁用表达（每行一条）').fontSize(13).fontColor(SketchColors.muted)
              TextArea({ text: this.banned }).attributeModifier(sketchArea).height(100).borderRadius(0)
                .fontColor(SketchColors.text).backgroundColor(SketchColors.soft).enabled(!this.busy)
                .onChange((text: string) => { this.banned = text; })
              Flex({ wrap: FlexWrap.Wrap }) {
                Button('保存风格').attributeModifier(sketchButton).drawModifier(sketchOutline()).backgroundColor(SketchColors.button)
                  .enabled(!this.busy && this.label.trim().length > 0).onClick(() => { this.save(); })
                Button('设为默认').attributeModifier(sketchButton).drawModifier(sketchOutline()).margin({ left: 8 })
                  .enabled(!this.busy && this.revision > 0 && this.defaultId !== this.selectedId)
                  .onClick(() => { this.useDefault(); })
              }
            }
            .width('100%').alignItems(HorizontalAlign.Start)
          }
        }
        Text(this.message).fontSize(13).fontColor(SketchColors.muted).width('100%')
      }
      .width('100%').constraintSize({ maxWidth: 1000 }).padding(24).alignItems(HorizontalAlign.Start)
    }
    .width('100%').height('100%').backgroundColor(SketchColors.canvas)
  }
}

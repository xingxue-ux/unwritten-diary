import { common } from '@kit.AbilityKit';
import { StyleProfile, StyleProfileInput } from '../core/Models';
import { SettingsStore } from '../state/SettingsStore';
import { DEFAULT_STYLE_ID, StyleStore } from '../state/StyleStore';
import { AppState } from '../ui/AppState';
import { databasePath, newOperationId } from '../ui/AppRuntime';
import { ErrorBanner, LoadingRow, PageTitle } from '../ui/Common';
import { EMPTY_UI_ERROR, UiError } from '../ui/Errors';
import { SketchCard, SketchColors, sketchButton, sketchInput, sketchArea, sketchOutline } from '../ui/Sketch';

interface StyleDraft {
  id: string;
  revision: number;
  label: string;
  lengthPreference: string;
  person: string;
  tone: string;
  temperature: string;
  rhetoric: string;
  lengthCustom: boolean;
  personCustom: boolean;
  toneCustom: boolean;
  temperatureCustom: boolean;
  rhetoricCustom: boolean;
  examples: string;
  banned: string;
}

@Component
export struct StylesPage {
  @State styles: StyleProfile[] = [];
  @State busy: boolean = true;
  @State error: UiError = EMPTY_UI_ERROR;
  @State message: string = '';
  @State selectedId: string = '';
  @State revision: number = 0;
  @State label: string = '';
  @State lengthPreference: string = '随内容';
  @State person: string = '沿用原文';
  @State tone: string = '沿用原文';
  @State temperature: string = '保留原样';
  @State rhetoric: string = '朴素直写';
  @State lengthCustom: boolean = false;
  @State personCustom: boolean = false;
  @State toneCustom: boolean = false;
  @State temperatureCustom: boolean = false;
  @State rhetoricCustom: boolean = false;
  @State examples: string = '';
  @State banned: string = '';
  @State defaultId: string = '';
  private store: StyleStore | null = null;
  private settings: SettingsStore | null = null;
  private drafts: Map<string, StyleDraft> = new Map<string, StyleDraft>();

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
    this.defaultId = this.settings.settings?.default_style_id ?? DEFAULT_STYLE_ID;
    this.drafts.clear();
    const active: StyleProfile | undefined = this.styles.find((profile: StyleProfile) => profile.id === this.defaultId);
    if (active !== undefined) { this.selectStyle(active); }
    else if (this.styles.length > 0) { this.selectStyle(this.styles[0]); }
    this.busy = false;
  }

  private selectStyle(profile: StyleProfile): void {
    this.selectedId = profile.id;
    this.revision = profile.revision;
    this.label = profile.label;
    this.lengthPreference = profile.lengthPreference || '随内容';
    this.person = profile.person || '沿用原文';
    this.tone = profile.tone || '沿用原文';
    this.temperature = profile.emotionalTemperature || '保留原样';
    this.rhetoric = profile.rhetoricLevel || '朴素直写';
    this.lengthCustom = !['随内容', '简短一些', '保留细节'].includes(this.lengthPreference);
    this.personCustom = !['沿用原文', '用“我”叙述', '少用人称'].includes(this.person);
    this.toneCustom = !['沿用原文', '日常口语', '平实书面'].includes(this.tone);
    this.temperatureCustom = !['保留原样', '含蓄一些', '直接一些'].includes(this.temperature);
    this.rhetoricCustom = !['朴素直写', '稍加润色', '更有文采'].includes(this.rhetoric);
    this.examples = profile.examples.join('\n');
    this.banned = profile.bannedExpressions.join('\n');
    this.message = '';
  }

  private rememberDraft(): void {
    if (this.selectedId.length === 0) { return; }
    this.drafts.set(this.selectedId, {
      id: this.selectedId, revision: this.revision, label: this.label,
      lengthPreference: this.lengthPreference, person: this.person, tone: this.tone,
      temperature: this.temperature, rhetoric: this.rhetoric,
      lengthCustom: this.lengthCustom, personCustom: this.personCustom,
      toneCustom: this.toneCustom, temperatureCustom: this.temperatureCustom,
      rhetoricCustom: this.rhetoricCustom, examples: this.examples, banned: this.banned,
    });
  }

  private restoreDraft(draft: StyleDraft): void {
    this.selectedId = draft.id;
    this.revision = draft.revision;
    this.label = draft.label;
    this.lengthPreference = draft.lengthPreference;
    this.person = draft.person;
    this.tone = draft.tone;
    this.temperature = draft.temperature;
    this.rhetoric = draft.rhetoric;
    this.lengthCustom = draft.lengthCustom;
    this.personCustom = draft.personCustom;
    this.toneCustom = draft.toneCustom;
    this.temperatureCustom = draft.temperatureCustom;
    this.rhetoricCustom = draft.rhetoricCustom;
    this.examples = draft.examples;
    this.banned = draft.banned;
    this.message = '';
  }

  private editStyle(profile: StyleProfile): void {
    if (this.selectedId === profile.id) { return; }
    this.rememberDraft();
    const draft: StyleDraft | undefined = this.drafts.get(profile.id);
    if (draft !== undefined) { this.restoreDraft(draft); }
    else { this.selectStyle(profile); }
  }

  private createStyle(): void {
    this.rememberDraft();
    this.selectedId = newOperationId('sty-ohos');
    this.revision = 0;
    this.label = '';
    this.applyPreset(0);
    this.examples = '';
    this.banned = '';
    this.message = '给新风格起个名字，再保存。';
  }

  private applyPreset(kind: number): void {
    this.lengthPreference = kind === 2 ? '简短一些' : '随内容';
    this.person = '沿用原文';
    this.tone = kind === 1 ? '日常口语' : (kind === 2 ? '平实书面' : '沿用原文');
    this.temperature = '保留原样';
    this.rhetoric = kind === 1 ? '稍加润色' : '朴素直写';
    this.lengthCustom = false;
    this.personCustom = false;
    this.toneCustom = false;
    this.temperatureCustom = false;
    this.rhetoricCustom = false;
  }

  private lines(value: string): string[] {
    return value.split('\n').map((line: string) => line.trim()).filter((line: string) => line.length > 0);
  }

  private optional(value: string, fallback: string): string | null {
    const trimmed: string = value.trim();
    return trimmed.length === 0 || trimmed === fallback ? null : trimmed;
  }

  private async save(): Promise<boolean> {
    if (this.busy || this.store === null) { return false; }
    this.busy = true;
    const profile: StyleProfileInput = {
      id: this.selectedId, label: this.label.trim(),
      lengthPreference: this.optional(this.lengthPreference, '随内容'),
      person: this.optional(this.person, '沿用原文'),
      tone: this.optional(this.tone, '沿用原文'),
      emotionalTemperature: this.optional(this.temperature, '保留原样'),
      rhetoricLevel: this.optional(this.rhetoric, '朴素直写'),
      examples: this.lines(this.examples), bannedExpressions: this.lines(this.banned),
    };
    const saved: StyleProfile | null = await this.store.saveStyle(profile, this.revision);
    this.styles = this.store.styles.slice();
    this.error = this.store.error;
    this.message = this.store.message;
    if (saved !== null) {
      this.drafts.delete(saved.id);
      this.selectStyle(saved);
      this.message = '风格已保存。';
      AppState.bumpDataVersion();
    } else if (this.store.conflicted && this.store.current !== null) {
      this.drafts.delete(this.selectedId);
      this.selectStyle(this.store.current);
      this.message = this.store.message;
    }
    this.busy = false;
    return saved !== null;
  }

  private async useStyle(styleId: string): Promise<void> {
    if (this.busy || this.settings === null) { return; }
    this.busy = true;
    const ok: boolean = await this.settings.setDefaultStyle(styleId);
    this.error = this.settings.error;
    this.message = ok ? '已使用这套风格。' : this.settings.message;
    this.defaultId = this.settings.settings?.default_style_id ?? DEFAULT_STYLE_ID;
    if (ok) { AppState.bumpDataVersion(); }
    this.busy = false;
  }

  private async saveAndUse(): Promise<void> {
    if (await this.save()) { await this.useStyle(this.selectedId); }
  }

  @Builder
  choiceField(title: string, value: string, custom: boolean, options: string[],
    select: (text: string, custom: boolean) => void, write: (text: string) => void) {
    Text(title).fontSize(14).fontColor(SketchColors.text).width('100%')
    Flex({ wrap: FlexWrap.Wrap }) {
      ForEach(options, (option: string) => {
        Button(option).attributeModifier(sketchButton).drawModifier(sketchOutline())
          .backgroundColor(!custom && value === option ? SketchColors.button : SketchColors.paper)
          .margin({ right: 8, bottom: 8 }).enabled(!this.busy)
          .onClick(() => { select(option, false); })
      }, (option: string) => option)
      Button('自己填写').attributeModifier(sketchButton).drawModifier(sketchOutline())
        .backgroundColor(custom ? SketchColors.button : SketchColors.paper)
        .margin({ bottom: 8 }).enabled(!this.busy)
        .onClick(() => { select('', true); })
    }
    if (custom) {
      TextInput({ text: value, placeholder: '写下你的偏好' }).attributeModifier(sketchInput)
        .fontSize(15).fontColor(SketchColors.text).backgroundColor(SketchColors.soft)
        .borderRadius(0).constraintSize({ minHeight: 48 }).enabled(!this.busy).onChange(write)
    }
  }

  build() {
    Scroll() {
      Column({ space: 20 }) {
        PageTitle({ title: '风格偏好', subtitle: '选好风格后，今后整理的日记会按它来写。' })
        if (this.busy) { LoadingRow() }
        ErrorBanner({ error: this.error, showReload: true, onReload: () => { this.load(); } })
        Text('已保存的风格').fontSize(18).fontColor(SketchColors.ink)
        Text('点“使用”切换风格，点“修改”单独编辑。').fontSize(13).fontColor(SketchColors.muted)
        ForEach(this.styles, (profile: StyleProfile) => {
          SketchCard() {
            Column({ space: 10 }) {
              Text(profile.label + (profile.id === this.defaultId ? ' · 正在使用' : ''))
                .fontSize(16).fontColor(SketchColors.ink).width('100%')
              Flex({ wrap: FlexWrap.Wrap }) {
                Button('使用').attributeModifier(sketchButton).drawModifier(sketchOutline())
                  .backgroundColor(profile.id === this.defaultId ? SketchColors.button : SketchColors.paper)
                  .enabled(!this.busy && profile.id !== this.defaultId)
                  .onClick(() => { this.useStyle(profile.id); })
                Button('修改').attributeModifier(sketchButton).drawModifier(sketchOutline())
                  .backgroundColor(profile.id === this.selectedId ? SketchColors.button : SketchColors.paper)
                  .margin({ left: 8 }).enabled(!this.busy)
                  .onClick(() => { this.editStyle(profile); })
              }
            }.width('100%').alignItems(HorizontalAlign.Start)
          }
        }, (profile: StyleProfile) => profile.id)
        Button('＋ 新建风格').attributeModifier(sketchButton).drawModifier(sketchOutline())
          .backgroundColor(SketchColors.button).width('100%').enabled(!this.busy)
          .onClick(() => { this.createStyle(); })
        if (this.selectedId.length > 0) {
          SketchCard() {
            Column({ space: 12 }) {
              Text(this.revision === 0 ? '新建风格' : '修改风格').fontSize(18).fontColor(SketchColors.ink)
              Text('名称').fontSize(14).fontColor(SketchColors.text)
              TextInput({ text: this.label, placeholder: '给风格起个名字' }).attributeModifier(sketchInput)
                .fontSize(15).fontColor(SketchColors.text).backgroundColor(SketchColors.soft)
                .borderRadius(0).constraintSize({ minHeight: 48 }).enabled(!this.busy)
                .onChange((text: string) => { this.label = text; })
              Text('快速选择').fontSize(14).fontColor(SketchColors.text)
              Text('保留原话是默认选项，只理顺语句，尽量保留你的说法。')
                .fontSize(13).fontColor(SketchColors.muted)
              Flex({ wrap: FlexWrap.Wrap }) {
                Button('保留原话').attributeModifier(sketchButton).drawModifier(sketchOutline()).margin({ right: 8, bottom: 8 })
                  .enabled(!this.busy).onClick(() => { this.applyPreset(0); })
                Button('自然成篇').attributeModifier(sketchButton).drawModifier(sketchOutline()).margin({ right: 8, bottom: 8 })
                  .enabled(!this.busy).onClick(() => { this.applyPreset(1); })
                Button('简洁记录').attributeModifier(sketchButton).drawModifier(sketchOutline()).margin({ bottom: 8 })
                  .enabled(!this.busy).onClick(() => { this.applyPreset(2); })
              }
              Text('也可以逐项调整。').fontSize(13).fontColor(SketchColors.muted)
              this.choiceField('篇幅', this.lengthPreference, this.lengthCustom,
                ['随内容', '简短一些', '保留细节'],
                (text: string, custom: boolean) => { this.lengthPreference = text; this.lengthCustom = custom; },
                (text: string) => { this.lengthPreference = text; })
              this.choiceField('叙述人称', this.person, this.personCustom,
                ['沿用原文', '用“我”叙述', '少用人称'],
                (text: string, custom: boolean) => { this.person = text; this.personCustom = custom; },
                (text: string) => { this.person = text; })
              this.choiceField('语气', this.tone, this.toneCustom,
                ['沿用原文', '日常口语', '平实书面'],
                (text: string, custom: boolean) => { this.tone = text; this.toneCustom = custom; },
                (text: string) => { this.tone = text; })
              this.choiceField('情绪表达', this.temperature, this.temperatureCustom,
                ['保留原样', '含蓄一些', '直接一些'],
                (text: string, custom: boolean) => { this.temperature = text; this.temperatureCustom = custom; },
                (text: string) => { this.temperature = text; })
              this.choiceField('修辞程度', this.rhetoric, this.rhetoricCustom,
                ['朴素直写', '稍加润色', '更有文采'],
                (text: string, custom: boolean) => { this.rhetoric = text; this.rhetoricCustom = custom; },
                (text: string) => { this.rhetoric = text; })
              Text('参考示例（每行一条）').fontSize(13).fontColor(SketchColors.muted)
              TextArea({ text: this.examples }).attributeModifier(sketchArea).height(120).borderRadius(0)
                .fontColor(SketchColors.text).backgroundColor(SketchColors.soft).enabled(!this.busy)
                .onChange((text: string) => { this.examples = text; })
              Text('不想出现的表达（每行一条）').fontSize(13).fontColor(SketchColors.muted)
              TextArea({ text: this.banned }).attributeModifier(sketchArea).height(100).borderRadius(0)
                .fontColor(SketchColors.text).backgroundColor(SketchColors.soft).enabled(!this.busy)
                .onChange((text: string) => { this.banned = text; })
              Flex({ wrap: FlexWrap.Wrap }) {
                Button('保存风格').attributeModifier(sketchButton).drawModifier(sketchOutline())
                  .enabled(!this.busy && this.label.trim().length > 0).onClick(() => { this.save(); })
                Button('保存并使用').attributeModifier(sketchButton).drawModifier(sketchOutline())
                  .backgroundColor(SketchColors.button).margin({ left: 8 })
                  .enabled(!this.busy && this.label.trim().length > 0).onClick(() => { this.saveAndUse(); })
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

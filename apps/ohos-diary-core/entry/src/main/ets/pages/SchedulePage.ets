import { common } from '@kit.AbilityKit';
import { i18n } from '@kit.LocalizationKit';
import { Settings } from '../core/Models';
import { SettingsStore } from '../state/SettingsStore';
import { AppState } from '../ui/AppState';
import { databasePath } from '../ui/AppRuntime';
import { ErrorBanner, LoadingRow, PageTitle, SectionTitle } from '../ui/Common';
import { EMPTY_UI_ERROR, UiError } from '../ui/Errors';
import { SketchCard, SketchColors, sketchButton, sketchOutline } from '../ui/Sketch';

interface ZoneChoice {
  label: string;
  id: string;
}

const ZONES: ZoneChoice[] = [
  { label: '中国标准时间', id: 'Asia/Shanghai' },
  { label: '东京时间', id: 'Asia/Tokyo' },
  { label: '新加坡时间', id: 'Asia/Singapore' },
  { label: '伦敦时间', id: 'Europe/London' },
  { label: '纽约时间', id: 'America/New_York' },
  { label: '洛杉矶时间', id: 'America/Los_Angeles' },
  { label: '世界协调时间', id: 'UTC' },
];

@Component
export struct SchedulePage {
  @State settings: Settings | null = null;
  @State busy: boolean = true;
  @State error: UiError = EMPTY_UI_ERROR;
  @State message: string = '';
  @State deviceZone: string = '';
  private store: SettingsStore | null = null;

  aboutToAppear(): void {
    const context = getContext(this) as common.UIAbilityContext;
    this.store = new SettingsStore(databasePath(context));
    try {
      this.deviceZone = i18n.getCalendar(i18n.getSystemLocale()).getTimeZone();
    } catch (_) {
      this.deviceZone = '';
    }
    this.load();
  }

  private async load(): Promise<void> {
    if (this.store === null) { return; }
    this.busy = true;
    await this.store.refresh();
    this.sync();
    this.busy = false;
  }

  private sync(): void {
    if (this.store === null) { return; }
    this.settings = this.store.settings;
    this.error = this.store.error;
    this.message = this.store.message;
  }

  private current(): Settings {
    return this.settings as Settings;
  }

  private zoneLabel(zone: string): string {
    for (let i: number = 0; i < ZONES.length; i++) {
      if (ZONES[i].id === zone) { return ZONES[i].label; }
    }
    return zone === this.deviceZone ? '设备当前时区' : '其他时区';
  }

  private two(value: number): string {
    return value < 10 ? `0${value}` : `${value}`;
  }

  private pickTime(): void {
    if (this.busy || this.settings === null) { return; }
    const parts: string[] = this.current().auto_organize_time.split(':');
    const selected: Date = new Date();
    selected.setHours(Number(parts[0]), Number(parts[1]), 0, 0);
    TimePickerDialog.show({
      selected: selected,
      useMilitaryTime: true,
      onAccept: (value: TimePickerResult) => {
        this.saveTime(`${this.two(value.hour)}:${this.two(value.minute)}`);
      },
    });
  }

  private async saveTime(time: string): Promise<void> {
    if (this.store === null || this.settings === null || this.busy || time === this.current().auto_organize_time) { return; }
    this.busy = true;
    const ok: boolean = await this.store.setAutoOrganizeTime(time);
    this.sync();
    if (ok) { this.message = '计划时间已更新。'; AppState.bumpDataVersion(); }
    this.busy = false;
  }

  private async saveZone(zone: string): Promise<void> {
    if (this.store === null || this.settings === null || this.busy || zone.length === 0 || zone === this.current().diary_time_zone) { return; }
    this.busy = true;
    const ok: boolean = await this.store.setDiaryTimeZone(zone);
    this.sync();
    if (ok) { this.message = '日记时区已更新。'; AppState.bumpDataVersion(); }
    this.busy = false;
  }

  private async toggleAutoOrganize(enabled: boolean): Promise<void> {
    if (this.store === null || this.settings === null || this.busy) { return; }
    this.busy = true;
    const ok: boolean = await this.store.setAutoOrganizeEnabled(enabled);
    this.sync();
    if (ok) {
      this.message = enabled ? '自动整理已开启。' : '自动整理已关闭。';
      AppState.bumpDataVersion();
    }
    this.busy = false;
  }

  build() {
    Scroll() {
      Column({ space: 18 }) {
        PageTitle({ title: '整理时机', subtitle: '安排一天何时结束，什么时候整理。' })
        ErrorBanner({ error: this.error, showReload: true, onReload: () => { this.load(); } })
        if (this.settings === null) {
          if (this.busy) { LoadingRow({ text: '正在读取设置…' }) }
          else { Text('暂时无法读取设置，请重试。').fontSize(14).fontColor(SketchColors.muted) }
        } else {
          SketchCard() {
            Column({ space: 12 }) {
              SectionTitle({ text: '自动整理' })
              Row({ space: 12 }) {
                Column({ space: 5 }) {
                  Text(this.current().auto_organize_enabled ? '已开启' : '已关闭')
                    .fontSize(17).fontColor(SketchColors.ink)
                  Text('只整理已经保存的记录。')
                    .fontSize(13).fontColor(SketchColors.muted)
                }.layoutWeight(1).alignItems(HorizontalAlign.Start)
                Toggle({ type: ToggleType.Switch, isOn: this.current().auto_organize_enabled })
                  .selectedColor(SketchColors.ink).enabled(!this.busy)
                  .onChange((enabled: boolean) => { this.toggleAutoOrganize(enabled); })
              }.width('100%')
              Text('到计划时间后，下次打开应用时会检查并处理。')
                .fontSize(13).fontColor(SketchColors.muted).width('100%')
            }.width('100%').alignItems(HorizontalAlign.Start)
          }
          SketchCard() {
            Column({ space: 12 }) {
              SectionTitle({ text: '计划时间' })
              Text(this.current().auto_organize_time)
                .fontSize(32).fontColor(SketchColors.ink)
              Text('按下方日记时区计算。修改后立即保存。')
                .fontSize(13).fontColor(SketchColors.muted)
              Button('修改时间').attributeModifier(sketchButton).drawModifier(sketchOutline())
                .backgroundColor(SketchColors.button).enabled(!this.busy)
                .onClick(() => { this.pickTime(); })
            }.width('100%').alignItems(HorizontalAlign.Start)
          }
          SketchCard() {
            Column({ space: 12 }) {
              SectionTitle({ text: '日记时区' })
              Text(`当前：${this.zoneLabel(this.current().diary_time_zone)}`)
                .fontSize(16).fontColor(SketchColors.ink)
              Text('决定记录属于哪一天，也用于计算计划时间。')
                .fontSize(13).fontColor(SketchColors.muted)
              Flex({ wrap: FlexWrap.Wrap }) {
                ForEach(ZONES, (zone: ZoneChoice) => {
                  Button(zone.label).attributeModifier(sketchButton).drawModifier(sketchOutline())
                    .margin({ right: 8, bottom: 8 })
                    .backgroundColor(zone.id === this.current().diary_time_zone ? SketchColors.button : SketchColors.paper)
                    .enabled(!this.busy)
                    .onClick(() => { this.saveZone(zone.id); })
                }, (zone: ZoneChoice) => zone.id)
              }
              if (this.deviceZone.length > 0) {
                Button('设为设备当前时区').attributeModifier(sketchButton).drawModifier(sketchOutline())
                  .backgroundColor(this.deviceZone === this.current().diary_time_zone ? SketchColors.button : SketchColors.paper)
                  .enabled(!this.busy).onClick(() => { this.saveZone(this.deviceZone); })
              }
              Text('调整时区只影响之后的记录和整理。')
                .fontSize(13).fontColor(SketchColors.muted)
            }.width('100%').alignItems(HorizontalAlign.Start)
          }
          if (this.busy) { LoadingRow({ text: '正在保存…' }) }
          if (this.message.length > 0) {
            Text(this.message).fontSize(13).fontColor(SketchColors.muted).width('100%')
          }
        }
      }.width('100%').constraintSize({ maxWidth: 760 })
        .padding({ left: 16, right: 16, top: 18, bottom: 32 })
        .alignItems(HorizontalAlign.Start)
    }.width('100%').height('100%').backgroundColor(SketchColors.canvas)
  }
}

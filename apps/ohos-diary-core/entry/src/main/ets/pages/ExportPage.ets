import { common } from '@kit.AbilityKit';
import { fileIo, picker } from '@kit.CoreFileKit';
import { ExportRequest, ExportTicket } from '../core/Models';
import { ExportStore } from '../state/ExportStore';
import { databasePath, dayKeyAtOffset } from '../ui/AppRuntime';
import { ErrorBanner, InfoRow, PageTitle } from '../ui/Common';
import { EMPTY_UI_ERROR, UiError } from '../ui/Errors';
import { SketchCard, SketchColors, sketchButton, sketchInput, sketchArea, sketchOutline } from '../ui/Sketch';

@Component
export struct ExportPage {
  @State from: string = dayKeyAtOffset(0);
  @State to: string = dayKeyAtOffset(0);
  @State format: string = 'markdown';
  @State busy: boolean = false;
  @State error: UiError = EMPTY_UI_ERROR;
  @State message: string = '';
  @State ticket: ExportTicket | null = null;
  private store: ExportStore | null = null;

  aboutToAppear(): void {
    const context = getContext(this) as common.UIAbilityContext;
    this.store = new ExportStore(databasePath(context));
  }

  private async createExport(): Promise<void> {
    if (this.busy || this.store === null) { return; }
    this.busy = true;
    const request: ExportRequest = {
      fromDayKey: this.from, toDayKey: this.to, format: this.format, includeAssets: false,
    };
    this.ticket = await this.store.createExport(request);
    this.error = this.store.error;
    this.message = this.store.message;
    this.busy = false;
  }

  private async saveCopy(): Promise<void> {
    const ticket: ExportTicket | null = this.ticket;
    if (this.busy || ticket === null) { return; }
    this.busy = true;
    try {
      const context = getContext(this) as common.UIAbilityContext;
      const view: picker.DocumentViewPicker = new picker.DocumentViewPicker(context);
      const suffix: string = ticket.format === 'plain_text' ? 'txt' : 'md';
      const firstDay: string = ticket.dayKeys.length > 0 ? ticket.dayKeys[0] : '日记';
      const lastDay: string = ticket.dayKeys.length > 0 ? ticket.dayKeys[ticket.dayKeys.length - 1] : '';
      const uris: string[] = await view.save({ newFileNames: [`光写日记-${firstDay}-${lastDay}.${suffix}`] });
      if (uris.length > 0) {
        const source: fileIo.File = await fileIo.open(ticket.path, fileIo.OpenMode.READ_ONLY);
        try {
          const target: fileIo.File = await fileIo.open(uris[0], fileIo.OpenMode.READ_WRITE | fileIo.OpenMode.TRUNC);
          try { await fileIo.copyFile(source.fd, target.fd); }
          finally { await fileIo.close(target); }
        } finally { await fileIo.close(source); }
        this.message = '日记副本已保存。';
      }
    } catch (_error) {
      this.message = '保存未完成，可重新选择位置。';
    } finally { this.busy = false; }
  }

  build() {
    Scroll() {
      Column({ space: 20 }) {
        PageTitle({ title: '阅读导出', subtitle: '按日期生成便于阅读的副本。' })
        Text('导出仅包含日记正文，不能用于恢复资料库。')
          .fontSize(15).fontColor(SketchColors.text).padding(20).backgroundColor(SketchColors.soft)
        SketchCard() {
          Column({ space: 16 }) {
            Text('开始日期').fontSize(13).fontColor(SketchColors.muted)
            TextInput({ text: this.from, placeholder: '年-月-日' }).attributeModifier(sketchInput).borderRadius(0)
              .backgroundColor(SketchColors.soft).fontColor(SketchColors.text).enabled(!this.busy)
              .onChange((text: string) => { this.from = text; })
            Text('结束日期').fontSize(13).fontColor(SketchColors.muted)
            TextInput({ text: this.to, placeholder: '年-月-日' }).attributeModifier(sketchInput).borderRadius(0)
              .backgroundColor(SketchColors.soft).fontColor(SketchColors.text).enabled(!this.busy)
              .onChange((text: string) => { this.to = text; })
            Flex({ wrap: FlexWrap.Wrap }) {
              Button('保留排版').attributeModifier(sketchButton).drawModifier(sketchOutline()).margin({ right: 8 })
                .backgroundColor(this.format === 'markdown' ? SketchColors.button : SketchColors.paper)
                .enabled(!this.busy).onClick(() => { this.format = 'markdown'; })
              Button('纯文本').attributeModifier(sketchButton).drawModifier(sketchOutline())
                .backgroundColor(this.format === 'plain_text' ? SketchColors.button : SketchColors.paper)
                .enabled(!this.busy).onClick(() => { this.format = 'plain_text'; })
            }
            Button(this.busy ? '处理中…' : '生成阅读副本').attributeModifier(sketchButton).drawModifier(sketchOutline())
              .backgroundColor(SketchColors.button).enabled(!this.busy).onClick(() => { this.createExport(); })
          }
          .width('100%').alignItems(HorizontalAlign.Start)
        }
        ErrorBanner({ error: this.error.code === 'not_found' ? EMPTY_UI_ERROR : this.error })
        if (this.ticket !== null) {
          SketchCard() {
            Column({ space: 12 }) {
              InfoRow({ label: '已导出', value: `${(this.ticket as ExportTicket).dayCount} 天` })
              Button('另存到所选位置').attributeModifier(sketchButton).drawModifier(sketchOutline()).enabled(!this.busy).onClick(() => { this.saveCopy(); })
            }.width('100%').alignItems(HorizontalAlign.Start)
          }
        }
        Text(this.message).fontSize(13).fontColor(SketchColors.muted).width('100%')
      }
      .width('100%').constraintSize({ maxWidth: 1000 }).padding(24).alignItems(HorizontalAlign.Start)
    }
    .width('100%').height('100%').backgroundColor(SketchColors.canvas)
  }
}

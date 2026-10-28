import { common } from '@kit.AbilityKit';
import { hilog } from '@kit.PerformanceAnalysisKit';
import { fileIo as fs } from '@kit.CoreFileKit';
import { promptAction } from '@kit.ArkUI';
import { SERVICE_NOTICE } from '../ui/OnlineService';
import { BundledModels, ModelProgress } from '../models/BundledModels';
import { appFilesDir } from '../session/AppDirectory';
import { SketchColors, SketchFonts, CatMark, sketchButton, sketchOutline } from '../ui/Sketch';

@Entry
@Component
struct Startup {
  @State completed: number = 0;
  @State total: number = 1;
  @State failed: boolean = false;
  @State noticeFailed: boolean = false;
  private running: boolean = false;
  private active: boolean = true;

  aboutToAppear(): void {
    this.active = true;
    this.getUIContext().getFont().registerFont({
      familyName: SketchFonts.display, familySrc: $rawfile('fonts/DiaryDisplay-AppSubset.ttf'),
    });
    this.begin();
  }

  aboutToDisappear(): void { this.active = false; }

  private async begin(): Promise<void> {
    if (this.running) { return; }
    this.noticeFailed = false;
    const context = getContext(this) as common.UIAbilityContext;
    const marker: string = `${appFilesDir(context)}/online-service-notice-v1`;
    try {
      let accepted: boolean = false;
      try { accepted = fs.accessSync(marker); } catch (_) { accepted = false; }
      if (!accepted) {
        const answer = await promptAction.showDialog({ title: '在线整理与隐私说明',
          message: SERVICE_NOTICE,
          buttons: [{ text: '退出', color: '#777777' }, { text: '同意并继续', color: '#303030' }] });
        if (answer.index !== 1) { await context.terminateSelf(); return; }
        let directoryExists: boolean = false;
        try { directoryExists = fs.accessSync(appFilesDir(context)); } catch (_) { directoryExists = false; }
        if (!directoryExists) { fs.mkdirSync(appFilesDir(context), true); }
        const file: fs.File = fs.openSync(marker, fs.OpenMode.CREATE | fs.OpenMode.READ_WRITE | fs.OpenMode.TRUNC);
        try { fs.writeSync(file.fd, '1'); } finally { fs.closeSync(file); }
      }
      if (this.active) { await this.prepare(); }
    } catch (_) {
      if (this.active) { this.noticeFailed = true; this.failed = true; }
    }
  }

  private openDiary(): void {
    this.getUIContext().getRouter().replaceUrl({ url: 'pages/Index' });
  }

  private async prepare(): Promise<void> {
    if (this.running) { return; }
    this.running = true;
    this.failed = false;
    try {
      const context = getContext(this) as common.UIAbilityContext;
      await BundledModels.prepare(context, (value: ModelProgress) => {
        if (this.active) { this.completed = value.completed; this.total = value.total; }
      }, appFilesDir(context));
      if (this.active) { this.openDiary(); }
    } catch (error) {
      hilog.error(0, 'DiaryModels', 'BUNDLED_MODELS_FAILED %{public}s', (error as Error).message);
      if (this.active) { this.failed = true; }
    } finally { this.running = false; }
  }

  build() {
    Column({ space: 22 }) {
      CatMark({ iconSize: 64 })
      Text('不写日记').fontFamily(SketchFonts.display).fontSize(36).fontColor(SketchColors.ink)
      Text(this.noticeFailed ? '说明确认未完成' : this.failed ? '本地模型准备失败' : '正在准备本地模型…').fontSize(18).fontColor(SketchColors.ink)
      Text(this.noticeFailed ? '请重试后继续使用。' : this.failed ? '请检查可用空间后重试，也可以先写下文字记录。' : '语音转写与语义检索均可离线使用，无需下载模型。')
        .fontSize(14).fontColor(SketchColors.muted).textAlign(TextAlign.Center)
      Progress({ value: this.completed, total: this.total, type: ProgressType.Linear })
        .width('85%').color(SketchColors.ink).backgroundColor(SketchColors.line)
      if (this.failed) {
        Button('重试').attributeModifier(sketchButton).drawModifier(sketchOutline()).onClick(() => { this.begin(); })
        if (!this.noticeFailed) {
          Button('先写文字记录').attributeModifier(sketchButton).drawModifier(sketchOutline()).onClick(() => { this.openDiary(); })
        }
      }
    }.width('100%').height('100%').padding(28).justifyContent(FlexAlign.Center).backgroundColor(SketchColors.canvas)
  }
}

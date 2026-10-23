import { common } from '@kit.AbilityKit';
import { hilog } from '@kit.PerformanceAnalysisKit';
import { BundledModels, ModelProgress } from '../models/BundledModels';
import { SketchColors, SketchFonts, CatMark, sketchButton, sketchOutline } from '../ui/Sketch';

@Entry
@Component
struct Startup {
  @State completed: number = 0;
  @State total: number = 1;
  @State failed: boolean = false;
  private running: boolean = false;
  private active: boolean = true;

  aboutToAppear(): void {
    this.active = true;
    this.getUIContext().getFont().registerFont({
      familyName: SketchFonts.display, familySrc: $rawfile('fonts/DiaryDisplay-AppSubset.ttf'),
    });
    this.prepare();
  }

  aboutToDisappear(): void { this.active = false; }

  private openDiary(): void {
    this.getUIContext().getRouter().replaceUrl({ url: 'pages/Index' });
  }

  private async prepare(): Promise<void> {
    if (this.running) { return; }
    this.running = true;
    this.failed = false;
    try {
      await BundledModels.prepare(getContext(this) as common.UIAbilityContext, (value: ModelProgress) => {
        if (this.active) { this.completed = value.completed; this.total = value.total; }
      });
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
      Text(this.failed ? '本地模型准备失败' : '正在准备本地模型…').fontSize(18).fontColor(SketchColors.ink)
      Text(this.failed ? '请检查可用空间后重试，也可以先写下文字记录。' : '语音转写与语义检索均可离线使用，无需下载模型。')
        .fontSize(14).fontColor(SketchColors.muted).textAlign(TextAlign.Center)
      Progress({ value: this.completed, total: this.total, type: ProgressType.Linear })
        .width('85%').color(SketchColors.ink).backgroundColor(SketchColors.line)
      if (this.failed) {
        Button('重试').attributeModifier(sketchButton).drawModifier(sketchOutline()).onClick(() => { this.prepare(); })
        Button('先写文字记录').attributeModifier(sketchButton).drawModifier(sketchOutline()).onClick(() => { this.openDiary(); })
      }
    }.width('100%').height('100%').padding(28).justifyContent(FlexAlign.Center).backgroundColor(SketchColors.canvas)
  }
}

import { ONLINE_PRIVACY } from '../ui/OnlineService';
import { SketchColors } from '../ui/Sketch';

@Component
export struct ServicePrivacyPage {
  build() {
    NavDestination() {
      Scroll() {
        Text(ONLINE_PRIVACY).fontSize(16).lineHeight(28).fontColor(SketchColors.text)
          .width('100%').padding(24).copyOption(CopyOptions.InApp)
      }.width('100%').height('100%').backgroundColor(SketchColors.canvas)
    }.title('隐私与模型服务说明', { backgroundColor: SketchColors.canvas })
  }
}

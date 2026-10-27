import { PageTitle, SectionTitle } from '../ui/Common';
import { SketchColors } from '../ui/Sketch';
import { PRIVACY_INTRODUCTION, PRIVACY_SECTIONS, PrivacySection } from '../privacy/PrivacyContent';

@Component
export struct PrivacyPage {
  build() {
    Scroll() {
      Column({ space: 20 }) {
        PageTitle({ title: '隐私政策', subtitle: '不写日记demo · 离线体验版' })
        Text(PRIVACY_INTRODUCTION).fontSize(15).lineHeight(26).fontColor(SketchColors.text)
        ForEach(PRIVACY_SECTIONS, (section: PrivacySection) => {
          Column({ space: 12 }) {
            SectionTitle({ text: section.title })
            Text(section.body).fontSize(15).lineHeight(26).fontColor(SketchColors.text)
          }.width('100%').alignItems(HorizontalAlign.Start)
        }, (section: PrivacySection) => section.title)
      }.width('100%').constraintSize({ maxWidth: 1100 }).padding(24)
        .alignItems(HorizontalAlign.Start)
    }.width('100%').height('100%').backgroundColor(SketchColors.canvas)
  }
}

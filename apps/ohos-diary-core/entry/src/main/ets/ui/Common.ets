import { SketchColors, SketchFonts, SketchType, SketchCard, sketchButton, sketchOutline } from './Sketch';
// 页面公用的小组件：状态徽标、错误条、空态、加载行、键值行。
//
// 与 Windows 共用纸色、墨色和手绘描边；状态文字保留原有语义。
// 字号用默认 fp（跟随系统字体缩放），不写死 px，也不给文本容器写死高度。

import { EMPTY_UI_ERROR, UiError } from './Errors';

/** 状态徽标：语气只影响文字颜色，底色使用共享纸色。 */
@Component
export struct StatePill {
  @Prop text: string = '';
  /** ok / warn / alert / active / muted */
  @Prop tone: string = 'muted';

  private textColor(): string {
    if (this.tone === 'ok') {
      return SketchColors.ink;
    }
    if (this.tone === 'warn') {
      return SketchColors.warning;
    }
    if (this.tone === 'alert') {
      return SketchColors.alert;
    }
    if (this.tone === 'active') {
      return SketchColors.ink;
    }
    return SketchColors.muted;
  }

  build() {
    Text(this.text)
      .fontSize(12)
      .fontColor(this.textColor())
      .backgroundColor(SketchColors.soft)
      .padding({ left: 8, right: 8, top: 3, bottom: 3 })
      .borderRadius(0)
      .maxLines(1)
      .textOverflow({ overflow: TextOverflow.Ellipsis })
  }
}

/** 错误条：中文标题与下一步建议；可选「重新加载」。 */
@Component
export struct ErrorBanner {
  @Prop error: UiError = EMPTY_UI_ERROR;
  @Prop showReload: boolean = false;
  onReload: () => void = () => {
  };

  build() {
    if (this.error.code.length > 0) {
      Column({ space: 6 }) {
        Text(this.error.title)
          .fontSize(15)
          .fontWeight(FontWeight.Medium)
          .fontColor(SketchColors.alert)
          .width('100%')
        if (this.error.message.length > 0) {
          Text(this.error.message).fontSize(14).fontColor(SketchColors.text).width('100%')
        }
        if (this.error.hint.length > 0) {
          Text(this.error.hint).fontSize(12).fontColor(SketchColors.muted).width('100%')
        }
        if (this.showReload) {
          Button('重新加载')
            .attributeModifier(sketchButton).drawModifier(sketchOutline())
            .fontSize(14)
            .backgroundColor(SketchColors.soft)
            .fontColor(SketchColors.text)
            .onClick(() => {
              this.onReload();
            })
        }
      }
      .width('100%')
      .padding(12)
      .backgroundColor(SketchColors.soft)
      .borderRadius(0)
      .alignItems(HorizontalAlign.Start)
    }
  }
}

/** 空态：图标 + 标题 + 说明，居中但不占满屏。 */
@Component
export struct EmptyState {
  @Prop icon: string = '·';
  @Prop title: string = '这里还没有内容';
  @Prop hint: string = '';

  build() {
    Column({ space: 8 }) {
      Text(this.icon)
        .fontSize(30)
        .fontColor(SketchColors.muted)
      Text(this.title)
        .fontSize(16)
        .fontWeight(FontWeight.Medium)
        .fontColor(SketchColors.text)
        .textAlign(TextAlign.Center)
      if (this.hint.length > 0) {
        Text(this.hint)
          .fontSize(13)
          .fontColor(SketchColors.muted)
          .textAlign(TextAlign.Center)
      }
    }
    .width('100%')
    .padding({ left: 24, right: 24, top: 40, bottom: 40 })
    .justifyContent(FlexAlign.Center)
  }
}

/** 加载行。 */
@Component
export struct LoadingRow {
  @Prop text: string = '加载中…';

  build() {
    Row({ space: 10 }) {
      LoadingProgress()
        .width(20)
        .height(20)
        .color(SketchColors.ink)
      Text(this.text)
        .fontSize(13)
        .fontColor(SketchColors.muted)
    }
    .width('100%')
    .justifyContent(FlexAlign.Center)
    .padding(16)
  }
}

/** 键值行：左标签固定宽，右值可换行。 */
@Component
export struct InfoRow {
  @Prop label: string = '';
  @Prop value: string = '';

  build() {
    Row({ space: 10 }) {
      Text(this.label)
        .fontSize(13)
        .fontColor(SketchColors.muted)
        .width(92)
        .flexShrink(0)
      Text(this.value.length > 0 ? this.value : '—')
        .fontSize(13)
        .fontColor(SketchColors.text)
        .layoutWeight(1)
    }
    .width('100%')
    .alignItems(VerticalAlign.Top)
    .padding({ top: 4, bottom: 4 })
  }
}

/** 小节标题。 */
@Component
export struct SectionTitle {
  @Prop text: string = '';

  build() {
    Text(this.text)
      .fontSize(SketchType.section)
      .fontFamily(SketchFonts.display)
      .fontWeight(FontWeight.Medium)
      .fontColor(SketchColors.text)
      .width('100%')
      .margin({ top: 8, bottom: 4 })
  }
}

/** 页面大标题 + 可选说明。 */
@Component
export struct PageTitle {
  @Prop title: string = '';
  @Prop subtitle: string = '';

  build() {
    Column({ space: 4 }) {
      Text(this.title)
        .fontSize(SketchType.pageTitle)
        .fontFamily(SketchFonts.display)
        .fontWeight(FontWeight.Bold)
        .fontColor(SketchColors.text)
        .width('100%')
      if (this.subtitle.length > 0) {
        Text(this.subtitle)
          .fontSize(13)
          .fontColor(SketchColors.muted)
          .width('100%')
      }
    }
    .width('100%')
    .alignItems(HorizontalAlign.Start)
  }
}

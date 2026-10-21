// 页面公用的小组件：状态徽标、错误条、空态、加载行、键值行。
//
// 颜色全部来自系统资源（`sys.color.ohos_id_color_*`），深色模式跟随系统；
// 字号用默认 fp（跟随系统字体缩放），不写死 px，也不给文本容器写死高度。

import { EMPTY_UI_ERROR, UiError } from './Errors';

/** 状态徽标：语气只影响文字颜色，底色统一用系统次级背景。 */
@Component
export struct StatePill {
  @Prop text: string = '';
  /** ok / warn / alert / active / muted */
  @Prop tone: string = 'muted';

  private textColor(): Resource {
    if (this.tone === 'ok') {
      return $r('sys.color.ohos_id_color_emphasize');
    }
    if (this.tone === 'warn') {
      return $r('sys.color.ohos_id_color_warning');
    }
    if (this.tone === 'alert') {
      return $r('sys.color.ohos_id_color_alert');
    }
    if (this.tone === 'active') {
      return $r('sys.color.ohos_id_color_emphasize');
    }
    return $r('sys.color.ohos_id_color_text_secondary');
  }

  build() {
    Text(this.text)
      .fontSize(12)
      .fontColor(this.textColor())
      .backgroundColor($r('sys.color.ohos_id_color_sub_background'))
      .padding({ left: 8, right: 8, top: 3, bottom: 3 })
      .borderRadius(8)
      .maxLines(1)
      .textOverflow({ overflow: TextOverflow.Ellipsis })
  }
}

/** 错误条：标题 + 核心原话 + 下一步建议；可选「重新加载」。 */
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
          .fontColor($r('sys.color.ohos_id_color_alert'))
          .width('100%')
        Text(this.error.message)
          .fontSize(14)
          .fontColor($r('sys.color.ohos_id_color_text_primary'))
          .width('100%')
        Text(this.error.hint)
          .fontSize(12)
          .fontColor($r('sys.color.ohos_id_color_text_secondary'))
          .width('100%')
        if (this.error.code.length > 0) {
          Text(`错误码：${this.error.code}${this.error.native ? '（调用没有发生）' : ''}`)
            .fontSize(11)
            .fontColor($r('sys.color.ohos_id_color_text_tertiary'))
            .width('100%')
        }
        if (this.showReload) {
          Button('重新加载')
            .fontSize(14)
            .height(34)
            .backgroundColor($r('sys.color.ohos_id_color_sub_background'))
            .fontColor($r('sys.color.ohos_id_color_text_primary'))
            .onClick(() => {
              this.onReload();
            })
        }
      }
      .width('100%')
      .padding(12)
      .backgroundColor($r('sys.color.ohos_id_color_alert_transparent'))
      .borderRadius(12)
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
        .fontColor($r('sys.color.ohos_id_color_text_tertiary'))
      Text(this.title)
        .fontSize(16)
        .fontWeight(FontWeight.Medium)
        .fontColor($r('sys.color.ohos_id_color_text_primary'))
        .textAlign(TextAlign.Center)
      if (this.hint.length > 0) {
        Text(this.hint)
          .fontSize(13)
          .fontColor($r('sys.color.ohos_id_color_text_secondary'))
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
        .color($r('sys.color.ohos_id_color_emphasize'))
      Text(this.text)
        .fontSize(13)
        .fontColor($r('sys.color.ohos_id_color_text_secondary'))
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
        .fontColor($r('sys.color.ohos_id_color_text_secondary'))
        .width(92)
        .flexShrink(0)
      Text(this.value.length > 0 ? this.value : '—')
        .fontSize(13)
        .fontColor($r('sys.color.ohos_id_color_text_primary'))
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
      .fontSize(14)
      .fontWeight(FontWeight.Medium)
      .fontColor($r('sys.color.ohos_id_color_text_primary'))
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
        .fontSize(28)
        .fontWeight(FontWeight.Bold)
        .fontColor($r('sys.color.ohos_id_color_text_primary'))
        .width('100%')
      if (this.subtitle.length > 0) {
        Text(this.subtitle)
          .fontSize(13)
          .fontColor($r('sys.color.ohos_id_color_text_secondary'))
          .width('100%')
      }
    }
    .width('100%')
    .alignItems(HorizontalAlign.Start)
  }
}

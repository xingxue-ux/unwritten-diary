@Entry
@Component
struct Index {
  @State message: string = 'OHOS core smoke probe';

  build() {
    Column() {
      Text(this.message)
        .fontSize(24)
        .fontWeight(FontWeight.Bold)
      Text('see hilog tag DiaryCore')
        .fontSize(14)
        .margin({ top: 12 })
    }
    .width('100%')
    .height('100%')
    .justifyContent(FlexAlign.Center)
  }
}
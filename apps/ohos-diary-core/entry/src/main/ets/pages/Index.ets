/**
 * 示例页面：只说明结果在哪里。
 *
 * 真机没连着，所以这里不做「点击重跑」之类的交互——避免写一段没人跑过的 UI 代码。
 * 需要手动重跑时，在 EntryAbility.onWindowStageCreate 里再调一次 DiaryCoreSmoke.run()。
 */
@Entry
@Component
struct Index {
  @State message: string = 'Diary Core · ArkTS NAPI';

  build() {
    Column() {
      Text(this.message)
        .fontSize(24)
        .fontWeight(FontWeight.Bold)
      Text('结果见 hilog：hdc shell "hilog -x" | grep DiaryCore')
        .fontSize(14)
        .margin({ top: 12 })
      Text('每一步都是 DIARY_SMOKE JSON[label][i/n] 分片')
        .fontSize(12)
        .margin({ top: 8 })
    }
    .width('100%')
    .height('100%')
    .justifyContent(FlexAlign.Center)
  }
}

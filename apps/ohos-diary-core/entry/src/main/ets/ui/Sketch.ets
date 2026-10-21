// 手绘风视觉令牌 —— 鸿蒙端的唯一来源（后面三条页面线都 import 这里，不要另写颜色）。
//
// 对照 Windows：`apps/diary_app/lib/src/sketch_ui.dart`。9 个颜色逐字符对齐（含大小写），
// 字号层级按 Windows 的实际用法收敛成 5 档。改色/改字号请两端一起改；跨端视觉漂移
// 没有检查脚本能拦，只能靠这条注释和 Code Review。
//
// ── 本版取舍：不做手绘抖动描边 ────────────────────────────────────────────────
// Windows 的 `SketchFrame` 在 1.8vp `ink` 边框上按正弦采样抖动，再叠一条 16% 透明的
// 重影线（`sketch_ui.dart:36-80`）。ArkTS 要复刻得用 `Canvas` 逐点 Path，还要监听尺寸
// 变化重绘，代价大、收益小；而当前阶段三条页面线要并行往里填页面，更需要稳定的骨架。
// 所以本版只保留「`paper` 底 + `line` 1vp 实线 + `borderRadius(0)` 直角」，抖动描边
// 留作后续单独 PR。**在那之前不要给卡片加圆角/阴影**——那会让两端更像两个 App。
//
// ── 字体 ─────────────────────────────────────────────────────────────────────
// Windows 用 `DiaryDisplay` → `Microsoft YaHei` → `Noto Sans CJK SC`；鸿蒙上这三者都
// 不可用（授权字体 / 未随 HAP 打包）。这里不提供 fontFamily 令牌，一律走系统默认的
// HarmonyOS Sans：字号、字重、颜色与 Windows 一致，只有字形不同。

/** 手绘风配色。值与 `sketch_ui.dart` 的 `SketchColors` 一一对应。 */
export class SketchColors {
  /** 页面底：整个壳的底色。 */
  static readonly canvas: string = '#F8F7F2';
  /** 卡片 / 面板底。 */
  static readonly paper: string = '#FFFEFA';
  /** 悬停 / 次级底。 */
  static readonly soft: string = '#F1F0EA';
  /** 次按钮底。 */
  static readonly button: string = '#EAE8E0';
  /** 主文字、描边。 */
  static readonly ink: string = '#303331';
  /** 正文。 */
  static readonly text: string = '#454541';
  /** 次要文字（未选中导航、说明、字数）。 */
  static readonly muted: string = '#62645E';
  /** 分隔线。 */
  static readonly line: string = '#D8D9D2';
  /** 强调（标签、选中态点缀）。 */
  static readonly blush: string = '#D9C1B3';
}

/**
 * 字号层级（单位 fp，数值与 Windows 对齐）。
 *
 * 按用途命名而不是按大小：接页面时先问「这块内容是什么」，再挑 token，
 * 避免又冒出 14 / 17 / 18 这种随手写的中间号。
 */
export class SketchType {
  /** 应用标题「不写日记」。Windows 桌面 `fontSize: 33`（`experience_shell.dart:577`）。 */
  static readonly display: number = 33;
  /** 页面标题（记录 / 日记 / 搜索…）。 */
  static readonly pageTitle: number = 20;
  /** 小节标题、顶部导航项。 */
  static readonly section: number = 15;
  /** 正文。 */
  static readonly body: number = 13;
  /** 次要说明、字数、标语。 */
  static readonly caption: number = 12;
}

/** `SketchCard` 的默认子内容：ArkTS 的 `@BuilderParam` 必须有初值。 */
@Builder
function SketchCardEmpty() {
}

/**
 * 手绘风卡片容器：`paper` 底 + `line` 1vp 边框 + **直角**，默认内边距 20。
 *
 * 用法（尾随闭包）：
 * ```
 * SketchCard() {
 *   Text('标题').fontSize(SketchType.section).fontColor(SketchColors.ink)
 * }
 * ```
 *
 * 为什么是 `@Component` + `@BuilderParam` 而不是全局 `@Builder` 函数：ArkTS 的全局
 * `@Builder` 收不到「子内容」，而卡片必须能包任意内容；`@BuilderParam` 是唯一等价物，
 * 调用形式仍然是尾随闭包。手绘抖动描边未实现，原因见文件头。
 */
@Component
export struct SketchCard {
  /** 卡片内容。 */
  @BuilderParam content: () => void = SketchCardEmpty;
  /** 内边距，默认 20（与 Windows `SketchFrame` 的 `EdgeInsets.all(20)` 一致）。 */
  @Prop contentPadding: number = 20;

  build() {
    Column() {
      this.content()
    }
    .width('100%')
    .alignItems(HorizontalAlign.Start)
    .padding(this.contentPadding)
    .backgroundColor(SketchColors.paper)
    .border({ width: 1, color: SketchColors.line })
    .borderRadius(0)
  }
}
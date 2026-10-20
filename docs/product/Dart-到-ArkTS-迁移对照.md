# Dart → ArkTS 迁移对照（本仓库专用）

对应 issue #108（鸿蒙化）· 基线 `origin/main` = `917bf22` · 2026-10-19

口径同仓库其它文档：**实测** = 本机跑过并贴输出；**读码** = 读源码确认；**未验证** = 明确标注；
**判断** = 推断。

**这份文档回答一个问题**：`apps/diary_app` 那 6160 行 Dart，怎么变成鸿蒙能跑的东西。

---

## 1 先说结论

**不迁移，重写；而且只重写视觉与页面。**

三句话：

1. **不能直接跑 Flutter**——不是"麻烦"，是**版本代差**卡死（见 §1.1）；
2. **没有可靠的 Dart → ArkTS 自动转换器**。能转语法的转不了语义，转出来的东西既不像 Dart 也不像 ArkTS；
3. **真正的迁移量只有 343 行 + 页面布局**——业务逻辑 1828 行（controller 层）**不迁**，因为鸿蒙侧已经重写过一遍（见 §2）。

### 1.1 为什么 A 路线（Flutter 鸿蒙分支）走不通

文档里已有记录，不是本文新判断：

> `docs/architecture/m6-鸿蒙可行性探针.md:19`
> Q4 界面走 A（Flutter 鸿蒙分支）还是 B（ArkTS 重写）→ **推荐 B**。A 的硬伤是**版本代差**：
> 本仓库 CI 钉 `Flutter 3.44.6` + `Dart >=3.9.0` + `FRB 2.13.0`，而鸿蒙 Flutter 分支最新只到
> **3.27.4（Dart 3.6）**；FRB 的鸿蒙 fork 与 2.13.0 是否兼容未验证。

翻译成人话：本仓库的核心通道是 `flutter_rust_bridge 2.13.0` 生成的，它生成的 Dart 代码
**要求 Dart 3.9**；鸿蒙 Flutter 分支停在 Dart 3.6。不是"适配一下"能解决的——
`packages/diary_bridge/lib/src/rust/frb_generated.dart` 是工具生成的，改不动也没法降级。

**这一条随时间可能变**：若上游把鸿蒙分支推到 Dart 3.9+ 且 FRB 2.13 能编，A 路线应重新评估。
在那之前，B 是唯一可行路径。

---

## 2 迁移范围：逐文件判定

`apps/diary_app/lib` 总计 **6160 行**（`find … | xargs wc -l`，实测）。

| 文件 | 行数 | 判定 | 理由 |
|---|---:|---|---|
| `experience_shell.dart` | 2556 | **要，但重构式** | 巨型单文件 StatefulWidget；鸿蒙侧已拆成 7 个 Page，**不逐行翻**（见 §7） |
| `sketch_ui.dart` | 343 | **要，近乎 1:1** | 纯绘制（3 个 `CustomPainter`），零业务、零网络、零状态机——**唯一能机械迁移的部分** |
| `theme.dart` | 46 | **要，但大部分可丢** | 实测 `colorScheme.*` 被引用 **0 次**，Material 3 派生色没人用（见 §6） |
| `capture_controller.dart` | 338 | 不迁 | 调核心 + 管界面状态；鸿蒙侧 `CoreSession`/`AppState` 已重写 |
| `library_care_controller.dart` | 335 | 不迁 | 同上 |
| `file_import_controller.dart` | 343 | 不迁 | 同上（平台能力差异见 §3.4） |
| `style_settings_controller.dart` | 126 | 不迁 | 同上 |
| `readable_export_controller.dart` | 139 | 不迁 | 同上 |
| `revision_controller.dart` | 209 | 不迁 | 同上 |
| `search_controller.dart` | 188 | 不迁 | 同上 |
| `core_wakeup.dart` | 195 | 不迁 | 核心句柄生命周期，鸿蒙侧 `CoreGateway` 已有 |
| `app_services.dart` | 47 | 不迁 | 依赖注入底座，两边形态不同 |
| `file_import_journal.dart` | 41 | 不迁 | 导入日志私有格式 |
| `revision_diff.dart` | 76 | **可能要** | 纯文本 diff 算法（无 Flutter 依赖的话可直接重写；未核） |
| `style_settings_panel.dart` / `readable_export_panel.dart` / `library_care_panel.dart` | 285+188+484 | **要**（属页面层） | 是 UI 面板，跟 `experience_shell` 的 panel 同性质 |

**总数核对**：2556 + 343 + 46 + 1828（controller 合计）+ 957（三个 panel）+ 117（journal/services/diff）
= 5847，余下为 `main.dart` 等入口文件。

### 2.1 为什么 controller 层绝不迁

controller 的职责是「调核心 + 管界面状态」。这件事**鸿蒙侧已经做过一遍**：

| Dart | 鸿蒙（PR #150，未合入） |
|---|---|
| `app_services.dart` | `ui/AppRuntime.ets` |
| `core_wakeup.dart` | `ui/CoreGateway.ets` + `ui/CoreSession.ets`（含 TaskPool 跨线程） |
| 各 controller 的 `ValueNotifier` 状态 | `ui/AppState.ets` |

再迁一遍 Dart controller = 维护两套业务胶水，必然漂移，且两边都要跟着核心改。
**这是架构决定的**：核心是 Rust，前端是薄壳；薄壳应该各写各的、语义一致，而不是共享代码。

---

## 3 对应表

### 3.1 语言与类型

| Dart | ArkTS | 备注 |
|---|---|---|
| `Color(0xFFF8F7F2)` | `'#F8F7F2'` | ArkTS 用字符串或 `ResourceColor` |
| `ink.withValues(alpha: .16)` | `'rgba(48,51,49,0.16)'` | **alpha 要自己算**（见 §5.5） |
| `EdgeInsets.all(20)` | `.padding(20)` | |
| `EdgeInsets.fromLTRB(46,35,46,40)` | `.padding({left:46,top:35,right:46,bottom:40})` | |
| `BorderRadius.circular(14)` | `.borderRadius(14)` | |
| `abstract final class SketchColors` | `export class SketchColors { static readonly canvas: string = '#F8F7F2'; }` | ArkTS 没有 `abstract final class` |
| `double` / `int` | `number` | 无 `double`/`int` 之分 |
| `dynamic` | **不支持** | ArkTS 禁 `any`/`unknown`；Dart 的隐式 downcast 在这里直接编不过 |
| `T?` / `T!` | `T \| undefined` + 显式判空 | ArkTS 严格空安全，但没有 `!` |
| 级联 `paint..a = 1..b = 2` | 拆成多行赋值 | 没有级联语法 |

### 3.2 组件与状态

| Flutter | ArkTS |
|---|---|
| `class X extends StatelessWidget` | `@Component struct X` |
| `class X extends StatefulWidget` + `_XState` | `@Component struct X { @State … }` |
| `setState(() => _x = 1)` | `this.x = 1`（`@State` 自动触发刷新） |
| `build()` 里算派生值 | `@Computed` 或状态变化时算好存 `@State`（见 §5.4） |
| `initState()` | `aboutToAppear()` |
| `dispose()` | `aboutToDisappear()` |
| `Semantics(label: '小猫线条图标')` | `.accessibilityText('小猫线条图标')` |
| `LayoutBuilder` | `onAreaChange` |
| `AnimatedBuilder` | `@Watch` 或直接在状态回调里重绘 |
| `PageStorageKey` | 无直接对应；用 `@State` 自己保存滚动位置 |

### 3.3 绘制与动画

| Flutter | ArkTS |
|---|---|
| `CustomPaint(painter: P())` | `Canvas(this.ctx)` + `onReady` 里画 |
| `CanvasRenderingContext2D` | 同名（`new CanvasRenderingContext2D(settings)`） |
| `Paint()..color=..strokeWidth=..style=stroke` | `ctx.strokeStyle/fillStyle` + `ctx.lineWidth` |
| `Path()..moveTo(..)` | `ctx.beginPath(); ctx.moveTo(..)` |
| `lineTo` | `lineTo` |
| `quadraticBezierTo(x1,y1,x2,y2)` | `quadraticCurveTo(cpx,cpy,x,y)` — **参数顺序一致** |
| `cubicTo(x1,y1,x2,y2,x,y)` | `bezierCurveTo(cp1x,cp1y,cp2x,cp2y,x,y)` — 一致 |
| `close()` | `closePath()` |
| `drawPath(p, paint)` | 设样式后 `ctx.stroke()` 或 `ctx.fill()` |
| `drawCircle(Offset(x,y), r, paint)` | `ctx.beginPath(); ctx.arc(x,y,r,0,2*Math.PI);` 然后 `fill()`/`stroke()` |
| `drawLine(a, b, paint)` | `beginPath(); moveTo(a); lineTo(b); stroke();` |
| `canvas.save/restore/translate/scale` | 同名 |
| `StrokeCap.round` | `ctx.lineCap = 'round'` |
| `StrokeJoin.round` | `ctx.lineJoin = 'round'` |
| `AnimationController(duration: 1100ms)` + `repeat(reverse: true)` | `animateTo({ duration: 1100, iterations: -1, playMode: PlayMode.Reverse }, …)` |
| `MediaQuery.disableAnimationsOf(context)` | 系统「减少动态效果」开关（`未验证`：鸿蒙对应开关的读取方式**没有核实**，见 §11） |
| `shouldRepaint()` | **没有对应物**（见 §5.3） |

### 3.4 平台能力（替换 `dart:` 包和插件）

| Dart 侧用法 | 出处 | ArkTS |
|---|---|---|
| `XFile` / `file_picker` | `experience_shell.dart:205,214` | `picker.DocumentViewPicker` |
| `showModalBottomSheet` | `experience_shell.dart:1236` | `bindSheet` |
| `ScaffoldMessenger.showSnackBar` | `experience_shell.dart:426` | `promptAction.showToast` |
| `path_provider` | 全仓库 | `context.filesDir` |
| `flutter_timezone` | 录音时区 | `i18n.System.getTimezone()`（`未验证`） |
| `ThemeData` / Material 3 | `theme.dart` | 常量（见 §6） |
| `fontFamily: 'Microsoft YaHei'` | `theme.dart:14` | **不能照搬**（见 §5.7） |

---

## 4 完整对照：`CatMark`（真实代码）

选它是因为它同时覆盖了路径、圆、线、样式切换四类原语。

### Flutter（`apps/diary_app/lib/src/sketch_ui.dart:177-213`）

```dart
canvas.scale(size.width / 100, size.height / 100);
final ink = Paint()
  ..color = SketchColors.ink          // #303331
  ..style = PaintingStyle.stroke
  ..strokeWidth = 3.4
  ..strokeCap = StrokeCap.round
  ..strokeJoin = StrokeJoin.round;
final face = Path()
  ..moveTo(22, 54) ..lineTo(22, 27) ..lineTo(29, 17) ..lineTo(39, 26)
  ..quadraticBezierTo(50, 22, 61, 26)
  ..lineTo(72, 17) ..lineTo(78, 27) ..lineTo(78, 54)
  ..cubicTo(78, 76, 22, 76, 22, 54);
canvas.drawPath(face, ink);
final dot = Paint()..color = SketchColors.ink;
canvas.drawCircle(const Offset(40, 51), 2.6, dot);
canvas.drawCircle(const Offset(60, 51), 2.6, dot);
canvas.drawCircle(const Offset(50, 60), 2.6, dot);
canvas.drawLine(const Offset(17, 84), const Offset(83, 84), ink..strokeWidth = 2.1);
```

### ArkTS

```ts
const s: number = this.size / 100;
this.ctx.save();                        // transform 会累积，必须包起来
this.ctx.scale(s, s);

this.ctx.strokeStyle = SketchColors.ink;
this.ctx.lineWidth = 3.4;
this.ctx.lineCap = 'round';
this.ctx.lineJoin = 'round';
this.ctx.beginPath();
this.ctx.moveTo(22, 54); this.ctx.lineTo(22, 27);
this.ctx.lineTo(29, 17); this.ctx.lineTo(39, 26);
this.ctx.quadraticCurveTo(50, 22, 61, 26);
this.ctx.lineTo(72, 17); this.ctx.lineTo(78, 27);
this.ctx.lineTo(78, 54);
this.ctx.bezierCurveTo(78, 76, 22, 76, 22, 54);
this.ctx.stroke();

this.ctx.fillStyle = SketchColors.ink;
for (const p of [[40, 51], [60, 51], [50, 60]]) {
  this.ctx.beginPath();
  this.ctx.arc(p[0], p[1], 2.6, 0, 2 * Math.PI);
  this.ctx.fill();
}

this.ctx.lineWidth = 2.1;               // ← 原码是 ink..strokeWidth = 2.1，必须拆开
this.ctx.beginPath();
this.ctx.moveTo(17, 84); this.ctx.lineTo(83, 84);
this.ctx.stroke();

this.ctx.restore();
```

**这段对照里藏了两个坑**，都在 §5：
- `ink..strokeWidth = 2.1` → 级联求值，ArkTS 必须**先赋 `lineWidth` 再画**（§5.1）；
- `save/restore` 不是可选的（§5.2）。

---

## 5 七个坑（全部来自本仓库真实代码）

### 5.1 Flutter 是「路径和样式一起传」，ArkTS 是「样式设在上下文上」

```dart
// Flutter：样式是绘制调用的参数，可以在调用处临时改
canvas.drawLine(a, b, ink..strokeWidth = 2.1);
```

`ink..strokeWidth = 2.1` 是 Dart 的级联——**先改对象，再把它当参数传**。ArkTS 没有级联，
`ctx.lineWidth` 是**上下文状态**，必须：

```ts
this.ctx.lineWidth = 2.1;   // 先改
this.ctx.stroke();          // 再画
```

**逐行翻译时最容易错的地方就在这里**：照抄成 `stroke(ink)` 之类的写法会直接编不过，
而"差不多改成 ctx 属性"又容易把顺序搞反——先 `stroke()` 后改 `lineWidth`，画出来的就是旧线宽。

### 5.2 Canvas 的 transform 会累积

Flutter 每次 `paint()` 拿到的 `canvas` 是**新的**，`canvas.scale()` 用完就随 canvas 一起丢。
ArkTS 的 `ctx` 是**长期存在**的对象，`scale/translate` 会**一直累积**，第二次重绘时会变成 `s²` 倍。

**规则**：每次重绘必须 `ctx.save()` → `clearRect` → 画 → `ctx.restore()`。
本仓库三个 painter 都调了 `canvas.scale`（`sketch_ui.dart:181,267`），全部会踩这个坑。

### 5.3 `shouldRepaint` 没有对应物——画布不会自己重画

Flutter 靠 `shouldRepaint(old) => old.progress != progress`（`sketch_ui.dart:341-343`）决定要不要重画。

ArkTS 里**没有任何东西帮你判断**：`@State` 变了会让 `build` 重跑，但 `Canvas` 的**内容不会自动重画**。
你必须在状态变化的回调里手动 `clearRect` + 重画，否则画面停在上一次的内容上。

**这是最典型的「看起来对、实际不动」故障**——代码齐、编译过、界面就是不动。

### 5.4 `build()` 里算派生值，在 ArkTS 里是反模式

Flutter 每次全量重建 widget 树，随手在 `build()` 里 `list.where(...)` 完全没问题。
ArkTS 是状态驱动：有些生命周期里组件**根本不会重新执行**，写在 `build` 里的计算不会因为
你"以为它每次都会跑"而真的每次跑。

**规则**：`@Computed`（派生状态）或状态变化时算好存进 `@State`。

### 5.5 alpha 要从颜色值里算出来

```dart
final ghost = Paint()..color = SketchColors.ink.withValues(alpha: .16);
```

`ink = 0xFF303331` = rgb(48, 51, 49)。ArkTS 没有 `withValues`，要么写 `rgba(48,51,49,0.16)`，
要么自己写个 `withAlpha(hex, a)` 工具函数——**别手抄错 rgb**。

本仓库出现 alpha 的地方：`sketch_ui.dart:60`（`.16`）、`:61`（线宽 `.7`）。

### 5.6 行高语义不同：倍数 vs vp

```dart
textTheme: const TextTheme(
  bodyLarge: TextStyle(color: SketchColors.text, height: 1.6),   // 倍数
  bodyMedium: TextStyle(color: SketchColors.text, height: 1.5),
)
```

Flutter 的 `height` 是**字号倍数**（1.6 × fontSize）。
ArkTS 的 `lineHeight` 是**绝对长度**（vp）。

**必须乘**：`lineHeight: fontSize * 1.6`。直接照抄成 `lineHeight: 1.6` 会让行高变成 1.6vp，
文字会叠在一起——这是那种"一眼就能看出不对，但看不出为什么"的错。

### 5.7 字体栈是 Windows 专用

```dart
fontFamily: 'Microsoft YaHei',
fontFamilyFallback: const ['Noto Sans CJK SC', 'sans-serif'],
```

`theme.dart:14-15`。**这两个字体鸿蒙上都没有**。照搬会全部 fallback 到 `sans-serif`，
字重和字宽都会变，手绘风的排版比例就散了。

鸿蒙上应该用 `HarmonyOS Sans SC`（系统字体）作为正文，`DiaryDisplay` 只用于标题
（且它是**子集字体**，只含部分字形，正文用它会大面积 fallback，见 §11）。

### 5.8 动画的无障碍分支不能丢

```dart
if (MediaQuery.disableAnimationsOf(context)) {
  _motion.stop();
  _motion.value = .5;          // 关动画时停在中间帧
} else if (!_motion.isAnimating) {
  _motion.repeat(reverse: true);
}
```

`sketch_ui.dart:239-244`。**用户关掉系统动画时，猫停在「正在写」的中间帧**，而不是消失或停在起点。

这个分支很容易在移植时被当成"装饰"丢掉。它不是装饰——它是无障碍要求。
（`未验证`：鸿蒙读「减少动态效果」开关的方式没有核实，见 §11）

---

## 6 `theme.dart` 怎么迁：好消息

**实测**：`grep -o "colorScheme\.[a-zA-Z]*" apps/diary_app/lib/src/*.dart` → **0 命中**。

也就是说 `ColorScheme.fromSeed(seedColor: SketchColors.ink, …)` 生成的整个 Material 3
配色方案，**界面一处都没用**。所有颜色都直接取 `SketchColors.*`：

| 颜色 | 用法次数 |
|---|---:|
| `ink` #303331 | 48 |
| `muted` #62645E | 40 |
| `text` #454541 | 13 |
| `line` #D8D9D2 | 13 |
| `soft` #F1F0EA | 11 |
| `paper` #FFFEFA | 7 |
| `canvas` #F8F7F2 | 3 |
| `blush` #D9C1B3 | 3 |
| `button` #EAE8E0 | 2 |

**结论：不需要在 ArkTS 里复刻 Material 3 的 seed 派生**（那本来也做不到）。要迁的只有
9 个常量 + 下面 5 个 `ThemeData` 覆盖：

| Flutter | 出处 | ArkTS 做法 |
|---|---|---|
| `scaffoldBackgroundColor: canvas` | `theme.dart:12` | 根容器 `.backgroundColor(SketchColors.canvas)` |
| `bodyLarge/bodyMedium` 的 `color` + `height` | `:16-19` | 正文 `.fontColor(text).lineHeight(16*1.6)` |
| `TextButton` foreground = `ink` | `:20-21` | 按钮 `.fontColor(SketchColors.ink)` |
| `inputDecorationTheme.border: none` | `:23-26` | 输入框 `.borderWidth(0)` |
| `chipTheme`：`paper`/`button` + **1px ink 边 + 直角** | `:27-35` | `.backgroundColor().border({width:1,color:ink}).borderRadius(0)` |
| `snackBarTheme`：`ink` 底 + `paper` 字 | `:36-39` | `promptAction.showToast` 的样式（能力有限，见 §11） |

---

## 7 工序（按这个顺序做，返工最少）

### 第 1 步：`sketch_ui.dart`（343 行）

**唯一能近乎 1:1 迁移的部分**：3 个 `CustomPainter`、零业务、零网络、零状态机。
产出可复用组件：`SketchColors` / `SketchFrame` / `CatMark` / `WritingCat`。

它不依赖任何页面，可以在页面还没写之前就把视觉底座定下来。

### 第 2 步：`theme.dart`（46 行）

按 §6，实际只需 9 个常量 + 5 条覆盖。

### 第 3 步：页面——**重构式，不逐行翻**

`experience_shell.dart` 是 **2556 行的单个 StatefulWidget**，里面有 5 个页面、10 个 panel、
2 个详情页、若干工具方法。鸿蒙侧已经把同样的功能拆成 7 个 `Page`（`apps/ohos-diary-core/entry/src/main/ets/pages/`）。

**硬翻的结果是一个 2556 行的 ArkTS 文件**，比现在的 7 个页面差得多，而且 ArkTS 的严格类型
会把"一个文件塞满所有状态"的做法放大成灾难（所有状态都得是同一个 `@State` 组的成员，
任何一处变化都会让整个文件重跑）。

**做法**：按「tab → 页面 → 区域 → 组件」重新组织，每迁一个区域就对照一次（§8）。

### 第 4 步：controller —— **不迁**

见 §2.1。

---

## 8 页面结构对照

`experience_shell.dart` 的真实分区（读码，行号为 `917bf22`）：

| 类别 | Flutter 方法 | 行号 | 鸿蒙对应 |
|---|---|---:|---|
| 骨架 | `build`（`LayoutBuilder`，`desktop = width >= 850`） | 431 | `Index.ets` |
| 顶栏 | `_header` | 503 | 各 `Page` 自带头部 |
| 底部导航 | `_bottomNav` / `_navItem`（5 个） | 599 / 615 | `Index.ets` 的 `Tabs`（现 4 个，待改） |
| 路由 | `_currentPage` | 651 | `NavPathStack` |
| **tab 0 记录** | `_recordPage` / `_editorCard` / `_saveStatus` | 963 / 1052 / 1169 | `RecordingPage` + `CaptureDetailPage` |
| **tab 1 日记** | `_diaryPage` / `_timelineHeader` / `_timelineFooter` | 1275 / 1546 / 1563 | `TodayPage`（部分） |
| **tab 2 搜索** | `_searchScroll` / `_coreSearchScroll` / `_coreSearchHeader` / `_searchHeader` / `_resultTile` | 672 / 727 / 800 / 1392 / 1506 | `SearchPage` |
| **tab 3 片段** | `_timelineScroll` | 877 | **缺失**（待补） |
| **tab 4 我的** | `_profilePage` / `_section` | 1601 / 1649 | `SettingsPage` |
| 面板路由 | `_panelPage` / `_openPanel` | 1711 / 328 | 各 `NavDestination` |
| panel | `_versionsPanel` / `_editPanel` / `_responsePanel` / `_tasksPanel` / `_modelPanel` / `_schedulePanel` / `_pluginsPanel` / `_recordingPanel` / `_coverPanel` / `_loadingPanel` | 1751–2175 | `JobsPage` / `TranscriptionConfigPage` / 其余**缺失** |
| 详情 | `_detailPage` / `_revisionDetail` / `_revisionDiffCard` | 2200 / 2322 / 2505 | `CaptureDetailPage` |
| 通用 | `_title` / `_notice` / `_importCard` / `_toast` | 919 / 945 / 271 / 426 | 各页内联 |

**这张表就是鸿蒙侧的功能缺口清单**：tab 3「片段」、6 个 panel 没有对应页。
（信息架构的正式契约见 `docs/product/界面结构契约.md`——若该文档尚未合入，以本文 §8 为临时依据。）

---

## 9 验证标准

**不设「像素级一致」这种做不到的指标。** 只做三件可复核的事：

### 9.1 截图对照

同一份数据、同一屏，两边各截一张，并排贴进 PR。走查清单（每项打勾或记差异）：

- [ ] 纸面色 `paper` / 画布色 `canvas` 是否同一色值
- [ ] 墨色 `ink` 的描边是否同一深度
- [ ] `SketchFrame` 边框线宽（默认 `weight=1.8`）
- [ ] 抖动的**幅度与相位**是否同形（公式见 `sketch_ui.dart:68-71`）
- [ ] 圆角、卡片间距、页面内边距
- [ ] 字号阶梯与行高比例
- [ ] `CatMark` 的猫脸比例与三点位置

### 9.2 交互路径对照

同一操作序列，两边最终状态与**提示文案**一致：

录音 → 停止 → 自动提交 → 删除（进回收站）→ 恢复 → 改正文（版本 +1）→ 导出。
每步记录：界面显示的状态词、错误提示、能否撤销。

### 9.3 无障碍对照

| 项 | Flutter | ArkTS |
|---|---|---|
| 小猫图标有可读名称 | `Semantics(label: '小猫线条图标')` | `.accessibilityText('小猫线条图标')` |
| 关闭动画后猫的姿态 | 停在 progress=0.5 | 需实现（`未验证`） |
| 按钮有可读名称 | 各处 `Semantics`/`tooltip` | `.accessibilityText` |

### 9.4 措辞纪律

**不写「视觉一致」，只写「对照过这 N 项，差异是这些」。**
这个仓库因为「声称做了但没做」被打回过多次，这一条不是形式主义。

---

## 10 DON'T

| 不要 | 因为 |
|---|---|
| 试图把 `experience_shell.dart` 逐行翻成 ArkTS | 2556 行的单文件在 ArkTS 里是灾难（§7） |
| 迁移 controller 层 | 鸿蒙侧已有等价实现，迁了就是两套业务胶水（§2.1） |
| 在 ArkTS 里复刻 `ColorScheme.fromSeed` | 实测界面 0 处使用（§6） |
| 照搬 `fontFamily: 'Microsoft YaHei'` | 鸿蒙上没有这个字体（§5.7） |
| 直接抄 `height: 1.6` 当 `lineHeight` | 一个是倍数，一个是 vp（§5.6） |
| 用随机数做手绘抖动 | 原码是**确定性** `sin`（`sketch_ui.dart:68-71`），改随机会让边框逐帧跳 |
| 指望 `Canvas` 自己重画 | ArkTS 没有 `shouldRepaint`（§5.3） |
| 用 `dynamic` / `any` 省事 | ArkTS 禁 `any`/`unknown`，编不过 |
| 为了「一致」砍掉平台特性 | 沉浸式、手势返回、系统字体缩放该保留；要一致的是**内容与语义**，不是交互外壳 |

---

## 11 未验证清单（如实列出）

这份文档里有些结论是读码得来的，**没有在真机上验证过**。逐条列出，避免被当成事实：

1. **鸿蒙「减少动态效果 / 关闭动画」的读取方式**——`MediaQuery.disableAnimationsOf` 的对应物
   我没有核实。可能是 `accessibility` 或 `configuration` 里的某个字段，也可能没有等价能力。
   **这会影响 §5.8 那条无障碍分支能不能实现。**
2. **`promptAction.showToast` 能否复刻 `snackBarTheme` 的 ink 底 + paper 字**——toast 的样式
   能力受系统限制，可能只能改文字、改不了背景。若改不了，就要换成自定义组件或 `bindSheet`。
3. **`i18n.System.getTimezone()`** 的存在与返回值格式——我按名字推断，未查文档。
4. **vp 与 Flutter logical px 的换算**——本文假设 1:1（在 1x 基准下）。大屏/折叠屏上
   可能需要按 `display` 的 density 换算。**这是「判断」，不是实测。**
5. **`@Computed` 的可用版本**——本文按 ArkUI API 12+ 推断，未在本仓库的 SDK 上验证。
6. **`revision_diff.dart`（76 行）是否有 Flutter 依赖**——只看了行数，没读内容。

---

## 12 附：与其它文档的关系

| 文档 | 管什么 | 与本文的边界 |
|---|---|---|
| `docs/architecture/m6-鸿蒙界面对接.md` | 鸿蒙界面**怎么调核心**（方法、状态、DO/DON'T） | 本文管**怎么把 Dart 的界面搬过来**，不重复讲契约 |
| `docs/product/界面结构契约.md`（若已合入） | 一级入口、页面职责、共享文案 | 本文 §8 的页面缺口清单以它为准 |
| `docs/architecture/m6-鸿蒙可行性探针.md` | 为什么选 ArkTS（§1.1 的出处） | 本文只引用结论 |

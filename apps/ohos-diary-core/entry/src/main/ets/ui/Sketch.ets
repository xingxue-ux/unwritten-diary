import { drawing } from '@kit.ArkGraphics2D';
// Windows sketch_ui.dart 的 ArkUI 实现：配色、描边采样与图标坐标使用同一基线。
export class SketchColors {
  static readonly canvas: string = '#F8F7F2';
  static readonly paper: string = '#FFFEFA';
  static readonly soft: string = '#F1F0EA';
  static readonly button: string = '#EAE8E0';
  static readonly ink: string = '#303331';
  static readonly text: string = '#454541';
  static readonly muted: string = '#62645E';
  static readonly line: string = '#D8D9D2';
  static readonly blush: string = '#D9C1B3';
  static readonly alert: string = '#9A4438';
  static readonly warning: string = '#785F3D';
}

export class SketchFonts {
  static readonly display: string = 'DiaryDisplay';
}

export class SketchType {
  static readonly display: number = 33;
  static readonly pageTitle: number = 42;
  static readonly section: number = 16;
  static readonly body: number = 15;
  static readonly caption: number = 13;
}

// 直接在宿主组件的最终尺寸内绘制；不创建百分比 overlay，也不参与布局或触摸。
export class SketchBorder extends DrawModifier {
  drawForeground(context: DrawContext): void {
    const scale: number = vp2px(1);
    const w: number = context.sizeInPixel.width / scale - 1.5;
    const h: number = context.sizeInPixel.height / scale - 1.5;
    if (w <= 0 || h <= 0) { return; }
    const path: drawing.Path = new drawing.Path();
    this.edge(path, 1.5, 1.5, w, 1.5, 36, true, true, w, h, scale);
    this.edge(path, w, 1.5, w, h, 30, false, false, w, h, scale);
    this.edge(path, w, h, 1.5, h, 36, true, false, w, h, scale);
    this.edge(path, 1.5, h, 1.5, 1.5, 30, false, false, w, h, scale);
    const pen: drawing.Pen = new drawing.Pen();
    pen.setAntiAlias(true);
    pen.setColor(255, 48, 51, 49);
    pen.setStrokeWidth(1.8 * scale);
    pen.setCapStyle(drawing.CapStyle.ROUND_CAP);
    const canvas: drawing.Canvas = context.canvas;
    canvas.save();
    canvas.attachPen(pen);
    canvas.drawPath(path);
    canvas.detachPen();
    canvas.translate(0.7 * scale, 0.4 * scale);
    pen.setColor(41, 48, 51, 49);
    pen.setStrokeWidth(0.7 * scale);
    canvas.attachPen(pen);
    canvas.drawPath(path);
    canvas.detachPen();
    canvas.restore();
  }

  private edge(path: drawing.Path, ax: number, ay: number, bx: number, by: number,
    count: number, horizontal: boolean, first: boolean, w: number, h: number, scale: number): void {
    for (let i: number = 0; i <= count; i++) {
      const t: number = i / count;
      const wobble: number = Math.sin(i * 0.73 + w * 0.013 + h * 0.009) * 0.55 +
        Math.sin(i * 0.21 + 2.3) * 0.28;
      const x: number = (ax + (bx - ax) * t + (horizontal ? 0 : wobble)) * scale;
      const y: number = (ay + (by - ay) * t + (horizontal ? wobble : 0)) * scale;
      if (first && i === 0) { path.moveTo(x, y); }
      else { path.lineTo(x, y); }
    }
  }
}

export function sketchOutline(): SketchBorder {
  // 每个宿主独立持有 DrawModifier，避免 invalidate/重绘状态串到另一张卡片。
  return new SketchBorder();
}

export class SketchButtonStyle implements AttributeModifier<ButtonAttribute> {
  applyNormalAttribute(button: ButtonAttribute): void {
    button.borderRadius(0)
      .constraintSize({ minWidth: 48, minHeight: 48 })
      .padding({ left: 16, right: 16, top: 10, bottom: 10 })
      .backgroundColor(SketchColors.paper)
      .fontColor(SketchColors.ink)
      .fontSize(15)
      .labelStyle({ maxLines: 3, overflow: TextOverflow.Ellipsis });
  }
}

export const sketchButton: SketchButtonStyle = new SketchButtonStyle();

export class SketchInputStyle implements AttributeModifier<TextInputAttribute> {
  applyNormalAttribute(input: TextInputAttribute): void {
    input.fontColor(SketchColors.text).caretColor(SketchColors.ink)
      .placeholderColor(SketchColors.muted).backgroundColor(SketchColors.soft).borderRadius(0);
  }
}

export class SketchAreaStyle implements AttributeModifier<TextAreaAttribute> {
  applyNormalAttribute(input: TextAreaAttribute): void {
    input.fontColor(SketchColors.text).caretColor(SketchColors.ink)
      .placeholderColor(SketchColors.muted).backgroundColor(SketchColors.paper).borderRadius(0);
  }
}

export const sketchInput: SketchInputStyle = new SketchInputStyle();
export const sketchArea: SketchAreaStyle = new SketchAreaStyle();

@Builder
function SketchCardEmpty() {
}

@Component
export struct SketchCard {
  @BuilderParam content: () => void = SketchCardEmpty;
  @Prop contentPadding: number = 20;

  build() {
    Column() {
      this.content()
    }
    .width('100%')
    .alignItems(HorizontalAlign.Start)
    .padding(this.contentPadding)
    .backgroundColor(SketchColors.paper)
    .drawModifier(sketchOutline())
  }
}

// 与 Windows CatMark 使用相同的 100 × 100 路径。
@Component
export struct CatMark {
  @Prop iconSize: number = 33;
  private context: CanvasRenderingContext2D = new CanvasRenderingContext2D(new RenderingContextSettings(true));
  build() {
    Canvas(this.context)
      .width(this.iconSize)
      .height(this.iconSize)
      .accessibilityText('小猫线条图标')
      .onReady(() => {
        const ctx: CanvasRenderingContext2D = this.context;
        ctx.scale(this.iconSize / 100, this.iconSize / 100);
        ctx.strokeStyle = SketchColors.ink;
        ctx.fillStyle = SketchColors.ink;
        ctx.lineWidth = 3.4;
        ctx.lineCap = 'round';
        ctx.lineJoin = 'round';
        ctx.beginPath();
        ctx.moveTo(22, 54);
        ctx.lineTo(22, 27);
        ctx.lineTo(29, 17);
        ctx.lineTo(39, 26);
        ctx.quadraticCurveTo(50, 22, 61, 26);
        ctx.lineTo(72, 17);
        ctx.lineTo(78, 27);
        ctx.lineTo(78, 54);
        ctx.bezierCurveTo(78, 76, 22, 76, 22, 54);
        ctx.stroke();
        for (let i: number = 0; i < 3; i++) {
          ctx.beginPath();
          ctx.arc(i === 2 ? 50 : 40 + i * 20, i === 2 ? 60 : 51, 2.6, 0, Math.PI * 2);
          ctx.fill();
        }
        ctx.beginPath();
        ctx.lineWidth = 2.1;
        ctx.moveTo(17, 84);
        ctx.lineTo(83, 84);
        ctx.stroke();
      })
  }
}

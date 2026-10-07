import 'dart:math' as math;

import 'package:flutter/material.dart';

abstract final class SketchFonts {
  static const display = 'DiaryDisplay';
}

abstract final class SketchColors {
  static const canvas = Color(0xFFF8F7F2);
  static const paper = Color(0xFFFFFEFA);
  static const soft = Color(0xFFF1F0EA);
  static const button = Color(0xFFEAE8E0);
  static const ink = Color(0xFF303331);
  static const text = Color(0xFF454541);
  static const muted = Color(0xFF858780);
  static const line = Color(0xFFD8D9D2);
  static const blush = Color(0xFFD9C1B3);
}

class SketchFrame extends StatelessWidget {
  const SketchFrame({
    super.key,
    required this.child,
    this.fill = SketchColors.paper,
    this.padding = const EdgeInsets.all(20),
    this.weight = 1.8,
  });

  final Widget child;
  final Color fill;
  final EdgeInsets padding;
  final double weight;

  @override
  Widget build(BuildContext context) => CustomPaint(
    foregroundPainter: _SketchBorder(weight),
    child: Material(
      color: fill,
      child: Padding(padding: padding, child: child),
    ),
  );
}

class _SketchBorder extends CustomPainter {
  const _SketchBorder(this.weight);
  final double weight;

  @override
  void paint(Canvas canvas, Size size) {
    final paint = Paint()
      ..color = SketchColors.ink
      ..strokeWidth = weight
      ..style = PaintingStyle.stroke
      ..strokeCap = StrokeCap.round;
    final ghost = Paint()
      ..color = SketchColors.ink.withValues(alpha: .16)
      ..strokeWidth = .7
      ..style = PaintingStyle.stroke;
    final path = Path();
    final w = size.width - 1.5;
    final h = size.height - 1.5;
    void edge(Offset a, Offset b, int count, bool horizontal) {
      for (var i = 0; i <= count; i++) {
        final t = i / count;
        final wobble =
            math.sin(i * .73 + w * .013 + h * .009) * .55 +
            math.sin(i * .21 + 2.3) * .28;
        final x = a.dx + (b.dx - a.dx) * t + (horizontal ? 0 : wobble);
        final y = a.dy + (b.dy - a.dy) * t + (horizontal ? wobble : 0);
        if (i == 0 && a.dx == 1.5 && a.dy == 1.5) {
          path.moveTo(x, y);
        } else {
          path.lineTo(x, y);
        }
      }
    }

    edge(const Offset(1.5, 1.5), Offset(w, 1.5), 36, true);
    edge(Offset(w, 1.5), Offset(w, h), 30, false);
    edge(Offset(w, h), Offset(1.5, h), 36, true);
    edge(Offset(1.5, h), const Offset(1.5, 1.5), 30, false);
    canvas.drawPath(path, paint);
    canvas.save();
    canvas.translate(.7, .4);
    canvas.drawPath(path, ghost);
    canvas.restore();
  }

  @override
  bool shouldRepaint(covariant _SketchBorder oldDelegate) =>
      oldDelegate.weight != weight;
}

class SketchAction extends StatelessWidget {
  const SketchAction({
    super.key,
    required this.label,
    required this.onPressed,
    this.icon,
    this.filled = false,
    this.compact = false,
  });

  final String label;
  final VoidCallback? onPressed;
  final IconData? icon;
  final bool filled;
  final bool compact;

  @override
  Widget build(BuildContext context) => Opacity(
    opacity: onPressed == null ? .42 : 1,
    child: InkWell(
      onTap: onPressed,
      hoverColor: SketchColors.soft,
      splashColor: SketchColors.blush.withValues(alpha: .4),
      child: SketchFrame(
        fill: filled ? SketchColors.button : SketchColors.paper,
        padding: EdgeInsets.symmetric(
          horizontal: compact ? 12 : 18,
          vertical: compact ? 10 : 13,
        ),
        child: Row(
          mainAxisSize: MainAxisSize.min,
          mainAxisAlignment: MainAxisAlignment.center,
          children: [
            if (icon != null) ...[
              Icon(icon, size: compact ? 16 : 18, color: SketchColors.ink),
              const SizedBox(width: 8),
            ],
            Flexible(
              child: Text(
                label,
                softWrap: true,
                textAlign: TextAlign.center,
                style: TextStyle(
                  color: SketchColors.ink,
                  fontSize: compact ? 13 : 15,
                  fontWeight: FontWeight.w600,
                ),
              ),
            ),
          ],
        ),
      ),
    ),
  );
}

class CatMark extends StatelessWidget {
  const CatMark({super.key, this.size = 48});
  final double size;

  @override
  Widget build(BuildContext context) => Semantics(
    label: '小猫线条图标',
    child: SizedBox(
      width: size,
      height: size,
      child: CustomPaint(painter: const _CatMarkPainter()),
    ),
  );
}

class _CatMarkPainter extends CustomPainter {
  const _CatMarkPainter();

  @override
  void paint(Canvas canvas, Size size) {
    canvas.scale(size.width / 100, size.height / 100);
    final ink = Paint()
      ..color = SketchColors.ink
      ..style = PaintingStyle.stroke
      ..strokeWidth = 3.4
      ..strokeCap = StrokeCap.round
      ..strokeJoin = StrokeJoin.round;
    final face = Path()
      ..moveTo(22, 54)
      ..lineTo(22, 27)
      ..lineTo(29, 17)
      ..lineTo(39, 26)
      ..quadraticBezierTo(50, 22, 61, 26)
      ..lineTo(72, 17)
      ..lineTo(78, 27)
      ..lineTo(78, 54)
      ..cubicTo(78, 76, 22, 76, 22, 54);
    canvas.drawPath(face, ink);
    final dot = Paint()..color = SketchColors.ink;
    canvas.drawCircle(const Offset(40, 51), 2.6, dot);
    canvas.drawCircle(const Offset(60, 51), 2.6, dot);
    canvas.drawCircle(const Offset(50, 60), 2.6, dot);
    canvas.drawLine(
      const Offset(17, 84),
      const Offset(83, 84),
      ink..strokeWidth = 2.1,
    );
  }

  @override
  bool shouldRepaint(covariant CustomPainter oldDelegate) => false;
}

class WritingCat extends StatefulWidget {
  const WritingCat({super.key, this.size = 250});
  final double size;

  @override
  State<WritingCat> createState() => _WritingCatState();
}

class _WritingCatState extends State<WritingCat>
    with SingleTickerProviderStateMixin {
  late final AnimationController _motion;

  @override
  void initState() {
    super.initState();
    _motion = AnimationController(
      vsync: this,
      duration: const Duration(milliseconds: 1100),
    )..repeat(reverse: true);
  }

  @override
  void dispose() {
    _motion.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) => SizedBox(
    width: widget.size,
    height: widget.size,
    child: AnimatedBuilder(
      animation: _motion,
      builder: (_, _) =>
          CustomPaint(painter: _WritingCatPainter(_motion.value)),
    ),
  );
}

class _WritingCatPainter extends CustomPainter {
  const _WritingCatPainter(this.progress);
  final double progress;

  @override
  void paint(Canvas canvas, Size size) {
    canvas.scale(size.width / 300, size.height / 300);
    final ink = Paint()
      ..color = SketchColors.ink
      ..style = PaintingStyle.stroke
      ..strokeWidth = 3.5
      ..strokeCap = StrokeCap.round
      ..strokeJoin = StrokeJoin.round;
    final faint = Paint()
      ..color = SketchColors.line
      ..style = PaintingStyle.stroke
      ..strokeWidth = 2;
    final face = Path()
      ..moveTo(93, 127)
      ..lineTo(94, 61)
      ..lineTo(102, 40)
      ..lineTo(119, 55)
      ..quadraticBezierTo(151, 42, 181, 55)
      ..lineTo(198, 40)
      ..lineTo(207, 59)
      ..lineTo(207, 125)
      ..cubicTo(213, 168, 181, 177, 151, 177)
      ..cubicTo(116, 177, 87, 161, 93, 127);
    canvas.drawPath(face, ink);
    final blush = Paint()
      ..color = SketchColors.blush
      ..strokeWidth = 4
      ..strokeCap = StrokeCap.round;
    canvas.drawLine(const Offset(106, 56), const Offset(112, 63), blush);
    canvas.drawLine(const Offset(194, 61), const Offset(201, 54), blush);
    final dot = Paint()..color = SketchColors.ink;
    canvas.drawCircle(const Offset(130, 121), 3.2, dot);
    canvas.drawCircle(const Offset(171, 121), 3.2, dot);
    canvas.drawCircle(const Offset(150, 137), 3.1, dot);
    canvas.drawLine(const Offset(150, 139), const Offset(148, 145), ink);
    final body = Path()
      ..moveTo(97, 157)
      ..cubicTo(71, 167, 60, 197, 60, 252)
      ..quadraticBezierTo(60, 268, 105, 264)
      ..moveTo(205, 155)
      ..cubicTo(231, 168, 242, 205, 239, 252)
      ..quadraticBezierTo(239, 266, 226, 264)
      ..moveTo(78, 231)
      ..quadraticBezierTo(115, 250, 147, 251)
      ..quadraticBezierTo(181, 252, 198, 227)
      ..quadraticBezierTo(225, 232, 226, 260);
    canvas.drawPath(body, ink);
    final page = Path()
      ..moveTo(141, 248)
      ..lineTo(222, 248)
      ..lineTo(231, 282)
      ..lineTo(148, 282)
      ..close();
    canvas.drawPath(page, ink);
    canvas.drawLine(const Offset(157, 260), const Offset(204, 261), faint);
    canvas.drawLine(const Offset(178, 273), const Offset(211, 273), faint);
    canvas.drawLine(const Offset(34, 264), const Offset(103, 264), ink);
    canvas.drawLine(const Offset(239, 264), const Offset(266, 264), ink);
    canvas.drawLine(
      const Offset(199, 215),
      Offset(210, 252 + progress * 5),
      ink,
    );
    canvas.drawLine(
      Offset(210, 252 + progress * 5),
      Offset(207, 261 + progress * 5),
      ink,
    );
  }

  @override
  bool shouldRepaint(covariant _WritingCatPainter oldDelegate) =>
      oldDelegate.progress != progress;
}

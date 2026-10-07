import 'package:flutter/material.dart';

import 'sketch_ui.dart';

abstract final class DiaryTheme {
  static ThemeData get light {
    final scheme = ColorScheme.fromSeed(
      seedColor: SketchColors.ink,
      brightness: Brightness.light,
      surface: SketchColors.paper,
    );
    return ThemeData(
      useMaterial3: true,
      colorScheme: scheme,
      scaffoldBackgroundColor: SketchColors.canvas,
      fontFamily: 'Microsoft YaHei',
      fontFamilyFallback: const ['Noto Sans CJK SC', 'sans-serif'],
      textTheme: const TextTheme(
        bodyLarge: TextStyle(color: SketchColors.text, height: 1.6),
        bodyMedium: TextStyle(color: SketchColors.text, height: 1.5),
      ),
      textButtonTheme: TextButtonThemeData(
        style: TextButton.styleFrom(foregroundColor: SketchColors.ink),
      ),
      inputDecorationTheme: const InputDecorationTheme(
        border: InputBorder.none,
        hintStyle: TextStyle(color: SketchColors.muted),
      ),
      chipTheme: ChipThemeData(
        backgroundColor: SketchColors.paper,
        selectedColor: SketchColors.button,
        side: const BorderSide(color: SketchColors.ink, width: 1),
        shape: const RoundedRectangleBorder(),
        labelStyle: const TextStyle(
          color: SketchColors.ink,
          fontFamily: 'Microsoft YaHei',
          fontFamilyFallback: ['Noto Sans CJK SC', 'sans-serif'],
        ),
      ),
      snackBarTheme: const SnackBarThemeData(
        backgroundColor: SketchColors.ink,
        contentTextStyle: TextStyle(color: SketchColors.paper),
      ),
    );
  }
}

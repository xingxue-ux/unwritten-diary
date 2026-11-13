import 'package:diary_mock/diary_mock.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:unwritten_diary/src/readable_export_controller.dart';
import 'package:unwritten_diary/src/readable_export_panel.dart';

/// 「附带原始材料」是真的能用的：打开后产物是 zip（正文 + 原件 + 清单）。
///
/// 以前这个开关能打开、但核心直接拒绝，点生成必然失败。现在核心实现了，
/// 开关也真的会把这个选择传给导出请求。
void main() {
  TestWidgetsFlutterBinding.ensureInitialized();

  testWidgets('附带原始材料的开关能打开，并把选择传给导出请求', (tester) async {
    final controller = ReadableExportController(
      MockDiaryApi(),
      capabilities: const {'exports.create'},
    );
    addTearDown(controller.dispose);
    await tester.pumpWidget(
      MaterialApp(
        home: Scaffold(
          body: SingleChildScrollView(
            child: ReadableExportPanel(controller: controller),
          ),
        ),
      ),
    );
    await tester.pump();

    var tile = tester.widget<SwitchListTile>(
      find.byKey(const ValueKey('include-assets')),
    );
    expect(tile.onChanged, isNotNull, reason: '现在真的支持了，开关要能按');
    expect(tile.value, isFalse);
    expect(find.textContaining('正文始终包含在导出中'), findsOneWidget);

    await tester.tap(find.byKey(const ValueKey('include-assets')));
    await tester.pumpAndSettle();
    tile = tester.widget<SwitchListTile>(
      find.byKey(const ValueKey('include-assets')),
    );
    expect(tile.value, isTrue);
    expect(find.textContaining('zip'), findsOneWidget);
  });
}

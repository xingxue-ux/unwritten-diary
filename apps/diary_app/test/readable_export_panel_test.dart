import 'package:diary_mock/diary_mock.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:unwritten_diary/src/readable_export_controller.dart';
import 'package:unwritten_diary/src/readable_export_panel.dart';

/// 「附带原始材料」这一版做不到，就不能给一个能按的开关。
///
/// 核心对 `includeAssets = true` 直接拒绝（「本构建的阅读导出只写当前日记正文」），
/// 以前这里是个能打开的开关，点生成必然失败——承诺了做不到的事。
void main() {
  TestWidgetsFlutterBinding.ensureInitialized();

  testWidgets('附带原始材料的开关是禁用的，并说明原因', (tester) async {
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

    final tile = tester.widget<SwitchListTile>(
      find.byKey(const ValueKey('include-assets')),
    );
    expect(tile.onChanged, isNull, reason: '做不到的事不能给一个能按的开关');
    expect(tile.value, isFalse);
    expect(find.textContaining('这一版只导出正文'), findsOneWidget);
  });
}

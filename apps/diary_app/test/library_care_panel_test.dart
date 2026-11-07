import 'package:diary_mock/diary_mock.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:unwritten_diary/src/library_care_controller.dart';
import 'package:unwritten_diary/src/library_care_panel.dart';

/// 恢复完成之后，界面必须给一条「打开恢复后的资料库」的路。
///
/// 没有它，用户恢复完、重开应用，看到的还是旧库——恢复等于白做。
void main() {
  TestWidgetsFlutterBinding.ensureInitialized();

  Future<LibraryCareController> pump(
    WidgetTester tester, {
    String? restoredPath,
    Future<void> Function(String?)? onOpen,
  }) async {
    final controller = LibraryCareController(
      MockDiaryApi(),
      capabilities: const {
        'backups.create',
        'backups.inspect',
        'backups.restore',
      },
      captureCount: 1,
    );
    controller.restoredLibraryPath = restoredPath;
    controller.message = restoredPath == null ? '任务已完成。' : '恢复完成，新的资料库已经生成。';
    addTearDown(controller.dispose);
    await tester.pumpWidget(
      MaterialApp(
        home: Scaffold(
          body: SingleChildScrollView(
            child: BackupRestorePanel(
              controller: controller,
              pickBackupTarget: () async => null,
              pickBackupPackage: () async => null,
              openRestoredLibrary: onOpen,
            ),
          ),
        ),
      ),
    );
    await tester.pump();
    return controller;
  }

  testWidgets('恢复完成后出现「打开恢复后的资料库」，点下去把路径交出去', (tester) async {
    String? opened;
    await pump(
      tester,
      restoredPath: '/tmp/restored/library.sqlite',
      onOpen: (path) async => opened = path,
    );

    final button = find.byKey(const ValueKey('open-restored-library'));
    expect(button, findsOneWidget);
    expect(find.textContaining('不会被覆盖'), findsOneWidget);

    await tester.ensureVisible(button);
    await tester.tap(button);
    await tester.pump();
    expect(opened, '/tmp/restored/library.sqlite');
  });

  testWidgets('没有恢复结果时不显示这个按钮', (tester) async {
    await pump(tester, onOpen: (_) async {});
    expect(find.byKey(const ValueKey('open-restored-library')), findsNothing);
  });

  testWidgets('恢复之后还能回到默认资料库', (tester) async {
    // 打开新库之后如果发现不是想要的那份，得有一条路回去——否则用户被卡在新库里。
    String? opened;
    await pump(
      tester,
      restoredPath: '/tmp/restored/library.sqlite',
      onOpen: (path) async => opened = path,
    );
    final back = find.byKey(const ValueKey('open-default-library'));
    expect(back, findsOneWidget);
    await tester.ensureVisible(back);
    await tester.tap(back);
    await tester.pump();
    expect(opened, isNull, reason: '传 null 表示回到默认库');
  });

  testWidgets('环境不支持切换时不显示按钮，不画一个按不动的入口', (tester) async {
    await pump(tester, restoredPath: '/tmp/restored/library.sqlite');
    expect(find.byKey(const ValueKey('open-restored-library')), findsNothing);
  });
}

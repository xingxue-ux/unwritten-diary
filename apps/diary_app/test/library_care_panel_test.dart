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
    String? libraryPath,
    String? defaultLibraryPath,
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
              libraryPath: libraryPath,
              defaultLibraryPath: defaultLibraryPath,
            ),
          ),
        ),
      ),
    );
    await tester.pump();
    return controller;
  }

  testWidgets('恢复完成后出现「打开恢复后的资料库」，点下去把路径交出去', (tester) async {
    var opened = '<没调用>';
    await pump(
      tester,
      restoredPath: '/tmp/restored/library.sqlite',
      onOpen: (path) async => opened = path ?? '<null>',
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

  testWidgets('用着恢复库时，「回到默认资料库」一直在（不依赖刚恢复完）', (tester) async {
    // 切库会重建整棵树、重启更是什么都不剩。这条回头路必须由**当前用的是哪个库**
    // 决定，而不是由「刚恢复完」这个临时状态决定。
    //
    // 用哨兵值而不是 null 作初值：回调传的就是 null（表示回到默认库），
    // 初值也用 null 的话「回调根本没执行」和「回调传了 null」分不出来。
    var opened = '<没调用>';
    await pump(
      tester,
      onOpen: (path) async => opened = path ?? '<null>',
      libraryPath: '/tmp/restored/library.sqlite',
      defaultLibraryPath: '/app/default/library.sqlite',
    );
    expect(find.textContaining('正在用另一个资料库'), findsOneWidget);
    final back = find.byKey(const ValueKey('open-default-library'));
    expect(back, findsOneWidget);
    await tester.ensureVisible(back);
    await tester.tap(back);
    await tester.pump();
    expect(opened, '<null>', reason: '传 null 表示回到默认库');
  });

  testWidgets('用着默认库时不显示「回到默认资料库」', (tester) async {
    await pump(
      tester,
      onOpen: (_) async {},
      libraryPath: '/app/default/library.sqlite',
      defaultLibraryPath: '/app/default/library.sqlite',
    );
    expect(find.textContaining('正在用默认资料库'), findsOneWidget);
    expect(find.byKey(const ValueKey('open-default-library')), findsNothing);
  });

  testWidgets('环境不支持切换时不显示按钮，不画一个按不动的入口', (tester) async {
    await pump(tester, restoredPath: '/tmp/restored/library.sqlite');
    expect(find.byKey(const ValueKey('open-restored-library')), findsNothing);
  });
}

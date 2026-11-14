import 'package:diary_api/diary_api.dart';
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

  _restoreRefreshTests();
}

/// 从回收站恢复之后，普通「片段」时间线要能看到这条记录。
///
/// 以前恢复只重载回收站列表；时间线读的是缓存里的状态，那条还是 trashed，
/// 于是用户恢复完回到「片段」看不到它——要重启才出现，看起来像恢复失败。
void _restoreRefreshTests() {
  testWidgets('恢复成功后通知时间线刷新', (tester) async {
    final api = MockDiaryApi();
    final seeded = await api.createDraft(operationId: 'op-seed');
    final controller = LibraryCareController(
      api,
      capabilities: const {'captures.restore', 'captures.list'},
      captureCount: 1,
    );
    addTearDown(controller.dispose);
    var refreshed = 0;
    controller.trashedCaptures = [
      Capture(
        id: seeded.id,
        revision: 3,
        state: CaptureState.trashed,
        occurredAt: DateTime.utc(2026, 11, 11),
        createdAt: DateTime.utc(2026, 11, 11),
        updatedAt: DateTime.utc(2026, 11, 11),
        timeZone: 'Asia/Shanghai',
        utcOffsetMinutes: 480,
        dayKey: '2026-11-11',
        draftText: '被恢复的记录。',
      ),
    ];

    await tester.pumpWidget(
      MaterialApp(
        home: Scaffold(
          body: SingleChildScrollView(
            child: StorageTrashPanel(
              controller: controller,
              onRestored: () async => refreshed++,
            ),
          ),
        ),
      ),
    );
    tester.view.physicalSize = const Size(1200, 2000);
    tester.view.devicePixelRatio = 1;
    addTearDown(tester.view.resetPhysicalSize);
    addTearDown(tester.view.resetDevicePixelRatio);
    await tester.pump();

    final restore = find.text('恢复').first;
    await tester.ensureVisible(restore);
    await tester.pump();
    await tester.tap(restore);
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 50));

    expect(refreshed, 1, reason: '恢复成功后必须让时间线重新读一遍');
  });

  testWidgets('恢复后存储页的记录条数跟着变', (tester) async {
    // 条数以前是构造时传进来的 final 标量：恢复完回到存储页还显示旧数字，
    // 退出重进也不变——因为没人再算过。
    final api = MockDiaryApi();
    final seeded = await api.createDraft(operationId: 'op-seed');
    final controller = LibraryCareController(
      api,
      capabilities: const {'captures.restore', 'captures.list'},
      captureCount: 6,
    );
    addTearDown(controller.dispose);
    controller.trashedCaptures = [
      Capture(
        id: seeded.id,
        revision: 3,
        state: CaptureState.trashed,
        occurredAt: DateTime.utc(2026, 11, 11),
        createdAt: DateTime.utc(2026, 11, 11),
        updatedAt: DateTime.utc(2026, 11, 11),
        timeZone: 'Asia/Shanghai',
        utcOffsetMinutes: 480,
        dayKey: '2026-11-11',
        draftText: '被恢复的记录。',
      ),
    ];

    tester.view.physicalSize = const Size(1200, 2000);
    tester.view.devicePixelRatio = 1;
    addTearDown(tester.view.resetPhysicalSize);
    addTearDown(tester.view.resetDevicePixelRatio);
    await tester.pumpWidget(
      MaterialApp(
        home: Scaffold(
          body: SingleChildScrollView(
            child: StorageTrashPanel(controller: controller),
          ),
        ),
      ),
    );
    await tester.pump();
    expect(find.textContaining('资料库记录 · 6 条'), findsOneWidget);

    final restore = find.text('恢复').first;
    await tester.ensureVisible(restore);
    await tester.pump();
    await tester.tap(restore);
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 50));

    expect(
      find.textContaining('资料库记录 · 1 条'),
      findsOneWidget,
      reason: '恢复之后条数要重新问核心，不能还显示启动时的旧数字',
    );
  });
}

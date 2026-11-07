import 'package:diary_api/diary_api.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:unwritten_diary/src/recording/recording_controller.dart';
import 'package:unwritten_diary/src/recording/recording_panel.dart';

import 'recording_fakes.dart';

/// 录音页的按钮必须真的接到控制器上。
///
/// 这一页以前是布局预览：按「模拟开始」只改本地状态，界面说「录制中」但其实
/// 什么都没录。这条测试钉住的是「按下去之后控制器真的走了那一步」——用假麦克风
/// 和假核心，不碰真设备。
void main() {
  TestWidgetsFlutterBinding.ensureInitialized();

  /// 录音的每一步都碰真实文件、还要在 isolate 里算哈希。`pump` 只推假时钟，
  /// 推不动真实 I/O——必须用 `runAsync` 给真实异步一点时间（与
  /// `user_flow_test.dart` 里的 `settleRealIo` 同一个理由）。
  Future<void> settle(WidgetTester tester) async {
    for (var i = 0; i < 8; i++) {
      await tester.runAsync(
        () => Future<void>.delayed(const Duration(milliseconds: 40)),
      );
      await tester.pump();
    }
  }

  Future<(RecordingController, FakeRecordingApi)> pump(
    WidgetTester tester, {
    bool permission = true,
  }) async {
    final api = FakeRecordingApi();
    final engine = FakeRecordingEngine(permission: permission);
    final controller = RecordingController(
      api: api,
      engine: engine,
      captureId: 'cap-1',
    );
    addTearDown(controller.dispose);
    // 页面比默认 800×600 高：按钮在折叠线以下时 `tap` 会点在空处，
    // 测试就会莫名其妙地「按了没反应」。给一块够大的画布。
    tester.view.physicalSize = const Size(1200, 2200);
    tester.view.devicePixelRatio = 1;
    addTearDown(tester.view.resetPhysicalSize);
    addTearDown(tester.view.resetDevicePixelRatio);
    await tester.pumpWidget(
      MaterialApp(
        home: Scaffold(
          body: SingleChildScrollView(
            child: RecordingPanel(api: api, controller: controller),
          ),
        ),
      ),
    );
    // 不能 pumpAndSettle：录音时有转圈和 200 ms 的计时器，永远不会静下来。
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 50));
    return (controller, api);
  }

  testWidgets('按「开始录音」真的开始收音，界面显示正在录音', (tester) async {
    final (controller, api) = await pump(tester);

    await tester.tap(find.byKey(const Key('recording-start')));
    await settle(tester);

    expect(controller.phase, RecordingPhase.recording);
    expect(api.prepareCalls, 1);
    expect(find.text('正在录音'), findsOneWidget);
    expect(find.byKey(const Key('recording-pause')), findsOneWidget);

    // 收尾：计时器还在跑的话，测试结束时会报「Timer is still pending」。
    await tester.runAsync(() => controller.discard());
    await tester.pump();
  });

  testWidgets('暂停之后按「继续」会开新的一段，最后保存', (tester) async {
    final (controller, api) = await pump(tester);
    await tester.tap(find.byKey(const Key('recording-start')));
    await settle(tester);

    await tester.tap(find.byKey(const Key('recording-pause')));
    await settle(tester);
    expect(controller.phase, RecordingPhase.paused);
    expect(find.textContaining('已暂停'), findsOneWidget);

    await tester.tap(find.byKey(const Key('recording-resume')));
    await settle(tester);

    await tester.tap(find.byKey(const Key('recording-finish')));
    await settle(tester);

    expect(controller.phase, RecordingPhase.saved);
    expect(api.segmentIndexes, [0, 1]);
    expect(find.textContaining('录音已保存'), findsOneWidget);
  });

  testWidgets('没有麦克风权限时说清楚原因，不显示「正在录音」', (tester) async {
    final (controller, _) = await pump(tester, permission: false);

    await tester.tap(find.byKey(const Key('recording-start')));
    await settle(tester);

    expect(controller.phase, RecordingPhase.failed);
    expect(find.text('正在录音'), findsNothing);
    expect(find.textContaining('麦克风'), findsOneWidget);
  });

  testWidgets('保存之后转写没结果时如实说「还没有转写结果」，不编一段正文', (tester) async {
    final (controller, api) = await pump(tester);
    await tester.tap(find.byKey(const Key('recording-start')));
    await settle(tester);
    await tester.tap(find.byKey(const Key('recording-finish')));
    await settle(tester);

    // 假核心没有给 extractedContent 任何正文。
    expect(api.calls, contains('content'));
    expect(controller.transcriptionPhase, TranscriptionPhase.idle);
    expect(find.textContaining('还没有转写结果'), findsOneWidget);
    expect(find.textContaining('今天'), findsNothing);
  });

  testWidgets('播放器打不开音频时说明原因，界面不假装能播', (tester) async {
    final (_, api) = await pump(tester);
    await tester.tap(find.byKey(const Key('recording-start')));
    await settle(tester);
    await tester.tap(find.byKey(const Key('recording-finish')));
    await settle(tester);

    expect(api.calls, contains('finalize'));
    expect(find.textContaining('音频打不开'), findsOneWidget);
    expect(find.byKey(const Key('recording-play')), findsNothing);
  });
}

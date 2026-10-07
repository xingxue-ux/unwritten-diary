import 'package:diary_api/diary_api.dart';
import 'package:diary_mock/diary_mock.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:unwritten_diary/main.dart';
import 'package:unwritten_diary/src/sketch_ui.dart';

class CountingMockDiaryApi extends MockDiaryApi {
  int openCount = 0;

  @override
  Future<CoreSnapshot> open({String? libraryHandle}) {
    openCount++;
    return super.open(libraryHandle: libraryHandle);
  }
}

class RetryableOpenFailureApi extends MockDiaryApi {
  int attempts = 0;

  @override
  Future<CoreSnapshot> open({String? libraryHandle}) {
    attempts++;
    if (attempts == 1) {
      throw const DiaryException(
        code: DiaryErrorCode.unknown,
        message: '资料库暂时忙，请重试。',
        retryable: true,
      );
    }
    return super.open(libraryHandle: libraryHandle);
  }
}

class IncompatibleOpenApi extends MockDiaryApi {
  @override
  Future<CoreSnapshot> open({String? libraryHandle}) async =>
      const CoreSnapshot(
        coreInfo: CoreInfo(
          apiVersion: '2.0',
          dataSchemaVersion: 0,
          buildVersion: 'incompatible-test',
          libraryId: 'test',
          capabilities: {
            'captures.createDraft',
            'captures.saveDraft',
            'captures.commit',
            'captures.list',
          },
        ),
        recovery: RecoverySummary(),
      );
}

class ConflictSaveApi extends MockDiaryApi {
  @override
  Future<DraftSaveResult> saveDraft({
    required String id,
    required String text,
    required int expectedRevision,
    required String operationId,
  }) async => throw const DiaryException(
    code: DiaryErrorCode.revisionConflict,
    message: '记录版本冲突，输入仍留在页面上。',
  );
}

Future<void> openApp(WidgetTester tester, MockDiaryApi api) async {
  await tester.pumpWidget(DiaryApp(api: api, demoMode: true));
  await tester.pumpAndSettle();
}

void main() {
  testWidgets(
    'record page opens immediately and committed text appears in timeline',
    (tester) async {
      final api = CountingMockDiaryApi();
      await openApp(tester, api);
      expect(api.openCount, 1);
      expect(find.text('今天，想留住什么？'), findsOneWidget);
      expect(find.textContaining('交互演示'), findsOneWidget);

      await tester.enterText(
        find.byKey(const Key('capture-editor')),
        '今天的风很轻。',
      );
      await tester.pump(const Duration(seconds: 1));
      await tester.pumpAndSettle();
      expect(find.textContaining('已模拟保存'), findsOneWidget);

      await tester.ensureVisible(find.byKey(const Key('finish-capture')));
      await tester.tap(find.byKey(const Key('finish-capture')));
      await tester.pumpAndSettle();
      expect(
        (await api.listCaptures()).captures.single.state,
        CaptureState.committed,
      );

      await tester.tap(find.text('片段').last);
      await tester.pumpAndSettle();
      expect(find.text('今天的风很轻。'), findsOneWidget);
    },
  );

  testWidgets('failed save leaves text editable and can retry', (tester) async {
    final api = MockDiaryApi()
      ..nextSaveFailure = const DiaryException(
        code: DiaryErrorCode.storageFull,
        message: '空间不足，尚未保存。',
      );
    await openApp(tester, api);
    await tester.enterText(find.byKey(const Key('capture-editor')), '第一句');
    await tester.testTextInput.receiveAction(TextInputAction.newline);
    await tester.pump(const Duration(seconds: 1));
    await tester.pumpAndSettle();
    expect(find.textContaining('空间不足'), findsOneWidget);
    expect(find.byKey(const Key('capture-editor')), findsOneWidget);

    await tester.ensureVisible(find.text('重试'));
    await tester.tap(find.text('重试'));
    await tester.pumpAndSettle();
    expect(find.textContaining('已模拟保存'), findsOneWidget);
    expect(
      (await api.listCaptures()).captures.single.state,
      CaptureState.draft,
    );
  });

  testWidgets('diary candidate stays separate until chosen', (tester) async {
    await openApp(tester, MockDiaryApi());
    await tester.tap(find.text('日记').last);
    await tester.pumpAndSettle();
    expect(find.text('普通的一天，也有光'), findsOneWidget);
    await tester.ensureVisible(find.text('模拟重新整理'));
    await tester.tap(find.text('模拟重新整理'));
    await tester.pumpAndSettle();
    expect(find.textContaining('当前阅读版不会自动替换'), findsOneWidget);
    await tester.tap(find.text('查看候选版本'));
    await tester.pumpAndSettle();
    expect(find.text('采用候选版'), findsOneWidget);
  });

  testWidgets('narrow viewport and large text keep primary action reachable', (
    tester,
  ) async {
    tester.view.physicalSize = const Size(320, 700);
    tester.view.devicePixelRatio = 1;
    tester.binding.platformDispatcher.textScaleFactorTestValue = 1.4;
    addTearDown(tester.view.resetPhysicalSize);
    addTearDown(tester.view.resetDevicePixelRatio);
    addTearDown(
      tester.binding.platformDispatcher.clearTextScaleFactorTestValue,
    );

    await openApp(tester, MockDiaryApi());
    await tester.enterText(find.byKey(const Key('capture-editor')), '小小一段。');
    await tester.ensureVisible(find.byKey(const Key('finish-capture')));
    await tester.pumpAndSettle();
    expect(find.byKey(const Key('finish-capture')), findsOneWidget);
    expect(tester.takeException(), isNull);
  });

  testWidgets('retryable open failure shows its message and retry works', (
    tester,
  ) async {
    final api = RetryableOpenFailureApi();
    await tester.pumpWidget(DiaryApp(api: api, demoMode: true));
    await tester.pumpAndSettle();
    expect(find.text('资料库暂时忙，请重试。'), findsOneWidget);
    await tester.tap(find.text('重试'));
    await tester.pumpAndSettle();
    expect(api.attempts, 2);
    expect(find.byKey(const Key('capture-editor')), findsOneWidget);
  });

  testWidgets('incompatible API version cannot enter the editor', (
    tester,
  ) async {
    await tester.pumpWidget(
      DiaryApp(api: IncompatibleOpenApi(), demoMode: true),
    );
    await tester.pumpAndSettle();
    expect(find.textContaining('版本不兼容'), findsOneWidget);
    expect(find.byKey(const Key('capture-editor')), findsNothing);
    expect(find.text('重试'), findsNothing);
  });

  testWidgets('finish failure keeps text and gives explicit feedback', (
    tester,
  ) async {
    await openApp(tester, ConflictSaveApi());
    await tester.enterText(find.byKey(const Key('capture-editor')), '没有丢失');
    await tester.ensureVisible(find.byKey(const Key('finish-capture')));
    await tester.tap(find.byKey(const Key('finish-capture')));
    await tester.pump(const Duration(seconds: 1));
    await tester.pumpAndSettle();
    expect(find.textContaining('记录版本冲突'), findsWidgets);
    expect(find.text('没有丢失'), findsOneWidget);
  });

  testWidgets('two times text scale keeps a committed capture reachable', (
    tester,
  ) async {
    tester.view.physicalSize = const Size(320, 640);
    tester.view.devicePixelRatio = 1;
    tester.binding.platformDispatcher.textScaleFactorTestValue = 2;
    addTearDown(tester.view.resetPhysicalSize);
    addTearDown(tester.view.resetDevicePixelRatio);
    addTearDown(
      tester.binding.platformDispatcher.clearTextScaleFactorTestValue,
    );

    await openApp(tester, MockDiaryApi());
    await tester.enterText(find.byKey(const Key('capture-editor')), '放大字体的记录');
    await tester.ensureVisible(find.byKey(const Key('finish-capture')));
    await tester.tap(find.byKey(const Key('finish-capture')));
    await tester.pumpAndSettle();
    await tester.tap(find.text('片段').last);
    await tester.pumpAndSettle();
    expect(find.text('放大字体的记录'), findsOneWidget);
    expect(tester.takeException(), isNull);
  });

  testWidgets('persistent mode labels a confirmed save accurately', (
    tester,
  ) async {
    await tester.pumpWidget(DiaryApp(api: MockDiaryApi(), demoMode: false));
    await tester.pumpAndSettle();
    await tester.enterText(find.byKey(const Key('capture-editor')), '确认文案');
    await tester.pump(const Duration(seconds: 1));
    await tester.pumpAndSettle();
    expect(find.text('已保存到本地资料库'), findsOneWidget);
    expect(find.textContaining('已模拟保存'), findsNothing);
  });

  testWidgets('attachment and recording entries disclose preview status', (
    tester,
  ) async {
    await openApp(tester, MockDiaryApi());
    await tester.ensureVisible(find.text('添加材料'));
    await tester.tap(find.text('添加材料'));
    await tester.pumpAndSettle();
    expect(find.textContaining('尚未访问设备文件'), findsOneWidget);
    await tester.tap(find.text('模拟添加照片'));
    await tester.pumpAndSettle();
    expect(find.textContaining('照片 · 演示'), findsOneWidget);

    await tester.ensureVisible(find.text('录音').first);
    await tester.tap(find.text('录音').first);
    await tester.pumpAndSettle();
    expect(find.textContaining('不会录音'), findsOneWidget);
  });

  testWidgets('long action wraps at 320 width and two times text scale', (
    tester,
  ) async {
    tester.view.physicalSize = const Size(320, 700);
    tester.view.devicePixelRatio = 1;
    tester.binding.platformDispatcher.textScaleFactorTestValue = 2;
    addTearDown(tester.view.resetPhysicalSize);
    addTearDown(tester.view.resetDevicePixelRatio);
    addTearDown(
      tester.binding.platformDispatcher.clearTextScaleFactorTestValue,
    );

    await tester.pumpWidget(
      MaterialApp(
        home: Scaffold(
          body: SizedBox(
            width: 276,
            child: SketchAction(
              label: '保存为新版（演示）',
              icon: Icons.save_outlined,
              onPressed: () {},
            ),
          ),
        ),
      ),
    );
    expect(find.text('保存为新版（演示）'), findsOneWidget);
    expect(tester.takeException(), isNull);
  });

  testWidgets('demo labels survive scrolling and identify sample cards', (
    tester,
  ) async {
    await openApp(tester, MockDiaryApi());
    await tester.drag(
      find.byType(SingleChildScrollView),
      const Offset(0, -600),
    );
    await tester.pumpAndSettle();
    expect(find.text('演示模式 · 内容仅在本次运行'), findsOneWidget);
    expect(tester.getTopLeft(find.text('演示模式 · 内容仅在本次运行')).dy, lessThan(100));

    await tester.tap(find.text('搜索').last);
    await tester.pumpAndSettle();
    expect(find.text('示例 · 日记 · 无实际日期'), findsOneWidget);
    expect(find.text('示例 · 原始文字 · 无实际日期'), findsOneWidget);
    await tester.tap(find.text('片段').last);
    await tester.pumpAndSettle();
    expect(find.text('示例 · 照片 · 待处理'), findsOneWidget);
    expect(find.text('示例 · 录音 · 待转写'), findsOneWidget);
  });

  testWidgets('preview settings disclose that no core action runs', (
    tester,
  ) async {
    await openApp(tester, MockDiaryApi());
    await tester.tap(find.text('我的').last);
    await tester.pumpAndSettle();
    await tester.ensureVisible(find.text('风格偏好'));
    await tester.tap(find.text('风格偏好'));
    await tester.pumpAndSettle();
    expect(find.textContaining('暂不保存到核心'), findsOneWidget);
    await tester.tap(find.text('返回'));
    await tester.pumpAndSettle();
    await tester.ensureVisible(find.text('整理时机'));
    await tester.tap(find.text('整理时机'));
    await tester.pumpAndSettle();
    expect(find.textContaining('自动整理尚未接入'), findsOneWidget);
  });

  testWidgets('main routes fit a narrow screen at two times text scale', (
    tester,
  ) async {
    tester.view.physicalSize = const Size(320, 700);
    tester.view.devicePixelRatio = 1;
    tester.binding.platformDispatcher.textScaleFactorTestValue = 2;
    addTearDown(tester.view.resetPhysicalSize);
    addTearDown(tester.view.resetDevicePixelRatio);
    addTearDown(
      tester.binding.platformDispatcher.clearTextScaleFactorTestValue,
    );

    await openApp(tester, MockDiaryApi());
    for (final label in ['日记', '搜索', '片段', '我的', '记录']) {
      await tester.tap(find.text(label).last);
      await tester.pumpAndSettle();
      expect(tester.takeException(), isNull, reason: '$label route overflowed');
      expect(find.text('演示模式 · 内容仅在本次运行'), findsOneWidget);
    }
  });

  testWidgets('secondary routes fit a narrow screen at two times text scale', (
    tester,
  ) async {
    tester.view.physicalSize = const Size(320, 700);
    tester.view.devicePixelRatio = 1;
    tester.binding.platformDispatcher.textScaleFactorTestValue = 2;
    addTearDown(tester.view.resetPhysicalSize);
    addTearDown(tester.view.resetDevicePixelRatio);
    addTearDown(
      tester.binding.platformDispatcher.clearTextScaleFactorTestValue,
    );
    await openApp(tester, MockDiaryApi());

    for (final label in [
      '风格偏好',
      '模型服务',
      '整理时机',
      '任务中心',
      '插件',
      '备份与恢复',
      '存储与回收站',
      '封面与加载动画',
    ]) {
      await tester.tap(find.text('我的').last);
      await tester.pumpAndSettle();
      await tester.ensureVisible(find.text(label));
      await tester.tap(find.text(label));
      await tester.pumpAndSettle();
      expect(tester.takeException(), isNull, reason: '$label panel overflowed');
    }

    await tester.ensureVisible(find.text('查看加载动画'));
    await tester.tap(find.text('查看加载动画'));
    await tester.pump(const Duration(milliseconds: 300));
    expect(tester.takeException(), isNull, reason: 'loading panel overflowed');

    await tester.tap(find.text('日记').last);
    await tester.pumpAndSettle();
    for (final label in ['版本', '编辑为新版', '主动请求回应']) {
      if (label != '版本') {
        await tester.tap(find.text('日记').last);
        await tester.pumpAndSettle();
      }
      await tester.ensureVisible(find.text(label).last);
      await tester.tap(find.text(label).last);
      await tester.pumpAndSettle();
      expect(tester.takeException(), isNull, reason: '$label panel overflowed');
    }
  });
}

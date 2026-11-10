import 'dart:async';
import 'dart:io';

import 'package:diary_api/diary_api.dart';
import 'package:diary_mock/diary_mock.dart';
import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';
import 'package:path/path.dart' as path;

import 'src/app_services.dart';
import 'src/core_wakeup.dart';
import 'src/experience_shell.dart';
import 'src/sketch_ui.dart';
import 'src/theme.dart';
import 'src/ui_strings.dart';

const _previewMode = bool.fromEnvironment('DIARY_DEMO_MODE');

Future<void> main() async {
  WidgetsFlutterBinding.ensureInitialized();
  // 文案表随安装包分发；读不到时界面会显示 ⟨文案表未加载: ...⟩ 占位，
  // 这里留下原因，不退回任何硬编码文案。
  try {
    await uiStrings.load();
  } on Object catch (error) {
    debugPrint('共享文案加载失败（${UiStrings.assetPath}）：$error');
  }
  if (Platform.isAndroid && !_previewMode) {
    // A scheduler failure must never prevent the diary from opening for text.
    try {
      await CoreWakeupScheduler.initialize();
    } on Object catch (error) {
      debugPrint('Background wakeup registration failed: $error');
    }
  }
  runApp(
    DiaryApp(api: _previewMode ? MockDiaryApi() : null, demoMode: _previewMode),
  );
}

class DiaryApp extends StatelessWidget {
  const DiaryApp({super.key, this.api, this.demoMode = false});

  /// Tests and the deliberate preview can inject a mock. Normal startup uses
  /// the persistent Rust library through [createProductionDiaryApi].
  final DiaryApi? api;
  final bool demoMode;

  @override
  Widget build(BuildContext context) => MaterialApp(
    title: '不写日记',
    debugShowCheckedModeBanner: false,
    theme: DiaryTheme.light,
    builder: (context, child) {
      if (MediaQuery.platformBrightnessOf(context) != Brightness.dark) {
        return child!;
      }
      // The hand-drawn UI is intentionally built from a fixed paper/ink
      // palette. This warm negative-print transform keeps every stroke and
      // text shade legible in system dark mode without changing its shapes.
      return ColorFiltered(
        colorFilter: const ColorFilter.matrix(<double>[
          0,
          0,
          -.88,
          0,
          245,
          0,
          -.88,
          0,
          0,
          245,
          -.88,
          0,
          0,
          0,
          245,
          0,
          0,
          0,
          1,
          0,
        ]),
        child: child!,
      );
    },
    home: _BootGate(api: api, demoMode: demoMode),
  );
}

class _BootGate extends StatefulWidget {
  const _BootGate({required this.api, required this.demoMode});
  final DiaryApi? api;
  final bool demoMode;

  @override
  State<_BootGate> createState() => _BootGateState();
}

class _BootGateState extends State<_BootGate> {
  /// 启动结果：核心 api、快照，以及**实际打开的是哪个库**（默认库路径也带上，
  /// 界面据此判断要不要给「回到默认资料库」）。
  late Future<
    ({
      DiaryApi api,
      CoreSnapshot snapshot,
      String? libraryPath,
      String? defaultLibraryPath,
    })
  >
  _opening;

  @override
  void initState() {
    super.initState();
    _opening = _open();
  }

  /// 打开资料库。[libraryPath] 为空时用默认路径（或用户上次选的那个）。
  Future<
    ({
      DiaryApi api,
      CoreSnapshot snapshot,
      String? libraryPath,
      String? defaultLibraryPath,
    })
  >
  _open({String? libraryPath}) async {
    try {
      // 注入 api 时（测试、演示）不读「上次选的库」：那条路要做真实文件 I/O，
      // 而 widget 测试跑在假时钟里，真 I/O 的 future 永远不会完成——整个启动
      // 会卡在转圈上。生产路径（`widget.api == null`）才需要这个选择。
      final chosen = widget.api == null
          ? (libraryPath ?? await readChosenLibrary())
          : null;
      final api = widget.api ?? await createProductionDiaryApi(libraryPath: chosen);
      final defaultLibraryPath = widget.api == null
          ? path.join((await defaultLibraryDirectory()).path, 'library.sqlite')
          : null;
      final snapshot = await api.open();
      final appMajor = int.tryParse(kApiVersion.split('.').first);
      final coreMajor = int.tryParse(
        snapshot.coreInfo.apiVersion.split('.').first,
      );
      if (appMajor == null || coreMajor != appMajor) {
        throw _CoreUnavailable('资料库接口版本不兼容，请更新应用后再打开。');
      }
      const requiredCaptureCapabilities = {
        'captures.createDraft',
        'captures.saveDraft',
        'captures.commit',
        'captures.list',
      };
      if (!snapshot.coreInfo.capabilities.containsAll(
        requiredCaptureCapabilities,
      )) {
        throw _CoreUnavailable('资料库缺少记录所需能力，请更新核心后再打开。');
      }
      // **不要按平台判断**：桌面端没有 WorkManager，但它同样需要在启动时把
      // 排队的任务推起来（`afterOpen` 内部自己分平台处理）。
      // 这里曾经是 `Platform.isAndroid && ...`，导致 Windows 上排队的任务
      // 永远没人执行——「昨天的日记一直没整理好」就是这么来的。
      if (!widget.demoMode) {
        unawaited(CoreWakeupScheduler.afterOpen(api, snapshot));
      }
      return (
        api: api,
        snapshot: snapshot,
        libraryPath: chosen ?? defaultLibraryPath,
        defaultLibraryPath: defaultLibraryPath,
      );
    } catch (failure, stackTrace) {
      // The release UI stays concise; debug runs retain the actual bridge or
      // SQLite error so a mismatched Rust/Dart build can be diagnosed.
      if (kDebugMode) {
        debugPrint(
          'Diary core startup failed (${failure.runtimeType}): $failure',
        );
        debugPrintStack(stackTrace: stackTrace);
      }
      rethrow;
    }
  }

  /// 切到另一个资料库（恢复备份之后用）。
  ///
  /// 顺序是刻意的：先记住选择，再关旧库，最后重开。关掉旧库之前调用方要先把
  /// 当前草稿存好——这里不替它猜「还有什么没保存」。
  /// [libraryPath] 为空表示回到默认资料库（应用支持目录下那个）。
  Future<void> _switchLibrary(String? libraryPath) async {
    await writeChosenLibrary(libraryPath);
    final previous = _opening;
    setState(() {
      _opening = _open(libraryPath: libraryPath);
    });
    // 旧库在后台关掉：它已经不在界面上了，关失败也不该挡住新库。
    unawaited(
      previous
          .then((value) => value.api.close())
          .catchError((Object _) {}),
    );
  }

  @override
  Widget build(BuildContext context) =>
      FutureBuilder<
        ({
          DiaryApi api,
          CoreSnapshot snapshot,
          String? libraryPath,
          String? defaultLibraryPath,
        })
      >(
        future: _opening,
        builder: (context, result) {
          if (result.hasData) {
            return ExperienceShell(
              // 换库之后整棵树要重建：控制器都绑在旧 api 上，复用 State 会
              // 让它们继续对着一个已经关掉的会话说话。
              key: ValueKey(identityHashCode(result.data!.api)),
              api: result.data!.api,
              demoMode: widget.demoMode,
              snapshot: result.data!.snapshot,
              libraryPath: result.data!.libraryPath,
              defaultLibraryPath: result.data!.defaultLibraryPath,
              onSwitchLibrary: widget.api == null ? _switchLibrary : null,
              supportsHybridUpgrade: result.data!.snapshot.coreInfo.capabilities
                  .contains('search.upgrade'),
            );
          }
          if (result.hasError) {
            final failure = result.error;
            final message = switch (failure) {
              _CoreUnavailable(:final message) => message,
              ProductionCoreUnavailable(:final message) => message,
              DiaryException(:final message) => message,
              _ => '资料库暂时没能打开，请稍后再试。',
            };
            final retryable = switch (failure) {
              _CoreUnavailable() => false,
              ProductionCoreUnavailable() => false,
              DiaryException(:final retryable) => retryable,
              _ => true,
            };
            return Scaffold(
              backgroundColor: SketchColors.canvas,
              body: Center(
                child: Column(
                  mainAxisSize: MainAxisSize.min,
                  children: [
                    const CatMark(size: 100),
                    const SizedBox(height: 20),
                    Text(message, textAlign: TextAlign.center),
                    const SizedBox(height: 16),
                    if (retryable)
                      SketchAction(
                        label: '重试',
                        onPressed: () => setState(() {
                          _opening = _open();
                        }),
                      ),
                  ],
                ),
              ),
            );
          }
          return const Scaffold(
            backgroundColor: SketchColors.canvas,
            body: Center(
              child: Column(
                mainAxisSize: MainAxisSize.min,
                children: [
                  WritingCat(size: 270),
                  SizedBox(height: 18),
                  Text(
                    '小纸团在整理今天',
                    style: TextStyle(
                      color: SketchColors.ink,
                      fontSize: 24,
                      fontWeight: FontWeight.w700,
                    ),
                  ),
                  SizedBox(height: 7),
                  Text(
                    '稍等一下，就可以开始记录了。',
                    style: TextStyle(color: SketchColors.muted),
                  ),
                ],
              ),
            ),
          );
        },
      );
}

class _CoreUnavailable implements Exception {
  const _CoreUnavailable(this.message);
  final String message;
}

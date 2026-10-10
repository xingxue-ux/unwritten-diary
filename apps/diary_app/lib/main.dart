import 'package:diary_api/diary_api.dart';
import 'package:flutter/material.dart';

import 'src/app_services.dart';
import 'src/experience_shell.dart';
import 'src/sketch_ui.dart';
import 'src/theme.dart';

void main() => runApp(const DiaryApp());

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
  late Future<({DiaryApi api, CoreSnapshot snapshot})> _opening;

  @override
  void initState() {
    super.initState();
    _opening = _open();
  }

  Future<({DiaryApi api, CoreSnapshot snapshot})> _open() async {
    final api = widget.api ?? await createProductionDiaryApi();
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
    return (api: api, snapshot: snapshot);
  }

  @override
  Widget build(BuildContext context) =>
      FutureBuilder<({DiaryApi api, CoreSnapshot snapshot})>(
    future: _opening,
    builder: (context, result) {
      if (result.hasData) {
        return ExperienceShell(
          api: result.data!.api,
          demoMode: widget.demoMode,
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

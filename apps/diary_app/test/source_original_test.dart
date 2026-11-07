import 'dart:io';

import 'package:diary_api/diary_api.dart';
import 'package:diary_mock/diary_mock.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:unwritten_diary/src/source_original.dart';

/// 原件定位：**能开就开，不能开就说清楚为什么**。
///
/// 这一页最容易犯的错是「原件不在也画一个播放器」——用户按下去什么都不发生。
/// 所以这里用假核心把三种情况都钉住。
class _LocateApi extends MockDiaryApi {
  _LocateApi({
    required this.available,
    this.reason,
    this.handle,
    this.sourceKind,
  });

  final bool available;
  final String? reason;
  final String? handle;
  final SourceKind? sourceKind;
  final List<String> calls = [];
  final List<SourceLocator> locators = [];
  int released = 0;

  @override
  Future<SourceLocation> locateSource({
    required String ref,
    required SourceLocator locator,
  }) async {
    calls.add('locate');
    locators.add(locator);
    return SourceLocation(
      sourceRef: ref,
      locator: locator,
      available: available,
      assetId: available ? 'asset-1' : null,
      reason: reason,
      sourceKind: sourceKind,
    );
  }

  @override
  Future<AssetLease> openAsset({
    required String assetId,
    required AssetUsage usage,
  }) async => AssetLease(
    leaseId: 'lease-1',
    assetId: assetId,
    usage: usage,
    handle: handle ?? '',
  );

  @override
  Future<void> releaseAsset(String leaseId) async {
    released++;
  }
}

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();

  Future<void> pump(WidgetTester tester, _LocateApi api) async {
    await tester.pumpWidget(
      MaterialApp(
        home: Scaffold(
          body: SingleChildScrollView(
            child: SourceOriginalCard(
              api: api,
              sourceId: 'src-1',
              sourceKind: SourceKind.audio,
              locator: const SourceLocator(
                type: LocatorType.audio,
                sourceRevisionId: 'rev-1',
                startMs: 12000,
                endMs: 15000,
              ),
            ),
          ),
        ),
      ),
    );
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 50));
  }

  testWidgets('原件不在时把核心给的原因原样显示，不画播放器', (tester) async {
    final api = _LocateApi(
      available: false,
      reason: '原件文件不在文件库里',
    );
    await pump(tester, api);

    expect(find.text('原件文件不在文件库里'), findsOneWidget);
    expect(find.byKey(const Key('source-play')), findsNothing);
    expect(find.textContaining('用系统程序打开'), findsNothing);
  });

  testWidgets('原件在时显示定位到的时刻和播放按钮', (tester) async {
    final dir = Directory.systemTemp.createTempSync('original');
    addTearDown(() => dir.deleteSync(recursive: true));
    final file = File('${dir.path}/a.wav')..writeAsBytesSync(List.filled(64, 0));
    final api = _LocateApi(available: true, handle: file.path);
    await pump(tester, api);

    expect(find.text('原件'), findsOneWidget);
    expect(find.textContaining('定位到 00:12'), findsOneWidget);
    expect(find.byKey(const Key('source-play')), findsOneWidget);
    expect(api.locators.single.startMs, 12000, reason: '位置要跟着命中走');
  });

  testWidgets('调用方不知道类型时，以核心回答的类型为准（录音才给播放器）', (tester) async {
    final dir = Directory.systemTemp.createTempSync('original3');
    addTearDown(() => dir.deleteSync(recursive: true));
    final file = File('${dir.path}/c.wav')..writeAsBytesSync(List.filled(64, 0));
    // 调用方什么都没说，核心说是录音。
    final api = _LocateApi(
      available: true,
      handle: file.path,
      sourceKind: SourceKind.audio,
    );
    await tester.pumpWidget(
      MaterialApp(
        home: Scaffold(
          body: SingleChildScrollView(
            child: SourceOriginalCard(api: api, sourceId: 'src-3'),
          ),
        ),
      ),
    );
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 50));

    expect(
      find.byKey(const Key('source-play')),
      findsOneWidget,
      reason: '类型来自核心，不该因为调用方没传就退化成「只能外部打开」',
    );
  });

  testWidgets('没有命中位置时只提供打开，不假装有定位点', (tester) async {
    final dir = Directory.systemTemp.createTempSync('original2');
    addTearDown(() => dir.deleteSync(recursive: true));
    final file = File('${dir.path}/b.pdf')..writeAsBytesSync(List.filled(8, 0));
    final api = _LocateApi(available: true, handle: file.path);
    await tester.pumpWidget(
      MaterialApp(
        home: Scaffold(
          body: SingleChildScrollView(
            child: SourceOriginalCard(api: api, sourceId: 'src-2'),
          ),
        ),
      ),
    );
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 50));

    expect(find.textContaining('定位到'), findsNothing);
    expect(find.textContaining('用系统程序打开'), findsOneWidget);
    expect(api.locators.single.type, LocatorType.file);
  });
}

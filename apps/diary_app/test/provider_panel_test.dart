import 'package:diary_api/diary_api.dart';
import 'package:diary_mock/diary_mock.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:unwritten_diary/main.dart';
import 'package:unwritten_diary/src/ui_strings.dart';

/// 真实安装版的行为：能保存、能列模型、能测试，服务列表会跟着变。
class _Api extends MockDiaryApi {
  final List<Provider> saved = [];
  int saves = 0;

  @override
  Future<List<Provider>> listProviders() async => List.of(saved);

  @override
  Future<Provider> saveProvider(Provider provider) async {
    saves++;
    saved.removeWhere((p) => p.id == provider.id);
    saved.add(provider);
    return provider;
  }

  @override
  Future<Provider> saveProviderCredential({
    required String providerId,
    required String secret,
  }) async => const Provider(
        id: 'unused',
        label: 'x',
        adapterId: 'openai_compatible',
        capabilities: {ProviderCapability.generateText},
        enabled: true,
      );

  @override
  Future<List<ProviderModel>> listProviderModels({
    required String providerId,
  }) async => const [
        ProviderModel(id: 'deepseek-chat'),
        ProviderModel(id: 'deepseek-reasoner'),
      ];

  @override
  Future<ProviderTestReport> testProvider({
    required String providerId,
    required Set<ProviderCapability> scope,
  }) async => ProviderTestReport(
        providerId: providerId,
        results: const {ProviderCapability.generateText: ProviderTestStatus.ok},
        actualCallMade: true,
      );
}

Future<void> _openForm(WidgetTester tester, _Api api) async {
  tester.view.physicalSize = const Size(1280, 720);
  tester.view.devicePixelRatio = 1;
  addTearDown(tester.view.resetPhysicalSize);
  addTearDown(tester.view.resetDevicePixelRatio);
  await tester.pumpWidget(DiaryApp(api: api));
  await tester.pumpAndSettle();
  await tester.tap(find.text('我的').last);
  await tester.pumpAndSettle();
  await tester.ensureVisible(find.text('模型服务'));
  await tester.tap(find.text('模型服务'));
  await tester.pumpAndSettle();
  await tester.enterText(find.widgetWithText(TextField, 'API Key'), 'sk-test');
  await tester.pumpAndSettle();
}

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();
  setUpAll(() async => uiStrings.load());

  testWidgets('连点「获取模型列表」只产生一条服务，不会重复', (tester) async {
    final api = _Api();
    await _openForm(tester, api);

    await tester.ensureVisible(find.text('获取模型列表').first);
    await tester.tap(find.text('获取模型列表').first);
    await tester.pumpAndSettle();
    await tester.tap(find.text('获取模型列表').first);
    await tester.pumpAndSettle();

    expect(api.saved.length, 1, reason: '两次点击应当更新同一条服务');
    expect(api.saved.single.id, isNotEmpty);
  });

  testWidgets('点「获取模型列表」后表单位置不跳', (tester) async {
    final api = _Api();
    await _openForm(tester, api);

    // 先滚到按钮那儿（这一步本身会移动表单），再取基准位置。
    await tester.ensureVisible(find.text('获取模型列表').first);
    await tester.pumpAndSettle();
    final before = tester.getTopLeft(find.text('添加服务')).dy;

    await tester.tap(find.text('获取模型列表').first);
    await tester.pumpAndSettle();
    final after = tester.getTopLeft(find.text('添加服务')).dy;

    expect(
      after,
      closeTo(before, 2),
      reason: '「已配置」在上面长出一条，不该把表单顶走',
    );
  });

  testWidgets('点「测试连接」后表单位置不跳，且不重复建服务', (tester) async {
    final api = _Api();
    await _openForm(tester, api);

    await tester.ensureVisible(find.text('测试连接').last);
    await tester.pumpAndSettle();
    final before = tester.getTopLeft(find.text('添加服务')).dy;
    await tester.tap(find.text('测试连接').last);
    await tester.pumpAndSettle();
    final after = tester.getTopLeft(find.text('添加服务')).dy;

    expect(after, closeTo(before, 2), reason: '测试连接后表单不该跳');
    expect(api.saved.length, 1);
  });
}

import 'dart:convert';
import 'dart:io';

/// 用户对「把当天材料发送到这个服务」的**一次性授权**。
///
/// 这是客户端同意记录，不是核心数据：核心只管「能不能发」，管不了「用户同不同意」。
/// 存在 support 目录下的小文件里，和 `ai-diary-ui.json` 同一类东西。
///
/// 授权按**服务 id** 记：换一个服务要重新确认，改同一个服务的地址也要重新确认
/// （地址变了就是发到别处去了，见 [ProviderConsent.revoke]）。
class ProviderConsent {
  ProviderConsent(this.file);

  final File file;

  Future<Set<String>> load() async {
    if (!await file.exists()) return {};
    try {
      final value = jsonDecode(await file.readAsString());
      final granted = (value as Map)['granted'];
      return (granted as List? ?? const []).whereType<String>().toSet();
    } on Object {
      // 读坏了就当没授权过：宁可多问一次，也不能默认放行。
      return {};
    }
  }

  Future<void> grant(String providerId) async {
    final granted = await load()..add(providerId);
    await _write(granted);
  }

  Future<void> revoke(String providerId) async {
    final granted = await load()..remove(providerId);
    await _write(granted);
  }

  Future<void> _write(Set<String> granted) async {
    await file.parent.create(recursive: true);
    await file.writeAsString(
      jsonEncode({'granted': granted.toList()..sort()}),
      flush: true,
    );
  }
}

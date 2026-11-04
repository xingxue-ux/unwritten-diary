import 'dart:async';

import 'package:diary_api/diary_api.dart';
import 'package:flutter/material.dart';

import 'sketch_ui.dart';

/// 常用在线服务的预填值。
///
/// **只是省打字**：核心有自己的预设表（`crates/diary_core/src/provider/presets.rs`），
/// 但那条 `providers.presets` 还没经 frb 桥接到 Dart。这里抄的是同一份 endpoint /
/// 模型名，用户随时可以改；真正的校验在核心（保存与连接测试都会重新判断）。
class _QuickFill {
  const _QuickFill(this.id, this.label, this.endpoint, this.model, this.hint);
  final String id;
  final String label;
  final String endpoint;
  final String model;
  final String hint;
}

const List<_QuickFill> _quickFills = [
  _QuickFill(
    'deepseek',
    'DeepSeek',
    'https://api.deepseek.com',
    'deepseek-chat',
    '要填 API Key。地址填到域名，不要带 /chat/completions。',
  ),
  _QuickFill(
    'openai',
    'OpenAI',
    'https://api.openai.com/v1',
    'gpt-4o-mini',
    '要填 API Key。地址带 /v1。',
  ),
  _QuickFill(
    'ollama',
    '本机 Ollama',
    'http://127.0.0.1:11434/v1',
    '',
    '本机服务不用填密钥，模型名照 `ollama list` 里那个填。',
  ),
];

/// 模型服务：配一个能生成日记的服务。
///
/// 之前这一页只允许本机地址（`localhost` / `127.0.0.1`），在线服务没有入口——
/// 用户在界面上根本配不出 DeepSeek。现在在线与本机走同一张表，区别只在要不要密钥。
class AiProviderPanel extends StatefulWidget {
  const AiProviderPanel({super.key, required this.api});
  final DiaryApi api;

  @override
  State<AiProviderPanel> createState() => _AiProviderPanelState();
}

class _AiProviderPanelState extends State<AiProviderPanel> {
  final _name = TextEditingController();
  final _endpoint = TextEditingController();
  final _model = TextEditingController();
  final _secret = TextEditingController();
  List<Provider> _providers = const [];
  final Map<String, List<ProviderModel>> _models = {};
  String _fill = 'deepseek';
  bool _busy = false;
  bool _obscure = true;
  bool _showDetail = false;
  String? _message;
  String? _detail;
  bool _messageIsError = false;

  @override
  void initState() {
    super.initState();
    _applyFill('deepseek');
    unawaited(_reload());
  }

  @override
  void dispose() {
    _name.dispose();
    _endpoint.dispose();
    _model.dispose();
    _secret.dispose();
    super.dispose();
  }

  void _applyFill(String id) {
    final fill = _quickFills.firstWhere((item) => item.id == id);
    setState(() {
      _fill = id;
      _name.text = fill.label;
      _endpoint.text = fill.endpoint;
      _model.text = fill.model;
    });
  }

  /// 只给人看一句话；核心那句又长又技术的诊断放进 [detail]，默认折叠。
  void _say(String text, {bool error = false, String? detail}) {
    if (!mounted) return;
    setState(() {
      _message = text;
      _detail = detail;
      _showDetail = false;
      _messageIsError = error;
    });
  }

  Future<void> _reload() async {
    try {
      final value = await widget.api.listProviders();
      if (mounted) setState(() => _providers = value);
    } on Object {
      _say('暂时读不到已配置的服务。', error: true);
    }
  }

  bool get _isLoopback {
    final uri = Uri.tryParse(_endpoint.text.trim());
    if (uri == null) return false;
    return const {'localhost', '127.0.0.1', '::1'}
        .contains(uri.host.toLowerCase());
  }

  Future<void> _save() async {
    if (_busy) return;
    final raw = _endpoint.text.trim();
    final uri = Uri.tryParse(raw);
    if (uri == null || !{'http', 'https'}.contains(uri.scheme) || uri.host.isEmpty) {
      _say('地址要写成 http(s)://…，例如 https://api.deepseek.com。', error: true);
      return;
    }
    final model = _model.text.trim();
    if (model.isEmpty) {
      _say('请填写模型名，例如 deepseek-chat。', error: true);
      return;
    }
    final secret = _secret.text.trim();
    if (secret.isEmpty && !_isLoopback) {
      _say('在线服务要填 API Key；只有本机地址可以留空。', error: true);
      return;
    }

    setState(() {
      _busy = true;
      _message = null;
    });
    try {
      final saved = await widget.api.saveProvider(
        Provider(
          id: 'prv-${DateTime.now().microsecondsSinceEpoch}',
          label: _name.text.trim().isEmpty ? model : _name.text.trim(),
          adapterId: 'openai_compatible',
          capabilities: const {ProviderCapability.generateText},
          enabled: true,
          endpoint: uri.toString(),
          model: model,
        ),
      );
      if (secret.isNotEmpty) {
        // `ProviderCredentialPort` 与 `DiaryApi` 没有子类型关系，`is` 不会提升类型，
        // 所以这里显式转一次。
        final api = widget.api;
        if (api is ProviderCredentialPort) {
          await (api as ProviderCredentialPort).saveProviderCredential(
            providerId: saved.id,
            secret: secret,
          );
        } else {
          _say('这个构建不能保存密钥，服务已保存但没有密钥。', error: true);
        }
      }
      _secret.clear();
      await _reload();
      _say('已保存「${saved.label}」。');
      // 保存成功后顺手拉一次模型列表：模型名不该让人手打。
      await _loadModels(saved, quiet: true);
    } on DiaryException catch (failure) {
      _say(failure.message, error: true);
    } on Object catch (failure) {
      _say('保存失败：$failure', error: true);
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  Future<void> _test(Provider provider) async {
    if (_busy) return;
    setState(() {
      _busy = true;
      _message = '正在向 ${provider.label} 发一次真实请求…';
      _messageIsError = false;
    });
    try {
      final report = await widget.api.testProvider(
        providerId: provider.id,
        scope: const {ProviderCapability.generateText},
      );
      await _reload();
      final ok =
          report.results[ProviderCapability.generateText] ==
          ProviderTestStatus.ok;
      _say(
        ok ? '连接正常。' : '没连上。检查地址、模型名和密钥。',
        error: !ok,
        detail: report.message,
      );
    } on DiaryException catch (failure) {
      _say(failure.message, error: true);
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  /// 拉某个服务的模型列表。失败时**不打扰**（很多服务没有 `/models`，
  /// 手填模型名照样能用），只有用户主动点才把原因说出来。
  Future<void> _loadModels(Provider provider, {bool quiet = false}) async {
    final api = widget.api;
    if (api is! ProviderModelPort) {
      if (!quiet) _say('这个构建不能列出模型，直接手填模型名就行。');
      return;
    }
    try {
      final models = await (api as ProviderModelPort).listProviderModels(
        providerId: provider.id,
      );
      if (!mounted) return;
      setState(() => _models[provider.id] = models);
      if (!quiet && models.isEmpty) {
        _say('这个服务没有返回模型列表，直接手填模型名就行。');
      }
    } on DiaryException catch (failure) {
      if (!quiet) {
        _say('拿不到模型列表，直接手填模型名也行。', detail: failure.message);
      }
    } on Object catch (failure) {
      if (!quiet) _say('拿不到模型列表。', detail: '$failure');
    }
  }

  Future<void> _pickModel(Provider provider, String model) async {
    if (_busy) return;
    setState(() => _busy = true);
    try {
      await widget.api.saveProvider(provider.copyWith(model: model));
      await _reload();
      _say('模型改成 $model。');
    } on DiaryException catch (failure) {
      _say(failure.message, error: true);
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  Future<void> _remove(Provider provider) async {
    if (_busy) return;
    setState(() => _busy = true);
    try {
      await widget.api.removeProvider(provider.id);
      await _reload();
      _say('已移除「${provider.label}」。');
    } on DiaryException catch (failure) {
      _say(failure.message, error: true);
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  String _statusOf(Provider provider) {
    if (!provider.enabled) return '已停用';
    return switch (provider.testStatus) {
      ProviderTestStatus.ok => '连接正常',
      ProviderTestStatus.failed => '上次连接失败',
      ProviderTestStatus.untested => '还没测过',
    };
  }

  @override
  Widget build(BuildContext context) {
    final fill = _quickFills.firstWhere((item) => item.id == _fill);
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        const Text(
          '模型服务',
          style: TextStyle(
            fontFamily: SketchFonts.display,
            color: SketchColors.ink,
            fontSize: 40,
          ),
        ),
        const SizedBox(height: 8),
        const Text(
          '配一个能写日记的服务。材料只在你按下生成时交给它。',
          style: TextStyle(color: SketchColors.text, height: 1.5),
        ),
        const SizedBox(height: 18),
        SketchFrame(
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              const Text('已配置', style: TextStyle(fontWeight: FontWeight.w700)),
              if (_providers.isEmpty)
                const Padding(
                  padding: EdgeInsets.only(top: 12),
                  child: Text('还没有服务。在下面选一个填好保存就行。'),
                ),
              for (final provider in _providers) ...[
                const Divider(color: SketchColors.line),
                Text(
                  provider.label,
                  style: const TextStyle(fontWeight: FontWeight.w700),
                ),
                Text(
                  '${provider.model ?? '模型未填'} · ${provider.endpoint ?? '地址未填'}',
                  style: const TextStyle(color: SketchColors.muted),
                ),
                Text(
                  '${_statusOf(provider)}'
                  '${provider.credentialRef == null ? ' · 没有密钥' : ''}',
                  style: const TextStyle(color: SketchColors.muted),
                ),
                const SizedBox(height: 8),
                Wrap(
                  spacing: 10,
                  children: [
                    SketchAction(
                      label: '测试连接',
                      onPressed: _busy ? null : () => unawaited(_test(provider)),
                    ),
                    SketchAction(
                      label: '换模型',
                      onPressed: _busy
                          ? null
                          : () => unawaited(_loadModels(provider)),
                    ),
                    SketchAction(
                      label: '移除',
                      onPressed: _busy
                          ? null
                          : () => unawaited(_remove(provider)),
                    ),
                  ],
                ),
                if (_models[provider.id]?.isNotEmpty ?? false) ...[
                  const SizedBox(height: 10),
                  Wrap(
                    spacing: 8,
                    runSpacing: 8,
                    children: [
                      for (final model in _models[provider.id]!)
                        ChoiceChip(
                          label: Text(model.id),
                          selected: provider.model == model.id,
                          onSelected: _busy
                              ? null
                              : (_) => unawaited(
                                  _pickModel(provider, model.id),
                                ),
                        ),
                    ],
                  ),
                ],
              ],
            ],
          ),
        ),
        const SizedBox(height: 18),
        SketchFrame(
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
              const Text('添加服务', style: TextStyle(fontWeight: FontWeight.w700)),
              const SizedBox(height: 12),
              Wrap(
                spacing: 8,
                runSpacing: 8,
                children: [
                  for (final item in _quickFills)
                    ChoiceChip(
                      label: Text(item.label),
                      selected: _fill == item.id,
                      onSelected: _busy ? null : (_) => _applyFill(item.id),
                    ),
                ],
              ),
              const SizedBox(height: 8),
              Text(
                fill.hint,
                style: const TextStyle(color: SketchColors.muted, fontSize: 13),
              ),
              const SizedBox(height: 12),
              TextField(
                controller: _name,
                decoration: const InputDecoration(labelText: '名称'),
              ),
              TextField(
                controller: _endpoint,
                decoration: const InputDecoration(
                  labelText: '服务地址',
                  hintText: 'https://api.deepseek.com',
                ),
              ),
              TextField(
                controller: _model,
                decoration: const InputDecoration(
                  labelText: '模型名',
                  hintText: 'deepseek-chat',
                  helperText: '保存后会自动拉一次可选模型，也可以直接手填',
                ),
              ),
              TextField(
                controller: _secret,
                obscureText: _obscure,
                decoration: InputDecoration(
                  labelText: 'API Key',
                  hintText: '本机服务可以留空',
                  suffixIcon: IconButton(
                    icon: Icon(
                      _obscure ? Icons.visibility_off : Icons.visibility,
                      size: 18,
                    ),
                    onPressed: () => setState(() => _obscure = !_obscure),
                  ),
                ),
              ),
              const SizedBox(height: 12),
              SketchAction(
                label: _busy ? '处理中…' : '保存服务',
                onPressed: _busy ? null : () => unawaited(_save()),
              ),
            ],
          ),
        ),
        if (_message != null) ...[
          const SizedBox(height: 14),
          SketchFrame(
            fill: _messageIsError ? SketchColors.soft : SketchColors.paper,
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Text(_message!),
                // 核心那句诊断（端点可达 / 认证通过 / 发起了几次请求…）默认收起来：
                // 对用户有用的只有「通没通」，其余是排查时才看的东西。
                if (_detail != null && _detail!.isNotEmpty) ...[
                  const SizedBox(height: 6),
                  GestureDetector(
                    onTap: () => setState(() => _showDetail = !_showDetail),
                    child: Text(
                      _showDetail ? '收起详情' : '详情',
                      style: const TextStyle(
                        color: SketchColors.muted,
                        fontSize: 12,
                        decoration: TextDecoration.underline,
                      ),
                    ),
                  ),
                  if (_showDetail) ...[
                    const SizedBox(height: 6),
                    Text(
                      _detail!,
                      style: const TextStyle(
                        color: SketchColors.muted,
                        fontSize: 12,
                        height: 1.5,
                      ),
                    ),
                  ],
                ],
              ],
            ),
          ),
        ],
      ],
    );
  }
}

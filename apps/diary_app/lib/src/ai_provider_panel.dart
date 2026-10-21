import 'dart:async';

import 'package:diary_api/diary_api.dart';
import 'package:flutter/material.dart';

import 'sketch_ui.dart';
import 'ui_strings.dart';

/// A narrow, usable local-provider setup while remote key storage is pending.
class AiProviderPanel extends StatefulWidget {
  const AiProviderPanel({super.key, required this.api});
  final DiaryApi api;

  @override
  State<AiProviderPanel> createState() => _AiProviderPanelState();
}

class _AiProviderPanelState extends State<AiProviderPanel> {
  final _name = TextEditingController(text: '本机模型');
  final _endpoint = TextEditingController(text: 'http://127.0.0.1:11434/v1');
  final _model = TextEditingController();
  List<Provider> _providers = const [];
  bool _busy = false;
  String? _message;

  @override
  void initState() {
    super.initState();
    unawaited(_reload());
  }

  @override
  void dispose() {
    _name.dispose();
    _endpoint.dispose();
    _model.dispose();
    super.dispose();
  }

  Future<void> _reload() async {
    try {
      final value = await widget.api.listProviders();
      if (mounted) setState(() => _providers = value);
    } on Object {
      if (mounted) setState(() => _message = '暂时无法读取模型服务配置。');
    }
  }

  Future<void> _saveLocal() async {
    if (_busy) return;
    final uri = Uri.tryParse(_endpoint.text.trim());
    if (uri == null ||
        !{'http', 'https'}.contains(uri.scheme) ||
        !{'localhost', '127.0.0.1', '::1'}.contains(uri.host.toLowerCase())) {
      setState(() => _message = '这里只添加本机地址，例如 http://127.0.0.1:11434/v1。');
      return;
    }
    if (_model.text.trim().isEmpty) {
      setState(() => _message = '请填写本机服务提供的模型名称。');
      return;
    }
    setState(() {
      _busy = true;
      _message = null;
    });
    try {
      await widget.api.saveProvider(
        Provider(
          id: 'local-${DateTime.now().microsecondsSinceEpoch}',
          label: _name.text.trim().isEmpty ? '本机模型' : _name.text.trim(),
          adapterId: 'openai_compatible',
          capabilities: const {ProviderCapability.generateText},
          enabled: true,
          endpoint: uri.toString(),
          model: _model.text.trim(),
        ),
      );
      await _reload();
      if (mounted) {
        setState(() => _message = '已保存本机服务。请点「测试生成能力」确认连接。');
      }
    } on DiaryException catch (failure) {
      if (mounted) setState(() => _message = failure.message);
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  Future<void> _test(Provider provider) async {
    if (_busy) return;
    setState(() {
      _busy = true;
      _message = '正在向 ${provider.label} 发起一次连接测试…';
    });
    try {
      final report = await widget.api.testProvider(
        providerId: provider.id,
        scope: const {ProviderCapability.generateText},
      );
      await _reload();
      if (mounted) {
        setState(
          () => _message =
              report.message ??
              (report.results[ProviderCapability.generateText] ==
                      ProviderTestStatus.ok
                  ? '生成能力已通过测试。'
                  : '测试未通过，请检查地址、模型和本机服务。'),
        );
      }
    } on DiaryException catch (failure) {
      if (mounted) setState(() => _message = failure.message);
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  @override
  Widget build(BuildContext context) => Column(
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
      Text(
        uiStrings.t('privacy.local_only'),
        style: const TextStyle(color: SketchColors.text, height: 1.5),
      ),
      const SizedBox(height: 18),
      SketchFrame(
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            const Text('已配置服务', style: TextStyle(fontWeight: FontWeight.w700)),
            if (_providers.isEmpty)
              const Padding(
                padding: EdgeInsets.only(top: 12),
                child: Text('暂无服务。可以在下面添加本机模型。'),
              ),
            for (final provider in _providers) ...[
              const Divider(color: SketchColors.line),
              Text(
                provider.label,
                style: const TextStyle(fontWeight: FontWeight.w700),
              ),
              Text(
                provider.endpoint ?? '地址未提供',
                style: const TextStyle(color: SketchColors.muted),
              ),
              Text(
                provider.enabled ? '已启用 · ${provider.testStatus.wire}' : '已停用',
                style: const TextStyle(color: SketchColors.muted),
              ),
              const SizedBox(height: 8),
              if (provider.capabilities.contains(
                ProviderCapability.generateText,
              ))
                SketchAction(
                  label: '测试生成能力',
                  onPressed: _busy ? null : () => unawaited(_test(provider)),
                ),
            ],
          ],
        ),
      ),
      const SizedBox(height: 18),
      SketchFrame(
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            const Text('添加本机模型', style: TextStyle(fontWeight: FontWeight.w700)),
            const SizedBox(height: 10),
            TextField(
              controller: _name,
              decoration: const InputDecoration(labelText: '名称'),
            ),
            TextField(
              controller: _endpoint,
              decoration: const InputDecoration(labelText: '本机 OpenAI 兼容地址'),
            ),
            TextField(
              controller: _model,
              decoration: const InputDecoration(labelText: '模型名称'),
            ),
            const SizedBox(height: 12),
            SketchAction(
              label: '保存本机服务',
              onPressed: _busy ? null : () => unawaited(_saveLocal()),
            ),
          ],
        ),
      ),
      const SizedBox(height: 14),
      const Text('在线服务的密钥配置正在完善；目前仅在此页新增本机端点。'),
      if (_message != null) ...[
        const SizedBox(height: 14),
        SketchFrame(fill: SketchColors.soft, child: Text(_message!)),
      ],
    ],
  );
}

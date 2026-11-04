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
    'API Key 必填。地址填到域名。',
  ),
  _QuickFill(
    'openai',
    'OpenAI',
    'https://api.openai.com/v1',
    'gpt-4o-mini',
    'API Key 必填。地址带 /v1。',
  ),
  _QuickFill(
    'ollama',
    '本机 Ollama',
    'http://127.0.0.1:11434/v1',
    '',
    '本机服务不用密钥。',
  ),
];

/// 一张卡片上的一句话提示。`detail` 是核心那句又长又技术的原文，默认折叠。
class _Note {
  const _Note(this.text, {this.error = false, this.detail});
  final String text;
  final bool error;
  final String? detail;
}

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
  /// 每个服务卡片**自己的**提示（「连接正常」属于那张卡片，不属于整页）。
  final Map<String, _Note> _notes = {};
  /// 「添加服务」里点「获取模型列表」拿到的候选。
  List<ProviderModel> _formModels = const [];
  /// 正在改哪个已有服务；null 表示新增。保存时用它决定是更新还是新建。
  String? _editingId;
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

  /// 「添加服务」表单里的一句话；核心那句又长又技术的诊断放进 [detail]，默认折叠。
  void _say(String text, {bool error = false, String? detail}) {
    if (!mounted) return;
    setState(() {
      _message = text;
      _detail = detail;
      _showDetail = false;
      _messageIsError = error;
    });
  }

  /// 某张服务卡片自己的提示。**连接结果属于那张卡片**，不该跑到页面底部去。
  void _sayTo(String providerId, String text, {bool error = false, String? detail}) {
    if (!mounted) return;
    setState(() => _notes[providerId] = _Note(text, error: error, detail: detail));
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

  /// 表单里的地址是否可用；不可用时把原因写进表单提示并返回 null。
  Uri? _checkedEndpoint() {
    final uri = Uri.tryParse(_endpoint.text.trim());
    if (uri == null ||
        !{'http', 'https'}.contains(uri.scheme) ||
        uri.host.isEmpty) {
      _say('地址要写成 http(s)://…，例如 https://api.deepseek.com。', error: true);
      return null;
    }
    return uri;
  }

  /// 把表单里的值落库（有 `_editingId` 就更新那一条，否则新建）。
  ///
  /// 「获取模型列表」也要用它：核心只能按**已保存的服务**列模型，
  /// 所以那一步会先把表单落一份——这正是「添加服务」的一部分。
  Future<Provider?> _persist(Uri uri, {required String model}) async {
    final secret = _secret.text.trim();
    final saved = await widget.api.saveProvider(
      Provider(
        id: _editingId ?? 'prv-${DateTime.now().microsecondsSinceEpoch}',
        label: _name.text.trim().isEmpty
            ? (model.isEmpty ? uri.host : model)
            : _name.text.trim(),
        adapterId: 'openai_compatible',
        capabilities: const {ProviderCapability.generateText},
        enabled: true,
        endpoint: uri.toString(),
        model: model.isEmpty ? null : model,
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
    return saved;
  }

  Future<void> _save() async {
    if (_busy) return;
    final uri = _checkedEndpoint();
    if (uri == null) return;
    final model = _model.text.trim();
    if (model.isEmpty) {
      _say('请填写模型名，例如 deepseek-chat。', error: true);
      return;
    }
    if (_secret.text.trim().isEmpty && !_isLoopback) {
      _say('在线服务要填 API Key。', error: true);
      return;
    }

    setState(() {
      _busy = true;
      _message = null;
    });
    try {
      final saved = await _persist(uri, model: model);
      if (saved == null) return;
      setState(() {
        _editingId = null;
        _formModels = const [];
      });
      _say('已保存「${saved.label}」。');
    } on DiaryException catch (failure) {
      _say(failure.message, error: true);
    } on Object catch (failure) {
      _say('保存失败：$failure', error: true);
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  /// 「添加服务」里的**获取模型列表**：用表单里的地址与密钥去问服务提供哪些模型。
  ///
  /// 核心的 `providers.models` 只按已保存的服务查（它要读 SecretStore），
  /// 所以这里先把表单落一份——用户此刻本来就在「添加服务」，落库是这条流程的一部分。
  Future<void> _loadModelsForForm() async {
    if (_busy) return;
    final uri = _checkedEndpoint();
    if (uri == null) return;
    if (_secret.text.trim().isEmpty && !_isLoopback) {
      _say('先填 API Key，才能问服务有哪些模型。', error: true);
      return;
    }
    final api = widget.api;
    if (api is! ProviderModelPort) {
      _say('这个构建不能列出模型。', error: true);
      return;
    }
    setState(() {
      _busy = true;
      _message = null;
      _formModels = const [];
    });
    try {
      final saved = await _persist(uri, model: _model.text.trim());
      if (saved == null) return;
      final models = await (api as ProviderModelPort).listProviderModels(
        providerId: saved.id,
      );
      if (!mounted) return;
      setState(() => _formModels = models);
      _say(
        models.isEmpty
            ? '这个服务里还没有模型。'
            : '这个服务提供 ${models.length} 个模型，点一个填进「模型名」。',
      );
    } on DiaryException catch (failure) {
      _say(failure.message, error: true);
    } on Object catch (failure) {
      _say('拿不到模型列表。', detail: '$failure', error: true);
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  /// 把某个已有服务填回「添加服务」表单，改完保存是**更新那一条**，不会多出一条。
  void _editProvider(Provider provider) {
    setState(() {
      _editingId = provider.id;
      _fill = '';
      _name.text = provider.label;
      _endpoint.text = provider.endpoint ?? '';
      _model.text = provider.model ?? '';
      _secret.clear();
      _formModels = const [];
      _message = null;
      _detail = null;
    });
  }

  Future<void> _test(Provider provider) async {
    if (_busy) return;
    setState(() => _busy = true);
    _sayTo(provider.id, '正在发一次真实请求…');
    try {
      final report = await widget.api.testProvider(
        providerId: provider.id,
        scope: const {ProviderCapability.generateText},
      );
      await _reload();
      final ok =
          report.results[ProviderCapability.generateText] ==
          ProviderTestStatus.ok;
      _sayTo(
        provider.id,
        ok ? '连接正常。' : '没连上。检查地址、模型名和密钥。',
        error: !ok,
        detail: report.message,
      );
    } on DiaryException catch (failure) {
      _sayTo(provider.id, failure.message, error: true);
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  /// 表单里点一个候选：只填进「模型名」，保存仍由用户按。
  void _pickFormModel(String id) {
    setState(() {
      _model.text = id;
      _formModels = const [];
    });
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
          '配一个能写日记的服务。',
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
                  child: Text('还没有服务。'),
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
                      label: '改模型',
                      onPressed: _busy ? null : () => _editProvider(provider),
                    ),
                    SketchAction(
                      label: '移除',
                      onPressed: _busy
                          ? null
                          : () => unawaited(_remove(provider)),
                    ),
                  ],
                ),
                // 连接结果就写在这张卡片里——它说的是这个服务，不是整页。
                if (_notes[provider.id] case final note?) ...[
                  const SizedBox(height: 8),
                  _NoteLine(note: note),
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
                  helperText: '不知道填什么就点下面的「获取模型列表」',
                ),
              ),
              const SizedBox(height: 8),
              Align(
                alignment: Alignment.centerLeft,
                child: SketchAction(
                  label: '获取模型列表',
                  onPressed: _busy
                      ? null
                      : () => unawaited(_loadModelsForForm()),
                ),
              ),
              if (_formModels.isNotEmpty) ...[
                const SizedBox(height: 10),
                Wrap(
                  spacing: 8,
                  runSpacing: 8,
                  children: [
                    for (final model in _formModels)
                      ChoiceChip(
                        label: Text(model.id),
                        selected: _model.text.trim() == model.id,
                        onSelected: _busy
                            ? null
                            : (_) => _pickFormModel(model.id),
                      ),
                  ],
                ),
              ],
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
                label: _busy
                    ? '处理中…'
                    : (_editingId == null ? '保存服务' : '更新这个服务'),
                onPressed: _busy ? null : () => unawaited(_save()),
              ),
              // 表单自己的提示留在这张卡片里：它说的是「这次填写」，不是整页。
              if (_message != null) ...[
                const SizedBox(height: 12),
                _NoteLine(
                  note: _Note(
                    _message!,
                    error: _messageIsError,
                    detail: _detail,
                  ),
                  expanded: _showDetail,
                  onToggle: () => setState(() => _showDetail = !_showDetail),
                ),
              ],
            ],
          ),
        ),
      ],
    );
  }
}

/// 一句话提示 + 可折叠的原始诊断。
class _NoteLine extends StatelessWidget {
  const _NoteLine({
    required this.note,
    this.expanded = false,
    this.onToggle,
  });

  final _Note note;
  final bool expanded;
  final VoidCallback? onToggle;

  @override
  Widget build(BuildContext context) {
    final detail = note.detail;
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Text(
          note.text,
          style: TextStyle(
            color: note.error ? SketchColors.ink : SketchColors.muted,
          ),
        ),
        // 核心那句诊断（端点可达 / 认证通过 / 发起了几次请求…）默认收起来：
        // 对用户有用的只有「通没通」，其余是排查时才看的东西。
        if (detail != null && detail.isNotEmpty && onToggle != null) ...[
          const SizedBox(height: 4),
          GestureDetector(
            onTap: onToggle,
            child: Text(
              expanded ? '收起详情' : '详情',
              style: const TextStyle(
                color: SketchColors.muted,
                fontSize: 12,
                decoration: TextDecoration.underline,
              ),
            ),
          ),
          if (expanded)
            Padding(
              padding: const EdgeInsets.only(top: 4),
              child: Text(
                detail,
                style: const TextStyle(
                  color: SketchColors.muted,
                  fontSize: 12,
                  height: 1.5,
                ),
              ),
            ),
        ],
      ],
    );
  }
}

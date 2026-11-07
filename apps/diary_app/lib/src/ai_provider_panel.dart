import 'dart:async';
import 'dart:io';

import 'package:path/path.dart' as path;
import 'package:path_provider/path_provider.dart';

import 'package:diary_api/diary_api.dart';
import 'package:flutter/material.dart';

import 'provider_consent.dart';
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
    // 用 `/models` 真列出来的那个，别用历史别名：别名还能路由，但不在列表里，
    // 用户看到「列表里没有这个名字」会以为配错了。
    'deepseek-flash',
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

  /// 「添加服务」里**这一次填写**对应的服务 id。
  ///
  /// 「获取模型列表」「测试连接」都得先把表单落库（核心只按已保存的服务跑），
  /// 落完要记住这个 id——不然每点一次就多出一条重复的服务。
  String? _formProviderId;

  /// 「添加服务」这张卡片的锚点：表单上方内容变高变矮时，用它把位置钉住。
  final GlobalKey _formKey = GlobalKey();

  /// 已经授权「把材料发送到这个服务」的服务 id。授权一次，之后不再问。
  ProviderConsent? _consent;
  Set<String> _consented = const {};

  /// 表单里那个「我同意把材料发送到这个服务」的勾。
  bool _consentChecked = false;
  /// 当前选中的预填项；改已有服务时为 null（那套值不是任何预填）。
  String? _fill = 'deepseek';
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
    // 改了任何字段就是「另一条配置」：不再沿用上一次保存的 id，
    // 否则改完地址再保存会把上一条覆盖掉。
    for (final controller in [_name, _endpoint, _model]) {
      controller.addListener(_forgetFormProvider);
    }
    unawaited(_reload());
    unawaited(_loadConsent());
  }

  /// 落库那一刻的字段快照。只有**内容真的变了**才算「另一条配置」。
  ///
  /// 不能用 controller 的通知直接判断：`TextEditingController` 在**光标/选区变化**时
  /// 也会通知，点一下输入框就会把 id 清掉，于是「拿模型列表 → 点候选 → 测连接」
  /// 会多建一条服务（流程测试抓到的）。
  String _formSnapshot = '';

  String get _currentFormSnapshot =>
      '${_name.text}\u0000${_endpoint.text}\u0000${_model.text}';

  void _forgetFormProvider() {
    if (_formProviderId == null) return;
    if (_currentFormSnapshot != _formSnapshot) _formProviderId = null;
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
      // 换一个服务商就是另一条配置：之前那次填写落下的 id 不再适用。
      _formProviderId = null;
      _formModels = const [];
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

  Future<ProviderConsent?> _makeConsent() async {
    try {
      final directory = await getApplicationSupportDirectory();
      return ProviderConsent(
        File(path.join(directory.path, 'provider-consent.json')),
      );
    } on Object {
      return null;
    }
  }

  Future<void> _loadConsent() async {
    try {
      final consent = await _makeConsent();
      if (consent == null) return;
      final granted = await consent.load();
      if (!mounted) return;
      setState(() {
        _consent = consent;
        _consented = granted;
      });
    } on Object {
      // 读不到授权文件不影响配置服务本身；日记页会再问一次。
    }
  }

  /// 这个服务是不是「会把材料发出去」的远端服务。
  bool _remoteOf(Provider provider) {
    final host = Uri.tryParse(provider.endpoint ?? '')?.host.toLowerCase() ?? '';
    return host.isNotEmpty &&
        host != 'localhost' &&
        host != '127.0.0.1' &&
        host != '::1';
  }

  bool get _formIsRemote {
    final host = Uri.tryParse(_endpoint.text.trim())?.host.toLowerCase() ?? '';
    return host.isNotEmpty &&
        host != 'localhost' &&
        host != '127.0.0.1' &&
        host != '::1';
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
  /// 这个服务已经存过密钥吗？改已有服务时不该逼用户再输一遍。
  bool get _hasStoredCredential {
    // 同一个表单里保存过一次之后，密钥已经在托管里了（输入框会被清空），
    // 这时再点「保存」不该逼用户重打一遍。
    final id = _editingId ?? _formProviderId;
    if (id == null) return false;
    for (final provider in _providers) {
      if (provider.id == id) return provider.credentialRef != null;
    }
    return false;
  }

  /// 这次操作是否必须让用户填密钥：本机服务不用，改已有服务也不用。
  bool get _needsSecret => !_isLoopback && !_hasStoredCredential;

  Future<Provider?> _persist(
    Uri uri, {
    required String model,
    bool clearSecret = false,
  }) async {
    final secret = _secret.text.trim();
    final saved = await widget.api.saveProvider(
      Provider(
        id: _editingId ??
            _formProviderId ??
            'prv-${DateTime.now().microsecondsSinceEpoch}',
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
    // **只有保存完成才清空**。获取模型列表 / 测试连接之后还要接着保存，
    // 这时候把输入框清掉等于让用户重打一遍密钥。
    if (clearSecret) _secret.clear();
    _formProviderId = saved.id;
    _formSnapshot = _currentFormSnapshot;
    await _reload();
    return saved;
  }

  /// 跑一段会改变「已配置」高度、进而推动表单的动作，同时把表单钉在原位。
  ///
  /// 「已配置」在表单**上面**：它多一条，表单就往下跳。用户点的是表单里的按钮，
  /// 视线却在原地——所以这里记下表单的全局 y，动作结束后把滚动位置补回去。
  Future<void> _keepingFormStill(Future<void> Function() body) async {
    final before = _formTop();
    final position = Scrollable.maybeOf(context)?.position;
    await body();
    if (!mounted) return;
    // body 里的 setState 只是排了帧，此时量到的还是**旧布局**的位置。
    // 等这一帧画完再量，否则补的偏移量会偏小（实测少 103px）。
    await WidgetsBinding.instance.endOfFrame;
    if (!mounted) return;
    final after = _formTop();
    if (before == null || after == null || position == null) return;
    final delta = after - before;
    if (delta.abs() < 1) return;
    final target = (position.pixels + delta).clamp(
      0.0,
      position.maxScrollExtent,
    );
    if ((target - position.pixels).abs() >= 1) position.jumpTo(target);
  }

  /// 「添加服务」卡片左上角的全局 y；还没挂载时返回 null。
  double? _formTop() {
    final box = _formKey.currentContext?.findRenderObject() as RenderBox?;
    if (box == null || !box.attached) return null;
    return box.localToGlobal(Offset.zero).dy;
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
    if (_secret.text.trim().isEmpty && _needsSecret) {
      _say('在线服务要填 API Key。', error: true);
      return;
    }

    setState(() {
      _busy = true;
      _message = null;
    });
    await _keepingFormStill(() async {
      try {
        // 不在这时清空密钥：改个名字再保存是常见操作，清掉就得重打一遍。
        // 换预填项、点「编辑」时才会清（那才是「另一份表单」）。
        final saved = await _persist(uri, model: model);
        if (saved == null) return;
        // 授权只在这里落：保存是一次明确的「我要用它」。
        // 「获取模型列表」「测试连接」也会落库，但那两步不是授权动作。
        if (_formIsRemote && _consentChecked) {
          await _consent?.grant(saved.id);
          if (mounted) {
            setState(() => _consented = {..._consented, saved.id});
          }
        }
        setState(() {
          _editingId = null;
          // **不清 `_formProviderId`**：同一个表单里再点一次「保存」应当更新刚才那条，
          // 而不是再建一条一模一样的。改了字段才会清（见构造函数里的监听）。
          _formModels = const [];
          _consentChecked = false;
        });
        _say('已保存「${saved.label}」。');
      } on DiaryException catch (failure) {
        _say(failure.message, error: true);
      } on Object catch (failure) {
        _say('保存失败：$failure', error: true);
      } finally {
        if (mounted) setState(() => _busy = false);
      }
    });
  }

  /// 「添加服务」里的**获取模型列表**：用表单里的地址与密钥去问服务提供哪些模型。
  ///
  /// 核心的 `providers.models` 只按已保存的服务查（它要读 SecretStore），
  /// 所以这里先把表单落一份——用户此刻本来就在「添加服务」，落库是这条流程的一部分。
  Future<void> _loadModelsForForm() async {
    if (_busy) return;
    final uri = _checkedEndpoint();
    if (uri == null) return;
    if (_secret.text.trim().isEmpty && _needsSecret) {
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
    await _keepingFormStill(() async {
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
    });
  }

  /// 「添加服务」里的**测试连接**：用表单里的值真发一次请求。
  ///
  /// 与「获取模型列表」同一口径——核心的 `providers.test` 只按已保存的服务测，
  /// 所以先把表单落一份再测。
  Future<void> _testForm() async {
    if (_busy) return;
    final uri = _checkedEndpoint();
    if (uri == null) return;
    final model = _model.text.trim();
    if (model.isEmpty) {
      _say('先填模型名，或者点「获取模型列表」挑一个。', error: true);
      return;
    }
    if (_secret.text.trim().isEmpty && _needsSecret) {
      _say('先填 API Key，才能测连接。', error: true);
      return;
    }
    setState(() {
      _busy = true;
      _message = null;
    });
    await _keepingFormStill(() async {
      try {
        final saved = await _persist(uri, model: model);
        if (saved == null) return;
        final report = await widget.api.testProvider(
          providerId: saved.id,
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
      } on Object catch (failure) {
        _say('测试没跑完。', detail: '$failure', error: true);
      } finally {
        if (mounted) setState(() => _busy = false);
      }
    });
  }

  /// 当前「用于整理」的那个服务。
  ///
  /// 不能拿 `enabled` 当判据：新建的服务默认就是启用的，两个服务都会显示
  /// 「正在用于整理」，而且**两个都点不动**——用户被卡死。
  /// 只有「恰好一个启用」才算选过；多个启用说明还没选。
  String? get _selectedId {
    final candidates = _providers.where(_canGenerate).toList(growable: false);
    final enabled = candidates.where((item) => item.enabled).toList(growable: false);
    return enabled.length == 1 ? enabled.single.id : null;
  }

  /// 这个服务能不能承担「整理日记」：在线 + 声明了生成文字。
  bool _canGenerate(Provider provider) =>
      provider.capabilities.contains(ProviderCapability.generateText) &&
      !_isLocalAdapter(provider);

  /// 本机引擎不做文本生成（它的 model 是模型目录），别让它出现在「用它整理」里。
  bool _isLocalAdapter(Provider provider) =>
      provider.adapterId != 'openai_compatible';

  /// 指定用它整理。核心的语义是单选：选中的启用，其余在线生成服务停用。
  Future<void> _selectForOrganize(Provider provider) async {
    final api = widget.api;
    if (api is! ProviderSelectionPort) {
      _say('这个构建不能指定整理服务。', error: true);
      return;
    }
    setState(() => _busy = true);
    try {
      await (api as ProviderSelectionPort).selectGenerationProvider(
        providerId: provider.id,
      );
      await _reload();
      _say('之后整理日记都用「${provider.label}」。');
    } on DiaryException catch (failure) {
      _say(failure.message, error: true);
    } on Object catch (failure) {
      // 只接 `DiaryException` 是不够的：桥层崩了、平台通道断了，都会是别的类型，
      // 那样异常会直接冒到 widget 层变成红屏。这里兜住，至少让用户看到一句话。
      _say('移除没完成。', detail: '$failure', error: true);
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  Future<void> _toggleConsent(Provider provider) async {
    final consent = _consent ?? await _makeConsent();
    if (consent == null) return;
    _consent = consent;
    final wasGranted = _consented.contains(provider.id);
    if (wasGranted) {
      await consent.revoke(provider.id);
    } else {
      await consent.grant(provider.id);
    }
    if (!mounted) return;
    // 本地直接改，不再回读文件：这一步只是把刚写下去的结果反映到界面，
    // 多一次磁盘读只会让按钮看起来「点了没反应」。
    setState(() {
      _consented = wasGranted
          ? ({..._consented}..remove(provider.id))
          : {..._consented, provider.id};
    });
  }

  /// 把某个已有服务填回「添加服务」表单，改完保存是**更新那一条**，不会多出一条。
  void _editProvider(Provider provider) {
    setState(() {
      _editingId = provider.id;
      _formProviderId = null;
      _fill = null;
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
  ///
  /// **要保住 `_formProviderId`**：点候选会改 `_model`，而改字段的监听会把它清掉，
  /// 于是「拿模型列表 → 选一个 → 测连接」会多建一条服务（真机流程测试抓到的）。
  /// 选模型是同一份表单里的动作，不是「另一条配置」。
  void _pickFormModel(String id) {
    setState(() {
      _model.text = id;
      _formModels = const [];
    });
    // 选模型是同一份表单里的动作：把快照跟上，避免被判成「另一条配置」。
    if (_formProviderId != null) _formSnapshot = _currentFormSnapshot;
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
    // 改已有服务时 `_fill` 是 null：这里不能 firstWhere，否则抛 StateError
    // （「点编辑就出错」就是这么来的）。
    final fill = _quickFills.where((item) => item.id == _fill).firstOrNull;
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
                // 远端服务：把「材料会发出去」这件事在配置时就讲清楚，并记下授权。
                if (_remoteOf(provider))
                  Padding(
                    padding: const EdgeInsets.only(top: 2),
                    child: Text(
                      _consented.contains(provider.id)
                          ? '已授权发送材料 · 可在下面撤销'
                          : '还没授权发送材料 · 日记页会拦住',
                      style: TextStyle(
                        color: _consented.contains(provider.id)
                            ? SketchColors.muted
                            : SketchColors.ink,
                        fontSize: 13,
                      ),
                    ),
                  ),
                const SizedBox(height: 8),
                Wrap(
                  spacing: 10,
                  children: [
                    if (_canGenerate(provider))
                      SketchAction(
                        label: _selectedId == provider.id
                            ? '正在用于整理'
                            : '用它整理',
                        filled: _selectedId == provider.id,
                        onPressed: _busy || _selectedId == provider.id
                            ? null
                            : () => unawaited(_selectForOrganize(provider)),
                      ),
                    SketchAction(
                      label: '测试连接',
                      busy: _busy,
                      onPressed: _busy ? null : () => unawaited(_test(provider)),
                    ),
                    SketchAction(
                      label: '编辑',
                      onPressed: _busy ? null : () => _editProvider(provider),
                    ),
                    if (_remoteOf(provider))
                      SketchAction(
                        label: _consented.contains(provider.id)
                            ? '撤销授权'
                            : '授权发送',
                        onPressed: _busy
                            ? null
                            : () => unawaited(_toggleConsent(provider)),
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
          key: _formKey,
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
              if (fill != null)
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
              Wrap(
                spacing: 10,
                runSpacing: 8,
                children: [
                  SketchAction(
                    label: '获取模型列表',
                    busy: _busy,
                    onPressed: _busy
                        ? null
                        : () => unawaited(_loadModelsForForm()),
                  ),
                  SketchAction(
                    label: '测试连接',
                    busy: _busy,
                    onPressed: _busy ? null : () => unawaited(_testForm()),
                  ),
                ],
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
              // 远端服务：**在这里确认一次**，之后日记页不再反复打勾。
              if (_formIsRemote) ...[
                const SizedBox(height: 4),
                CheckboxListTile(
                  contentPadding: EdgeInsets.zero,
                  value: _consentChecked ||
                      (_editingId != null &&
                          _consented.contains(_editingId)),
                  onChanged: _busy
                      ? null
                      : (value) =>
                            setState(() => _consentChecked = value ?? false),
                  title: const Text('我同意把当天材料发送到这个服务'),
                  subtitle: const Text('只需确认一次，之后不再询问。'),
                ),
              ],
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

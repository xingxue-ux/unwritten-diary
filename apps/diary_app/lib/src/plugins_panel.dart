import 'dart:async';

import 'package:diary_api/diary_api.dart';
import 'package:flutter/material.dart';

import 'sketch_ui.dart';

/// 插件页：**读真实安装列表**，开关真的落到核心。
///
/// 以前这一页是预览：一张写死的「示例导出插件」卡片，开关只改内存变量，
/// 页面自己写着「插件还没接上」。现在：
///
/// - 列表来自 `plugins.list`（核心的安装表）；
/// - 启用/停用走 `plugins.enable` / `plugins.disable`；
/// - 卸载走 `plugins.uninstall`，要确认；
/// - 安装走「选包 → `plugins.inspectPackage` 检查 → 逐项授权 → `plugins.install`」，
///   检查不通过就装不上，权限由用户自己勾。
///
/// **能力缺失时不画按钮**：`plugins.invoke` 还没接（契约是票据形状，核心是同步执行），
/// 所以这里不显示「运行」，而不是画一个按下去报错的入口。
class PluginsPanel extends StatefulWidget {
  const PluginsPanel({
    super.key,
    required this.api,
    required this.pickPackage,
    this.capabilities = const {},
  });

  final DiaryApi api;

  /// 选一个插件包文件，返回它的路径（票据）；取消返回 null。
  final Future<String?> Function() pickPackage;

  final Set<String> capabilities;

  @override
  State<PluginsPanel> createState() => _PluginsPanelState();
}

class _PluginsPanelState extends State<PluginsPanel> {
  List<Plugin> _plugins = const [];
  bool _loading = true;
  bool _busy = false;
  String? _error;
  String? _notice;

  /// 正在切换的那一个插件（按钮上转圈，别让整页都像卡住）。
  String? _pendingId;

  /// 最近一次运行的插件与结果。**只展示，不自动写进日记**。
  String? _runPluginId;
  PluginRunResult? _runResult;

  /// 用户保存过的风格。风格模板必须带一份 `StyleProfile`——核心不替用户编风格，
  /// 所以界面上得真的能选一份。
  List<StyleProfile> _styles = const [];
  bool _stylesLoaded = false;

  /// 有当前版本的日记天。代码型导出插件（Wasm，没有模板）要的是
  /// **用户选定的日记版本**——插件里 `run()` 返回 2 就是「没选日记」。
  List<DiaryDay> _daysWithVersion = const [];
  bool _daysLoaded = false;

  /// 翻到页数上限时置位：界面要如实说「只列出了最近一部分」。
  bool _daysTruncated = false;

  /// 实际扫过多少天（用于如实说明「查了多少」）。
  int _daysScanned = 0;

  /// 下次「继续往后找」从哪儿接着扫。
  String? _dayCursor;
  bool _daysLoadingMore = false;

  /// 每个插件选中的模板 / 风格 / 日期范围（按插件 id 记）。
  final Map<String, String> _templateChoice = {};
  final Map<String, String> _styleChoice = {};
  final Map<String, String> _fromDay = {};
  final Map<String, String> _toDay = {};

  /// 每个插件选中的「导出哪一天」（按插件 id 记）。
  final Map<String, String> _versionDay = {};

  bool get _canInstall =>
      widget.capabilities.contains('plugins.install') &&
      widget.capabilities.contains('plugins.inspectPackage');

  bool get _canToggle =>
      widget.capabilities.contains('plugins.enable') &&
      widget.capabilities.contains('plugins.disable');

  bool get _canUninstall => widget.capabilities.contains('plugins.uninstall');

  bool get _canInvoke => widget.capabilities.contains('plugins.invoke');

  @override
  void initState() {
    super.initState();
    unawaited(_load());
  }

  Future<void> _load() async {
    setState(() {
      _loading = true;
      _error = null;
    });
    try {
      final plugins = await widget.api.listPlugins();
      if (!mounted) return;
      setState(() => _plugins = plugins);
      await _loadStyles();
      await _loadDays();
    } on DiaryException catch (failure) {
      if (mounted) setState(() => _error = failure.message);
    } on Object catch (failure) {
      if (mounted) setState(() => _error = '$failure');
    } finally {
      if (mounted) setState(() => _loading = false);
    }
  }

  /// 风格列表只在有插件需要它时读；读不到就如实说，不假装能跑风格模板。
  Future<void> _loadStyles() async {
    if (_stylesLoaded) return;
    final needsStyles = _plugins.any(
      (plugin) => plugin.templates.any((template) => template.kind == 'style'),
    );
    if (!needsStyles) return;
    try {
      final styles = await widget.api.listStyles();
      if (!mounted) return;
      setState(() {
        _styles = styles;
        _stylesLoaded = true;
      });
    } on Object {
      if (mounted) setState(() => _stylesLoaded = true);
    }
  }

  /// 代码型导出插件要「用户选定的日记版本」，所以这里读有当前版本的天。
  ///
  /// **必须翻页**：只取第一页（以前是 30 天）时，日记落在后面的页里就会被误报成
  /// 「库里还没有任何一天生成过日记」——用户明明生成过，界面却说没有。
  /// 翻到没有下一页为止，最多 [_maxDayPages] 页；到上限时如实说「只列出了最近
  /// 一部分」，不假装全都在。
  static const _maxDayPages = 20;
  static const _dayPageSize = 100;

  Future<void> _loadDays() async {
    if (_daysLoaded) return;
    final needsVersions = _plugins.any(_needsVersions);
    if (!needsVersions) return;
    await _scanDays(fromStart: true);
  }

  /// 从上次停下的地方**继续往后找**。
  ///
  /// 扫描有上限（[`_maxDayPages`]），到上限时界面说的是「还没查完」。以前只有
  /// 「按日期选」一条路；现在也能直接接着扫——用户不用先知道是哪一天。
  Future<void> _loadMoreDays() async {
    if (_daysLoadingMore) return;
    setState(() => _daysLoadingMore = true);
    await _scanDays(fromStart: false);
    if (mounted) setState(() => _daysLoadingMore = false);
  }

  Future<void> _scanDays({required bool fromStart}) async {
    try {
      final withVersion = fromStart
          ? <DiaryDay>[]
          : List<DiaryDay>.of(_daysWithVersion);
      String? cursor = fromStart ? null : _dayCursor;
      var pages = 0;
      var truncated = false;
      while (true) {
        final page = await widget.api.listDays(
          cursor: cursor,
          pageSize: _dayPageSize,
        );
        withVersion.addAll(
          page.days
              .where((day) => day.currentVersionId != null)
              .where((day) => !withVersion.any((item) => item.dayKey == day.dayKey)),
        );
        pages++;
        cursor = page.nextCursor;
        if (cursor == null) break;
        if (pages >= _maxDayPages) {
          truncated = true;
          break;
        }
      }
      if (!mounted) return;
      setState(() {
        _daysWithVersion = withVersion;
        _daysTruncated = truncated;
        _dayCursor = cursor;
        _daysScanned += pages * _dayPageSize;
        _daysLoaded = true;
      });
    } on Object {
      if (mounted) setState(() => _daysLoaded = true);
    }
  }

  /// 这个插件要不要「选定的日记版本」：代码型（无模板）的导出插件。
  ///
  /// 声明式导出模板走的是日期范围（`fromDayKey` / `toDayKey`），代码插件走的是
  /// `selectedVersionIds`——两条路不一样，界面不能只做一半。
  bool _needsVersions(Plugin plugin) =>
      plugin.entryKind == PluginEntryKind.code &&
      plugin.activationEvents.contains('export.start');

  DiaryDay? _selectedDay(Plugin plugin) {
    if (_daysWithVersion.isEmpty) return null;
    final chosen = _versionDay[plugin.id];
    return _daysWithVersion
            .where((day) => day.dayKey == chosen)
            .firstOrNull ??
        _daysWithVersion.first;
  }

  PluginTemplate? _selectedTemplate(Plugin plugin) {
    if (plugin.templates.isEmpty) return null;
    final chosen = _templateChoice[plugin.id];
    return plugin.templates
            .where((template) => template.id == chosen)
            .firstOrNull ??
        plugin.templates.first;
  }

  StyleProfile? _selectedStyle(Plugin plugin) {
    if (_styles.isEmpty) return null;
    final chosen = _styleChoice[plugin.id];
    return _styles.where((style) => style.id == chosen).firstOrNull ??
        _styles.first;
  }

  String _todayKey() {
    final now = DateTime.now();
    final month = now.month.toString().padLeft(2, '0');
    final day = now.day.toString().padLeft(2, '0');
    return '${now.year}-$month-$day';
  }

  Future<void> _toggle(Plugin plugin, bool enable) async {
    if (_busy) return;
    setState(() {
      _busy = true;
      _pendingId = plugin.id;
      _error = null;
      _notice = null;
    });
    try {
      if (enable) {
        await widget.api.enablePlugin(plugin.id);
      } else {
        await widget.api.disablePlugin(plugin.id);
      }
      await _load();
      if (mounted) {
        setState(() => _notice = enable ? '已启用「${plugin.id}」。' : '已停用「${plugin.id}」，之后不会再被调用。');
      }
    } on DiaryException catch (failure) {
      if (mounted) setState(() => _error = failure.message);
    } on Object catch (failure) {
      if (mounted) setState(() => _error = '切换失败：$failure');
    } finally {
      if (mounted) {
        setState(() {
          _busy = false;
          _pendingId = null;
        });
      }
    }
  }

  Future<void> _uninstall(Plugin plugin) async {
    final confirmed = await showDialog<bool>(
      context: context,
      builder: (context) => AlertDialog(
        title: const Text('卸载这个插件？'),
        content: const Text('插件会被移除，它已经写进日记的内容与导出物不会被删除。'),
        actions: [
          TextButton(
            onPressed: () => Navigator.of(context).pop(false),
            child: const Text('取消'),
          ),
          TextButton(
            onPressed: () => Navigator.of(context).pop(true),
            child: const Text('卸载'),
          ),
        ],
      ),
    );
    if (confirmed != true || !mounted) return;
    setState(() {
      _busy = true;
      _pendingId = plugin.id;
      _error = null;
      _notice = null;
    });
    try {
      await widget.api.uninstallPlugin(plugin.id);
      await _load();
      if (mounted) setState(() => _notice = '已卸载「${plugin.id}」。');
    } on DiaryException catch (failure) {
      if (mounted) setState(() => _error = failure.message);
    } on Object catch (failure) {
      if (mounted) setState(() => _error = '卸载失败：$failure');
    } finally {
      if (mounted) {
        setState(() {
          _busy = false;
          _pendingId = null;
        });
      }
    }
  }

  /// 安装：先检查（不执行包内容），再让用户逐项授权，最后才装。
  Future<void> _install() async {
    if (_busy) return;
    final ticket = await widget.pickPackage();
    if (ticket == null || !mounted) return;
    setState(() {
      _busy = true;
      _error = null;
      _notice = null;
    });
    try {
      final report = await widget.api.inspectPluginPackage(ticket);
      if (!mounted) return;
      if (report.issues.isNotEmpty) {
        setState(() => _error = '这个包不能安装：${report.issues.join('；')}');
        return;
      }
      final granted = await showDialog<Set<String>>(
        context: context,
        builder: (context) => _PermissionDialog(report: report),
      );
      if (granted == null || !mounted) return;
      final installed = await widget.api.installPlugin(
        ticket: ticket,
        grantedPermissions: granted,
      );
      await _load();
      if (mounted) {
        setState(
          () => _notice = '已安装「${installed.id}」，默认是停用状态，需要时再启用。',
        );
      }
    } on DiaryException catch (failure) {
      if (mounted) setState(() => _error = failure.message);
    } on Object catch (failure) {
      if (mounted) setState(() => _error = '安装失败：$failure');
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  /// 跑一次插件。票据指向**已经完成**的任务记录，结果按 `jobId` 读回来。
  Future<void> _run(Plugin plugin, String activation) async {
    if (_busy) return;
    final template = _selectedTemplate(plugin);
    final style = template?.kind == 'style' ? _selectedStyle(plugin) : null;
    if (template != null && template.kind == 'style' && style == null) {
      setState(() => _error = '这个模板要用一份风格，但你还没有保存过风格。先到「风格」里存一份。');
      return;
    }
    final day = _needsVersions(plugin) ? _selectedDay(plugin) : null;
    if (_needsVersions(plugin) && day == null) {
      setState(
        () => _error = '这个插件要导出「你选定的日记版本」，但库里还没有任何一天生成过日记。'
            '先去「日记」里生成一天，再回来运行。',
      );
      return;
    }
    setState(() {
      _busy = true;
      _pendingId = plugin.id;
      _error = null;
      _notice = null;
      _runPluginId = plugin.id;
      _runResult = null;
    });
    try {
      final ticket = await widget.api.invokePlugin(
        pluginId: plugin.id,
        command: activation,
        templateId: template?.id,
        format: template?.kind == 'export' ? template?.format : null,
        styleProfile: style,
        versionIds: day?.currentVersionId == null ? null : [day!.currentVersionId!],
        fromDayKey: template?.kind == 'export'
            ? (_fromDay[plugin.id] ?? _todayKey())
            : null,
        toDayKey: template?.kind == 'export'
            ? (_toDay[plugin.id] ?? _todayKey())
            : null,
      );
      final result = await widget.api.pluginRun(ticket.jobId);
      if (!mounted) return;
      setState(() {
        _runResult = result;
        if (result == null) {
          _error = '插件跑完了，但结果读不回来（任务 ${ticket.jobId}）。';
        } else if (result.failed) {
          _error = result.errorMessage ?? '插件没有成功：${result.status}';
        }
      });
    } on DiaryException catch (failure) {
      if (mounted) setState(() => _error = failure.message);
    } on Object catch (failure) {
      if (mounted) setState(() => _error = '运行失败：$failure');
    } finally {
      if (mounted) {
        setState(() {
          _busy = false;
          _pendingId = null;
        });
      }
    }
  }

  @override
  Widget build(BuildContext context) => Column(
    crossAxisAlignment: CrossAxisAlignment.stretch,
    children: [
      const Text(
        '插件',
        style: TextStyle(
          fontFamily: SketchFonts.display,
          fontSize: 31,
          color: SketchColors.ink,
        ),
      ),
      const SizedBox(height: 5),
      const Text(
        '看清功能和权限，再决定是否启用。插件跑在沙箱里，只能读你授权的材料。',
        style: TextStyle(color: SketchColors.muted),
      ),
      const SizedBox(height: 18),
      if (_error != null) ...[
        _message(_error!, danger: true),
        const SizedBox(height: 12),
      ],
      if (_notice != null) ...[
        _message(_notice!),
        const SizedBox(height: 12),
      ],
      Wrap(
        spacing: 9,
        runSpacing: 9,
        children: [
          SketchAction(
            key: const ValueKey('plugin-install'),
            label: _canInstall ? '安装插件包…' : '当前核心不支持安装插件',
            icon: Icons.add_to_drive,
            filled: true,
            onPressed: _canInstall && !_busy ? () => unawaited(_install()) : null,
          ),
          SketchAction(
            label: '刷新',
            icon: Icons.refresh,
            compact: true,
            onPressed: _busy ? null : () => unawaited(_load()),
          ),
        ],
      ),
      const SizedBox(height: 18),
      if (_loading)
        const Center(
          child: Padding(
            padding: EdgeInsets.all(24),
            child: SizedBox(
              width: 22,
              height: 22,
              child: CircularProgressIndicator(strokeWidth: 2),
            ),
          ),
        )
      else if (_plugins.isEmpty)
        _message('还没有装任何插件。插件是可选的：不装也能正常写日记。')
      else
        for (final plugin in _plugins) ...[
          _pluginCard(plugin),
          const SizedBox(height: 12),
        ],
    ],
  );

  Widget _pluginCard(Plugin plugin) {
    final enabled = plugin.state == PluginState.enabled;
    final pending = _pendingId == plugin.id;
    return SketchFrame(
      key: ValueKey('plugin-${plugin.id}'),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Row(
            children: [
              Expanded(
                child: Text(
                  plugin.id,
                  style: const TextStyle(
                    fontSize: 18,
                    fontWeight: FontWeight.w700,
                  ),
                ),
              ),
              Text(
                'v${plugin.version}',
                style: const TextStyle(color: SketchColors.muted),
              ),
            ],
          ),
          const SizedBox(height: 6),
          Text(
            '${plugin.entryKind == PluginEntryKind.declarative ? '声明式' : '代码（沙箱）'}'
            ' · 接口 ${plugin.apiRange}'
            '${plugin.supportedPlatforms.isEmpty ? '' : ' · ${plugin.supportedPlatforms.join(' / ')}'}',
            style: const TextStyle(color: SketchColors.muted, fontSize: 12),
          ),
          const SizedBox(height: 10),
          Text(
            plugin.grantedPermissions.isEmpty
                ? '权限：没有授权任何权限'
                : '权限：${plugin.grantedPermissions.join('、')}',
            style: const TextStyle(fontSize: 13),
          ),
          if (plugin.activationEvents.isNotEmpty) ...[
            const SizedBox(height: 4),
            Text(
              '会在这些时候被调用：${plugin.activationEvents.join('、')}',
              style: const TextStyle(color: SketchColors.muted, fontSize: 12),
            ),
          ],
          if (plugin.lastError != null) ...[
            const SizedBox(height: 8),
            Text(
              '上次出错：${plugin.lastError}',
              style: const TextStyle(color: SketchColors.ink, fontSize: 12),
            ),
          ],
          if (_canInvoke && plugin.templates.isNotEmpty) ...[
            const SizedBox(height: 12),
            _templateInputs(plugin),
          ],
          if (_canInvoke && _needsVersions(plugin)) ...[
            const SizedBox(height: 12),
            _versionInput(plugin),
          ],
          if (_canInvoke && plugin.activationEvents.isNotEmpty) ...[
            const SizedBox(height: 12),
            Wrap(
              spacing: 8,
              runSpacing: 8,
              children: [
                for (final activation in plugin.activationEvents)
                  SketchAction(
                    key: ValueKey('plugin-run-${plugin.id}-$activation'),
                    label: '运行（$activation）',
                    icon: Icons.play_arrow,
                    compact: true,
                    onPressed: _busy
                        ? null
                        : () => unawaited(_run(plugin, activation)),
                  ),
              ],
            ),
          ],
          if (_runPluginId == plugin.id && _runResult != null) ...[
            const SizedBox(height: 12),
            _runCard(_runResult!),
          ],
          const Divider(color: SketchColors.line, height: 27),
          Row(
            children: [
              if (pending)
                const SizedBox(
                  width: 18,
                  height: 18,
                  child: CircularProgressIndicator(strokeWidth: 2),
                )
              else
                Switch(
                  key: ValueKey('plugin-switch-${plugin.id}'),
                  value: enabled,
                  onChanged: _canToggle && !_busy
                      ? (value) => unawaited(_toggle(plugin, value))
                      : null,
                ),
              const SizedBox(width: 8),
              Expanded(
                child: Text(
                  enabled ? '已启用' : '已停用',
                  style: const TextStyle(fontSize: 13),
                ),
              ),
              if (_canUninstall)
                TextButton(
                  key: ValueKey('plugin-uninstall-${plugin.id}'),
                  onPressed: _busy ? null : () => unawaited(_uninstall(plugin)),
                  child: const Text('卸载'),
                ),
            ],
          ),
        ],
      ),
    );
  }

  /// 模板 / 风格 / 日期范围的选择。
  ///
  /// 没有这些输入，声明式插件根本跑不起来（核心会直接报「需要 templateId」、
  /// 「风格模板需要 styleProfile」）。以前界面只传插件 id 与激活点，等于把
  /// 「能运行」写在了按钮上，实际按下去只会报错。
  Widget _templateInputs(Plugin plugin) {
    final template = _selectedTemplate(plugin);
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        const Text('用哪个模板', style: TextStyle(fontSize: 13)),
        const SizedBox(height: 6),
        DropdownButton<String>(
          key: ValueKey('plugin-template-${plugin.id}'),
          value: template?.id,
          isExpanded: true,
          items: [
            for (final item in plugin.templates)
              DropdownMenuItem(
                value: item.id,
                child: Text(
                  '${item.label}（${item.kind}）',
                  overflow: TextOverflow.ellipsis,
                ),
              ),
          ],
          onChanged: (value) => setState(() {
            if (value != null) _templateChoice[plugin.id] = value;
          }),
        ),
        if (template?.kind == 'style') ...[
          const SizedBox(height: 10),
          const Text('用哪份风格', style: TextStyle(fontSize: 13)),
          const SizedBox(height: 6),
          if (_styles.isEmpty)
            Text(
              _stylesLoaded
                  ? '你还没有保存过风格。风格模板需要一份真实的风格，先去「风格」里存一份再来。'
                  : '正在读取风格…',
              style: const TextStyle(color: SketchColors.muted, fontSize: 12),
            )
          else
            DropdownButton<String>(
              key: ValueKey('plugin-style-${plugin.id}'),
              value: _selectedStyle(plugin)?.id,
              isExpanded: true,
              items: [
                for (final style in _styles)
                  DropdownMenuItem(
                    value: style.id,
                    child: Text(style.label, overflow: TextOverflow.ellipsis),
                  ),
              ],
              onChanged: (value) => setState(() {
                if (value != null) _styleChoice[plugin.id] = value;
              }),
            ),
        ],
        if (template?.kind == 'export') ...[
          const SizedBox(height: 10),
          const Text(
            '导出哪几天（闭区间，格式 2026-11-08）',
            style: TextStyle(fontSize: 13),
          ),
          const SizedBox(height: 6),
          Row(
            children: [
              Expanded(
                child: TextFormField(
                  key: ValueKey('plugin-from-${plugin.id}'),
                  initialValue: _fromDay[plugin.id] ?? _todayKey(),
                  decoration: const InputDecoration(labelText: '从'),
                  onChanged: (value) => _fromDay[plugin.id] = value,
                ),
              ),
              const SizedBox(width: 8),
              Expanded(
                child: TextFormField(
                  key: ValueKey('plugin-to-${plugin.id}'),
                  initialValue: _toDay[plugin.id] ?? _todayKey(),
                  decoration: const InputDecoration(labelText: '到'),
                  onChanged: (value) => _toDay[plugin.id] = value,
                ),
              ),
            ],
          ),
        ],
      ],
    );
  }

  /// 代码型导出插件：选一天导出。用这一天的**当前版本**作为「用户选定的日记版本」。
  ///
  /// 列表是**翻页扫出来的**，扫到上限还没找到时不能判「库里没有日记」——那只是
  /// 「还没查完」。所以这里始终给一个**按日期直接查**的入口：`getDay(dayKey)`
  /// 是 O(1) 的，不受扫描上限影响。
  Widget _versionInput(Plugin plugin) {
    final day = _selectedDay(plugin);
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        const Text('导出哪一天的日记', style: TextStyle(fontSize: 13)),
        const SizedBox(height: 6),
        if (!_daysLoaded)
          const Text(
            '正在读取日记…',
            style: TextStyle(color: SketchColors.muted, fontSize: 12),
          )
        else if (_daysWithVersion.isEmpty)
          Text(
            _daysTruncated
                ? '已经查了最近 $_daysScanned 天，还没有找到生成过日记的一天——'
                      '**这不等于库里没有**。用下面的「按日期选…」直接查更早的那天。'
                : '库里还没有任何一天生成过日记。这个插件要的是「你选定的日记版本」，'
                      '先去「日记」里生成一天再来。',
            style: const TextStyle(color: SketchColors.muted, fontSize: 12),
          )
        else
          DropdownButton<String>(
            key: ValueKey('plugin-version-${plugin.id}'),
            value: day?.dayKey,
            isExpanded: true,
            items: [
              for (final item in _daysWithVersion)
                DropdownMenuItem(
                  value: item.dayKey,
                  child: Text(
                    '${item.dayKey}（版本 ${item.currentVersionId}）',
                    overflow: TextOverflow.ellipsis,
                  ),
                ),
            ],
            onChanged: (value) => setState(() {
              if (value != null) _versionDay[plugin.id] = value;
            }),
          ),
        const SizedBox(height: 8),
        SketchAction(
          key: ValueKey('plugin-pick-day-${plugin.id}'),
          label: '按日期选…',
          icon: Icons.event_outlined,
          compact: true,
          onPressed: () => unawaited(_pickDay(plugin)),
        ),
        if (_daysTruncated) ...[
          const SizedBox(height: 8),
          SketchAction(
            key: ValueKey('plugin-more-days-${plugin.id}'),
            label: _daysLoadingMore ? '正在往后找…' : '继续往后找',
            icon: Icons.expand_more,
            compact: true,
            onPressed: _daysLoadingMore
                ? null
                : () => unawaited(_loadMoreDays()),
          ),
          Padding(
            padding: const EdgeInsets.only(top: 6),
            child: Text(
              '已经查了 $_daysScanned 天，还没查完；可以继续往后找，也可以直接用「按日期选…」。',
              style: const TextStyle(color: SketchColors.muted, fontSize: 12),
            ),
          ),
        ],
      ],
    );
  }

  /// 按日期直接查一天：不受「最多扫多少天」的限制。
  Future<void> _pickDay(Plugin plugin) async {
    final now = DateTime.now();
    final picked = await showDatePicker(
      context: context,
      initialDate: now,
      firstDate: DateTime(2000),
      lastDate: now.add(const Duration(days: 365)),
    );
    if (picked == null || !mounted) return;
    final key =
        '${picked.year}-${picked.month.toString().padLeft(2, '0')}-${picked.day.toString().padLeft(2, '0')}';
    try {
      final day = await widget.api.getDay(key);
      if (!mounted) return;
      if (day.currentVersionId == null) {
        setState(() => _error = '$key 这一天还没有生成过日记。先去「日记」里生成一天。');
        return;
      }
      setState(() {
        _error = null;
        _daysWithVersion = [
          day,
          ..._daysWithVersion.where((item) => item.dayKey != key),
        ];
        _versionDay[plugin.id] = key;
      });
    } on Object catch (failure) {
      if (mounted) setState(() => _error = '读不到 $key：$failure');
    }
  }

  /// 运行结果：**候选内容不自动写进日记**，用户自己决定要不要用。
  Widget _runCard(PluginRunResult result) => Container(
    width: double.infinity,
    padding: const EdgeInsets.all(12),
    decoration: BoxDecoration(
      border: Border.all(color: SketchColors.ink),
      borderRadius: BorderRadius.circular(8),
    ),
    child: Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Row(
          children: [
            Icon(
              result.failed ? Icons.error_outline : Icons.check_circle_outline,
              size: 18,
              color: SketchColors.ink,
            ),
            const SizedBox(width: 6),
            Expanded(
              child: Text(
                result.failed ? '没有成功（${result.status}）' : '跑完了（${result.status}）',
                style: const TextStyle(fontWeight: FontWeight.w700),
              ),
            ),
            Text(
              '${result.durationMs} ms',
              style: const TextStyle(color: SketchColors.muted, fontSize: 12),
            ),
          ],
        ),
        if (result.errorMessage != null) ...[
          const SizedBox(height: 6),
          Text(result.errorMessage!, style: const TextStyle(fontSize: 13)),
        ],
        if (result.output != null && result.output!.trim().isNotEmpty) ...[
          const SizedBox(height: 10),
          const Text('输出（候选内容，不会自动写进日记）：',
              style: TextStyle(color: SketchColors.muted, fontSize: 12)),
          const SizedBox(height: 6),
          SelectableText(result.output!, style: const TextStyle(height: 1.6)),
        ],
        for (final artifact in result.artifacts) ...[
          const SizedBox(height: 8),
          Text(
            '文件：${artifact.fileName}（${artifact.byteSize} 字节）'
            '${artifact.path == null ? ' · 只在内存里' : ''}',
            style: const TextStyle(fontSize: 12),
          ),
        ],
        for (final content in result.derivedContents) ...[
          const SizedBox(height: 8),
          Text(
            '候选内容（${content.kind}）：${content.text}',
            style: const TextStyle(fontSize: 12),
          ),
        ],
        for (final warning in result.warnings) ...[
          const SizedBox(height: 6),
          Text(
            '提醒：$warning',
            style: const TextStyle(color: SketchColors.muted, fontSize: 12),
          ),
        ],
      ],
    ),
  );

  Widget _message(String text, {bool danger = false}) => Container(
    width: double.infinity,
    padding: const EdgeInsets.all(12),
    decoration: BoxDecoration(
      border: Border.all(color: danger ? SketchColors.ink : SketchColors.line),
      borderRadius: BorderRadius.circular(8),
    ),
    child: Text(text, style: const TextStyle(fontSize: 13)),
  );
}

/// 安装前的授权清单：**每一项都要用户自己勾**，不默认全给。
class _PermissionDialog extends StatefulWidget {
  const _PermissionDialog({required this.report});

  final PluginPackageReport report;

  @override
  State<_PermissionDialog> createState() => _PermissionDialogState();
}

class _PermissionDialogState extends State<_PermissionDialog> {
  late final Set<String> _granted = {...widget.report.requestedPermissions};

  @override
  Widget build(BuildContext context) => AlertDialog(
    title: Text('安装 ${widget.report.packageId}？'),
    content: SingleChildScrollView(
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        mainAxisSize: MainAxisSize.min,
        children: [
          Text('版本 ${widget.report.version} · 接口 ${widget.report.apiRange}'),
          const SizedBox(height: 12),
          if (widget.report.requestedPermissions.isEmpty)
            const Text('这个插件不申请任何权限。')
          else ...[
            const Text('它申请的权限（可以取消勾选，取消后相应功能不可用）：'),
            const SizedBox(height: 8),
            for (final permission in widget.report.requestedPermissions)
              CheckboxListTile(
                contentPadding: EdgeInsets.zero,
                dense: true,
                value: _granted.contains(permission),
                title: Text(permission),
                onChanged: (value) => setState(() {
                  if (value ?? false) {
                    _granted.add(permission);
                  } else {
                    _granted.remove(permission);
                  }
                }),
              ),
          ],
          if (widget.report.issues.isNotEmpty) ...[
            const SizedBox(height: 12),
            Text('检查发现的问题：${widget.report.issues.join('；')}'),
          ],
        ],
      ),
    ),
    actions: [
      TextButton(
        onPressed: () => Navigator.of(context).pop(null),
        child: const Text('取消'),
      ),
      TextButton(
        onPressed: () => Navigator.of(context).pop(_granted),
        child: const Text('安装'),
      ),
    ],
  );
}

import 'dart:async';

import 'package:diary_api/diary_api.dart';
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import 'capture_controller.dart';
import 'sketch_ui.dart';

/// A navigable preview of the product plan. Only text capture uses DiaryApi;
/// all other content is clearly identified as a local, disposable simulation.
class ExperienceShell extends StatefulWidget {
  const ExperienceShell({super.key, required this.api, required this.demoMode});

  final DiaryApi api;
  final bool demoMode;

  @override
  State<ExperienceShell> createState() => _ExperienceShellState();
}

class _ExperienceShellState extends State<ExperienceShell> {
  late final CaptureController _capture;
  final _editor = TextEditingController();
  final _search = TextEditingController();
  final _editVersion = TextEditingController();
  final _responseText = TextEditingController();
  final _secret = TextEditingController();
  Timer? _searchDebounce;

  int _tab = 0;
  String? _panel;
  String? _detail;
  bool _candidate = false;
  bool _adopted = false;
  bool _showOldVersions = false;
  String? _reply;
  String _tone = '温暖克制';
  String _length = '适中';
  String _voice = '第一人称';
  String _searchKind = '全部';
  String _activeQuery = '';
  String _candidateDraft = _candidateBody;
  String _schedule = '22:30';
  bool _automatic = true;
  bool _pluginEnabled = false;
  String _recordingDemoState = 'idle';
  final List<String> _attachments = [];

  static const _demoBody =
      '今天路过花店，看到一束很明亮的橘色花。我停了一会儿，才发现自己已经很久没有这样认真看过街角。\n\n'
      '回家的路和平时一样，却因为这一点小小的颜色变得柔和。普通的一天，也有值得留下的光。';
  static const _candidateBody =
      '今天在花店门口停住了脚步。一束橘色的花把平常的街角照得很亮，我没有急着往前走。\n\n'
      '那只是很短的一刻，但回家后仍然记得。普通的一天，也可以被温柔地收好。';

  @override
  void initState() {
    super.initState();
    _capture = CaptureController(widget.api);
    unawaited(
      _capture.initialize(opened: true).then((_) {
        if (mounted && _editor.text.isEmpty && _capture.text.isNotEmpty) {
          _editor.text = _capture.text;
        }
      }),
    );
    _editVersion.text = _demoBody;
  }

  @override
  void dispose() {
    _capture.dispose();
    _editor.dispose();
    _search.dispose();
    _editVersion.dispose();
    _responseText.dispose();
    _secret.dispose();
    _searchDebounce?.cancel();
    super.dispose();
  }

  void _go(int tab) => setState(() {
    _tab = tab;
    _panel = null;
    _detail = null;
  });

  void _openPanel(String panel) => setState(() {
    _panel = panel;
    _detail = null;
  });

  void _openDetail(String detail) => setState(() {
    _detail = detail;
    _panel = null;
  });

  void _back() => setState(() {
    _panel = null;
    _detail = null;
  });

  void _toast(String message) => ScaffoldMessenger.of(context).showSnackBar(
    SnackBar(content: Text(message), duration: const Duration(seconds: 3)),
  );

  @override
  Widget build(BuildContext context) => AnimatedBuilder(
    animation: _capture,
    builder: (context, _) => LayoutBuilder(
      builder: (context, bounds) {
        final desktop = bounds.maxWidth >= 850;
        return Scaffold(
          backgroundColor: SketchColors.canvas,
          body: SafeArea(
            child: Center(
              child: ConstrainedBox(
                constraints: const BoxConstraints(maxWidth: 1500),
                child: Padding(
                  padding: EdgeInsets.all(desktop ? 22 : 0),
                  child: SketchFrame(
                    fill: SketchColors.canvas,
                    padding: EdgeInsets.zero,
                    child: Column(
                      children: [
                        _header(desktop),
                        if (widget.demoMode) _demoBanner(desktop),
                        const Divider(
                          color: SketchColors.ink,
                          thickness: 1,
                          height: 1,
                          indent: 24,
                          endIndent: 24,
                        ),
                        Expanded(
                          child: SingleChildScrollView(
                            key: ValueKey('${_tab}_${_panel ?? _detail ?? ''}'),
                            padding: EdgeInsets.fromLTRB(
                              desktop ? 46 : 22,
                              desktop ? 35 : 25,
                              desktop ? 46 : 22,
                              40,
                            ),
                            child: Center(
                              child: ConstrainedBox(
                                constraints: const BoxConstraints(
                                  maxWidth: 1240,
                                ),
                                child: AnimatedSwitcher(
                                  duration: const Duration(milliseconds: 220),
                                  child: _currentPage(desktop),
                                ),
                              ),
                            ),
                          ),
                        ),
                        if (!desktop) _bottomNav(),
                      ],
                    ),
                  ),
                ),
              ),
            ),
          ),
        );
      },
    ),
  );

  Widget _header(bool desktop) => Padding(
    padding: EdgeInsets.symmetric(
      horizontal: desktop ? 46 : 22,
      vertical: desktop ? 19 : 14,
    ),
    child: Row(
      children: [
        InkWell(
          onTap: () => _openPanel('cover'),
          child: SketchFrame(
            padding: const EdgeInsets.all(5),
            child: const CatMark(size: 33),
          ),
        ),
        const SizedBox(width: 15),
        Flexible(
          child: Text(
            '不写日记',
            maxLines: 1,
            overflow: TextOverflow.ellipsis,
            style: TextStyle(
              fontFamily: SketchFonts.display,
              color: SketchColors.ink,
              fontSize: desktop ? 33 : 29,
            ),
          ),
        ),
        if (desktop) ...[
          const SizedBox(width: 40),
          for (final (index, label) in <(int, String)>[
            (0, '记录'),
            (1, '日记'),
            (2, '搜索'),
            (3, '片段'),
            (4, '我的'),
          ])
            Padding(
              padding: const EdgeInsets.only(right: 6),
              child: TextButton(
                onPressed: () => _go(index),
                child: Text(
                  label,
                  style: TextStyle(
                    color: _tab == index && _panel == null && _detail == null
                        ? SketchColors.ink
                        : SketchColors.muted,
                    fontWeight: _tab == index
                        ? FontWeight.w700
                        : FontWeight.w400,
                  ),
                ),
              ),
            ),
        ],
        const Spacer(),
        if (desktop)
          const Text(
            '给今天留一页',
            style: TextStyle(color: SketchColors.muted, fontSize: 13),
          ),
        IconButton(
          tooltip: '任务中心',
          onPressed: () => _openPanel('tasks'),
          icon: const Icon(Icons.notifications_none, color: SketchColors.ink),
        ),
      ],
    ),
  );

  Widget _demoBanner(bool desktop) => Container(
    width: double.infinity,
    color: SketchColors.soft,
    padding: EdgeInsets.symmetric(horizontal: desktop ? 46 : 22, vertical: 7),
    child: const Text(
      '演示模式 · 内容仅在本次运行',
      softWrap: true,
      style: TextStyle(
        color: SketchColors.ink,
        fontSize: 12,
        fontWeight: FontWeight.w600,
      ),
    ),
  );

  Widget _bottomNav() => Container(
    decoration: const BoxDecoration(
      border: Border(top: BorderSide(color: SketchColors.ink)),
      color: SketchColors.paper,
    ),
    child: Row(
      children: [
        _navItem(0, Icons.edit_outlined, '记录'),
        _navItem(1, Icons.menu_book_outlined, '日记'),
        _navItem(2, Icons.search_outlined, '搜索'),
        _navItem(3, Icons.view_agenda_outlined, '片段'),
        _navItem(4, Icons.person_outline, '我的'),
      ],
    ),
  );

  Widget _navItem(int index, IconData icon, String label) => Expanded(
    child: InkWell(
      onTap: () => _go(index),
      child: Padding(
        padding: const EdgeInsets.symmetric(vertical: 10),
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            Icon(
              icon,
              size: 21,
              color: _tab == index ? SketchColors.ink : SketchColors.muted,
            ),
            const SizedBox(height: 3),
            Text(
              label,
              style: TextStyle(
                fontSize: 11,
                color: _tab == index ? SketchColors.ink : SketchColors.muted,
                fontWeight: _tab == index ? FontWeight.w700 : FontWeight.w400,
              ),
            ),
          ],
        ),
      ),
    ),
  );

  Widget _currentPage(bool desktop) {
    if (_detail != null) return _detailPage(desktop);
    if (_panel != null) return _panelPage(desktop);
    return switch (_tab) {
      0 => _recordPage(desktop),
      1 => _diaryPage(desktop),
      2 => _searchPage(desktop),
      3 => _timelinePage(desktop),
      _ => _profilePage(desktop),
    };
  }

  Widget _title(String title, String subtitle, {Widget? trailing}) => Column(
    crossAxisAlignment: CrossAxisAlignment.start,
    children: [
      Row(
        children: [
          Expanded(
            child: Text(
              title,
              style: const TextStyle(
                fontFamily: SketchFonts.display,
                color: SketchColors.ink,
                fontSize: 40,
              ),
            ),
          ),
          ?trailing,
        ],
      ),
      const SizedBox(height: 5),
      Text(
        subtitle,
        style: const TextStyle(color: SketchColors.muted, fontSize: 14),
      ),
    ],
  );

  Widget _notice(String text, {IconData icon = Icons.info_outline}) =>
      SketchFrame(
        fill: SketchColors.soft,
        padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 13),
        child: Row(
          children: [
            Icon(icon, size: 17, color: SketchColors.ink),
            const SizedBox(width: 11),
            Expanded(
              child: Text(
                text,
                style: const TextStyle(color: SketchColors.text, fontSize: 13),
              ),
            ),
          ],
        ),
      );

  Widget _recordPage(bool desktop) {
    final left = Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        InkWell(
          onTap: _capture.canChangeDate ? _chooseDate : null,
          child: Row(
            children: [
              Flexible(
                child: Text(
                  _dateLabel(_capture.occurredAt ?? DateTime.now()),
                  maxLines: 1,
                  overflow: TextOverflow.ellipsis,
                  style: TextStyle(
                    color: SketchColors.muted,
                    fontSize: desktop ? 17 : 14,
                  ),
                ),
              ),
              if (_capture.canChangeDate) ...[
                const SizedBox(width: 6),
                const Icon(
                  Icons.edit_calendar_outlined,
                  size: 15,
                  color: SketchColors.muted,
                ),
              ],
            ],
          ),
        ),
        const SizedBox(height: 26),
        Text(
          desktop ? '今天，\n想留住什么？' : '今天，想留住什么？',
          style: TextStyle(
            fontFamily: SketchFonts.display,
            color: SketchColors.ink,
            fontSize: desktop ? 52 : 42,
            height: 1.34,
          ),
        ),
        const SizedBox(height: 18),
        const Text(
          '从眼前这一刻开始就好。',
          style: TextStyle(color: SketchColors.muted, fontSize: 17),
        ),
        const SizedBox(height: 42),
        _notice(
          widget.demoMode
              ? '交互演示 · 文字仅保留本次运行；附件和录音是流程预览。'
              : '记录会保存到本地资料库；附件和录音仍是流程预览。',
        ),
      ],
    );
    if (desktop) {
      return Row(
        key: const ValueKey('record-desktop'),
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Expanded(
            flex: 4,
            child: Padding(
              padding: const EdgeInsets.only(right: 28, top: 20),
              child: left,
            ),
          ),
          Container(width: 1, height: 575, color: SketchColors.ink),
          const SizedBox(width: 32),
          Expanded(flex: 7, child: _editorCard(desktop)),
        ],
      );
    }
    return Column(
      key: const ValueKey('record-mobile'),
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [left, const SizedBox(height: 24), _editorCard(desktop)],
    );
  }

  Widget _editorCard(bool desktop) => Column(
    crossAxisAlignment: CrossAxisAlignment.stretch,
    children: [
      SketchFrame(
        padding: EdgeInsets.all(desktop ? 28 : 21),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            const Text(
              '此刻的记录',
              style: TextStyle(
                fontFamily: SketchFonts.display,
                color: SketchColors.ink,
                fontSize: 31,
              ),
            ),
            const SizedBox(height: 16),
            const Divider(color: SketchColors.ink, thickness: .8),
            CallbackShortcuts(
              bindings: {
                const SingleActivator(
                  LogicalKeyboardKey.enter,
                  control: true,
                ): () {
                  final composing = _editor.value.composing;
                  if (composing.isValid && !composing.isCollapsed) return;
                  if (_capture.text.trim().isNotEmpty) _finish();
                },
              },
              child: TextField(
                key: const Key('capture-editor'),
                controller: _editor,
                onChanged: _capture.updateText,
                readOnly: _capture.finishing,
                minLines: desktop ? 9 : 6,
                maxLines: desktop ? 12 : 10,
                textInputAction: TextInputAction.newline,
                keyboardType: TextInputType.multiline,
                style: const TextStyle(
                  color: SketchColors.text,
                  fontSize: 18,
                  height: 1.65,
                ),
                decoration: const InputDecoration(
                  hintText: '今天路过花店，看到一束很明亮的橘色花…',
                  hintStyle: TextStyle(color: SketchColors.muted),
                  border: InputBorder.none,
                ),
              ),
            ),
            if (_attachments.isNotEmpty) ...[
              const Divider(color: SketchColors.line),
              Wrap(
                spacing: 8,
                runSpacing: 8,
                children: [
                  for (final item in _attachments)
                    InputChip(
                      label: Text('$item · 演示'),
                      onDeleted: () =>
                          setState(() => _attachments.remove(item)),
                    ),
                ],
              ),
            ],
            const Divider(color: SketchColors.ink, thickness: .8),
            const SizedBox(height: 6),
            _saveStatus(),
            const SizedBox(height: 12),
            Wrap(
              spacing: 8,
              runSpacing: 8,
              children: [
                SketchAction(
                  label: '添加材料',
                  icon: Icons.attach_file,
                  compact: true,
                  onPressed: _showAttachmentChoices,
                ),
                SketchAction(
                  label: '录音',
                  icon: Icons.mic_none,
                  compact: true,
                  onPressed: () => _openPanel('recording'),
                ),
              ],
            ),
          ],
        ),
      ),
      const SizedBox(height: 20),
      SketchAction(
        key: const Key('finish-capture'),
        label: '收好这一刻',
        filled: true,
        onPressed: _capture.text.trim().isEmpty || _capture.finishing
            ? null
            : _finish,
      ),
    ],
  );

  Widget _saveStatus() {
    final (text, icon) = switch (_capture.phase) {
      SavePhase.ready => ('写一点什么，随时可以开始。', Icons.circle_outlined),
      SavePhase.editing => ('正在写，还没有保存确认。', Icons.edit_outlined),
      SavePhase.saving => (
        widget.demoMode ? '正在保存演示内容…' : '正在保存到本地资料库…',
        Icons.sync,
      ),
      SavePhase.saved => (
        widget.demoMode ? '已模拟保存 · 仅限本次运行' : '已保存到本地资料库',
        Icons.check_circle_outline,
      ),
      SavePhase.failed => (_capture.error ?? '保存失败，输入仍在。', Icons.error_outline),
    };
    return Row(
      children: [
        Icon(icon, color: SketchColors.muted, size: 17),
        const SizedBox(width: 7),
        Expanded(
          child: Text(
            text,
            style: const TextStyle(color: SketchColors.muted, fontSize: 13),
          ),
        ),
        if (_capture.phase == SavePhase.failed && !_capture.finishing)
          TextButton(onPressed: _capture.saveNow, child: const Text('重试')),
      ],
    );
  }

  Future<void> _finish() async {
    final success = await _capture.finish();
    if (!mounted) return;
    if (success) {
      _editor.clear();
      setState(_attachments.clear);
    } else if (_capture.text.trim().isNotEmpty) {
      _toast(_capture.error ?? '记录还没有收好，请检查后重试。');
    }
  }

  Future<void> _chooseDate() async {
    final now = DateTime.now();
    final picked = await showDatePicker(
      context: context,
      initialDate: _capture.occurredAt ?? now,
      firstDate: DateTime(2000),
      lastDate: now.add(const Duration(days: 365)),
    );
    if (picked != null) _capture.chooseDate(picked);
  }

  void _showAttachmentChoices() => showModalBottomSheet<void>(
    context: context,
    backgroundColor: SketchColors.paper,
    builder: (sheetContext) => SafeArea(
      child: Padding(
        padding: const EdgeInsets.all(22),
        child: Column(
          mainAxisSize: MainAxisSize.min,
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            const Text(
              '添加一份材料',
              style: TextStyle(fontSize: 20, fontWeight: FontWeight.w700),
            ),
            const SizedBox(height: 7),
            const Text(
              '这里演示入口和卡片状态；尚未访问设备文件。',
              style: TextStyle(color: SketchColors.muted),
            ),
            const SizedBox(height: 18),
            for (final (name, icon) in <(String, IconData)>[
              ('照片', Icons.image_outlined),
              ('文件', Icons.insert_drive_file_outlined),
              ('视频', Icons.videocam_outlined),
            ])
              ListTile(
                leading: Icon(icon),
                title: Text('模拟添加$name'),
                onTap: () {
                  Navigator.pop(sheetContext);
                  setState(() => _attachments.add(name));
                },
              ),
          ],
        ),
      ),
    ),
  );

  Widget _diaryPage(bool desktop) => Column(
    key: const ValueKey('diary'),
    crossAxisAlignment: CrossAxisAlignment.stretch,
    children: [
      _title(
        '日记',
        '让原本零散的片刻，慢慢成为一页。',
        trailing: SketchAction(
          label: '版本',
          compact: true,
          icon: Icons.history,
          onPressed: () => _openPanel('versions'),
        ),
      ),
      const SizedBox(height: 24),
      _notice('以下为示例日记。真实整理、转写和来源关联要在后续接入本地核心。'),
      const SizedBox(height: 22),
      SketchFrame(
        padding: EdgeInsets.all(desktop ? 34 : 23),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Text(
              '示例日记 · 无实际日期',
              style: const TextStyle(color: SketchColors.muted),
            ),
            const SizedBox(height: 12),
            const Text(
              '普通的一天，也有光',
              style: TextStyle(
                fontFamily: SketchFonts.display,
                color: SketchColors.ink,
                fontSize: 38,
              ),
            ),
            const SizedBox(height: 24),
            ConstrainedBox(
              constraints: const BoxConstraints(maxWidth: 690),
              child: Text(
                _adopted ? _candidateDraft : _demoBody,
                style: const TextStyle(
                  color: SketchColors.text,
                  fontSize: 17,
                  height: 1.9,
                ),
              ),
            ),
            const SizedBox(height: 24),
            const Divider(color: SketchColors.ink),
            InkWell(
              onTap: () => _openDetail('source'),
              child: const Padding(
                padding: EdgeInsets.symmetric(vertical: 7),
                child: Row(
                  children: [
                    Icon(Icons.description_outlined, size: 18),
                    SizedBox(width: 9),
                    Expanded(child: Text('查看示例来源 · 文字 1 项')),
                    Icon(Icons.arrow_forward, size: 17),
                  ],
                ),
              ),
            ),
            const Text(
              '还有 2 项材料待处理 · 示例状态',
              style: TextStyle(color: SketchColors.muted, fontSize: 12),
            ),
          ],
        ),
      ),
      if (_candidate) ...[
        const SizedBox(height: 15),
        _notice('有一版新的整理候选。当前阅读版不会自动替换。', icon: Icons.auto_stories_outlined),
      ],
      const SizedBox(height: 18),
      Wrap(
        spacing: 10,
        runSpacing: 10,
        children: [
          SketchAction(
            label: '编辑为新版',
            icon: Icons.edit_outlined,
            onPressed: () => _openPanel('edit'),
          ),
          SketchAction(
            label: _candidate ? '查看候选版本' : '模拟重新整理',
            icon: Icons.auto_fix_high_outlined,
            onPressed: () => _candidate
                ? _openPanel('versions')
                : setState(() {
                    _candidateDraft = _candidateBody;
                    _candidate = true;
                  }),
          ),
          SketchAction(
            label: '主动请求回应',
            icon: Icons.chat_bubble_outline,
            onPressed: () => _openPanel('response'),
          ),
        ],
      ),
    ],
  );

  Widget _searchPage(bool desktop) {
    final query = _activeQuery;
    final captures = _capture.recent.where(
      (c) =>
          c.state == CaptureState.committed &&
          (query.isEmpty || c.draftText.toLowerCase().contains(query)),
    );
    final sampleMatches =
        query.isEmpty || '今天路过花店看到一束橘色花普通的一天也有光'.contains(query);
    final showSample =
        sampleMatches && _searchKind != '录音' && _searchKind != '文件';
    return Column(
      key: const ValueKey('search'),
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        _title('搜索', '一个入口找回日记和原始材料。'),
        const SizedBox(height: 22),
        _notice('当前只演示本次会话文字的关键词匹配；语义检索与文件内容检索尚未接入。'),
        const SizedBox(height: 18),
        SketchFrame(
          padding: const EdgeInsets.symmetric(horizontal: 17, vertical: 6),
          child: Row(
            children: [
              const Icon(Icons.search, color: SketchColors.ink),
              const SizedBox(width: 10),
              Expanded(
                child: TextField(
                  key: const Key('global-search'),
                  controller: _search,
                  onChanged: _onSearchChanged,
                  decoration: const InputDecoration(
                    hintText: '搜索一个词，或一段记忆…',
                    border: InputBorder.none,
                  ),
                ),
              ),
              if (query.isNotEmpty)
                IconButton(
                  tooltip: '清除搜索',
                  onPressed: () {
                    _search.clear();
                    _onSearchChanged('');
                  },
                  icon: const Icon(Icons.close),
                ),
            ],
          ),
        ),
        const SizedBox(height: 13),
        Wrap(
          spacing: 7,
          children: [
            for (final kind in ['全部', '日记', '文字', '录音', '文件'])
              ChoiceChip(
                label: Text(kind),
                selected: _searchKind == kind,
                onSelected: (_) => setState(() => _searchKind = kind),
              ),
          ],
        ),
        const SizedBox(height: 24),
        Text(
          query.isEmpty ? '可以从这些片刻开始' : '匹配结果',
          style: const TextStyle(
            color: SketchColors.ink,
            fontWeight: FontWeight.w700,
          ),
        ),
        const SizedBox(height: 12),
        if (showSample)
          _resultTile(
            '普通的一天，也有光',
            '示例 · 日记 · 无实际日期',
            '今天路过花店，看到一束很明亮的橘色花…',
            () => _go(1),
          ),
        if (showSample && _searchKind != '日记')
          _resultTile(
            '花店门口的片刻',
            '示例 · 原始文字 · 无实际日期',
            '今天路过花店，看到一束橘色花。',
            () => _openDetail('source'),
          ),
        if (_searchKind != '日记' && _searchKind != '录音' && _searchKind != '文件')
          for (final capture in captures)
            _resultTile(
              '刚刚收好的记录',
              '本次会话文字 · ${_dateLabel(capture.occurredAt.toLocal())}',
              capture.draftText,
              () => _openDetail(capture.id),
            ),
        if (!showSample || _searchKind == '录音' || _searchKind == '文件')
          if (captures.isEmpty || _searchKind == '录音' || _searchKind == '文件')
            _notice('当前范围没有匹配项。待处理或未解析的材料，不等于内容不存在。', icon: Icons.search_off),
      ],
    );
  }

  void _onSearchChanged(String value) {
    _searchDebounce?.cancel();
    final composing = _search.value.composing;
    if (composing.isValid && !composing.isCollapsed) return;
    _searchDebounce = Timer(const Duration(milliseconds: 250), () {
      if (mounted) setState(() => _activeQuery = value.trim().toLowerCase());
    });
  }

  Widget _resultTile(
    String title,
    String eyebrow,
    String excerpt,
    VoidCallback onTap,
  ) => Padding(
    padding: const EdgeInsets.only(bottom: 11),
    child: InkWell(
      onTap: onTap,
      child: SketchFrame(
        padding: const EdgeInsets.all(18),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Text(
              eyebrow,
              style: const TextStyle(color: SketchColors.muted, fontSize: 12),
            ),
            const SizedBox(height: 7),
            Text(
              title,
              style: const TextStyle(
                color: SketchColors.ink,
                fontSize: 17,
                fontWeight: FontWeight.w700,
              ),
            ),
            const SizedBox(height: 5),
            Text(
              excerpt,
              maxLines: 2,
              overflow: TextOverflow.ellipsis,
              style: const TextStyle(color: SketchColors.text),
            ),
          ],
        ),
      ),
    ),
  );

  Widget _timelinePage(bool desktop) => Column(
    key: const ValueKey('timeline'),
    crossAxisAlignment: CrossAxisAlignment.stretch,
    children: [
      _title('留下的片段', '原始材料和日记分开保存，随时可以回看。'),
      const SizedBox(height: 23),
      _notice('这页混合展示示例材料和本次运行新写的文字；真实原件库后续接入。'),
      const SizedBox(height: 18),
      for (final capture in _capture.recent)
        _resultTile(
          capture.state == CaptureState.draft ? '正在写的草稿' : '刚刚收好的记录',
          '本次会话文字 · ${capture.state == CaptureState.draft ? '草稿' : '已提交'} · ${_dateLabel(capture.occurredAt.toLocal())}',
          capture.draftText,
          () => _openDetail(capture.id),
        ),
      _resultTile(
        '花店门口的片刻',
        '示例 · 文字 · 无实际日期',
        '今天路过花店，看到一束橘色花。',
        () => _openDetail('source'),
      ),
      _resultTile(
        '傍晚的街角',
        '示例 · 照片 · 待处理',
        '原图已保存的展示位置；画面描述尚未生成。',
        () => _openDetail('photo'),
      ),
      _resultTile(
        '回家路上的声音',
        '示例 · 录音 · 待转写',
        '声音原件可播放；文字检索范围取决于转写状态。',
        () => _openDetail('audio'),
      ),
    ],
  );

  Widget _profilePage(bool desktop) => Column(
    key: const ValueKey('profile'),
    crossAxisAlignment: CrossAxisAlignment.stretch,
    children: [
      _title('我的小天地', '预览资料与整理方式，慢慢布置自己的空间。'),
      const SizedBox(height: 23),
      _notice('这是单设备演示资料库，没有账号与自动同步。'),
      const SizedBox(height: 18),
      _section('创作与整理', [
        ('风格偏好', '篇幅、语气与叙述方式', 'style', Icons.tune),
        ('模型服务', '能力、连接与使用范围', 'model', Icons.auto_awesome_outlined),
        ('整理时机', '安静地安排每天的整理', 'schedule', Icons.schedule),
        ('任务中心', '等待、处理中与需要处理', 'tasks', Icons.inbox_outlined),
      ]),
      const SizedBox(height: 18),
      _section('资料与应用', [
        ('插件', '查看权限与启停', 'plugins', Icons.extension_outlined),
        ('备份与恢复', '完整备份和阅读导出', 'backup', Icons.backup_outlined),
        ('存储与回收站', '原件、缓存、索引的边界', 'storage', Icons.folder_outlined),
        ('封面与加载动画', '回看小纸团', 'cover', Icons.pets_outlined),
      ]),
    ],
  );

  Widget _section(
    String title,
    List<(String, String, String, IconData)> items,
  ) => SketchFrame(
    padding: const EdgeInsets.all(18),
    child: Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Text(
          title,
          style: const TextStyle(
            color: SketchColors.ink,
            fontSize: 16,
            fontWeight: FontWeight.w700,
          ),
        ),
        const SizedBox(height: 9),
        for (final (label, description, route, icon) in items) ...[
          const Divider(color: SketchColors.line),
          InkWell(
            onTap: () => _openPanel(route),
            child: Padding(
              padding: const EdgeInsets.symmetric(vertical: 8),
              child: Row(
                children: [
                  Icon(icon, color: SketchColors.ink, size: 22),
                  const SizedBox(width: 14),
                  Expanded(
                    child: Column(
                      crossAxisAlignment: CrossAxisAlignment.start,
                      children: [
                        Text(
                          label,
                          style: const TextStyle(
                            color: SketchColors.ink,
                            fontWeight: FontWeight.w600,
                          ),
                        ),
                        Text(
                          description,
                          style: const TextStyle(
                            color: SketchColors.muted,
                            fontSize: 12,
                          ),
                        ),
                      ],
                    ),
                  ),
                  const Icon(
                    Icons.arrow_forward_ios,
                    size: 14,
                    color: SketchColors.muted,
                  ),
                ],
              ),
            ),
          ),
        ],
      ],
    ),
  );

  Widget _panelPage(bool desktop) {
    final panel = _panel!;
    final content = switch (panel) {
      'versions' => _versionsPanel(desktop),
      'edit' => _editPanel(),
      'response' => _responsePanel(),
      'tasks' => _tasksPanel(),
      'style' => _stylePanel(),
      'model' => _modelPanel(),
      'schedule' => _schedulePanel(),
      'plugins' => _pluginsPanel(),
      'backup' => _backupPanel(),
      'storage' => _storagePanel(),
      'recording' => _recordingPanel(),
      'loading' => _loadingPanel(),
      _ => _coverPanel(),
    };
    return Column(
      key: ValueKey('panel-$panel'),
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        Align(
          alignment: Alignment.centerLeft,
          child: TextButton.icon(
            onPressed: _back,
            icon: const Icon(Icons.arrow_back),
            label: const Text('返回'),
          ),
        ),
        const SizedBox(height: 12),
        content,
      ],
    );
  }

  Widget _versionsPanel(bool desktop) => Column(
    crossAxisAlignment: CrossAxisAlignment.stretch,
    children: [
      _title('版本', '重新整理会产生候选，当前阅读版始终由你决定。'),
      const SizedBox(height: 20),
      _notice('这里是版本交互演示。真实版本与来源快照由本地核心保存。'),
      const SizedBox(height: 18),
      if (desktop)
        Row(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Expanded(
              child: _versionCard(
                '当前阅读版',
                _adopted ? _candidateDraft : _demoBody,
              ),
            ),
            const SizedBox(width: 16),
            Expanded(child: _versionCard('整理候选', _candidateDraft)),
          ],
        )
      else ...[
        _versionCard('当前阅读版', _adopted ? _candidateDraft : _demoBody),
        const SizedBox(height: 16),
        _versionCard('整理候选', _candidateDraft),
      ],
      const SizedBox(height: 18),
      Wrap(
        spacing: 10,
        runSpacing: 10,
        children: [
          SketchAction(
            label: '采用候选版',
            filled: true,
            onPressed: () {
              setState(() {
                _candidate = false;
                _adopted = true;
              });
              _toast('已在演示中采用候选版');
            },
          ),
          SketchAction(label: '保留当前版', onPressed: _back),
          SketchAction(label: '编辑后保存新版', onPressed: () => _openPanel('edit')),
        ],
      ),
      const SizedBox(height: 18),
      CheckboxListTile(
        contentPadding: EdgeInsets.zero,
        title: const Text('包括历史版本'),
        value: _showOldVersions,
        onChanged: (value) => setState(() => _showOldVersions = value ?? false),
      ),
      if (_showOldVersions) _notice('原始生成版 · 示例历史记录（不会被新的候选覆盖）'),
    ],
  );

  Widget _versionCard(String label, String body) => SketchFrame(
    child: Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Text(
          label,
          style: const TextStyle(
            color: SketchColors.ink,
            fontWeight: FontWeight.w700,
          ),
        ),
        const SizedBox(height: 9),
        const Text(
          '示例生成 · 无实际日期 · 使用文字 1 项',
          style: TextStyle(color: SketchColors.muted, fontSize: 12),
        ),
        const Divider(color: SketchColors.line, height: 25),
        Text(
          body,
          style: const TextStyle(
            color: SketchColors.text,
            fontSize: 15,
            height: 1.8,
          ),
        ),
      ],
    ),
  );

  Widget _editPanel() => Column(
    crossAxisAlignment: CrossAxisAlignment.stretch,
    children: [
      _title('编辑日记', '在当前版上修改，保存后成为另一版。'),
      const SizedBox(height: 20),
      _notice('编辑内容只在本次演示中暂存；不会写入真实版本库。'),
      const SizedBox(height: 18),
      SketchFrame(
        child: TextField(
          controller: _editVersion,
          minLines: 12,
          maxLines: 22,
          decoration: const InputDecoration(border: InputBorder.none),
        ),
      ),
      const SizedBox(height: 16),
      Align(
        alignment: Alignment.centerLeft,
        child: SketchAction(
          label: '保存为新版（演示）',
          filled: true,
          onPressed: () {
            setState(() {
              _candidateDraft = _editVersion.text;
              _candidate = true;
            });
            _toast('已创建演示候选；当前阅读版未替换');
            _openPanel('versions');
          },
        ),
      ),
    ],
  );

  Widget _responsePanel() => Column(
    crossAxisAlignment: CrossAxisAlignment.stretch,
    children: [
      _title('主动回应', '只有你点下请求，才会出现一段回应。'),
      const SizedBox(height: 20),
      _notice('演示不会调用 AI，也不会上传材料；下方只展示回应会如何单独呈现。'),
      const SizedBox(height: 18),
      SketchFrame(
        child: TextField(
          controller: _responseText,
          minLines: 3,
          maxLines: 7,
          decoration: const InputDecoration(
            hintText: '想对这一页问些什么？',
            border: InputBorder.none,
          ),
        ),
      ),
      const SizedBox(height: 15),
      Align(
        alignment: Alignment.centerLeft,
        child: SketchAction(
          label: '看看回应示例',
          filled: true,
          onPressed: () => setState(
            () => _reply = '你已经把那个短暂的停留记下来了。那束花的颜色，以及你愿意看它一会儿的心情，都可以先留在这里。',
          ),
        ),
      ),
      if (_reply != null) ...[
        const SizedBox(height: 19),
        SketchFrame(
          fill: SketchColors.soft,
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              const Text(
                '回应示例 · 与日记正文分开展示',
                style: TextStyle(color: SketchColors.muted, fontSize: 12),
              ),
              const SizedBox(height: 10),
              Text(
                _reply!,
                style: const TextStyle(color: SketchColors.text, height: 1.8),
              ),
            ],
          ),
        ),
      ],
    ],
  );

  Widget _tasksPanel() => Column(
    crossAxisAlignment: CrossAxisAlignment.stretch,
    children: [
      _title('任务中心', '材料保存与后续处理各有自己的状态。'),
      const SizedBox(height: 20),
      _notice('以下是示例状态，不代表本机正在运行模型或转写任务。'),
      const SizedBox(height: 16),
      _resultTile(
        '待配置 · 录音转写',
        '示例 · 需要处理',
        '声音原件仍可保留；配置转写能力后再继续。',
        () => _openPanel('model'),
      ),
      _resultTile(
        '图片内容提取',
        '示例 · 等待',
        '尚未配置图片理解服务，文件名仍可检索。',
        () => _openPanel('model'),
      ),
      _resultTile(
        '示例日记整理',
        '示例 · 完成',
        '已生成候选版；不会自动覆盖人工编辑。',
        () => _openPanel('versions'),
      ),
    ],
  );

  Widget _stylePanel() => Column(
    crossAxisAlignment: CrossAxisAlignment.stretch,
    children: [
      _title('风格偏好', '让文字更像你，又不替你编造。'),
      const SizedBox(height: 20),
      _notice('偏好设置仅供预览，暂不保存到核心，也不会影响整理结果；整理功能尚未接入。'),
      const SizedBox(height: 18),
      _choiceGroup(
        '篇幅',
        ['简短', '适中', '舒展'],
        _length,
        (value) => setState(() => _length = value),
      ),
      const SizedBox(height: 17),
      _choiceGroup(
        '语气',
        ['更克制', '温暖克制', '更亲近'],
        _tone,
        (value) => setState(() => _tone = value),
      ),
      const SizedBox(height: 17),
      _choiceGroup(
        '叙述',
        ['第一人称', '旁观叙述'],
        _voice,
        (value) => setState(() => _voice = value),
      ),
      const SizedBox(height: 19),
      _notice('示例：普通的一天，也有值得留下的光。', icon: Icons.format_quote),
    ],
  );

  Widget _choiceGroup(
    String title,
    List<String> choices,
    String current,
    ValueChanged<String> onChange,
  ) => SketchFrame(
    child: Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Text(
          title,
          style: const TextStyle(
            color: SketchColors.ink,
            fontWeight: FontWeight.w700,
          ),
        ),
        const SizedBox(height: 12),
        Wrap(
          spacing: 8,
          runSpacing: 8,
          children: [
            for (final choice in choices)
              ChoiceChip(
                label: Text(choice),
                selected: current == choice,
                onSelected: (_) => onChange(choice),
              ),
          ],
        ),
      ],
    ),
  );

  Widget _modelPanel() => Column(
    crossAxisAlignment: CrossAxisAlignment.stretch,
    children: [
      _title('模型服务', '记录不需要先配置模型。'),
      const SizedBox(height: 20),
      _notice('配置界面预览：这里不会保存密钥，也不会发起真实连接测试。'),
      const SizedBox(height: 18),
      SketchFrame(
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            const Text(
              '远端服务 · 尚未配置',
              style: TextStyle(fontWeight: FontWeight.w700),
            ),
            const SizedBox(height: 7),
            const Text(
              '日记生成、语音转写、图片理解分别检查能力；本地文字记录始终可用。',
              style: TextStyle(color: SketchColors.muted),
            ),
            const SizedBox(height: 14),
            const TextField(
              enabled: false,
              decoration: InputDecoration(labelText: '服务地址（后续接入）'),
            ),
            TextField(
              controller: _secret,
              obscureText: true,
              enabled: false,
              decoration: const InputDecoration(labelText: '密钥（演示不收集）'),
            ),
            const SizedBox(height: 13),
            const Text(
              '可能发送的材料类型须在首次配置时明确告知。',
              style: TextStyle(color: SketchColors.muted, fontSize: 12),
            ),
          ],
        ),
      ),
    ],
  );

  Widget _schedulePanel() => Column(
    crossAxisAlignment: CrossAxisAlignment.stretch,
    children: [
      _title('整理时机', '安静地整理，不打断记录。'),
      const SizedBox(height: 20),
      _notice('自动整理尚未接入；这里仅预览设置方式，开关与时间不会保存或启动任务。'),
      const SizedBox(height: 18),
      SketchFrame(
        child: Column(
          children: [
            SwitchListTile(
              title: const Text('自动整理'),
              subtitle: const Text('只整理已经提交的材料'),
              value: _automatic,
              onChanged: (value) => setState(() => _automatic = value),
            ),
            const Divider(color: SketchColors.line),
            ListTile(
              title: const Text('计划时间'),
              trailing: Text(_schedule),
              onTap: () async {
                final picked = await showTimePicker(
                  context: context,
                  initialTime: const TimeOfDay(hour: 22, minute: 30),
                );
                if (picked != null && mounted) {
                  setState(
                    () => _schedule =
                        '${picked.hour.toString().padLeft(2, '0')}:${picked.minute.toString().padLeft(2, '0')}',
                  );
                }
              },
            ),
          ],
        ),
      ),
    ],
  );

  Widget _pluginsPanel() => Column(
    crossAxisAlignment: CrossAxisAlignment.stretch,
    children: [
      _title('插件', '看清功能和权限，再决定是否启用。'),
      const SizedBox(height: 20),
      _notice('插件安装与沙箱尚未接入；下面的开关仅供体验流程。'),
      const SizedBox(height: 18),
      SketchFrame(
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            const Text(
              '示例导出插件 · v0.1',
              style: TextStyle(fontSize: 18, fontWeight: FontWeight.w700),
            ),
            const SizedBox(height: 8),
            const Text(
              '作者：社区示例 · Windows / Android',
              style: TextStyle(color: SketchColors.muted),
            ),
            const SizedBox(height: 12),
            const Text('权限：读取用户选中的日记；写入用户指定的导出位置。'),
            const Divider(color: SketchColors.line, height: 27),
            SwitchListTile(
              contentPadding: EdgeInsets.zero,
              title: const Text('演示启用状态'),
              value: _pluginEnabled,
              onChanged: (value) => setState(() => _pluginEnabled = value),
            ),
          ],
        ),
      ),
    ],
  );

  Widget _backupPanel() => Column(
    crossAxisAlignment: CrossAxisAlignment.stretch,
    children: [
      _title('备份与恢复', '完整备份和阅读导出，是两件不同的事。'),
      const SizedBox(height: 20),
      _notice('这里仅展示用户流程，不生成可恢复备份。'),
      const SizedBox(height: 18),
      _resultTile(
        '创建完整备份',
        '示例 · 包含原始材料、日记版本与配置',
        '正式版将展示目标位置、空间需求与加密选项。',
        () => _toast('完整备份需要真实本地核心；演示没有生成文件。'),
      ),
      _resultTile(
        '导出阅读文档',
        '示例 · 方便阅读与分享',
        '阅读文档不能用于完整恢复。',
        () => _toast('阅读导出尚未接入；演示没有生成文件。'),
      ),
      _resultTile(
        '恢复备份',
        '示例 · 先检查，再预览',
        '默认恢复到新资料库，避免覆盖当前资料。',
        () => _toast('恢复需要真实备份；当前资料未发生变化。'),
      ),
    ],
  );

  Widget _storagePanel() => Column(
    crossAxisAlignment: CrossAxisAlignment.stretch,
    children: [
      _title('存储与回收站', '先看清影响，再清理。'),
      const SizedBox(height: 20),
      _notice('占用数字需由核心实际测量；演示不显示虚构容量。'),
      const SizedBox(height: 18),
      SketchFrame(
        child: Column(
          children: [
            for (final (name, detail) in <(String, String)>[
              ('原始材料', '清理缓存不会删除原件'),
              ('缓存', '可重建的临时内容'),
              ('索引', '可重建的搜索数据'),
              ('备份', '由用户选择保存位置'),
              ('回收站', '永久清理前需预览影响'),
            ]) ...[
              ListTile(
                title: Text(name),
                subtitle: Text(detail),
                trailing: const Text(
                  '待接入',
                  style: TextStyle(color: SketchColors.muted),
                ),
              ),
              const Divider(color: SketchColors.line),
            ],
          ],
        ),
      ),
    ],
  );

  Widget _recordingPanel() => Column(
    crossAxisAlignment: CrossAxisAlignment.stretch,
    children: [
      _title('录音', '一句话或一次长谈，用同一个入口。'),
      const SizedBox(height: 20),
      _notice('真实麦克风采集尚未接入。此页只预览准备、录制、暂停、保存的布局，不会录音。'),
      const SizedBox(height: 18),
      SketchFrame(
        child: Column(
          children: [
            const Icon(Icons.mic_none, size: 55, color: SketchColors.ink),
            const SizedBox(height: 15),
            Text(
              switch (_recordingDemoState) {
                'recording' => '流程预览：录制中（没有采集声音）',
                'paused' => '流程预览：已暂停',
                'saved' => '流程预览：片段已收好',
                _ => '等待麦克风准备',
              },
              style: const TextStyle(fontWeight: FontWeight.w700, fontSize: 18),
              textAlign: TextAlign.center,
            ),
            const SizedBox(height: 7),
            const Text(
              '获得真实首个音频缓冲后，才显示“正在录音”。',
              textAlign: TextAlign.center,
              style: TextStyle(color: SketchColors.muted),
            ),
            const SizedBox(height: 20),
            Wrap(
              alignment: WrapAlignment.center,
              spacing: 9,
              runSpacing: 9,
              children: [
                if (_recordingDemoState == 'idle' ||
                    _recordingDemoState == 'saved')
                  SketchAction(
                    label: '模拟开始',
                    icon: Icons.fiber_manual_record,
                    onPressed: () =>
                        setState(() => _recordingDemoState = 'recording'),
                  ),
                if (_recordingDemoState == 'recording')
                  SketchAction(
                    label: '模拟暂停',
                    icon: Icons.pause,
                    onPressed: () =>
                        setState(() => _recordingDemoState = 'paused'),
                  ),
                if (_recordingDemoState == 'paused')
                  SketchAction(
                    label: '模拟继续',
                    icon: Icons.play_arrow,
                    onPressed: () =>
                        setState(() => _recordingDemoState = 'recording'),
                  ),
                if (_recordingDemoState == 'recording' ||
                    _recordingDemoState == 'paused')
                  SketchAction(
                    label: '模拟结束',
                    icon: Icons.stop,
                    onPressed: () => setState(() {
                      _recordingDemoState = 'saved';
                      _attachments.add('录音占位');
                    }),
                  ),
                SketchAction(label: '返回文字记录', filled: true, onPressed: _back),
              ],
            ),
          ],
        ),
      ),
    ],
  );

  Widget _coverPanel() => Center(
    child: ConstrainedBox(
      constraints: const BoxConstraints(maxWidth: 520),
      child: Column(
        children: [
          const SizedBox(height: 45),
          const CatMark(size: 170),
          const SizedBox(height: 35),
          const Text(
            '不写日记',
            style: TextStyle(
              fontFamily: SketchFonts.display,
              fontSize: 60,
              color: SketchColors.ink,
            ),
          ),
          const SizedBox(height: 14),
          const Text(
            '把今天，轻轻收好。',
            style: TextStyle(fontSize: 20, color: SketchColors.text),
          ),
          const SizedBox(height: 35),
          SketchAction(label: '开始记录', filled: true, onPressed: () => _go(0)),
          const SizedBox(height: 14),
          TextButton(
            onPressed: () => _openPanel('loading'),
            child: const Text('查看加载动画'),
          ),
        ],
      ),
    ),
  );

  Widget _loadingPanel() => Center(
    child: Column(
      children: [
        const SizedBox(height: 55),
        const WritingCat(size: 290),
        const SizedBox(height: 32),
        const Text(
          '小纸团在整理今天',
          style: TextStyle(
            fontFamily: SketchFonts.display,
            fontSize: 41,
            color: SketchColors.ink,
          ),
        ),
        const SizedBox(height: 8),
        const Text(
          '稍等一下，就可以开始记录了。',
          style: TextStyle(color: SketchColors.muted),
        ),
        const SizedBox(height: 22),
        _notice('这是动画预览；实际启动只在等待资料库打开时显示，不人为延长加载。'),
      ],
    ),
  );

  Widget _detailPage(bool desktop) {
    final detail = _detail!;
    final capture = _capture.recent.where((c) => c.id == detail).firstOrNull;
    final title = switch (detail) {
      'source' => '花店门口的片刻',
      'photo' => '傍晚的街角',
      'audio' => '回家路上的声音',
      _ => capture?.state == CaptureState.draft ? '正在写的草稿' : '刚刚收好的记录',
    };
    final content = switch (detail) {
      'source' => '今天路过花店，看到一束橘色花。\n普通的一天也有光。',
      'photo' => '照片原件展示位置。画面尚未解析时，只能搜索文件信息与备注。',
      'audio' => '录音原件播放器与时间定位展示位置。真实音频尚未接入，不能播放示例声音。',
      _ => capture?.draftText ?? '这条记录暂时无法打开。',
    };
    return Column(
      key: ValueKey('detail-$detail'),
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        Align(
          alignment: Alignment.centerLeft,
          child: TextButton.icon(
            onPressed: _back,
            icon: const Icon(Icons.arrow_back),
            label: const Text('返回'),
          ),
        ),
        const SizedBox(height: 12),
        _title(title, capture == null ? '示例原始材料 · 无实际日期' : '本次会话文字 · 仅在本次运行保留'),
        const SizedBox(height: 20),
        _notice(
          capture == null
              ? '来源详情与定位是交互预览；真实文件和播放器需要平台能力接入。'
              : '这是本次会话中写下的文字；演示不会永久保存。',
        ),
        const SizedBox(height: 18),
        SketchFrame(
          padding: const EdgeInsets.all(27),
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Text(
                content,
                style: const TextStyle(
                  color: SketchColors.text,
                  fontSize: 17,
                  height: 1.85,
                ),
              ),
              const SizedBox(height: 30),
              const Divider(color: SketchColors.line),
              Text(
                capture == null
                    ? '处理状态：示例 · 部分材料仍待处理'
                    : '状态：本次会话文字 · ${capture.state == CaptureState.draft ? '草稿' : '已提交'}',
                style: const TextStyle(color: SketchColors.muted),
              ),
            ],
          ),
        ),
      ],
    );
  }

  String _dateLabel(DateTime date) =>
      '${date.year} 年 ${date.month} 月 ${date.day} 日';
}

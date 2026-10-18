import 'package:diary_api/diary_api.dart';
import 'package:flutter/material.dart';

import 'sketch_ui.dart';
import 'style_settings_controller.dart';

class StyleSettingsPanel extends StatefulWidget {
  const StyleSettingsPanel({super.key, required this.controller});
  final StyleSettingsController controller;

  @override
  State<StyleSettingsPanel> createState() => _StyleSettingsPanelState();
}

class _StyleSettingsPanelState extends State<StyleSettingsPanel> {
  final _label = TextEditingController(text: '我的日记风格');
  final _examples = TextEditingController();
  final _banned = TextEditingController();
  StyleProfile? _editing;
  String _length = '适中';
  String _tone = '温暖克制';
  String _person = '第一人称';
  String _temperature = '温和';
  String _rhetoric = '少量修辞';

  StyleSettingsController get controller => widget.controller;

  @override
  void dispose() {
    _label.dispose();
    _examples.dispose();
    _banned.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) => AnimatedBuilder(
    animation: controller,
    builder: (context, _) => Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        const Text(
          '风格偏好',
          style: TextStyle(
            fontFamily: SketchFonts.display,
            fontSize: 31,
            color: SketchColors.ink,
          ),
        ),
        const SizedBox(height: 5),
        const Text(
          '让之后生成的文字更像你，同时保留事实边界。',
          style: TextStyle(color: SketchColors.muted, fontSize: 15),
        ),
        const SizedBox(height: 18),
        if (controller.loading)
          const LinearProgressIndicator()
        else ...[
          if (controller.message != null) _notice(controller.message!),
          if (controller.styles.isNotEmpty) ...[
            const SizedBox(height: 16),
            SketchFrame(
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  const Text(
                    '已保存的风格',
                    style: TextStyle(fontWeight: FontWeight.w700),
                  ),
                  const SizedBox(height: 8),
                  for (final profile in controller.styles) ...[
                    const Divider(color: SketchColors.line),
                    ListTile(
                      contentPadding: EdgeInsets.zero,
                      title: Text(profile.label),
                      subtitle: Text(_summary(profile)),
                      leading: Icon(
                        controller.selectedStyleId == profile.id
                            ? Icons.check_circle
                            : Icons.circle_outlined,
                      ),
                      onTap: () => _edit(profile),
                      trailing: TextButton(
                        onPressed: controller.canSelect && !controller.saving
                            ? () => controller.select(profile)
                            : null,
                        child: Text(
                          controller.selectedStyleId == profile.id
                              ? '正在使用'
                              : '设为默认',
                        ),
                      ),
                    ),
                  ],
                ],
              ),
            ),
            const SizedBox(height: 18),
          ],
          _editor(),
        ],
      ],
    ),
  );

  Widget _editor() => SketchFrame(
    child: Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Row(
          children: [
            Expanded(
              child: Text(
                _editing == null ? '新建风格' : '编辑“${_editing!.label}”',
                style: const TextStyle(
                  fontSize: 18,
                  fontWeight: FontWeight.w700,
                ),
              ),
            ),
            if (_editing != null)
              TextButton(onPressed: _newStyle, child: const Text('另建一个')),
          ],
        ),
        const SizedBox(height: 12),
        TextField(
          controller: _label,
          decoration: const InputDecoration(labelText: '名称'),
          onChanged: (_) => setState(() {}),
        ),
        const SizedBox(height: 16),
        _choices('篇幅', const ['简短', '适中', '舒展'], _length, (value) {
          setState(() => _length = value);
        }),
        _choices('语气', const ['更克制', '温暖克制', '更亲近'], _tone, (value) {
          setState(() => _tone = value);
        }),
        _choices('叙述', const ['第一人称', '旁观叙述'], _person, (value) {
          setState(() => _person = value);
        }),
        _choices('情绪温度', const ['冷静', '温和', '明亮'], _temperature, (value) {
          setState(() => _temperature = value);
        }),
        _choices('修辞', const ['直接', '少量修辞', '更有画面'], _rhetoric, (value) {
          setState(() => _rhetoric = value);
        }),
        const SizedBox(height: 10),
        TextField(
          controller: _examples,
          minLines: 2,
          maxLines: 4,
          decoration: const InputDecoration(
            labelText: '喜欢的表达（每行一条）',
            hintText: '普通的一天，也有值得留下的光。',
          ),
        ),
        const SizedBox(height: 12),
        TextField(
          controller: _banned,
          minLines: 2,
          maxLines: 4,
          decoration: const InputDecoration(labelText: '不想出现的表达（每行一条）'),
        ),
        const SizedBox(height: 18),
        Container(
          width: double.infinity,
          padding: const EdgeInsets.all(15),
          color: SketchColors.soft,
          child: Text(
            '预览 · ${_label.text.trim().isEmpty ? '未命名风格' : _label.text.trim()}\n'
            '${_person == '第一人称' ? '我' : '她'}在傍晚的街角停了一会儿。'
            '${_tone == '更克制' ? '风很轻。' : '那一点光，让普通的一天也柔和下来。'}',
            style: const TextStyle(height: 1.7),
          ),
        ),
        const SizedBox(height: 16),
        SketchAction(
          key: const ValueKey('save-style'),
          label: controller.saving ? '正在保存…' : '保存风格',
          filled: true,
          onPressed:
              controller.canSave &&
                  !controller.saving &&
                  _label.text.trim().isNotEmpty
              ? _save
              : null,
        ),
        const SizedBox(height: 10),
        const Text(
          '保存偏好只影响之后的新整理任务，不会重写已有日记。当前契约没有删除风格入口。',
          style: TextStyle(color: SketchColors.muted, fontSize: 12),
        ),
      ],
    ),
  );

  Widget _choices(
    String label,
    List<String> choices,
    String selected,
    ValueChanged<String> onSelected,
  ) => Padding(
    padding: const EdgeInsets.only(bottom: 10),
    child: Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Text(label, style: const TextStyle(fontWeight: FontWeight.w600)),
        const SizedBox(height: 5),
        Wrap(
          spacing: 7,
          runSpacing: 7,
          children: [
            for (final choice in choices)
              ChoiceChip(
                label: Text(choice),
                selected: choice == selected,
                onSelected: (_) => onSelected(choice),
              ),
          ],
        ),
      ],
    ),
  );

  void _edit(StyleProfile profile) {
    setState(() {
      _editing = profile;
      _label.text = profile.label;
      _length = profile.lengthPreference ?? '适中';
      _tone = profile.tone ?? '温暖克制';
      _person = profile.person ?? '第一人称';
      _temperature = profile.emotionalTemperature ?? '温和';
      _rhetoric = profile.rhetoricLevel ?? '少量修辞';
      _examples.text = profile.examples.join('\n');
      _banned.text = profile.bannedExpressions.join('\n');
    });
  }

  void _newStyle() {
    setState(() {
      _editing = null;
      _label.text = '我的日记风格';
      _length = '适中';
      _tone = '温暖克制';
      _person = '第一人称';
      _temperature = '温和';
      _rhetoric = '少量修辞';
      _examples.clear();
      _banned.clear();
    });
  }

  Future<void> _save() async {
    final saved = await controller.save(
      existing: _editing,
      label: _label.text,
      lengthPreference: _length,
      tone: _tone,
      person: _person,
      emotionalTemperature: _temperature,
      rhetoricLevel: _rhetoric,
      examples: _lines(_examples.text),
      bannedExpressions: _lines(_banned.text),
    );
    if (saved != null && mounted) _edit(saved);
  }

  Widget _notice(String message) => SketchFrame(
    fill: SketchColors.soft,
    padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 13),
    child: Text(message),
  );
}

List<String> _lines(String value) => value
    .split('\n')
    .map((line) => line.trim())
    .where((line) => line.isNotEmpty)
    .toList(growable: false);

String _summary(StyleProfile profile) => [
  profile.lengthPreference,
  profile.tone,
  profile.person,
].whereType<String>().where((value) => value.isNotEmpty).join(' · ');

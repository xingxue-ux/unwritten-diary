import 'package:diary_api/diary_api.dart';
import 'package:flutter/material.dart';

import 'sketch_ui.dart';
import 'style_settings_controller.dart';

class StyleSettingsPanel extends StatefulWidget {
  const StyleSettingsPanel({super.key, required this.controller});
  final StyleSettingsController controller;

  @override
  State<StyleSettingsPanel> createState() => StyleSettingsPanelState();
}

class StyleSettingsPanelState extends State<StyleSettingsPanel> {
  final _label = TextEditingController(text: '我的日记风格');
  final _examples = TextEditingController();
  final _banned = TextEditingController();
  StyleProfile? _editing;

  /// 五个维度的当前值。**不是枚举**：核心对这几个字段只做长度校验
  /// （`styles.rs` 的 `validate_style_text`），所以「自定义」填什么都会原样
  /// 进提示词。界面给一组常见说法只是省打字，不是限制。
  String _length = '适中';
  String _tone = kStyleTonePresets[2];
  String _person = kStylePersonPresets[0];
  String _temperature = kStyleTemperaturePresets[2];
  String _rhetoric = '少量修辞';

  /// 处于自定义模式的维度。**显式记录**，不用「值是否在预设里」反推：
  /// 刚点「自定义」还没输入时值是原预设，反推会立刻把输入框收起来。
  final Set<String> _customKeys = {};

  /// 自定义输入框：切到「自定义」时把原值带进去，避免用户从空白重敲。
  late final Map<String, TextEditingController> _custom = {
    for (final key in kStyleDimensionKeys)
      key: TextEditingController(text: _currentOf(key)),
  };

  StyleSettingsController get controller => widget.controller;

  @override
  void dispose() {
    _label.dispose();
    _examples.dispose();
    _banned.dispose();
    for (final controller in _custom.values) {
      controller.dispose();
    }
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
        _archetypes(),
        const SizedBox(height: 6),
        _dimension('length', '篇幅', kStyleLengthPresets, _length, '例如：三段以内，不超过 200 字'),
        _dimension('tone', '语气', kStyleTonePresets, _tone, '例如：像跟老朋友说话，别太文艺'),
        _dimension('person', '叙述', kStylePersonPresets, _person, '例如：第三人称，但只写她自己的感受'),
        _dimension('temperature', '情绪温度', kStyleTemperaturePresets, _temperature, '例如：低落的部分不要强行安慰'),
        _dimension('rhetoric', '修辞', kStyleRhetoricPresets, _rhetoric, '例如：少用形容词，多用具体动作'),
        const SizedBox(height: 4),
        const Text(
          '「自定义」会原样写进提示词：核心对这些字段只校验长度，不限定说法，所以'
          '你可以写得更具体（「像给朋友发消息」「不要总结，不要升华」）。',
          style: TextStyle(color: SketchColors.muted, fontSize: 12),
        ),
        const SizedBox(height: 6),
        const Text(
          '想让改动更小，去「我的 → 整理时机 → 整理方式」。',
          style: TextStyle(color: SketchColors.muted, fontSize: 12),
        ),
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

  /// 一套现成组合，省得五项一项项挑。它们只是把预设填进去，之后仍可逐项改。
  Widget _archetypes() => Padding(
    padding: const EdgeInsets.only(bottom: 12),
    child: Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        const Text('从一套现成的开始', style: TextStyle(fontWeight: FontWeight.w600)),
        const SizedBox(height: 5),
        Wrap(
          spacing: 7,
          runSpacing: 7,
          children: [
            for (final archetype in kStyleArchetypes)
              ActionChip(
                label: Text(archetype.label),
                onPressed: () => setState(() => archetype.applyTo(this)),
              ),
          ],
        ),
      ],
    ),
  );

  /// 一个维度：一排预设 + 「自定义」。
  ///
  /// `value` 不在预设里就说明是自定义值——这时把输入框显示出来并保留它，
  /// 而不是把用户刚打的字丢掉换成第一个预设。
  Widget _dimension(
    String key,
    String label,
    List<String> presets,
    String value,
    String hint,
  ) {
    final isCustom = _customKeys.contains(key);
    return Padding(
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
              for (final preset in presets)
                ChoiceChip(
                  label: Text(preset),
                  selected: !isCustom && preset == value,
                  onSelected: (_) => setState(() {
                    _customKeys.remove(key);
                    _setValue(key, preset);
                  }),
                ),
              ChoiceChip(
                label: const Text('自定义'),
                selected: isCustom,
                onSelected: (_) => setState(() {
                  if (isCustom) return;
                  _custom[key]!.text = value;
                  _customKeys.add(key);
                }),
              ),
            ],
          ),
          if (isCustom) ...[
            const SizedBox(height: 6),
            TextField(
              controller: _custom[key],
              decoration: InputDecoration(
                isDense: true,
                hintText: hint,
                border: const OutlineInputBorder(),
              ),
              onChanged: (text) => setState(() => _setValue(key, text)),
            ),
          ],
        ],
      ),
    );
  }

  void _setValue(String key, String value) {
    switch (key) {
      case 'length':
        _length = value;
      case 'tone':
        _tone = value;
      case 'person':
        _person = value;
      case 'temperature':
        _temperature = value;
      case 'rhetoric':
        _rhetoric = value;
    }
  }

  String _currentOf(String key) => switch (key) {
    'length' => _length,
    'tone' => _tone,
    'person' => _person,
    'temperature' => _temperature,
    _ => _rhetoric,
  };

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

/// 五个维度的预设说法。**核心不认这些词**——它们只是省打字的常见写法，
/// 用户点「自定义」后写什么都行（核心只校验长度）。
const List<String> kStyleDimensionKeys = [
  'length',
  'tone',
  'person',
  'temperature',
  'rhetoric',
];

const List<String> kStyleLengthPresets = [
  // 「跟随原文」放第一档。文案只有一个词：带括号解释就变成一条硬约束了，
  // 模型会照着括号里的细则做事，反而失去「大致跟着原文」的分寸。
  '跟随原文',
  '很短（三五行）',
  '简短',
  '适中',
  '舒展',
  '长（千字上下）',
];

const List<String> kStyleTonePresets = [
  '冷静记录',
  '更克制',
  '温暖克制',
  '更亲近',
  '俏皮',
  '诗意',
];

const List<String> kStylePersonPresets = [
  '第一人称',
  '旁观叙述',
  '第二人称（对她说）',
];

const List<String> kStyleTemperaturePresets = [
  '克制',
  '冷静',
  '温和',
  '明亮',
  '热烈',
];

const List<String> kStyleRhetoricPresets = [
  '少润色，尽量用原话',
  '直接',
  '少量修辞',
  '更有画面',
  '多用比喻',
];

/// 一套现成组合：五个维度各取一个。
class StyleArchetype {
  const StyleArchetype({
    required this.label,
    required this.length,
    required this.tone,
    required this.person,
    required this.temperature,
    required this.rhetoric,
  });

  final String label;
  final String length;
  final String tone;
  final String person;
  final String temperature;
  final String rhetoric;

  /// 只填值，不改名称与示例——用户可能已经写了自己的示例。
  void applyTo(StyleSettingsPanelState state) {
    state._length = length;
    state._tone = tone;
    state._person = person;
    state._temperature = temperature;
    state._rhetoric = rhetoric;
    state._customKeys.clear();
    for (final key in kStyleDimensionKeys) {
      state._custom[key]!.text = switch (key) {
        'length' => length,
        'tone' => tone,
        'person' => person,
        'temperature' => temperature,
        _ => rhetoric,
      };
    }
  }
}

const List<StyleArchetype> kStyleArchetypes = [
  StyleArchetype(
    label: '日记体',
    length: '简短',
    tone: '温暖克制',
    person: '第一人称',
    temperature: '温和',
    rhetoric: '少量修辞',
  ),
  StyleArchetype(
    label: '随笔',
    length: '舒展',
    tone: '诗意',
    person: '第一人称',
    temperature: '明亮',
    rhetoric: '更有画面',
  ),
  StyleArchetype(
    label: '记录体',
    length: '很短（三五行）',
    tone: '冷静记录',
    person: '旁观叙述',
    temperature: '克制',
    rhetoric: '直接',
  ),
  StyleArchetype(
    label: '书信',
    length: '适中',
    tone: '更亲近',
    person: '第二人称（对她说）',
    temperature: '温和',
    rhetoric: '少量修辞',
  ),
];

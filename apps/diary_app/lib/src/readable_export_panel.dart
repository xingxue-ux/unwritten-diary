import 'dart:io';

import 'package:file_selector/file_selector.dart';
import 'package:flutter/material.dart';
import 'package:path/path.dart' as path;

import 'readable_export_controller.dart';
import 'sketch_ui.dart';
import 'ui_strings.dart';

class ReadableExportPanel extends StatefulWidget {
  const ReadableExportPanel({super.key, required this.controller});
  final ReadableExportController controller;

  @override
  State<ReadableExportPanel> createState() => _ReadableExportPanelState();
}

class _ReadableExportPanelState extends State<ReadableExportPanel> {
  late DateTime _from;
  late DateTime _to;
  String _format = 'markdown';
  bool _includeAssets = false;

  ReadableExportController get controller => widget.controller;

  @override
  void initState() {
    super.initState();
    _to = DateTime.now();
    _from = DateTime(_to.year, _to.month - 1, _to.day);
  }

  @override
  Widget build(BuildContext context) => AnimatedBuilder(
    animation: controller,
    builder: (context, _) => Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        const Text(
          '阅读导出',
          style: TextStyle(
            fontFamily: SketchFonts.display,
            fontSize: 31,
            color: SketchColors.ink,
          ),
        ),
        const SizedBox(height: 5),
        const Text(
          '选择日期与格式，生成方便阅读和分享的副本。',
          style: TextStyle(color: SketchColors.muted, fontSize: 15),
        ),
        const SizedBox(height: 18),
        SketchFrame(
          fill: SketchColors.soft,
          child: Text(
            '${uiStrings.t('export.contains_private_data')}\n'
            '阅读导出不能用于完整恢复；需要迁移资料时请使用“完整备份”。',
          ),
        ),
        const SizedBox(height: 18),
        SketchFrame(
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              const Text('日期范围', style: TextStyle(fontWeight: FontWeight.w700)),
              const SizedBox(height: 10),
              Wrap(
                spacing: 10,
                runSpacing: 10,
                children: [
                  SketchAction(
                    label: '从 ${_date(_from)}',
                    icon: Icons.calendar_today_outlined,
                    onPressed: () => _pickDate(true),
                  ),
                  SketchAction(
                    label: '到 ${_date(_to)}',
                    icon: Icons.event_outlined,
                    onPressed: () => _pickDate(false),
                  ),
                ],
              ),
              const Divider(color: SketchColors.line, height: 30),
              const Text('格式', style: TextStyle(fontWeight: FontWeight.w700)),
              const SizedBox(height: 8),
              Wrap(
                spacing: 8,
                children: [
                  ChoiceChip(
                    label: const Text('Markdown'),
                    selected: _format == 'markdown',
                    onSelected: (_) => setState(() => _format = 'markdown'),
                  ),
                  ChoiceChip(
                    label: const Text('纯文本'),
                    selected: _format == 'plain_text',
                    onSelected: (_) => setState(() => _format = 'plain_text'),
                  ),
                ],
              ),
              const SizedBox(height: 10),
              SwitchListTile(
                contentPadding: EdgeInsets.zero,
                title: const Text('附带原始材料'),
                subtitle: const Text('文件会更大；正文始终包含在导出中。'),
                value: _includeAssets,
                onChanged: (value) => setState(() => _includeAssets = value),
              ),
              const SizedBox(height: 12),
              SketchAction(
                key: const ValueKey('create-readable-export'),
                label: controller.working ? '正在导出…' : '生成阅读文档',
                icon: Icons.article_outlined,
                filled: true,
                onPressed: controller.canCreate && !controller.working
                    ? () => controller.create(
                        from: _from,
                        to: _to,
                        format: _format,
                        includeAssets: _includeAssets,
                      )
                    : null,
              ),
            ],
          ),
        ),
        const SizedBox(height: 18),
        SketchFrame(
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Text(
                controller.message ??
                    (controller.canCreate
                        ? '尚未开始导出。'
                        : '等待 #113 的导出核心接入后自动启用。'),
              ),
              if (controller.progress != null) ...[
                const SizedBox(height: 12),
                LinearProgressIndicator(
                  value: controller.progress!.clamp(0, 1),
                ),
              ] else if (controller.working) ...[
                const SizedBox(height: 12),
                const LinearProgressIndicator(),
              ],
              if (controller.working && controller.canCancel) ...[
                const SizedBox(height: 8),
                TextButton(
                  onPressed: controller.cancel,
                  child: const Text('停止导出'),
                ),
              ],
              if (controller.ticket?.packageTicket case final package?) ...[
                const SizedBox(height: 8),
                Text(
                  '导出包：$package',
                  style: const TextStyle(
                    color: SketchColors.muted,
                    fontSize: 12,
                  ),
                ),
                const SizedBox(height: 10),
                Wrap(
                  spacing: 10,
                  runSpacing: 8,
                  children: [
                    // 核心把文件写在自己的 exports 目录里；用户要的是「存到我能找到的地方」。
                    SketchAction(
                      label: '另存为…',
                      icon: Icons.save_alt,
                      compact: true,
                      onPressed: () => _saveAs(package),
                    ),
                    SketchAction(
                      label: '打开所在文件夹',
                      icon: Icons.folder_open,
                      compact: true,
                      onPressed: () => _reveal(package),
                    ),
                  ],
                ),
              ],
            ],
          ),
        ),
        const SizedBox(height: 12),
        const Text(
          '导出文件先写在资料库的 exports 目录里；用「另存为…」复制到你选的位置。',
          style: TextStyle(color: SketchColors.muted, fontSize: 12),
        ),
      ],
    ),
  );

  /// 把核心写好的导出文件复制到用户选的位置。
  ///
  /// **不移动**：核心那条路径是任务记录里写的产物位置，挪走会让 `jobs` 里的
  /// `targetIds` 指向不存在的文件。
  Future<void> _saveAs(String sourcePath) async {
    try {
      final location = await getSaveLocation(
        suggestedName: path.basename(sourcePath),
      );
      if (location == null) return;
      await File(sourcePath).copy(location.path);
      if (!mounted) return;
      ScaffoldMessenger.of(context).showSnackBar(
        SnackBar(content: Text('已保存到 ${location.path}')),
      );
    } on Object catch (failure) {
      if (!mounted) return;
      ScaffoldMessenger.of(context).showSnackBar(
        SnackBar(content: Text('另存为没完成：$failure')),
      );
    }
  }

  /// 在文件管理器里定位这个文件。Windows 用 `explorer /select,`。
  Future<void> _reveal(String filePath) async {
    try {
      if (Platform.isWindows) {
        await Process.run('explorer.exe', ['/select,', filePath]);
      } else if (Platform.isMacOS) {
        await Process.run('open', ['-R', filePath]);
      } else {
        await Process.run('xdg-open', [path.dirname(filePath)]);
      }
    } on Object catch (failure) {
      if (!mounted) return;
      ScaffoldMessenger.of(context).showSnackBar(
        SnackBar(content: Text('打不开文件夹：$failure')),
      );
    }
  }

  Future<void> _pickDate(bool start) async {
    final picked = await showDatePicker(
      context: context,
      initialDate: start ? _from : _to,
      firstDate: DateTime(2000),
      lastDate: DateTime.now().add(const Duration(days: 1)),
    );
    if (picked == null) return;
    setState(() {
      if (start) {
        _from = picked;
      } else {
        _to = picked;
      }
    });
  }
}

String _date(DateTime value) =>
    '${value.year}-${value.month.toString().padLeft(2, '0')}-${value.day.toString().padLeft(2, '0')}';

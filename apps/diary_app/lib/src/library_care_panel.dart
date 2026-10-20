import 'package:diary_api/diary_api.dart';
import 'package:flutter/material.dart';

import 'library_care_controller.dart';
import 'sketch_ui.dart';
import 'ui_strings.dart';

typedef TicketPicker = Future<String?> Function();

class BackupRestorePanel extends StatelessWidget {
  const BackupRestorePanel({
    super.key,
    required this.controller,
    required this.pickBackupTarget,
    required this.pickBackupPackage,
  });

  final LibraryCareController controller;
  final TicketPicker pickBackupTarget;
  final TicketPicker pickBackupPackage;

  @override
  Widget build(BuildContext context) => AnimatedBuilder(
    animation: controller,
    builder: (context, _) => Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        const _PanelTitle(title: '备份与恢复', subtitle: '完整备份用于恢复资料库；阅读导出用于查看和分享。'),
        const SizedBox(height: 18),
        _CapabilityNotice(controller: controller),
        const SizedBox(height: 18),
        SketchFrame(
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              const _FlowHeading(
                icon: Icons.inventory_2_outlined,
                title: '完整备份',
                detail: '包含原始材料、日记版本和必要配置，可以恢复。',
              ),
              const SizedBox(height: 8),
              Text(
                uiStrings.t('backup.not_encrypted'),
                style: const TextStyle(color: SketchColors.muted, height: 1.5),
              ),
              const SizedBox(height: 14),
              SketchAction(
                key: const ValueKey('create-backup'),
                label: controller.canCreateBackup ? '选择位置并创建' : '等待核心接入',
                icon: Icons.save_alt,
                filled: true,
                onPressed: controller.canCreateBackup
                    ? () async {
                        final ticket = await pickBackupTarget();
                        if (ticket != null) {
                          await controller.createBackup(ticket);
                        }
                      }
                    : null,
              ),
              const Divider(color: SketchColors.line, height: 32),
              const _FlowHeading(
                icon: Icons.article_outlined,
                title: '阅读导出',
                detail: '按日期与格式导出正文，不能作为完整备份恢复。',
              ),
              const SizedBox(height: 10),
              Text(
                controller.capabilities.contains('exports.create')
                    ? '导出核心已就绪，日期和格式选择将在“阅读导出”流程中提供。'
                    : '等待 #113 的导出核心接入后开放日期、格式和保存位置。',
                style: const TextStyle(color: SketchColors.muted),
              ),
            ],
          ),
        ),
        const SizedBox(height: 18),
        SketchFrame(
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              const _FlowHeading(
                icon: Icons.settings_backup_restore,
                title: '恢复备份',
                detail: '先检查版本与完整性，确认后恢复到新的资料库。',
              ),
              const SizedBox(height: 14),
              SketchAction(
                key: const ValueKey('inspect-backup'),
                label: controller.canInspectBackup ? '选择备份并检查' : '等待核心接入',
                icon: Icons.folder_open,
                onPressed: controller.canInspectBackup
                    ? () async {
                        final ticket = await pickBackupPackage();
                        if (ticket != null) {
                          await controller.inspectBackup(ticket);
                        }
                      }
                    : null,
              ),
              if (controller.inspection case final inspection?) ...[
                const SizedBox(height: 16),
                _InspectionCard(inspection: inspection),
                const SizedBox(height: 14),
                SketchAction(
                  key: const ValueKey('restore-backup'),
                  label: '恢复到新的资料库',
                  filled: true,
                  onPressed:
                      inspection.issues.isEmpty && controller.canRestoreBackup
                      ? () => _confirmRestore(context)
                      : null,
                ),
              ],
            ],
          ),
        ),
        if (controller.message != null || controller.activeJobId != null) ...[
          const SizedBox(height: 18),
          _JobStatus(controller: controller),
        ],
      ],
    ),
  );

  Future<void> _confirmRestore(BuildContext context) async {
    final confirmed = await showDialog<bool>(
      context: context,
      builder: (context) => AlertDialog(
        title: const Text('恢复备份'),
        content: Text(uiStrings.t('danger.restore_confirm')),
        actions: [
          TextButton(
            onPressed: () => Navigator.pop(context, false),
            child: const Text('取消'),
          ),
          TextButton(
            onPressed: () => Navigator.pop(context, true),
            child: const Text('开始恢复'),
          ),
        ],
      ),
    );
    if (confirmed == true) await controller.restoreInspectedBackup();
  }
}

class StorageTrashPanel extends StatelessWidget {
  const StorageTrashPanel({super.key, required this.controller});

  final LibraryCareController controller;

  @override
  Widget build(BuildContext context) => AnimatedBuilder(
    animation: controller,
    builder: (context, _) => Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        const _PanelTitle(title: '存储与回收站', subtitle: '先看清边界和影响，再决定清理。'),
        const SizedBox(height: 18),
        SketchFrame(
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Text(
                '资料库记录 · ${controller.captureCount} 条',
                style: const TextStyle(
                  fontSize: 17,
                  fontWeight: FontWeight.w700,
                ),
              ),
              const SizedBox(height: 8),
              const Text(
                '当前接口尚未提供可靠的分类容量，因此不显示估算数字。',
                style: TextStyle(color: SketchColors.muted),
              ),
              const Divider(color: SketchColors.line, height: 28),
              for (final item in const <(String, String)>[
                ('原始材料', '照片、录音和文件；清理缓存不会删除它们。'),
                ('缓存', '可重新生成的临时内容。'),
                ('索引', '用于搜索，可以通过重建恢复。'),
                ('备份', '保存在你选择的位置，不计入资料库内占用。'),
              ])
                ListTile(
                  contentPadding: EdgeInsets.zero,
                  dense: true,
                  title: Text(item.$1),
                  subtitle: Text(item.$2),
                ),
            ],
          ),
        ),
        const SizedBox(height: 18),
        SketchFrame(
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Row(
                children: [
                  const Expanded(
                    child: _FlowHeading(
                      icon: Icons.delete_outline,
                      title: '回收站',
                      detail: '恢复不会丢失版本；永久清理前必须预览影响。',
                    ),
                  ),
                  IconButton(
                    tooltip: '刷新回收站',
                    onPressed: controller.canListCaptures
                        ? controller.loadTrash
                        : null,
                    icon: const Icon(Icons.refresh),
                  ),
                ],
              ),
              const SizedBox(height: 12),
              if (controller.loadingTrash)
                const LinearProgressIndicator()
              else if (!controller.canListCaptures)
                const Text(
                  '当前核心不能读取回收站。',
                  style: TextStyle(color: SketchColors.muted),
                )
              else if (controller.trashedCaptures.isEmpty)
                const Text(
                  '回收站是空的。',
                  style: TextStyle(color: SketchColors.muted),
                )
              else
                for (final capture in controller.trashedCaptures)
                  _TrashTile(
                    capture: capture,
                    canRestore: controller.canRestoreTrash,
                    canPurge: controller.canPreviewPurge && controller.canPurge,
                    onRestore: () => controller.restoreCapture(capture),
                    onPurge: () => _previewAndConfirmPurge(context, capture),
                  ),
            ],
          ),
        ),
        if (controller.message != null) ...[
          const SizedBox(height: 18),
          _MessageCard(message: controller.message!),
        ],
      ],
    ),
  );

  Future<void> _previewAndConfirmPurge(
    BuildContext context,
    Capture capture,
  ) async {
    final preview = await controller.previewPurge(capture);
    if (preview == null || !context.mounted) return;
    final confirmed = await showDialog<bool>(
      context: context,
      builder: (context) => AlertDialog(
        title: const Text('永久清理'),
        content: Column(
          mainAxisSize: MainAxisSize.min,
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Text(uiStrings.t('danger.purge_confirm')),
            const SizedBox(height: 12),
            Text(
              '将删除 ${preview.assetIds.length} 个原件、'
              '${preview.derivedContentIds.length} 项派生内容。'
              '${preview.diaryVersionsStillReferencing.isEmpty ? '' : '\n另有 ${preview.diaryVersionsStillReferencing.length} 个日记版本引用过它，会单独列出。'}',
            ),
          ],
        ),
        actions: [
          TextButton(
            onPressed: () => Navigator.pop(context, false),
            child: const Text('保留'),
          ),
          TextButton(
            onPressed: () => Navigator.pop(context, true),
            child: const Text('永久清理'),
          ),
        ],
      ),
    );
    if (confirmed == true) await controller.purge(preview);
  }
}

class _CapabilityNotice extends StatelessWidget {
  const _CapabilityNotice({required this.controller});
  final LibraryCareController controller;

  @override
  Widget build(BuildContext context) {
    final ready =
        controller.canCreateBackup &&
        controller.canInspectBackup &&
        controller.canRestoreBackup;
    return _MessageCard(
      message: ready
          ? '备份能力已就绪。所有文件只写入你选择的位置。'
          : '界面已经接好；当前核心尚未开放全部备份能力，可用操作会自动启用。',
    );
  }
}

class _JobStatus extends StatelessWidget {
  const _JobStatus({required this.controller});
  final LibraryCareController controller;

  @override
  Widget build(BuildContext context) {
    final progress = controller.progress;
    final active =
        controller.activeJob != null &&
        !(controller.activeJob?.state.isTerminal ?? true);
    return SketchFrame(
      fill: SketchColors.soft,
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Text(controller.message ?? '任务已提交。'),
          if (controller.activeJobId != null) ...[
            const SizedBox(height: 5),
            Text(
              '任务 ${controller.activeJobId}',
              style: const TextStyle(color: SketchColors.muted, fontSize: 12),
            ),
          ],
          if (progress != null) ...[
            const SizedBox(height: 12),
            LinearProgressIndicator(value: progress.clamp(0, 1)),
          ] else if (active) ...[
            const SizedBox(height: 12),
            const LinearProgressIndicator(),
          ],
          if (active && controller.canCancelJobs) ...[
            const SizedBox(height: 10),
            TextButton(
              onPressed: controller.cancelActiveJob,
              child: const Text('停止任务'),
            ),
          ],
        ],
      ),
    );
  }
}

class _InspectionCard extends StatelessWidget {
  const _InspectionCard({required this.inspection});
  final BackupInspection inspection;

  @override
  Widget build(BuildContext context) => Container(
    padding: const EdgeInsets.all(14),
    color: SketchColors.soft,
    child: Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Text('格式 ${inspection.schemaVersion} · 接口 ${inspection.apiVersion}'),
        Text('${_bytes(inspection.byteSize)} · ${inspection.assetCount} 个原件'),
        Text('创建于 ${inspection.createdAt.toLocal()}'),
        if (inspection.issues.isNotEmpty) ...[
          const Divider(color: SketchColors.line),
          for (final issue in inspection.issues) Text('• $issue'),
        ],
      ],
    ),
  );
}

class _TrashTile extends StatelessWidget {
  const _TrashTile({
    required this.capture,
    required this.canRestore,
    required this.canPurge,
    required this.onRestore,
    required this.onPurge,
  });

  final Capture capture;
  final bool canRestore;
  final bool canPurge;
  final VoidCallback onRestore;
  final VoidCallback onPurge;

  @override
  Widget build(BuildContext context) => Column(
    children: [
      const Divider(color: SketchColors.line),
      ListTile(
        contentPadding: EdgeInsets.zero,
        title: Text(
          capture.draftText.trim().isEmpty ? '未命名记录' : capture.draftText.trim(),
          maxLines: 2,
          overflow: TextOverflow.ellipsis,
        ),
        subtitle: Text(
          '${uiStrings.t('state.trashed')} · ${capture.dayKey} · '
          '${capture.orderedSourceIds.length} 个原始来源',
        ),
      ),
      Wrap(
        spacing: 8,
        children: [
          TextButton(
            onPressed: canRestore ? onRestore : null,
            child: const Text('恢复'),
          ),
          TextButton(
            onPressed: canPurge ? onPurge : null,
            child: const Text('预览并永久清理'),
          ),
        ],
      ),
    ],
  );
}

class _PanelTitle extends StatelessWidget {
  const _PanelTitle({required this.title, required this.subtitle});
  final String title;
  final String subtitle;

  @override
  Widget build(BuildContext context) => Column(
    crossAxisAlignment: CrossAxisAlignment.start,
    children: [
      Text(
        title,
        style: const TextStyle(
          fontFamily: SketchFonts.display,
          fontSize: 31,
          color: SketchColors.ink,
        ),
      ),
      const SizedBox(height: 5),
      Text(
        subtitle,
        style: const TextStyle(color: SketchColors.muted, fontSize: 15),
      ),
    ],
  );
}

class _FlowHeading extends StatelessWidget {
  const _FlowHeading({
    required this.icon,
    required this.title,
    required this.detail,
  });
  final IconData icon;
  final String title;
  final String detail;

  @override
  Widget build(BuildContext context) => Row(
    crossAxisAlignment: CrossAxisAlignment.start,
    children: [
      Icon(icon, color: SketchColors.ink),
      const SizedBox(width: 12),
      Expanded(
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Text(
              title,
              style: const TextStyle(fontSize: 17, fontWeight: FontWeight.w700),
            ),
            const SizedBox(height: 3),
            Text(detail, style: const TextStyle(color: SketchColors.muted)),
          ],
        ),
      ),
    ],
  );
}

class _MessageCard extends StatelessWidget {
  const _MessageCard({required this.message});
  final String message;

  @override
  Widget build(BuildContext context) => SketchFrame(
    fill: SketchColors.soft,
    padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 13),
    child: Text(message),
  );
}

String _bytes(int value) {
  if (value < 1024) return '$value B';
  if (value < 1024 * 1024) return '${(value / 1024).toStringAsFixed(1)} KB';
  if (value < 1024 * 1024 * 1024) {
    return '${(value / (1024 * 1024)).toStringAsFixed(1)} MB';
  }
  return '${(value / (1024 * 1024 * 1024)).toStringAsFixed(1)} GB';
}

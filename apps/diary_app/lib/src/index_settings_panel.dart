import 'package:diary_api/diary_api.dart';
import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';

import 'index_settings_controller.dart';
import 'sketch_ui.dart';
import 'ui_strings.dart';

class IndexSettingsPanel extends StatelessWidget {
  const IndexSettingsPanel({super.key, required this.controller});

  final IndexSettingsController controller;

  @override
  Widget build(BuildContext context) => AnimatedBuilder(
    animation: controller,
    builder: (context, _) {
      final status = controller.status;
      final coverage = status?.coverage ?? Coverage.unavailable;
      final ready = status?.semanticIndexReady == true;
      final partiallySearchable = !ready && coverage == Coverage.partial;
      final progress = controller.progress;
      return Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          const Text(
            '搜索索引',
            style: TextStyle(
              fontFamily: SketchFonts.display,
              fontSize: 31,
              color: SketchColors.ink,
            ),
          ),
          const SizedBox(height: 5),
          const Text(
            '让本机的语义检索跟上新的记录和模型。',
            style: TextStyle(color: SketchColors.muted, fontSize: 15),
          ),
          const SizedBox(height: 18),
          SketchFrame(
            fill: ready ? SketchColors.paper : SketchColors.soft,
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Text(
                  status == null
                      ? '正在确认索引状态'
                      : ready
                      ? '语义检索已就绪'
                      : partiallySearchable
                      ? '语义检索部分可用'
                      : '语义检索暂不可用',
                  style: const TextStyle(
                    color: SketchColors.ink,
                    fontSize: 18,
                    fontWeight: FontWeight.w700,
                  ),
                ),
                const SizedBox(height: 8),
                Text(
                  status == null
                      ? '正在读取本机索引的覆盖情况。'
                      : ready
                      ? '当前生效的索引可以用于语义检索。'
                      : partiallySearchable
                      ? '已嵌入的内容可以搜到，索引覆盖还不完整。'
                      : '文字记录和关键词搜索仍可使用；语义检索要等本地模型与索引就绪。',
                  style: const TextStyle(color: SketchColors.text, height: 1.5),
                ),
                if (partiallySearchable && status!.pendingCount > 0) ...[
                  const SizedBox(height: 8),
                  Text(
                    '还有 ${status.pendingCount} 块待嵌入。',
                    style: const TextStyle(color: SketchColors.text),
                  ),
                ],
                if (status?.modelVersion != null) ...[
                  const SizedBox(height: 10),
                  Text(
                    '索引使用的模型版本：${status!.modelVersion}',
                    style: const TextStyle(
                      color: SketchColors.muted,
                      fontSize: 12,
                    ),
                  ),
                ],
                if (status != null && status.reasons.isNotEmpty) ...[
                  const SizedBox(height: 12),
                  for (final reason in status.reasons)
                    Padding(
                      padding: const EdgeInsets.only(bottom: 5),
                      child: Text(
                        '· $reason',
                        style: const TextStyle(color: SketchColors.text),
                      ),
                    ),
                ],
                if (defaultTargetPlatform == TargetPlatform.windows) ...[
                  const SizedBox(height: 12),
                  const Text(
                    '当前 Windows 构建不含任何本地模型文件；语义检索与录音转写需要配置模型目录后才能使用。应用目前不会自动下载模型。',
                    style: TextStyle(
                      color: SketchColors.muted,
                      fontSize: 12,
                      height: 1.5,
                    ),
                  ),
                ],
              ],
            ),
          ),
          if (controller.job != null) ...[
            const SizedBox(height: 16),
            SketchFrame(
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  const Text(
                    '重建任务',
                    style: TextStyle(fontWeight: FontWeight.w700),
                  ),
                  const SizedBox(height: 10),
                  Text(_jobLabel(controller.job!.state)),
                  if (controller.hasActiveJob && progress != null) ...[
                    const SizedBox(height: 10),
                    LinearProgressIndicator(value: progress.clamp(0, 1)),
                    const SizedBox(height: 5),
                    Text(
                      '${controller.job!.progress!.completed} / ${controller.job!.progress!.total} 块',
                      style: const TextStyle(color: SketchColors.muted),
                    ),
                  ] else if (controller.hasActiveJob) ...[
                    const SizedBox(height: 10),
                    const LinearProgressIndicator(),
                    const SizedBox(height: 5),
                    const Text(
                      '核心尚未给出可计算的进度。',
                      style: TextStyle(color: SketchColors.muted),
                    ),
                  ],
                ],
              ),
            ),
          ],
          if (controller.message != null) ...[
            const SizedBox(height: 15),
            Text(
              controller.message!,
              style: const TextStyle(color: SketchColors.text),
            ),
          ],
          const SizedBox(height: 18),
          Wrap(
            spacing: 12,
            runSpacing: 12,
            children: [
              if (controller.canRebuild)
                SketchAction(
                  key: const ValueKey('rebuild-semantic-index'),
                  label: controller.scheduling ? '正在安排…' : '重建语义索引',
                  filled: !ready,
                  onPressed: controller.scheduling || controller.hasActiveJob
                      ? null
                      : controller.rebuild,
                )
              else
                const Text('当前核心不支持手动重建；请更新应用。'),
              TextButton.icon(
                onPressed: controller.loading ? null : controller.load,
                icon: const Icon(Icons.refresh),
                label: const Text('刷新状态'),
              ),
            ],
          ),
          const SizedBox(height: 12),
          const Text(
            '换用本地模型后，建议在这里重建索引。任务会在本机后台继续；离开页面不会删除任务。',
            style: TextStyle(color: SketchColors.muted, fontSize: 12),
          ),
        ],
      );
    },
  );
}

String _jobLabel(JobState state) => switch (state) {
  JobState.queued => uiStrings.t('job.queued'),
  JobState.running => uiStrings.t('job.running'),
  JobState.succeeded => uiStrings.t('job.succeeded'),
  JobState.retryWait => uiStrings.t('job.retry_wait'),
  JobState.waitingConfiguration => uiStrings.t('job.waiting_configuration'),
  JobState.waitingDependency => uiStrings.t('job.waiting_dependency'),
  JobState.waitingNetwork => uiStrings.t('job.waiting_network'),
  JobState.failed => uiStrings.t('job.failed'),
  JobState.cancelled => uiStrings.t('job.cancelled'),
};

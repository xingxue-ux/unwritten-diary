import 'dart:async';

import 'package:diary_api/diary_api.dart';
import 'package:flutter/material.dart';

import 'sketch_ui.dart';
import 'ui_strings.dart';

/// 任务中心：**读真实任务列表**，不是示例。
///
/// 之前这一页是固定的三条示例（「示例 · 完成」），用户看不出本机到底在跑什么、
/// 卡在哪一步。现在直接读 `jobs.list`，并把「需要配置 / 失败 / 等待重试」如实标出来。
class TasksPanel extends StatefulWidget {
  const TasksPanel({super.key, required this.api});
  final DiaryApi api;

  @override
  State<TasksPanel> createState() => _TasksPanelState();
}

class _TasksPanelState extends State<TasksPanel> {
  List<Job> _jobs = const [];
  bool _loading = true;
  bool _busy = false;
  String? _error;

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
      final page = await widget.api.listJobs(pageSize: 50);
      if (!mounted) return;
      setState(() => _jobs = page.jobs);
    } on DiaryException catch (failure) {
      if (mounted) setState(() => _error = failure.message);
    } on Object catch (failure) {
      if (mounted) setState(() => _error = '$failure');
    } finally {
      if (mounted) setState(() => _loading = false);
    }
  }

  Future<void> _act(Future<void> Function() body, String failureText) async {
    if (_busy) return;
    setState(() => _busy = true);
    try {
      await body();
      await _load();
    } on DiaryException catch (failure) {
      if (mounted) setState(() => _error = failure.message);
    } on Object catch (failure) {
      if (mounted) setState(() => _error = '$failureText：$failure');
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  /// 错误码 → 用户能读的一句话。**说清下一步**，不把枚举名丢给用户。
  String _reasonText(Job job) {
    final code = job.errorCode;
    if (code == null) return '没有记录原因。';
    return switch (code) {
      DiaryErrorCode.providerNotConfigured => '还没有配置能用的模型服务，去「模型服务」里配一个。',
      DiaryErrorCode.permissionDenied => '这个服务没有授权，去「模型服务」里确认授权。',
      DiaryErrorCode.networkUnavailable => '网络暂时不可用，联网后会自动重试。',
      DiaryErrorCode.providerTimeout => '服务响应太慢，稍后会自动重试。',
      DiaryErrorCode.quotaExceeded => '服务的额度用完了，换个服务或稍后再试。',
      DiaryErrorCode.rateLimited => '请求太频繁，稍后会自动重试。',
      DiaryErrorCode.authenticationFailed => '密钥无效，去「模型服务」里重新填一次。',
      DiaryErrorCode.modelUnavailable => '这个模型不可用，去「模型服务」里换一个。',
      DiaryErrorCode.unsupportedFormat => '这个文件的格式还不支持，换个文件或先转成文本。',
      DiaryErrorCode.integrityFailed => '文件内容与记录对不上，可能被改动过。',
      DiaryErrorCode.invalidState => '当前状态不允许这一步；刷新后按最新状态再试。',
      DiaryErrorCode.revisionConflict => '内容已被修改；刷新后重新操作。',
      DiaryErrorCode.notFound => '要处理的内容不在了（可能已被删除）。',
      DiaryErrorCode.storageFull => '磁盘空间不够了，清理一些空间再试。',
      DiaryErrorCode.assetMissing => '原件文件不在了（可能被移动或删除）。',
      DiaryErrorCode.invalidArgument => '这一步的参数不对，可能是版本不匹配。',
      DiaryErrorCode.idempotencyConflict => '同一个操作用了不同的内容重试，已拒绝。',
      DiaryErrorCode.pluginDenied => '插件没有这个权限，去插件页确认授权。',
      DiaryErrorCode.pluginBudgetExceeded => '插件用完了这次允许的执行预算。',
      DiaryErrorCode.incompatibleBackup => '备份包来自更新的版本，先升级应用。',
      DiaryErrorCode.corruptedBackup => '备份包损坏了，换一个备份试试。',
      DiaryErrorCode.searchExpired => '搜索会话过期了，重新搜一次。',
      DiaryErrorCode.cursorExpired => '翻页游标过期了，重新加载这一页。',
      DiaryErrorCode.unknown => '本机没有给出具体原因；可以在「诊断」里看这次运行的记录。',
    };
  }

  @override
  Widget build(BuildContext context) => Column(
    crossAxisAlignment: CrossAxisAlignment.stretch,
    children: [
      const Text(
        '任务中心',
        style: TextStyle(
          fontFamily: SketchFonts.display,
          fontSize: 31,
          color: SketchColors.ink,
        ),
      ),
      const SizedBox(height: 5),
      const Text(
        '本机正在做什么、卡在哪一步。',
        style: TextStyle(color: SketchColors.muted, fontSize: 15),
      ),
      const SizedBox(height: 18),
      if (_error != null) ...[
        SketchFrame(
          fill: SketchColors.soft,
          child: Text(_error!, style: const TextStyle(color: SketchColors.ink)),
        ),
        const SizedBox(height: 12),
      ],
      Row(
        children: [
          SketchAction(
            label: '刷新',
            icon: Icons.refresh,
            busy: _loading,
            onPressed: _loading ? null : () => unawaited(_load()),
          ),
        ],
      ),
      const SizedBox(height: 14),
      if (_loading && _jobs.isEmpty)
        const Padding(
          padding: EdgeInsets.symmetric(vertical: 24),
          child: Center(child: CircularProgressIndicator()),
        )
      else if (_jobs.isEmpty)
        const Text(
          '还没有任务。整理、导入、重建索引都会在这里出现。',
          style: TextStyle(color: SketchColors.muted),
        )
      else
        for (final job in _jobs) ...[
          _jobCard(job),
          const SizedBox(height: 10),
        ],
    ],
  );

  Widget _jobCard(Job job) {
    final terminal = job.state.isTerminal;
    return SketchFrame(
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Row(
            children: [
              Expanded(
                child: Text(
                  _kindLabel(job.kind),
                  style: const TextStyle(fontWeight: FontWeight.w700),
                ),
              ),
              Text(
                _stateLabel(job.state),
                style: TextStyle(
                  color: job.state == JobState.failed
                      ? SketchColors.ink
                      : SketchColors.muted,
                  fontWeight: job.state == JobState.failed
                      ? FontWeight.w700
                      : FontWeight.w400,
                ),
              ),
            ],
          ),
          const SizedBox(height: 4),
          Text(
            '${_time(job.createdAt)} · 尝试 ${job.attemptCount} 次',
            style: const TextStyle(color: SketchColors.muted, fontSize: 12),
          ),
          if (job.progress case final progress?) ...[
            const SizedBox(height: 8),
            LinearProgressIndicator(
              value: progress.total <= 0
                  ? null
                  : progress.completed / progress.total,
            ),
            const SizedBox(height: 4),
            Text(
              '${progress.completed} / ${progress.total}',
              style: const TextStyle(color: SketchColors.muted, fontSize: 12),
            ),
          ],
          if (job.errorCode != null) ...[
            const SizedBox(height: 6),
            Text(
              // 不能直接插值枚举：用户看到的是 `DiaryErrorCode.unknown`，
              // 既看不懂也不知道下一步做什么。
              '原因：${_reasonText(job)}',
              style: const TextStyle(color: SketchColors.muted, fontSize: 12),
            ),
          ],
          if (!terminal) ...[
            const SizedBox(height: 8),
            Wrap(
              spacing: 8,
              children: [
                SketchAction(
                  label: '取消',
                  compact: true,
                  onPressed: _busy
                      ? null
                      : () => unawaited(
                          _act(
                            () => widget.api.cancelJob(
                              id: job.id,
                              operationId: _operation('cancel'),
                            ),
                            '取消没成功',
                          ),
                        ),
                ),
              ],
            ),
          ] else if (job.state == JobState.failed) ...[
            const SizedBox(height: 8),
            Wrap(
              spacing: 8,
              children: [
                SketchAction(
                  label: '重试',
                  compact: true,
                  onPressed: _busy
                      ? null
                      : () => unawaited(
                          _act(
                            () => widget.api.retryJob(
                              id: job.id,
                              operationId: _operation('retry'),
                            ),
                            '重试没成功',
                          ),
                        ),
                ),
              ],
            ),
          ],
        ],
      ),
    );
  }

  /// 每次操作一个幂等 id：同一个 id 重放不会产生第二次副作用。
  static String _operation(String action) =>
      'ui-$action-${DateTime.now().microsecondsSinceEpoch}';

  static String _kindLabel(String kind) => switch (kind) {
    'diary_generate' => '整理日记',
    'transcribe' => '录音转写',
    'semantic_index' => '语义索引',
    'index_rebuild' => '重建索引',
    'import' => '导入文件',
    'backup' => '备份',
    'restore' => '恢复',
    _ => kind,
  };

  static String _stateLabel(JobState state) => switch (state) {
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

  static String _time(DateTime value) {
    final local = value.toLocal();
    String two(int n) => n.toString().padLeft(2, '0');
    return '${local.year}-${two(local.month)}-${two(local.day)} '
        '${two(local.hour)}:${two(local.minute)}';
  }
}

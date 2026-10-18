import 'dart:async';
import 'dart:io';

import 'package:diary_api/diary_api.dart';
import 'package:flutter/material.dart';
import 'package:path/path.dart' as path;
import 'package:path_provider/path_provider.dart';

import 'ai_diary_controller.dart';
import 'core_wakeup.dart';
import 'sketch_ui.dart';

class AiDiaryPanel extends StatefulWidget {
  const AiDiaryPanel({
    super.key,
    required this.api,
    required this.onOpenModel,
    this.initialDayKey,
  });

  final DiaryApi api;
  final VoidCallback onOpenModel;
  final String? initialDayKey;

  @override
  State<AiDiaryPanel> createState() => _AiDiaryPanelState();
}

class _AiDiaryPanelState extends State<AiDiaryPanel> {
  AiDiaryController? _controller;
  bool _remoteConfirmed = false;
  String? _setupError;

  static String _key(DateTime day) =>
      '${day.year.toString().padLeft(4, '0')}-${day.month.toString().padLeft(2, '0')}-${day.day.toString().padLeft(2, '0')}';

  @override
  void initState() {
    super.initState();
    unawaited(_initialize());
  }

  Future<void> _initialize() async {
    try {
      final directory = await getApplicationSupportDirectory();
      final store = FileAiLocalStore(
        File(path.join(directory.path, 'ai-diary-ui.json')),
      );
      final controller = AiDiaryController(widget.api, store);
      if (!mounted) {
        controller.dispose();
        return;
      }
      setState(() => _controller = controller);
      await controller.open(widget.initialDayKey ?? _key(DateTime.now()));
    } on Object {
      if (mounted) {
        setState(() => _setupError = '暂时无法读取本机的整理状态，请稍后重试。');
      }
    }
  }

  @override
  void dispose() {
    _controller?.dispose();
    super.dispose();
  }

  Future<void> _chooseDay(AiDiaryController controller) async {
    final initial = DateTime.tryParse(controller.dayKey) ?? DateTime.now();
    final picked = await showDatePicker(
      context: context,
      initialDate: initial,
      firstDate: DateTime(2000),
      lastDate: DateTime.now().add(const Duration(days: 365)),
    );
    if (picked == null) return;
    setState(() => _remoteConfirmed = false);
    await controller.open(_key(picked));
  }

  bool _isRemote(Provider provider) {
    final host =
        Uri.tryParse(provider.endpoint ?? '')?.host.toLowerCase() ?? '';
    return host != 'localhost' && host != '127.0.0.1' && host != '::1';
  }

  String _providerNotice(Provider? provider, List<Provider> eligible) {
    if (provider == null) {
      return '默认不联网。尚未配置可生成文字的服务；原始记录和搜索仍可使用。';
    }
    final remote = eligible.where(_isRemote).toList(growable: false);
    if (remote.isNotEmpty) {
      final destinations = remote
          .map((item) => Uri.tryParse(item.endpoint ?? '')?.host)
          .whereType<String>()
          .where((host) => host.isNotEmpty)
          .toSet();
      return destinations.isEmpty
          ? '已启用服务中有无法展示地址的服务。请先到「模型服务」核对数据去向，再决定是否整理。'
          : '整理时核心会从已启用服务中选择执行者；当天材料可能发送到 ${destinations.join('、')}。请先确认这些服务的隐私规则与额度。';
    }
    final host = Uri.tryParse(provider.endpoint ?? '')?.host;
    final destination = host == null || host.isEmpty ? provider.label : host;
    if (host == null || host.isEmpty) {
      return '此服务没有可展示的地址。整理前请到「模型服务」核对数据去向与配额。';
    }
    return '点击整理后，会交给已启用的本机服务处理（例如 ${provider.label}：$destination）。';
  }

  Future<void> _generate(
    AiDiaryController controller, {
    bool again = false,
  }) async {
    await controller.generate(regenerate: again);
    if (controller.job != null) {
      unawaited(CoreWakeupScheduler.afterCoreMutation(widget.api));
    }
  }

  @override
  Widget build(BuildContext context) {
    final controller = _controller;
    if (_setupError != null) return _notice(_setupError!);
    if (controller == null) {
      return const Center(child: CircularProgressIndicator());
    }
    return AnimatedBuilder(
      animation: controller,
      builder: (context, _) => _content(controller),
    );
  }

  Widget _content(AiDiaryController controller) {
    final provider = controller.generationProvider;
    final job = controller.job;
    final eligible = controller.providers
        .where(
          (item) =>
              item.enabled &&
              item.capabilities.contains(ProviderCapability.generateText),
        )
        .toList(growable: false);
    final remote = eligible.any(_isRemote);
    final unknownDestination = eligible.any(
      (item) =>
          _isRemote(item) &&
          (Uri.tryParse(item.endpoint ?? '')?.host.isEmpty ?? true),
    );
    final canGenerate =
        provider != null &&
        !unknownDestination &&
        (controller.day?.materialCounts.captures ?? 0) > 0 &&
        (!remote || _remoteConfirmed) &&
        !controller.busy &&
        job?.state != JobState.running &&
        job?.state != JobState.queued;
    return Column(
      key: const ValueKey('ai-diary'),
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        const Text(
          '日记',
          style: TextStyle(
            fontFamily: SketchFonts.display,
            color: SketchColors.ink,
            fontSize: 40,
          ),
        ),
        const Text('把当天已保存的材料整理成一页；由你决定哪一版是当前正文。'),
        const SizedBox(height: 20),
        SketchAction(
          label:
              '日期 ${controller.dayKey.isEmpty ? '今天' : controller.dayKey} · 更换',
          icon: Icons.calendar_today_outlined,
          onPressed: () => unawaited(_chooseDay(controller)),
        ),
        const SizedBox(height: 16),
        _notice(_providerNotice(provider, eligible)),
        if (controller.providerProblem != null) ...[
          const SizedBox(height: 12),
          _notice(controller.providerProblem!),
        ],
        const SizedBox(height: 12),
        _notice('配额和本次费用尚无可靠统计；界面不会猜测用量。'),
        if (provider == null) ...[
          const SizedBox(height: 12),
          SketchAction(
            label: '查看模型服务',
            icon: Icons.settings_outlined,
            onPressed: widget.onOpenModel,
          ),
        ] else if (provider.testStatus != ProviderTestStatus.ok) ...[
          const SizedBox(height: 12),
          _notice('此服务尚未通过连接测试。任务可能等待配置，请先检查服务。'),
        ],
        if (remote) ...[
          const SizedBox(height: 10),
          CheckboxListTile(
            contentPadding: EdgeInsets.zero,
            title: const Text('我确认把当天材料发送到上述服务'),
            subtitle: const Text('每次重新打开本页都需要重新确认。'),
            value: _remoteConfirmed,
            onChanged: (value) =>
                setState(() => _remoteConfirmed = value ?? false),
          ),
        ],
        const SizedBox(height: 18),
        _notice(
          controller.day == null
              ? '这一天还没有记录。先保存文字或导入材料，再回来整理。'
              : '当天记录 ${controller.day!.materialCounts.captures} 条 · 已处理 ${controller.day!.materialCounts.processed} 条 · 待处理 ${controller.day!.materialCounts.pending} 条。',
        ),
        const SizedBox(height: 16),
        Wrap(
          spacing: 9,
          runSpacing: 9,
          children: [
            SketchAction(
              label: '整理这一天',
              icon: Icons.auto_awesome_outlined,
              filled: true,
              onPressed: canGenerate
                  ? () => unawaited(_generate(controller))
                  : null,
            ),
            SketchAction(
              label: '刷新状态',
              icon: Icons.refresh,
              onPressed: () => unawaited(controller.refresh()),
            ),
          ],
        ),
        if (controller.error != null) ...[
          const SizedBox(height: 14),
          _notice(controller.error!),
        ],
        if (job != null) ...[
          const SizedBox(height: 22),
          _jobCard(controller, job),
        ],
        const SizedBox(height: 26),
        _sectionTitle('当前正文'),
        const SizedBox(height: 10),
        if (controller.current case final current?)
          _versionCard(current, isCurrent: true)
        else
          _notice('还没有当前正文。首次整理完成后才会出现；失败不会显示空白假结果。'),
        const SizedBox(height: 23),
        _sectionTitle('整理候选'),
        const SizedBox(height: 10),
        if (controller.candidates.isEmpty)
          _notice('目前没有待选择的候选。重新整理会保留当前正文并生成候选。'),
        for (final candidate in controller.candidates) ...[
          _versionCard(candidate, isCurrent: false),
          const SizedBox(height: 12),
          Wrap(
            spacing: 9,
            runSpacing: 9,
            children: [
              SketchAction(
                label: '采纳为当前正文',
                onPressed: controller.busy
                    ? null
                    : () => unawaited(controller.adopt(candidate.id)),
              ),
              SketchAction(
                label: '收起候选',
                onPressed: () =>
                    unawaited(controller.dismissCandidate(candidate.id)),
              ),
            ],
          ),
          const SizedBox(height: 20),
        ],
        _notice('收起只隐藏本机列表里的候选，版本仍保存在资料库，可随时找回。'),
        TextButton(
          onPressed: controller.toggleDismissed,
          child: Text(controller.showDismissed ? '隐藏已收起候选' : '查看已收起候选'),
        ),
        if (controller.current != null) ...[
          const SizedBox(height: 12),
          SketchAction(
            label: '再生成一版候选',
            onPressed: canGenerate
                ? () => unawaited(_generate(controller, again: true))
                : null,
          ),
        ],
        if (controller.recentAiJobs.isNotEmpty) ...[
          const SizedBox(height: 25),
          _sectionTitle('最近的整理与转写任务'),
          for (final item in controller.recentAiJobs)
            ListTile(
              contentPadding: EdgeInsets.zero,
              title: Text(item.kind == 'transcribe' ? '录音转写' : '日记整理'),
              subtitle: Text(_jobDescription(item)),
            ),
        ],
      ],
    );
  }

  Widget _jobCard(AiDiaryController controller, Job job) => SketchFrame(
    child: Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        const Text('正在处理的请求', style: TextStyle(fontWeight: FontWeight.w700)),
        const SizedBox(height: 7),
        Text(_jobDescription(job)),
        if (job.progress?.fraction case final progress?) ...[
          const SizedBox(height: 12),
          LinearProgressIndicator(value: progress.clamp(0, 1).toDouble()),
        ],
        const SizedBox(height: 8),
        const Text(
          '材料快照已冻结；任务完成前不会把候选显示成正文。',
          style: TextStyle(color: SketchColors.muted),
        ),
        const SizedBox(height: 12),
        Wrap(
          spacing: 9,
          runSpacing: 9,
          children: [
            if (!job.state.isTerminal && !controller.busy)
              SketchAction(
                label: '取消这次整理',
                onPressed: () => unawaited(controller.cancel()),
              ),
            if (job.state == JobState.failed && !controller.busy)
              SketchAction(
                label: '重试任务',
                onPressed: () async {
                  await controller.retry();
                  unawaited(CoreWakeupScheduler.afterCoreMutation(widget.api));
                },
              ),
          ],
        ),
      ],
    ),
  );

  String _jobDescription(Job job) {
    final phase = switch (job.state) {
      JobState.queued => '已排队，等待处理',
      JobState.running => '正在处理',
      JobState.succeeded => '处理完成',
      JobState.retryWait => '暂时失败，稍后自动重试',
      JobState.waitingConfiguration => '等待模型服务配置',
      JobState.waitingDependency => '等待材料处理完成',
      JobState.waitingNetwork => '等待网络恢复',
      JobState.failed => '处理失败，可检查原因后重试',
      JobState.cancelled => '已取消，原始材料仍在',
    };
    final reason = job.errorCode == null
        ? ''
        : ' · ${AiDiaryController.nextStepFor(job.errorCode!)}';
    return '$phase$reason';
  }

  Widget _versionCard(
    DiaryVersion version, {
    required bool isCurrent,
  }) => SketchFrame(
    child: Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Text(
          isCurrent ? '当前阅读版' : 'AI 整理候选 · 尚未采纳',
          style: const TextStyle(fontWeight: FontWeight.w700),
        ),
        const SizedBox(height: 8),
        Text(
          version.body,
          style: const TextStyle(color: SketchColors.text, height: 1.7),
        ),
        const Divider(color: SketchColors.line, height: 26),
        Text(
          version.generationInfo == null
              ? '此版本没有生成覆盖统计。'
              : '来源 ${version.provenanceLinks.map((link) => link.sourceId).toSet().length} 项 · '
                    '已处理 ${version.generationInfo!.coveredSourceCount} 项 · '
                    '未处理 ${version.generationInfo!.unprocessedSourceCount} 项',
          style: const TextStyle(color: SketchColors.muted, fontSize: 12),
        ),
        if (version.provenanceLinks.isNotEmpty) ...[
          const SizedBox(height: 7),
          Text(
            '来源编号：${version.provenanceLinks.map((link) => link.sourceId).toSet().join('、')}',
            style: const TextStyle(color: SketchColors.muted, fontSize: 12),
          ),
        ],
      ],
    ),
  );

  Widget _sectionTitle(String label) => Text(
    label,
    style: const TextStyle(
      color: SketchColors.ink,
      fontSize: 19,
      fontWeight: FontWeight.w700,
    ),
  );

  Widget _notice(String text) => SketchFrame(
    fill: SketchColors.soft,
    padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 13),
    child: Text(text, style: const TextStyle(color: SketchColors.text)),
  );
}

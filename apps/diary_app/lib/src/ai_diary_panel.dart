import 'dart:async';
import 'dart:io';

import 'package:diary_api/diary_api.dart';
import 'package:flutter/material.dart';
import 'package:path/path.dart' as path;
import 'package:path_provider/path_provider.dart';

import 'ai_diary_controller.dart';
import 'provider_consent.dart';
import 'core_wakeup.dart';
import 'sketch_ui.dart';
import 'ui_strings.dart';

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
  /// 用户已经授权过「把材料发送到这个服务」的 id 集合。
  /// 授权在「模型服务」里做一次，之后不再反复问。
  Set<String> _consented = const {};
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
      // 授权表**不挡启动**：晚一点读到也没关系，读到之前按「未授权」处理。
      // 之前是 await 它，结果多了一次磁盘 I/O 才建控制器，页面白等了一拍。
      final consent = ProviderConsent(
        File(path.join(directory.path, 'provider-consent.json')),
      );
      unawaited(
        consent.load().then((granted) {
          if (mounted) setState(() => _consented = granted);
        }),
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
    await controller.open(_key(picked));
  }

  bool _isRemote(Provider provider) {
    final host =
        Uri.tryParse(provider.endpoint ?? '')?.host.toLowerCase() ?? '';
    return host != 'localhost' && host != '127.0.0.1' && host != '::1';
  }

  String _providerNotice(Provider? provider, List<Provider> eligible) {
    if (provider == null) {
      return '还没有配置模型服务。';
    }
    final remote = eligible.where(_isRemote).toList(growable: false);
    if (remote.isNotEmpty) {
      final destinations = remote
          .map((item) => Uri.tryParse(item.endpoint ?? '')?.host)
          .whereType<String>()
          .where((host) => host.isNotEmpty)
          .toSet();
      return destinations.isEmpty
          ? '有服务没填地址，去「模型服务」看一下。'
          : '材料会发送到：${destinations.join('、')}。';
    }
    final host = Uri.tryParse(provider.endpoint ?? '')?.host;
    final destination = host == null || host.isEmpty ? provider.label : host;
    if (host == null || host.isEmpty) {
      return '这个服务没填地址，去「模型服务」看一下。';
    }
    return '由本机服务处理：$destination。';
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
    // 只看**这次真要用的那个服务**要不要授权：别的服务授权过不算数。
    final remote = provider != null && _isRemote(provider);
    final unknownDestination =
        remote && (Uri.tryParse(provider.endpoint ?? '')?.host.isEmpty ?? true);
    final needsConsent = remote && !_consented.contains(provider.id);
    final canGenerate =
        provider != null &&
        !unknownDestination &&
        !needsConsent &&
        (controller.day?.materialCounts.captures ?? 0) > 0 &&
        !controller.busy &&
        (job == null || job.state.isTerminal);
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
        const Text('把当天材料整理成一页。'),
        const SizedBox(height: 20),
        SketchAction(
          label:
              '日期 ${controller.dayKey.isEmpty ? '今天' : controller.dayKey} · 更换',
          icon: Icons.calendar_today_outlined,
          onPressed: () => unawaited(_chooseDay(controller)),
        ),
        const SizedBox(height: 16),
        _hint(_providerNotice(provider, eligible)),
        if (controller.providerProblem != null) ...[
          const SizedBox(height: 12),
          _notice(controller.providerProblem!),
        ],
        const SizedBox(height: 12),
        if (provider == null) ...[
          const SizedBox(height: 12),
          SketchAction(
            label: '查看模型服务',
            icon: Icons.settings_outlined,
            onPressed: widget.onOpenModel,
          ),
        ] else if (provider.testStatus != ProviderTestStatus.ok) ...[
          const SizedBox(height: 12),
          _notice('服务还没测试通过。'),
        ],
        // 授权只在「模型服务」里做一次；这里不再每次打勾。
        if (needsConsent) ...[
          const SizedBox(height: 10),
          _notice('这个服务还没授权发送材料。'),
          const SizedBox(height: 8),
          SketchAction(
            label: '去模型服务授权',
            icon: Icons.settings_outlined,
            onPressed: widget.onOpenModel,
          ),
        ],
        const SizedBox(height: 18),
        _hint(
          controller.day == null
              ? '这一天还没有记录。'
              : '${controller.day!.materialCounts.captures} 条材料 · '
                    '已处理 ${controller.day!.materialCounts.processed} · '
                    '待处理 ${controller.day!.materialCounts.pending}',
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
          _hint('还没有正文。'),
        const SizedBox(height: 23),
        _sectionTitle('整理候选'),
        const SizedBox(height: 10),
        if (controller.candidates.isEmpty)
          _hint('还没有候选。'),
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

  /// 告警框只留给**真的需要用户处理**的事（出错、要确认）。
  Widget _notice(String text) => SketchFrame(
    fill: SketchColors.soft,
    padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 13),
    child: Text(text, style: const TextStyle(color: SketchColors.text)),
  );

  /// 一行淡字：状态与说明用它，不要用框。框会把「还没有记录」说得像出了事。
  ///
  /// 带 key 是为了让测试能区分「淡字」和「告警框」——两者渲染出来的文字一样，
  /// 只看文字断言不出区别。
  Widget _hint(String text) => Text(
    text,
    key: Key('hint-$text'),
    style: const TextStyle(color: SketchColors.muted, height: 1.5),
  );
}

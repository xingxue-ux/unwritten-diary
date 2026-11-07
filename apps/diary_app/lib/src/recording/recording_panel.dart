import 'dart:async';

import 'package:audioplayers/audioplayers.dart';
import 'package:diary_api/diary_api.dart';
import 'package:flutter/material.dart';

import 'recording_controller.dart';
import '../sketch_ui.dart';
import 'waveform.dart';

/// 录音页：**真的收音、真的保存、真的转写**。
///
/// 之前这一页是布局预览（「真实麦克风采集尚未接入」），按钮只改本地状态。
/// 现在每个按钮都落到核心：申请票据 → 登记片段 → 最终化 → 排转写任务。
/// 界面上不出现「正在录音」除非真的拿到了采集流。
class RecordingPanel extends StatefulWidget {
  const RecordingPanel({
    super.key,
    required this.api,
    required this.controller,
  });

  final DiaryApi api;
  final RecordingController controller;

  @override
  State<RecordingPanel> createState() => _RecordingPanelState();
}

class _RecordingPanelState extends State<RecordingPanel> {
  /// 播放器**按需创建**：没保存过录音、或者用户根本没按播放时，不该去碰平台
  /// 播放器（widget 测试里也没有它）。第一次真的要用时才建。
  AudioPlayer? _player;

  Waveform? _waveform;
  String? _playbackPath;
  String? _leaseId;
  Duration _position = Duration.zero;
  Duration _duration = Duration.zero;
  bool _playing = false;
  String? _playbackError;
  bool _loadingPlayback = false;
  StreamSubscription<Duration>? _positionSub;
  StreamSubscription<Duration>? _durationSub;
  StreamSubscription<void>? _completeSub;

  RecordingController get _controller => widget.controller;

  @override
  void initState() {
    super.initState();
    _controller.addListener(_onControllerChanged);
    unawaited(_controller.loadRecovery());
    if (_controller.result != null) {
      unawaited(_preparePlayback());
    }
  }

  @override
  void dispose() {
    _controller.removeListener(_onControllerChanged);
    _positionSub?.cancel();
    _durationSub?.cancel();
    _completeSub?.cancel();
    final player = _player;
    if (player != null) unawaited(player.dispose());
    unawaited(_releaseLease());
    super.dispose();
  }

  /// 第一次真的要用播放器时才建，并把三个订阅挂上。
  AudioPlayer get _ensurePlayer {
    final existing = _player;
    if (existing != null) return existing;
    final player = AudioPlayer();
    _player = player;
    _positionSub = player.onPositionChanged.listen((value) {
      if (mounted) setState(() => _position = value);
    });
    _durationSub = player.onDurationChanged.listen((value) {
      if (mounted) setState(() => _duration = value);
    });
    _completeSub = player.onPlayerComplete.listen((_) {
      if (mounted) {
        setState(() {
          _playing = false;
          _position = Duration.zero;
        });
      }
    });
    return player;
  }

  void _onControllerChanged() {
    if (!mounted) return;
    setState(() {});
    if (_controller.result != null && _playbackPath == null && !_loadingPlayback) {
      unawaited(_preparePlayback());
    }
  }

  /// 拿到资产的只读租约，读波形。**播放走租约给的路径**，不自己拼 blob 路径。
  Future<void> _preparePlayback() async {
    final result = _controller.result;
    if (result == null || _loadingPlayback) return;
    _loadingPlayback = true;
    try {
      final lease = await widget.api.openAsset(
        assetId: result.assetId,
        usage: AssetUsage.play,
      );
      final waveform = await loadWaveform(lease.handle);
      if (!mounted) {
        await widget.api.releaseAsset(lease.leaseId);
        return;
      }
      setState(() {
        _leaseId = lease.leaseId;
        _playbackPath = lease.handle;
        _waveform = waveform;
        _duration = Duration(
          milliseconds: result.logicalDurationMs ?? waveform?.duration.inMilliseconds ?? 0,
        );
      });
    } on Object catch (failure) {
      if (mounted) {
        setState(() => _playbackError = '音频打不开：$failure');
      }
    } finally {
      _loadingPlayback = false;
    }
  }

  Future<void> _releaseLease() async {
    final leaseId = _leaseId;
    _leaseId = null;
    if (leaseId == null) return;
    try {
      await widget.api.releaseAsset(leaseId);
    } on Object {
      // 租约到期会自己失效；释放失败不影响用户。
    }
  }

  Future<void> _togglePlay() async {
    final path = _playbackPath;
    if (path == null) return;
    try {
      if (_playing) {
        await _ensurePlayer.pause();
        if (mounted) setState(() => _playing = false);
        return;
      }
      await _ensurePlayer.play(DeviceFileSource(path));
      if (mounted) setState(() => _playing = true);
    } on Object catch (failure) {
      if (mounted) setState(() => _playbackError = '播放失败：$failure');
    }
  }

  Future<void> _seekToFraction(double fraction) async {
    final total = _duration.inMilliseconds;
    if (total <= 0) return;
    final target = Duration(milliseconds: (total * fraction).round());
    try {
      await _ensurePlayer.seek(target);
      if (mounted) setState(() => _position = target);
    } on Object {
      // seek 失败不算错误：下一次播放仍然从头开始。
    }
  }

  @override
  Widget build(BuildContext context) => Column(
    crossAxisAlignment: CrossAxisAlignment.stretch,
    children: [
      const Text(
        '录音',
        style: TextStyle(
          fontFamily: SketchFonts.display,
          fontSize: 31,
          color: SketchColors.ink,
        ),
      ),
      const SizedBox(height: 5),
      const Text(
        '一句话或一次长谈，用同一个入口。声音留在本机，转写也在本机。',
        style: TextStyle(color: SketchColors.muted),
      ),
      const SizedBox(height: 18),
      if (_controller.recovery != null) ...[
        _recoveryCard(),
        const SizedBox(height: 14),
      ],
      if (_controller.error != null) ...[
        _notice(_controller.error!, danger: true),
        const SizedBox(height: 14),
      ],
      if (_controller.phase == RecordingPhase.saved && _controller.result != null)
        _savedCard()
      else
        _recorderCard(),
    ],
  );

  Widget _recorderCard() => SketchFrame(
    child: Column(
      children: [
        _levelRing(),
        const SizedBox(height: 16),
        Text(
          switch (_controller.phase) {
            RecordingPhase.preparing => '正在准备麦克风…',
            RecordingPhase.recording => '正在录音',
            RecordingPhase.paused => '已暂停（这一段已经存好）',
            RecordingPhase.finishing => '正在收好这段录音…',
            RecordingPhase.failed => '这次没有录成',
            _ => '还没有开始',
          },
          key: const Key('recording-phase'),
          style: const TextStyle(fontWeight: FontWeight.w700, fontSize: 18),
          textAlign: TextAlign.center,
        ),
        const SizedBox(height: 6),
        Text(
          _formatDuration(_controller.elapsed),
          style: const TextStyle(
            fontFamily: SketchFonts.display,
            fontSize: 34,
            color: SketchColors.ink,
          ),
        ),
        const SizedBox(height: 4),
        Text(
          // 这句话必须跟着状态走：正在收音的时候还写「按下开始之后才会收音」
          // 就是在骗人（用户会以为没在录）。
          switch (_controller.phase) {
            RecordingPhase.recording => '正在收音。按暂停或结束才会把这一段存进资料库。',
            RecordingPhase.paused =>
              '已经存好 ${_controller.segmentCount} 段，共 ${_formatDuration(_controller.elapsed)}。',
            RecordingPhase.finishing => '正在把片段拼成一条音频…',
            RecordingPhase.preparing => '正在申请麦克风…',
            _ => '按下开始之后才会收音。',
          },
          textAlign: TextAlign.center,
          style: const TextStyle(color: SketchColors.muted),
        ),
        const SizedBox(height: 20),
        Wrap(
          alignment: WrapAlignment.center,
          spacing: 9,
          runSpacing: 9,
          children: _recorderActions(),
        ),
      ],
    ),
  );

  List<Widget> _recorderActions() {
    switch (_controller.phase) {
      case RecordingPhase.idle:
      case RecordingPhase.failed:
        return [
          SketchAction(
            key: const Key('recording-start'),
            label: '开始录音',
            icon: Icons.mic_none,
            filled: true,
            onPressed: () => unawaited(_controller.start()),
          ),
        ];
      case RecordingPhase.preparing:
      case RecordingPhase.finishing:
        return [
          const SizedBox(
            width: 22,
            height: 22,
            child: CircularProgressIndicator(strokeWidth: 2),
          ),
        ];
      case RecordingPhase.recording:
        return [
          SketchAction(
            key: const Key('recording-pause'),
            label: '暂停',
            icon: Icons.pause,
            onPressed: () => unawaited(_controller.pause()),
          ),
          SketchAction(
            key: const Key('recording-finish'),
            label: '结束并保存',
            icon: Icons.stop,
            filled: true,
            onPressed: () => unawaited(_controller.finish()),
          ),
          SketchAction(
            label: '丢弃',
            icon: Icons.delete_outline,
            onPressed: () => unawaited(_controller.discard()),
          ),
        ];
      case RecordingPhase.paused:
        return [
          SketchAction(
            key: const Key('recording-resume'),
            label: '继续',
            icon: Icons.play_arrow,
            onPressed: () => unawaited(_controller.resume()),
          ),
          SketchAction(
            key: const Key('recording-finish'),
            label: '结束并保存',
            icon: Icons.stop,
            filled: true,
            onPressed: () => unawaited(_controller.finish()),
          ),
          SketchAction(
            label: '丢弃',
            icon: Icons.delete_outline,
            onPressed: () => unawaited(_controller.discard()),
          ),
        ];
      case RecordingPhase.saved:
        return const [];
    }
  }

  /// 电平环：只有真的收到采集流才会动。
  Widget _levelRing() {
    final active = _controller.phase == RecordingPhase.recording;
    final value = active ? _controller.level.clamp(0.0, 1.0) : 0.0;
    return SizedBox(
      width: 132,
      height: 132,
      child: Stack(
        alignment: Alignment.center,
        children: [
          SizedBox(
            width: 132,
            height: 132,
            child: CircularProgressIndicator(
              value: active ? value : 0,
              strokeWidth: 6,
              backgroundColor: SketchColors.line,
              valueColor: const AlwaysStoppedAnimation(SketchColors.ink),
            ),
          ),
          Icon(
            active ? Icons.graphic_eq : Icons.mic_none,
            size: 46,
            color: SketchColors.ink,
          ),
        ],
      ),
    );
  }

  Widget _savedCard() {
    final result = _controller.result!;
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        SketchFrame(
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
              Row(
                children: [
                  const Icon(Icons.check_circle_outline, color: SketchColors.ink),
                  const SizedBox(width: 8),
                  Expanded(
                    child: Text(
                      '录音已保存：${_formatDuration(Duration(milliseconds: result.logicalDurationMs ?? 0))}，'
                      '${result.segmentCount} 段',
                      style: const TextStyle(fontWeight: FontWeight.w700),
                    ),
                  ),
                ],
              ),
              const SizedBox(height: 14),
              if (_playbackError != null)
                _notice(_playbackError!, danger: true)
              else if (_playbackPath == null)
                const Center(
                  child: SizedBox(
                    width: 22,
                    height: 22,
                    child: CircularProgressIndicator(strokeWidth: 2),
                  ),
                )
              else
                _playerRow(),
              const SizedBox(height: 16),
              _transcriptBlock(),
            ],
          ),
        ),
        const SizedBox(height: 16),
        Wrap(
          spacing: 9,
          runSpacing: 9,
          children: [
            SketchAction(
              label: '再录一段',
              icon: Icons.mic_none,
              filled: true,
              onPressed: () {
                unawaited(_releaseLease());
                setState(() {
                  _waveform = null;
                  _playbackPath = null;
                  _position = Duration.zero;
                  _duration = Duration.zero;
                  _playing = false;
                });
                unawaited(_controller.start());
              },
            ),
          ],
        ),
      ],
    );
  }

  Widget _playerRow() {
    final peaks = _waveform?.peaks ?? const <double>[];
    final total = _duration.inMilliseconds;
    final fraction = total <= 0
        ? 0.0
        : (_position.inMilliseconds / total).clamp(0.0, 1.0);
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        Row(
          children: [
            IconButton(
              key: const Key('recording-play'),
              onPressed: () => unawaited(_togglePlay()),
              icon: Icon(_playing ? Icons.pause_circle : Icons.play_circle),
              iconSize: 40,
              color: SketchColors.ink,
            ),
            Expanded(
              child: GestureDetector(
                onTapDown: (details) {
                  final width = context.size?.width ?? 1;
                  unawaited(_seekToFraction((details.localPosition.dx / width).clamp(0.0, 1.0)));
                },
                child: SizedBox(
                  height: 56,
                  child: CustomPaint(
                    painter: _WaveformPainter(peaks: peaks, progress: fraction),
                    size: Size.infinite,
                  ),
                ),
              ),
            ),
            const SizedBox(width: 8),
            Text(
              '${_formatDuration(_position)} / ${_formatDuration(_duration)}',
              style: const TextStyle(color: SketchColors.muted, fontSize: 12),
            ),
          ],
        ),
        if (peaks.isEmpty)
          const Padding(
            padding: EdgeInsets.only(top: 4),
            child: Text(
              '这段音频的波形读不出来（容器不是 PCM WAV），播放不受影响。',
              style: TextStyle(color: SketchColors.muted, fontSize: 12),
            ),
          ),
      ],
    );
  }

  Widget _transcriptBlock() {
    final phase = _controller.transcriptionPhase;
    final (title, detail) = switch (phase) {
      TranscriptionPhase.running => ('本机转写中…', '转写不阻塞保存：音频已经进库，文字好了会出现在这里和材料里。'),
      TranscriptionPhase.ready => ('转写完成', '文字同时作为这条记录的材料，可以搜索。'),
      TranscriptionPhase.empty => (
        '转写跑完了，但没有识别出文字',
        _controller.transcriptReason ?? '可能是静音、纯音乐，或说的是模型不支持的语言。',
      ),
      TranscriptionPhase.partial => ('转写只完成了一部分', _controller.transcriptReason ?? '剩下的还没有转。'),
      TranscriptionPhase.failed => ('转写没有完成', _controller.transcriptReason ?? '原因见任务中心。'),
      TranscriptionPhase.waitingConfiguration => (
        '本机转写引擎还没配置好',
        _controller.transcriptReason ?? '音频已经保存，可以播放；配置好引擎之后可以在任务中心重试转写。',
      ),
      TranscriptionPhase.idle => ('还没有转写结果', '音频已经保存；转写任务会在本机跑，不需要联网。'),
    };
    final text = _controller.transcript?.trim() ?? '';
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        Row(
          children: [
            Expanded(
              child: Text(title, style: const TextStyle(fontWeight: FontWeight.w700)),
            ),
            TextButton(
              onPressed: () => unawaited(_controller.refreshTranscript()),
              child: const Text('刷新'),
            ),
          ],
        ),
        const SizedBox(height: 4),
        Text(detail, style: const TextStyle(color: SketchColors.muted, fontSize: 12)),
        if (text.isNotEmpty) ...[
          const SizedBox(height: 10),
          Container(
            padding: const EdgeInsets.all(12),
            decoration: BoxDecoration(
              border: Border.all(color: SketchColors.line),
              borderRadius: BorderRadius.circular(8),
            ),
            child: SelectableText(text, style: const TextStyle(height: 1.6)),
          ),
        ],
      ],
    );
  }

  Widget _recoveryCard() {
    final recovery = _controller.recovery!;
    final segments = recovery.closedSegmentIndexes.length;
    return SketchFrame(
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          const Text(
            '上次有一段录音没有收尾',
            style: TextStyle(fontWeight: FontWeight.w700),
          ),
          const SizedBox(height: 6),
          Text(
            segments == 0
                ? '那一段没有封闭的片段，音频救不回来。可以把它丢掉，别让它一直挂着。'
                : '已经封闭 $segments 段可以救回来；末尾缺口约 ${recovery.gapMs} 毫秒，'
                      '那部分在断电/被杀时丢了，补不回来。',
            style: const TextStyle(color: SketchColors.muted, fontSize: 12),
          ),
          if (recovery.notes.isNotEmpty) ...[
            const SizedBox(height: 6),
            for (final note in recovery.notes)
              Text(
                '· $note',
                style: const TextStyle(color: SketchColors.muted, fontSize: 12),
              ),
          ],
          const SizedBox(height: 12),
          Wrap(
            spacing: 9,
            runSpacing: 9,
            children: [
              SketchAction(
                key: const Key('recording-adopt'),
                label: segments == 0 ? '丢掉它' : '收好它',
                icon: segments == 0 ? Icons.delete_outline : Icons.save_outlined,
                filled: true,
                onPressed: () => unawaited(_controller.adoptRecovery()),
              ),
            ],
          ),
        ],
      ),
    );
  }

  Widget _notice(String text, {bool danger = false}) => Container(
    width: double.infinity,
    padding: const EdgeInsets.all(12),
    decoration: BoxDecoration(
      border: Border.all(color: danger ? SketchColors.ink : SketchColors.line),
      borderRadius: BorderRadius.circular(8),
    ),
    child: Text(text, style: const TextStyle(fontSize: 13)),
  );

  String _formatDuration(Duration value) {
    final total = value.inSeconds;
    final minutes = (total ~/ 60).toString().padLeft(2, '0');
    final seconds = (total % 60).toString().padLeft(2, '0');
    return '$minutes:$seconds';
  }
}

class _WaveformPainter extends CustomPainter {
  const _WaveformPainter({required this.peaks, required this.progress});

  final List<double> peaks;
  final double progress;

  @override
  void paint(Canvas canvas, Size size) {
    if (peaks.isEmpty || size.width <= 0) return;
    final barWidth = size.width / peaks.length;
    final played = Paint()..color = SketchColors.ink;
    final rest = Paint()..color = SketchColors.line;
    final middle = size.height / 2;
    for (var index = 0; index < peaks.length; index++) {
      final height = (peaks[index] * size.height).clamp(2.0, size.height);
      final left = index * barWidth;
      final fraction = index / peaks.length;
      canvas.drawRect(
        Rect.fromLTWH(
          left,
          middle - height / 2,
          (barWidth - 1).clamp(1.0, barWidth),
          height,
        ),
        fraction <= progress ? played : rest,
      );
    }
  }

  @override
  bool shouldRepaint(_WaveformPainter old) =>
      old.progress != progress || old.peaks.length != peaks.length;
}

import 'dart:async';
import 'dart:io';

import 'package:audioplayers/audioplayers.dart';
import 'package:diary_api/diary_api.dart';
import 'package:flutter/material.dart';

import 'recording/playable_audio.dart';
import 'sketch_ui.dart';

/// 原件定位：把「这条片段是从哪来的」变成**能打开的东西**。
///
/// 分工写死在这里：
/// - 核心（`extractors::locate`）只回答「原件在不在、能不能开、为什么不能开」；
/// - 这一层负责真的打开它：音频就地播放并跳到时间点，图片就地显示，
///   其他类型交给系统默认程序。
///
/// **不假装**：`available == false` 时把核心给的原因原样显示出来（原件不在文件库、
/// 状态不是 ready、这个来源本来就没有可打开的原件）。没有原件就不画播放器。
class SourceOriginalCard extends StatefulWidget {
  const SourceOriginalCard({
    super.key,
    required this.api,
    required this.sourceId,
    this.sourceKind,
    this.locator,
  });

  final DiaryApi api;
  final String sourceId;

  /// 命中时知道的来源类型；不知道就按「文件」处理（只提供打开，不提供时间点）。
  final SourceKind? sourceKind;

  /// 搜索命中带来的精确位置（音频时间点等）。为空表示「打开原件就行」。
  final SourceLocator? locator;

  @override
  State<SourceOriginalCard> createState() => _SourceOriginalCardState();
}

class _SourceOriginalCardState extends State<SourceOriginalCard> {
  AssetLease? _lease;
  SourceLocation? _location;
  bool _loading = true;
  String? _error;

  AudioPlayer? _player;
  final PlayableAudio _playable = PlayableAudio();
  bool _playing = false;
  Duration _position = Duration.zero;
  Duration _duration = Duration.zero;
  StreamSubscription<Duration>? _positionSub;
  StreamSubscription<Duration>? _durationSub;
  StreamSubscription<void>? _completeSub;

  /// 原件类型：**以核心回答的为准**，调用方传进来的只是提示。
  ///
  /// 从搜索命中进来时调用方知道类型；从记录详情进来时不知道——以前那样就画不出
  /// 播放器，录音的原件只能「用系统程序打开」。现在核心在 `locateSource` 里
  /// 直接回答这份原件是什么。
  SourceKind? get _kind => _location?.sourceKind ?? widget.sourceKind;

  bool get _isAudio => _kind == SourceKind.audio || _kind == SourceKind.video;

  bool get _isImage => _kind == SourceKind.image;

  /// 要定位到的毫秒数。搜索命中会带 `startMs`；否则从头开始。
  int get _startMs => widget.locator?.startMs ?? 0;

  /// 要定位到的页码（PDF 等文档）。核心只在**页码可靠**时才给（PDF 按实际页序），
  /// DOCX 那种页码不可靠的一律留空，所以这里拿到就一定是对的。
  int? get _pageNumber => widget.locator?.pageNumber;

  @override
  void initState() {
    super.initState();
    unawaited(_open());
  }

  @override
  void dispose() {
    _positionSub?.cancel();
    _durationSub?.cancel();
    _completeSub?.cancel();
    final player = _player;
    if (player != null) unawaited(player.dispose());
    final lease = _lease;
    if (lease != null) unawaited(_release(lease.leaseId));
    super.dispose();
  }

  Future<void> _release(String leaseId) async {
    try {
      await widget.api.releaseAsset(leaseId);
    } on Object {
      // 租约到期会自己失效，释放失败不影响用户。
    }
  }

  Future<void> _open() async {
    try {
      // 位置类型要跟来源类型对得上：核心按 locator 的类型判断「这个位置在这份原件上
      // 有没有意义」。不知道类型时用 file，只表示「打开原件」。
      final locator = widget.locator ??
          SourceLocator(
            type: switch (widget.sourceKind) {
              SourceKind.audio => LocatorType.audio,
              SourceKind.video => LocatorType.video,
              SourceKind.image => LocatorType.image,
              SourceKind.text => LocatorType.textRange,
              _ => LocatorType.file,
            },
            sourceRevisionId: '',
          );
      final located = await widget.api.locateSource(
        ref: widget.sourceId,
        locator: locator,
      );
      if (!mounted) return;
      if (!located.available || located.assetId == null) {
        setState(() {
          _location = located;
          _loading = false;
        });
        return;
      }
      final lease = await widget.api.openAsset(
        assetId: located.assetId!,
        usage: AssetUsage.preview,
      );
      if (!mounted) {
        await _release(lease.leaseId);
        return;
      }
      setState(() {
        _location = located;
        _lease = lease;
        _loading = false;
      });
      if (_isAudio) {
        await _preparePlayer(lease.handle);
      }
    } on Object catch (failure) {
      if (mounted) {
        setState(() {
          _error = '$failure';
          _loading = false;
        });
      }
    }
  }

  Future<void> _preparePlayer(String path) async {
    final player = _player ??= AudioPlayer();
    _positionSub ??= player.onPositionChanged.listen((value) {
      if (mounted) setState(() => _position = value);
    });
    _durationSub ??= player.onDurationChanged.listen((value) {
      if (mounted) setState(() => _duration = value);
    });
    _completeSub ??= player.onPlayerComplete.listen((_) {
      if (mounted) {
        setState(() {
          _playing = false;
          _position = Duration.zero;
        });
      }
    });
    // 先把位置摆到命中点：用户点「定位原件」要的就是那一刻，不是从头放。
    try {
      final playable = await _playable.resolve(path);
      await player.setSource(DeviceFileSource(playable));
      if (_startMs > 0) {
        await player.seek(Duration(milliseconds: _startMs));
        if (mounted) {
          setState(() => _position = Duration(milliseconds: _startMs));
        }
      }
    } on Object {
      // 摆位置失败不影响播放：按下去仍然能从头听。
    }
  }

  Future<void> _toggle() async {
    final lease = _lease;
    if (lease == null) return;
    final player = _player ??= AudioPlayer();
    try {
      if (_playing) {
        await player.pause();
        if (mounted) setState(() => _playing = false);
        return;
      }
      final playable = await _playable.resolve(lease.handle);
      final position = _position;
      await player.play(DeviceFileSource(playable));
      if (position > Duration.zero) {
        await player.seek(position);
      }
      if (mounted) setState(() => _playing = true);
    } on Object catch (failure) {
      if (mounted) setState(() => _error = '播放失败：$failure');
    }
  }

  /// 用系统默认程序打开。**这是唯一一条把路径交给外部程序的路径**，只对
  /// 用户点下去的这一个原件生效。
  Future<void> _openExternal(String path) async {
    try {
      if (Platform.isWindows) {
        await Process.start('explorer.exe', [path], mode: ProcessStartMode.detached);
      } else if (Platform.isMacOS) {
        await Process.start('open', [path], mode: ProcessStartMode.detached);
      } else {
        await Process.start('xdg-open', [path], mode: ProcessStartMode.detached);
      }
    } on Object catch (failure) {
      if (mounted) setState(() => _error = '打不开原件：$failure');
    }
  }

  /// 带着页码打开：交给系统的 URL 处理器，浏览器类程序会认 `#page=N`。
  Future<void> _openAtPage(String path) async {
    final page = _pageNumber;
    if (page == null) return;
    final normalized = path.replaceAll('\\', '/');
    final url = 'file:///${normalized.startsWith('/') ? normalized.substring(1) : normalized}#page=$page';
    try {
      await Process.start(
        'cmd.exe',
        ['/c', 'start', '', url],
        mode: ProcessStartMode.detached,
      );
    } on Object catch (failure) {
      if (mounted) setState(() => _error = '打不开第 $page 页：$failure');
    }
  }

  @override
  Widget build(BuildContext context) {
    if (_loading) {
      return const Padding(
        padding: EdgeInsets.symmetric(vertical: 12),
        child: Center(
          child: SizedBox(
            width: 20,
            height: 20,
            child: CircularProgressIndicator(strokeWidth: 2),
          ),
        ),
      );
    }
    final location = _location;
    if (location == null || !location.available || _lease == null) {
      return _notice(
        location?.reason ?? _error ?? '这个来源没有可打开的原件。',
      );
    }
    return SketchFrame(
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          Row(
            children: [
              const Icon(Icons.folder_open, size: 18, color: SketchColors.ink),
              const SizedBox(width: 8),
              const Expanded(
                child: Text(
                  '原件',
                  style: TextStyle(fontWeight: FontWeight.w700),
                ),
              ),
              if (_startMs > 0)
                Text(
                  '定位到 ${_format(Duration(milliseconds: _startMs))}',
                  style: const TextStyle(
                    color: SketchColors.muted,
                    fontSize: 12,
                  ),
                )
              else if (_pageNumber != null)
                Text(
                  '第 $_pageNumber 页',
                  style: const TextStyle(
                    color: SketchColors.muted,
                    fontSize: 12,
                  ),
                ),
            ],
          ),
          const SizedBox(height: 10),
          if (_isAudio) _audioRow() else if (_isImage) _imagePreview(),
          const SizedBox(height: 10),
          Wrap(
            spacing: 8,
            runSpacing: 8,
            children: [
              SketchAction(
                label: '用系统程序打开',
                icon: Icons.open_in_new,
                compact: true,
                onPressed: () => unawaited(_openExternal(_lease!.handle)),
              ),
              // 页码只有在文档类原件上才有意义。用 `#page=N` 打开是浏览器/Edge 的
              // 约定，Acrobat 之类不认——所以这是**另一个**按钮，不是默认行为，
              // 按下去打不开也不会影响「用系统程序打开」。
              if (_pageNumber != null && Platform.isWindows)
                SketchAction(
                  key: const Key('source-open-page'),
                  label: '打开第 $_pageNumber 页',
                  icon: Icons.menu_book_outlined,
                  compact: true,
                  onPressed: () => unawaited(_openAtPage(_lease!.handle)),
                ),
            ],
          ),
          if (_error != null) ...[
            const SizedBox(height: 8),
            Text(
              _error!,
              style: const TextStyle(color: SketchColors.muted, fontSize: 12),
            ),
          ],
        ],
      ),
    );
  }

  Widget _audioRow() => Row(
    children: [
      IconButton(
        key: const Key('source-play'),
        onPressed: () => unawaited(_toggle()),
        icon: Icon(_playing ? Icons.pause_circle : Icons.play_circle),
        iconSize: 36,
        color: SketchColors.ink,
      ),
      Expanded(
        child: LinearProgressIndicator(
          value: _duration.inMilliseconds <= 0
              ? 0
              : (_position.inMilliseconds / _duration.inMilliseconds).clamp(0.0, 1.0),
          minHeight: 4,
          backgroundColor: SketchColors.line,
        ),
      ),
      const SizedBox(width: 8),
      Text(
        '${_format(_position)} / ${_format(_duration)}',
        style: const TextStyle(color: SketchColors.muted, fontSize: 12),
      ),
    ],
  );

  Widget _imagePreview() {
    final lease = _lease!;
    return ConstrainedBox(
      constraints: const BoxConstraints(maxHeight: 260),
      child: Image.file(
        File(lease.handle),
        fit: BoxFit.contain,
        errorBuilder: (context, error, stack) => Text(
          '这张图片打不开：$error',
          style: const TextStyle(color: SketchColors.muted, fontSize: 12),
        ),
      ),
    );
  }

  Widget _notice(String text) => Container(
    width: double.infinity,
    padding: const EdgeInsets.all(12),
    decoration: BoxDecoration(
      border: Border.all(color: SketchColors.line),
      borderRadius: BorderRadius.circular(8),
    ),
    child: Row(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        const Icon(Icons.info_outline, size: 16, color: SketchColors.muted),
        const SizedBox(width: 8),
        Expanded(
          child: Text(
            text,
            style: const TextStyle(color: SketchColors.muted, fontSize: 12),
          ),
        ),
      ],
    ),
  );

  String _format(Duration value) {
    final total = value.inSeconds;
    final minutes = (total ~/ 60).toString().padLeft(2, '0');
    final seconds = (total % 60).toString().padLeft(2, '0');
    return '$minutes:$seconds';
  }
}

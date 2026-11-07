import 'dart:async';
import 'dart:io';
import 'dart:isolate';

import 'package:crypto/crypto.dart';
import 'package:diary_api/diary_api.dart';
import 'package:flutter/foundation.dart';
import 'package:path/path.dart' as p;

import 'recording_engine.dart';

/// 录音界面看到的状态。
///
/// **不是**核心的 `RecordingState`：核心的状态描述「会话在库里是什么样」，
/// 这里描述「用户现在能做什么」。两者刻意分开——核心有 `recoverable` 这种
/// 界面不直接展示的中间态。
enum RecordingPhase {
  /// 还没有开始。
  idle,

  /// 正在向核心申请票据、申请麦克风权限。
  preparing,

  /// 真的在收音（拿到首个缓冲之后才进这里）。
  recording,

  /// 用户按了暂停：当前片段已经封闭并登记，麦克风已释放。
  paused,

  /// 正在收尾：封闭最后一段、拼装逻辑音频、排转写任务。
  finishing,

  /// 音频已经保存进资料库。
  saved,

  /// 出错了，[RecordingController.error] 里有原因。
  failed,
}

/// 转写进度。核心的转写是**异步任务**，最终化只保证「任务排上了」。
enum TranscriptionPhase { idle, running, ready, empty, partial, failed, waitingConfiguration }

/// 一条录音的完整生命周期。
///
/// 分工写死在这里，别的地方不要绕过去：
/// - **平台层**（[RecordingEngine]）只收音、只写文件；
/// - **核心**负责登记片段、校验哈希、拼装逻辑音频、排转写任务；
/// - 这一层只做编排：申请票据 → 起一段 → 停一段 → 登记 → 最终化 → 等转写。
///
/// 片段边界只在**用户按暂停/继续**时产生。这不是偷懒：`record` 在 Windows 上
/// 暂停时原生暂停采集，继续时接着写同一个文件，中间没有空洞；换成「定时切段」
/// 反而每次切换都会丢掉几十到几百毫秒的音频。
class RecordingController extends ChangeNotifier {
  RecordingController({
    required this.api,
    required this.engine,
    required this.captureId,
  });

  final DiaryApi api;
  final RecordingEngine engine;

  /// 这段录音挂到哪条记录上。
  final String captureId;

  RecordingPhase phase = RecordingPhase.idle;
  TranscriptionPhase transcriptionPhase = TranscriptionPhase.idle;

  /// 已录时长（已封闭片段之和 + 当前片段）。
  Duration elapsed = Duration.zero;

  /// 已登记到核心的片段数。
  int segmentCount = 0;

  /// 实时电平 0..1。
  double level = 0;

  /// 用户可读的失败原因。**不吞异常**，也不把异常原文直接当文案用。
  String? error;

  /// 最终化结果：音频资产 id、记录 revision、时长。
  RecordingFinalizeResult? result;

  /// 转写出来的正文（可能为空）。
  String? transcript;

  /// 转写为什么没有正文 / 为什么没跑完，来自核心的 `coverageReason`。
  String? transcriptReason;

  /// 上次没来得及收尾的录音（打开面板时探测）。
  RecordingRecovery? recovery;

  /// 正在播放的资产租约；播放器要用它拿路径。
  AssetLease? lease;

  RecordingTicket? _ticket;
  Stopwatch? _segment;
  Timer? _ticker;
  Timer? _heartbeat;
  StreamSubscription<double>? _levels;
  int _nextSegmentIndex = 0;
  int _persistedMs = 0;
  bool _disposed = false;
  bool _closing = false;

  bool get isActive =>
      phase == RecordingPhase.recording ||
      phase == RecordingPhase.paused ||
      phase == RecordingPhase.preparing;

  void _notify() {
    if (!_disposed) notifyListeners();
  }

  void _setPhase(RecordingPhase next) {
    phase = next;
    _notify();
  }

  // ---------------------------------------------------------------- 录音

  /// 开始一段新录音。权限先问，票据后申请——顺序反了会在核心留下一个
  /// 没有片段的空会话（`prepare` 之后没有 `cancel` 的话它永远收不了尾）。
  Future<void> start() async {
    if (isActive || phase == RecordingPhase.finishing) return;
    error = null;
    result = null;
    transcript = null;
    transcriptReason = null;
    transcriptionPhase = TranscriptionPhase.idle;
    elapsed = Duration.zero;
    segmentCount = 0;
    _nextSegmentIndex = 0;
    _persistedMs = 0;
    _setPhase(RecordingPhase.preparing);

    if (!await engine.ensurePermission()) {
      _fail('没有麦克风权限，或这台机器没有可用的输入设备。录音没有开始。');
      return;
    }
    try {
      _ticket = await api.prepareRecording(
        captureId: captureId,
        formatPreference: 'audio/wav',
        operationId: _operation('rec-prepare'),
      );
      await _beginSegment();
      _startTicker();
      _setPhase(RecordingPhase.recording);
      await _report(RecordingState.recording);
    } on Object catch (failure) {
      await _abandon();
      _fail(_describe(failure, '录音没能开始'));
    }
  }

  /// 暂停：封闭当前片段并登记，然后释放麦克风。
  ///
  /// 用「停一段 + 登记」代替原生暂停：片段进了库，进程被杀也丢不掉已经录好的
  /// 部分；原生暂停只把音频留在内存缓冲里，崩溃就一起没了。
  Future<void> pause() async {
    if (phase != RecordingPhase.recording || _closing) return;
    _closing = true;
    try {
      await _closeSegment();
      _stopTicker();
      _setPhase(RecordingPhase.paused);
      await _report(RecordingState.paused);
    } on Object catch (failure) {
      _fail(_describe(failure, '暂停时没能把这一段存好'));
    } finally {
      _closing = false;
    }
  }

  Future<void> resume() async {
    if (phase != RecordingPhase.paused || _closing) return;
    _closing = true;
    try {
      await _beginSegment();
      _startTicker();
      _setPhase(RecordingPhase.recording);
      await _report(RecordingState.recording);
    } on Object catch (failure) {
      _fail(_describe(failure, '继续录音失败'));
    } finally {
      _closing = false;
    }
  }

  /// 结束并保存：封闭最后一段 → 最终化 → 跑转写。
  Future<void> finish() async {
    if (!isActive || phase == RecordingPhase.finishing || _closing) return;
    _closing = true;
    final ticket = _ticket;
    if (ticket == null) {
      _closing = false;
      return;
    }
    // 先记下「现在是不是正在收音」再改状态：`_setPhase(finishing)` 之后
    // `phase` 已经不是 recording 了，拿它当条件会跳过封闭最后一段，
    // 结果是「录了半天，保存时说没录到声音」。
    final wasRecording = _segment != null;
    _setPhase(RecordingPhase.finishing);
    try {
      if (wasRecording) {
        await _closeSegment();
      }
      _stopTicker();
      if (_nextSegmentIndex == 0) {
        // 一段都没登记上：没有音频可保存。会话要丢掉，否则它会一直以
        // 「上次没来得及收尾」的身份出现在下次启动里。
        await api.discardRecording(recordingId: ticket.recordingId);
        _ticket = null;
        _fail('没有录到声音（片段是空的），这段录音没有保存。');
        return;
      }
      final before = await _sourceIds();
      final finalized = await api.finalizeRecording(
        recordingId: ticket.recordingId,
        lastSegmentIndex: _nextSegmentIndex - 1,
        endReason: 'user',
      );
      result = finalized;
      _ticket = null;
      _setPhase(RecordingPhase.saved);
      await _loadTranscript(before);
    } on Object catch (failure) {
      _fail(_describe(failure, '保存录音失败'));
    } finally {
      _closing = false;
    }
  }

  /// 丢弃：用户按了录音又不要了。
  ///
  /// 核心那边删会话与暂存片段，本地什么都不留。**已经最终化的不能丢**——
  /// 到那时音频已经挂到记录上，界面也不会给这个按钮。
  Future<void> discard() async {
    final ticket = _ticket;
    _stopTimers();
    await _stopLevels();
    try {
      if (ticket != null) {
        if (_segment != null) {
          await engine.stop();
          _segment = null;
        }
        await api.discardRecording(recordingId: ticket.recordingId);
      }
    } on Object catch (failure) {
      _fail(_describe(failure, '丢弃录音失败'));
      return;
    } finally {
      _ticket = null;
    }
    elapsed = Duration.zero;
    segmentCount = 0;
    _nextSegmentIndex = 0;
    _persistedMs = 0;
    _setPhase(RecordingPhase.idle);
  }

  // ---------------------------------------------------------------- 恢复

  /// 探测上次没来得及收尾的录音。
  Future<void> loadRecovery() async {
    try {
      final found = await api.recoverRecording();
      recovery = found.state == RecordingState.idle ? null : found;
      _notify();
    } on Object {
      // 探测失败不该挡住新录音：如实留空，用户仍然可以开始录。
      recovery = null;
      _notify();
    }
  }

  /// 把上次没来得及收尾的录音收好（能救的片段拼成音频）。
  Future<void> adoptRecovery() async {
    final found = recovery;
    if (found == null || found.recordingId == null) return;
    if (found.closedSegmentIndexes.isEmpty) {
      // 一段都没封闭：没有音频可救。丢掉会话，别让它一直报。
      try {
        await api.discardRecording(recordingId: found.recordingId!);
      } on Object {
        // 丢不掉也要把界面上的提示撤掉，不然用户以为还能救。
      }
      recovery = null;
      _notify();
      return;
    }
    try {
      final last = found.closedSegmentIndexes.reduce((a, b) => a > b ? a : b);
      final before = await _sourceIds();
      final finalized = await api.finalizeRecording(
        recordingId: found.recordingId!,
        lastSegmentIndex: last,
        endReason: 'recovered',
      );
      result = finalized;
      recovery = null;
      _setPhase(RecordingPhase.saved);
      await _loadTranscript(before);
    } on Object catch (failure) {
      _fail(_describe(failure, '收好上次的录音失败'));
    }
  }

  // ---------------------------------------------------------------- 转写

  /// 把这条录音的转写任务跑起来，并读回正文。
  ///
  /// [before] 是最终化之前的来源 id 集合：最终化会往记录上追加一个音频来源，
  /// 用差集找它比「取最后一个」可靠——用户可能在录音之后又导入了一个文件。
  Future<void> _loadTranscript(Set<String> before) async {
    transcriptionPhase = TranscriptionPhase.running;
    _notify();
    try {
      await api.runDueJobs(budgetMs: 120000, trigger: 'manual');
    } on Object {
      // 跑不动就往下读状态：任务还在队列里，界面会显示「排队中」。
    }
    await refreshTranscript(before);
  }

  /// 重新读一次转写结果（界面上的「刷新」按钮也走这里）。
  Future<void> refreshTranscript([Set<String>? before]) async {
    final finalized = result;
    if (finalized == null) return;
    try {
      final sourceId = await _findAudioSource(before);
      if (sourceId == null) {
        transcriptionPhase = TranscriptionPhase.idle;
        _notify();
        return;
      }
      final content = await api.extractedContent(sourceId);
      final job = await _findJob(finalized.assetId);
      transcriptReason = content?.coverageReason;
      if (content != null && content.status == ProcessingStatus.ready) {
        final text = content.text?.trim() ?? '';
        transcript = text.isEmpty ? null : text;
        transcriptionPhase = text.isEmpty
            ? TranscriptionPhase.empty
            : TranscriptionPhase.ready;
      } else if (content != null && content.status == ProcessingStatus.partial) {
        transcript = content.text;
        transcriptionPhase = TranscriptionPhase.partial;
      } else if (content != null && content.status == ProcessingStatus.failed) {
        transcriptionPhase = TranscriptionPhase.failed;
      } else if (job?.requiresUserAction ?? false) {
        // 核心说「需要配置」：不是失败，是缺本机转写引擎/模型。
        transcriptionPhase = TranscriptionPhase.waitingConfiguration;
      } else if (job == null) {
        transcriptionPhase = TranscriptionPhase.idle;
      } else {
        transcriptionPhase = TranscriptionPhase.running;
      }
    } on Object catch (failure) {
      transcriptionPhase = TranscriptionPhase.failed;
      transcriptReason = _describe(failure, '读转写结果失败');
    }
    _notify();
  }

  Future<Set<String>> _sourceIds() async {
    try {
      final capture = await api.getCapture(captureId);
      return capture.orderedSourceIds.toSet();
    } on Object {
      return const <String>{};
    }
  }

  Future<String?> _findAudioSource(Set<String>? before) async {
    final capture = await api.getCapture(captureId);
    if (before == null) {
      // 没有基线（例如重开面板）：音频来源就是最后追加的那一个。
      return capture.orderedSourceIds.isEmpty
          ? null
          : capture.orderedSourceIds.last;
    }
    final added = capture.orderedSourceIds
        .where((id) => !before.contains(id))
        .toList();
    if (added.length == 1) return added.first;
    return capture.orderedSourceIds.isEmpty
        ? null
        : capture.orderedSourceIds.last;
  }

  Future<Job?> _findJob(String assetId) async {
    try {
      final page = await api.listJobs(pageSize: 50);
      for (final job in page.jobs) {
        if (job.kind == 'transcribe' && job.targetIds.contains(assetId)) {
          return job;
        }
      }
    } on Object {
      return null;
    }
    return null;
  }

  // ---------------------------------------------------------------- 内部

  Future<void> _beginSegment() async {
    final ticket = _ticket;
    if (ticket == null) {
      throw StateError('没有录音票据');
    }
    final path = p.join(ticket.stagingTicket, 'seg-$_nextSegmentIndex.wav');
    await engine.start(path);
    _segment = Stopwatch()..start();
    _levels = engine.levels.listen(
      (value) {
        level = value;
        _notify();
      },
      onError: (Object _) {
        // 电平只是显示：拿不到不影响录音本身。
      },
    );
    _heartbeat?.cancel();
    _heartbeat = Timer.periodic(const Duration(seconds: 5), (_) {
      // 定期把「已收到多少毫秒」报给核心：崩溃恢复时它就是缺口的依据。
      unawaited(_report(RecordingState.recording).catchError((Object _) {}));
    });
  }

  /// 封闭当前片段：停采集 → 算哈希 → 登记。空片段不登记。
  Future<void> _closeSegment() async {
    final ticket = _ticket;
    final segment = _segment;
    _segment = null;
    _heartbeat?.cancel();
    _heartbeat = null;
    await _stopLevels();
    final path = await engine.stop();
    if (ticket == null || path == null) return;
    final file = File(path);
    if (!await file.exists()) return;
    final size = await file.length();
    // 只有容器头的文件里没有一个采样：登记它只会让最终化拼出一段空音频。
    if (size <= 64) {
      await file.delete().catchError((Object _) => file);
      return;
    }
    final sha = await Isolate.run(() => _sha256OfFile(path));
    final index = _nextSegmentIndex;
    await api.registerSegment(
      recordingId: ticket.recordingId,
      segmentIndex: index,
      segment: SegmentManifest(
        segmentId: 'seg-$index-${DateTime.now().microsecondsSinceEpoch}',
        relativeFileName: p.basename(path),
        durationMs: segment?.elapsedMilliseconds ?? 0,
        sha256: sha,
        byteSize: size,
        closedAt: DateTime.now().toUtc(),
      ),
    );
    _nextSegmentIndex = index + 1;
    segmentCount = _nextSegmentIndex;
    _persistedMs += segment?.elapsedMilliseconds ?? 0;
    elapsed = Duration(milliseconds: _persistedMs);
    _notify();
  }

  Future<void> _report(RecordingState state) async {
    final ticket = _ticket;
    if (ticket == null) return;
    await api.updateRecordingState(
      NativeRecordingStatus(
        recordingId: ticket.recordingId,
        state: state,
        wallClock: DateTime.now().toUtc(),
        elapsedMs: elapsed.inMilliseconds,
        persistedThroughMs: _persistedMs,
      ),
    );
  }

  void _startTicker() {
    _ticker?.cancel();
    _ticker = Timer.periodic(const Duration(milliseconds: 200), (_) {
      final segment = _segment;
      if (segment == null) return;
      elapsed = Duration(milliseconds: _persistedMs + segment.elapsedMilliseconds);
      _notify();
    });
  }

  void _stopTicker() {
    _ticker?.cancel();
    _ticker = null;
  }

  /// 停掉所有计时器：进度条那个 200 ms 的，和每 5 秒上报一次的那个。
  ///
  /// 两个都要停。只停进度条的话，丢弃/失败之后心跳还在跑，会继续往一个已经
  /// 不存在的会话上报状态（而且是每 5 秒一次，直到控制器被回收）。
  void _stopTimers() {
    _stopTicker();
    _heartbeat?.cancel();
    _heartbeat = null;
  }

  Future<void> _stopLevels() async {
    await _levels?.cancel();
    _levels = null;
    level = 0;
  }

  /// 出错时把核心那边的会话收拾掉，别留下一个永远收不了尾的空会话。
  Future<void> _abandon() async {
    final ticket = _ticket;
    _ticket = null;
    _stopTimers();
    _stopLevels();
    if (ticket == null) return;
    try {
      await engine.stop();
    } on Object {
      // 已经没在录了，停不下来不影响后面的清理。
    }
    try {
      await api.discardRecording(recordingId: ticket.recordingId);
    } on Object {
      // 丢不掉就把原因留在界面上；这里不再抛，免得盖住真正的错误。
    }
  }

  void _fail(String message) {
    error = message;
    _stopTimers();
    _setPhase(RecordingPhase.failed);
  }

  String _operation(String prefix) =>
      '$prefix-${DateTime.now().microsecondsSinceEpoch}';

  /// 把异常翻成用户能读的一句话。**不把 `DiaryException.message` 之外的东西
  /// 直接丢给用户**，也不把密钥之类的细节拼进去。
  String _describe(Object failure, String prefix) {
    if (failure is DiaryException) return '$prefix：${failure.message}';
    return '$prefix：$failure';
  }

  @override
  void dispose() {
    _disposed = true;
    _stopTimers();
    _levels?.cancel();
    unawaited(engine.dispose());
    super.dispose();
  }
}

/// 流式计算文件哈希。放顶层是为了能在 isolate 里跑：大文件在 UI isolate 上
/// 算哈希会卡住动画。
Future<String> _sha256OfFile(String path) async {
  final digest = await sha256.bind(File(path).openRead()).first;
  return digest.toString();
}

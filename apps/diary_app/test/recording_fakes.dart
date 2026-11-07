import 'dart:io';
import 'dart:typed_data';

import 'package:diary_api/diary_api.dart';
import 'package:diary_mock/diary_mock.dart';
import 'package:unwritten_diary/src/recording/recording_engine.dart';

/// 假麦克风：**真的写文件**，这样哈希、字节数、空片段判断都按真实路径走。
///
/// 不写文件的话，控制器里「算哈希 → 登记」这段就永远测不到——而那正是最容易
/// 出错的地方（哈希算错核心会报 integrity_failed，用户看到的是「保存失败」）。
class FakeRecordingEngine implements RecordingEngine {
  FakeRecordingEngine({this.permission = true, this.framesPerSegment = 1600});

  bool permission;
  int framesPerSegment;
  final List<String> started = [];
  String? _path;
  bool disposed = false;

  @override
  Future<bool> ensurePermission() async => permission;

  @override
  Future<void> start(String path) async {
    _path = path;
    started.add(path);
  }

  @override
  Future<String?> stop() async {
    final path = _path;
    _path = null;
    if (path == null) return null;
    // 16 kHz 单声道 16 位：framesPerSegment 帧。
    final data = Uint8List(framesPerSegment * 2);
    for (var index = 0; index < framesPerSegment; index++) {
      final value = (index % 100) * 300;
      data[index * 2] = value & 0xff;
      data[index * 2 + 1] = (value >> 8) & 0xff;
    }
    await File(path).writeAsBytes(_wrapWav(data));
    return path;
  }

  @override
  Stream<double> get levels => const Stream<double>.empty();

  @override
  Future<void> dispose() async {
    disposed = true;
  }

  static Uint8List _wrapWav(Uint8List data) {
    final header = BytesBuilder();
    header.add('RIFF'.codeUnits);
    header.add(_u32(36 + data.length));
    header.add('WAVEfmt '.codeUnits);
    header.add(_u32(16));
    header.add(_u16(1));
    header.add(_u16(1));
    header.add(_u32(16000));
    header.add(_u32(32000));
    header.add(_u16(2));
    header.add(_u16(16));
    header.add('data'.codeUnits);
    header.add(_u32(data.length));
    return Uint8List.fromList([...header.toBytes(), ...data]);
  }

  static Uint8List _u32(int value) =>
      Uint8List(4)..buffer.asByteData().setUint32(0, value, Endian.little);

  static Uint8List _u16(int value) =>
      Uint8List(2)..buffer.asByteData().setUint16(0, value, Endian.little);
}

/// 记录调用的假核心：只实现录音控制器真正用到的那几个方法。
class FakeRecordingApi extends MockDiaryApi {
  FakeRecordingApi({this.stagingRoot});

  final String? stagingRoot;

  final List<String> calls = [];
  final List<SegmentManifest> segments = [];
  final List<int> segmentIndexes = [];
  final List<RecordingState> reported = [];
  int prepareCalls = 0;
  int discardCalls = 0;
  int? lastSegmentIndex;
  String? lastEndReason;
  bool finalizeThrows = false;
  String? transcriptText;
  String? transcriptReason;
  ProcessingStatus transcriptStatus = ProcessingStatus.ready;
  Coverage transcriptCoverage = Coverage.complete;
  JobState? jobState;
  final List<String> sourceIds = [];

  @override
  Future<RecordingTicket> prepareRecording({
    required String captureId,
    String? formatPreference,
    required String operationId,
  }) async {
    prepareCalls++;
    calls.add('prepare');
    final root = stagingRoot ?? Directory.systemTemp.createTempSync('staging').path;
    Directory(root).createSync(recursive: true);
    return RecordingTicket(
      recordingId: 'rec-1',
      assetId: 'asset-1',
      stagingTicket: root,
    );
  }

  @override
  Future<SegmentReceipt> registerSegment({
    required String recordingId,
    required int segmentIndex,
    required SegmentManifest segment,
  }) async {
    calls.add('register:$segmentIndex');
    segments.add(segment);
    segmentIndexes.add(segmentIndex);
    return SegmentReceipt(
      segmentId: segment.segmentId,
      segmentIndex: segmentIndex,
      durable: true,
      recordedAt: DateTime.utc(2026, 11, 7),
    );
  }

  @override
  Future<RecordingSession> updateRecordingState(
    NativeRecordingStatus status,
  ) async {
    calls.add('state:${status.state.wire}');
    reported.add(status.state);
    return RecordingSession(
      recordingId: status.recordingId,
      assetId: 'asset-1',
      state: status.state,
    );
  }

  @override
  Future<RecordingFinalizeResult> finalizeRecording({
    required String recordingId,
    required int lastSegmentIndex,
    required String endReason,
  }) async {
    calls.add('finalize');
    if (finalizeThrows) {
      throw const DiaryException(
        code: DiaryErrorCode.integrityFailed,
        message: '片段内容已变化',
      );
    }
    this.lastSegmentIndex = lastSegmentIndex;
    this.lastEndReason = endReason;
    sourceIds.add('src-audio');
    return RecordingFinalizeResult(
      recordingId: recordingId,
      captureId: 'cap-1',
      captureRevision: 2,
      assetId: 'asset-1',
      state: RecordingState.saved,
      segmentCount: lastSegmentIndex + 1,
      transcriptionQueued: true,
      logicalDurationMs: 1000,
    );
  }

  @override
  Future<void> discardRecording({required String recordingId}) async {
    calls.add('discard');
    discardCalls++;
  }

  @override
  Future<RecordingRecovery> recoverRecording({String? recordingId}) async {
    calls.add('recover');
    return const RecordingRecovery(
      recordingId: 'rec-old',
      state: RecordingState.recoverable,
      closedSegmentIndexes: [0, 2],
      gapMs: 500,
      notes: ['缺 1 个片段'],
    );
  }

  /// 面板要拿资产租约才能播放。widget 测试里没有播放器，也不该有：
  /// 让它如实失败，面板就会停在「音频打不开」而不是去碰平台通道。
  @override
  Future<AssetLease> openAsset({
    required String assetId,
    required AssetUsage usage,
  }) async {
    throw const DiaryException(
      code: DiaryErrorCode.assetMissing,
      message: '测试里不打开音频',
    );
  }

  @override
  Future<Capture> getCapture(String id) async => Capture(
    id: id,
    revision: 2,
    state: CaptureState.draft,
    occurredAt: DateTime.utc(2026, 11, 7),
    createdAt: DateTime.utc(2026, 11, 7),
    updatedAt: DateTime.utc(2026, 11, 7),
    timeZone: 'Asia/Shanghai',
    utcOffsetMinutes: 480,
    dayKey: '2026-11-07',
    orderedSourceIds: List.of(sourceIds),
  );

  @override
  Future<DueRunSummary> runDueJobs({
    required int budgetMs,
    required String trigger,
  }) async {
    calls.add('runDue');
    return DueRunSummary(startedAt: DateTime.utc(2026, 11, 7));
  }

  @override
  Future<ExtractedContent?> extractedContent(String sourceId) async {
    calls.add('content');
    if (transcriptText == null) return null;
    return ExtractedContent(
      id: 'content-1',
      sourceId: sourceId,
      sourceRevisionId: 'rev-1',
      extractorId: 'sherpa',
      extractorVersion: '1',
      status: transcriptStatus,
      coverage: transcriptCoverage,
      text: transcriptText,
      coverageReason: transcriptReason,
    );
  }

  @override
  Future<JobPage> listJobs({
    Set<JobState>? states,
    String? cursor,
    int pageSize = 20,
  }) async {
    final state = jobState;
    if (state == null) return const JobPage(jobs: [], nextCursor: null);
    return JobPage(
      jobs: [
        Job(
          id: 'job-1',
          kind: 'transcribe',
          state: state,
          targetIds: const ['asset-1'],
          createdAt: DateTime.utc(2026, 11, 7),
          updatedAt: DateTime.utc(2026, 11, 7),
          requiresUserAction: state == JobState.waitingConfiguration,
        ),
      ],
      nextCursor: null,
    );
  }
}


import 'dart:io';

import 'package:diary_api/diary_api.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:unwritten_diary/src/recording/recording_controller.dart';
import 'package:unwritten_diary/src/recording/recording_engine.dart';

import 'recording_fakes.dart';

RecordingController _controller(FakeRecordingApi api, FakeRecordingEngine engine) =>
    RecordingController(api: api, engine: engine, captureId: 'cap-1');

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();

  test('一次录音：申请票据 → 登记片段 → 最终化 → 跑转写', () async {
    final api = FakeRecordingApi();
    final engine = FakeRecordingEngine();
    final controller = _controller(api, engine);

    await controller.start();
    expect(controller.phase, RecordingPhase.recording);
    expect(api.prepareCalls, 1);
    expect(engine.started, hasLength(1));

    await controller.finish();
    expect(controller.phase, RecordingPhase.saved);
    expect(api.segmentIndexes, [0]);
    expect(api.lastSegmentIndex, 0);
    expect(api.lastEndReason, 'user');
    expect(api.calls, contains('finalize'));
    expect(api.calls, contains('runDue'));
    expect(controller.result?.assetId, 'asset-1');

    // 登记的字节数与哈希来自真实文件，不是编的。
    final segment = api.segments.single;
    final file = File(engine.started.single);
    expect(segment.byteSize, file.lengthSync());
    expect(segment.sha256, hasLength(64));
    expect(segment.relativeFileName, endsWith('.wav'));
    controller.dispose();
  });

  test('暂停会封闭并登记片段，继续会开新的一段', () async {
    final api = FakeRecordingApi();
    final engine = FakeRecordingEngine();
    final controller = _controller(api, engine);

    await controller.start();
    await controller.pause();
    expect(controller.phase, RecordingPhase.paused);
    expect(api.segmentIndexes, [0]);
    expect(api.reported, contains(RecordingState.paused));

    await controller.resume();
    expect(controller.phase, RecordingPhase.recording);
    expect(engine.started, hasLength(2));

    await controller.finish();
    expect(api.segmentIndexes, [0, 1]);
    expect(api.lastSegmentIndex, 1, reason: '最终化要报最后一段的序号');
    expect(controller.segmentCount, 2);
    controller.dispose();
  });

  test('没有麦克风权限时不会向核心申请票据，也不留下空会话', () async {
    final api = FakeRecordingApi();
    final engine = FakeRecordingEngine(permission: false);
    final controller = _controller(api, engine);

    await controller.start();
    expect(controller.phase, RecordingPhase.failed);
    expect(api.prepareCalls, 0, reason: '权限没拿到就不该建会话');
    expect(controller.error, contains('麦克风'));
    controller.dispose();
  });

  test('一段都没录到时丢弃会话，不假装保存成功', () async {
    final api = FakeRecordingApi();
    final engine = FakeRecordingEngine(framesPerSegment: 0); // 只有容器头
    final controller = _controller(api, engine);

    await controller.start();
    await controller.finish();
    expect(controller.phase, RecordingPhase.failed);
    expect(api.segmentIndexes, isEmpty, reason: '空片段不该登记');
    expect(api.discardCalls, 1, reason: '空会话要丢掉，否则会一直报「上次没来得及收尾」');
    expect(api.calls, isNot(contains('finalize')));
    controller.dispose();
  });

  test('丢弃会通知核心删会话，并回到未开始', () async {
    final api = FakeRecordingApi();
    final engine = FakeRecordingEngine();
    final controller = _controller(api, engine);

    await controller.start();
    await controller.discard();
    expect(controller.phase, RecordingPhase.idle);
    expect(api.discardCalls, 1);
    expect(controller.segmentCount, 0);
    controller.dispose();
  });

  test('最终化失败时如实报错，不把状态改成已保存', () async {
    final api = FakeRecordingApi()..finalizeThrows = true;
    final engine = FakeRecordingEngine();
    final controller = _controller(api, engine);

    await controller.start();
    await controller.finish();
    expect(controller.phase, RecordingPhase.failed);
    expect(controller.result, isNull);
    expect(controller.error, contains('片段内容已变化'));
    controller.dispose();
  });

  test('恢复上次没来得及收尾的录音：用最大已封闭序号最终化', () async {
    final api = FakeRecordingApi();
    final engine = FakeRecordingEngine();
    final controller = _controller(api, engine);

    await controller.loadRecovery();
    expect(controller.recovery, isNotNull);
    await controller.adoptRecovery();
    expect(api.lastSegmentIndex, 2, reason: 'closedSegmentIndexes 是 [0, 2]，最后一段是 2');
    expect(api.lastEndReason, 'recovered');
    expect(controller.recovery, isNull);
    expect(controller.phase, RecordingPhase.saved);
    controller.dispose();
  });

  test('转写结果读回来：有正文就是 ready，没有正文说清原因', () async {
    final api = FakeRecordingApi()
      ..transcriptText = '今天路过花店。'
      ..jobState = JobState.succeeded;
    final engine = FakeRecordingEngine();
    final controller = _controller(api, engine);

    await controller.start();
    await controller.finish();
    expect(controller.transcriptionPhase, TranscriptionPhase.ready);
    expect(controller.transcript, '今天路过花店。');
    controller.dispose();
  });

  test('转写跑完但没有文字时，不把空正文当成功', () async {
    final api = FakeRecordingApi()
      ..transcriptText = ''
      ..transcriptCoverage = Coverage.metadataOnly
      ..transcriptReason = '本机引擎跑完了，但没有识别出文字'
      ..jobState = JobState.succeeded;
    final engine = FakeRecordingEngine();
    final controller = _controller(api, engine);

    await controller.start();
    await controller.finish();
    expect(controller.transcriptionPhase, TranscriptionPhase.empty);
    expect(controller.transcript, isNull);
    expect(controller.transcriptReason, contains('没有识别出文字'));
    controller.dispose();
  });

  test('本机引擎没配置好时显示「需要配置」，不是「失败」', () async {
    final api = FakeRecordingApi()..jobState = JobState.waitingConfiguration;
    final engine = FakeRecordingEngine();
    final controller = _controller(api, engine);

    await controller.start();
    await controller.finish();
    expect(controller.transcriptionPhase, TranscriptionPhase.waitingConfiguration);
    controller.dispose();
  });

  test('电平归一化：-60 dB 以下当静音，0 dB 是满刻度', () {
    expect(normalizeDbfs(-60), 0);
    expect(normalizeDbfs(-120), 0);
    expect(normalizeDbfs(0), 1);
    expect(normalizeDbfs(-30), closeTo(0.5, 0.001));
    expect(normalizeDbfs(double.nan), 0);
    expect(normalizeDbfs(double.negativeInfinity), 0);
  });
}

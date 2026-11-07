import 'dart:async';

import 'package:record/record.dart';

/// 麦克风采集的**平台层**。
///
/// 核心（`crates/diary_core/src/recordings.rs`）只负责登记片段、校验哈希、拼装
/// 逻辑音频；真正收音的是这一层。抽成接口有两个原因：
///
/// 1. widget 测试里没有麦克风，也不该有——测试要能跑完整的「准备 → 录 → 停 →
///    登记 → 最终化」流程，不能因为 CI 机器没声卡就跳过；
/// 2. 平台差异（Windows 的 Media Foundation、Android 的 AudioRecord）只应该
///    出现在实现里，控制器不该知道。
abstract class RecordingEngine {
  /// 申请（或确认）麦克风权限。返回 `false` 表示用户拒绝，或这台机器根本没有
  /// 可用的输入设备——两种情况对上层是同一件事：**现在不能录**。
  Future<bool> ensurePermission();

  /// 开始往 [path] 写一个片段。
  ///
  /// [path] 一定在核心给的限域暂存目录里（`RecordingTicket.stagingTicket`）。
  /// 格式固定为 16 kHz 单声道 16 位 PCM WAV：转写链只解码 PCM WAV，换别的
  /// 容器等于让「录下来的音」永远转不出文字。
  Future<void> start(String path);

  /// 停止并封闭当前片段。返回真正落盘的文件路径；没有写出任何东西时返回 `null`。
  Future<String?> stop();

  /// 实时电平（0..1），用于画音量条。平台不支持时是空流，不是错误。
  Stream<double> get levels;

  Future<void> dispose();
}

/// `record` 插件实现。Windows 走 Media Foundation，Android 走 AudioRecord。
class MicrophoneRecordingEngine implements RecordingEngine {
  MicrophoneRecordingEngine({AudioRecorder? recorder})
    : _recorder = recorder ?? AudioRecorder();

  final AudioRecorder _recorder;

  /// 采集参数。**不要**改采样率/声道数去迁就设备：插件会做重采样，
  /// 而转写链按 WAV 头里的格式解码，改了就两边对不上。
  static const RecordConfig _config = RecordConfig(
    encoder: AudioEncoder.wav,
    sampleRate: 16000,
    numChannels: 1,
  );

  @override
  Future<bool> ensurePermission() async {
    try {
      return await _recorder.hasPermission();
    } on Object {
      // 插件在「没有输入设备」时抛异常而不是返回 false。对上层是同一件事。
      return false;
    }
  }

  @override
  Future<void> start(String path) => _recorder.start(_config, path: path);

  @override
  Future<String?> stop() => _recorder.stop();

  @override
  Stream<double> get levels => _recorder
      .onAmplitudeChanged(const Duration(milliseconds: 100))
      .map((amplitude) => normalizeDbfs(amplitude.current));

  @override
  Future<void> dispose() => _recorder.dispose();
}

/// dBFS（负数，0 是满刻度）→ 0..1 的显示用比例。
///
/// 取 -60 dB 作底：再小的声音在界面上和静音没有区别，而把 -60..0 铺满整条
/// 音量条能让「有没有在收音」一眼看出来。`NaN`/`-inf`（插件在流刚起来时会报）
/// 一律当静音，不让它把布局算崩。
double normalizeDbfs(double dbfs) {
  if (!dbfs.isFinite) return 0;
  final ratio = (dbfs + 60) / 60;
  return ratio.clamp(0.0, 1.0);
}

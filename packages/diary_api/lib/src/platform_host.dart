/// PlatformHost：系统能力接口。契约第 5 节。
///
/// Flutter 侧平台层实现它，核心通过桥接注册的宿主接口受控调用。
/// 调用不得经过 Widget；宿主不可用时返回明确错误或等待状态。
/// 测试使用 FakePlatformHost 实现。
library;

import 'dart:async';

import 'models_transport.dart';
import 'values.dart';

/// 平台提供的一个短期媒体句柄。它不是长期资产地址。
class MediaHandle {
  const MediaHandle({
    required this.handleTicket,
    required this.displayName,
    required this.origin,
    this.mimeHint,
    this.byteSize,
  });

  final String handleTicket;
  final String displayName;
  final ImportOrigin origin;
  final String? mimeHint;
  final int? byteSize;

  @override
  bool operator ==(Object other) =>
      other is MediaHandle &&
      other.handleTicket == handleTicket &&
      other.displayName == displayName &&
      other.origin == origin &&
      other.mimeHint == mimeHint &&
      other.byteSize == byteSize;

  @override
  int get hashCode =>
      Object.hash(handleTicket, displayName, origin, mimeHint, byteSize);

  @override
  String toString() => 'MediaHandle($displayName, ${origin.wire})';
}

/// 从视频里准备好的音轨，保留时间映射。不支持的编码要如实返回。
class PreparedAudioAsset {
  const PreparedAudioAsset({
    required this.assetTicket,
    required this.durationMs,
    this.timeMappingNote,
  });

  final String assetTicket;
  final int durationMs;

  /// 时间映射的说明；无法保证精确对应时必须写明。
  final String? timeMappingNote;

  @override
  bool operator ==(Object other) =>
      other is PreparedAudioAsset &&
      other.assetTicket == assetTicket &&
      other.durationMs == durationMs &&
      other.timeMappingNote == timeMappingNote;

  @override
  int get hashCode => Object.hash(assetTicket, durationMs, timeMappingNote);

  @override
  String toString() => 'PreparedAudioAsset($assetTicket, ${durationMs}ms)';
}

/// 播放句柄。
class PlaybackHandle {
  const PlaybackHandle({
    required this.playbackId,
    required this.assetId,
    this.durationMs,
  });

  final String playbackId;
  final String assetId;
  final int? durationMs;

  @override
  bool operator ==(Object other) =>
      other is PlaybackHandle &&
      other.playbackId == playbackId &&
      other.assetId == assetId &&
      other.durationMs == durationMs;

  @override
  int get hashCode => Object.hash(playbackId, assetId, durationMs);

  @override
  String toString() => 'PlaybackHandle($playbackId)';
}

/// 票据用途。
///
/// `PlatformHost.resolveTicket` 必须知道用途：一张票据不该被跨用途复用
/// （拿备份目标票据去读备份包、或拿去当录音暂存目录）。
enum TicketPurpose {
  /// 备份包要写到哪里（契约 `BackupRequest.targetTicket`）。
  backupTarget,

  /// 要检查 / 恢复的既有备份包（`inspectBackup` 与 `restoreBackup` 的 `packageTicket`）。
  backupPackage,

  /// 录音暂存范围（`startRecording(stagingTicket:)`）。
  recordingStaging,

  /// 从视频里导出音轨的目标位置（`prepareVideoAudioTrack(targetTicket:)`）。
  videoAudioTarget,
}

/// 系统能力接口。
abstract class PlatformHost {
  // -------------------------------------------------------------- 票据

  /// 把限域票据解析成本机可访问的**绝对路径**（issue #170 方案 A）。
  ///
  /// 契约里凡是出现「票据」的地方——`startRecording(stagingTicket:)`、
  /// `prepareVideoAudioTrack(targetTicket:)`、`BackupRequest.targetTicket`、
  /// `inspectBackup(packageTicket:)`、`restoreBackup(packageTicket:)`——都由**宿主**
  /// 先解析，核心只认路径。这条边界只写一次，避免每个接入方各自发明约定：
  /// C ABI（PR #153）与 Dart 适配层（PR #163）各踩过一次「把票据当路径」。
  ///
  /// 默认实现是**恒等映射**（票据本身就是路径）：桌面端文件选择器给的就是真实路径，
  /// 既有实现因此不用改。需要限域的平台（鸿蒙沙箱、Android SAF）覆盖它，把票据映射
  /// 到自己的受控目录；票据无效 / 过期 / 不属于本次授权时抛 [ArgumentError]
  /// （对应契约错误码 `invalid_argument`），**不要静默回退成"当成路径"**。
  Future<String> resolveTicket(
    String ticket, {
    required TicketPurpose purpose,
  }) async {
    return ticket;
  }

  // -------------------------------------------------------------- 权限

  Future<PermissionStatus> queryPermission(SystemPermission permission);

  /// 用户拒绝时其他记录方式继续可用，不循环弹权限框。
  Future<PermissionStatus> requestPermission(SystemPermission permission);

  // -------------------------------------------------------------- 录音

  /// 在票据限定的暂存范围内开始录音。
  ///
  /// 首个有效音频缓冲之后才上报 recording 状态，不能由按钮点击直接推定。
  Future<void> startRecording({
    required String recordingId,
    required String stagingTicket,
    String? formatPreference,
  });

  Future<void> pauseRecording(String recordingId);

  Future<void> resumeRecording(String recordingId);

  Future<void> stopRecording({required String recordingId, String? reason});

  /// 录音真实状态，含已录时长、已持久化位置与输入设备变化。
  ///
  /// 原生服务在 UI 销毁后仍可继续或恢复，因此这个流不能依赖页面对象存活。
  Stream<NativeRecordingStatus> recordingStatus(String recordingId);

  // ---------------------------------------------------- 选择、拍摄与导入

  Future<List<MediaHandle>> pickFiles({bool allowMultiple = true});

  Future<MediaHandle?> capturePhoto();

  Future<List<MediaHandle>> pasteContent();

  /// 处理系统分享进入的内容。
  Future<List<MediaHandle>> handleShareIntent();

  /// 处理拖放进入的内容（桌面端）。
  Future<List<MediaHandle>> handleDrop(List<String> paths);

  // -------------------------------------------------------------- 播放

  Future<PlaybackHandle> startPlayback({
    required String assetId,
    required String handle,
    int startMs = 0,
  });

  Future<void> stopPlayback(String playbackId);

  /// 按需把指定视频音轨导出到受控临时资产，保留时间映射。
  Future<PreparedAudioAsset> prepareVideoAudioTrack({
    required String assetId,
    required String targetTicket,
  });

  // -------------------------------------------------------------- 凭据

  /// 存入系统凭据存储，返回核心可引用的 credentialRef，不回传明文。
  Future<String> storeCredential({
    required String label,
    required String secret,
  });

  /// 读取凭据明文。只有发起调用的受控路径可以拿到。
  Future<String?> readCredential(String credentialRef);

  Future<void> deleteCredential(String credentialRef);

  // ---------------------------------------------------------- 后台与生命周期

  /// 向系统登记任务；唤醒后由平台层调用核心处理到期任务。
  ///
  /// 平台可以推迟执行，登记成功不代表会准时运行。
  Future<void> registerBackgroundWakeup({
    required String taskId,
    required DateTime earliest,
    int? budgetMs,
  });

  Future<void> cancelBackgroundWakeup(String taskId);

  /// 前后台、电源与内存压力变化。
  Stream<RuntimeState> lifecycle();

  /// 当前生命周期状态，供启动时立即读取一次。
  Future<RuntimeState> currentRuntimeState();
}
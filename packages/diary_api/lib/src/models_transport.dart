/// 契约第 1.3、4、5 节的传输与结果类型：核心信息、分页、票据与报告。
///
/// 这些类型多数是调用结果，仍保持不可变值语义，便于 Mock 与真实实现做一致性比较。
library;

import 'equality.dart';
import 'models_content.dart';
import 'models_diary.dart';
import 'values.dart';

/// 核心与协议的版本信息。契约第 1.3 节。
class CoreInfo {
  const CoreInfo({
    required this.apiVersion,
    required this.dataSchemaVersion,
    required this.buildVersion,
    required this.libraryId,
    this.capabilities = const {},
  });

  /// 主次版本；新增可选字段是次版本，删除或改变语义是主版本。
  final String apiVersion;
  final int dataSchemaVersion;
  final String buildVersion;
  final String libraryId;
  final Set<String> capabilities;

  @override
  bool operator ==(Object other) =>
      other is CoreInfo &&
      other.apiVersion == apiVersion &&
      other.dataSchemaVersion == dataSchemaVersion &&
      other.buildVersion == buildVersion &&
      other.libraryId == libraryId &&
      setEquals(other.capabilities, capabilities);

  @override
  int get hashCode => Object.hash(
        apiVersion,
        dataSchemaVersion,
        buildVersion,
        libraryId,
        unorderedHash(capabilities),
      );

  @override
  String toString() => 'CoreInfo(api $apiVersion, schema $dataSchemaVersion)';
}

/// 启动恢复摘要，用于如实告知用户上次发生了什么。
class RecoverySummary {
  const RecoverySummary({
    this.recoveredDraftCount = 0,
    this.orphanedImportCount = 0,
    this.unrecoveredRecordingCount = 0,
    this.notes = const [],
  });

  final int recoveredDraftCount;
  final int orphanedImportCount;
  final int unrecoveredRecordingCount;
  final List<String> notes;

  @override
  bool operator ==(Object other) =>
      other is RecoverySummary &&
      other.recoveredDraftCount == recoveredDraftCount &&
      other.orphanedImportCount == orphanedImportCount &&
      other.unrecoveredRecordingCount == unrecoveredRecordingCount &&
      listEquals(other.notes, notes);

  @override
  int get hashCode => Object.hash(
        recoveredDraftCount,
        orphanedImportCount,
        unrecoveredRecordingCount,
        Object.hashAll(notes),
      );

  @override
  String toString() => 'RecoverySummary(drafts: $recoveredDraftCount, '
      'imports: $orphanedImportCount, recordings: $unrecoveredRecordingCount)';
}

/// 轻量启动快照，不返回整库正文或媒体。契约第 4.1 节。
class CoreSnapshot {
  const CoreSnapshot({
    required this.coreInfo,
    required this.recovery,
    this.pendingJobCount = 0,
    this.lastEventSequence = 0,
    this.captureCount = 0,
  });

  final CoreInfo coreInfo;
  final RecoverySummary recovery;
  final int pendingJobCount;
  final int lastEventSequence;
  final int captureCount;

  @override
  bool operator ==(Object other) =>
      other is CoreSnapshot &&
      other.coreInfo == coreInfo &&
      other.recovery == recovery &&
      other.pendingJobCount == pendingJobCount &&
      other.lastEventSequence == lastEventSequence &&
      other.captureCount == captureCount;

  @override
  int get hashCode => Object.hash(
        coreInfo,
        recovery,
        pendingJobCount,
        lastEventSequence,
        captureCount,
      );

  @override
  String toString() => 'CoreSnapshot(pending jobs: $pendingJobCount)';
}

/// 运行状态，供核心调整任务预算。契约第 4.1 节 `core.setRuntimeState`。
class RuntimeState {
  const RuntimeState({
    required this.foreground,
    this.recordingActive = false,
    this.memoryPressure = PressureLevel.normal,
    this.batteryPressure = PressureLevel.normal,
  });

  final bool foreground;
  final bool recordingActive;
  final PressureLevel memoryPressure;
  final PressureLevel batteryPressure;

  @override
  bool operator ==(Object other) =>
      other is RuntimeState &&
      other.foreground == foreground &&
      other.recordingActive == recordingActive &&
      other.memoryPressure == memoryPressure &&
      other.batteryPressure == batteryPressure;

  @override
  int get hashCode =>
      Object.hash(foreground, recordingActive, memoryPressure, batteryPressure);

  @override
  String toString() => 'RuntimeState(foreground: $foreground, '
      'recording: $recordingActive)';
}

/// 分页结果：不透明 cursor，确定性排序包含 ID 作为并列次序。契约第 1.2 节。
class CapturePage {
  const CapturePage({required this.captures, this.nextCursor});

  final List<Capture> captures;
  final String? nextCursor;

  @override
  bool operator ==(Object other) =>
      other is CapturePage &&
      listEquals(other.captures, captures) &&
      other.nextCursor == nextCursor;

  @override
  int get hashCode => Object.hash(Object.hashAll(captures), nextCursor);

  @override
  String toString() => 'CapturePage(${captures.length} 条)';
}

/// 日期分页结果。
class DiaryDayPage {
  const DiaryDayPage({required this.days, this.nextCursor});

  final List<DiaryDay> days;
  final String? nextCursor;

  @override
  bool operator ==(Object other) =>
      other is DiaryDayPage &&
      listEquals(other.days, days) &&
      other.nextCursor == nextCursor;

  @override
  int get hashCode => Object.hash(Object.hashAll(days), nextCursor);

  @override
  String toString() => 'DiaryDayPage(${days.length} 天)';
}

/// 版本分页结果。
class DiaryVersionPage {
  const DiaryVersionPage({required this.versions, this.nextCursor});

  final List<DiaryVersion> versions;
  final String? nextCursor;

  @override
  bool operator ==(Object other) =>
      other is DiaryVersionPage &&
      listEquals(other.versions, versions) &&
      other.nextCursor == nextCursor;

  @override
  int get hashCode => Object.hash(Object.hashAll(versions), nextCursor);

  @override
  String toString() => 'DiaryVersionPage(${versions.length} 个版本)';
}

/// 任务分页结果。
class JobPage {
  const JobPage({required this.jobs, this.nextCursor});

  final List<Job> jobs;
  final String? nextCursor;

  @override
  bool operator ==(Object other) =>
      other is JobPage &&
      listEquals(other.jobs, jobs) &&
      other.nextCursor == nextCursor;

  @override
  int get hashCode => Object.hash(Object.hashAll(jobs), nextCursor);

  @override
  String toString() => 'JobPage(${jobs.length} 个任务)';
}

/// 草稿保存结果。UI 的「已保存」只在 durable 为真时显示。契约第 1.2 节。
class DraftSaveResult {
  const DraftSaveResult({
    required this.revision,
    required this.durable,
    required this.savedAt,
  });

  final int revision;
  final bool durable;
  final DateTime savedAt;

  @override
  bool operator ==(Object other) =>
      other is DraftSaveResult &&
      other.revision == revision &&
      other.durable == durable &&
      other.savedAt == savedAt;

  @override
  int get hashCode => Object.hash(revision, durable, savedAt);

  @override
  String toString() => 'DraftSaveResult(rev $revision, durable: $durable)';
}

/// 提交结果：记录本身与提交时创建的原始文字版本。
class CommitResult {
  const CommitResult({required this.capture, this.originalTextRevision});

  final Capture capture;
  final SourceRevision? originalTextRevision;

  @override
  bool operator ==(Object other) =>
      other is CommitResult &&
      other.capture == capture &&
      other.originalTextRevision == originalTextRevision;

  @override
  int get hashCode => Object.hash(capture, originalTextRevision);

  @override
  String toString() => 'CommitResult(${capture.id})';
}

/// 永久清理的影响预览。契约第 4.1 节 `captures.previewPurge`。
class PurgePreview {
  const PurgePreview({
    required this.captureId,
    required this.previewToken,
    this.assetIds = const [],
    this.derivedContentIds = const [],
    this.diaryVersionsStillReferencing = const [],
  });

  final String captureId;

  /// 一次性令牌，`captures.purge` 必须带上，避免误删。
  final String previewToken;

  final List<String> assetIds;
  final List<String> derivedContentIds;

  /// 正文里可能仍包含当时信息的历史版本，必须单独列出让用户决定。
  final List<String> diaryVersionsStillReferencing;

  @override
  bool operator ==(Object other) =>
      other is PurgePreview &&
      other.captureId == captureId &&
      other.previewToken == previewToken &&
      listEquals(other.assetIds, assetIds) &&
      listEquals(other.derivedContentIds, derivedContentIds) &&
      listEquals(other.diaryVersionsStillReferencing, diaryVersionsStillReferencing);

  @override
  int get hashCode => Object.hash(
        captureId,
        previewToken,
        Object.hashAll(assetIds),
        Object.hashAll(derivedContentIds),
        Object.hashAll(diaryVersionsStillReferencing),
      );

  @override
  String toString() =>
      'PurgePreview($captureId, ${assetIds.length} 个原件受影响)';
}

/// 永久清理结果。
class PurgeResult {
  const PurgeResult({
    required this.captureId,
    this.purgedAssetIds = const [],
    this.purgedDerivedContentIds = const [],
    this.purgedDiaryVersionIds = const [],
  });

  final String captureId;
  final List<String> purgedAssetIds;
  final List<String> purgedDerivedContentIds;
  final List<String> purgedDiaryVersionIds;

  @override
  bool operator ==(Object other) =>
      other is PurgeResult &&
      other.captureId == captureId &&
      listEquals(other.purgedAssetIds, purgedAssetIds) &&
      listEquals(other.purgedDerivedContentIds, purgedDerivedContentIds) &&
      listEquals(other.purgedDiaryVersionIds, purgedDiaryVersionIds);

  @override
  int get hashCode => Object.hash(
        captureId,
        Object.hashAll(purgedAssetIds),
        Object.hashAll(purgedDerivedContentIds),
        Object.hashAll(purgedDiaryVersionIds),
      );

  @override
  String toString() => 'PurgeResult($captureId)';
}

/// 导入票据：限域暂存位置。平台层只能在这个范围内写入。契约第 4.2 节。
class ImportTicket {
  const ImportTicket({
    required this.importId,
    required this.stagingTicket,
    this.maxBytes,
  });

  final String importId;
  final String stagingTicket;

  /// 允许写入的最大字节数；为空表示不额外限制。
  final int? maxBytes;

  @override
  bool operator ==(Object other) =>
      other is ImportTicket &&
      other.importId == importId &&
      other.stagingTicket == stagingTicket &&
      other.maxBytes == maxBytes;

  @override
  int get hashCode => Object.hash(importId, stagingTicket, maxBytes);

  @override
  String toString() => 'ImportTicket($importId)';
}

/// 复制完成信息，用于 `imports.finish`。
class ImportManifest {
  const ImportManifest({
    required this.copiedBytes,
    required this.sha256,
    required this.detectedMime,
    required this.originalName,
  });

  final int copiedBytes;
  final String sha256;
  final String detectedMime;
  final String originalName;

  @override
  bool operator ==(Object other) =>
      other is ImportManifest &&
      other.copiedBytes == copiedBytes &&
      other.sha256 == sha256 &&
      other.detectedMime == detectedMime &&
      other.originalName == originalName;

  @override
  int get hashCode => Object.hash(copiedBytes, sha256, detectedMime, originalName);

  @override
  String toString() => 'ImportManifest($originalName, $copiedBytes bytes)';
}

/// 导入状态。ready 之后才视为完整文件入库。
class ImportStatus {
  const ImportStatus({
    required this.importId,
    required this.state,
    required this.copiedBytes,
    required this.totalBytes,
    this.assetId,
    this.errorCode,
    this.message,
  });

  final String importId;
  final ImportState state;
  final int copiedBytes;
  final int totalBytes;
  final String? assetId;
  final DiaryErrorCode? errorCode;
  final String? message;

  @override
  bool operator ==(Object other) =>
      other is ImportStatus &&
      other.importId == importId &&
      other.state == state &&
      other.copiedBytes == copiedBytes &&
      other.totalBytes == totalBytes &&
      other.assetId == assetId &&
      other.errorCode == errorCode &&
      other.message == message;

  @override
  int get hashCode => Object.hash(
        importId,
        state,
        copiedBytes,
        totalBytes,
        assetId,
        errorCode,
        message,
      );

  @override
  String toString() => 'ImportStatus($importId, ${state.wire})';
}

/// 录音票据。契约第 4.2 节。
class RecordingTicket {
  const RecordingTicket({
    required this.recordingId,
    required this.assetId,
    required this.stagingTicket,
  });

  final String recordingId;
  final String assetId;
  final String stagingTicket;

  @override
  bool operator ==(Object other) =>
      other is RecordingTicket &&
      other.recordingId == recordingId &&
      other.assetId == assetId &&
      other.stagingTicket == stagingTicket;

  @override
  int get hashCode => Object.hash(recordingId, assetId, stagingTicket);

  @override
  String toString() => 'RecordingTicket($recordingId)';
}

/// 一个已封闭片段的描述。封口后就不再改动。
class SegmentManifest {
  const SegmentManifest({
    required this.segmentId,
    required this.relativeFileName,
    required this.durationMs,
    required this.sha256,
    required this.byteSize,
    this.closedAt,
  });

  final String segmentId;

  /// 相对暂存目录的文件名，不是绝对路径。
  final String relativeFileName;

  final int durationMs;
  final String sha256;
  final int byteSize;
  final DateTime? closedAt;

  @override
  bool operator ==(Object other) =>
      other is SegmentManifest &&
      other.segmentId == segmentId &&
      other.relativeFileName == relativeFileName &&
      other.durationMs == durationMs &&
      other.sha256 == sha256 &&
      other.byteSize == byteSize &&
      other.closedAt == closedAt;

  @override
  int get hashCode => Object.hash(
        segmentId,
        relativeFileName,
        durationMs,
        sha256,
        byteSize,
        closedAt,
      );

  @override
  String toString() => 'SegmentManifest($segmentId, ${durationMs}ms)';
}

/// 片段登记回执。重复登记同一片段返回原结果。
class SegmentReceipt {
  const SegmentReceipt({
    required this.segmentId,
    required this.segmentIndex,
    required this.durable,
    required this.recordedAt,
  });

  final String segmentId;
  final int segmentIndex;
  final bool durable;
  final DateTime recordedAt;

  @override
  bool operator ==(Object other) =>
      other is SegmentReceipt &&
      other.segmentId == segmentId &&
      other.segmentIndex == segmentIndex &&
      other.durable == durable &&
      other.recordedAt == recordedAt;

  @override
  int get hashCode => Object.hash(segmentId, segmentIndex, durable, recordedAt);

  @override
  String toString() => 'SegmentReceipt($segmentIndex, durable: $durable)';
}

/// 原生上报的录音状态。后端不虚构麦克风状态。契约第 5 节。
class NativeRecordingStatus {
  const NativeRecordingStatus({
    required this.recordingId,
    required this.state,
    required this.wallClock,
    this.elapsedMs = 0,
    this.persistedThroughMs = 0,
    this.inputDeviceChanged = false,
    this.message,
  });

  final String recordingId;
  final RecordingState state;
  final DateTime wallClock;
  final int elapsedMs;

  /// 已经持久化的位置，用于恢复时判断缺口。
  final int persistedThroughMs;

  final bool inputDeviceChanged;
  final String? message;

  @override
  bool operator ==(Object other) =>
      other is NativeRecordingStatus &&
      other.recordingId == recordingId &&
      other.state == state &&
      other.wallClock == wallClock &&
      other.elapsedMs == elapsedMs &&
      other.persistedThroughMs == persistedThroughMs &&
      other.inputDeviceChanged == inputDeviceChanged &&
      other.message == message;

  @override
  int get hashCode => Object.hash(
        recordingId,
        state,
        wallClock,
        elapsedMs,
        persistedThroughMs,
        inputDeviceChanged,
        message,
      );

  @override
  String toString() =>
      'NativeRecordingStatus($recordingId, ${state.wire}, ${elapsedMs}ms)';
}

/// 录音会话快照。
class RecordingSession {
  const RecordingSession({
    required this.recordingId,
    required this.assetId,
    required this.state,
    this.elapsedMs = 0,
    this.durableThroughMs = 0,
    this.segmentCount = 0,
    this.lastError,
  });

  final String recordingId;
  final String assetId;
  final RecordingState state;
  final int elapsedMs;
  final int durableThroughMs;
  final int segmentCount;
  final String? lastError;

  @override
  bool operator ==(Object other) =>
      other is RecordingSession &&
      other.recordingId == recordingId &&
      other.assetId == assetId &&
      other.state == state &&
      other.elapsedMs == elapsedMs &&
      other.durableThroughMs == durableThroughMs &&
      other.segmentCount == segmentCount &&
      other.lastError == lastError;

  @override
  int get hashCode => Object.hash(
        recordingId,
        assetId,
        state,
        elapsedMs,
        durableThroughMs,
        segmentCount,
        lastError,
      );

  @override
  String toString() =>
      'RecordingSession($recordingId, ${state.wire}, $segmentCount 段)';
}

/// 录音最终化结果。转写不阻塞完成。
class RecordingFinalizeResult {
  const RecordingFinalizeResult({
    required this.recordingId,
    this.captureId = '',
    this.captureRevision = 0,
    required this.assetId,
    required this.state,
    required this.segmentCount,
    this.transcriptionQueued = false,
    this.logicalDurationMs,
  });

  final String recordingId;

  /// 这条录音挂到的记录；提交成正式记录时要用它。
  final String captureId;

  /// finalize 之后记录的权威 revision，`commit` 的 `expectedRevision` 必须用它。
  final int captureRevision;
  final String assetId;
  final RecordingState state;
  final int segmentCount;
  final bool transcriptionQueued;
  final int? logicalDurationMs;

  @override
  bool operator ==(Object other) =>
      other is RecordingFinalizeResult &&
      other.recordingId == recordingId &&
      other.captureId == captureId &&
      other.captureRevision == captureRevision &&
      other.assetId == assetId &&
      other.state == state &&
      other.segmentCount == segmentCount &&
      other.transcriptionQueued == transcriptionQueued &&
      other.logicalDurationMs == logicalDurationMs;

  @override
  int get hashCode => Object.hash(
        recordingId,
        captureId,
        captureRevision,
        assetId,
        state,
        segmentCount,
        transcriptionQueued,
        logicalDurationMs,
      );

  @override
  String toString() => 'RecordingFinalizeResult($recordingId, $segmentCount 段)';
}

/// 录音恢复结果。不假设最后一段完好，明确报告缺口。
class RecordingRecovery {
  const RecordingRecovery({
    this.recordingId,
    required this.state,
    this.closedSegmentIndexes = const [],
    this.gapMs = 0,
    this.notes = const [],
  });

  final String? recordingId;
  final RecordingState state;
  final List<int> closedSegmentIndexes;

  /// 末尾缺口的毫秒数；无法估计时为 0 并在 notes 里说明。
  final int gapMs;
  final List<String> notes;

  @override
  bool operator ==(Object other) =>
      other is RecordingRecovery &&
      other.recordingId == recordingId &&
      other.state == state &&
      listEquals(other.closedSegmentIndexes, closedSegmentIndexes) &&
      other.gapMs == gapMs &&
      listEquals(other.notes, notes);

  @override
  int get hashCode => Object.hash(
        recordingId,
        state,
        Object.hashAll(closedSegmentIndexes),
        gapMs,
        Object.hashAll(notes),
      );

  @override
  String toString() =>
      'RecordingRecovery(${closedSegmentIndexes.length} 段封闭, 缺口 ${gapMs}ms)';
}

/// 只读资产句柄或限域租约。不把任意路径开放给插件。契约第 4.2 节。
class AssetLease {
  const AssetLease({
    required this.leaseId,
    required this.assetId,
    required this.usage,
    required this.handle,
    this.expiresAt,
    this.byteSize,
  });

  final String leaseId;
  final String assetId;
  final AssetUsage usage;

  /// 平台可读取的位置（应用私有目录内的路径或句柄标识）。
  final String handle;

  final DateTime? expiresAt;
  final int? byteSize;

  @override
  bool operator ==(Object other) =>
      other is AssetLease &&
      other.leaseId == leaseId &&
      other.assetId == assetId &&
      other.usage == usage &&
      other.handle == handle &&
      other.expiresAt == expiresAt &&
      other.byteSize == byteSize;

  @override
  int get hashCode =>
      Object.hash(leaseId, assetId, usage, handle, expiresAt, byteSize);

  @override
  String toString() => 'AssetLease($assetId, ${usage.wire})';
}

/// 定位结果：前端能否打开原件，以及不能打开时的真实原因。
class SourceLocation {
  const SourceLocation({
    required this.sourceRef,
    required this.locator,
    required this.available,
    this.assetId,
    this.reason,
  });

  final String sourceRef;
  final SourceLocator locator;
  final bool available;
  final String? assetId;
  final String? reason;

  @override
  bool operator ==(Object other) =>
      other is SourceLocation &&
      other.sourceRef == sourceRef &&
      other.locator == locator &&
      other.available == available &&
      other.assetId == assetId &&
      other.reason == reason;

  @override
  int get hashCode =>
      Object.hash(sourceRef, locator, available, assetId, reason);

  @override
  String toString() => 'SourceLocation($sourceRef, available: $available)';
}

/// 日记生成票据。输入在冻结后处理。
class DiaryGenerationTicket {
  const DiaryGenerationTicket({
    required this.jobId,
    required this.sourceSnapshotId,
  });

  final String jobId;
  final String sourceSnapshotId;

  @override
  bool operator ==(Object other) =>
      other is DiaryGenerationTicket &&
      other.jobId == jobId &&
      other.sourceSnapshotId == sourceSnapshotId;

  @override
  int get hashCode => Object.hash(jobId, sourceSnapshotId);

  @override
  String toString() => 'DiaryGenerationTicket($jobId)';
}

/// 主动回应票据。只有用户主动调用才运行。
class ResponseTicket {
  const ResponseTicket({required this.jobId, this.responseId});

  final String jobId;
  final String? responseId;

  @override
  bool operator ==(Object other) =>
      other is ResponseTicket &&
      other.jobId == jobId &&
      other.responseId == responseId;

  @override
  int get hashCode => Object.hash(jobId, responseId);

  @override
  String toString() => 'ResponseTicket($jobId)';
}

/// 独立保存的 AI 回应，带来源与生成信息，不并入用户原话。
class AiResponse {
  const AiResponse({
    required this.id,
    required this.requestText,
    required this.body,
    required this.createdAt,
    this.captureIds = const [],
    this.providerId,
    this.model,
  });

  final String id;
  final String requestText;
  final String body;
  final DateTime createdAt;
  final List<String> captureIds;
  final String? providerId;
  final String? model;

  @override
  bool operator ==(Object other) =>
      other is AiResponse &&
      other.id == id &&
      other.requestText == requestText &&
      other.body == body &&
      other.createdAt == createdAt &&
      listEquals(other.captureIds, captureIds) &&
      other.providerId == providerId &&
      other.model == model;

  @override
  int get hashCode => Object.hash(
        id,
        requestText,
        body,
        createdAt,
        Object.hashAll(captureIds),
        providerId,
        model,
      );

  @override
  String toString() => 'AiResponse($id)';
}

/// 索引覆盖状态。未解析、失败、不支持都要可见。契约第 4.4 节。
class IndexStatus {
  const IndexStatus({
    required this.coverage,
    this.keywordIndexReady = false,
    this.semanticIndexReady = false,
    this.modelVersion,
    this.chunkerVersion,
    this.pendingCount = 0,
    this.failedCount = 0,
    this.reasons = const [],
  });

  final Coverage coverage;
  final bool keywordIndexReady;
  final bool semanticIndexReady;
  final String? modelVersion;
  final String? chunkerVersion;
  final int pendingCount;
  final int failedCount;

  /// 用户可读的原因说明（例如「没有 OCR 能力」）。
  final List<String> reasons;

  @override
  bool operator ==(Object other) =>
      other is IndexStatus &&
      other.coverage == coverage &&
      other.keywordIndexReady == keywordIndexReady &&
      other.semanticIndexReady == semanticIndexReady &&
      other.modelVersion == modelVersion &&
      other.chunkerVersion == chunkerVersion &&
      other.pendingCount == pendingCount &&
      other.failedCount == failedCount &&
      listEquals(other.reasons, reasons);

  @override
  int get hashCode => Object.hash(
        coverage,
        keywordIndexReady,
        semanticIndexReady,
        modelVersion,
        chunkerVersion,
        pendingCount,
        failedCount,
        Object.hashAll(reasons),
      );

  @override
  String toString() =>
      'IndexStatus(${coverage.wire}, pending: $pendingCount, failed: $failedCount)';
}

/// 任务票据。长任务统一返回 jobId。
class JobTicket {
  const JobTicket({required this.jobId});

  final String jobId;

  @override
  bool operator ==(Object other) => other is JobTicket && other.jobId == jobId;

  @override
  int get hashCode => jobId.hashCode;

  @override
  String toString() => 'JobTicket($jobId)';
}

/// 一轮到期任务执行摘要。契约第 4.4 节 `jobs.runDue`。
///
/// 字段与核心的 `JobRunSummary` **一一对应**（桥接镜像的就是它）。数字都是真实统计：
/// - 没有跑的任务绝不算进 [processed]；被预算/条数截住的到期任务在 [dueBacklog]
///   里如实报出来（`dueBacklog > 0` 就说明这一轮是**提前收手**，不是「没有活了」，
///   所以不再单独给 `budgetExhausted`：那会和这个数字说同一件事）；
/// - [waitingDependency] 是还在等前置（现在只有 `index` 等 `extract`）的任务数，
///   它们没被领取、没消耗重试额度；闸门在每次领取前重评估，所以一条依赖链
///   （导入 → 提取 → 索引）在**一次** `runDueJobs` 里就能走完；
/// - [nextWakeup] 是下一个到期时刻；等前置的不算待办，但有等待态时它必定非空
///   （等在 `queued`/`retry_wait` 的前置上），不会出现「有等待态却报没有待办」；
/// - [dependencyFailed] 是这一轮被闸门判定「前置不可能再满足」而终态失败的任务数
///   （同样没有执行者跑过），`jobStatus().failed` / `recentErrors` 里也看得到；
/// - [backfilled] 只说「补做」，不说「准时完成」。
class DueRunSummary {
  const DueRunSummary({
    required this.startedAt,
    this.trigger = '',
    this.processed = 0,
    this.succeeded = 0,
    this.failed = 0,
    this.retrying = 0,
    this.backfilled = 0,
    this.recovered = 0,
    this.dueBacklog = 0,
    this.waitingDependency = 0,
    this.dependencyFailed = 0,
    this.nextWakeup,
  });

  /// 本次运行开始的时刻。「补做」以它为界。
  final DateTime startedAt;

  /// 唤醒来源，调用方给什么就记什么（startup / foreground / wake / manual …）。
  final String trigger;

  /// 本次领取并执行的任务数。
  final int processed;

  /// 其中执行成功、进入 `succeeded` 的。
  final int succeeded;

  /// 其中进入「已放弃」（`failed`）的。
  final int failed;

  /// 其中失败但还有额度、已排退避重试（`retry_wait`）的。
  final int retrying;

  /// 其中属于「补做」的。
  final int backfilled;

  /// 运行开始时回收的「上次没跑完」（`running`）任务数。
  final int recovered;

  /// 本轮结束后仍然到期、但没跑上的任务数。
  final int dueBacklog;

  /// 本轮结束时还在等前置条件的任务数。
  final int waitingDependency;

  /// 本轮被闸门终态失败（前置不可能再满足）的任务数。
  final int dependencyFailed;

  /// 下一个到期时刻；没有待办时为空。
  final DateTime? nextWakeup;

  @override
  bool operator ==(Object other) =>
      other is DueRunSummary &&
      other.startedAt == startedAt &&
      other.trigger == trigger &&
      other.processed == processed &&
      other.succeeded == succeeded &&
      other.failed == failed &&
      other.retrying == retrying &&
      other.backfilled == backfilled &&
      other.recovered == recovered &&
      other.dueBacklog == dueBacklog &&
      other.waitingDependency == waitingDependency &&
      other.dependencyFailed == dependencyFailed &&
      other.nextWakeup == nextWakeup;

  @override
  int get hashCode => Object.hash(
        startedAt,
        trigger,
        processed,
        succeeded,
        failed,
        retrying,
        backfilled,
        recovered,
        dueBacklog,
        waitingDependency,
        dependencyFailed,
        nextWakeup,
      );

  @override
  String toString() => 'DueRunSummary(trigger: $trigger, processed: $processed, '
      'dueBacklog: $dueBacklog, waitingDependency: $waitingDependency)';
}

/// 能力测试报告。必须说明是否发生了实际模型调用。契约第 4.4 节。
class ProviderTestReport {
  const ProviderTestReport({
    required this.providerId,
    this.results = const {},
    this.actualCallMade = false,
    this.message,
  });

  final String providerId;
  final Map<ProviderCapability, ProviderTestStatus> results;
  final bool actualCallMade;
  final String? message;

  @override
  bool operator ==(Object other) =>
      other is ProviderTestReport &&
      other.providerId == providerId &&
      mapEquals(other.results, results) &&
      other.actualCallMade == actualCallMade &&
      other.message == message;

  @override
  int get hashCode => Object.hash(
        providerId,
        unorderedHash(results.entries.map((e) => e.key)),
        unorderedHash(results.entries.map((e) => e.value)),
        actualCallMade,
        message,
      );

  @override
  String toString() =>
      'ProviderTestReport($providerId, actualCall: $actualCallMade)';
}

/// 可编辑设置。契约第 4.4 节 `settings.get / update`。
class Settings {
  const Settings({
    required this.diaryTimeZone,
    required this.revision,
    required this.updatedAt,
    this.autoOrganizeEnabled = true,
    this.autoOrganizeTime = '22:30',
    this.defaultStyleId,
  });

  /// 首次设置时为设备时区，之后可改；改动只影响新材料的默认归属。
  final String diaryTimeZone;
  final int revision;
  final DateTime updatedAt;

  final bool autoOrganizeEnabled;

  /// 计划触发时间，不是完成承诺。HH:mm。
  final String autoOrganizeTime;
  final String? defaultStyleId;

  Settings copyWith({
    String? diaryTimeZone,
    bool? autoOrganizeEnabled,
    String? autoOrganizeTime,
    String? defaultStyleId,
    int? revision,
    DateTime? updatedAt,
  }) =>
      Settings(
        diaryTimeZone: diaryTimeZone ?? this.diaryTimeZone,
        revision: revision ?? this.revision,
        updatedAt: updatedAt ?? this.updatedAt,
        autoOrganizeEnabled: autoOrganizeEnabled ?? this.autoOrganizeEnabled,
        autoOrganizeTime: autoOrganizeTime ?? this.autoOrganizeTime,
        defaultStyleId: defaultStyleId ?? this.defaultStyleId,
      );

  @override
  bool operator ==(Object other) =>
      other is Settings &&
      other.diaryTimeZone == diaryTimeZone &&
      other.revision == revision &&
      other.updatedAt == updatedAt &&
      other.autoOrganizeEnabled == autoOrganizeEnabled &&
      other.autoOrganizeTime == autoOrganizeTime &&
      other.defaultStyleId == defaultStyleId;

  @override
  int get hashCode => Object.hash(
        diaryTimeZone,
        revision,
        updatedAt,
        autoOrganizeEnabled,
        autoOrganizeTime,
        defaultStyleId,
      );

  @override
  String toString() =>
      'Settings($diaryTimeZone, 整理 $autoOrganizeTime, revision: $revision)';
}

/// 插件包检查报告。检查不执行插件。契约第 4.4 节。
class PluginPackageReport {
  const PluginPackageReport({
    required this.packageId,
    required this.version,
    required this.apiRange,
    required this.compatible,
    this.supportedPlatforms = const [],
    this.requestedPermissions = const {},
    this.entryKind = PluginEntryKind.declarative,
    this.issues = const [],
  });

  final String packageId;
  final String version;
  final String apiRange;
  final bool compatible;
  final List<String> supportedPlatforms;
  final Set<String> requestedPermissions;
  final PluginEntryKind entryKind;

  /// 兼容性与校验问题清单，用户可读。
  final List<String> issues;

  @override
  bool operator ==(Object other) =>
      other is PluginPackageReport &&
      other.packageId == packageId &&
      other.version == version &&
      other.apiRange == apiRange &&
      other.compatible == compatible &&
      listEquals(other.supportedPlatforms, supportedPlatforms) &&
      setEquals(other.requestedPermissions, requestedPermissions) &&
      other.entryKind == entryKind &&
      listEquals(other.issues, issues);

  @override
  int get hashCode => Object.hash(
        packageId,
        version,
        apiRange,
        compatible,
        Object.hashAll(supportedPlatforms),
        unorderedHash(requestedPermissions),
        entryKind,
        Object.hashAll(issues),
      );

  @override
  String toString() =>
      'PluginPackageReport($packageId, compatible: $compatible)';
}

/// 插件调用票据。invoke 受授权与预算约束。
class PluginInvocationTicket {
  const PluginInvocationTicket({required this.jobId});

  final String jobId;

  @override
  bool operator ==(Object other) =>
      other is PluginInvocationTicket && other.jobId == jobId;

  @override
  int get hashCode => jobId.hashCode;

  @override
  String toString() => 'PluginInvocationTicket($jobId)';
}

/// 备份请求。
class BackupRequest {
  const BackupRequest({required this.targetTicket, this.includeAssets = true});

  /// 目标位置的限域票据，由 PlatformHost 提供。
  final String targetTicket;
  final bool includeAssets;

  @override
  bool operator ==(Object other) =>
      other is BackupRequest &&
      other.targetTicket == targetTicket &&
      other.includeAssets == includeAssets;

  @override
  int get hashCode => Object.hash(targetTicket, includeAssets);

  @override
  String toString() => 'BackupRequest($targetTicket)';
}

/// 备份检查结果。恢复前先检验格式、版本、哈希、大小与安全路径。
class BackupInspection {
  const BackupInspection({
    required this.packageTicket,
    required this.schemaVersion,
    required this.apiVersion,
    required this.byteSize,
    required this.createdAt,
    this.assetCount = 0,
    this.issues = const [],
  });

  final String packageTicket;
  final int schemaVersion;
  final String apiVersion;
  final int byteSize;
  final DateTime createdAt;
  final int assetCount;
  final List<String> issues;

  @override
  bool operator ==(Object other) =>
      other is BackupInspection &&
      other.packageTicket == packageTicket &&
      other.schemaVersion == schemaVersion &&
      other.apiVersion == apiVersion &&
      other.byteSize == byteSize &&
      other.createdAt == createdAt &&
      other.assetCount == assetCount &&
      listEquals(other.issues, issues);

  @override
  int get hashCode => Object.hash(
        packageTicket,
        schemaVersion,
        apiVersion,
        byteSize,
        createdAt,
        assetCount,
        Object.hashAll(issues),
      );

  @override
  String toString() => 'BackupInspection($packageTicket, schema $schemaVersion)';
}

/// 恢复选项。默认新建资料库，不覆盖现库。
class RestoreOptions {
  const RestoreOptions({
    this.restoreAsNewLibrary = true,
    this.includeIndexes = false,
  });

  final bool restoreAsNewLibrary;
  final bool includeIndexes;

  @override
  bool operator ==(Object other) =>
      other is RestoreOptions &&
      other.restoreAsNewLibrary == restoreAsNewLibrary &&
      other.includeIndexes == includeIndexes;

  @override
  int get hashCode => Object.hash(restoreAsNewLibrary, includeIndexes);

  @override
  String toString() => 'RestoreOptions(newLibrary: $restoreAsNewLibrary)';
}

/// 导出请求。普通导出与可完整恢复的备份是不同入口。
class ExportRequest {
  const ExportRequest({
    required this.fromDayKey,
    required this.toDayKey,
    this.format = 'markdown',
    this.includeAssets = false,
  });

  final String fromDayKey;
  final String toDayKey;
  final String format;
  final bool includeAssets;

  @override
  bool operator ==(Object other) =>
      other is ExportRequest &&
      other.fromDayKey == fromDayKey &&
      other.toDayKey == toDayKey &&
      other.format == format &&
      other.includeAssets == includeAssets;

  @override
  int get hashCode => Object.hash(fromDayKey, toDayKey, format, includeAssets);

  @override
  String toString() => 'ExportRequest($fromDayKey..$toDayKey, $format)';
}

/// 导出票据。
class ExportTicket {
  const ExportTicket({required this.jobId, this.packageTicket});

  final String jobId;
  final String? packageTicket;

  @override
  bool operator ==(Object other) =>
      other is ExportTicket &&
      other.jobId == jobId &&
      other.packageTicket == packageTicket;

  @override
  int get hashCode => Object.hash(jobId, packageTicket);

  @override
  String toString() => 'ExportTicket($jobId)';
}
/// 契约第 2.5–2.7 节的数据对象：日记、风格、检索、任务、服务与插件。
library;

import 'equality.dart';
import 'models_content.dart';
import 'values.dart';

/// 当天的材料统计。契约第 2.5 节。
class MaterialCounts {
  const MaterialCounts({
    this.captures = 0,
    this.processed = 0,
    this.pending = 0,
    this.failed = 0,
  });

  final int captures;
  final int processed;
  final int pending;
  final int failed;

  @override
  bool operator ==(Object other) =>
      other is MaterialCounts &&
      other.captures == captures &&
      other.processed == processed &&
      other.pending == pending &&
      other.failed == failed;

  @override
  int get hashCode => Object.hash(captures, processed, pending, failed);

  @override
  String toString() =>
      'MaterialCounts(captures: $captures, processed: $processed, '
      'pending: $pending, failed: $failed)';
}

/// 某一天的日记聚合视图。契约第 2.5 节。
class DiaryDay {
  const DiaryDay({
    required this.dayKey,
    required this.timeZone,
    required this.updatedAt,
    this.currentVersionId,
    this.candidateVersionIds = const [],
    this.revision = 0,
    this.materialCounts = const MaterialCounts(),
    this.coverage = Coverage.unavailable,
  });

  final String dayKey;
  final String timeZone;
  final DateTime updatedAt;

  /// 当前版本；没有生成过时为 null。
  final String? currentVersionId;

  /// 候选版本，等待用户选择。
  final List<String> candidateVersionIds;

  /// 这一天的 **revision**：持久、单调递增的状态推进计数。
  ///
  /// 每次成功写入 +1（新增一版算一次，换当前版本也算一次）；调用方把这个值原样
  /// 回传给 [DiaryApi.saveDiaryEdit] / [DiaryApi.setCurrentVersion] /
  /// [DiaryApi.mergeDiary] 的 `expectedRevision`。冲突保护就建立在它上：两个窗口
  /// 读到同一个 revision 时，后一个写会被拒绝，而不是静默覆盖前一个。
  final int revision;

  final MaterialCounts materialCounts;
  final Coverage coverage;

  DiaryDay copyWith({
    String? timeZone,
    DateTime? updatedAt,
    String? currentVersionId,
    List<String>? candidateVersionIds,
    int? revision,
    MaterialCounts? materialCounts,
    Coverage? coverage,
    bool clearCurrentVersion = false,
  }) =>
      DiaryDay(
        dayKey: dayKey,
        timeZone: timeZone ?? this.timeZone,
        updatedAt: updatedAt ?? this.updatedAt,
        currentVersionId:
            clearCurrentVersion ? null : (currentVersionId ?? this.currentVersionId),
        candidateVersionIds: candidateVersionIds ?? this.candidateVersionIds,
        revision: revision ?? this.revision,
        materialCounts: materialCounts ?? this.materialCounts,
        coverage: coverage ?? this.coverage,
      );

  @override
  bool operator ==(Object other) =>
      other is DiaryDay &&
      other.dayKey == dayKey &&
      other.timeZone == timeZone &&
      other.updatedAt == updatedAt &&
      other.currentVersionId == currentVersionId &&
      listEquals(other.candidateVersionIds, candidateVersionIds) &&
      other.revision == revision &&
      other.materialCounts == materialCounts &&
      other.coverage == coverage;

  @override
  int get hashCode => Object.hash(
        dayKey,
        timeZone,
        updatedAt,
        currentVersionId,
        Object.hashAll(candidateVersionIds),
        revision,
        materialCounts,
        coverage,
      );

  @override
  String toString() =>
      'DiaryDay($dayKey, current: $currentVersionId, revision: $revision)';
}

/// 生成过程的审计信息。契约第 2.5、6.2 节。
class GenerationInfo {
  const GenerationInfo({
    required this.promptVersion,
    this.providerId,
    this.model,
    this.generationOptions = const {},
    this.coveredSourceCount = 0,
    this.unprocessedSourceCount = 0,
  });

  final String promptVersion;
  final String? providerId;
  final String? model;
  final Map<String, String> generationOptions;

  /// 记录哪些材料已处理、哪些暂未处理，便于如实显示覆盖范围。
  final int coveredSourceCount;
  final int unprocessedSourceCount;

  @override
  bool operator ==(Object other) =>
      other is GenerationInfo &&
      other.promptVersion == promptVersion &&
      other.providerId == providerId &&
      other.model == model &&
      mapEquals(other.generationOptions, generationOptions) &&
      other.coveredSourceCount == coveredSourceCount &&
      other.unprocessedSourceCount == unprocessedSourceCount;

  @override
  int get hashCode => Object.hash(
        promptVersion,
        providerId,
        model,
        Object.hashAll(generationOptions.entries.map((e) => e.key)),
        Object.hashAll(generationOptions.entries.map((e) => e.value)),
        coveredSourceCount,
        unprocessedSourceCount,
      );

  @override
  String toString() => 'GenerationInfo($promptVersion, $providerId/$model)';
}

/// 正文与来源的关联。涉及事实的内容要能追溯；不要求每句修辞都有引用。
/// 契约第 2.5 节。
class ProvenanceLink {
  const ProvenanceLink({
    required this.sourceId,
    required this.sourceRevisionId,
    required this.locator,
    this.bodyRange,
    this.note,
  });

  final String sourceId;
  final String sourceRevisionId;
  final SourceLocator locator;

  /// 正文中对应的字符区间；整段关联时为空。
  final TextRange? bodyRange;
  final String? note;

  @override
  bool operator ==(Object other) =>
      other is ProvenanceLink &&
      other.sourceId == sourceId &&
      other.sourceRevisionId == sourceRevisionId &&
      other.locator == locator &&
      other.bodyRange == bodyRange &&
      other.note == note;

  @override
  int get hashCode =>
      Object.hash(sourceId, sourceRevisionId, locator, bodyRange, note);

  @override
  String toString() => 'ProvenanceLink($sourceId @ $bodyRange)';
}

/// 日记的一个版本。正文是自然段，来源关联放在独立字段。契约第 2.5 节。
class DiaryVersion {
  const DiaryVersion({
    required this.id,
    required this.dayKey,
    required this.origin,
    required this.body,
    required this.createdAt,
    this.parentVersionId,
    this.sourceSnapshotId,
    this.styleSnapshotId,
    this.generationInfo,
    this.provenanceLinks = const [],
  });

  final String id;
  final String dayKey;
  final DiaryVersionOrigin origin;

  /// 自然段正文。不写分点总结、不带报告腔。
  final String body;

  final DateTime createdAt;
  final String? parentVersionId;

  /// 不可变的输入清单快照。
  final String? sourceSnapshotId;
  final String? styleSnapshotId;
  final GenerationInfo? generationInfo;
  final List<ProvenanceLink> provenanceLinks;

  @override
  bool operator ==(Object other) =>
      other is DiaryVersion &&
      other.id == id &&
      other.dayKey == dayKey &&
      other.origin == origin &&
      other.body == body &&
      other.createdAt == createdAt &&
      other.parentVersionId == parentVersionId &&
      other.sourceSnapshotId == sourceSnapshotId &&
      other.styleSnapshotId == styleSnapshotId &&
      other.generationInfo == generationInfo &&
      listEquals(other.provenanceLinks, provenanceLinks);

  @override
  int get hashCode => Object.hash(
        id,
        dayKey,
        origin,
        body,
        createdAt,
        parentVersionId,
        sourceSnapshotId,
        styleSnapshotId,
        generationInfo,
        Object.hashAll(provenanceLinks),
      );

  @override
  String toString() => 'DiaryVersion($id, ${origin.wire}, day: $dayKey)';
}

/// 写作风格偏好。自动推断出的偏好只能作为建议。契约第 2.5 节。
class StyleProfile {
  const StyleProfile({
    required this.id,
    required this.label,
    required this.revision,
    required this.updatedAt,
    this.lengthPreference,
    this.person,
    this.tone,
    this.emotionalTemperature,
    this.rhetoricLevel,
    this.examples = const [],
    this.bannedExpressions = const [],
  });

  final String id;
  final String label;
  final int revision;
  final DateTime updatedAt;

  /// 以下都是自然语言描述，不强行枚举，避免把风格锁死成有限选项。
  final String? lengthPreference;
  final String? person;
  final String? tone;
  final String? emotionalTemperature;
  final String? rhetoricLevel;

  final List<String> examples;
  final List<String> bannedExpressions;

  @override
  bool operator ==(Object other) =>
      other is StyleProfile &&
      other.id == id &&
      other.label == label &&
      other.revision == revision &&
      other.updatedAt == updatedAt &&
      other.lengthPreference == lengthPreference &&
      other.person == person &&
      other.tone == tone &&
      other.emotionalTemperature == emotionalTemperature &&
      other.rhetoricLevel == rhetoricLevel &&
      listEquals(other.examples, examples) &&
      listEquals(other.bannedExpressions, bannedExpressions);

  @override
  int get hashCode => Object.hash(
        id,
        label,
        revision,
        updatedAt,
        lengthPreference,
        person,
        tone,
        emotionalTemperature,
        rhetoricLevel,
        Object.hashAll(examples),
        Object.hashAll(bannedExpressions),
      );

  @override
  String toString() => 'StyleProfile($id, $label, revision: $revision)';
}

/// 检索过滤条件。默认 hybrid、排除回收站、折叠历史版本。契约第 2.6 节。
class SearchFilters {
  const SearchFilters({
    this.fromDayKey,
    this.toDayKey,
    this.kinds = const {},
    this.sourceScope,
    this.includeOldDiaryVersions = false,
    this.includeTrashed = false,
  });

  final String? fromDayKey;
  final String? toDayKey;
  final Set<SourceKind> kinds;

  /// 限定来源范围；为空表示全部。
  final Set<String>? sourceScope;

  final bool includeOldDiaryVersions;
  final bool includeTrashed;

  @override
  bool operator ==(Object other) =>
      other is SearchFilters &&
      other.fromDayKey == fromDayKey &&
      other.toDayKey == toDayKey &&
      setEquals(other.kinds, kinds) &&
      setEquals(other.sourceScope, sourceScope) &&
      other.includeOldDiaryVersions == includeOldDiaryVersions &&
      other.includeTrashed == includeTrashed;

  @override
  int get hashCode => Object.hash(
        fromDayKey,
        toDayKey,
        unorderedHash(kinds.map((e) => e.wire)),
        sourceScope == null ? null : unorderedHash(sourceScope!),
        includeOldDiaryVersions,
        includeTrashed,
      );

  @override
  String toString() => 'SearchFilters($fromDayKey..$toDayKey)';
}

/// 一次检索请求。契约第 2.6 节。
class SearchRequest {
  const SearchRequest({
    required this.query,
    this.mode = SearchMode.hybrid,
    this.filters = const SearchFilters(),
    this.pageSize = 20,
  });

  /// 用户原样输入，是普通检索文字，不直接当 FTS 表达式执行。
  final String query;
  final SearchMode mode;
  final SearchFilters filters;
  final int pageSize;

  @override
  bool operator ==(Object other) =>
      other is SearchRequest &&
      other.query == query &&
      other.mode == mode &&
      other.filters == filters &&
      other.pageSize == pageSize;

  @override
  int get hashCode => Object.hash(query, mode, filters, pageSize);

  @override
  String toString() => 'SearchRequest($query, ${mode.wire})';
}

/// 一条检索命中。契约第 2.6 节。
class SearchHit {
  const SearchHit({
    required this.hitId,
    required this.groupId,
    required this.sourceKind,
    required this.matchedBy,
    required this.coverage,
    this.sourceId,
    this.revisionId,
    this.dayKey,
    this.title,
    this.snippet,
    this.highlights = const [],
    this.locator,
  });

  final String hitId;

  /// 同一记录或来源的命中归到一组，避免相邻块占满结果。
  final String groupId;

  final SourceKind sourceKind;

  /// 关键词、语义、元数据中的一个或多个。向量相似度不作为事实置信度显示。
  final Set<MatchedBy> matchedBy;
  final Coverage coverage;

  final String? sourceId;
  final String? revisionId;
  final String? dayKey;
  final String? title;
  final String? snippet;
  final List<TextRange> highlights;
  final SourceLocator? locator;

  @override
  bool operator ==(Object other) =>
      other is SearchHit &&
      other.hitId == hitId &&
      other.groupId == groupId &&
      other.sourceKind == sourceKind &&
      setEquals(other.matchedBy, matchedBy) &&
      other.coverage == coverage &&
      other.sourceId == sourceId &&
      other.revisionId == revisionId &&
      other.dayKey == dayKey &&
      other.title == title &&
      other.snippet == snippet &&
      listEquals(other.highlights, highlights) &&
      other.locator == locator;

  @override
  int get hashCode => Object.hash(
        hitId,
        groupId,
        sourceKind,
        unorderedHash(matchedBy.map((e) => e.wire)),
        coverage,
        sourceId,
        revisionId,
        dayKey,
        title,
        snippet,
        Object.hashAll(highlights),
        locator,
      );

  @override
  String toString() => 'SearchHit($hitId, group: $groupId)';
}

/// 检索会话的完整快照。每次事件发当前页的完整快照，前端按 hitId 更新；
/// 不要把两份完整快照当增量拼接。契约第 2.6 节。
class SearchSnapshot {
  const SearchSnapshot({
    required this.sessionId,
    required this.queryRevision,
    required this.phase,
    this.results = const [],
    this.cursor,
    this.indexCoverage = Coverage.unavailable,
    this.warnings = const [],
  });

  final String sessionId;
  final int queryRevision;
  final SearchPhase phase;
  final List<SearchHit> results;
  final String? cursor;
  final Coverage indexCoverage;
  final List<String> warnings;

  @override
  bool operator ==(Object other) =>
      other is SearchSnapshot &&
      other.sessionId == sessionId &&
      other.queryRevision == queryRevision &&
      other.phase == phase &&
      listEquals(other.results, results) &&
      other.cursor == cursor &&
      other.indexCoverage == indexCoverage &&
      listEquals(other.warnings, warnings);

  @override
  int get hashCode => Object.hash(
        sessionId,
        queryRevision,
        phase,
        Object.hashAll(results),
        cursor,
        indexCoverage,
        Object.hashAll(warnings),
      );

  @override
  String toString() =>
      'SearchSnapshot($sessionId rev $queryRevision, ${phase.wire}, '
      '${results.length} hits)';
}

/// 可空的任务进度。没有可靠进度时必须为空，不能虚构百分比。契约第 2.7 节。
class JobProgress {
  const JobProgress({required this.completed, required this.total});

  final int completed;
  final int total;

  double? get fraction => total <= 0 ? null : completed / total;

  @override
  bool operator ==(Object other) =>
      other is JobProgress && other.completed == completed && other.total == total;

  @override
  int get hashCode => Object.hash(completed, total);

  @override
  String toString() => 'JobProgress($completed/$total)';
}

/// 持久任务。契约第 2.7 节。
class Job {
  const Job({
    required this.id,
    required this.kind,
    required this.state,
    required this.createdAt,
    required this.updatedAt,
    this.targetIds = const [],
    this.inputSnapshotHash,
    this.progress,
    this.attemptCount = 0,
    this.nextAttemptAt,
    this.errorCode,
    this.requiresUserAction = false,
    this.attentionKey,
  });

  final String id;
  final String kind;
  final JobState state;

  final List<String> targetIds;

  /// 输入快照哈希，配合 operationId 阻止本地重复版本。
  final String? inputSnapshotHash;

  final JobProgress? progress;
  final DateTime createdAt;
  final DateTime updatedAt;
  final int attemptCount;
  final DateTime? nextAttemptAt;
  final DiaryErrorCode? errorCode;

  /// 是否需要用户处理（如缺配置）。
  final bool requiresUserAction;

  /// 同一个需处理问题的标识，用于跨重启去重通知。
  final String? attentionKey;

  Job copyWith({
    JobState? state,
    JobProgress? progress,
    DateTime? updatedAt,
    int? attemptCount,
    DateTime? nextAttemptAt,
    DiaryErrorCode? errorCode,
    bool? requiresUserAction,
    String? attentionKey,
    bool clearProgress = false,
    bool clearError = false,
  }) =>
      Job(
        id: id,
        kind: kind,
        state: state ?? this.state,
        targetIds: targetIds,
        inputSnapshotHash: inputSnapshotHash,
        progress: clearProgress ? null : (progress ?? this.progress),
        createdAt: createdAt,
        updatedAt: updatedAt ?? this.updatedAt,
        attemptCount: attemptCount ?? this.attemptCount,
        nextAttemptAt: nextAttemptAt ?? this.nextAttemptAt,
        errorCode: clearError ? null : (errorCode ?? this.errorCode),
        requiresUserAction: requiresUserAction ?? this.requiresUserAction,
        attentionKey: attentionKey ?? this.attentionKey,
      );

  @override
  bool operator ==(Object other) =>
      other is Job &&
      other.id == id &&
      other.kind == kind &&
      other.state == state &&
      listEquals(other.targetIds, targetIds) &&
      other.inputSnapshotHash == inputSnapshotHash &&
      other.progress == progress &&
      other.createdAt == createdAt &&
      other.updatedAt == updatedAt &&
      other.attemptCount == attemptCount &&
      other.nextAttemptAt == nextAttemptAt &&
      other.errorCode == errorCode &&
      other.requiresUserAction == requiresUserAction &&
      other.attentionKey == attentionKey;

  @override
  int get hashCode => Object.hash(
        id,
        kind,
        state,
        Object.hashAll(targetIds),
        inputSnapshotHash,
        progress,
        createdAt,
        updatedAt,
        attemptCount,
        nextAttemptAt,
        errorCode,
        requiresUserAction,
        attentionKey,
      );

  @override
  String toString() => 'Job($id, $kind, ${state.wire})';
}

/// 远端服务的连接测试状态。
enum ProviderTestStatus {
  untested('untested'),
  ok('ok'),
  failed('failed');

  const ProviderTestStatus(this.wire);

  final String wire;

  static ProviderTestStatus fromWire(String value) => ProviderTestStatus.values
      .firstWhere((e) => e.wire == value,
          orElse: () => throw UnknownWireValueError(value, 'ProviderTestStatus'));
}

/// 脱敏后的远端服务配置。API 返回凭据存在与否，不返回密钥明文。契约第 2.7 节。
class Provider {
  const Provider({
    required this.id,
    required this.label,
    required this.adapterId,
    required this.capabilities,
    required this.enabled,
    this.endpoint,
    this.model,
    this.credentialRef,
    this.testStatus = ProviderTestStatus.untested,
  });

  final String id;
  final String label;

  /// 适配器标识；业务层不硬编码具体供应商。
  final String adapterId;

  final Set<ProviderCapability> capabilities;
  final bool enabled;

  final String? endpoint;
  final String? model;

  /// 系统凭据里的引用，不是密钥本身。
  final String? credentialRef;

  final ProviderTestStatus testStatus;

  Provider copyWith({
    String? label,
    String? adapterId,
    Set<ProviderCapability>? capabilities,
    bool? enabled,
    String? endpoint,
    String? model,
    String? credentialRef,
    ProviderTestStatus? testStatus,
  }) =>
      Provider(
        id: id,
        label: label ?? this.label,
        adapterId: adapterId ?? this.adapterId,
        capabilities: capabilities ?? this.capabilities,
        enabled: enabled ?? this.enabled,
        endpoint: endpoint ?? this.endpoint,
        model: model ?? this.model,
        credentialRef: credentialRef ?? this.credentialRef,
        testStatus: testStatus ?? this.testStatus,
      );

  @override
  bool operator ==(Object other) =>
      other is Provider &&
      other.id == id &&
      other.label == label &&
      other.adapterId == adapterId &&
      setEquals(other.capabilities, capabilities) &&
      other.enabled == enabled &&
      other.endpoint == endpoint &&
      other.model == model &&
      other.credentialRef == credentialRef &&
      other.testStatus == testStatus;

  @override
  int get hashCode => Object.hash(
        id,
        label,
        adapterId,
        unorderedHash(capabilities.map((e) => e.wire)),
        enabled,
        endpoint,
        model,
        credentialRef,
        testStatus,
      );

  @override
  String toString() => 'Provider($id, $label, ${capabilities.length} 项能力)';
}

/// 插件。模型提供者插件可请求核心代为调用已配置服务。契约第 2.7 节。
class Plugin {
  const Plugin({
    required this.id,
    required this.version,
    required this.apiRange,
    required this.entryKind,
    required this.state,
    this.supportedPlatforms = const [],
    this.activationEvents = const [],
    this.requestedPermissions = const {},
    this.grantedPermissions = const {},
    this.lastError,
  });

  final String id;
  final String version;

  /// 兼容的 apiVersion 范围，如 `>=1.0.0 <2.0.0`。
  final String apiRange;

  final PluginEntryKind entryKind;
  final PluginState state;

  final List<String> supportedPlatforms;
  final List<String> activationEvents;
  final Set<String> requestedPermissions;
  final Set<String> grantedPermissions;
  final String? lastError;

  Plugin copyWith({
    PluginState? state,
    Set<String>? grantedPermissions,
    String? lastError,
    bool clearError = false,
  }) =>
      Plugin(
        id: id,
        version: version,
        apiRange: apiRange,
        entryKind: entryKind,
        state: state ?? this.state,
        supportedPlatforms: supportedPlatforms,
        activationEvents: activationEvents,
        requestedPermissions: requestedPermissions,
        grantedPermissions: grantedPermissions ?? this.grantedPermissions,
        lastError: clearError ? null : (lastError ?? this.lastError),
      );

  @override
  bool operator ==(Object other) =>
      other is Plugin &&
      other.id == id &&
      other.version == version &&
      other.apiRange == apiRange &&
      other.entryKind == entryKind &&
      other.state == state &&
      listEquals(other.supportedPlatforms, supportedPlatforms) &&
      listEquals(other.activationEvents, activationEvents) &&
      setEquals(other.requestedPermissions, requestedPermissions) &&
      setEquals(other.grantedPermissions, grantedPermissions) &&
      other.lastError == lastError;

  @override
  int get hashCode => Object.hash(
        id,
        version,
        apiRange,
        entryKind,
        state,
        Object.hashAll(supportedPlatforms),
        Object.hashAll(activationEvents),
        unorderedHash(requestedPermissions),
        unorderedHash(grantedPermissions),
        lastError,
      );

  @override
  String toString() => 'Plugin($id $version, ${state.wire})';
}
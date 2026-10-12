/// 契约里的枚举与错误码。
///
/// 每个枚举都带一个 `wire` 值：它是桥接层与持久层使用的稳定字符串，
/// 与 `docs/development/01-共享接口契约.md` 里的写法一致。Dart 侧的
/// 名字可以改，`wire` 值不能随便改。
library;

/// 枚举解析失败时抛出的错误。
class UnknownWireValueError extends ArgumentError {
  UnknownWireValueError(String value, String type)
      : super.value(value, 'value', '不是已知的 $type');
}

T _parseWire<T>(List<T> values, String Function(T) wire, String value, String type) {
  for (final candidate in values) {
    if (wire(candidate) == value) {
      return candidate;
    }
  }
  throw UnknownWireValueError(value, type);
}

/// Capture 状态，契约第 3 节。
enum CaptureState {
  draft('draft'),
  committed('committed'),
  trashed('trashed');

  const CaptureState(this.wire);

  final String wire;

  static CaptureState fromWire(String value) =>
      _parseWire(values, (e) => e.wire, value, 'CaptureState');
}

/// 原始内容的类型，契约第 2.2 节。
enum SourceKind {
  text('text'),
  image('image'),
  audio('audio'),
  video('video'),
  file('file'),
  link('link');

  const SourceKind(this.wire);

  final String wire;

  static SourceKind fromWire(String value) =>
      _parseWire(values, (e) => e.wire, value, 'SourceKind');
}

/// 原始文字或来源元数据的作者，契约第 2.2 节。
enum AuthorType {
  user('user'),
  import('import');

  const AuthorType(this.wire);

  final String wire;

  static AuthorType fromWire(String value) =>
      _parseWire(values, (e) => e.wire, value, 'AuthorType');
}

/// 资产在文件库中的状态，契约第 2.3 节。
enum AssetStorageState {
  importing('importing'),
  ready('ready'),
  recoverable('recoverable'),
  missing('missing'),
  trashed('trashed');

  const AssetStorageState(this.wire);

  final String wire;

  static AssetStorageState fromWire(String value) =>
      _parseWire(values, (e) => e.wire, value, 'AssetStorageState');
}

/// 资产的来源方式，契约第 2.3 节。
enum ImportOrigin {
  picker('picker'),
  camera('camera'),
  paste('paste'),
  drop('drop'),
  share('share'),
  recording('recording'),
  restore('restore');

  const ImportOrigin(this.wire);

  final String wire;

  static ImportOrigin fromWire(String value) =>
      _parseWire(values, (e) => e.wire, value, 'ImportOrigin');
}

/// 导入状态，契约第 3 节「导入」行。
enum ImportState {
  prepared('prepared'),
  copying('copying'),
  verifying('verifying'),
  ready('ready'),
  recoverable('recoverable'),
  error('error');

  const ImportState(this.wire);

  final String wire;

  static ImportState fromWire(String value) =>
      _parseWire(values, (e) => e.wire, value, 'ImportState');
}

/// 派生内容的处理状态。取值沿用契约第 3 节「索引」行。
enum ProcessingStatus {
  pending('pending'),
  processing('processing'),
  ready('ready'),
  partial('partial'),
  failed('failed'),
  stale('stale');

  const ProcessingStatus(this.wire);

  final String wire;

  static ProcessingStatus fromWire(String value) =>
      _parseWire(values, (e) => e.wire, value, 'ProcessingStatus');
}

/// 覆盖程度，契约第 2.4 节。用户可见原因放在同名字段里，不靠枚举表达。
enum Coverage {
  complete('complete'),
  partial('partial'),
  metadataOnly('metadata_only'),
  unavailable('unavailable');

  const Coverage(this.wire);

  final String wire;

  static Coverage fromWire(String value) =>
      _parseWire(values, (e) => e.wire, value, 'Coverage');
}

/// 定位类型，契约第 2.4 节。
enum LocatorType {
  textRange('text_range'),
  audio('audio'),
  video('video'),
  document('document'),
  image('image'),
  file('file');

  const LocatorType(this.wire);

  final String wire;

  static LocatorType fromWire(String value) =>
      _parseWire(values, (e) => e.wire, value, 'LocatorType');
}

/// 日记版本的来源，契约第 2.5 节。
enum DiaryVersionOrigin {
  generated('generated'),
  userEdit('user_edit'),
  merge('merge');

  const DiaryVersionOrigin(this.wire);

  final String wire;

  static DiaryVersionOrigin fromWire(String value) =>
      _parseWire(values, (e) => e.wire, value, 'DiaryVersionOrigin');
}

/// 任务状态，契约第 3 节「任务」行。
enum JobState {
  queued('queued'),
  running('running'),
  succeeded('succeeded'),
  retryWait('retry_wait'),
  waitingConfiguration('waiting_configuration'),
  waitingDependency('waiting_dependency'),
  waitingNetwork('waiting_network'),
  failed('failed'),
  cancelled('cancelled');

  const JobState(this.wire);

  final String wire;

  static JobState fromWire(String value) =>
      _parseWire(values, (e) => e.wire, value, 'JobState');

  /// 是否已经结束。
  bool get isTerminal => this == succeeded || this == failed || this == cancelled;
}

/// 录音状态，契约第 3 节「录音」行。
enum RecordingState {
  idle('idle'),
  preparing('preparing'),
  recording('recording'),
  paused('paused'),
  stopping('stopping'),
  saved('saved'),
  interrupted('interrupted'),
  recoverable('recoverable'),
  error('error');

  const RecordingState(this.wire);

  final String wire;

  static RecordingState fromWire(String value) =>
      _parseWire(values, (e) => e.wire, value, 'RecordingState');
}

/// 插件状态，契约第 3 节「插件」行。
enum PluginState {
  installed('installed'),
  disabled('disabled'),
  enabled('enabled'),
  running('running'),
  quarantined('quarantined');

  const PluginState(this.wire);

  final String wire;

  static PluginState fromWire(String value) =>
      _parseWire(values, (e) => e.wire, value, 'PluginState');
}

/// 插件入口类型，契约第 2.7 节。
enum PluginEntryKind {
  declarative('declarative'),
  code('code');

  const PluginEntryKind(this.wire);

  final String wire;

  static PluginEntryKind fromWire(String value) =>
      _parseWire(values, (e) => e.wire, value, 'PluginEntryKind');
}

/// 检索模式，契约第 2.6 节。
enum SearchMode {
  keyword('keyword'),
  semantic('semantic'),
  hybrid('hybrid');

  const SearchMode(this.wire);

  final String wire;

  static SearchMode fromWire(String value) =>
      _parseWire(values, (e) => e.wire, value, 'SearchMode');
}

/// 搜索会话阶段，契约第 2.6 节。
enum SearchPhase {
  initial('initial'),
  keywordReady('keyword_ready'),
  hybridReady('hybrid_ready'),
  done('done'),
  cancelled('cancelled');

  const SearchPhase(this.wire);

  final String wire;

  static SearchPhase fromWire(String value) =>
      _parseWire(values, (e) => e.wire, value, 'SearchPhase');
}

/// 命中来源，契约第 2.6 节。可以同时有多个。
enum MatchedBy {
  keyword('keyword'),
  semantic('semantic'),
  metadata('metadata');

  const MatchedBy(this.wire);

  final String wire;

  static MatchedBy fromWire(String value) =>
      _parseWire(values, (e) => e.wire, value, 'MatchedBy');
}

/// 远端服务能力，契约第 2.7 节。
enum ProviderCapability {
  generateText('generate_text'),
  transcribeAudio('transcribe_audio'),
  understandImage('understand_image');

  const ProviderCapability(this.wire);

  final String wire;

  static ProviderCapability fromWire(String value) =>
      _parseWire(values, (e) => e.wire, value, 'ProviderCapability');
}

/// 事件类型，契约第 6 节。
enum DomainEventType {
  captureChanged('capture.changed'),
  assetChanged('asset.changed'),
  diaryVersionCreated('diary.versionCreated'),
  diaryCurrentChanged('diary.currentChanged'),
  jobChanged('job.changed'),
  indexCoverageChanged('index.coverageChanged'),
  pluginChanged('plugin.changed');

  const DomainEventType(this.wire);

  final String wire;

  static DomainEventType fromWire(String value) =>
      _parseWire(values, (e) => e.wire, value, 'DomainEventType');
}

/// 资产打开用途，契约第 4.2 节 `assets.open`。
enum AssetUsage {
  preview('preview'),
  play('play'),
  export('export');

  const AssetUsage(this.wire);

  final String wire;

  static AssetUsage fromWire(String value) =>
      _parseWire(values, (e) => e.wire, value, 'AssetUsage');
}

/// 系统权限种类，契约第 5 节。具体平台是否支持由 PlatformHost 如实返回。
enum SystemPermission {
  microphone('microphone'),
  camera('camera'),
  photos('photos'),
  notifications('notifications'),
  backgroundExecution('background_execution');

  const SystemPermission(this.wire);

  final String wire;

  static SystemPermission fromWire(String value) =>
      _parseWire(values, (e) => e.wire, value, 'SystemPermission');
}

/// 权限状态，契约第 5 节。
enum PermissionStatus {
  granted('granted'),
  denied('denied'),
  restricted('restricted');

  const PermissionStatus(this.wire);

  final String wire;

  static PermissionStatus fromWire(String value) =>
      _parseWire(values, (e) => e.wire, value, 'PermissionStatus');
}

/// 压力等级，用于 `core.setRuntimeState` 的资源建议。
enum PressureLevel {
  normal('normal'),
  moderate('moderate'),
  critical('critical');

  const PressureLevel(this.wire);

  final String wire;

  static PressureLevel fromWire(String value) =>
      _parseWire(values, (e) => e.wire, value, 'PressureLevel');
}

/// 生成日记的原因，用于 `diary.generate` 的审计与幂等。
enum DiaryGenerationReason {
  manual('manual'),
  scheduled('scheduled'),
  automatic('automatic'),
  regenerate('regenerate');

  const DiaryGenerationReason(this.wire);

  final String wire;

  static DiaryGenerationReason fromWire(String value) =>
      _parseWire(values, (e) => e.wire, value, 'DiaryGenerationReason');
}

/// 错误码，契约第 7 节。用户可见行为与自动行为见该节表格。
enum DiaryErrorCode {
  storageFull('storage_full'),
  permissionDenied('permission_denied'),
  revisionConflict('revision_conflict'),
  idempotencyConflict('idempotency_conflict'),
  notFound('not_found'),
  invalidState('invalid_state'),
  invalidArgument('invalid_argument'),
  assetMissing('asset_missing'),
  integrityFailed('integrity_failed'),
  unsupportedFormat('unsupported_format'),
  providerNotConfigured('provider_not_configured'),
  authenticationFailed('authentication_failed'),
  networkUnavailable('network_unavailable'),
  rateLimited('rate_limited'),
  quotaExceeded('quota_exceeded'),
  providerTimeout('provider_timeout'),
  modelUnavailable('model_unavailable'),
  pluginDenied('plugin_denied'),
  pluginBudgetExceeded('plugin_budget_exceeded'),
  incompatibleBackup('incompatible_backup'),
  corruptedBackup('corrupted_backup'),
  searchExpired('search_expired'),
  cursorExpired('cursor_expired'),
  unknown('unknown');

  const DiaryErrorCode(this.wire);

  final String wire;

  static DiaryErrorCode fromWire(String value) => _parseWire(
        values,
        (e) => e.wire,
        value,
        'DiaryErrorCode',
      );
}

/// 契约第 1.2 节的统一错误形状：code、用户可读 message、retryable，
/// 以及可选的 jobId / fieldErrors。堆栈只进入脱敏诊断，不放进这里。
class DiaryException implements Exception {
  const DiaryException({
    required this.code,
    required this.message,
    this.retryable = false,
    this.jobId,
    this.fieldErrors = const {},
  });

  final DiaryErrorCode code;
  final String message;
  final bool retryable;
  final String? jobId;
  final Map<String, String> fieldErrors;

  @override
  String toString() =>
      'DiaryException(${code.wire}): $message'
      '${retryable ? ' (retryable)' : ''}';
}
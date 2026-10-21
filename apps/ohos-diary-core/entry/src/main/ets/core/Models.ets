// C ABI 的 `data` / 参数类型（snake_case，与 `crates/diary_core` 的 serde 输出同一套字段名）。
//
// 口径：
// - 只声明界面真正会用到的字段；核心模型是这些接口的**超集**，运行时多出来的字段仍然在，
//   只是类型上没有名字（要用就补到这里，或走 `DiaryCore.rawCall()`）；
// - `Option<T>` 序列化成 `null`，所以这里写成 `T | null`，不是可选属性；
// - 枚举一律按**线上字符串**（`draft` / `committed` / `keyword` …）声明成 `string`，
//   不自造一套 ArkTS 枚举——C ABI 的字段名与取值以 `docs/architecture/m6-核心-C-ABI.md` 为准。

/** `core.info` 的恢复摘要。 */
export interface RecoverySummary {
  pending_jobs: number;
  open_recordings: number;
  recoverable_imports: number;
  notes: string[];
}

/** `core.info` 的 data（与 `diary_bridge::api::LibraryInfo` 同形）。 */
export interface LibraryInfo {
  api_version: string;
  data_schema_version: number;
  build_version: string;
  library_id: string;
  /** 已经接到 C ABI 上的方法名；没列出来的就是还没有。 */
  capabilities: string[];
  recovery: RecoverySummary;
  capture_count: number;
  last_event_sequence: number;
  /**
   * 两条能力的配置来源与就绪状态（C ABI 加性字段，`core.info.runtime_config`）。
   *
   * **键是 camelCase**——它与 `core-config.json` 的字段同名同形（`modelDir` /
   * `ortDylib` / `asrEngineLib` / `asrModelDir`）；`core.info` 的外层键仍是 snake_case。
   * 只 stat、不加载模型，宿主可以随手调。
   */
  runtime_config?: RuntimeConfig;
}

/** `core.info.runtime_config.file`：配置文件本身（不是配置项）的状态。 */
export interface ConfigFileReport {
  /** 期望位置；内存库（没有根目录）是 null。 */
  path: string | null;
  /** 文件存在且读到了（解析失败也算存在）。 */
  present: boolean;
  /** 读不了/解析失败的原因；正常时为 null。 */
  error: string | null;
  /** 文件里不认识的字段名（拼错字段时就是它）。 */
  ignoredFields: string[];
}

/** 语义（嵌入器）的配置状态。 */
export interface SemanticConfigReport {
  /** `absent` / `configured` / `ready` / `invalid`。 */
  state: string;
  /** `env` / `file` / `absent`。 */
  configSource: string;
  modelDir: string | null;
  modelDirSource: string;
  ortDylib: string | null;
  ortDylibSource: string;
  /** 已加载时的模型版本。 */
  modelVersion: string | null;
  reason: string | null;
}

/** 转写的配置状态。 */
export interface TranscriptionConfigReport {
  /** `absent` / `configured` / `ready`。 */
  state: string;
  /** 模型目录的来源：`env` / `file` / `provider` / `bundled` / `absent`。 */
  configSource: string;
  modelDir: string | null;
  modelDirSource: string;
  engineLib: string | null;
  /** `env` / `file` / `platform` / `absent`。 */
  engineLibSource: string;
  providerId: string | null;
  reason: string | null;
}

export interface RuntimeConfig {
  file: ConfigFileReport;
  semantic: SemanticConfigReport;
  transcription: TranscriptionConfigReport;
}

export interface ProcessingSummary {
  unprocessed: number;
  processing: number;
  searchable: number;
  needs_attention: number;
}

/** 一条混合记录（草稿或已提交）。 */
export interface Capture {
  id: string;
  revision: number;
  /** `draft` / `committed` / `trashed`。 */
  state: string;
  occurred_at: string;
  created_at: string;
  updated_at: string;
  time_zone: string;
  utc_offset_minutes: number;
  day_key: string;
  ordered_source_ids: string[];
  draft_text: string;
  processing_summary: ProcessingSummary;
}

/**
 * `captures.list` 的分页结果（「看今天」/历史列表）。
 *
 * `next_cursor` 是**不透明**字符串：原样回传给下一次 `captures.list`，不要解析、
 * 不要自己拼。`null` 表示没有下一页。
 */
export interface CapturePage {
  captures: Capture[];
  next_cursor: string | null;
}

/** `captures.saveDraft` 的结果：只有 `durable === true` 才能显示「已保存」。 */
export interface DraftSaveResult {
  revision: number;
  durable: boolean;
  saved_at: string;
}

/** 原始文字或来源元数据的某一次修订。 */
export interface SourceRevision {
  revision_id: string;
  source_id: string;
  parent_revision_id: string | null;
  text: string | null;
  asset_id: string | null;
  /** `user` / `system` …（线上字符串）。 */
  author_type: string;
  occurred_at: string;
}

/** `captures.commit` 的结果。 */
export interface CommitResult {
  capture: Capture;
  /** 草稿转成正式记录时创建的原文修订；理论上不会为 null。 */
  original_text_revision: SourceRevision | null;
}

/** `sources.locate` 的结果：前端能否打开原件，以及不能打开时的真实原因。 */
export interface SourceLocation {
  source_ref: string;
  locator: SourceLocator;
  available: boolean;
  /** 原件资产 id；纯文字来源没有可打开的原件，这里是 null。 */
  asset_id: string | null;
  /** `available === false` 时的原因（原样展示，不要自己改写）。 */
  reason: string | null;
}

/** 定位信息：文字区间 / 时间区间 / 页码 / 图片矩形（用不到的字段是 null）。 */
export interface SourceLocator {
  locator_type: string;
  source_revision_id: string;
  text_start: number | null;
  text_end: number | null;
  start_ms: number | null;
  end_ms: number | null;
  page_number: number | null;
  block_id: string | null;
  /** 图片归一化矩形 `[x0, y0, x1, y1]`；不是图片时为 null。 */
  rect: number[] | null;
  asset_id: string | null;
}

export interface ExtractedSegment {
  ordinal: number;
  text: string;
  locator: SourceLocator;
}

/** `sources.extract` 的结果（可重建的派生内容）。 */
export interface ExtractedContent {
  id: string;
  source_id: string;
  source_revision_id: string;
  extractor_id: string;
  extractor_version: string;
  text: string;
  segments: ExtractedSegment[];
  /** `pending` / `processing` / `ready` / `partial` / `failed` / `stale`。 */
  status: string;
  /** `complete` / `partial` / `metadata_only` / `unavailable`。 */
  coverage: string;
  coverage_reason: string | null;
  error_code: string | null;
  created_at: string;
}

export interface TextRange {
  start: number;
  end: number;
}

/** 一条检索命中。 */
export interface SearchHit {
  hit_id: string;
  /** 同一来源的命中折叠归组用的键；片段命中时是 `source_id`，**不是**记录 id。 */
  group_id: string;
  /** 这一条命中属于哪篇记录（`captures.id`）；两种命中都有，跳详情页用它。 */
  capture_id: string;
  /** `text` / `image` / `audio` / `video` / `file` / `link`。 */
  source_kind: string;
  /** 命中原因，元素是 `keyword` / `semantic` / `metadata`。 */
  matched_by: string[];
  coverage: string;
  source_id: string | null;
  revision_id: string | null;
  day_key: string | null;
  title: string | null;
  snippet: string | null;
  highlights: TextRange[];
  locator: SourceLocator | null;
}

/** `search.start` 的结果：每次都是**当前页的完整快照**，不要把两份快照当增量拼。 */
export interface SearchSnapshot {
  session_id: string;
  query_revision: number;
  /** `initial` / `keyword_ready` / `hybrid_ready` / `done` / `cancelled`。 */
  phase: string;
  results: SearchHit[];
  cursor: string | null;
  index_coverage: string;
  warnings: string[];
}

export interface JobProgress {
  completed: number;
  total: number;
}

/** 持久任务。 */
export interface Job {
  id: string;
  kind: string;
  /** `queued` / `running` / `succeeded` / `retry_wait` / `waiting_configuration` /
   *  `waiting_dependency` / `waiting_network` / `failed` / `cancelled`。 */
  state: string;
  priority: number;
  target_ids: string[];
  input_snapshot_hash: string | null;
  progress: JobProgress | null;
  attempt_count: number;
  max_attempts: number;
  next_attempt_at: string | null;
  error_code: string | null;
  requires_user_action: boolean;
  attention_key: string | null;
  created_at: string;
  updated_at: string;
}

/**
 * `jobs.list` 的分页结果（任务中心逐条）。
 *
 * 每条 `Job` 里带 `attention_key` 与 `progress`；`next_cursor` 不透明，原样回传。
 */
export interface JobPage {
  jobs: Job[];
  next_cursor: string | null;
}

/** `jobs.runDue` 的摘要。 */
export interface JobRunSummary {
  started_at: string;
  trigger: string;
  processed: number;
  succeeded: number;
  failed: number;
  retrying: number;
  backfilled: number;
  recovered: number;
  due_backlog: number;
  waiting_dependency: number;
  dependency_failed: number;
  next_wakeup: string | null;
}

export interface JobErrorSummary {
  job_id: string;
  kind: string;
  error_code: string;
  occurred_at: string;
  requires_user_action: boolean;
}

/** `jobs.status` 的结果。 */
export interface JobStatus {
  queued: number;
  running: number;
  retry_wait: number;
  waiting_configuration: number;
  waiting_dependency: number;
  waiting_network: number;
  failed: number;
  cancelled: number;
  succeeded: number;
  due_backlog: number;
  next_wakeup: string | null;
  recent_errors: JobErrorSummary[];
}

/**
 * `indexes.status` 的结果：索引就绪与覆盖。
 *
 * `semantic_index_ready` 只在**生效代次覆盖全部块且口径一致**时为 true；
 * 为 false 时原因在 `reasons[]` 里（原样展示，不要自己改写）。`model_version`
 * 为 null 有三种情况：还没有代次、代次里没有向量、代次里混了多种模型版本
 * （最后一种会在 `reasons` 里点名并提示重建）。
 */
export interface IndexStatus {
  /** 关键词覆盖：`complete` / `partial` / `metadata_only` / `unavailable`。 */
  coverage: string;
  keyword_index_ready: boolean;
  semantic_index_ready: boolean;
  tokenizer_version: string;
  model_version: string | null;
  chunker_version: string | null;
  total_chunks: number;
  embedded_chunks: number;
  indexed_segments: number;
  total_segments: number;
  pending_segments: number;
  stale_segments: number;
  indexed_captures: number;
  total_captures: number;
  failed_sources: number;
  indexed_chars: number;
  index_rows: number;
  index_terms: number;
  /** 整库索引占用（含语义表）；不随 `source_scope` 变化。 */
  index_bytes: number;
  reasons: string[];
}

// ---------------------------------------------------------------- 日记（契约 2.5 / 4.3）
//
// 字段名与 `diary_core::model` 的 serde 输出一致（snake_case）。
// 「哪一版是当前、哪些是候选」只在 `DiaryDay` 里表达：`DiaryVersion` 本身没有状态字段。

/** 当天的材料统计（数字都是真值，不是估计）。 */
export interface MaterialCounts {
  /** 当天的记录数（不含回收站）。 */
  captures: number;
  /** 已经有可用正文的材料数（用户的文字天然算「已处理」）。 */
  processed: number;
  /** 还在等提取的材料数。 */
  pending: number;
  /** 提取失败、暂无可读正文的材料数。 */
  failed: number;
}

/**
 * 某一天的日记聚合视图（`diary.getDay`）。
 *
 * **空的一天是 `not_found`，不是空对象**：核心只在「这一天既没有材料也没有版本」时
 * 报 `not_found`，界面据此显示「今天还没有成篇日记」，不要把它当读取失败。
 */
export interface DiaryDay {
  day_key: string;
  /** 这一天的时区名；没有任何材料也没有生成过时是 `UTC`（缺省，不是推测）。 */
  time_zone: string;
  /** 当前版本；这一天还没有任何版本时为 null。 */
  current_version_id: string | null;
  /** 候选版本（除当前版本之外的全部版本），最新在前。 */
  candidate_version_ids: string[];
  /**
   * 这一天的 revision：持久、单调递增的状态推进计数。
   *
   * `saveEdit` / `setCurrent` / `merge` 的 `expected_revision` 必须原样回传它
   * （不是版本数：换当前版本不增加版本数）。
   */
  revision: number;
  material_counts: MaterialCounts;
  /** 覆盖程度：`complete` / `partial` / `metadata_only` / `unavailable`。 */
  coverage: string;
  updated_at: string;
}

/** 生成过程的审计信息（合并版本也会带这一格：`modelCalls = "0"`）。 */
export interface GenerationInfo {
  prompt_version: string;
  provider_id: string | null;
  model: string | null;
  /** 生成选项：固定的协议参数与**如实记录的步数**（键值都是字符串）。 */
  generation_options: Object;
  covered_source_count: number;
  unprocessed_source_count: number;
}

/** 正文与来源的关联；`body_range` 为 null 表示整段关联或位置不可靠（不伪造）。 */
export interface ProvenanceLink {
  source_id: string;
  source_revision_id: string;
  locator: SourceLocator;
  body_range: TextRange | null;
  note: string | null;
}

/** 一条日记版本（正文是自然段；来源关联在 `provenance_links`）。 */
export interface DiaryVersion {
  id: string;
  day_key: string;
  parent_version_id: string | null;
  /** `generated` / `user_edit` / `merge`。 */
  origin: string;
  body: string;
  created_at: string;
  source_snapshot_id: string | null;
  style_snapshot_id: string | null;
  generation_info: GenerationInfo | null;
  provenance_links: ProvenanceLink[];
}

/** `diary.listVersions` 的分页结果；`next_cursor` 不透明，原样回传。 */
export interface DiaryVersionPage {
  versions: DiaryVersion[];
  next_cursor: string | null;
}

// ---------------------------------------------------------------- 回收站（契约 4.1 / §8）
//
// 字段名与 `diary_core::model` 的 `PurgePreview` / `PurgeResult` serde 输出一致（snake_case）。
// 契约文档里的 `derivedDiaryVersionIds` / `undeterminedDiaryVersionIds` / `previewToken`
// 是 Dart 面的 camelCase 写法，线上是下面这些 snake_case 名字。

/**
 * `captures.previewPurge` 的结果：这次清理会删掉什么、会保留什么。
 *
 * 清单是**这次预览时**数据库里的真实行。`preview_token` 一次性、带过期、绑定
 * `capture_id`，`purgeCapture` 必须原样带上；预览之后清单变了核心会拒绝，要求重新预览。
 */
export interface PurgePreview {
  capture_id: string;
  /** 一次性令牌（`purgeCapture.preview_token`）；默认 10 分钟过期。 */
  preview_token: string;
  /** 会被一起清理的原件（`assets.id`）。 */
  asset_ids: string[];
  /** 会被一起清理的派生内容（提取正文、片段与检索词项都在里面）。 */
  derived_content_ids: string[];
  /**
   * 与这条记录**有可判定关联**的日记版本：`delete_diary_versions = true` 时删，
   * 默认（`false`）保留。契约里叫「有可判定关联的版本」。
   */
  diary_versions_still_referencing: string[];
  /** 会被一并清理的 AI 回应（请求文字与生成正文一起删）。 */
  ai_response_ids: string[];
  /**
   * 清理后**仍保留**的输入快照：`body` 里可能仍含这条记录的正文（#161 方案 A）。
   * 界面不能把永久清理说成「所有历史快照里的字节都消失」。
   */
  retained_source_snapshot_ids: string[];
  /**
   * 由上面那些版本派生/合并来的版本（`user_edit` / `merge`）：**一个都不删**，
   * 但正文可能仍含这条记录的文字，必须如实告知用户「这些版本还在」。
   */
  derived_diary_version_ids: string[];
  /** **归属判不出来**的版本：判不出来就不删，列出来由用户自己处置。 */
  undetermined_diary_version_ids: string[];
}

/**
 * `captures.purge` 的结果：`purged_*` 是真删掉的 id，`retained_*` 是如实保留的 id。
 *
 * `purged_diary_version_ids` / `retained_diary_version_ids` 按 `delete_diary_versions`
 * 二选一；`derived_diary_version_ids` / `undetermined_diary_version_ids` 与那个选择
 * **无关**（这两类一个都不删），与预览同名项一致。
 */
export interface PurgeResult {
  capture_id: string;
  purged_asset_ids: string[];
  purged_derived_content_ids: string[];
  purged_diary_version_ids: string[];
  purged_ai_response_ids: string[];
  retained_diary_version_ids: string[];
  retained_source_snapshot_ids: string[];
  derived_diary_version_ids: string[];
  undetermined_diary_version_ids: string[];
  /** 被清掉的该记录幂等回执数（`captures.purge` 自己的回执保留，供重放）。 */
  cleaned_receipt_count: number;
}

// ---------------------------------------------------------------- 请求参数
//
// 字段名以核心 Rust 参数名为准（snake_case）；契约表里的 camelCase 写法 C ABI 也接受，
// 但 ArkTS 侧只用一种，避免同一个字段两种拼法。

/** `captures.createDraft` 的参数。 */
export interface CreateDraftParams {
  /** 事件时间（RFC 3339）；不给就是现在。 */
  occurred_at?: string;
  /** 设备当前 IANA 时区名，例如 `Asia/Shanghai`。 */
  time_zone: string;
  /** 事件发生时该时区的偏移分钟数。 */
  utc_offset_minutes: number;
  /** 幂等键：同一个 operationId 重复提交返回原结果。 */
  operation_id: string;
}

/** `captures.saveDraft` 的参数。 */
export interface SaveDraftParams {
  capture_id: string;
  text: string;
  /** 读到的 revision；与库里不一致时核心报 `revision_conflict`。 */
  expected_revision: number;
  operation_id: string;
}

/** `captures.commit` 的参数。 */
export interface CommitParams {
  capture_id: string;
  expected_revision: number;
  operation_id: string;
}

/** `captures.get` 的参数。 */
export interface CaptureIdParams {
  capture_id: string;
}

/**
 * `captures.list` 的参数：三个字段都可省。
 *
 * - `day_key` 省略 = 全部日期（历史列表）；「看今天」给当天的 `YYYY-MM-DD`；
 * - `cursor` 原样回传上一页的 `next_cursor`，不要解析或自己拼；
 * - `limit` 默认 20，核心最多给 100 条。
 */
export interface ListCapturesParams {
  day_key?: string | null;
  cursor?: string | null;
  limit?: number;
}

/**
 * `captures.trash` / `captures.restore` 的参数。
 *
 * `expected_revision` 是 `Capture.revision`（不是来源数、也不是日记的 revision）：
 * 不符报 `revision_conflict`，核心零改动。`operation_id` 是幂等键，重放返回原结果。
 */
export interface TrashCaptureParams {
  capture_id: string;
  expected_revision: number;
  operation_id: string;
}

/**
 * `captures.purge` 的参数。
 *
 * `preview_token` **必填**：永久清理必须由一次明确的预览授权（`previewPurge` 发出的
 * 一次性令牌）。`delete_diary_versions` 省略即 `false` = 保留仍引用这条记录的日记版本；
 * `true` 也只删「有可判定关联」的 `generated` 版本。同一个 `operation_id` 换选择报
 * `idempotency_conflict`。
 */
export interface PurgeCaptureParams {
  capture_id: string;
  preview_token: string;
  delete_diary_versions?: boolean;
  operation_id: string;
}

/** `sources.extract` 的参数：`source_ref` 可以是来源 id，也可以是修订 id。 */
export interface ExtractParams {
  source_ref: string;
}

/**
 * `sources.extractedContent` / `sources.previousRevision` 的参数：只要来源 id。
 *
 * 两个方法都可能返回 `null`（还没提取过 / 没有上一版），这是正常结果，不是错误。
 */
export interface SourceIdParams {
  source_id: string;
}

/**
 * `sources.reviseText` 的参数：改正文会建**新修订**，不覆盖旧内容。
 *
 * `expected_revision` 是记录的 revision；不符会抛 `revision_conflict`，
 * 核心一个字节都不改。`operation_id` 是幂等键，重放返回同一条修订。
 */
export interface ReviseTextParams {
  source_id: string;
  text: string;
  expected_revision: number;
  operation_id: string;
}

/** `sources.locate` 的参数：来源引用 + 定位信息（`locator_type` 用线上字符串）。 */
export interface LocateSourceParams {
  /** 来源 id 或修订 id；契约表写作 `sourceRef`，Dart 方法参数名是 `ref`。 */
  source_ref: string;
  locator: SourceLocator;
}

/** `search.start` 的检索条件；只有 `query` 必填。 */
export interface SearchRequestParams {
  query: string;
  /** `keyword`（默认）/ `semantic` / `hybrid`（以核心支持为准）。 */
  mode?: string;
  filters?: SearchFiltersParams;
  /** 默认 20（与 Dart 侧的 pageSize 同一口径）。 */
  page_size?: number;
}

/** 检索过滤条件：全字段可选，缺省即「不限」。 */
export interface SearchFiltersParams {
  from_day_key?: string;
  to_day_key?: string;
  kinds?: string[];
  source_scope?: string[];
  include_old_diary_versions?: boolean;
  include_trashed?: boolean;
}

/** `search.start` 的参数。 */
export interface SearchParams {
  request: SearchRequestParams;
  /** 与检索结果里的 `query_revision` 对齐；第一次检索给 1。 */
  query_revision: number;
}

/**
 * `search.nextPage` 的参数。
 *
 * `cursor` 省略 = 用会话自己记着的下一页；显式给时必须是**同一会话、同一结果代次**
 * 的游标（升级后旧游标一律 `cursor_expired`）。
 */
export interface SearchNextPageParams {
  session_id: string;
  cursor?: string | null;
}

/** `search.upgrade` / `search.cancel` 的参数。 */
export interface SearchSessionParams {
  session_id: string;
}

/** `indexes.rebuild` 的参数；`scope` 编码是 `all` 或 `sources:<id>[,<id>]*`。 */
export interface RebuildIndexParams {
  scope: string;
  reason: string;
  operation_id: string;
}

/**
 * `indexes.rebuildSemantic` 的参数：没有 scope。
 *
 * 语义的代次与向量是全局的，范围只决定这一轮重算了哪些块（与关键词那条不同）。
 */
export interface RebuildSemanticParams {
  reason: string;
  operation_id: string;
}

/**
 * `indexes.status` 的参数。
 *
 * `source_scope` 省略 = 整库；给空数组 = 「什么都不看」（两者刻意不同，与核心同义）。
 */
export interface IndexStatusParams {
  source_scope?: string[] | null;
}

/** `jobs.runDue` 的参数。 */
export interface RunDueParams {
  budget_ms: number;
  /** 唤醒来源，例如 `startup` / `foreground` / `manual`。 */
  trigger: string;
}

/**
 * `jobs.list` 的参数。
 *
 * `states` 省略或空数组都是「不限状态」；`cursor` 原样回传 `next_cursor`；
 * `limit` 默认 20，核心最多给 200 条。
 */
export interface ListJobsParams {
  states?: string[] | null;
  cursor?: string | null;
  limit?: number;
}

/** `jobs.get` 的参数。 */
export interface JobIdParams {
  job_id: string;
}

/**
 * `jobs.retry` 的参数。
 *
 * `operation_id` 是幂等键：同一个 op 重放返回第一次的结果。建议总是给——
 * 重试按钮要能安全重放。
 */
export interface RetryJobParams {
  job_id: string;
  operation_id?: string | null;
}

/** `jobs.cancel` 的参数（核心要求 `operation_id`）。 */
export interface CancelJobParams {
  job_id: string;
  operation_id: string;
}

/** `jobs.acknowledgeAttention` 的参数：原样传 `Job.attention_key`。 */
export interface AcknowledgeAttentionParams {
  attention_key: string;
}

// ---------------------------------------------------------------- 日记请求参数（契约 4.3）

/** `diary.getDay` 的参数。 */
export interface DiaryDayParams {
  /** `YYYY-MM-DD`（核心校验；形状不对报 `invalid_argument`）。 */
  day_key: string;
}

/**
 * `diary.listVersions` 的参数。
 *
 * `cursor` 原样回传上一页的 `next_cursor`；`limit` 默认 20，核心夹 1–100。
 */
export interface ListDiaryVersionsParams {
  day_key: string;
  cursor?: string | null;
  limit?: number;
}

/** `diary.getVersion` 的参数。 */
export interface DiaryVersionParams {
  version_id: string;
}

/**
 * `diary.saveEdit` 的参数：人工编辑写成**新版本**，基版一个字节都不动。
 *
 * `expected_revision` 是 `DiaryDay.revision`（不是版本数）；不符报
 * `revision_conflict`，核心零改动。`operation_id` 是幂等键。
 */
export interface SaveDiaryEditParams {
  base_version_id: string;
  body: string;
  expected_revision: number;
  operation_id: string;
}

/**
 * `diary.setCurrent` 的参数。
 *
 * 只能指向这一天自己的版本；`expected_revision` 是 `DiaryDay.revision`。
 * 这一行契约没有 `operation_id`：按目标状态幂等。
 */
export interface SetDiaryCurrentParams {
  day_key: string;
  version_id: string;
  expected_revision: number;
}

/** `diary.merge` 的参数：`body` 是用户确认后的正文（核心不调模型）。 */
export interface MergeDiaryParams {
  base_version_id: string;
  candidate_version_ids: string[];
  body: string;
  expected_revision: number;
  operation_id: string;
}

// ---------------------------------------------------------------- 录音（契约 4.2）

/** `recordings.prepare` 的结果：平台层只能往 `staging_ticket` 这个目录里写片段。 */
export interface RecordingTicket {
  recording_id: string;
  asset_id: string;
  /** 应用私有目录里的**绝对路径**（核心已经建好）。 */
  staging_ticket: string;
}

/** 一个已封闭片段的描述（`register_segment` 的 `segment` 参数）。 */
export interface SegmentManifest {
  segment_id: string;
  /** 相对暂存目录的文件名，不是绝对路径。 */
  relative_file_name: string;
  duration_ms: number;
  /** 小写/大写十六进制都行；核心会自己重算并忽略大小写比较。 */
  sha256: string;
  byte_size: number;
  /** RFC 3339；不给就由核心用当前时间。 */
  closed_at?: string | null;
}

/** `recordings.registerSegment` 的回执；`durable === true` 才算落库。 */
export interface SegmentReceipt {
  segment_id: string;
  segment_index: number;
  durable: boolean;
  recorded_at: string;
}

/** 录音会话快照（`recordings.updateState` 的返回）。 */
export interface RecordingSession {
  recording_id: string;
  asset_id: string;
  /** `idle` / `preparing` / `recording` / `paused` / `stopping` / `saved` /
   *  `interrupted` / `recoverable` / `error`。 */
  state: string;
  /** 最后一次上报的「已收到」时长（`received_ms` 的同源值）。 */
  elapsed_ms: number;
  /** 最后一次上报的「已登记」时长；与 `elapsed_ms` 的差就是可能丢掉的尾巴。 */
  durable_through_ms: number;
  segment_count: number;
  last_error: string | null;
}

/** `recordings.finalize` 的结果。转写任务与最终化同一个事务，`transcription_queued` 为 true。 */
export interface RecordingFinalizeResult {
  recording_id: string;
  /** 这条录音挂到的记录；恢复路径靠它拿回 capture id 才能提交。 */
  capture_id: string;
  /**
   * finalize 完成后记录的权威 revision（就是这次递增后的值）。
   *
   * `captures.commit` 的 `expected_revision` 必须用它，**不要自己猜**。
   */
  capture_revision: number;
  asset_id: string;
  state: string;
  segment_count: number;
  transcription_queued: boolean;
  logical_duration_ms: number;
}

/** `recordings.recover` 的结果：缺口、已登记/已收到的时长与「尾巴未知」都在 notes 里如实写。 */
export interface RecordingRecovery {
  recording_id: string | null;
  state: string;
  /** 真实已登记的片段序号（没登记成功的尾巴不在里面）。 */
  closed_segment_indexes: number[];
  /** **已登记**片段时长之和：真正落盘、`finalize` 能拼出来的音频长度。 */
  persisted_ms: number;
  /** 采集器**最后一次上报**的「已收到 PCM 总时长」；可能落后，是实际收到量的下界。 */
  received_ms: number;
  /** 已知缺口的总时长（按平均段长估算）；没有缺口信息时为 0，并在 notes 里说明。 */
  gap_ms: number;
  notes: string[];
}

/** `recordings.prepare` 的参数。 */
export interface PrepareRecordingParams {
  capture_id: string;
  operation_id: string;
}

/** `recordings.registerSegment` 的参数。 */
export interface RegisterSegmentParams {
  recording_id: string;
  segment_index: number;
  segment: SegmentManifest;
}

/** `recordings.updateState` 的参数（核心不虚构麦克风状态，全部由平台层上报）。 */
export interface NativeRecordingStatusParams {
  recording_id: string;
  state: string;
  /** RFC 3339 的墙上时间。 */
  wall_clock: string;
  /** 采集器**实际收到**的 PCM 总时长（只增，含还没封闭的尾巴）。 */
  elapsed_ms: number;
  /** **已成功登记**片段时长之和；只能用它当 `durable_through_ms`，不能用 `elapsed_ms`。 */
  persisted_through_ms: number;
  input_device_changed: boolean;
  message?: string | null;
}

/** `recordings.finalize` 的参数。 */
export interface FinalizeRecordingParams {
  recording_id: string;
  last_segment_index: number;
  end_reason: string;
}

/** `recordings.recover` 的参数；`recording_id` 省略时恢复最近一条未收尾的录音。 */
export interface RecoverRecordingParams {
  recording_id?: string | null;
}

/**
 * `providers.save` 的配置。本机引擎（`adapter_id = sherpa_onnx`）的 `model` 是
 * **模型目录**，`endpoint` 必须为空；`test_status` 由核心维护，调用方写什么都会被忽略。
 */
export interface ProviderConfig {
  id: string;
  label: string;
  adapter_id: string;
  endpoint: string | null;
  model: string | null;
  capabilities: string[];
  credential_ref: string | null;
  enabled: boolean;
  test_status: string;
}

/** `providers.save` 的参数（`secret` 只用于远端服务；本机引擎传 null）。 */
export interface SaveProviderParams {
  config: ProviderConfig;
  secret: string | null;
}

/** `providers.test` 的参数：`scope` 是用户显式授权的测试范围。 */
export interface TestProviderParams {
  provider_id: string;
  scope: string[];
}

/** `providers.remove` 的参数。 */
export interface ProviderIdParams {
  provider_id: string;
}

/** `providers.test` 的报告：`actual_call_made` 是数出来的真实调用次数。 */
export interface ProviderTestReport {
  provider_id: string;
  actual_call_made: boolean;
  /**
   * 用户可读说明；核心是 `Option<String>`，**没有说明时是 null**。
   *
   * 这里以前声明成 `string`，直接 `.length` 会在 null 上崩——已按 Rust 模型改成
   * `string | null`，调用方要先判空。
   */
  message: string | null;
  /** 能力 → `ok` / `failed` 的逐项结论（JSON 对象，键是能力名）。 */
  results: Object;
}

/**
 * 完整设置对象（契约 4.4 `settings.get / update`）。
 *
 * 字段名与核心 serde 输出一致（snake_case）；整理设置的开关与计划时间就是这里的
 * `auto_organize_enabled` / `auto_organize_time`——**与 `organizeSettings()` 是同一份
 * 真值**（核心 `settings` 表的同一行），不是两套存储。
 */
export interface Settings {
  /** IANA 时区名。新库首次是 `UTC`；平台层应在首次启动时用设备时区 update 一次。 */
  diary_time_zone: string;
  /** 整行设置的乐观锁版本。 */
  revision: number;
  /** RFC 3339（UTC）。 */
  updated_at: string;
  /** 自动整理开关。**默认 false**：没有显式开启，核心不会自动调用任何服务。 */
  auto_organize_enabled: boolean;
  /** 计划触发时间 `HH:mm`，不是完成承诺。 */
  auto_organize_time: string;
  /** 新建日记默认使用的风格 id（没有则为 null）。 */
  default_style_id: string | null;
}

/**
 * `settings.update` 的参数。
 *
 * `settings.revision` / `updated_at` 是只读结论：核心忽略它们，冲突保护只认
 * `expected_revision`。
 */
export interface UpdateSettingsParams {
  settings: SettingsInput;
  expected_revision: number;
}

/**
 * `settings.update` 的入参形态（只含可写字段）。
 *
 * `diary_time_zone` / `auto_organize_enabled` / `auto_organize_time` 三个必填；
 * `default_style_id` 是**三态**：
 * - **省略**（`undefined`）= 不改已经设好的默认风格；
 * - 显式 `null` = 清空；
 * - 显式值 = 设成它。
 *
 * 省略与 `null` 刻意分开：`settings.update` 只改这几个可写字段（模板与上限原样不
 * 动），把「没传」当成「清空」会静默丢掉用户设好的默认风格。`revision` /
 * `updated_at` 是只读结论，可省。
 */
export interface SettingsInput {
  diary_time_zone: string;
  auto_organize_enabled: boolean;
  auto_organize_time: string;
  default_style_id?: string | null;
  revision?: number;
  updated_at?: string;
}

// ---------------------------------------------------------------- 事件（契约第 6 节）

/** 一条持久业务事件；`sequence` 是单调递增的游标。 */
export interface DomainEvent {
  event_id: string;
  sequence: number;
  /** 线上字符串：`capture.changed` / `asset.changed` / `job.changed` … */
  type: string;
  entity_id: string;
  revision: number;
  emitted_at: string;
}

/**
 * `core.events` 的一页。
 *
 * `next_sequence` 原样作为下一次调用的 `from_sequence`；`truncated === true` 表示
 * 这一页之后还有事件，必须继续拉，不能把一页当成全部。
 */
export interface DomainEventPage {
  events: DomainEvent[];
  next_sequence: number;
  truncated: boolean;
}

/**
 * `core.events` 的参数。
 *
 * `from_sequence` 省略 = 从**当前最新**序号开始（只等下一条，不补历史）；
 * 显式给 0 才是从头补。`limit` 默认 200、上限 1000。
 */
export interface EventsParams {
  from_sequence?: number | null;
  limit?: number;
}

// ---------------------------------------------------------------- 备份与恢复（契约 4.5）

/**
 * 备份快照里的关键行数。
 *
 * **键是 camelCase**：核心的 `BackupCounts` 带 `#[serde(rename_all = "camelCase")]`
 * （与备份清单的其它字段同一套写法）。外层 `BackupReport` 仍是 snake_case——这是
 * 核心 serde 的现状，ArkTS 侧照实声明，不要按外层类推。
 */
export interface BackupCounts {
  /** 记录数（含草稿与回收站：备份是整库）。 */
  captures: number;
  /** 原始文字/原件修订数。 */
  sourceRevisions: number;
  /** 老 schema 的备份可能没有这几项，是 null。 */
  assets: number | null;
  blobs: number | null;
  importsInFlight: number | null;
  openRecordings: number | null;
}

/** `backups.create` 的结果（同步入口，不是契约里的 `jobId`）。 */
export interface BackupReport {
  /** 备份包（归档文件）的最终路径；写完并自校验通过之后才有这个文件。 */
  package_path: string;
  byte_size: number;
  schema_version: number;
  api_version: string;
  core_version: string;
  created_at: string;
  blob_count: number;
  blob_bytes: number;
  transient_clip_count: number;
  counts: BackupCounts;
  /** 需要调用方知道的事实（未完成的导入/录音、暂存片段没进包等）。 */
  warnings: string[];
}

/**
 * `backups.inspect` 的结果。
 *
 * **不抛错**：包坏了/路径越界/版本不兼容都进 `issues`，先展示再决定要不要恢复；
 * 只有连清单都读不出来才抛 `corrupted_backup`。
 */
export interface BackupInspection {
  package_path: string;
  format_version: number;
  schema_version: number;
  api_version: string;
  core_version: string;
  created_at: string;
  byte_size: number;
  blob_count: number;
  blob_bytes: number;
  asset_count: number;
  transient_clip_count: number;
  counts: BackupCounts;
  /** 每条格式是 `种类: 细节`，种类只有 `corrupted` / `unsafe_path` / `incompatible`。 */
  issues: string[];
  /** 本构建能不能恢复这个包。 */
  compatible: boolean;
  /** 是否逐条目校验过哈希。 */
  hashes_verified: boolean;
}

/** `backups.restore` 的结果：恢复出来的新库（当前库一个字节都没动）。 */
export interface RestoreReport {
  /** 恢复出来的资料库文件路径；直接交给 `DiaryCore.open()`。 */
  library_path: string;
  root_dir: string;
  schema_version: number;
  api_version: string;
  core_version: string;
  created_at: string;
  byte_size: number;
  blob_count: number;
  blob_bytes: number;
  transient_clip_count: number;
  counts: BackupCounts;
  warnings: string[];
}

/**
 * `backups.create` 的参数。
 *
 * `target_path` 是**包文件的完整路径**；OHOS 上必须在应用沙箱内（否则
 * `invalid_argument`）。`include_assets` 只能是 true（核心只做完整备份）。
 */
export interface CreateBackupParams {
  target_path: string;
  include_assets?: boolean;
}

/** `backups.inspect` 的参数：包路径（OHOS 上同样必须在沙箱内）。 */
export interface InspectBackupParams {
  package_path: string;
}

/** `RestoreOptions` 的 JSON 形态；默认恢复到新库、不动当前库。 */
export interface RestoreOptions {
  restore_as_new_library?: boolean;
  /**
   * 形状兼容字段：核心恢复的是快照整体（索引/向量都在里面），这个开关不改变结果。
   */
  include_indexes?: boolean;
}

/**
 * `backups.restore` 的参数。
 *
 * 契约的 Dart 形状只有包票据 + 选项；C ABI 必须把**恢复目标目录**也说清楚
 * （核心要求一个新的空目录），所以多一个 `target_dir`。
 */
export interface RestoreBackupParams {
  package_path: string;
  target_dir: string;
  options?: RestoreOptions;
}

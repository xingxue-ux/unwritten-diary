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
  group_id: string;
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

/** `sources.extract` 的参数：`source_ref` 可以是来源 id，也可以是修订 id。 */
export interface ExtractParams {
  source_ref: string;
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

/** `indexes.rebuild` 的参数；`scope` 编码是 `all` 或 `sources:<id>[,<id>]*`。 */
export interface RebuildIndexParams {
  scope: string;
  reason: string;
  operation_id: string;
}

/** `jobs.runDue` 的参数。 */
export interface RunDueParams {
  budget_ms: number;
  /** 唤醒来源，例如 `startup` / `foreground` / `manual`。 */
  trigger: string;
}

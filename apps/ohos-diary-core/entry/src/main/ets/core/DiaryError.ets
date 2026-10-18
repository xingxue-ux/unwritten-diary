// ArkTS 侧的错误类型：**所有**失败（核心业务错误 + 原生层失败）都归一到 `DiaryError`。
//
// 为什么统一成异常而不是「返回信封」：
// - 业务代码不该在每次调用后手写 `if (!env.ok)`——漏判一次就会把错误当数据用；
// - ArkTS 的 `try / catch / finally` 与资源释放（`core.close()`）天然配套；
// - 原始信封仍然是可拿到的（NAPI 层 `call()` 的返回值），需要日志 / 回放时走
//   `DiaryCore.rawEnvelope()`，两种口径不冲突。
//
// 错误码分两类：
// - **核心码**（`CORE_ERROR_CODES`，21 个）：与契约第 7 节、`diary_core::ErrorCode`、
//   Dart 的 `DiaryErrorCode` 是同一套。`code` 直接透传，不自造、不改写；
// - **宿主码** `native_unavailable`：原生库没加载 / 参数或句柄层失败 / 信封不是合法 JSON。
//   它**不是**核心契约里的码，只表示「调用没能发生」，出现即打包或部署有问题。

/** 核心契约第 7 节的错误码（与 `crates/diary_core/src/error.rs` 的 `ErrorCode::wire()` 一致）。 */
export const CORE_ERROR_CODES: string[] = [
  'not_found',
  'invalid_state',
  'invalid_argument',
  'revision_conflict',
  'idempotency_conflict',
  'storage_full',
  'asset_missing',
  'permission_denied',
  'integrity_failed',
  'unsupported_format',
  'search_expired',
  'cursor_expired',
  'provider_not_configured',
  'authentication_failed',
  'network_unavailable',
  'rate_limited',
  'quota_exceeded',
  'provider_timeout',
  'model_unavailable',
  'corrupted_backup',
  'incompatible_backup',
];

/** 宿主码：调用没能发生（不是核心契约里的码）。 */
export const NATIVE_UNAVAILABLE: string = 'native_unavailable';

/** 信封里 `error` 的形状（也是 `diary_last_error()`  JSON 的形状）。 */
export interface ErrorBody {
  code: string;
  message: string;
  retryable?: boolean;
}

/** JS `Error` 上可能带的字段（原生层抛的错带 `ERR_DIARY_*`）。 */
interface ThrownLike {
  code?: string;
  message?: string;
}

/**
 * 把一段文本按 `diary_last_error()` 的 JSON 解析；不是这种形状就返回 `null`。
 *
 * 这条路径用在 `open()` 失败上：C 侧只能返回 NULL，原因在 `diary_last_error()` 里，
 * shim 把那段 JSON 原样放进 JS 异常的 message，这里再还原成 `code/message/retryable`。
 */
export function parseErrorBody(text: string): ErrorBody | null {
  const trimmed = text.trim();
  if (!trimmed.startsWith('{')) {
    return null;
  }
  try {
    const body = JSON.parse(trimmed) as ErrorBody;
    if (body !== null && body !== undefined && typeof body.code === 'string'
      && typeof body.message === 'string') {
      return body;
    }
  } catch (e) {
    return null;
  }
  return null;
}

/**
 * 一次核心调用的失败。
 *
 * 用法：
 * ```ts
 * try {
 *   core.saveDraft(params);
 * } catch (e) {
 *   const err = e as DiaryError;
 *   if (err.code === 'revision_conflict') { ... }   // 重新读取后再试
 *   if (err.retryable) { ... }                      // 可退避重试
 * }
 * ```
 */
export class DiaryError extends Error {
  /** 核心码（21 个之一）或宿主码 `native_unavailable`。 */
  readonly code: string;
  /** 核心给出的「可重试」建议；宿主码恒为 false。 */
  readonly retryable: boolean;
  /** 信封里的 `apiVersion`（核心码才有；宿主码为空串）。 */
  readonly apiVersion: string;

  constructor(code: string, message: string, retryable: boolean = false, apiVersion: string = '') {
    super(message);
    this.name = 'DiaryError';
    this.code = code;
    this.retryable = retryable;
    this.apiVersion = apiVersion;
  }

  /** 这个错误是不是核心契约里的码（而不是宿主侧的调用失败）。 */
  isCoreError(): boolean {
    return this.code !== NATIVE_UNAVAILABLE;
  }

  /**
   * 从 NAPI 层抛出的 JS 异常还原。
   *
   * - `open()` 失败时，message 是 `diary_last_error()` 的 JSON 原文 ⇒ 还原出真实的
   *   `code/message/retryable`（例如 `permission_denied`、`invalid_argument`）；
   * - 其他原生层失败（`ERR_DIARY_LIB` / `ERR_DIARY_ARGS` / `ERR_DIARY_HANDLE`）⇒
   *   `native_unavailable`，并把原 code 拼进 message，排障时一眼能看出断在哪一层。
   */
  static fromThrown(raw: Object): DiaryError {
    const thrown = raw as ThrownLike;
    const message: string = thrown.message !== undefined ? thrown.message : '原生层调用失败';
    const nativeCode: string = thrown.code !== undefined ? thrown.code : '';
    const nested = parseErrorBody(message);
    if (nested !== null) {
      return new DiaryError(nested.code, nested.message, nested.retryable === true);
    }
    if (nativeCode.length > 0) {
      return new DiaryError(NATIVE_UNAVAILABLE, `${nativeCode}: ${message}`);
    }
    return new DiaryError(NATIVE_UNAVAILABLE, message);
  }

  /** hilog 友好的一行（`code=` 在前，便于 grep）。 */
  toLogLine(): string {
    const retry = this.retryable ? 'true' : 'false';
    return `DiaryError code=${this.code} retryable=${retry} message=${this.message}`;
  }
}

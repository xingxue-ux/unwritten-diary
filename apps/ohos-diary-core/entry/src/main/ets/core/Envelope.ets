// C ABI 的请求/响应信封（`docs/architecture/m6-核心-C-ABI.md` §4.2–4.3）。
//
// 这里只做两件事：拼请求 JSON、拆响应信封。**不翻译字段名**（snake_case 原样）、
// 不重排 data（核心 serde 出来的就是最终形状）。
//
// 响应：
//   {"apiVersion":"1.0","data":{…},"ok":true}
//   {"apiVersion":"1.0","error":{"code":"…","message":"…","retryable":false},"ok":false}

import { DiaryError, ErrorBody, NATIVE_UNAVAILABLE } from './DiaryError';

/** 请求信封（`params` 省略时给 `null`，C 侧把 null 与 {} 同等对待）。 */
interface RequestEnvelope {
  method: string;
  params: Object | null;
}

/** 响应信封。字段全部可能缺省：先按「未知」收下，再逐个校验。 */
interface ResponseEnvelope {
  apiVersion?: string;
  ok?: boolean;
  data?: Object | null;
  error?: ErrorBody;
}

/** 截断过长的原文，避免把一整条 JSON 塞进错误 message。 */
function preview(text: string): string {
  const LIMIT = 200;
  return text.length <= LIMIT ? text : `${text.substring(0, LIMIT)}…（共 ${text.length} 字符）`;
}

/** `{"method":"…","params":{…}}`。`params` 为 null 时也显式写出来（C 侧两种都收）。 */
export function buildRequest(method: string, params: Object | null): string {
  const request: RequestEnvelope = { method: method, params: params };
  return JSON.stringify(request);
}

/**
 * 拆信封：
 * - `ok:true` 且有 `data` ⇒ 返回 data；
 * - `ok:true` 且 `data` 为 null（无返回值的方法）⇒ 返回 null；
 * - `ok:false` ⇒ 抛 `DiaryError`（code/message/retryable/apiVersion 原样带出）；
 * - 不是合法 JSON / 形状不对 ⇒ 抛 `native_unavailable`（说明 shim 或 C ABI 坏了，不是业务错误）。
 */
export function unwrapEnvelope(text: string): Object | null {
  let envelope: ResponseEnvelope;
  try {
    envelope = JSON.parse(text) as ResponseEnvelope;
  } catch (e) {
    throw new DiaryError(NATIVE_UNAVAILABLE, `核心返回的不是合法 JSON：${preview(text)}`);
  }
  if (envelope === null || envelope === undefined) {
    throw new DiaryError(NATIVE_UNAVAILABLE, `核心返回了 null 信封：${preview(text)}`);
  }
  const apiVersion: string = envelope.apiVersion !== undefined ? envelope.apiVersion : '';
  if (envelope.ok === true) {
    if (envelope.data === undefined || envelope.data === null) {
      return null;
    }
    return envelope.data;
  }
  const body = envelope.error;
  if (body === undefined || body === null) {
    throw new DiaryError(NATIVE_UNAVAILABLE, `核心返回 ok:false 但没有 error：${preview(text)}`);
  }
  throw new DiaryError(body.code, body.message, body.retryable === true, apiVersion);
}

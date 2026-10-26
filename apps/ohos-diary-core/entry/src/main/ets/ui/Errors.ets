// 界面层唯一的错误出口。
//
// 规则（#1019 / 文档 §3.4）：
// - **按错误码映射文案**，不按 `runtimeType`、不按 message 猜原因；
// - C ABI / DiaryCore 的异常（`DiaryError`）与原生层失败都先归一成 `CoreErrorInfo`，
//   再由 `toUiError()` 变成界面能直接渲染的 `UiError`；
// - 页面拿到的是 `UiError`，**不把 `DiaryError` 抛给 UI**。
//
// 这里只做「码 → 文案」，不改核心码、不新增码；`retryable` 也原样来自核心，
// 界面不要自己判断「看起来可以再试」。

import { DiaryError } from '../core/DiaryError';

/** 核心 / 宿主错误的线上形状（与 Envelope.error 一致）。 */
export interface CoreErrorInfo {
  code: string;
  message: string;
  retryable: boolean;
}

/** 界面直接渲染的错误：标题与简洁的操作建议。 */
export interface UiError {
  code: string;
  title: string;
  message: string;
  hint: string;
  retryable: boolean;
  /** 是不是「调用没有发生」的宿主问题（打包/部署），不是业务失败。 */
  native: boolean;
}

/** 空错误哨兵：`code === ''` 表示没有错误。 */
export const EMPTY_UI_ERROR: UiError = {
  code: '',
  title: '',
  message: '',
  hint: '',
  retryable: false,
  native: false,
};

/** 核心码 → 用户文案。**只用 code 分支**。 */
export function toUiError(info: CoreErrorInfo): UiError {
  const code: string = info.code;
  const retryable: boolean = info.retryable;
  const native: boolean = code === 'native_unavailable';
  let title: string = '操作未完成';
  let hint: string = '请稍后重试。';
  switch (code) {
    case 'not_found': title = '内容已不存在'; hint = '请返回列表刷新。'; break;
    case 'invalid_state': title = '暂时无法操作'; hint = '请刷新后再试。'; break;
    case 'invalid_argument': title = '填写内容有误'; hint = '请检查后重试。'; break;
    case 'revision_conflict': title = '内容已更新'; hint = '请刷新后再保存。'; break;
    case 'idempotency_conflict': title = '操作未完成'; hint = '请重新操作。'; break;
    case 'storage_full': title = '存储空间不足'; hint = '请清理空间后重试。'; break;
    case 'asset_missing': title = '原始材料已丢失'; hint = '已有文字仍可查看。'; break;
    case 'permission_denied': title = '缺少必要权限'; hint = '请在系统设置中授权。'; break;
    case 'integrity_failed': title = '材料不完整'; hint = '请重新选择材料。'; break;
    case 'unsupported_format': title = '暂不支持此格式'; hint = '请使用其他格式。'; break;
    case 'search_expired': case 'cursor_expired': title = '搜索结果已更新'; hint = '请重新搜索。'; break;
    case 'provider_not_configured': title = '本机模型尚未就绪'; hint = '请重新打开应用，等待模型准备完成。'; break;
    case 'authentication_failed': title = '本机配置需要更新'; hint = '请重新打开应用。'; break;
    case 'network_unavailable': title = '此版本仅支持本机处理'; hint = '请使用应用内置的本机功能。'; break;
    case 'rate_limited': title = '正在处理其他任务'; hint = '请稍后再试。'; break;
    case 'quota_exceeded': title = '整理次数已达上限'; hint = '请稍后再整理。'; break;
    case 'provider_timeout': title = '本机处理超时'; hint = '请稍后重试。'; break;
    case 'model_unavailable': title = '本机模型暂不可用'; hint = '请重试，或重新打开应用。'; break;
    case 'corrupted_backup': title = '备份无法读取'; hint = '请选择其他备份。'; break;
    case 'incompatible_backup': title = '备份版本不兼容'; hint = '请更新应用后再试。'; break;
    case 'native_unavailable': title = '应用暂时无法使用'; hint = '请重新打开应用。'; break;
    default: break;
  }
  return {
    code: code,
    title: title,
    // 原始错误仅留在调用结果中，展示层不回显路径、接口名或服务响应。
    message: '',
    hint: hint,
    retryable: retryable,
    native: native,
  };
}

/**
 * 把任意抛出的对象变成 `UiError`。
 *
 * 只在这里碰 `DiaryError`；页面不再 import 它。
 */
export function uiErrorFromThrown(raw: Object): UiError {
  let err: DiaryError;
  if (raw instanceof DiaryError) {
    err = raw;
  } else {
    err = DiaryError.fromThrown(raw);
  }
  return toUiError({
    code: err.code,
    message: err.message,
    retryable: err.retryable,
  });
}

/** 给网关兜底用的「调用没有完成」错误。 */
export function gatewayError(message: string): UiError {
  return toUiError({
    code: 'native_unavailable',
    message: message,
    retryable: false,
  });
}

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

/** 界面直接渲染的错误：标题、原样原因、下一步建议。 */
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
  let title: string = '操作没有完成';
  let hint: string = '如果反复出现，请到「设置 → 诊断」查看版本与能力信息。';
  switch (code) {
    case 'not_found':
      title = '没有找到这条内容';
      hint = '它可能已被清理或删除；返回列表刷新后再试。';
      break;
    case 'invalid_state':
      title = '当前状态不允许这个操作';
      hint = '数据可能已在别处更新；刷新后按当前状态操作，不要直接重试。';
      break;
    case 'invalid_argument':
      title = '请求参数不合法';
      hint = '可能是界面与核心版本不匹配；请到「设置 → 诊断」核对版本。';
      break;
    case 'revision_conflict':
      title = '内容已被修改';
      hint = '你读到的是旧版本；刷新后重新查看，再决定是否保存。';
      break;
    case 'idempotency_conflict':
      title = '同一次操作的内容变了';
      hint = '这次提交不能安全重放；请重新发起一次新的操作。';
      break;
    case 'storage_full':
      title = '存储空间不足';
      hint = '本次保存/录音没有完成；清理空间后再试，不要立刻重试。';
      break;
    case 'asset_missing':
      title = '原件不在';
      hint = '这条内容引用的文件已被移动或删除；已有文字仍可查看。';
      break;
    case 'permission_denied':
      title = '没有权限';
      hint = '到系统设置授权后返回；不要反复弹权限框。';
      break;
    case 'integrity_failed':
      title = '材料不完整';
      hint = '哈希或字节数对不上，核心没有保存；保留暂存并重新选择材料，不要自动重试。';
      break;
    case 'unsupported_format':
      title = '格式暂不支持';
      hint = '文件已保留，但正文还不能提取；目前只能按文件名和时间找到它。';
      break;
    case 'search_expired':
      title = '搜索已过期';
      hint = '索引在查询期间变了；重新发起同一查询。';
      break;
    case 'cursor_expired':
      title = '翻页位置已过期';
      hint = '结果集换代后旧游标不能再用；从第一页重新搜索/加载。';
      break;
    case 'provider_not_configured':
      title = '还没有配置好';
      hint = '需要先配置转写/语义/AI 服务；到设置页完成配置后再试。';
      break;
    case 'authentication_failed':
      title = '认证失败';
      hint = '服务拒绝了当前凭据；检查密钥或重新配置，不要重试。';
      break;
    case 'network_unavailable':
      title = '网络不可用';
      hint = '材料已保存在本机；任务会自动退避重试，也可以等网络恢复后手动运行。';
      break;
    case 'rate_limited':
      title = '请求太频繁';
      hint = '服务正在限流；稍后再试，不要连点。';
      break;
    case 'quota_exceeded':
      title = '额度不足';
      hint = '服务额度已经用完；处理后才能继续，重试不会成功。';
      break;
    case 'provider_timeout':
      title = '服务超时';
      hint = '这次调用没有在预期时间内返回；可以有限次重试。';
      break;
    case 'model_unavailable':
      title = '模型不可用';
      hint = '模型文件或运行库不对；关键词检索仍可用。到设置页核对模型状态，不要只重试。';
      break;
    case 'corrupted_backup':
      title = '备份损坏';
      hint = '备份包无法读取；换一份备份，不要覆盖现有资料库。';
      break;
    case 'incompatible_backup':
      title = '备份版本不兼容';
      hint = '这份备份来自不兼容的版本；使用对应版本的备份。';
      break;
    case 'native_unavailable':
      title = '核心库没有加载';
      hint = '这次调用没有发生（打包/部署问题，不是操作错误）；到「设置 → 诊断」查看原生库与版本。';
      break;
    default:
      break;
  }
  return {
    code: code,
    title: title,
    // 核心 message 是用户可读的，原样带出；空时用标题兜底。
    message: info.message.length > 0 ? info.message : title,
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

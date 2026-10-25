import { JobRunSummary } from '../core/Models';
import { toUiError } from './Errors';

/** Ordinary labels stay Chinese; model configuration retains exact model IDs. */
export function presetLabel(label: string): string {
  const value: string = label.toLowerCase();
  if (value.includes('deepseek')) { return '深度求索'; }
  if (value.includes('openai')) { return '兼容服务'; }
  if (value.includes('ollama')) { return '本机服务'; }
  return '其他服务';
}

export function modelLabel(model: string | null): string {
  if (model === null || model.length === 0) { return '未选择模型'; }
  if (model === 'deepseek-flash' || model === 'deepseek-chat') { return `快速模型（${model}）`; }
  if (model === 'deepseek-v4-pro') { return `专业模型（${model}）`; }
  if (model === 'deepseek-reasoner') { return `推理模型（${model}）`; }
  return model;
}

export function testStatusLabel(status: string): string {
  if (status === 'ok') { return '检查通过'; }
  if (status === 'failed') { return '检查未通过'; }
  if (status === 'untested') { return '尚未检查'; }
  return '状态未知';
}

export function timeZoneLabel(zone: string): string {
  if (zone === 'Asia/Shanghai' || zone === 'Asia/Chongqing' || zone === 'Asia/Hong_Kong') {
    return '中国标准时间';
  }
  if (zone === 'UTC' || zone === 'Etc/UTC') { return '世界协调时间'; }
  return '自定时区';
}

export function errorSummary(code: string | null): string {
  return toUiError({ code: code === null ? '' : code, message: '', retryable: false }).title;
}

export function runSummary(summary: JobRunSummary): string {
  if (summary.processed === 0) {
    return summary.due_backlog > 0 ? '还有任务等待处理。' : '本轮未处理任务。';
  }
  let text: string = `已完成 ${summary.succeeded} 项`;
  if (summary.failed > 0) { text += `，未完成 ${summary.failed} 项`; }
  if (summary.retrying > 0) { text += `，稍后重试 ${summary.retrying} 项`; }
  if (summary.due_backlog > 0) { text += `，待处理 ${summary.due_backlog} 项`; }
  return text + '。';
}

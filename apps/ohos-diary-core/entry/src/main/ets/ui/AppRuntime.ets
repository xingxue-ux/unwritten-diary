// 页面公用的运行时工具：路径、时间、格式化、状态文案、toast/确认框。
//
// 这里**不做核心调用**，也不缓存核心状态；纯函数 + 平台小工具，方便单测/复查。
// 颜色不在这里写死：颜色由 `ui/Common.ets` 里的组件用系统资源决定。

import { common } from '@kit.AbilityKit';
import { promptAction } from '@kit.ArkUI';
import { appFilesDir } from '../session/AppDirectory';

/** 资料库文件的绝对路径（与 DiaryCore.openInDirectory 同一口径）。 */
export function databasePath(context: common.UIAbilityContext): string {
  const dir: string = appFilesDir(context);
  return `${dir}/library.sqlite`;
}

function pad2(value: number): string {
  return value < 10 ? `0${value}` : `${value}`;
}

/** 本地时区的 `YYYY-MM-DD`（captures.list 的 day_key 口径）。 */
export function dayKeyOf(date: Date): string {
  return `${date.getFullYear()}-${pad2(date.getMonth() + 1)}-${pad2(date.getDate())}`;
}

/** 相对今天偏移 N 天的 day_key（0 = 今天，-1 = 昨天）。 */
export function dayKeyAtOffset(offset: number): string {
  const date: Date = new Date();
  date.setDate(date.getDate() + offset);
  return dayKeyOf(date);
}

/** 日期的人话标签：今天 / 昨天 / 10月17日 周五。 */
export function dayLabelAtOffset(offset: number): string {
  const date: Date = new Date();
  date.setDate(date.getDate() + offset);
  if (offset === 0) {
    return '今天';
  }
  if (offset === -1) {
    return '昨天';
  }
  const weekdays: string[] = ['周日', '周一', '周二', '周三', '周四', '周五', '周六'];
  return `${date.getMonth() + 1}月${date.getDate()}日 ${weekdays[date.getDay()]}`;
}

/** 完整日期（详情页标题用）。 */
export function dayTitleAtOffset(offset: number): string {
  const date: Date = new Date();
  date.setDate(date.getDate() + offset);
  return `${date.getFullYear()}年${date.getMonth() + 1}月${date.getDate()}日`;
}

/** RFC 3339 → 本地 `HH:mm`；解析不了就原样返回，不猜。 */
export function formatTime(iso: string): string {
  const date: Date = new Date(iso);
  if (isNaN(date.getTime())) {
    return '时间未知';
  }
  return `${pad2(date.getHours())}:${pad2(date.getMinutes())}`;
}

/** RFC 3339 → 本地 `YYYY-MM-DD HH:mm`。 */
export function formatDateTime(iso: string): string {
  const date: Date = new Date(iso);
  if (isNaN(date.getTime())) {
    return '时间未知';
  }
  return `${dayKeyOf(date)} ${pad2(date.getHours())}:${pad2(date.getMinutes())}`;
}

/** 毫秒 → 人话时长；保留一位小数，不做毫秒级承诺。 */
export function formatDuration(ms: number): string {
  const safe: number = ms < 0 ? 0 : ms;
  if (safe < 60000) {
    return `${(safe / 1000).toFixed(1)} 秒`;
  }
  const minutes: number = Math.floor(safe / 60000);
  const seconds: number = Math.floor((safe % 60000) / 1000);
  return `${minutes} 分 ${seconds} 秒`;
}

/** 字节 → 人话体积。 */
export function formatBytes(bytes: number): string {
  if (bytes < 1024) {
    return `${bytes} 字节`;
  }
  if (bytes < 1024 * 1024) {
    return `${(bytes / 1024).toFixed(1)} 千字节`;
  }
  if (bytes < 1024 * 1024 * 1024) {
    return `${(bytes / (1024 * 1024)).toFixed(1)} 兆字节`;
  }
  return `${(bytes / (1024 * 1024 * 1024)).toFixed(2)} 吉字节`;
}

/** 幂等键：同一个用户动作复用一个；新的动作换新的。 */
export function newOperationId(prefix: string): string {
  const random: number = Math.floor(Math.random() * 1000000);
  return `${prefix}-${Date.now()}-${random}`;
}

/**
 * 记录状态 → 徽标文案（契约 §4.2 的 `state.*` 槽位）。
 *
 * `draft` 表示保存流程尚未完成，用「待保存」说明；不要求用户补全内容；
 * `trashed` 是「**已移入回收站**」，不是「已删除」（它是可逆状态，写「已删除」会诱发误清理）。
 */
export function captureStateLabel(state: string): string {
  if (state === 'draft') {
    return '待保存';
  }
  if (state === 'committed') {
    return '已保存';
  }
  if (state === 'trashed') {
    return '已移入回收站';
  }
  return '状态未知';
}

/** 任务状态 → 文案（与核心 9 个 JobState 一一对应）。 */
export function jobStateLabel(state: string): string {
  if (state === 'queued') {
    return '排队中';
  }
  if (state === 'running') {
    return '处理中';
  }
  if (state === 'succeeded') {
    return '已完成';
  }
  if (state === 'retry_wait') {
    return '等待自动重试';
  }
  if (state === 'waiting_configuration') {
    return '需要配置';
  }
  if (state === 'waiting_dependency') {
    return '等待前一步';
  }
  if (state === 'waiting_network') {
    return '等待网络';
  }
  if (state === 'failed') {
    return '未完成';
  }
  if (state === 'cancelled') {
    return '已取消';
  }
  return '状态未知';
}

/** 任务类型 → 领域文案；未知 kind 原样显示，不猜。 */
export function jobKindLabel(kind: string): string {
  if (kind === 'index') {
    return '关键词索引';
  }
  if (kind === 'semantic_index') {
    return '语义索引';
  }
  if (kind === 'transcribe') {
    return '转写';
  }
  if (kind === 'extract') {
    return '正文提取';
  }
  if (kind === 'respond') {
    return '智能整理';
  }
  if (kind === 'import') {
    return '导入';
  }
  if (kind === 'diary_generate') { return '日记整理'; }
  if (kind === 'export') { return '阅读导出'; }
  if (kind === 'backup') { return '资料备份'; }
  return '其他任务';
}

/** 任务状态对应的视觉语气：ok / warn / alert / active / muted。 */
export function jobStateTone(state: string): string {
  if (state === 'succeeded') {
    return 'ok';
  }
  if (state === 'waiting_configuration') {
    return 'warn';
  }
  if (state === 'failed') {
    return 'alert';
  }
  if (state === 'running') {
    return 'active';
  }
  return 'muted';
}

/** 抽取状态（ExtractedContent.status）→ 文案。 */
export function extractStatusLabel(status: string): string {
  if (status === 'pending') {
    return '待处理';
  }
  if (status === 'processing') {
    return '转写中';
  }
  if (status === 'ready') {
    return '可阅读';
  }
  if (status === 'partial') {
    return '部分完成';
  }
  if (status === 'failed') {
    return '处理失败';
  }
  if (status === 'stale') {
    return '需要更新';
  }
  return '状态未知';
}

/** 抽取状态 → 语气。 */
export function extractStatusTone(status: string): string {
  if (status === 'ready') {
    return 'ok';
  }
  if (status === 'partial' || status === 'stale') {
    return 'warn';
  }
  if (status === 'failed') {
    return 'alert';
  }
  if (status === 'processing') {
    return 'active';
  }
  return 'muted';
}

/** 覆盖程度 → 文案。 */
export function coverageLabel(coverage: string): string {
  if (coverage === 'complete') {
    return '完整';
  }
  if (coverage === 'partial') {
    return '覆盖不全';
  }
  if (coverage === 'metadata_only') {
    return '仅文件信息';
  }
  if (coverage === 'unavailable') {
    return '不可用';
  }
  return '状态未知';
}

/** 日记版本的来源 → 文案（`DiaryVersion.origin` 的线上字符串）。 */
export function diaryOriginLabel(origin: string): string {
  if (origin === 'generated') {
    return '整理生成';
  }
  if (origin === 'user_edit') {
    return '手写编辑';
  }
  if (origin === 'merge') {
    return '合并';
  }
  return '其他来源';
}

/** 日记版本来源的语气（只影响徽标文字颜色）。 */
export function diaryOriginTone(origin: string): string {
  if (origin === 'user_edit') {
    return 'ok';
  }
  if (origin === 'merge') {
    return 'active';
  }
  return 'muted';
}

/** 检索命中的来源类型 → 文案。 */
export function hitSourceKindLabel(kind: string): string {
  if (kind === 'text') {
    return '文字';
  }
  if (kind === 'audio') {
    return '语音';
  }
  if (kind === 'image') {
    return '图片';
  }
  if (kind === 'video') {
    return '视频';
  }
  if (kind === 'file') {
    return '文件';
  }
  if (kind === 'link') {
    return '链接';
  }
  return '其他';
}

/** 命中原因 → 徽标文案。 */
export function matchedByLabel(matchedBy: string): string {
  if (matchedBy === 'keyword') {
    return '关键词';
  }
  if (matchedBy === 'semantic') {
    return '语义';
  }
  if (matchedBy === 'metadata') {
    return '材料信息';
  }
  return '相关内容';
}

/** 检索阶段 → 文案；**phase 不是结束标志，翻页看 cursor**。 */
export function searchPhaseLabel(phase: string): string {
  if (phase === 'initial') {
    return '准备中';
  }
  if (phase === 'keyword_ready') {
    return '关键词已就绪';
  }
  if (phase === 'hybrid_ready') {
    return '混合结果已就绪';
  }
  if (phase === 'done') {
    return '已完成';
  }
  if (phase === 'cancelled') {
    return '已取消';
  }
  return '状态未知';
}

/** 轻提示。失败只吞掉：提示失败不该影响主流程。 */
export function toast(message: string): void {
  try {
    promptAction.showToast({ message: message, duration: 2200 });
  } catch (e) {
    // 忽略
  }
}

/** 二次确认框；返回 true 表示用户点了「确定」。 */
export async function confirmAction(title: string, message: string, confirmText: string): Promise<boolean> {
  try {
    const result = await promptAction.showDialog({
      title: title,
      message: message,
      buttons: [
        { text: '返回', color: $r('sys.color.ohos_id_color_text_primary') },
        { text: confirmText, color: $r('sys.color.ohos_id_color_emphasize') },
      ],
    }) as promptAction.ShowDialogSuccessResponse;
    return result.index === 1;
  } catch (e) {
    return false;
  }
}

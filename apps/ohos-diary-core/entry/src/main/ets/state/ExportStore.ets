// 阅读导出的数据层（契约 4.4 的 `exports.create`）。
//
// 这一层不碰 UI：不 import 任何 ArkUI 组件，页面只把公开字段镜像到自己的 `@State`。
// 风格与 `state/SettingsStore.ets` 一致：字段 + `refresh()` + 写操作。
//
// 四条硬口径：
//
// 1. **`exports.create` 是同步的**：返回时文件已经写完。返回的 `jobId` 指向一条已经
//    `succeeded` 的导出任务记录，**不是排队中的任务**——所以这里没有轮询、没有
//    `jobs.get`、也没有「进度」可以报。不要照搬「先入队再等」的界面结构。
// 2. **产物是绝对路径**：`path`（以及平台保存句柄 `packageTicket`，本构建里就是同一个
//    路径）由核心给，落在资料库自己的 `exports/` 下，重名自动加后缀、绝不覆盖。
//    文件系统是数据层之外的事，本 store 不碰 `@kit.CoreFileKit`。
// 3. **`restorable` 恒为 `false`**：文件里只有当前日记正文（没有数据库、没有原件、
//    没有索引），**这不是备份**，不能用于恢复资料库。界面不能写「备份已导出」。
// 4. **OHOS 上资料库必须在应用沙箱内**，否则核心报 `permission_denied`。本 store 把
//    这个码翻成「把资料库放到沙箱目录」的具体说明，而不是一句「操作没有完成」。
//
// `includeAssets = true` 会被核心以 `invalid_argument` 明确拒绝（本构建只写正文）。
// 本 store 在本地就拦住它并给出同样意思的说明——不是静默忽略，也不是另一套规则。

import { CreateExportParams, ExportRequest, ExportTicket } from '../core/Models';
import { CoreGateway, CoreResult, uiErrorOf } from '../ui/CoreGateway';
import { EMPTY_UI_ERROR, UiError } from '../ui/Errors';

/**
 * 阅读导出状态容器。
 *
 * 用法（页面侧）：
 * ```ts
 * const store = new ExportStore(dbPath);
 * const ticket = await store.createExport({ fromDayKey: '2026-10-01', toDayKey: '2026-10-21' });
 * if (ticket !== null) {
 *   this.path = ticket.path;          // 绝对路径，文件已经写完
 *   this.message = store.message;     // 已写明「这不是备份」
 * }
 * ```
 *
 * `refresh()` 只清错误、不清票据：`exports.create` 没有读接口（C ABI 就这一条），
 * 上一次的 `ticket` 已经是最终结果，不需要也不可能「再读一次」。
 */
export class ExportStore {
  /** 最近一次成功的导出票据（核心原样）；`null` = 还没导出过。 */
  ticket: ExportTicket | null = null;
  /** 正在读（本 store 没有读接口，保留字段与其它 store 同形）。 */
  loading: boolean = false;
  /** 正在导出。 */
  working: boolean = false;
  /** 失败时核心或宿主的原话（成功时是空哨兵）。 */
  error: UiError = EMPTY_UI_ERROR;
  /** 最近一次操作的结果说明（成功时包含绝对路径与「不是备份」的提醒）。 */
  message: string = '';

  private dbPath: string = '';

  constructor(dbPath: string) {
    this.dbPath = dbPath;
  }

  /**
   * 清掉上一次的错误与提示，**保留已经完成的票据**。
   *
   * 为什么不是「重读」：C ABI 里 `exports` 只有 `create`，没有 `exports.get`；而且
   * `create` 是同步的，返回时任务已经是 `succeeded`。重新发一次 `create` 会**再写一个
   * 新文件**（重名加后缀、不覆盖），那是新的一次导出，不是刷新。
   */
  async refresh(): Promise<boolean> {
    if (this.loading || this.working) {
      return false;
    }
    this.error = EMPTY_UI_ERROR;
    return true;
  }

  /**
   * 把日期范围内的**当前日记正文**导成 Markdown / 纯文本（`exports.create`）。
   *
   * 成功返回票据（文件已写完、`path` 是绝对路径、`restorable === false`），并把
   * 「这不是备份」写进 `message`；失败把核心原话留在 `error`，返回 `null`。
   *
   * 没有输出路径参数：契约与核心的 `ExportRequest` 都没有路径字段，产物固定落在
   * 资料库的 `exports/` 下。想控制落点请用 `backups.*`（那不是本 store 的职责）。
   */
  async createExport(request: ExportRequest): Promise<ExportTicket | null> {
    if (this.working || this.loading) {
      return null;
    }
    const from: string = request.fromDayKey.trim();
    const to: string = request.toDayKey.trim();
    if (from.length === 0 || to.length === 0) {
      this.message = '请选择开始和结束日期。';
      return null;
    }
    // **不能直接比较原始字符串**：核心是「先 parse 日期、再按规范形比较」
    // （`crates/diary_core/src/export.rs`），所以 `2026-10-5` 是合法起点；但 JS/ArkTS 里
    // `'2026-10-5' > '2026-10-15'` 为 true（第 9 位 `'5' > '1'`），直接比会把合法范围
    // 拒掉，还告诉用户「开始日期不能晚于结束日期」——理由与事实相反。
    // 本地只做「能规范化时才比」：两边都能补成 `YYYY-MM-DD` 的前提下，字典序才等于
    // 时间序。形状不合法（`2026-10`、`abc`、月份越界）时**不做本地判断**，原样交给
    // 核心报 `invalid_argument`——本地不替核心编另一套日期规则。
    const normalizedFrom: string | null = normalizeDayKey(from);
    const normalizedTo: string | null = normalizeDayKey(to);
    if (normalizedFrom !== null && normalizedTo !== null && normalizedFrom > normalizedTo) {
      this.message = '开始日期不能晚于结束日期；没有生成文件。';
      return null;
    }
    if (request.includeAssets === true) {
      this.message = '暂不支持导出原始材料。';
      return null;
    }
    // 只转发核心认识的字段；`includeAssets` 明确写 false，与核心默认一致，
    // 也避免「省略」在别的实现里被误读成 true。
    const clean: ExportRequest = { fromDayKey: from, toDayKey: to, includeAssets: false };
    if (request.format !== undefined) {
      clean.format = request.format;
    }
    const params: CreateExportParams = { request: clean };
    this.working = true;
    this.error = EMPTY_UI_ERROR;
    this.message = '';
    const result: CoreResult = await CoreGateway.call(this.dbPath, 'exports.create', params);
    this.working = false;
    if (!result.ok) {
      this.applyFailure(result);
      return null;
    }
    const ticket: ExportTicket = result.data as ExportTicket;
    this.ticket = ticket;
    this.message = describeExport(ticket);
    return ticket;
  }

  // ---------------------------------------------------------------- 内部

  /** 按错误码给导出场景的具体说明；`error` 本身保持核心映射（标题/建议统一）。 */
  private applyFailure(result: CoreResult): void {
    const error: UiError = uiErrorOf(result);
    this.error = error;
    if (error.code === 'permission_denied') {
      this.message = '无法保存导出副本，请重新打开应用后再试。';
    } else if (error.code === 'not_found') {
      this.message = '所选日期没有可导出的日记。';
    } else if (error.code === 'invalid_argument') {
      this.message = '请检查日期范围后重试。';
    } else if (error.code === 'storage_full') {
      this.message = '存储空间不足，导出没有完成。';
    } else {
      this.message = '导出没有完成，资料库没有变化。';
    }
  }
}

/**
 * 把 `YYYY-M-D` 补零成 `YYYY-MM-DD`；形状不合法或日期越界时返回 `null`。
 *
 * 为什么要补零：核心的 `parse_day_key` 用 chrono 的 `%Y-%m-%d` 解析，接受
 * `2026-10-5` / `2026-1-5`，再按 `%Y-%m-%d` 的规范形绑定 SQL。客户端只有补成定长
 * 之后，字典序才等于时间序。返回 `null` 的两种情况都**不在本地下结论**：
 * - 不是 `YYYY-M-D` 三段（例如 `2026/10/05`、`2026-10`）——核心会报 `invalid_argument`；
 * - 月/日越界（例如 `2026-13-01`）——同样交给核心报错，本地不猜「哪天算合法」。
 *
 * 年份只接受 4 位数字（dayKey 的口径就是 `YYYY-MM-DD`）；月日接受 1–2 位。
 */
export function normalizeDayKey(dayKey: string): string | null {
  const text: string = dayKey.trim();
  const parts: string[] = text.split('-');
  if (parts.length !== 3) {
    return null;
  }
  const year: string = parts[0];
  const month: string = parts[1];
  const day: string = parts[2];
  if (year.length !== 4 || !isDigits(year) ||
    month.length < 1 || month.length > 2 || !isDigits(month) ||
    day.length < 1 || day.length > 2 || !isDigits(day)) {
    return null;
  }
  const monthValue: number = parseInt(month, 10);
  const dayValue: number = parseInt(day, 10);
  if (monthValue < 1 || monthValue > 12 || dayValue < 1 || dayValue > 31) {
    return null;
  }
  const paddedMonth: string = monthValue < 10 ? `0${monthValue}` : `${monthValue}`;
  const paddedDay: string = dayValue < 10 ? `0${dayValue}` : `${dayValue}`;
  return `${year}-${paddedMonth}-${paddedDay}`;
}

function isDigits(value: string): boolean {
  if (value.length === 0) {
    return false;
  }
  for (let i = 0; i < value.length; i++) {
    const ch: string = value.charAt(i);
    if (ch < '0' || ch > '9') {
      return false;
    }
  }
  return true;
}

/**
 * 导出结果的人话：天数、字节数、绝对路径，以及「这不是备份」。
 *
 * 如果核心哪天回了 `restorable = true`，这里如实指出与既有口径不符——不替核心圆场。
 */
function describeExport(ticket: ExportTicket): string {
  return ticket.restorable ? '导出类型异常，请更新应用后重试。'
    : `已生成 ${ticket.dayCount} 天日记的阅读副本，请选择保存位置。`;
}

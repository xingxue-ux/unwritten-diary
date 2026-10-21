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
      this.message = '导出的开始日期和结束日期都不能为空（YYYY-MM-DD）；没有生成文件。';
      return null;
    }
    // `YYYY-MM-DD` 定长、字典序与时间序一致，可以直接比较；形状不对的由核心校验。
    if (from > to) {
      this.message = '开始日期不能晚于结束日期；没有生成文件。';
      return null;
    }
    if (request.includeAssets === true) {
      this.message = '这次导出只写日记正文，不随附原件（includeAssets 必须为 false）；没有生成文件。';
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
      this.message = '资料库不在应用沙箱内（或导出目录不可写），核心拒绝写文件；' +
        '把资料库放到应用沙箱目录后再导出。这次没有生成文件。';
    } else if (error.code === 'not_found') {
      this.message = '这个日期范围内没有可导出的当前日记正文；没有生成文件。';
    } else if (error.code === 'invalid_argument') {
      this.message = '导出参数不合法（日期范围或格式），核心没有写文件。';
    } else if (error.code === 'storage_full') {
      this.message = '存储空间不足，导出没有完成。';
    } else {
      this.message = '导出没有完成，资料库没有变化。';
    }
  }
}

/**
 * 导出结果的人话：天数、字节数、绝对路径，以及「这不是备份」。
 *
 * 如果核心哪天回了 `restorable = true`，这里如实指出与既有口径不符——不替核心圆场。
 */
function describeExport(ticket: ExportTicket): string {
  let message: string = `导出完成：${ticket.dayCount} 天、${ticket.byteSize} 字节；` +
    `文件：${ticket.path}。`;
  if (ticket.packageTicket === null) {
    message += '平台保存句柄为空，但文件已经落盘在资料库的 exports/ 下。';
  }
  if (ticket.restorable) {
    message += '（注意：核心返回 restorable=true，与「阅读导出不能恢复资料库」的既有口径不符，请核对版本。）';
  } else {
    message += '这不是备份（restorable=false）：文件里只有当前日记正文，不能用来恢复资料库。';
  }
  return message;
}
// 回收站的数据层（契约 4.1 / §8：`captures.restore` / `captures.previewPurge` /
// `captures.purge`，以及用 `captures.list` **自己筛**出回收站里的那一部分）。
//
// 这一层不碰 UI：不 import 任何 ArkUI 组件，页面只把这里公开的字段镜像到自己的
// `@State` 上。风格与 `state/SettingsStore.ets` 一致：构造时给 `dbPath`，字段 +
// `refresh()` + 写操作，失败把核心原话留在 `error` 里，**不假装成功**。
//
// 三条必须记住的硬口径（每条都有实测教训）：
//
// 1. **`captures.list` 不按状态过滤**：核心把草稿、已提交、回收站里的记录一起返回。
//    所以回收站必须自己筛 `state === 'trashed'`（[`selectTrashedCaptures`]），
//    主列表必须**排除** `trashed`（[`selectActiveCaptures`]）。「从列表里消失」不是
//    契约承诺，界面文案不要这么写：记录仍然在库里，只是状态变了。
//
// 2. **`purge` 的令牌流**：先 `previewPurge` 拿 `preview_token`，再 `purge`。令牌一次性、
//    默认 10 分钟过期、绑定 `capture_id`；预览之后清单变了核心报 `invalid_state`。
//    **旧令牌绝不重试**——本 store 在 `invalid_state` 时当场作废旧令牌，并重新**预览**
//    （只读），让用户核对新的清单后再确认；绝不用旧令牌再发一次 `purge`。
//
// 3. **三类版本清单原样暴露**（「界面不许说谎」的硬约束）：`preview` 与 `lastPurge` 就是
//    核心 serde 的原样对象，本 store 不归并、不过滤、不改名：
//    - `diary_versions_still_referencing`：有可判定关联的版本，`delete_diary_versions = true`
//      时才删；默认 `false` = **保留**（一次生成通常覆盖同一天多条记录，删 A 会连带删掉
//      含 B 内容的日记，所以默认保留）；
//    - `derived_diary_version_ids`：由这些版本派生/合并来的版本，**一个都不删**，
//      但正文可能仍含这条记录的文字，必须如实告诉用户「它们还在」；
//    - `undetermined_diary_version_ids`：归属判不出来的版本，**不删，也不许假装没有**。
//
// 另外：`purge` 之后 `retained_source_snapshot_ids` 里的快照正文仍可能含这条记录的文字
// （#161 方案 A）。`lastPurge` 把那份清单原样留着，界面的「已永久清理」不能写成
// 「所有副本里的字节都消失了」。

import {
  Capture,
  CaptureIdParams,
  CapturePage,
  ListCapturesParams,
  PurgeCaptureParams,
  PurgePreview,
  PurgeResult,
  TrashCaptureParams,
} from '../core/Models';
import { CoreGateway, CoreResult, uiErrorOf } from '../ui/CoreGateway';
import { EMPTY_UI_ERROR, UiError } from '../ui/Errors';

/** 核心 `Capture.state` 的回收站取值（`draft` / `committed` / `trashed`）。 */
export const CAPTURE_STATE_TRASHED: string = 'trashed';

/** 每页条数：核心把 `captures.list` 的 `limit` 夹在 1–100。 */
const LIST_PAGE_SIZE: number = 100;

/**
 * 翻页上限（防御）：回收站条目多时也要有界。
 *
 * 100 页 × 100 条 = 1 万条。超过就停下并如实说「没扫完」，而不是悄悄截断。
 */
const MAX_LIST_PAGES: number = 100;

/** 这一条是不是回收站里的。**只按 `state` 判断**，不猜别的字段。 */
export function isTrashedCapture(capture: Capture): boolean {
  return capture.state === CAPTURE_STATE_TRASHED;
}

/**
 * 从一页 `captures.list` 里挑出回收站条目（顺序原样保留）。
 *
 * 核心不按状态过滤，所以这是回收站页面的**唯一**入口。
 */
export function selectTrashedCaptures(captures: Capture[]): Capture[] {
  const out: Capture[] = [];
  for (let i = 0; i < captures.length; i++) {
    if (isTrashedCapture(captures[i])) {
      out.push(captures[i]);
    }
  }
  return out;
}

/**
 * 从一页 `captures.list` 里挑出**主列表**该显示的条目：排除 `trashed`。
 *
 * 草稿与已提交都保留（主列表不按这两种状态再分）；回收站条目一律不要。
 */
export function selectActiveCaptures(captures: Capture[]): Capture[] {
  const out: Capture[] = [];
  for (let i = 0; i < captures.length; i++) {
    if (!isTrashedCapture(captures[i])) {
      out.push(captures[i]);
    }
  }
  return out;
}

/**
 * 回收站状态容器：可被任意页面复用。
 *
 * 用法（页面侧）：
 * ```ts
 * const store = new RecycleStore(dbPath);
 * await store.refresh();                       // 只把 trashed 收进 store.captures
 * const preview = await store.previewPurge(id); // 三类版本清单在 preview 里原样给出
 * await store.purge(false);                     // 用 store 里那份令牌；过期会作废并要求重新预览
 * await store.restore(store.captures[0]);       // 用 Capture.revision 做乐观锁
 * this.items = store.captures;                  // 页面自己镜像到 @State
 * ```
 *
 * `loading`（读列表）与 `working`（写操作）互斥：任一为 true 时新的调用直接返回
 * `false` / `null`，不排队、不重放——写操作不是幂等的，排队只会拿旧 revision 连发。
 */
export class RecycleStore {
  /** 回收站里的记录（`state === 'trashed'`）。主列表要的是它的补集，见 [`selectActiveCaptures`]。 */
  captures: Capture[] = [];
  /** 最近一次 `previewPurge` 的结果（核心原样）；`null` = 还没预览或令牌已作废。 */
  preview: PurgePreview | null = null;
  /** 最近一次成功 `purge` 的结果（核心原样）：`purged_*` / `retained_*` / 两类「没删」的清单都在。 */
  lastPurge: PurgeResult | null = null;
  /** 最近一次预览是不是因为「过期 / 清单变了」被作废（为 true 时 `preview` 已不可用）。 */
  previewStale: boolean = false;
  /** 正在读列表。 */
  loading: boolean = false;
  /** 正在执行写操作（恢复 / 预览 / 清理）。 */
  working: boolean = false;
  /** 读/写失败时核心或宿主的原话（成功时是空哨兵）。 */
  error: UiError = EMPTY_UI_ERROR;
  /** 最近一次操作的结果说明：成功、令牌作废后已重新预览、冲突后已刷新……没有提示时是空串。 */
  message: string = '';
  /** 最近一次写是不是因为 `revision` 过期被拒（为 true 时 store 已经自动重读过列表）。 */
  conflicted: boolean = false;
  /** 这一轮列表扫描实际看了多少条（含非回收站）：用来如实解释「为什么回收站比想象的多/少」。 */
  scanned: number = 0;
  /** 列表是否因为 [`MAX_LIST_PAGES`] 没扫完。为 true 时 `captures` 只是前一部分，不是全部。 */
  truncated: boolean = false;

  private dbPath: string = '';
  private operationSeq: number = 0;

  constructor(dbPath: string) {
    this.dbPath = dbPath;
  }

  /**
   * 重读回收站：分页扫 `captures.list`（核心不按状态过滤），只把 `trashed` 收进来。
   *
   * 读失败时保留上一次成功读到的列表，把原因写进 `error`，返回 `false`。
   * 预览对应的记录如果已经不在回收站里，`preview` 会作废——不拿一份对不上号的
   * 清单去授权一次不可逆操作。
   */
  async refresh(): Promise<boolean> {
    if (this.loading || this.working) {
      return false;
    }
    this.loading = true;
    this.error = EMPTY_UI_ERROR;
    this.message = '';
    this.conflicted = false;
    const ok: boolean = await this.loadTrashPages();
    this.loading = false;
    return ok;
  }

  /**
   * 预览永久清理的影响（`captures.previewPurge`）。
   *
   * 只接受回收站里的记录（核心对不在回收站里的报 `invalid_state`）。返回的三类版本
   * 清单原样放在 `preview` 里：可判定关联（`diary_versions_still_referencing`）、
   * 派生（`derived_diary_version_ids`）、归属未定（`undetermined_diary_version_ids`）。
   * 后两类**一个都不删**，但必须让用户看见。
   */
  async previewPurge(captureId: string): Promise<PurgePreview | null> {
    if (this.working || this.loading) {
      return null;
    }
    const id: string = captureId.trim();
    if (id.length === 0) {
      this.message = '要清理哪一条记录还没有选定。';
      return null;
    }
    this.working = true;
    this.error = EMPTY_UI_ERROR;
    this.message = '';
    this.conflicted = false;
    const preview: PurgePreview | null = await this.fetchPreview(id);
    this.working = false;
    return preview;
  }

  /**
   * 按最近一次预览永久清理（`captures.purge`，不可逆）。
   *
   * - **只用 store 自己保存的那份预览**，不接受页面传令牌：页面手里的旧预览对象
   *   不能拿来发第二次请求；
   * - `deleteDiaryVersions` 默认 `false` = 保留仍引用这条记录的日记版本。`true` 也只删
   *   「有可判定关联」的版本；`derived_*` / `undetermined_*` 永远不删；
   * - 核心报 `invalid_state`（令牌过期 / 预览后清单变了）时：**旧令牌当场作废**，
   *   自动重新预览一次（只读），让用户核对新清单后再确认；不会拿旧令牌重试，
   *   也不会自动替用户再删一次；
   * - 删除后 `lastPurge` 保留核心的完整结果（含「没删」的两类），`message` 也把
   *   「哪些还留着」说出来。
   */
  async purge(deleteDiaryVersions: boolean = false): Promise<PurgeResult | null> {
    if (this.working || this.loading) {
      return null;
    }
    const preview: PurgePreview | null = this.preview;
    if (preview === null) {
      this.message = this.previewStale
        ? '上一次预览已经失效，请重新预览清理影响后再确认永久清理。'
        : '永久清理前要先预览清理影响（previewPurge）；没有预览不会删除任何东西。';
      return null;
    }
    this.working = true;
    this.error = EMPTY_UI_ERROR;
    this.message = '';
    this.conflicted = false;
    const params: PurgeCaptureParams = {
      capture_id: preview.capture_id,
      preview_token: preview.preview_token,
      delete_diary_versions: deleteDiaryVersions,
      operation_id: this.nextOperationId('purge'),
    };
    const result: CoreResult = await CoreGateway.call(this.dbPath, 'captures.purge', params);
    if (result.ok) {
      const purged: PurgeResult = result.data as PurgeResult;
      this.lastPurge = purged;
      this.preview = null;
      this.previewStale = false;
      const refreshed: boolean = await this.loadTrashPages();
      this.working = false;
      this.message = describePurgeResult(purged, deleteDiaryVersions);
      if (!refreshed) {
        this.message += '（回收站列表没有刷新成功，原因见上面的错误；这次清理本身已经完成。）';
      }
      return purged;
    }
    const error: UiError = uiErrorOf(result);
    if (error.code === 'invalid_state') {
      // 令牌过期、或预览之后这条记录变了：作废旧令牌，重新**预览**（只读）。
      // 绝不拿旧令牌重发 purge——那会把「重新确认」变成隐式重试。
      this.preview = null;
      this.previewStale = true;
      const fresh: PurgePreview | null = await this.fetchPreview(preview.capture_id);
      this.working = false;
      if (fresh !== null) {
        this.message = '预览令牌已过期，或预览之后这条记录的内容变了：旧令牌已作废，' +
          '已重新读取最新的清理影响；请核对后再次确认。记录没有被删除。';
      } else {
        this.message = '预览令牌已过期，或预览之后这条记录变了；旧令牌已作废，' +
          '重新预览也失败了（原因见上面的错误）。记录没有被删除。';
      }
      return null;
    }
    this.working = false;
    this.error = error;
    this.message = '永久清理没有完成，记录没有被删除。';
    return null;
  }

  /**
   * 从回收站恢复一条记录（`captures.restore`）：恢复回**移入之前**的状态。
   *
   * `expected_revision` 用传进来的 `capture.revision`。不符（`revision_conflict`）时
   * 自动重读列表并说明「已刷新、没有用旧 revision 重试」；不在回收站里（`invalid_state`）
   * 也会刷新——列表是唯一事实来源。
   */
  async restore(capture: Capture): Promise<boolean> {
    if (this.working || this.loading) {
      return false;
    }
    this.working = true;
    this.error = EMPTY_UI_ERROR;
    this.message = '';
    this.conflicted = false;
    const params: TrashCaptureParams = {
      capture_id: capture.id,
      expected_revision: capture.revision,
      operation_id: this.nextOperationId('restore'),
    };
    const result: CoreResult = await CoreGateway.call(this.dbPath, 'captures.restore', params);
    if (result.ok) {
      const restored: Capture = result.data as Capture;
      const refreshed: boolean = await this.loadTrashPages();
      this.working = false;
      this.message = isTrashedCapture(restored)
        ? '核心返回的记录仍在回收站里；已刷新列表，请以列表为准。'
        : `已恢复为「${restored.state}」状态；正文与来源都还在。`;
      if (!refreshed) {
        this.message += '（回收站列表没有刷新成功，原因见上面的错误；这次恢复本身已经完成。）';
      }
      return true;
    }
    const error: UiError = uiErrorOf(result);
    if (error.code === 'revision_conflict') {
      this.conflicted = true;
      const refreshed: boolean = await this.loadTrashPages();
      this.working = false;
      this.message = refreshed
        ? '这条记录已在别处被改动，已刷新回收站；没有用旧 revision 重试，请核对后再恢复。'
        : '这条记录已在别处被改动；没有用旧 revision 重试，列表也没刷新成功（原因见上面的错误）。';
      return false;
    }
    if (error.code === 'invalid_state') {
      const refreshed: boolean = await this.loadTrashPages();
      this.working = false;
      this.message = refreshed
        ? '这条记录已经不在回收站里（可能已被恢复）；已刷新列表。'
        : '这条记录已经不在回收站里（可能已被恢复）；列表刷新失败，原因见上面的错误。';
      return false;
    }
    this.working = false;
    this.error = error;
    this.message = '恢复没有完成，记录仍在回收站。';
    return false;
  }

  // ---------------------------------------------------------------- 内部

  /** 分页扫全部记录，只留 `trashed`；顺手把失效的预览作废。 */
  private async loadTrashPages(): Promise<boolean> {
    const collected: Capture[] = [];
    let cursor: string | null = null;
    let scanned: number = 0;
    let pages: number = 0;
    let truncated: boolean = false;
    while (true) {
      const params: ListCapturesParams = { limit: LIST_PAGE_SIZE };
      if (cursor !== null) {
        params.cursor = cursor;
      }
      const result: CoreResult = await CoreGateway.call(this.dbPath, 'captures.list', params);
      if (!result.ok) {
        this.error = uiErrorOf(result);
        return false;
      }
      const page: CapturePage = result.data as CapturePage;
      for (let i = 0; i < page.captures.length; i++) {
        scanned++;
        if (isTrashedCapture(page.captures[i])) {
          collected.push(page.captures[i]);
        }
      }
      pages++;
      cursor = page.next_cursor;
      if (cursor === null) {
        break;
      }
      if (pages >= MAX_LIST_PAGES) {
        truncated = true;
        break;
      }
    }
    this.captures = collected;
    this.scanned = scanned;
    this.truncated = truncated;
    if (truncated) {
      // 只扫了一部分：列表本身就不完整，必须说出来（不能悄悄截断）。
      this.message = `记录很多，这次只扫描了前 ${MAX_LIST_PAGES * LIST_PAGE_SIZE} 条；` +
        '回收站列表不完整（truncated=true）。';
    }
    // ⚠️ 只有**扫完了**才能判断「预览的那条不在回收站里」；截断时它可能在后面几页，
    // 这时作废预览等于说谎。
    if (!truncated && this.preview !== null && !this.containsCapture(this.preview.capture_id)) {
      // 预览的那条已经不在回收站里了：留着令牌只会让界面以为还能删。
      this.preview = null;
      this.previewStale = true;
      this.message = '之前预览的那条记录已经不在回收站里，预览已作废；要清理请重新预览。';
    }
    return true;
  }

  /** 发一次 `previewPurge`，成功时收下预览；失败时写 `error` 与一句人话。 */
  private async fetchPreview(captureId: string): Promise<PurgePreview | null> {
    const params: CaptureIdParams = { capture_id: captureId };
    const result: CoreResult = await CoreGateway.call(this.dbPath, 'captures.previewPurge', params);
    if (!result.ok) {
      const error: UiError = uiErrorOf(result);
      this.error = error;
      if (error.code === 'invalid_state') {
        this.message = '只有回收站里的记录才能永久清理；先把它移入回收站再预览。';
      } else if (error.code === 'not_found') {
        this.message = '这条记录已经不存在（可能已被清理）。';
      } else {
        this.message = '清理影响没有读取完成，记录没有被删除。';
      }
      return null;
    }
    const preview: PurgePreview = result.data as PurgePreview;
    this.preview = preview;
    this.previewStale = false;
    this.message = '已列出这次永久清理会删掉什么、会保留什么；确认后才会真正删除。';
    return preview;
  }

  private containsCapture(captureId: string): boolean {
    for (let i = 0; i < this.captures.length; i++) {
      if (this.captures[i].id === captureId) {
        return true;
      }
    }
    return false;
  }

  private nextOperationId(prefix: string): string {
    this.operationSeq++;
    return `${prefix}-${new Date().getTime()}-${this.operationSeq}`;
  }
}

/**
 * 清理结果的人话：删了什么、**还留着什么**都要说出来。
 *
 * 尤其是 `derived_diary_version_ids` / `undetermined_diary_version_ids` /
 * `retained_source_snapshot_ids`：它们与 `deleteDiaryVersions` 的选择无关，永远保留；
 * 不把它们说出来，界面就等于在说「所有副本都没了」。
 */
function describePurgeResult(result: PurgeResult, deleteDiaryVersions: boolean): string {
  const parts: string[] = [];
  parts.push(`已永久清理：原件 ${result.purged_asset_ids.length} 个、` +
    `派生内容 ${result.purged_derived_content_ids.length} 个、` +
    `AI 回应 ${result.purged_ai_response_ids.length} 个`);
  if (deleteDiaryVersions) {
    parts.push(`按选择删除了 ${result.purged_diary_version_ids.length} 个有可判定关联的日记版本`);
  } else {
    parts.push(`按默认保留 ${result.retained_diary_version_ids.length} 个仍引用这条记录的日记版本`);
  }
  if (result.derived_diary_version_ids.length > 0) {
    parts.push(`另有 ${result.derived_diary_version_ids.length} 个由它派生的版本仍然保留` +
      '（正文可能仍含这条记录的文字）');
  }
  if (result.undetermined_diary_version_ids.length > 0) {
    parts.push(`还有 ${result.undetermined_diary_version_ids.length} 个版本归属判不出来，` +
      '同样保留、没有删除');
  }
  if (result.retained_source_snapshot_ids.length > 0) {
    parts.push(`${result.retained_source_snapshot_ids.length} 份输入快照仍然保留` +
      '（快照里的正文可能仍有这条记录的文字）');
  }
  return parts.join('；') + '。';
}
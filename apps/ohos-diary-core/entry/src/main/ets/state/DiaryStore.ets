// 日记（AI 整理）的数据层：`diary.listDays` / `diary.getDay` / `diary.generate` /
// `diary.setCurrent` + `jobs.runDue` / `jobs.status`。
//
// 这一层不碰 UI：不 import 任何 ArkUI 组件，页面只把公开字段镜像到自己的 `@State`。
// 风格与 `state/SettingsStore.ets` 一致：字段 + `refresh()` + 写操作，失败把核心原话
// 留在 `error` 里。
//
// 三条硬口径（每一条都是「界面不许说谎」）：
//
// 1. **`diary.generate` 返回票据 ≠ 已生成**。它只冻结这一天的输入（不可变快照）、排一条
//    `diary_generate` 任务。真正的模型调用在 `jobs.runDue` 里。所以本 store 把
//    [`generate`] 与 [`runDueJobs`] 分成两个动作，`message` 也明说「这还不是正文」。
//    没有可用服务时 `generate` **照样受理并返回票据**：任务停在 `waiting_configuration`
//    等配置，**不是一次调用失败**（见第 3 条）。
// 2. **自动整理的结果一律是候选**：`scheduled` / `automatic` 的结果
//    `current_version_id` 仍是 NULL；要变成当前必须显式 `diary.setCurrent`。
//    首次（当天没有当前版本、也没有用户自己的版本）生成的结果可能直接成为当前版本，
//    界面以 `diary.getDay` 返回的 `current_version_id` / `candidate_version_ids` 为准，
//    **不要**把「有候选」显示成「已生成日记」。本 store 提供
//    [`currentVersionId`] / [`candidateVersionIds`] / [`candidatesOnly`] 三个只读结论。
// 3. **没有可用服务时不要报错成「生成失败」**：`jobs.status.waiting_configuration > 0`
//    时，本 store 把它如实暴露成 [`waitingConfiguration`] = true（附计数）与一句
//    「还没配置模型服务」的 `message`，`error` 保持空哨兵。只有调用本身失败才写 `error`。
//    但**「等待配置」与「调用失败」不是互斥关系**：前者解释「为什么没有生成」，后者说明
//    「这次调用失败了」，两者可以同时成立，页面必须都显示。而且 `jobs.status` 读失败时
//    本 store 会复位 `waitingConfiguration`（见 `jobsStatusKnown`）——拿不到结论就别下结论。
//
// 另外：
// - `diary.getDay` 的 `not_found` 是**空态**（这一天既没有材料也没有版本），不是读取
//   失败：本 store 置 [`dayEmpty`] = true、`error` 保持空；其余错误才进 `error`；
// - 三条写路径的 `expected_revision` 取自 `DiaryDay.revision`（持久单调，换当前版本也
//   推进），不是版本数；冲突时重读、**不拿旧 revision 重试**；
// - `jobs.status` 是**全队列聚合**，不区分任务种类；本 store 不拿它猜某一版是不是当前，
//   也不拿它冒充按任务 id 的状态（按 id 读要 `jobs.get`，不在这一片的 ABI 清单里）。
//   所以防重复点击靠页面在 `working` 期间禁用按钮，加上核心的 `operation_id` 幂等。

import {
  DiaryDay,
  DiaryDayPage,
  DiaryDayParams,
  DiaryGenerationReason,
  DiaryGenerationTicket,
  GenerateDiaryParams,
  JobErrorSummary,
  JobRunSummary,
  JobStatus,
  ListDiaryDaysParams,
  RunDueParams,
  SetDiaryCurrentParams,
} from '../core/Models';
import { CoreGateway, CoreResult, uiErrorOf } from '../ui/CoreGateway';
import { EMPTY_UI_ERROR, UiError } from '../ui/Errors';
import { runSummary } from '../ui/UserText';

/** `diary.listDays` 每页条数：核心把 `page_size` 夹在 1–100。 */
const DAYS_PAGE_SIZE: number = 100;

/** 天数翻页上限（防御）：100 页 × 100 天已经远超正常使用；超过就停下并如实置 `daysTruncated`。 */
const MAX_DAYS_PAGES: number = 100;

/**
 * `jobs.runDue` 的默认预算。
 *
 * 与任务中心页一致（30 秒）：生成一次要出网，预算太小会「排了但没跑」；预算只是上限，
 * 没任务时核心立刻返回。
 */
export const DEFAULT_RUN_BUDGET_MS: number = 30000;

/**
 * 日记状态容器：可被「日记 / 日记详情」页面复用。
 *
 * 用法（页面侧）：
 * ```ts
 * const store = new DiaryStore(dbPath);
 * await store.openDay('2026-10-21');      // diary.getDay；空的一天是空态，不是错误
 * await store.refreshJobStatus();         // jobs.status（可选：页面上显示队列/配置状态）
 * await store.generate('2026-10-21');     // 只受理：返回票据，还没有正文
 * await store.runDueJobs();               // 真正执行；之后 day 会重读
 * await store.setCurrent(store.candidateVersionIds()[0]);  // 候选 → 当前必须显式做
 * ```
 */
export class DiaryStore {
  /** `diary.listDays` 的全部结果（按 dayKey 倒序，核心排序）；分页由 store 自己走完。 */
  days: DiaryDay[] = [];
  /**
   * `days` 是否**至少成功读过一次**（本次会话）。
   *
   * `false` 时 [`days`] 是初值 `[]`（或上一次成功的旧值），**不代表「没有有材料的天」**。
   * 页面在 `!daysLoaded && error.code.length > 0` 时不能显示空态文案（那是把读失败
   * 说成没数据）。成功读完（含 `daysTruncated` 的截断）才置 `true`。
   */
  daysLoaded: boolean = false;
  /** `days` 是否因为 [`MAX_DAYS_PAGES`] 没走完：为 true 时它只是前一部分，不是全部。 */
  daysTruncated: boolean = false;
  /** 当前看的是哪一天（`YYYY-MM-DD`）；空串 = 还没选。 */
  dayKey: string = '';
  /** 当前这一天的聚合视图（`diary.getDay` 原样）；`null` = 还没读或这一天是空的。 */
  day: DiaryDay | null = null;
  /** 这一天的 `diary.getDay` 是 `not_found`（空态）。**不是错误**。 */
  dayEmpty: boolean = false;
  /** 最近一次 `diary.generate` 返回的票据（`job_id` / `source_snapshot_id`，snake_case）。 */
  ticket: DiaryGenerationTicket | null = null;
  /** `jobs.status` 的全队列聚合（核心原样）；任务计数与最近错误都在这里。 */
  jobStatus: JobStatus | null = null;
  /** 最近一次 `jobs.runDue` 的摘要（核心原样）。 */
  lastRun: JobRunSummary | null = null;
  /** 最近一次 `jobs.status` 里的 `recent_errors`（按任务 id、错误码、时间；原样）。 */
  recentErrors: JobErrorSummary[] = [];
  /**
   * 任务队列里有 `waiting_configuration` 的任务（`requires_user_action = true`）。
   *
   * **这不是失败**：它表示「还没配置模型服务」，配置好后任务会自动继续。界面按这个
   * 字段显示「还没配置模型服务 / 去配置」，不要显示成「生成失败」。
   *
   * ⚠️ 口径是**全队列聚合**，不是「这一天 / 这一次生成」的结论：`jobs.status` 不区分
   * 任务种类，语义索引等本地模型任务也会计入。界面文案必须写成「队列里有 N 个任务在
   * 等配置（含索引等本地模型任务）」，不能让人以为是某一次生成的判定。
   *
   * 读 `jobs.status` **失败**时本字段复位成 `false`、[`jobsStatusKnown`] 置 `false`：
   * 拿不到结论时不能说「有任务在等配置」，否则上一次的 `true` 会一直挂着，把真实错误
   * 盖掉（`pages/DiaryPage.ets` 曾因此只显示「不是生成失败」）。
   */
  waitingConfiguration: boolean = false;
  /** 上面那个状态的计数（0 = 没有任务在等配置）。 */
  waitingConfigurationCount: number = 0;
  /**
   * `waitingConfiguration` 背后那次 `jobs.status` 是否**读成功过**。
   *
   * `false` = 现在不知道有没有任务在等配置（没读过或上一次读失败）。页面在 `false` 时
   * 不要显示「还没配置模型服务」这种断言。
   */
  jobsStatusKnown: boolean = false;
  /** 正在读（天数列表 / 某一天 / 任务状态）。 */
  loading: boolean = false;
  /** 正在执行写操作（受理生成 / 运行任务 / 切换当前版本）。 */
  working: boolean = false;
  /** 最近一次操作是否因为 `revision` 过期被拒（为 true 时 store 已经重读过这一天）。 */
  conflicted: boolean = false;
  /** 读/写失败时核心或宿主的原话（成功时是空哨兵）。 */
  error: UiError = EMPTY_UI_ERROR;
  /** 最近一次操作的结果说明；没有提示时是空串。 */
  message: string = '';

  private dbPath: string = '';
  private operationSeq: number = 0;

  constructor(dbPath: string) {
    this.dbPath = dbPath;
  }

  /**
   * 重读：全部有材料的天（`diary.listDays`）+ 当前这一天（如果选了）+ 任务状态。
   *
   * 三件事各自独立：某一项失败不会把已经读到的其它项清空，`error` 保留第一处失败的原话，
   * 返回 `false`。`message` 不清空（上一次「运行结果」的说明还在，界面上不会一闪而过）。
   *
   * 注意：这是**页面级刷新**（天数列表可能翻很多页）；只要任务计数的页面用
   * [`refreshJobStatus`]，别在高频轮询里调这个。
   */
  async refresh(): Promise<boolean> {
    if (this.loading || this.working) {
      return false;
    }
    this.loading = true;
    this.error = EMPTY_UI_ERROR;
    let ok: boolean = await this.loadDaysPages();
    if (this.dayKey.length > 0) {
      const dayOk: boolean = await this.loadDayInternal(this.dayKey, false);
      if (!dayOk) {
        ok = false;
      }
    }
    const statusOk: boolean = await this.loadJobStatusInternal();
    if (!statusOk) {
      ok = false;
    }
    this.loading = false;
    return ok;
  }

  /**
   * 打开某一天（`diary.getDay`）：当前版本、候选版本、材料计数、`revision`。
   *
   * `not_found`（这一天既没有材料也没有成篇日记）是**空态**：`dayEmpty = true`、
   * `day = null`、`error` 保持空，返回 `true`。换一天会清掉上一天的票据（票据只对
   * 发起它的那一天有意义）。
   */
  async openDay(dayKey: string): Promise<boolean> {
    if (this.loading || this.working) {
      return false;
    }
    const key: string = dayKey.trim();
    if (key.length === 0) {
      this.message = '要看哪一天还没有选定。';
      return false;
    }
    if (key !== this.dayKey) {
      this.ticket = null;
    }
    this.dayKey = key;
    this.loading = true;
    this.error = EMPTY_UI_ERROR;
    this.message = '';
    this.conflicted = false;
    const ok: boolean = await this.loadDayInternal(key, true);
    this.loading = false;
    return ok;
  }

  /**
   * 受理一次生成（`diary.generate`）：冻结输入快照、排一条 `diary_generate` 任务。
   *
   * **返回票据 ≠ 已生成**：真正生成要再调 [`runDueJobs`]。这里不替调用方跑任务，也不
   * 假装有了一版正文。没有可用服务时也照样受理——任务会停在 `waiting_configuration`。
   *
   * `reason` 只接受 `manual` / `regenerate`：`scheduled` / `automatic` 是自动整理那一路，
   * 由计划任务发起（结果一律是候选），界面上的「生成 / 再来一版」不该伪造它们。
   * `styleId` 省略 = 用设置里选用的风格，未设置才用内置默认；不存在时报错，**不静默回退**。
   */
  async generate(
    dayKey: string,
    reason: DiaryGenerationReason = 'manual',
    styleId?: string,
  ): Promise<DiaryGenerationTicket | null> {
    if (this.working || this.loading) {
      return null;
    }
    const key: string = dayKey.trim();
    if (key.length === 0) {
      this.message = '要整理哪一天还没有选定。';
      return null;
    }
    if (reason !== 'manual' && reason !== 'regenerate') {
      this.message = '请重新发起整理。';
      return null;
    }
    if (styleId !== undefined && styleId.trim().length === 0) {
      this.message = '请重新选择风格。';
      return null;
    }
    if (key !== this.dayKey) {
      // 票据只属于发起它的那一天；换天时先把上一天的状态清掉，免得串台。
      this.dayKey = key;
      this.day = null;
      this.dayEmpty = false;
      this.ticket = null;
    }
    this.working = true;
    this.error = EMPTY_UI_ERROR;
    this.message = '';
    this.conflicted = false;
    const params: GenerateDiaryParams = {
      day_key: key,
      reason: reason,
      operation_id: this.nextOperationId('generate'),
    };
    if (styleId !== undefined) {
      params.style_id = styleId.trim();
    }
    const result: CoreResult = await CoreGateway.call(this.dbPath, 'diary.generate', params);
    this.working = false;
    if (!result.ok) {
      this.applyGenerateFailure(result);
      return null;
    }
    const ticket: DiaryGenerationTicket = result.data as DiaryGenerationTicket;
    this.ticket = ticket;
    this.message = '整理已排队，请稍候。';
    return ticket;
  }

  /** 「再来一版」：`reason = regenerate`，其余与 [`generate`] 相同。 */
  regenerate(dayKey: string, styleId?: string): Promise<DiaryGenerationTicket | null> {
    return this.generate(dayKey, 'regenerate', styleId);
  }

  /**
   * 指定这一天的当前版本（`diary.setCurrent`）。
   *
   * `expectedRevision` 省略时用 `day.revision`。冲突（`revision_conflict`）时重读这一天
   * 并说明「已刷新、当前版本没有变」，**不拿旧 revision 重试**；版本不属于这一天时
   * 重读并说明，不猜。
   *
   * 旧当前版变候选、不删除；切到已经是当前的版本是无操作。
   */
  async setCurrent(versionId: string, expectedRevision?: number): Promise<boolean> {
    if (this.working || this.loading) {
      return false;
    }
    const day: DiaryDay | null = this.day;
    if (day === null) {
      this.message = '请先打开这一天的日记。';
      return false;
    }
    const id: string = versionId.trim();
    if (id.length === 0) {
      this.message = '要设为当前的版本还没有选定。';
      return false;
    }
    const revision: number = expectedRevision === undefined ? day.revision : expectedRevision;
    this.working = true;
    this.error = EMPTY_UI_ERROR;
    this.message = '';
    this.conflicted = false;
    const params: SetDiaryCurrentParams = {
      day_key: day.day_key,
      version_id: id,
      expected_revision: revision,
    };
    const result: CoreResult = await CoreGateway.call(this.dbPath, 'diary.setCurrent', params);
    if (result.ok) {
      this.day = result.data as DiaryDay;
      this.dayEmpty = false;
      this.working = false;
      this.message = '已采用，原版本已保留。';
      return true;
    }
    const error: UiError = uiErrorOf(result);
    if (error.code === 'revision_conflict') {
      this.conflicted = true;
      const reread: boolean = await this.loadDayInternal(day.day_key, false);
      this.working = false;
      this.message = reread
        ? '日记已更新，请核对后重试。'
        : '日记已更新，暂时无法读取，请稍后重试。';
      return false;
    }
    if (error.code === 'not_found' || error.code === 'invalid_argument') {
      const reread: boolean = await this.loadDayInternal(day.day_key, false);
      this.working = false;
      this.message = reread
        ? '这个版本不属于这一天（或已被清理）；已重读这一天的状态，当前版本没有变。'
        : '这个版本不属于这一天（或已被清理）；当前版本没有变，这一天的状态也没能重读。';
      return false;
    }
    this.working = false;
    this.error = error;
    this.message = '切换当前版本没有完成；当前版本没有变。';
    return false;
  }

  /**
   * 运行到期的任务（`jobs.runDue`）：**这一步才真的生成**。
   *
   * 跑完会重读 `jobs.status` 与当前这一天：候选 / 当前以核心返回的 `diary.getDay` 为准。
   * 没有可用服务时任务停在 `waiting_configuration`：本方法把
   * [`waitingConfiguration`] 置 true 并在 `message` 里说明「还没配置模型服务」，
   * **`error` 保持空哨兵**——这不是生成失败。
   *
   * `trigger` 是唤醒来源（`manual` / `foreground` / `startup`），`budgetMs` 是这一轮的
   * 时间预算（默认 30 秒，与任务中心页一致）。
   */
  async runDueJobs(trigger: string = 'manual', budgetMs: number = DEFAULT_RUN_BUDGET_MS): Promise<JobRunSummary | null> {
    if (this.working || this.loading) {
      return null;
    }
    this.working = true;
    this.error = EMPTY_UI_ERROR;
    this.message = '';
    this.conflicted = false;
    const params: RunDueParams = {
      budget_ms: budgetMs,
      trigger: trigger,
    };
    const result: CoreResult = await CoreGateway.call(this.dbPath, 'jobs.runDue', params);
    if (!result.ok) {
      this.working = false;
      this.error = uiErrorOf(result);
      this.message = '运行待办任务没有完成；任务状态没有被改动。';
      return null;
    }
    const summary: JobRunSummary = result.data as JobRunSummary;
    this.lastRun = summary;
    const statusOk: boolean = await this.loadJobStatusInternal();
    let dayOk: boolean = true;
    if (this.dayKey.length > 0) {
      dayOk = await this.loadDayInternal(this.dayKey, false);
    }
    this.working = false;
    this.message = this.describeRun(summary, statusOk, dayOk);
    return summary;
  }

  /**
   * 只重读任务队列状态（`jobs.status`）：给高频轮询用，不碰天数列表。
   *
   * 只清 `error`，不清 `message`（上一次运行结果还留着）。
   */
  async refreshJobStatus(): Promise<boolean> {
    if (this.loading || this.working) {
      return false;
    }
    this.loading = true;
    this.error = EMPTY_UI_ERROR;
    const ok: boolean = await this.loadJobStatusInternal();
    this.loading = false;
    return ok;
  }

  // ---------------------------------------------------------------- 只读结论
  //
  // 这三个方法是「候选 vs 当前」的唯一口径：页面不要自己从版本列表里猜。

  /** 当前版本 id；`null` = 这一天还没有当前版本（**候选再新也不是当前**）。 */
  currentVersionId(): string | null {
    return this.day === null ? null : this.day.current_version_id;
  }

  /** 候选版本 id（除当前版本外的全部版本，最新在前）；空数组 = 没有候选。 */
  candidateVersionIds(): string[] {
    return this.day === null ? [] : this.day.candidate_version_ids;
  }

  /** 候选数量。 */
  candidateCount(): number {
    return this.day === null ? 0 : this.day.candidate_version_ids.length;
  }

  /** 这一天有没有当前版本。 */
  hasCurrentVersion(): boolean {
    return this.currentVersionId() !== null;
  }

  /**
   * 只有候选、没有当前版本：自动整理跑完但还没被采纳时的真实状态。
   *
   * 界面这时**不能**说「已生成日记」——只能说「有 N 个候选，还没设为当前」。
   */
  candidatesOnly(): boolean {
    return this.day !== null && this.day.current_version_id === null
      && this.day.candidate_version_ids.length > 0;
  }

  /** 某个版本是不是这一天的候选。 */
  isCandidate(versionId: string): boolean {
    return this.day !== null && this.day.candidate_version_ids.indexOf(versionId) >= 0;
  }

  /** 这一天的 `revision`（写路径的乐观锁基准）；没有读到这一天时是 0。 */
  dayRevision(): number {
    return this.day === null ? 0 : this.day.revision;
  }

  // ---------------------------------------------------------------- 内部

  /** 走完 `diary.listDays` 的所有页；失败时保留旧列表。 */
  private async loadDaysPages(): Promise<boolean> {
    const collected: DiaryDay[] = [];
    let cursor: string | null = null;
    let pages: number = 0;
    let truncated: boolean = false;
    while (true) {
      const params: ListDiaryDaysParams = { page_size: DAYS_PAGE_SIZE };
      if (cursor !== null) {
        params.cursor = cursor;
      }
      const result: CoreResult = await CoreGateway.call(this.dbPath, 'diary.listDays', params);
      if (!result.ok) {
        this.error = uiErrorOf(result);
        return false;
      }
      const page: DiaryDayPage = result.data as DiaryDayPage;
      for (let i = 0; i < page.days.length; i++) {
        collected.push(page.days[i]);
      }
      pages++;
      // ⚠️ 外层键是 camelCase 的 `nextCursor`（核心 `DiaryDayPage` 带 rename_all），
      // 不是 `next_cursor`；照核心 serde 原样读。
      cursor = page.nextCursor;
      if (cursor === null) {
        break;
      }
      if (pages >= MAX_DAYS_PAGES) {
        truncated = true;
        break;
      }
    }
    this.days = collected;
    this.daysLoaded = true;
    this.daysTruncated = truncated;
    if (truncated) {
      // 只走了一部分页：必须说出来，别让页面以为这就是全部。
      this.message = `有材料的日期很多，这次只载入了前 ${MAX_DAYS_PAGES * DAYS_PAGE_SIZE} 天；` +
        '日期列表尚未加载完整。';
    }
    return true;
  }

  /**
   * 读某一天。`not_found` 是空态：`dayEmpty = true`、`error` 不动，返回 `true`。
   *
   * `announceMissing` 只影响 `message`：`openDay` 是用户主动打开，要说清楚「这一天还没有
   * 内容」；`refresh` / `runDueJobs` 的附带重读不该覆盖上一次的操作说明。
   */
  private async loadDayInternal(key: string, announceMissing: boolean): Promise<boolean> {
    const params: DiaryDayParams = { day_key: key };
    const result: CoreResult = await CoreGateway.call(this.dbPath, 'diary.getDay', params);
    if (result.ok) {
      this.day = result.data as DiaryDay;
      this.dayEmpty = false;
      return true;
    }
    const error: UiError = uiErrorOf(result);
    if (error.code === 'not_found') {
      // 空的一天是界面的空态，不是「读取失败」（m6 C ABI 文档 §6.1）。
      this.day = null;
      this.dayEmpty = true;
      if (announceMissing) {
        this.message = '这一天既没有材料，也没有成篇日记；不是读取失败。';
      }
      return true;
    }
    this.day = null;
    this.dayEmpty = false;
    this.error = error;
    if (announceMissing) {
      this.message = '这一天的状态没有读取完成。';
    }
    return false;
  }

  /**
   * 读 `jobs.status`，把 `waiting_configuration` 如实映射成「还没配置」而不是错误。
   *
   * 读失败时**必须复位** `waitingConfiguration` / 计数，并把 `jobsStatusKnown` 置 false：
   * 拿不到结论就别说结论。否则上一次的 `true` 会一直挂着，页面就会把「读失败」解释成
   * 「还在等配置」，把真实错误藏掉。（这正是 P1 ③ 的根因。）
   */
  private async loadJobStatusInternal(): Promise<boolean> {
    const result: CoreResult = await CoreGateway.call(this.dbPath, 'jobs.status', null);
    if (!result.ok) {
      this.error = uiErrorOf(result);
      this.waitingConfiguration = false;
      this.waitingConfigurationCount = 0;
      this.jobsStatusKnown = false;
      return false;
    }
    const status: JobStatus = result.data as JobStatus;
    this.jobStatus = status;
    this.recentErrors = status.recent_errors;
    this.waitingConfigurationCount = status.waiting_configuration;
    this.waitingConfiguration = status.waiting_configuration > 0;
    this.jobsStatusKnown = true;
    return true;
  }

  /** 运行摘要的人话；「等待配置」与「候选」都在这里如实说出来。 */
  private describeRun(summary: JobRunSummary, statusOk: boolean, dayOk: boolean): string {
    const parts: string[] = [runSummary(summary)];
    if (statusOk && this.jobStatus !== null && this.jobStatus.waiting_configuration > 0) {
      parts.push('有任务等待配置，请先配置模型服务。');
    }
    if (dayOk && this.day !== null && this.day.candidate_version_ids.length > 0) {
      parts.push(`有 ${this.day.candidate_version_ids.length} 版候选日记，可阅读后采用。`);
    }
    if (!statusOk) { parts.push('暂时无法读取任务状态。'); }
    if (!dayOk) { parts.push('暂时无法读取日记。'); }
    return parts.join('');
  }

  /** 受理生成失败时按错误码说清楚：风格不存在 / 没有材料 / 其它。 */
  private applyGenerateFailure(result: CoreResult): void {
    const error: UiError = uiErrorOf(result);
    this.error = error;
    if (error.code === 'not_found') {
      this.message = '指定的风格不存在（不会回退默认风格）；这次没有排队。';
    } else if (error.code === 'invalid_argument') {
      this.message = '这一天没有可读材料（或参数不合法）；没有排空任务。';
    } else if (error.code === 'idempotency_conflict') {
      this.message = '同一个操作号的内容变了，这次不能安全重放；请重新发起一次生成。';
    } else if (error.code === 'revision_conflict') {
      this.message = '这一天的状态刚被改动；没有受理，请重读后再生成。';
    } else {
      this.message = '生成请求没有受理；没有任务被排队。';
    }
  }

  private nextOperationId(prefix: string): string {
    this.operationSeq++;
    return `${prefix}-${new Date().getTime()}-${this.operationSeq}`;
  }
}

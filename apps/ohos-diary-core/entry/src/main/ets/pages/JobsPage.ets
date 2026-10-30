import { SketchColors, SketchFonts, SketchType, SketchCard, sketchButton, sketchOutline } from '../ui/Sketch';
// 任务中心：`jobs.status` 聚合 + `jobs.list` 游标翻页 + `jobs.get` 逐条详情。
//
// 按钮只按**状态**给（文档 §3.2 / §3.5）：
// - `waiting_configuration`：`attention_key` 以 `job-config:` 开头 ⇒ 只给「去配置」，不给重试；
// - `failed`：只有「错误码不是不可恢复」时才给重试；`storage_full` / `invalid_state` /
//   `permission_denied` / 认证 / 额度 / 模型缺失等重试必失败，不给；
// - `queued` / `running` / `retry_wait` / `waiting_*`：非终态，可取消（核心支持）；
// - `succeeded` / `cancelled`：终态，都不给。
//
// 不按 `error_code` 拼用户文案：`attention_key` 只用来决定**跳哪**，原因用任务状态与
// 核心错误码原样展示。

import { common } from '@kit.AbilityKit';
import {
  CancelJobParams,
  Job,
  JobPage,
  JobRunSummary,
  JobStatus,
  ListJobsParams,
  RetryJobParams,
  JobIdParams,
  RunDueParams,
} from '../core/Models';
import { CoreGateway, CoreRequest, CoreResult, uiErrorOf } from '../ui/CoreGateway';
import { EMPTY_UI_ERROR, UiError } from '../ui/Errors';
import { AppState } from '../ui/AppState';
import { errorSummary, runSummary } from '../ui/UserText';
import {
  confirmAction,
  databasePath,
  formatDateTime,
  jobKindLabel,
  jobStateLabel,
  jobStateTone,
  newOperationId,
  toast,
} from '../ui/AppRuntime';
import { EmptyState, ErrorBanner, InfoRow, LoadingRow, PageTitle, SectionTitle, StatePill } from '../ui/Common';

const NON_RETRYABLE: string[] = [
  'storage_full',
  'invalid_state',
  'permission_denied',
  'authentication_failed',
  'quota_exceeded',
  'model_unavailable',
  'provider_not_configured',
  'unsupported_format',
  'asset_missing',
  'integrity_failed',
  'not_found',
  'revision_conflict',
];

@Component
struct JobCountChip {
  @Prop label: string = '';
  @Prop value: number = 0;
  @Prop tone: string = '';

  build() {
    Column({ space: 2 }) {
      Text(`${this.value}`).fontSize(18).fontWeight(FontWeight.Bold)
        .fontColor(this.tone === 'warn' ? SketchColors.warning : SketchColors.text)
      Text(this.label).fontSize(11).fontColor(SketchColors.muted)
    }.width('100%').padding({ top: 8, bottom: 8 }).alignItems(HorizontalAlign.Center)
  }
}

@Component
export struct JobsPage {
  pathStack: NavPathStack = new NavPathStack();
  @State jobs: Job[] = [];
  /** 至少成功读过一次任务列表。「false + error」时不能把空数组说成「这个筛选下没有任务」。 */
  @State loaded: boolean = false;
  @State status: JobStatus | null = null;
  @State filterIndex: number = 0;
  @State loading: boolean = true;
  @State refreshing: boolean = false;
  @State loadingMore: boolean = false;
  @State error: UiError = EMPTY_UI_ERROR;
  @State statusError: UiError = EMPTY_UI_ERROR;
  @State actionError: UiError = EMPTY_UI_ERROR;
  @State nextCursor: string = '';
  @State hasMore: boolean = false;
  @State expandedJobId: string = '';
  @State expandedJob: Job | null = null;
  @State detailLoading: boolean = false;
  @State busyJobId: string = '';
  @State runningDue: boolean = false;
  @State runMessage: string = '';
  @StorageLink('ohosDataVersion') @Watch('onDataVersionChanged') dataVersion: number = 0;

  private dbPath: string = '';
  private seq: number = 0;
  private filterLabels: string[] = ['全部', '需要处理', '处理中', '等待', '完成/取消'];
  private filterStates: string[][] = [
    [],
    ['waiting_configuration', 'failed'],
    ['queued', 'running'],
    ['retry_wait', 'waiting_dependency', 'waiting_network'],
    ['succeeded', 'cancelled'],
  ];

  aboutToAppear(): void {
    const context = getContext(this) as common.UIAbilityContext;
    this.dbPath = databasePath(context);
    this.reload();
  }

  onDataVersionChanged(): void {
    if (this.dbPath.length > 0) {
      this.reload();
    }
  }

  private async reload(): Promise<void> {
    this.seq++;
    const token: number = this.seq;
    this.error = EMPTY_UI_ERROR;
    this.statusError = EMPTY_UI_ERROR;
    this.nextCursor = '';
    this.hasMore = false;
    if (this.jobs.length === 0) {
      this.loading = true;
    }
    const listParams: ListJobsParams = { limit: 20 };
    const states: string[] = this.filterStates[this.filterIndex];
    if (states.length > 0) {
      listParams.states = states;
    }
    const requests: CoreRequest[] = [
      { method: 'jobs.status', params: null },
      { method: 'jobs.list', params: listParams },
    ];
    const results: CoreResult[] = await CoreGateway.callMany(this.dbPath, requests);
    if (token !== this.seq) {
      return;
    }
    this.loading = false;
    this.refreshing = false;
    if (results.length > 0) {
      if (results[0].ok) {
        this.status = results[0].data as JobStatus;
      } else {
        this.statusError = uiErrorOf(results[0]);
      }
    }
    if (results.length > 1) {
      if (results[1].ok) {
        const page: JobPage = results[1].data as JobPage;
        this.jobs = page.jobs;
        this.loaded = true;
        this.nextCursor = page.next_cursor === null ? '' : page.next_cursor;
        this.hasMore = page.next_cursor !== null;
      } else {
        this.error = uiErrorOf(results[1]);
      }
    }
  }

  private async refresh(): Promise<void> {
    this.seq++;
    const token: number = this.seq;
    await this.reload();
    if (token === this.seq) {
      this.refreshing = false;
    }
  }

  private async loadMore(): Promise<void> {
    if (!this.hasMore || this.loadingMore || this.loading || this.nextCursor.length === 0) {
      return;
    }
    this.loadingMore = true;
    const token: number = this.seq;
    const params: ListJobsParams = {
      limit: 20,
      cursor: this.nextCursor,
    };
    const states: string[] = this.filterStates[this.filterIndex];
    if (states.length > 0) {
      params.states = states;
    }
    const result = await CoreGateway.call(this.dbPath, 'jobs.list', params);
    if (token !== this.seq) {
      return;
    }
    this.loadingMore = false;
    if (!result.ok) {
      this.error = uiErrorOf(result);
      return;
    }
    const page: JobPage = result.data as JobPage;
    this.jobs = this.jobs.concat(page.jobs);
    this.nextCursor = page.next_cursor === null ? '' : page.next_cursor;
    this.hasMore = page.next_cursor !== null;
  }

  private setFilter(index: number): void {
    if (this.filterIndex === index) {
      return;
    }
    this.filterIndex = index;
    this.jobs = [];
    this.loading = true;
    this.expandedJobId = '';
    this.expandedJob = null;
    this.reload();
  }

  private async expand(job: Job): Promise<void> {
    if (this.expandedJobId === job.id) {
      this.expandedJobId = '';
      this.expandedJob = null;
      return;
    }
    this.expandedJobId = job.id;
    this.expandedJob = null;
    this.detailLoading = true;
    this.actionError = EMPTY_UI_ERROR;
    const params: JobIdParams = { job_id: job.id };
    const result = await CoreGateway.call(this.dbPath, 'jobs.get', params);
    if (this.expandedJobId !== job.id) {
      return;
    }
    this.detailLoading = false;
    if (!result.ok) {
      this.actionError = uiErrorOf(result);
      return;
    }
    this.expandedJob = result.data as Job;
  }

  private attentionKey(job: Job): string {
    return job.attention_key === null ? '' : job.attention_key;
  }

  private isConfigAttention(job: Job): boolean {
    return this.attentionKey(job).startsWith('job-config:');
  }

  private isTerminalAttention(job: Job): boolean {
    return this.attentionKey(job).startsWith('job-terminal:');
  }

  private canRetry(job: Job): boolean {
    if (job.state !== 'failed') {
      return false;
    }
    const code: string = job.error_code === null ? '' : job.error_code;
    if (code.length === 0) {
      return true;
    }
    return !NON_RETRYABLE.includes(code);
  }

  private canCancel(job: Job): boolean {
    return job.state !== 'succeeded' && job.state !== 'failed' && job.state !== 'cancelled';
  }

  private goConfigure(job: Job): void {
    if (!this.isConfigAttention(job)) {
      return;
    }
    if (job.kind === 'transcribe') {
      this.pathStack.pushPathByName('transcription', '');
      return;
    }
    if (job.kind === 'respond' || job.kind === 'diary_generate' || job.kind === 'organize') {
      this.pathStack.pushPathByName('llm', '');
      return;
    }
    // semantic_index / index / extract：都去设置页看模型与索引状态。
    this.pathStack.pushPathByName('settings', '');
  }

  private async retry(job: Job): Promise<void> {
    if (!this.canRetry(job) || this.busyJobId.length > 0) {
      return;
    }
    this.busyJobId = job.id;
    this.actionError = EMPTY_UI_ERROR;
    const params: RetryJobParams = {
      job_id: job.id,
      operation_id: newOperationId('job-retry'),
    };
    const result = await CoreGateway.call(this.dbPath, 'jobs.retry', params);
    this.busyJobId = '';
    if (!result.ok) {
      this.actionError = uiErrorOf(result);
      return;
    }
    this.expandedJob = result.data as Job;
    AppState.bumpDataVersion();
    this.reload();
    toast('已重新排队');
  }

  private async cancel(job: Job): Promise<void> {
    if (!this.canCancel(job) || this.busyJobId.length > 0) {
      return;
    }
    const ok: boolean = await confirmAction(
      '取消任务',
      `确定取消「${jobKindLabel(job.kind)}」这条任务？不会删除任何原件。`,
      '取消任务',
    );
    if (!ok) {
      return;
    }
    this.busyJobId = job.id;
    this.actionError = EMPTY_UI_ERROR;
    const params: CancelJobParams = {
      job_id: job.id,
      operation_id: newOperationId('job-cancel'),
    };
    const result = await CoreGateway.call(this.dbPath, 'jobs.cancel', params);
    this.busyJobId = '';
    if (!result.ok) {
      this.actionError = uiErrorOf(result);
      return;
    }
    this.expandedJob = result.data as Job;
    AppState.bumpDataVersion();
    this.reload();
    toast('任务已取消');
  }

  private async runDue(): Promise<void> {
    if (this.runningDue) {
      return;
    }
    this.runningDue = true;
    this.runMessage = '正在运行到期任务（最长 30 秒）…';
    this.actionError = EMPTY_UI_ERROR;
    const params: RunDueParams = {
      budget_ms: 30000,
      trigger: 'manual',
    };
    const result = await CoreGateway.call(this.dbPath, 'jobs.runDue', params);
    this.runningDue = false;
    if (!result.ok) {
      this.runMessage = '';
      this.actionError = uiErrorOf(result);
      return;
    }
    const summary: JobRunSummary = result.data as JobRunSummary;
    this.runMessage = runSummary(summary);
    AppState.bumpDataVersion();
    this.reload();
  }

  @Builder
  filterButton(label: string, index: number) {
    Button(label)
      .attributeModifier(sketchButton).drawModifier(sketchOutline())
      .fontSize(13)
      .margin({ right: 6, bottom: 6 })
      .backgroundColor(this.filterIndex === index
        ? SketchColors.button
        : SketchColors.soft)
      .fontColor(this.filterIndex === index
        ? SketchColors.ink
        : SketchColors.text)
      .onClick(() => {
        this.setFilter(index);
      })
  }

  @Builder
  jobDetail(job: Job) {
    Column({ space: 4 }) {
      SectionTitle({ text: '任务详情' })
      InfoRow({ label: '类型', value: jobKindLabel(job.kind) })
      InfoRow({ label: '状态', value: jobStateLabel(job.state) })
      InfoRow({ label: '尝试次数', value: `${job.attempt_count} / ${job.max_attempts}` })
      InfoRow({ label: '下次尝试', value: job.next_attempt_at === null ? '—' : formatDateTime(job.next_attempt_at) })
      InfoRow({ label: '创建时间', value: formatDateTime(job.created_at) })
      InfoRow({ label: '更新时间', value: formatDateTime(job.updated_at) })
      if (job.progress !== null) {
        InfoRow({ label: '进度', value: `${job.progress.completed} / ${job.progress.total}` })
      } else {
        InfoRow({ label: '进度', value: '处理中' })
      }
      if (job.requires_user_action) {
        if (this.isConfigAttention(job)) {
          Button('去配置')
            .attributeModifier(sketchButton).drawModifier(sketchOutline())
            .fontSize(13)
            .backgroundColor(SketchColors.button)
            .fontColor(SketchColors.ink)
            .onClick(() => {
              this.goConfigure(job);
            })
        } else if (this.isTerminalAttention(job)) {
          Text('任务未完成，请检查服务设置。')
            .fontSize(12)
            .fontColor(SketchColors.warning)
            .width('100%')
        } else {
          Text('任务需要处理，请检查服务设置。')
            .fontSize(12)
            .fontColor(SketchColors.warning)
            .width('100%')
        }
      }
      Row({ space: 8 }) {
        if (this.canRetry(job)) {
          Button(this.busyJobId === job.id ? '提交中…' : '重试')
            .attributeModifier(sketchButton).drawModifier(sketchOutline())
            .fontSize(13)
            .enabled(this.busyJobId.length === 0)
            .backgroundColor(SketchColors.button)
            .fontColor(SketchColors.ink)
            .onClick(() => {
              this.retry(job);
            })
        }
        if (this.canCancel(job)) {
          Button(this.busyJobId === job.id ? '提交中…' : '取消任务')
            .attributeModifier(sketchButton).drawModifier(sketchOutline())
            .fontSize(13)
            .enabled(this.busyJobId.length === 0)
            .backgroundColor(SketchColors.soft)
            .fontColor(SketchColors.text)
            .onClick(() => {
              this.cancel(job);
            })
        }
      }
      .width('100%')
      .margin({ top: 8 })
    }
    .width('100%')
    .padding({ top: 8 })
    .alignItems(HorizontalAlign.Start)
  }

  @Builder
  jobCard(job: Job) {
    Column({ space: 8 }) {
      Row({ space: 8 }) {
        Text(jobKindLabel(job.kind))
          .fontSize(15)
          .fontWeight(FontWeight.Medium)
          .fontColor(SketchColors.text)
        StatePill({ text: jobStateLabel(job.state), tone: jobStateTone(job.state) })
        Blank()
        Text(this.expandedJobId === job.id ? '收起' : '详情')
          .fontSize(12)
          .fontColor(SketchColors.ink)
      }
      .width('100%')
      .onClick(() => {
        this.expand(job);
      })
      Row({ space: 10 }) {
        Text(`尝试 ${job.attempt_count}/${job.max_attempts}`)
          .fontSize(12)
          .fontColor(SketchColors.muted)
        if (job.error_code !== null) {
          Text(errorSummary(job.error_code))
            .fontSize(12)
            .fontColor(SketchColors.alert)
        }
        if (job.next_attempt_at !== null) {
          Text(`下次：${formatDateTime(job.next_attempt_at)}`)
            .fontSize(12)
            .fontColor(SketchColors.muted)
        }
      }
      .width('100%')
      if (job.progress !== null) {
        Row({ space: 8 }) {
          Progress({ value: job.progress.completed, total: job.progress.total, type: ProgressType.Linear })
            .layoutWeight(1)
            .color(SketchColors.ink)
          Text(`${job.progress.completed} / ${job.progress.total}`)
            .fontSize(11)
            .fontColor(SketchColors.muted)
        }
        .width('100%')
      }
      if (job.requires_user_action) {
        Row({ space: 8 }) {
          StatePill({ text: '需要处理', tone: 'warn' })
          if (this.isConfigAttention(job)) {
            Text('可以「去配置」')
              .fontSize(12)
              .fontColor(SketchColors.warning)
          } else {
            Text('请检查服务设置')
              .fontSize(12)
              .fontColor(SketchColors.muted)
          }
        }
        .width('100%')
      }
      if (this.expandedJobId === job.id) {
        if (this.detailLoading) {
          LoadingRow({ text: '读取任务详情…' })
        } else if (this.expandedJob !== null) {
          this.jobDetail(this.expandedJob as Job)
        }
        if (this.actionError.code.length > 0) {
          ErrorBanner({ error: this.actionError, showReload: false, onReload: () => {
          } })
        }
      }
    }
    .width('100%')
    .padding(14)
    .backgroundColor(SketchColors.paper)
    .drawModifier(sketchOutline())
    .alignItems(HorizontalAlign.Start)
  }

  build() {
    Column({ space: 10 }) {
      Row({ space: 8 }) {
        Column() {
          PageTitle({ title: '任务中心', subtitle: '查看转写和搜索处理进度。' })
        }
        .layoutWeight(1)
        .alignItems(HorizontalAlign.Start)
        Button(this.runningDue ? '运行中…' : '运行到期任务')
          .attributeModifier(sketchButton).drawModifier(sketchOutline())
          .fontSize(13)
          .enabled(!this.runningDue)
          .backgroundColor(SketchColors.button)
          .fontColor(SketchColors.ink)
          .onClick(() => {
            this.runDue();
          })
      }
      .width('100%')
      .padding({ left: 16, right: 16, top: 12 })

      if (this.status !== null) {
        Column() {
          Row() {
            JobCountChip({ label: '处理中', value: this.status.queued + this.status.running }).layoutWeight(1)
            JobCountChip({ label: '等待', value: this.status.retry_wait + this.status.waiting_dependency + this.status.waiting_network }).layoutWeight(1)
            JobCountChip({ label: '需要处理', value: this.status.waiting_configuration + this.status.failed, tone: 'warn' }).layoutWeight(1)
            JobCountChip({ label: '完成', value: this.status.succeeded }).layoutWeight(1)
          }
          .width('100%')
          Row({ space: 8 }) {
            if (this.status.due_backlog > 0) {
              Text(`待处理：${this.status.due_backlog}`)
                .fontSize(12)
                .fontColor(SketchColors.warning)
            }
            if (this.status.next_wakeup !== null) {
              Text(`下次处理：${formatDateTime(this.status.next_wakeup)}`)
                .fontSize(12)
                .fontColor(SketchColors.muted)
            }
            if (this.status.cancelled > 0) {
              Text(`已取消：${this.status.cancelled}`)
                .fontSize(12)
                .fontColor(SketchColors.muted)
            }
          }
          .width('100%')
        }
        .width('100%')
        .padding(10)
        .backgroundColor(SketchColors.paper)
        .drawModifier(sketchOutline())
        .margin({ left: 16, right: 16 })
      }

      if (this.runMessage.length > 0) {
        Text(this.runMessage)
          .fontSize(12)
          .fontColor(SketchColors.muted)
          .width('100%')
          .padding({ left: 16, right: 16 })
      }

      if (this.statusError.code.length > 0) {
        Text('任务数量暂时无法读取。')
          .fontSize(12)
          .fontColor(SketchColors.muted)
          .width('100%')
          .padding({ left: 16, right: 16 })
      }

      if (this.error.code.length > 0) {
        Column() {
          ErrorBanner({
            error: this.error,
            showReload: true,
            onReload: () => {
              this.reload();
            },
          })
        }
        .padding({ left: 16, right: 16 })
      }

      Flex({ wrap: FlexWrap.Wrap }) {
        ForEach(this.filterLabels, (label: string, index: number) => {
          this.filterButton(label, index)
        }, (label: string, index: number) => `${index}-${label}`)
      }
      .width('100%')
      .padding({ left: 16, right: 16 })

      Refresh({ refreshing: $$this.refreshing }) {
        List({ space: 10 }) {
          if (this.loading) {
            ListItem() {
              LoadingRow({ text: '读取任务…' })
            }
          } else if (this.jobs.length === 0 && this.loaded) {
            ListItem() {
              EmptyState({
                icon: '—',
                title: '这个筛选下没有任务',
                hint: '转写和搜索处理任务会显示在这里。',
              })
            }
          } else if (this.jobs.length === 0) {
            // 一次都没读成功：这是「读失败」，不是「这个筛选下没有任务」。
            ListItem() {
              Text('任务列表没有读出来：这不代表没有任务；原因见上面的错误。')
                .fontSize(12)
                .fontColor(SketchColors.muted)
                .width('100%')
                .padding(12)
            }
          } else {
            ForEach(this.jobs, (job: Job) => {
              ListItem() {
                this.jobCard(job)
              }
            }, (job: Job) => `${job.id}-${job.state}-${job.updated_at}`)
            if (this.loadingMore) {
              ListItem() {
                LoadingRow({ text: '加载更多任务…' })
              }
            } else if (!this.hasMore) {
              ListItem() {
                Text('没有更多了')
                  .fontSize(12)
                  .fontColor(SketchColors.muted)
                  .width('100%')
                  .textAlign(TextAlign.Center)
                  .padding(12)
              }
            }
          }
        }
        .width('100%')
        .height('100%')
        .padding({ left: 16, right: 16 })
        .onReachEnd(() => {
          this.loadMore();
        })
      }
      .onRefreshing(() => {
        this.refresh();
      })
      .layoutWeight(1)
    }
    .width('100%')
    .height('100%')
    .backgroundColor(SketchColors.canvas)
  }
}

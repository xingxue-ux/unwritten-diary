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
export struct JobsPage {
  pathStack: NavPathStack = new NavPathStack();
  @State jobs: Job[] = [];
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
  private filterLabels: string[] = ['全部', '需要处理', '处理中', '等待', '完成'];
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
    if (job.kind === 'respond') {
      toast('AI 服务配置入口还没有接进 C ABI；当前只能查看任务状态。');
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
    this.runMessage = `本轮：处理 ${summary.processed}，成功 ${summary.succeeded}，失败 ${summary.failed}，` +
      `重试 ${summary.retrying}，积压 ${summary.due_backlog}`;
    AppState.bumpDataVersion();
    this.reload();
  }

  private countOf(state: string): number {
    const status: JobStatus | null = this.status;
    if (status === null) {
      return 0;
    }
    if (state === 'queued') {
      return status.queued;
    }
    if (state === 'running') {
      return status.running;
    }
    if (state === 'retry_wait') {
      return status.retry_wait;
    }
    if (state === 'waiting_configuration') {
      return status.waiting_configuration;
    }
    if (state === 'waiting_dependency') {
      return status.waiting_dependency;
    }
    if (state === 'waiting_network') {
      return status.waiting_network;
    }
    if (state === 'failed') {
      return status.failed;
    }
    if (state === 'cancelled') {
      return status.cancelled;
    }
    if (state === 'succeeded') {
      return status.succeeded;
    }
    return 0;
  }

  @Builder
  countChip(label: string, value: number, tone: string) {
    Column({ space: 2 }) {
      Text(`${value}`)
        .fontSize(18)
        .fontWeight(FontWeight.Bold)
        .fontColor(tone === 'warn'
          ? $r('sys.color.ohos_id_color_warning')
          : (tone === 'alert'
            ? $r('sys.color.ohos_id_color_alert')
            : $r('sys.color.ohos_id_color_text_primary')))
      Text(label)
        .fontSize(11)
        .fontColor($r('sys.color.ohos_id_color_text_secondary'))
    }
    .layoutWeight(1)
    .padding({ top: 8, bottom: 8 })
    .alignItems(HorizontalAlign.Center)
  }

  @Builder
  filterButton(label: string, index: number) {
    Button(label)
      .fontSize(13)
      .height(32)
      .backgroundColor(this.filterIndex === index
        ? $r('sys.color.ohos_id_color_emphasize')
        : $r('sys.color.ohos_id_color_sub_background'))
      .fontColor(this.filterIndex === index
        ? $r('sys.color.ohos_id_color_text_primary_contrary')
        : $r('sys.color.ohos_id_color_text_primary'))
      .onClick(() => {
        this.setFilter(index);
      })
  }

  @Builder
  jobDetail(job: Job) {
    Column({ space: 4 }) {
      SectionTitle({ text: '任务详情（jobs.get）' })
      InfoRow({ label: '任务 ID', value: job.id })
      InfoRow({ label: '类型', value: `${jobKindLabel(job.kind)}（${job.kind}）` })
      InfoRow({ label: '状态', value: jobStateLabel(job.state) })
      InfoRow({ label: '尝试次数', value: `${job.attempt_count} / ${job.max_attempts}` })
      InfoRow({ label: '优先级', value: `${job.priority}` })
      InfoRow({ label: '目标数', value: `${job.target_ids.length}` })
      InfoRow({ label: '错误码', value: job.error_code === null ? '—' : job.error_code })
      InfoRow({ label: '下次尝试', value: job.next_attempt_at === null ? '—' : formatDateTime(job.next_attempt_at) })
      InfoRow({ label: 'attention_key', value: job.attention_key === null ? '—' : job.attention_key })
      InfoRow({ label: '创建时间', value: formatDateTime(job.created_at) })
      InfoRow({ label: '更新时间', value: formatDateTime(job.updated_at) })
      if (job.progress !== null) {
        InfoRow({ label: '进度', value: `${job.progress.completed} / ${job.progress.total}` })
      } else {
        InfoRow({ label: '进度', value: '核心没有给可靠进度（不编百分比）' })
      }
      if (job.requires_user_action) {
        if (this.isConfigAttention(job)) {
          Button('去配置')
            .fontSize(13)
            .height(34)
            .backgroundColor($r('sys.color.ohos_id_color_emphasize'))
            .fontColor($r('sys.color.ohos_id_color_text_primary_contrary'))
            .onClick(() => {
              this.goConfigure(job);
            })
        } else if (this.isTerminalAttention(job)) {
          Text('终态、需要人工处理；没有可靠的自动跳转目标，先看上面的错误码。')
            .fontSize(12)
            .fontColor($r('sys.color.ohos_id_color_warning'))
            .width('100%')
        } else {
          Text('需要处理，但核心没有给出 attention_key；不猜原因、不乱跳。')
            .fontSize(12)
            .fontColor($r('sys.color.ohos_id_color_warning'))
            .width('100%')
        }
      }
      Row({ space: 8 }) {
        if (this.canRetry(job)) {
          Button(this.busyJobId === job.id ? '提交中…' : '重试')
            .fontSize(13)
            .height(34)
            .enabled(this.busyJobId.length === 0)
            .backgroundColor($r('sys.color.ohos_id_color_emphasize'))
            .fontColor($r('sys.color.ohos_id_color_text_primary_contrary'))
            .onClick(() => {
              this.retry(job);
            })
        }
        if (this.canCancel(job)) {
          Button(this.busyJobId === job.id ? '提交中…' : '取消任务')
            .fontSize(13)
            .height(34)
            .enabled(this.busyJobId.length === 0)
            .backgroundColor($r('sys.color.ohos_id_color_sub_background'))
            .fontColor($r('sys.color.ohos_id_color_text_primary'))
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
          .fontColor($r('sys.color.ohos_id_color_text_primary'))
        StatePill({ text: jobStateLabel(job.state), tone: jobStateTone(job.state) })
        Blank()
        Text(job.id.substring(0, 10))
          .fontSize(11)
          .fontColor($r('sys.color.ohos_id_color_text_tertiary'))
        Text(this.expandedJobId === job.id ? '收起' : '详情')
          .fontSize(12)
          .fontColor($r('sys.color.ohos_id_color_emphasize'))
      }
      .width('100%')
      .onClick(() => {
        this.expand(job);
      })
      Row({ space: 10 }) {
        Text(`尝试 ${job.attempt_count}/${job.max_attempts}`)
          .fontSize(12)
          .fontColor($r('sys.color.ohos_id_color_text_secondary'))
        if (job.error_code !== null) {
          Text(`错误码：${job.error_code}`)
            .fontSize(12)
            .fontColor($r('sys.color.ohos_id_color_alert'))
        }
        if (job.next_attempt_at !== null) {
          Text(`下次：${formatDateTime(job.next_attempt_at)}`)
            .fontSize(12)
            .fontColor($r('sys.color.ohos_id_color_text_tertiary'))
        }
      }
      .width('100%')
      if (job.progress !== null) {
        Row({ space: 8 }) {
          Progress({ value: job.progress.completed, total: job.progress.total, type: ProgressType.Linear })
            .layoutWeight(1)
            .color($r('sys.color.ohos_id_color_emphasize'))
          Text(`${job.progress.completed} / ${job.progress.total}`)
            .fontSize(11)
            .fontColor($r('sys.color.ohos_id_color_text_secondary'))
        }
        .width('100%')
      }
      if (job.requires_user_action) {
        Row({ space: 8 }) {
          StatePill({ text: '需要处理', tone: 'warn' })
          if (this.isConfigAttention(job)) {
            Text('可以「去配置」')
              .fontSize(12)
              .fontColor($r('sys.color.ohos_id_color_warning'))
          } else {
            Text('没有可靠的跳转目标')
              .fontSize(12)
              .fontColor($r('sys.color.ohos_id_color_text_tertiary'))
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
    .backgroundColor($r('sys.color.ohos_id_color_card_bg'))
    .borderRadius(14)
    .alignItems(HorizontalAlign.Start)
  }

  build() {
    Column({ space: 10 }) {
      Row({ space: 8 }) {
        Column() {
          PageTitle({ title: '任务中心', subtitle: '转写、索引与 AI 任务的真实状态；不编进度' })
        }
        .layoutWeight(1)
        .alignItems(HorizontalAlign.Start)
        Button(this.runningDue ? '运行中…' : '运行到期任务')
          .fontSize(13)
          .height(36)
          .enabled(!this.runningDue)
          .backgroundColor($r('sys.color.ohos_id_color_emphasize'))
          .fontColor($r('sys.color.ohos_id_color_text_primary_contrary'))
          .onClick(() => {
            this.runDue();
          })
      }
      .width('100%')
      .padding({ left: 16, right: 16, top: 12 })

      if (this.status !== null) {
        Column() {
          Row() {
            this.countChip('处理中', this.countOf('queued') + this.countOf('running'), 'active')
            this.countChip('等待', this.countOf('retry_wait') + this.countOf('waiting_dependency') + this.countOf('waiting_network'), 'muted')
            this.countChip('需要处理', this.countOf('waiting_configuration') + this.countOf('failed'), 'warn')
            this.countChip('完成', this.countOf('succeeded') + this.countOf('cancelled'), 'ok')
          }
          .width('100%')
          Row({ space: 8 }) {
            if (this.status.due_backlog > 0) {
              Text(`到期积压：${this.status.due_backlog}`)
                .fontSize(12)
                .fontColor($r('sys.color.ohos_id_color_warning'))
            }
            if (this.status.next_wakeup !== null) {
              Text(`下次唤醒：${formatDateTime(this.status.next_wakeup)}`)
                .fontSize(12)
                .fontColor($r('sys.color.ohos_id_color_text_tertiary'))
            }
          }
          .width('100%')
        }
        .width('100%')
        .padding(10)
        .backgroundColor($r('sys.color.ohos_id_color_card_bg'))
        .borderRadius(12)
        .margin({ left: 16, right: 16 })
      }

      if (this.runMessage.length > 0) {
        Text(this.runMessage)
          .fontSize(12)
          .fontColor($r('sys.color.ohos_id_color_text_secondary'))
          .width('100%')
          .padding({ left: 16, right: 16 })
      }

      if (this.statusError.code.length > 0) {
        Text(`队列计数读取失败（${this.statusError.code}）；列表仍按真实任务展示。`)
          .fontSize(12)
          .fontColor($r('sys.color.ohos_id_color_text_secondary'))
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

      Row({ space: 6 }) {
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
          } else if (this.jobs.length === 0) {
            ListItem() {
              EmptyState({
                icon: '—',
                title: '这个筛选下没有任务',
                hint: '转写/索引任务在录音保存、重建索引或运行到期任务后出现。',
              })
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
                  .fontColor($r('sys.color.ohos_id_color_text_tertiary'))
                  .width('100%')
                  .textAlign(TextAlign.Center)
                  .padding(12)
              }
            }
          }
        }
        .width('100%')
        .layoutWeight(1)
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
    .backgroundColor($r('sys.color.ohos_id_color_background'))
  }
}

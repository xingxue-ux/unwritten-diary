import { SketchColors, SketchFonts, SketchType, SketchCard, sketchButton, sketchOutline } from '../ui/Sketch';
// 录音页：复用 `recording/VoiceRecorder.ets`（权限 / 常亮 / 分片 / 崩溃恢复）。
//
// 停止后的真实流程：
//   VoiceRecorder.stop() → recordings.finalize（音频/来源落库 + 排转写任务）
//                        → captures.commit（自动提交成篇）
// finalize 只保证「音频已保存」；提交由 VoiceRecorder 在 finalize 之后立刻做一次，
// 结果（committed / already_committed / failed / not_attempted）保存在
// `commitResult()` 里。本页必须读取并如实显示：提交失败时音频仍已保存、记录仍是
// draft，不能让用户以为存好了。
// 转写任务用 `jobs.runDue` 在 worker 里推进，不在 UI 线程加载模型。

import { common } from '@kit.AbilityKit';
import { appFilesDir } from '../session/AppDirectory';
import {
  CommitParams,
  CommitResult,
  FinalizeRecordingParams,
  JobRunSummary,
  RecoverRecordingParams,
  RecordingFinalizeResult,
  RecordingRecovery,
  RunDueParams,
} from '../core/Models';
import { CoreGateway, uiErrorOf } from '../ui/CoreGateway';
import { EMPTY_UI_ERROR, UiError, uiErrorFromThrown } from '../ui/Errors';
import { CoreOpenResult, CoreSession } from '../ui/CoreSession';
import { AppState } from '../ui/AppState';
import { runSummary } from '../ui/UserText';
import { databasePath, formatDuration, toast } from '../ui/AppRuntime';
import { ensureMicrophonePermission, PermissionOutcome } from '../recording/MicPermission';
import { RecorderOptions, RecordingCommitResult, VoiceRecorder } from '../recording/VoiceRecorder';
import { ErrorBanner, InfoRow, LoadingRow, SectionTitle, StatePill } from '../ui/Common';

@Component
export struct RecordingPage {
  pathStack: NavPathStack = new NavPathStack();
  @State phase: string = 'idle';
  @State elapsedMs: number = 0;
  @State persistedMs: number = 0;
  @State error: UiError = EMPTY_UI_ERROR;
  @State permissionDenied: boolean = false;
  @State permissionMessage: string = '';
  @State recovery: RecordingRecovery | null = null;
  @State recoveryLoading: boolean = true;
  @State recoverySaving: boolean = false;
  @State recoveryError: UiError = EMPTY_UI_ERROR;
  @State finalized: RecordingFinalizeResult | null = null;
  @State interrupted: boolean = false;
  @State keepScreenOk: boolean = true;
  @State bgTaskOk: boolean = true;
  @State transcribeHint: string = '';
  @State runningDue: boolean = false;
  /** stop() 已经尝试过 finalize；失败时不再给「保存已录部分」，避免二次 finalize。 */
  @State finalizeAttempted: boolean = false;
  /**
   * finalize 之后那次自动提交的结果（来自 `VoiceRecorder.commitResult()`）。
   *
   * 空串 = 还没走到 finalize；其余按 state 原样显示，不做「看起来成功」的默认值。
   */
  @State commitState: string = '';
  @State commitRevision: number = -1;
  @State commitError: string = '';
  @State commitRetrying: boolean = false;
  @State commitRetryError: UiError = EMPTY_UI_ERROR;

  private recorder: VoiceRecorder | null = null;
  private timer: number = -1;
  private context: common.UIAbilityContext | null = null;
  private dbPath: string = '';
  private active: boolean = false;
  private startSeq: number = 0;

  aboutToAppear(): void {
    if (this.active) {
      return;
    }
    this.active = true;
    this.context = getContext(this) as common.UIAbilityContext;
    this.dbPath = databasePath(this.context);
    this.enterRecording();
  }

  aboutToDisappear(): void {
    this.active = false;
    this.startSeq++;
    this.stopTimer();
    const recorder: VoiceRecorder | null = this.recorder;
    if (recorder !== null && (this.phase === 'recording' || this.phase === 'preparing')) {
      // 页面被系统销毁/切走：释放常亮与长时任务；不 finalize，留给下次启动 recover。
      recorder.abort().catch((e: Object) => {
        // 页面已经离开；清理失败只影响常亮/长时任务，由系统回收。
      });
    }
  }

  private startTimer(): void {
    this.stopTimer();
    this.timer = setInterval(() => {
      const recorder: VoiceRecorder | null = this.recorder;
      if (recorder === null) {
        return;
      }
      this.elapsedMs = recorder.receivedMs();
      this.persistedMs = recorder.persistedMs();
      this.interrupted = recorder.wasInterrupted();
      if (recorder.currentPhase() === 'error') {
        this.phase = 'error';
        const failure = recorder.lastError();
        this.error = uiErrorFromThrown(failure === null ? new Error(recorder.error()) : failure);
        this.refreshHoldHints(recorder);
        this.stopTimer();
      }
    }, 250);
  }

  private stopTimer(): void {
    if (this.timer >= 0) {
      clearInterval(this.timer);
      this.timer = -1;
    }
  }

  private refreshHoldHints(recorder: VoiceRecorder): void {
    this.keepScreenOk = recorder.keepScreenResult().ok;
    this.bgTaskOk = recorder.taskResult().ok;
  }

  private async loadRecovery(): Promise<void> {
    this.recoveryLoading = true;
    this.recoveryError = EMPTY_UI_ERROR;
    const params: RecoverRecordingParams = {};
    const result = await CoreGateway.call(this.dbPath, 'recordings.recover', params);
    this.recoveryLoading = false;
    if (!result.ok) {
      this.recoveryError = uiErrorOf(result);
      return;
    }
    this.recovery = result.data as RecordingRecovery;
  }

  private async enterRecording(): Promise<void> {
    const token: number = this.startSeq;
    // 先读取旧会话，避免把刚创建的本次录音误识别成待恢复录音。
    await this.loadRecovery();
    if (!this.active || token !== this.startSeq) {
      return;
    }
    await this.startRecording();
  }

  private async recordAgain(): Promise<void> {
    if (!this.active || this.phase === 'preparing' || this.phase === 'recording' || this.phase === 'stopping') {
      return;
    }
    const token: number = ++this.startSeq;
    this.phase = 'preparing';
    this.stopTimer();
    try {
      if (this.recorder !== null) {
        await this.recorder.abort();
      }
    } catch (e) {
      if (this.active && token === this.startSeq) {
        this.phase = 'error';
        this.error = uiErrorFromThrown(e as Object);
      }
      return;
    }
    if (!this.active || token !== this.startSeq) {
      return;
    }
    this.recorder = null;
    this.finalized = null;
    this.phase = 'idle';
    this.error = EMPTY_UI_ERROR;
    this.transcribeHint = '';
    this.finalizeAttempted = false;
    this.commitState = '';
    this.commitRevision = -1;
    this.commitError = '';
    this.commitRetryError = EMPTY_UI_ERROR;
    await this.enterRecording();
  }

  private async startRecording(): Promise<void> {
    if (!this.active) {
      return;
    }
    if (this.phase === 'preparing' || this.phase === 'recording' || this.phase === 'stopping') {
      return;
    }
    const context: common.UIAbilityContext | null = this.context;
    if (context === null) {
      return;
    }
    const token: number = ++this.startSeq;
    this.phase = 'preparing';
    this.error = EMPTY_UI_ERROR;
    this.finalized = null;
    this.transcribeHint = '';
    this.interrupted = false;
    this.finalizeAttempted = false;
    this.commitState = '';
    this.commitRevision = -1;
    this.commitError = '';
    this.commitRetryError = EMPTY_UI_ERROR;
    this.permissionMessage = '';

    const permission: PermissionOutcome = await ensureMicrophonePermission(context);
    if (!this.active || token !== this.startSeq) {
      return;
    }
    if (!permission.granted) {
      this.phase = 'idle';
      this.permissionDenied = true;
      this.permissionMessage = permission.message;
      return;
    }
    this.permissionDenied = false;

    const opened: CoreOpenResult = CoreSession.open(appFilesDir(context));
    if (opened.core === null) {
      this.phase = 'error';
      this.error = opened.error;
      return;
    }
    const recorder: VoiceRecorder = new VoiceRecorder(opened.core, context, new RecorderOptions());
    this.recorder = recorder;
    this.phase = 'preparing';
    try {
      await recorder.start();
      if (!this.active || token !== this.startSeq) {
        await recorder.abort();
        return;
      }
      this.phase = 'recording';
      this.elapsedMs = recorder.receivedMs();
      this.persistedMs = recorder.persistedMs();
      this.refreshHoldHints(recorder);
      this.startTimer();
    } catch (e) {
      if (!this.active || token !== this.startSeq) {
        return;
      }
      this.phase = 'error';
      this.error = uiErrorFromThrown(e as Object);
      this.refreshHoldHints(recorder);
    }
  }

  private async stopRecording(): Promise<void> {
    const recorder: VoiceRecorder | null = this.recorder;
    if (recorder === null) {
      return;
    }
    if (this.phase !== 'recording' && this.phase !== 'error') {
      return;
    }
    this.phase = 'stopping';
    this.error = EMPTY_UI_ERROR;
    this.stopTimer();
    try {
      const result: RecordingFinalizeResult = await recorder.stop('user_stop');
      this.finalized = result;
      this.phase = result.state;
      this.elapsedMs = recorder.receivedMs();
      this.persistedMs = recorder.persistedMs();
      this.interrupted = recorder.wasInterrupted();
      this.refreshHoldHints(recorder);
      // #151：stop() 内部在 finalize 之后自动 commit；这里必须读结果，
      // 否则 commit 失败时用户完全看不到（会误以为已存好）。
      const commit: RecordingCommitResult = recorder.commitResult();
      this.commitState = commit.state;
      this.commitRevision = commit.revision_after;
      this.commitError = commit.error;
      this.commitRetryError = EMPTY_UI_ERROR;
      AppState.bumpDataVersion();
      this.kickRunDue();
    } catch (e) {
      this.finalizeAttempted = true;
      this.phase = 'error';
      this.error = uiErrorFromThrown(e as Object);
      this.refreshHoldHints(recorder);
      // #133：stop() 的异常路径可能没释放常亮/长时任务；abort 兜底（重复调用安全）。
      try {
        await recorder.abort();
      } catch (e2) {
        // 清理失败不掩盖主错误
      }
    }
  }

  private async abortRecording(): Promise<void> {
    const recorder: VoiceRecorder | null = this.recorder;
    this.stopTimer();
    if (recorder !== null) {
      await recorder.abort();
    }
    this.phase = 'aborted';
    this.finalized = null;
    this.error = EMPTY_UI_ERROR;
    toast('已放弃这次录音；未收尾的会话会在下次进入录音页时提示恢复（当前没有丢弃接口）');
  }

  private async saveRecovery(): Promise<void> {
    const recovery: RecordingRecovery | null = this.recovery;
    if (recovery === null || recovery.recording_id === null || this.recoverySaving) {
      return;
    }
    if (recovery.closed_segment_indexes.length === 0) {
      toast('没有已登记的片段，核心会拒绝最终化；这次不能保存。');
      return;
    }
    let last: number = recovery.closed_segment_indexes[0];
    for (let i = 1; i < recovery.closed_segment_indexes.length; i++) {
      if (recovery.closed_segment_indexes[i] > last) {
        last = recovery.closed_segment_indexes[i];
      }
    }
    this.recoverySaving = true;
    this.recoveryError = EMPTY_UI_ERROR;
    const params: FinalizeRecordingParams = {
      recording_id: recovery.recording_id as string,
      last_segment_index: last,
      end_reason: 'recovered_on_startup',
    };
    const result = await CoreGateway.call(this.dbPath, 'recordings.finalize', params);
    this.recoverySaving = false;
    if (!result.ok) {
      this.recoveryError = uiErrorOf(result);
      return;
    }
    const finalized: RecordingFinalizeResult = result.data as RecordingFinalizeResult;
    this.finalized = finalized;
    this.phase = finalized.state;
    // 恢复路径直接调 `recordings.finalize`，没有走 VoiceRecorder.stop()，
    // 因此这次没有尝试提交；如实标成 not_attempted，不冒充「已提交」。
    this.commitState = 'not_attempted';
    this.commitRevision = -1;
    this.commitError = '';
    this.commitRetryError = EMPTY_UI_ERROR;
    this.recovery = null;
    AppState.bumpDataVersion();
    this.kickRunDue();
    toast('已保存上次录音的已登记片段');
  }

  private async kickRunDue(): Promise<void> {
    if (this.runningDue) {
      return;
    }
    this.runningDue = true;
    this.transcribeHint = '转写任务已排队，正在后台运行…';
    const params: RunDueParams = {
      budget_ms: 30000,
      trigger: 'recording',
    };
    const result = await CoreGateway.call(this.dbPath, 'jobs.runDue', params);
    this.runningDue = false;
    if (!result.ok) {
      const mapped: UiError = uiErrorOf(result);
      this.transcribeHint = '转写尚未完成，可稍后在片段页查看。';
      return;
    }
    const summary: JobRunSummary = result.data as JobRunSummary;
    this.transcribeHint = runSummary(summary);
    AppState.bumpDataVersion();
  }

  private recoveryTail(): number {
    const recovery: RecordingRecovery | null = this.recovery;
    if (recovery === null) {
      return 0;
    }
    const tail: number = recovery.received_ms - recovery.persisted_ms;
    return tail > 0 ? tail : 0;
  }

  private finalizeStateLabel(state: string): string {
    if (state === 'saved') {
      return '已保存（完整）';
    }
    if (state === 'recoverable') {
      return '已保存（不完整）';
    }
    return '状态未知';
  }

  /** 提交结果的标题：成功就说成功，失败就明确说「音频已保存、记录仍是草稿」。 */
  private commitHeadline(): string {
    if (this.commitState === 'committed') {
      return '录音已保存';
    }
    if (this.commitState === 'already_committed') {
      return '录音已保存';
    }
    if (this.commitState === 'failed') {
      return '音频已保留，记录保存未完成';
    }
    return '音频已保留，记录待保存';
  }

  private commitDetail(): string {
    if (this.commitState === 'committed' || this.commitState === 'already_committed') {
      return '可在片段页查看录音和转写文字。';
    }
    return '音频已保留，请重试保存。';
  }

  /** 从 `DiaryError code=… retryable=… message=…` 里只取错误码，单独显示给用户。 */
  private async retryCommit(): Promise<void> {
    const finalized: RecordingFinalizeResult | null = this.finalized;
    if (finalized === null || this.commitRetrying) {
      return;
    }
    this.commitRetrying = true;
    this.commitRetryError = EMPTY_UI_ERROR;
    const params: CommitParams = {
      capture_id: finalized.capture_id,
      expected_revision: finalized.capture_revision,
      operation_id: `op-ohos-commit-retry-${finalized.capture_id}-${finalized.capture_revision}`,
    };
    const result = await CoreGateway.call(this.dbPath, 'captures.commit', params);
    this.commitRetrying = false;
    if (!result.ok) {
      const mapped: UiError = uiErrorOf(result);
      if (mapped.code === 'invalid_state') {
        this.commitState = 'already_committed';
        this.commitError = '';
        this.commitRevision = -1;
        AppState.bumpDataVersion();
        return;
      }
      this.commitRetryError = mapped;
      return;
    }
    const committed: CommitResult = result.data as CommitResult;
    this.commitState = 'committed';
    this.commitError = '';
    this.commitRevision = committed.capture.revision;
    AppState.bumpDataVersion();
    toast('记录已保存');
  }

  private recoveryStateLabel(state: string): string {
    if (state === 'recoverable') {
      return '可恢复（有已登记片段）';
    }
    if (state === 'interrupted') {
      return '被系统中断';
    }
    return '状态未知';
  }

  private segmentCount(): number {
    const recorder: VoiceRecorder | null = this.recorder;
    return recorder === null ? 0 : recorder.segmentCount();
  }

  @Builder
  recoveryCard() {
    if (this.recovery !== null && (this.recovery as RecordingRecovery).recording_id !== null &&
      this.finalized === null) {
      Column({ space: 8 }) {
        Row({ space: 8 }) {
          Text('上次录音未收尾')
            .fontSize(15)
            .fontWeight(FontWeight.Medium)
            .fontColor(SketchColors.text)
          StatePill({ text: this.recoveryStateLabel((this.recovery as RecordingRecovery).state), tone: 'warn' })
        }
        .width('100%')
        Text(`已保存 ${formatDuration((this.recovery as RecordingRecovery).persisted_ms)}`)
          .fontSize(14)
          .fontColor(SketchColors.text)
          .width('100%')
        if (this.recoveryTail() > 0) {
          Text(`录音不完整，约有 ${formatDuration(this.recoveryTail())} 未保存。`)
            .fontSize(12)
            .fontColor(SketchColors.warning)
            .width('100%')
        } else {
          Text('可恢复已保存的录音。')
            .fontSize(12)
            .fontColor(SketchColors.muted)
            .width('100%')
        }
        ForEach((this.recovery as RecordingRecovery).notes, (note: string, index: number) => {
        }, (note: string, index: number) => `${index}-${note}`)
        Row({ space: 8 }) {
          Button(this.recoverySaving ? '保存中…' : '保存已录部分')
            .attributeModifier(sketchButton).drawModifier(sketchOutline())
            .fontSize(13)
            .enabled(!this.recoverySaving && (this.recovery as RecordingRecovery).closed_segment_indexes.length > 0)
            .backgroundColor(SketchColors.button)
            .fontColor(SketchColors.ink)
            .onClick(() => {
              this.saveRecovery();
            })
          Button('稍后')
            .attributeModifier(sketchButton).drawModifier(sketchOutline())
            .fontSize(13)
            .enabled(!this.recoverySaving)
            .backgroundColor(SketchColors.soft)
            .fontColor(SketchColors.text)
            .onClick(() => {
              this.recovery = null;
            })
        }
        .width('100%')
        if (this.recoveryError.code.length > 0) {
          ErrorBanner({ error: this.recoveryError, showReload: false, onReload: () => {
          } })
        }
      }
      .width('100%')
      .padding(14)
      // 同 S2：warning_transparent 在本机主题下是不透明红，不能再当卡片底色；
      // 警告信息用正常文字色，warning 只留给 StatePill / 具体风险句子。
      .backgroundColor(SketchColors.soft)
      .borderRadius(0)
      .alignItems(HorizontalAlign.Start)
    }
  }

  @Builder
  finalizeCard() {
    if (this.finalized !== null) {
      Column({ space: 6 }) {
        Row({ space: 8 }) {
          Text('已保存')
            .fontSize(18)
            .fontWeight(FontWeight.Bold)
            .fontColor(SketchColors.text)
          StatePill({
            text: this.finalizeStateLabel((this.finalized as RecordingFinalizeResult).state),
            tone: (this.finalized as RecordingFinalizeResult).state === 'saved' ? 'ok' : 'warn',
          })
        }
        .width('100%')
        InfoRow({ label: '音频时长', value: formatDuration((this.finalized as RecordingFinalizeResult).logical_duration_ms) })
        InfoRow({
          label: '转写任务',
          value: (this.finalized as RecordingFinalizeResult).transcription_queued ? '等待转写' : '没有排队',
        })
        if ((this.finalized as RecordingFinalizeResult).state === 'recoverable') {
          Text('录音不完整，已保留可用部分。')
            .fontSize(12)
            .fontColor(SketchColors.warning)
            .width('100%')
        }
        if (this.interrupted) {
          Text('录音曾被系统中断，部分声音可能缺失。')
            .fontSize(12)
            .fontColor(SketchColors.warning)
            .width('100%')
        }
        Column({ space: 4 }) {
          Text(this.commitHeadline())
            .fontSize(15)
            .fontWeight(FontWeight.Medium)
            .fontColor(SketchColors.text)
            .width('100%')
          Text(this.commitDetail())
            .fontSize(12)
            .fontColor(SketchColors.muted)
            .width('100%')
          if (this.commitState === 'failed') {
            Button(this.commitRetrying ? '保存中…' : '重试保存')
              .attributeModifier(sketchButton).drawModifier(sketchOutline())
              .fontSize(13)
              .enabled(!this.commitRetrying)
              .backgroundColor(SketchColors.button)
              .fontColor(SketchColors.ink)
              .onClick(() => {
                this.retryCommit();
              })
            if (this.commitRetryError.code.length > 0) {
              ErrorBanner({ error: this.commitRetryError, showReload: false, onReload: () => {
              } })
            }
          }
        }
        .width('100%')
        .alignItems(HorizontalAlign.Start)
        if (this.transcribeHint.length > 0) {
          Text(this.transcribeHint)
            .fontSize(12)
            .fontColor(SketchColors.muted)
            .width('100%')
        }
        Row({ space: 8 }) {
          Button('回到今日')
            .attributeModifier(sketchButton).drawModifier(sketchOutline())
            .fontSize(13)
            .backgroundColor(SketchColors.button)
            .fontColor(SketchColors.ink)
            .onClick(() => {
              this.pathStack.pop();
            })
          Button('再录一条')
            .attributeModifier(sketchButton).drawModifier(sketchOutline())
            .fontSize(13)
            .backgroundColor(SketchColors.soft)
            .fontColor(SketchColors.text)
            .onClick(() => {
              this.recordAgain();
            })
        }
        .width('100%')
      }
      .width('100%')
      .padding(14)
      .backgroundColor(SketchColors.paper)
      .drawModifier(sketchOutline())
      .alignItems(HorizontalAlign.Start)
    }
  }

  build() {
    NavDestination() {
      Scroll() {
        Column({ space: 10 }) {
          if (this.phase === 'idle' || this.phase === 'aborted') {
            Column({ space: 12 }) {
              Text('说一条')
                .fontSize(28)
                .fontWeight(FontWeight.Bold)
                .fontColor(SketchColors.text)
                .width('100%')
              Text('录音时请保持屏幕亮起，锁屏会中断录音。')
                .fontSize(13)
                .fontColor(SketchColors.muted)
                .width('100%')
              Text('停止后自动保存并转成文字。')
                .fontSize(13)
                .fontColor(SketchColors.muted)
                .width('100%')
              if (this.phase === 'aborted') {
                Button('重新录音')
                  .attributeModifier(sketchButton).drawModifier(sketchOutline())
                  .fontSize(16)
                  .height(48)
                  .width('100%')
                  .backgroundColor(SketchColors.button)
                  .fontColor(SketchColors.ink)
                  .onClick(() => {
                    this.startRecording();
                  })
              }
              if (this.permissionDenied) {
                Column({ space: 6 }) {
                  Text('需要麦克风权限')
                    .fontSize(15)
                    .fontWeight(FontWeight.Medium)
                    .fontColor(SketchColors.alert)
                    .width('100%')
                  Text(this.permissionMessage.length > 0 ? this.permissionMessage : '系统没有授予麦克风权限。')
                    .fontSize(13)
                    .fontColor(SketchColors.text)
                    .width('100%')
                  Text('请到系统设置 → 应用 → 光写日记 → 权限 → 麦克风 打开，然后回来重新检查。')
                    .fontSize(12)
                    .fontColor(SketchColors.muted)
                    .width('100%')
                  Button('重新检查权限')
                    .attributeModifier(sketchButton).drawModifier(sketchOutline())
                    .fontSize(13)
                    .backgroundColor(SketchColors.soft)
                    .fontColor(SketchColors.text)
                    .onClick(() => {
                      this.startRecording();
                    })
                }
                .width('100%')
                .padding(14)
                .backgroundColor(SketchColors.soft)
                .borderRadius(0)
                .alignItems(HorizontalAlign.Start)
              }
            }
            .width('100%')
            .padding(14)
            .backgroundColor(SketchColors.paper)
            .drawModifier(sketchOutline())
            .alignItems(HorizontalAlign.Start)
          }

          if (this.phase === 'preparing') {
            LoadingRow({ text: '正在准备录音…' })
          }

          if (this.phase === 'recording') {
            Column({ space: 10 }) {
              Text('录音中')
                .fontSize(15)
                .fontColor(SketchColors.muted)
                .width('100%')
              Text(formatDuration(this.persistedMs))
                .fontSize(40)
                .fontWeight(FontWeight.Bold)
                .fontColor(SketchColors.text)
                .width('100%')
              if (this.elapsedMs > this.persistedMs) {
                Text(`正在保存最近 ${formatDuration(this.elapsedMs - this.persistedMs)} 的录音…`)
                  .fontSize(12)
                  .fontColor(SketchColors.warning)
                  .width('100%')
              }
              Button('停止并保存')
                .attributeModifier(sketchButton).drawModifier(sketchOutline())
                .fontSize(16)
                .height(46)
                .width('100%')
                .backgroundColor(SketchColors.button)
                .fontColor(SketchColors.ink)
                .onClick(() => {
                  this.stopRecording();
                })
              Button('放弃录音')
                .attributeModifier(sketchButton).drawModifier(sketchOutline())
                .fontSize(13)
                .width('100%')
                .backgroundColor(SketchColors.soft)
                .fontColor(SketchColors.text)
                .onClick(() => {
                  this.abortRecording();
                })
              if (!this.keepScreenOk) {
                Text('无法保持屏幕常亮；熄屏可能中断录音。')
                  .fontSize(12)
                  .fontColor(SketchColors.warning)
                  .width('100%')
              }
              if (!this.bgTaskOk) {
                Text('请保持应用在前台录音。')
                  .fontSize(12)
                  .fontColor(SketchColors.warning)
                  .width('100%')
              }
            }
            .width('100%')
            .padding(16)
            .backgroundColor(SketchColors.paper)
            .drawModifier(sketchOutline())
            .alignItems(HorizontalAlign.Start)
          }

          if (this.phase === 'stopping') {
            LoadingRow({ text: '正在保存录音…' })
          }

          this.recoveryCard()

          if (this.error.code.length > 0) {
            ErrorBanner({
              error: this.error,
              showReload: false,
              onReload: () => {
              },
            })
          }

          if (this.phase === 'error') {
            Column({ space: 8 }) {
              Text(`已保留 ${this.segmentCount()} 段录音`)
                .fontSize(13)
                .fontColor(SketchColors.muted)
                .width('100%')
              if (this.segmentCount() > 0 && this.finalized === null && !this.finalizeAttempted) {
                Button('保存已录部分')
                  .attributeModifier(sketchButton).drawModifier(sketchOutline())
                  .fontSize(13)
                  .backgroundColor(SketchColors.button)
                  .fontColor(SketchColors.ink)
                  .onClick(() => {
                    this.stopRecording();
                  })
              }
              if (this.finalizeAttempted && this.finalized === null) {
                Text('保存尚未确认，请到片段页查看录音。')
                  .fontSize(12)
                  .fontColor(SketchColors.warning)
                  .width('100%')
              }
              Button('重新开始')
                .attributeModifier(sketchButton).drawModifier(sketchOutline())
                .fontSize(13)
                .backgroundColor(SketchColors.soft)
                .fontColor(SketchColors.text)
                .onClick(() => {
                  this.recordAgain();
                })
            }
            .width('100%')
            .alignItems(HorizontalAlign.Start)
          }

          this.finalizeCard()

          if (this.recoveryLoading) {
            LoadingRow({ text: '检查上次未收尾的录音…' })
          }
          if (this.recoveryError.code.length > 0) {
            ErrorBanner({ error: this.recoveryError, showReload: true, onReload: () => {
              this.loadRecovery();
            } })
          }

          Text('录音中断后，需要重新开始录音。')
            .fontSize(11)
            .fontColor(SketchColors.muted)
            .width('100%')
            .padding({ top: 8, bottom: 24 })
        }
        .width('100%')
        .padding({ left: 16, right: 16, top: 10 })
      }
      .width('100%')
      .height('100%')
      // Scroll 的 align 默认是 Center：内容不足一屏时会被垂直居中（真机 02 截图）。
      // 显式 Top，内容从顶部开始排。
      .align(Alignment.Top)
      .backgroundColor(SketchColors.canvas)
    }
    .title('录音', { backgroundColor: SketchColors.canvas })
    .onBackPressed(() => {
      if (this.phase === 'preparing' || this.phase === 'recording' || this.phase === 'stopping') {
        toast('录音还没结束；请先「停止并保存」或「放弃录音」。');
        return true;
      }
      return false;
    })
  }
}

// 录音页：复用 `recording/VoiceRecorder.ets`（权限 / 常亮 / 分片 / 崩溃恢复）。
//
// 停止后的真实流程：
//   VoiceRecorder.stop() → recordings.finalize → 同事务排转写任务
// 但 **finalize 不 commit**：capture 仍是 draft。当前接口没有 finalize→commit
// （#1019 的修复还没合入），`VoiceRecorder` 也不返回 capture id，所以这里：
//   - 如实显示「已保存，但未提交」；
//   - 不猜 revision、不自动 commit（详情页可以用 captures.get 拿到 revision 后手动提交）；
//   - 转写任务用 `jobs.runDue` 在 worker 里推进，不在 UI 线程加载 228 MB 模型。

import { common } from '@kit.AbilityKit';
import {
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
import { databasePath, formatDuration, toast } from '../ui/AppRuntime';
import { ensureMicrophonePermission, PermissionOutcome } from '../recording/MicPermission';
import { RecorderOptions, VoiceRecorder } from '../recording/VoiceRecorder';
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

  private recorder: VoiceRecorder | null = null;
  private timer: number = -1;
  private context: common.UIAbilityContext | null = null;
  private dbPath: string = '';

  aboutToAppear(): void {
    this.context = getContext(this) as common.UIAbilityContext;
    this.dbPath = databasePath(this.context);
    this.loadRecovery();
  }

  aboutToDisappear(): void {
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

  private async startRecording(): Promise<void> {
    if (this.phase === 'preparing' || this.phase === 'recording' || this.phase === 'stopping') {
      return;
    }
    const context: common.UIAbilityContext | null = this.context;
    if (context === null) {
      return;
    }
    this.error = EMPTY_UI_ERROR;
    this.finalized = null;
    this.transcribeHint = '';
    this.interrupted = false;
    this.finalizeAttempted = false;
    this.permissionMessage = '';

    const permission: PermissionOutcome = await ensureMicrophonePermission(context);
    if (!permission.granted) {
      this.permissionDenied = true;
      this.permissionMessage = permission.message;
      return;
    }
    this.permissionDenied = false;

    const opened: CoreOpenResult = CoreSession.open(context.filesDir);
    if (opened.core === null) {
      this.error = opened.error;
      return;
    }
    const recorder: VoiceRecorder = new VoiceRecorder(opened.core, context, new RecorderOptions());
    this.recorder = recorder;
    this.phase = 'preparing';
    try {
      await recorder.start();
      this.phase = 'recording';
      this.elapsedMs = recorder.receivedMs();
      this.persistedMs = recorder.persistedMs();
      this.refreshHoldHints(recorder);
      this.startTimer();
    } catch (e) {
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
      this.transcribeHint = `转写任务已排队；这次运行没有完成（${mapped.title}：${mapped.message}）`;
      return;
    }
    const summary: JobRunSummary = result.data as JobRunSummary;
    this.transcribeHint = `本轮任务：处理 ${summary.processed}，成功 ${summary.succeeded}，` +
      `失败 ${summary.failed}，积压 ${summary.due_backlog}`;
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
            .fontColor($r('sys.color.ohos_id_color_warning'))
          StatePill({ text: (this.recovery as RecordingRecovery).state, tone: 'warn' })
        }
        .width('100%')
        Text(`已保存 ${formatDuration((this.recovery as RecordingRecovery).persisted_ms)}（已登记 ${(this.recovery as RecordingRecovery).closed_segment_indexes.length} 段）`)
          .fontSize(14)
          .fontColor($r('sys.color.ohos_id_color_text_primary'))
          .width('100%')
        if (this.recoveryTail() > 0) {
          Text(`崩溃前还有约 ${formatDuration(this.recoveryTail())} 未保存（不完整）；收到量只是最后一次上报值，实际可能更多。`)
            .fontSize(12)
            .fontColor($r('sys.color.ohos_id_color_warning'))
            .width('100%')
        } else {
          Text('没有可见的未登记尾巴；这不代表墙上时间一秒没少。')
            .fontSize(12)
            .fontColor($r('sys.color.ohos_id_color_text_secondary'))
            .width('100%')
        }
        ForEach((this.recovery as RecordingRecovery).notes, (note: string, index: number) => {
          Text(`· ${note}`)
            .fontSize(12)
            .fontColor($r('sys.color.ohos_id_color_text_secondary'))
            .width('100%')
        }, (note: string, index: number) => `${index}-${note}`)
        Row({ space: 8 }) {
          Button(this.recoverySaving ? '保存中…' : '保存已录部分')
            .fontSize(13)
            .height(34)
            .enabled(!this.recoverySaving && (this.recovery as RecordingRecovery).closed_segment_indexes.length > 0)
            .backgroundColor($r('sys.color.ohos_id_color_emphasize'))
            .fontColor($r('sys.color.ohos_id_color_text_primary_contrary'))
            .onClick(() => {
              this.saveRecovery();
            })
          Button('稍后')
            .fontSize(13)
            .height(34)
            .enabled(!this.recoverySaving)
            .backgroundColor($r('sys.color.ohos_id_color_sub_background'))
            .fontColor($r('sys.color.ohos_id_color_text_primary'))
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
      .backgroundColor($r('sys.color.ohos_id_color_warning_transparent'))
      .borderRadius(14)
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
            .fontColor($r('sys.color.ohos_id_color_text_primary'))
          StatePill({
            text: (this.finalized as RecordingFinalizeResult).state,
            tone: (this.finalized as RecordingFinalizeResult).state === 'saved' ? 'ok' : 'warn',
          })
        }
        .width('100%')
        InfoRow({ label: '音频时长', value: formatDuration((this.finalized as RecordingFinalizeResult).logical_duration_ms) })
        InfoRow({ label: '片段数', value: `${(this.finalized as RecordingFinalizeResult).segment_count}` })
        InfoRow({
          label: '转写任务',
          value: (this.finalized as RecordingFinalizeResult).transcription_queued ? '已排队（finalize 同事务）' : '没有排队',
        })
        InfoRow({ label: '录音 ID', value: (this.finalized as RecordingFinalizeResult).recording_id })
        if ((this.finalized as RecordingFinalizeResult).state === 'recoverable') {
          Text('这次录音不完整：核心只按已登记片段拼接；缺口如实标成 recoverable。')
            .fontSize(12)
            .fontColor($r('sys.color.ohos_id_color_warning'))
            .width('100%')
        }
        if (this.interrupted) {
          Text('录音被系统中断过（锁屏/来电/抢占），墙上时间可能少录；核心只能保证已登记片段。')
            .fontSize(12)
            .fontColor($r('sys.color.ohos_id_color_warning'))
            .width('100%')
        }
        Text('未提交（草稿）：finalize 不会 commit；这条记录现在还是 draft。当前接口没有 finalize→commit，' +
          'VoiceRecorder 也不返回 capture id。到「今日」打开这条记录，可以用详情页的「提交为正式记录」手动提交。')
          .fontSize(12)
          .fontColor($r('sys.color.ohos_id_color_warning'))
          .width('100%')
        if (this.transcribeHint.length > 0) {
          Text(this.transcribeHint)
            .fontSize(12)
            .fontColor($r('sys.color.ohos_id_color_text_secondary'))
            .width('100%')
        }
        Row({ space: 8 }) {
          Button('回到今日')
            .fontSize(13)
            .height(36)
            .backgroundColor($r('sys.color.ohos_id_color_emphasize'))
            .fontColor($r('sys.color.ohos_id_color_text_primary_contrary'))
            .onClick(() => {
              this.pathStack.pop();
            })
          Button('再录一条')
            .fontSize(13)
            .height(36)
            .backgroundColor($r('sys.color.ohos_id_color_sub_background'))
            .fontColor($r('sys.color.ohos_id_color_text_primary'))
            .onClick(() => {
              this.finalized = null;
              this.phase = 'idle';
              this.error = EMPTY_UI_ERROR;
              this.transcribeHint = '';
              this.recorder = null;
              this.finalizeAttempted = false;
              this.loadRecovery();
            })
        }
        .width('100%')
      }
      .width('100%')
      .padding(14)
      .backgroundColor($r('sys.color.ohos_id_color_card_bg'))
      .borderRadius(14)
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
                .fontColor($r('sys.color.ohos_id_color_text_primary'))
                .width('100%')
              Text('录音期间屏幕会保持常亮（系统限制：熄屏会停止采集，且不会自动恢复）。')
                .fontSize(13)
                .fontColor($r('sys.color.ohos_id_color_text_secondary'))
                .width('100%')
              Text('停止后核心只做 finalize，不 commit；这条记录会以「未提交」出现在今日。')
                .fontSize(13)
                .fontColor($r('sys.color.ohos_id_color_warning'))
                .width('100%')
              Button('开始录音')
                .fontSize(16)
                .height(48)
                .width('100%')
                .backgroundColor($r('sys.color.ohos_id_color_emphasize'))
                .fontColor($r('sys.color.ohos_id_color_text_primary_contrary'))
                .onClick(() => {
                  this.startRecording();
                })
              if (this.permissionDenied) {
                Column({ space: 6 }) {
                  Text('需要麦克风权限')
                    .fontSize(15)
                    .fontWeight(FontWeight.Medium)
                    .fontColor($r('sys.color.ohos_id_color_alert'))
                    .width('100%')
                  Text(this.permissionMessage.length > 0 ? this.permissionMessage : '系统没有授予麦克风权限。')
                    .fontSize(13)
                    .fontColor($r('sys.color.ohos_id_color_text_primary'))
                    .width('100%')
                  Text('请到系统设置 → 应用 → 不写日记 → 权限 → 麦克风 打开，然后回来重新检查。')
                    .fontSize(12)
                    .fontColor($r('sys.color.ohos_id_color_text_secondary'))
                    .width('100%')
                  Button('重新检查权限')
                    .fontSize(13)
                    .height(34)
                    .backgroundColor($r('sys.color.ohos_id_color_sub_background'))
                    .fontColor($r('sys.color.ohos_id_color_text_primary'))
                    .onClick(() => {
                      this.startRecording();
                    })
                }
                .width('100%')
                .padding(14)
                .backgroundColor($r('sys.color.ohos_id_color_alert_transparent'))
                .borderRadius(14)
                .alignItems(HorizontalAlign.Start)
              }
            }
            .width('100%')
            .padding(14)
            .backgroundColor($r('sys.color.ohos_id_color_card_bg'))
            .borderRadius(14)
            .alignItems(HorizontalAlign.Start)
          }

          if (this.phase === 'preparing') {
            LoadingRow({ text: '准备中：权限 / 草稿 / 录音票据 / 采集器…' })
          }

          if (this.phase === 'recording') {
            Column({ space: 10 }) {
              Text('录音中')
                .fontSize(15)
                .fontColor($r('sys.color.ohos_id_color_text_secondary'))
                .width('100%')
              Text(formatDuration(this.persistedMs))
                .fontSize(40)
                .fontWeight(FontWeight.Bold)
                .fontColor($r('sys.color.ohos_id_color_text_primary'))
                .width('100%')
              Text(`已登记 ${this.segmentCount()} 段；已收到 ${formatDuration(this.elapsedMs)}`)
                .fontSize(12)
                .fontColor($r('sys.color.ohos_id_color_text_secondary'))
                .width('100%')
              if (this.elapsedMs > this.persistedMs) {
                Text(`还有约 ${formatDuration(this.elapsedMs - this.persistedMs)} 未登记（进程崩溃会丢；时长以已登记为准）`)
                  .fontSize(12)
                  .fontColor($r('sys.color.ohos_id_color_warning'))
                  .width('100%')
              }
              Button('停止并保存')
                .fontSize(16)
                .height(46)
                .width('100%')
                .backgroundColor($r('sys.color.ohos_id_color_emphasize'))
                .fontColor($r('sys.color.ohos_id_color_text_primary_contrary'))
                .onClick(() => {
                  this.stopRecording();
                })
              Button('放弃录音')
                .fontSize(13)
                .height(36)
                .width('100%')
                .backgroundColor($r('sys.color.ohos_id_color_sub_background'))
                .fontColor($r('sys.color.ohos_id_color_text_primary'))
                .onClick(() => {
                  this.abortRecording();
                })
              if (!this.keepScreenOk) {
                Text('无法保持屏幕常亮；熄屏可能中断录音。')
                  .fontSize(12)
                  .fontColor($r('sys.color.ohos_id_color_warning'))
                  .width('100%')
              }
              if (!this.bgTaskOk) {
                Text('后台录音不可用（长时任务申请失败）；请保持应用在前台。')
                  .fontSize(12)
                  .fontColor($r('sys.color.ohos_id_color_warning'))
                  .width('100%')
              }
            }
            .width('100%')
            .padding(16)
            .backgroundColor($r('sys.color.ohos_id_color_card_bg'))
            .borderRadius(14)
            .alignItems(HorizontalAlign.Start)
          }

          if (this.phase === 'stopping') {
            LoadingRow({ text: '保存中：封闭最后一段 → 登记 → finalize…' })
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
              Text(`当前录音：${this.segmentCount()} 个已登记片段`)
                .fontSize(13)
                .fontColor($r('sys.color.ohos_id_color_text_secondary'))
                .width('100%')
              if (this.segmentCount() > 0 && this.finalized === null && !this.finalizeAttempted) {
                Button('保存已录部分')
                  .fontSize(13)
                  .height(36)
                  .backgroundColor($r('sys.color.ohos_id_color_emphasize'))
                  .fontColor($r('sys.color.ohos_id_color_text_primary_contrary'))
                  .onClick(() => {
                    this.stopRecording();
                  })
              }
              if (this.finalizeAttempted && this.finalized === null) {
                Text('最终化已经尝试过；不要再重复保存（核心可能报 invalid_state / integrity_failed）。')
                  .fontSize(12)
                  .fontColor($r('sys.color.ohos_id_color_warning'))
                  .width('100%')
              }
              Button('重新开始')
                .fontSize(13)
                .height(36)
                .backgroundColor($r('sys.color.ohos_id_color_sub_background'))
                .fontColor($r('sys.color.ohos_id_color_text_primary'))
                .onClick(() => {
                  this.phase = 'idle';
                  this.error = EMPTY_UI_ERROR;
                  this.recorder = null;
                  this.finalizeAttempted = false;
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

          Text('平台限制：熄屏/锁屏时系统会强制停止采集，亮屏也不会自动续录；应用只能保持常亮，不能绕过。')
            .fontSize(11)
            .fontColor($r('sys.color.ohos_id_color_text_tertiary'))
            .width('100%')
            .padding({ top: 8, bottom: 24 })
        }
        .width('100%')
        .padding({ left: 16, right: 16, top: 10 })
      }
      .width('100%')
      .height('100%')
      .backgroundColor($r('sys.color.ohos_id_color_background'))
    }
    .title('录音')
    .onBackPressed(() => {
      if (this.phase === 'preparing' || this.phase === 'recording' || this.phase === 'stopping') {
        toast('录音还没结束；请先「停止并保存」或「放弃录音」。');
        return true;
      }
      return false;
    })
  }
}

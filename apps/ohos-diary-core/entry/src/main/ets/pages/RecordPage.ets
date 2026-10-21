// 记录（契约 §2.1）：新建一条原始记录——写下来，或者说一段。
//
// 这一页承担从旧「今日」页拆出来的新建职责：
// - **文字**：`captures.createDraft` → `captures.saveDraft`（「保存草稿」），
//   「收好」再 `captures.commit` 提交成正式记录（出现在「片段」里）；
// - **录音**：跳到现有的 `RecordingPage`（真实采集链路：prepare → 分片 → finalize）。
//
// 不虚构状态：
// - 只有 `saveDraft` 返回 `durable === true` 才显示「已保存」；
// - 提交后这篇记录就不能再在这里改（核心的 saveDraft 只接受草稿），只给「再写一条」与
//   「查看这条」；要改正文去记录详情/后续的编辑入口；
// - 日期取当前本地日期（这一版没有日期选择器；`occurred_at` 在创建草稿时由核心落库）。

import { common } from '@kit.AbilityKit';
import {
  Capture,
  CommitParams,
  CommitResult,
  CreateDraftParams,
  DraftSaveResult,
  SaveDraftParams,
} from '../core/Models';
import { CoreGateway, uiErrorOf } from '../ui/CoreGateway';
import { EMPTY_UI_ERROR, UiError } from '../ui/Errors';
import { AppState } from '../ui/AppState';
import {
  databasePath,
  dayTitleAtOffset,
  newOperationId,
  toast,
} from '../ui/AppRuntime';
import { readTimeZone } from '../recording/VoiceRecorder';
import { ErrorBanner, InfoRow, PageTitle, SectionTitle, StatePill } from '../ui/Common';

@Component
export struct RecordPage {
  pathStack: NavPathStack = new NavPathStack();
  @State text: string = '';
  @State busy: boolean = false;
  @State error: UiError = EMPTY_UI_ERROR;
  @State message: string = '';
  @State captureId: string = '';
  @State revision: number = 0;
  /** 空串（还没保存）/ `draft` / `committed`。 */
  @State savedState: string = '';
  @State savedAt: string = '';

  private dbPath: string = '';

  aboutToAppear(): void {
    const context = getContext(this) as common.UIAbilityContext;
    this.dbPath = databasePath(context);
  }

  private canSave(): boolean {
    return !this.busy && this.savedState !== 'committed' && this.text.trim().length > 0;
  }

  private stateLabel(): string {
    if (this.savedState === 'draft') {
      return '草稿';
    }
    if (this.savedState === 'committed') {
      return '已提交';
    }
    return '未保存';
  }

  private stateTone(): string {
    if (this.savedState === 'draft') {
      return 'warn';
    }
    if (this.savedState === 'committed') {
      return 'ok';
    }
    return 'muted';
  }

  /**
   * 保存（`submit = false`）或收好（`submit = true`）。
   *
   * 第一次保存先 `captures.createDraft`（核心的 createDraft 只收时间与时区，不收正文），
   * 再按返回的 `revision` 写正文；提交用保存后的 revision，不猜。
   */
  private async save(submit: boolean): Promise<void> {
    if (!this.canSave()) {
      return;
    }
    this.busy = true;
    this.error = EMPTY_UI_ERROR;
    this.message = '';
    const now: Date = new Date();

    if (this.captureId.length === 0) {
      const draftParams: CreateDraftParams = {
        occurred_at: now.toISOString(),
        time_zone: readTimeZone(),
        utc_offset_minutes: -now.getTimezoneOffset(),
        operation_id: newOperationId('op-ohos-record'),
      };
      const created = await CoreGateway.call(this.dbPath, 'captures.createDraft', draftParams);
      if (!created.ok) {
        this.busy = false;
        this.error = uiErrorOf(created);
        return;
      }
      const capture: Capture = created.data as Capture;
      this.captureId = capture.id;
      this.revision = capture.revision;
    }

    const saveParams: SaveDraftParams = {
      capture_id: this.captureId,
      text: this.text,
      expected_revision: this.revision,
      operation_id: newOperationId('op-ohos-save'),
    };
    const saved = await CoreGateway.call(this.dbPath, 'captures.saveDraft', saveParams);
    if (!saved.ok) {
      this.busy = false;
      this.error = uiErrorOf(saved);
      return;
    }
    const receipt: DraftSaveResult = saved.data as DraftSaveResult;
    this.revision = receipt.revision;
    this.savedAt = receipt.saved_at;
    this.savedState = 'draft';
    this.message = receipt.durable ? '草稿已保存。' : '核心没有确认落盘（durable=false），先别离开这一页。';

    if (submit) {
      const commitParams: CommitParams = {
        capture_id: this.captureId,
        expected_revision: this.revision,
        operation_id: newOperationId('op-ohos-commit'),
      };
      const committed = await CoreGateway.call(this.dbPath, 'captures.commit', commitParams);
      if (!committed.ok) {
        this.busy = false;
        this.error = uiErrorOf(committed);
        return;
      }
      const result: CommitResult = committed.data as CommitResult;
      this.revision = result.capture.revision;
      this.savedState = 'committed';
      this.message = '已收好（提交为正式记录），可以在「片段」里看到它。';
    } else {
      toast('草稿已保存');
    }
    this.busy = false;
    AppState.bumpDataVersion();
  }

  /** 清空这一页的状态，开始写新的一条（不动已经提交的记录）。 */
  private startNew(): void {
    this.text = '';
    this.captureId = '';
    this.revision = 0;
    this.savedState = '';
    this.savedAt = '';
    this.message = '';
    this.error = EMPTY_UI_ERROR;
  }

  build() {
    Scroll() {
      Column({ space: 12 }) {
        Row({ space: 8 }) {
          Column() {
            PageTitle({ title: '记录', subtitle: '新建一条原始记录：写下来，或者说一段。' })
          }
          .layoutWeight(1)
          .alignItems(HorizontalAlign.Start)
        }
        .width('100%')
        .padding({ top: 8 })

        // ---------------------------------------------------------- 文字
        Column({ space: 10 }) {
          Row({ space: 8 }) {
            Text(dayTitleAtOffset(0))
              .fontSize(13)
              .fontColor($r('sys.color.ohos_id_color_text_secondary'))
            StatePill({ text: this.stateLabel(), tone: this.stateTone() })
            Blank()
            Text(`${this.text.length} 字`)
              .fontSize(12)
              .fontColor($r('sys.color.ohos_id_color_text_tertiary'))
          }
          .width('100%')

          TextArea({ text: this.text, placeholder: '今天想留下什么？写几句就好。' })
            .height(200)
            .width('100%')
            .fontSize(16)
            .enabled(this.savedState !== 'committed')
            .backgroundColor($r('sys.color.ohos_id_color_sub_background'))
            .onChange((value: string) => {
              this.text = value;
            })

          Row({ space: 8 }) {
            if (this.savedState === 'committed') {
              Button('再写一条')
                .fontSize(14)
                .height(38)
                .backgroundColor($r('sys.color.ohos_id_color_emphasize'))
                .fontColor($r('sys.color.ohos_id_color_text_primary_contrary'))
                .onClick(() => {
                  this.startNew();
                })
              Button('查看这条')
                .fontSize(14)
                .height(38)
                .backgroundColor($r('sys.color.ohos_id_color_sub_background'))
                .fontColor($r('sys.color.ohos_id_color_text_primary'))
                .onClick(() => {
                  this.pathStack.pushPathByName('capture', this.captureId);
                })
            } else {
              Button(this.busy ? '保存中…' : '保存草稿')
                .fontSize(14)
                .height(38)
                .enabled(this.canSave())
                .backgroundColor($r('sys.color.ohos_id_color_sub_background'))
                .fontColor($r('sys.color.ohos_id_color_text_primary'))
                .onClick(() => {
                  this.save(false);
                })
              Button(this.busy ? '提交中…' : '收好（提交）')
                .fontSize(14)
                .height(38)
                .enabled(this.canSave())
                .backgroundColor($r('sys.color.ohos_id_color_emphasize'))
                .fontColor($r('sys.color.ohos_id_color_text_primary_contrary'))
                .onClick(() => {
                  this.save(true);
                })
            }
            Blank()
          }
          .width('100%')

          if (this.message.length > 0) {
            Text(this.message)
              .fontSize(12)
              .fontColor($r('sys.color.ohos_id_color_text_secondary'))
              .width('100%')
          }
          if (this.savedAt.length > 0) {
            InfoRow({ label: '保存时间', value: this.savedAt })
          }
          if (this.error.code.length > 0) {
            ErrorBanner({ error: this.error, showReload: false, onReload: () => {
            } })
          }
        }
        .width('100%')
        .padding(14)
        .backgroundColor($r('sys.color.ohos_id_color_card_bg'))
        .borderRadius(14)
        .alignItems(HorizontalAlign.Start)

        // ---------------------------------------------------------- 录音
        Column({ space: 6 }) {
          SectionTitle({ text: '说一段' })
          Text('录音会走真实的采集链路：先建草稿、再分片登记，停止后自动收尾并排转写任务。')
            .fontSize(13)
            .fontColor($r('sys.color.ohos_id_color_text_secondary'))
            .width('100%')
          Row({ space: 8 }) {
            Button('开始录音')
              .fontSize(14)
              .height(38)
              .backgroundColor($r('sys.color.ohos_id_color_emphasize'))
              .fontColor($r('sys.color.ohos_id_color_text_primary_contrary'))
              .onClick(() => {
                this.pathStack.pushPathByName('recording', '');
              })
            Blank()
          }
          .width('100%')
        }
        .width('100%')
        .padding(14)
        .backgroundColor($r('sys.color.ohos_id_color_card_bg'))
        .borderRadius(14)
        .alignItems(HorizontalAlign.Start)

        Text('文字先落成草稿；「收好」把草稿提交为正式记录。片段、日记、搜索都只读已落库的内容。')
          .fontSize(11)
          .fontColor($r('sys.color.ohos_id_color_text_tertiary'))
          .width('100%')
          .padding({ bottom: 24 })
      }
      .width('100%')
      .padding({ left: 16, right: 16 })
    }
    .width('100%')
    .height('100%')
    .backgroundColor($r('sys.color.ohos_id_color_background'))
  }
}
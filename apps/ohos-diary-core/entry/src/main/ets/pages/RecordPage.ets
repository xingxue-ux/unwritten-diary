import { LengthMetrics } from '@kit.ArkUI';
import { SketchColors, SketchFonts, SketchType, SketchCard, sketchButton, sketchInput, sketchArea, sketchOutline } from '../ui/Sketch';
// 记录（契约 §2.1）：新建一条原始记录——写下来，或者说一段。
//
// 这一页承担从旧「今日」页拆出来的新建职责：
// - **文字**：「保存」依次创建、持久化并提交为正式记录，零散原文也直接保存；
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
  CaptureIdParams,
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
  formatDateTime,
  newOperationId,
} from '../ui/AppRuntime';
import { readTimeZone } from '../recording/VoiceRecorder';
import { ErrorBanner, InfoRow, PageTitle, SectionTitle, StatePill } from '../ui/Common';

@Component
export struct RecordPage {
  pathStack: NavPathStack = new NavPathStack();
  @StorageProp('ohosWideLayout') desktop: boolean = false;
  @State text: string = '';
  @State busy: boolean = false;
  @State error: UiError = EMPTY_UI_ERROR;
  @State message: string = '';
  @State captureId: string = '';
  @State revision: number = 0;
  /** 空串（还没保存）/ `draft` / `committed`。 */
  @State savedState: string = '';
  @State savedAt: string = '';
  @State preservedText: string = '';
  @StorageLink('ohosDataVersion') @Watch('onDataVersionChanged') dataVersion: number = 0;

  private dbPath: string = '';
  private savedText: string = '';
  private refreshSeq: number = 0;

  aboutToAppear(): void {
    const context = getContext(this) as common.UIAbilityContext;
    this.dbPath = databasePath(context);
  }

  onDataVersionChanged(): void {
    this.refreshCapture();
  }

  private async refreshCapture(): Promise<void> {
    if (this.captureId.length === 0 || this.busy) {
      return;
    }
    const id: string = this.captureId;
    const token: number = ++this.refreshSeq;
    const params: CaptureIdParams = { capture_id: id };
    const result = await CoreGateway.call(this.dbPath, 'captures.get', params);
    if (token !== this.refreshSeq || id !== this.captureId || this.busy) {
      return;
    }
    if (!result.ok) {
      this.error = uiErrorOf(result);
      return;
    }
    const capture: Capture = result.data as Capture;
    this.error = EMPTY_UI_ERROR;
    if (capture.revision === this.revision && capture.state === this.savedState) {
      return;
    }
    // Keep local edits separately before adopting the authoritative revision/state.
    // Never replay them into a record that was committed on another page.
    const dirty: boolean = this.text !== this.savedText;
    if (dirty) {
      this.preservedText = this.text;
    }
    this.text = capture.draft_text;
    this.savedText = capture.draft_text;
    this.revision = capture.revision;
    this.savedState = capture.state;
    this.error = EMPTY_UI_ERROR;
    this.message = dirty
      ? '这条记录已在其他页面更新。未保存的修改已保留，可另存为新记录。'
      : (capture.state === 'committed' ? '这条记录已保存，可以再写一条。' : '已刷新记录，请重试保存。');
  }

  private savePreservedAsNew(): void {
    const text: string = this.preservedText;
    this.startNew();
    this.text = text;
  }

  private canSave(): boolean {
    return !this.busy && this.savedState !== 'committed' && this.text.trim().length > 0;
  }

  private stateLabel(): string {
    if (this.savedState === 'draft') {
      return '待保存';
    }
    if (this.savedState === 'committed') {
      return '已保存';
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
   * 一次点击直接保存为正式记录，不判断文字是否完整，也不改写原文。
   *
   * 第一次保存先 `captures.createDraft`（核心的 createDraft 只收时间与时区，不收正文），
   * 再按返回的 `revision` 写正文；提交用保存后的 revision，不猜。
   */
  private async save(): Promise<void> {
    if (!this.canSave()) {
      return;
    }
    this.busy = true;
    this.refreshSeq++;
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

    const savedText: string = this.text;
    const saveParams: SaveDraftParams = {
      capture_id: this.captureId,
      text: savedText,
      expected_revision: this.revision,
      operation_id: newOperationId('op-ohos-save'),
    };
    const saved = await CoreGateway.call(this.dbPath, 'captures.saveDraft', saveParams);
    if (!saved.ok) {
      this.busy = false;
      this.error = uiErrorOf(saved);
      if (this.error.code === 'revision_conflict' || this.error.code === 'invalid_state') {
        await this.refreshCapture();
      }
      return;
    }
    const receipt: DraftSaveResult = saved.data as DraftSaveResult;
    this.revision = receipt.revision;
    this.savedAt = receipt.durable ? receipt.saved_at : '';
    this.savedState = 'draft';
    this.savedText = savedText;
    this.message = receipt.durable ? '' : '保存尚未确认，请暂时留在本页。';
    if (!receipt.durable) {
      this.busy = false;
      AppState.bumpDataVersion();
      return;
    }

    const commitParams: CommitParams = {
      capture_id: this.captureId,
      expected_revision: this.revision,
      operation_id: newOperationId('op-ohos-commit'),
    };
    const committed = await CoreGateway.call(this.dbPath, 'captures.commit', commitParams);
    if (!committed.ok) {
      this.busy = false;
      this.message = '内容已保留，请重试保存。';
      this.error = uiErrorOf(committed);
      if (this.error.code === 'revision_conflict' || this.error.code === 'invalid_state') {
        await this.refreshCapture();
      }
      return;
    }
    const result: CommitResult = committed.data as CommitResult;
    this.revision = result.capture.revision;
    this.savedState = 'committed';
    this.message = '已保存，可在片段页查看。';
    this.busy = false;
    AppState.bumpDataVersion();
  }

  /** 清空这一页的状态，开始写新的一条（不动已经提交的记录）。 */
  private startNew(): void {
    this.refreshSeq++;
    this.text = '';
    this.captureId = '';
    this.revision = 0;
    this.savedState = '';
    this.savedAt = '';
    this.message = '';
    this.error = EMPTY_UI_ERROR;
    this.savedText = '';
    this.preservedText = '';
  }

  @Builder
  introduction() {
    Column({ space: 18 }) {
      Text(dayTitleAtOffset(0))
        .fontSize(this.desktop ? 17 : 14)
        .fontColor(SketchColors.muted)
      Text(this.desktop ? '今天，\n想留住什么？' : '今天，想留住什么？')
        .fontFamily(SketchFonts.display)
        .fontSize(this.desktop ? 52 : 42)
        .fontColor(SketchColors.ink)
        .lineHeight(this.desktop ? 70 : 56)
        .margin({ top: 8 })
      Text('从眼前这一刻开始就好。')
        .fontSize(17)
        .fontColor(SketchColors.muted)
      Text('记录保存在本地，写下来，或者说一段。')
        .fontSize(13)
        .fontColor(SketchColors.text)
        .padding(20)
        .backgroundColor(SketchColors.soft)
        .drawModifier(sketchOutline())
        .margin({ top: 24 })
    }
    .alignItems(HorizontalAlign.Start)
    .width('100%')
  }

  @Builder
  editor() {
    Column({ space: 20 }) {
      SketchCard({ contentPadding: this.desktop ? 28 : 21 }) {
        Column({ space: 16 }) {
          Text('此刻的记录')
            .fontFamily(SketchFonts.display)
            .fontSize(31)
            .fontColor(SketchColors.ink)
          Divider().color(SketchColors.ink).strokeWidth(0.8)
          TextArea({ text: this.text, placeholder: '今天路过花店，看到一束很明亮的橘色花…' }).attributeModifier(sketchArea)
            .height(this.desktop ? 295 : 220)
            .width('100%')
            .fontSize(18)
            .fontColor(SketchColors.text)
            .placeholderColor(SketchColors.muted)
            .backgroundColor(SketchColors.paper)
            .borderRadius(0)
            .padding(0)
            .enabled(!this.busy && this.savedState !== 'committed')
            .onChange((value: string) => { this.text = value; })
          Divider().color(SketchColors.ink).strokeWidth(0.8)
          Row({ space: 8 }) {
            StatePill({ text: this.stateLabel(), tone: this.stateTone() })
            Blank()
            Text(`${this.text.length} 字`).fontSize(13).fontColor(SketchColors.muted)
          }
          .width('100%')
          Flex({ wrap: FlexWrap.Wrap, space: { main: LengthMetrics.vp(8), cross: LengthMetrics.vp(8) } }) {
            Button(this.busy ? '保存中…' : (this.savedState === 'committed' ? '已保存' : '保存'))
              .attributeModifier(sketchButton).drawModifier(sketchOutline())
              .backgroundColor(SketchColors.button)
              .enabled(this.canSave())
              .onClick(() => { this.save(); })
            Button('录音')
              .attributeModifier(sketchButton).drawModifier(sketchOutline())
              .enabled(!this.busy)
              .onClick(() => { this.pathStack.pushPathByName('recording', ''); })
          }
          if (this.message.length > 0) {
            Text(this.message).fontSize(13).fontColor(SketchColors.muted).width('100%')
          }
          if (this.savedAt.length > 0) {
            InfoRow({ label: '保存时间', value: formatDateTime(this.savedAt) })
          }
          ErrorBanner({ error: this.error, showReload: !this.busy, onReload: () => { this.refreshCapture(); } })
        }
        .width('100%')
        .alignItems(HorizontalAlign.Start)
      }
      if (this.preservedText.length > 0) {
        Button('将未保存的修改另存为新记录')
          .attributeModifier(sketchButton).drawModifier(sketchOutline())
          .enabled(!this.busy)
          .onClick(() => { this.savePreservedAsNew(); })
      }
      if (this.savedState === 'committed') {
        Flex({ wrap: FlexWrap.Wrap }) {
          Button('再写一条').attributeModifier(sketchButton).drawModifier(sketchOutline()).onClick(() => { this.startNew(); })
          Button('查看这条').attributeModifier(sketchButton).drawModifier(sketchOutline())
            .onClick(() => { this.pathStack.pushPathByName('capture', this.captureId); })
        }
      }
    }
    .width('100%')
    .alignItems(HorizontalAlign.Start)
  }

  build() {
    Scroll() {
      Column() {
        if (this.desktop) {
          Row({ space: 32 }) {
            Column() { this.introduction() }.layoutWeight(4).padding({ top: 20 })
            Divider().vertical(true).height(575).color(SketchColors.ink)
            Column() { this.editor() }.layoutWeight(7)
          }
          .alignItems(VerticalAlign.Top)
          .width('100%')
        } else {
          this.introduction()
          Column() { this.editor() }.margin({ top: 24 })
        }
      }
      .width('100%')
      .constraintSize({ maxWidth: 1240 })
      .padding({ left: 16, right: 16, top: 12, bottom: 30 })
      .alignItems(HorizontalAlign.Start)
    }
    .width('100%')
    .height('100%')
    .backgroundColor(SketchColors.canvas)
  }
}

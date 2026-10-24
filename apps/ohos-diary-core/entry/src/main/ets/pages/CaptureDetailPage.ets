import { SketchColors, SketchFonts, SketchType, SketchCard, sketchButton, sketchOutline } from '../ui/Sketch';
// 记录/详情：`captures.get` 显示来源与转写；草稿可手动提交为正式记录。
//
// 「未提交」是如实状态，不是装饰：
// - `recordings.finalize` 只把音频最终化并排转写任务；#151 起 `VoiceRecorder.stop()`
//   会在 finalize 之后自动 commit 一次，提交结果由录音页读取 `commitResult()` 显示；
// - 但如果那次提交失败，记录仍会是 `draft`；从「上次录音未收尾」恢复保存的路径
//   也只 finalize、不 commit。所以草稿状态仍然存在，本页保留手动提交入口；
// - 本页用 `captures.commit`（revision 来自 captures.get，不猜）；不提交也照样能看到来源与转写。

import { common } from '@kit.AbilityKit';
import {
  Capture,
  CaptureIdParams,
  CommitParams,
  CommitResult,
  ExtractedContent,
  ExtractedSegment,
  ExtractParams,
} from '../core/Models';
import { CoreGateway, CoreRequest, CoreResult, uiErrorOf } from '../ui/CoreGateway';
import { EMPTY_UI_ERROR, UiError } from '../ui/Errors';
import { AppState } from '../ui/AppState';
import {
  captureStateLabel,
  coverageLabel,
  databasePath,
  extractStatusLabel,
  extractStatusTone,
  formatDateTime,
  formatDuration,
  newOperationId,
  toast,
} from '../ui/AppRuntime';
import { EmptyState, ErrorBanner, InfoRow, LoadingRow, SectionTitle, StatePill } from '../ui/Common';

interface SourceView {
  sourceId: string;
  extracted: ExtractedContent | null;
  error: UiError;
}

@Component
export struct CaptureDetailPage {
  pathStack: NavPathStack = new NavPathStack();
  captureId: string = '';
  @State capture: Capture | null = null;
  @State sources: SourceView[] = [];
  @State loading: boolean = true;
  @State error: UiError = EMPTY_UI_ERROR;
  @State commitBusy: boolean = false;
  @State commitError: UiError = EMPTY_UI_ERROR;

  private dbPath: string = '';

  aboutToAppear(): void {
    const context = getContext(this) as common.UIAbilityContext;
    this.dbPath = databasePath(context);
    this.reload();
  }

  private async reload(): Promise<void> {
    this.loading = true;
    this.error = EMPTY_UI_ERROR;
    this.commitError = EMPTY_UI_ERROR;
    const params: CaptureIdParams = { capture_id: this.captureId };
    const result = await CoreGateway.call(this.dbPath, 'captures.get', params);
    if (!result.ok) {
      this.loading = false;
      this.error = uiErrorOf(result);
      return;
    }
    const capture: Capture = result.data as Capture;

    const requests: CoreRequest[] = [];
    for (let i = 0; i < capture.ordered_source_ids.length; i++) {
      const extractParams: ExtractParams = { source_ref: capture.ordered_source_ids[i] };
      const request: CoreRequest = { method: 'sources.extract', params: extractParams };
      requests.push(request);
    }
    let extractResults: CoreResult[] = [];
    if (requests.length > 0) {
      extractResults = await CoreGateway.callMany(this.dbPath, requests);
    }
    const views: SourceView[] = [];
    for (let i = 0; i < capture.ordered_source_ids.length; i++) {
      let extracted: ExtractedContent | null = null;
      let extractError: UiError = EMPTY_UI_ERROR;
      if (i < extractResults.length) {
        if (extractResults[i].ok) {
          extracted = extractResults[i].data as ExtractedContent;
        } else {
          extractError = uiErrorOf(extractResults[i]);
        }
      }
      views.push({
        sourceId: capture.ordered_source_ids[i],
        extracted: extracted,
        error: extractError,
      });
    }
    this.capture = capture;
    this.sources = views;
    this.loading = false;
  }

  private sourceTitle(source: SourceView, index: number): string {
    if (source.extracted !== null) {
      const id: string = source.extracted.extractor_id;
      if (id === 'local_transcription') {
        return '语音转写';
      }
      if (id === 'media_metadata_only') {
        return '音频（还没有转写）';
      }
      if (id === 'text_direct' || id === 'plain_text' || id === 'markdown') {
        return '文字来源';
      }
    }
    return `来源 ${index + 1}`;
  }

  private sourceStatus(source: SourceView): string {
    if (source.error.code.length > 0) {
      return '状态未知';
    }
    if (source.extracted === null) {
      return '等待处理';
    }
    return extractStatusLabel(source.extracted.status);
  }

  private sourceTone(source: SourceView): string {
    if (source.error.code.length > 0) {
      return 'warn';
    }
    if (source.extracted === null) {
      return 'muted';
    }
    return extractStatusTone(source.extracted.status);
  }

  private segmentLabel(segment: ExtractedSegment): string {
    const locator = segment.locator;
    if (locator.start_ms !== null && locator.end_ms !== null) {
      return `${formatDuration(locator.start_ms)} – ${formatDuration(locator.end_ms)}`;
    }
    if (locator.text_start !== null && locator.text_end !== null) {
      return `第 ${locator.text_start}–${locator.text_end} 字`;
    }
    return `#${segment.ordinal}`;
  }

  private sourceBody(source: SourceView): string {
    if (source.extracted === null) {
      return '';
    }
    const text: string = source.extracted.text.trim();
    if (text.length > 0) {
      return text;
    }
    if (source.extracted.status === 'pending') {
      return '转写任务还在排队；稍后回到这条记录再看。';
    }
    if (source.extracted.status === 'processing') {
      return '转写正在进行；稍后回到这条记录再看。';
    }
    if (source.extracted.status === 'failed') {
      return '处理未完成，请稍后重试。';
    }
    return '这条来源还没有可显示的正文。';
  }

  private async commit(): Promise<void> {
    const capture: Capture | null = this.capture;
    if (capture === null || this.commitBusy) {
      return;
    }
    this.commitBusy = true;
    this.commitError = EMPTY_UI_ERROR;
    const params: CommitParams = {
      capture_id: capture.id,
      expected_revision: capture.revision,
      operation_id: newOperationId('ui-commit'),
    };
    const result = await CoreGateway.call(this.dbPath, 'captures.commit', params);
    this.commitBusy = false;
    if (!result.ok) {
      this.commitError = uiErrorOf(result);
      return;
    }
    const committed: CommitResult = result.data as CommitResult;
    this.capture = committed.capture;
    AppState.bumpDataVersion();
    toast('记录已收好');
    await this.reload();
  }

  @Builder
  sourceCard(source: SourceView, index: number) {
    Column({ space: 8 }) {
      Row({ space: 8 }) {
        Text(this.sourceTitle(source, index))
          .fontSize(14)
          .fontWeight(FontWeight.Medium)
          .fontColor(SketchColors.text)
        StatePill({ text: this.sourceStatus(source), tone: this.sourceTone(source) })
        Blank()
        if (source.extracted !== null) {
          Text(coverageLabel(source.extracted.coverage))
            .fontSize(11)
            .fontColor(source.extracted.coverage === 'complete'
              ? SketchColors.muted
              : SketchColors.warning)
        }
      }
      .width('100%')
      if (source.error.code.length > 0) {
        ErrorBanner({ error: source.error, showReload: false, onReload: () => {
        } })
      }
      Text(this.sourceBody(source))
        .fontSize(15)
        .fontColor(SketchColors.text)
        .width('100%')
        .lineHeight(23)
      if (source.extracted !== null && source.extracted.coverage_reason !== null) {
        Text('部分内容暂时无法读取。')
          .fontSize(12)
          .fontColor(SketchColors.warning)
          .width('100%')
      }
      if (source.extracted !== null && source.extracted.error_code !== null) {
        Text('处理未完成，请稍后重试。')
          .fontSize(12)
          .fontColor(SketchColors.alert)
          .width('100%')
      }
      if (source.extracted !== null && source.extracted.segments.length > 0) {
        Column({ space: 6 }) {
          Text('分段时间轴')
            .fontSize(12)
            .fontColor(SketchColors.muted)
            .width('100%')
          ForEach((source.extracted as ExtractedContent).segments, (segment: ExtractedSegment) => {
            Row({ space: 8 }) {
              Text(this.segmentLabel(segment))
                .fontSize(11)
                .fontColor(SketchColors.muted)
                .width(110)
                .flexShrink(0)
              Text(segment.text)
                .fontSize(13)
                .fontColor(SketchColors.text)
                .layoutWeight(1)
            }
            .width('100%')
            .alignItems(VerticalAlign.Top)
          }, (segment: ExtractedSegment) => `${segment.ordinal}-${segment.locator.start_ms}-${segment.locator.text_start}`)
        }
        .width('100%')
        .padding(10)
        .backgroundColor(SketchColors.soft)
        .borderRadius(0)
      }
    }
    .width('100%')
    .padding(14)
    .backgroundColor(SketchColors.paper)
    .drawModifier(sketchOutline())
    .alignItems(HorizontalAlign.Start)
  }

  build() {
    NavDestination() {
      Scroll() {
        Column({ space: 10 }) {
          Row() {
            Blank()
            Button('刷新')
              .attributeModifier(sketchButton).drawModifier(sketchOutline())
              .fontSize(12)
              .backgroundColor(SketchColors.soft)
              .fontColor(SketchColors.text)
              .onClick(() => {
                this.reload();
              })
          }
          .width('100%')
          if (this.loading) {
            LoadingRow({ text: '读取记录…' })
          }
          if (this.error.code.length > 0) {
            ErrorBanner({
              error: this.error,
              showReload: true,
              onReload: () => {
                this.reload();
              },
            })
          }
          if (this.capture !== null) {
            Column({ space: 6 }) {
              Row({ space: 8 }) {
                Text(formatDateTime((this.capture as Capture).occurred_at))
                  .fontSize(20)
                  .fontWeight(FontWeight.Bold)
                  .fontColor(SketchColors.text)
                StatePill({
                  text: captureStateLabel((this.capture as Capture).state),
                  tone: (this.capture as Capture).state === 'draft' ? 'warn' : 'ok',
                })
              }
              .width('100%')
              InfoRow({ label: '按天', value: (this.capture as Capture).day_key })
              InfoRow({ label: '来源数', value: `${(this.capture as Capture).ordered_source_ids.length}` })
            }
            .width('100%')
            .padding(14)
            .backgroundColor(SketchColors.paper)
            .drawModifier(sketchOutline())
            .alignItems(HorizontalAlign.Start)

            if ((this.capture as Capture).state === 'draft') {
              Column({ space: 8 }) {
                Text('未提交（草稿）')
                  .fontSize(15)
                  .fontWeight(FontWeight.Medium)
                  .fontColor(SketchColors.text)
                  .width('100%')
                Text('这条记录还是草稿，收好后可参与日记整理。')
                  .fontSize(12)
                  .fontColor(SketchColors.muted)
                  .width('100%')
                Button(this.commitBusy ? '提交中…' : '收好记录')
                  .attributeModifier(sketchButton).drawModifier(sketchOutline())
                  .fontSize(13)
                  .enabled(!this.commitBusy)
                  .backgroundColor(SketchColors.button)
                  .fontColor(SketchColors.ink)
                  .onClick(() => {
                    this.commit();
                  })
                if (this.commitError.code.length > 0) {
                  ErrorBanner({ error: this.commitError, showReload: false, onReload: () => {
                  } })
                }
              }
              .width('100%')
              .padding(14)
              // 同 S2：warning_transparent 在本机主题下是不透明红，不能再当卡片底色；
              // 标题用正常文字色，「草稿」的警告语义由顶部 StatePill(tone=warn) 承担。
              .backgroundColor(SketchColors.soft)
              .borderRadius(0)
              .alignItems(HorizontalAlign.Start)
            }

            if ((this.capture as Capture).draft_text.trim().length > 0) {
              Column({ space: 6 }) {
                // 已提交的记录，正文已经不是草稿；标题必须跟着状态走（G4）。
                SectionTitle({ text: (this.capture as Capture).state === 'draft' ? '草稿正文' : '正文' })
                Text((this.capture as Capture).draft_text)
                  .fontSize(16)
                  .fontColor(SketchColors.text)
                  .width('100%')
                  .lineHeight(24)
              }
              .width('100%')
              .padding(14)
              .backgroundColor(SketchColors.paper)
              .drawModifier(sketchOutline())
              .alignItems(HorizontalAlign.Start)
            }

            SectionTitle({ text: '记录内容' })
            if (this.sources.length === 0) {
              EmptyState({
                icon: '·',
                title: '这条记录还没有来源',
                hint: '收好记录后，可在这里查看。',
              })
            } else {
              ForEach(this.sources, (source: SourceView, index: number) => {
                this.sourceCard(source, index)
              }, (source: SourceView) => source.sourceId)
            }
          }
        }
        .width('100%')
        .padding({ left: 16, right: 16, top: 10, bottom: 24 })
      }
      .width('100%')
      .height('100%')
      // Scroll 的 align 默认是 Center：内容不足一屏时会被垂直居中（真机 07/33 的
      // 上下各约 1/4 屏空白）。显式 Top，从顶部开始排。
      .align(Alignment.Top)
      .backgroundColor(SketchColors.canvas)
      .scrollBar(BarState.Auto)
    }
    .title('记录', { backgroundColor: SketchColors.canvas })
  }
}

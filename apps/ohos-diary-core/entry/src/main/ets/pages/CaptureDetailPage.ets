// 记录/详情：`captures.get` 显示来源与转写；草稿可手动提交为正式记录。
//
// 「未提交」是如实状态，不是装饰：
// - `recordings.finalize` 只把音频最终化并排转写任务，**不会 commit**；
// - 当前 `VoiceRecorder` 也不返回 capture id（#1019 的 finalize→commit 修复还没合入），
//   所以录音停止后这条记录仍是 `draft`；
// - 本页给「提交为正式记录」按钮，用现有 `captures.commit`（revision 来自 captures.get，
//   不猜）；不提交也照样能看到来源与转写。

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
      return '这次提取/转写失败；原因见下面的覆盖说明与错误码。';
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
    toast('已提交为正式记录');
    await this.reload();
  }

  @Builder
  sourceCard(source: SourceView, index: number) {
    Column({ space: 8 }) {
      Row({ space: 8 }) {
        Text(this.sourceTitle(source, index))
          .fontSize(14)
          .fontWeight(FontWeight.Medium)
          .fontColor($r('sys.color.ohos_id_color_text_primary'))
        StatePill({ text: this.sourceStatus(source), tone: this.sourceTone(source) })
        Blank()
        if (source.extracted !== null) {
          Text(coverageLabel(source.extracted.coverage))
            .fontSize(11)
            .fontColor(source.extracted.coverage === 'complete'
              ? $r('sys.color.ohos_id_color_text_tertiary')
              : $r('sys.color.ohos_id_color_warning'))
        }
      }
      .width('100%')
      if (source.error.code.length > 0) {
        ErrorBanner({ error: source.error, showReload: false, onReload: () => {
        } })
      }
      Text(this.sourceBody(source))
        .fontSize(15)
        .fontColor($r('sys.color.ohos_id_color_text_primary'))
        .width('100%')
        .lineHeight(23)
      if (source.extracted !== null && source.extracted.coverage_reason !== null) {
        Text(source.extracted.coverage_reason)
          .fontSize(12)
          .fontColor($r('sys.color.ohos_id_color_warning'))
          .width('100%')
      }
      if (source.extracted !== null && source.extracted.error_code !== null) {
        Text(`错误码：${source.extracted.error_code}`)
          .fontSize(12)
          .fontColor($r('sys.color.ohos_id_color_alert'))
          .width('100%')
      }
      if (source.extracted !== null && source.extracted.segments.length > 0) {
        Column({ space: 6 }) {
          Text('分段时间轴')
            .fontSize(12)
            .fontColor($r('sys.color.ohos_id_color_text_secondary'))
            .width('100%')
          ForEach((source.extracted as ExtractedContent).segments, (segment: ExtractedSegment) => {
            Row({ space: 8 }) {
              Text(this.segmentLabel(segment))
                .fontSize(11)
                .fontColor($r('sys.color.ohos_id_color_text_tertiary'))
                .width(110)
                .flexShrink(0)
              Text(segment.text)
                .fontSize(13)
                .fontColor($r('sys.color.ohos_id_color_text_primary'))
                .layoutWeight(1)
            }
            .width('100%')
            .alignItems(VerticalAlign.Top)
          }, (segment: ExtractedSegment) => `${segment.ordinal}-${segment.locator.start_ms}-${segment.locator.text_start}`)
        }
        .width('100%')
        .padding(10)
        .backgroundColor($r('sys.color.ohos_id_color_sub_background'))
        .borderRadius(10)
      }
      Text(`来源 ID：${source.sourceId}`)
        .fontSize(10)
        .fontColor($r('sys.color.ohos_id_color_text_tertiary'))
        .width('100%')
    }
    .width('100%')
    .padding(14)
    .backgroundColor($r('sys.color.ohos_id_color_card_bg'))
    .borderRadius(14)
    .alignItems(HorizontalAlign.Start)
  }

  build() {
    NavDestination() {
      Scroll() {
        Column({ space: 10 }) {
          Row() {
            Blank()
            Button('刷新')
              .fontSize(12)
              .height(30)
              .backgroundColor($r('sys.color.ohos_id_color_sub_background'))
              .fontColor($r('sys.color.ohos_id_color_text_primary'))
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
                  .fontColor($r('sys.color.ohos_id_color_text_primary'))
                StatePill({
                  text: captureStateLabel((this.capture as Capture).state),
                  tone: (this.capture as Capture).state === 'draft' ? 'warn' : 'ok',
                })
              }
              .width('100%')
              InfoRow({ label: '按天', value: (this.capture as Capture).day_key })
              InfoRow({ label: '时区', value: `${(this.capture as Capture).time_zone}（UTC${(this.capture as Capture).utc_offset_minutes >= 0 ? '+' : ''}${(this.capture as Capture).utc_offset_minutes} 分钟）` })
              InfoRow({ label: 'revision', value: `${(this.capture as Capture).revision}` })
              InfoRow({ label: '来源数', value: `${(this.capture as Capture).ordered_source_ids.length}` })
              InfoRow({ label: '记录 ID', value: (this.capture as Capture).id })
            }
            .width('100%')
            .padding(14)
            .backgroundColor($r('sys.color.ohos_id_color_card_bg'))
            .borderRadius(14)
            .alignItems(HorizontalAlign.Start)

            if ((this.capture as Capture).state === 'draft') {
              Column({ space: 8 }) {
                Text('未提交（草稿）')
                  .fontSize(15)
                  .fontWeight(FontWeight.Medium)
                  .fontColor($r('sys.color.ohos_id_color_warning'))
                  .width('100%')
                Text('录音停止后核心只做了 finalize，不会自动 commit；这条记录现在还是草稿。' +
                  '当前接口没有 finalize→commit（#1019 修复未合入），下面用现有的 captures.commit 手动提交。')
                  .fontSize(12)
                  .fontColor($r('sys.color.ohos_id_color_text_secondary'))
                  .width('100%')
                Button(this.commitBusy ? '提交中…' : '提交为正式记录')
                  .fontSize(13)
                  .height(34)
                  .enabled(!this.commitBusy)
                  .backgroundColor($r('sys.color.ohos_id_color_emphasize'))
                  .fontColor($r('sys.color.ohos_id_color_text_primary_contrary'))
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
              .backgroundColor($r('sys.color.ohos_id_color_warning_transparent'))
              .borderRadius(14)
              .alignItems(HorizontalAlign.Start)
            }

            if ((this.capture as Capture).draft_text.trim().length > 0) {
              Column({ space: 6 }) {
                SectionTitle({ text: '草稿正文' })
                Text((this.capture as Capture).draft_text)
                  .fontSize(16)
                  .fontColor($r('sys.color.ohos_id_color_text_primary'))
                  .width('100%')
                  .lineHeight(24)
              }
              .width('100%')
              .padding(14)
              .backgroundColor($r('sys.color.ohos_id_color_card_bg'))
              .borderRadius(14)
              .alignItems(HorizontalAlign.Start)
            }

            SectionTitle({ text: '来源与转写（sources.extract）' })
            if (this.sources.length === 0) {
              EmptyState({
                icon: '·',
                title: '这条记录还没有来源',
                hint: '纯文字草稿在提交后才会生成文字来源；录音来源会在 finalize 后出现。',
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
      .backgroundColor($r('sys.color.ohos_id_color_background'))
      .scrollBar(BarState.Auto)
    }
    .title('记录')
  }
}

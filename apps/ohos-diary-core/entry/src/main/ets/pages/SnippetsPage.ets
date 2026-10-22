import { SketchColors, SketchFonts, SketchType, SketchCard, sketchButton, sketchOutline } from '../ui/Sketch';
// 片段（契约 §2.4）：原始材料的时间线——保存下来的文字、录音、文件，**不是日记正文**。
//
// 从一个旧的「今日」页拆出来的：按天列出 captures（`captures.list`，游标分页），
// 列表语义属于「片段」；录音与文字新建入口在「记录」tab，成篇正文在「日记」。
//
// 状态口径（重要）：
// - **不用 `Capture.processing_summary`**：核心当前恒为全 0（issue #146），
//   拿它判断「有没有在跑」会撒谎；
// - 每条的真实转写状态来自 `sources.extract(ordered_source_ids[0])` 的
//   `status` / `coverage` / `coverage_reason`；
// - `state=draft` 的条目显式标「未提交」，不写成「已保存」来掩盖；
// - `state=trashed` 的记录**不出现在这一页**：`captures.list` 不过滤状态，客户端用
//   `selectActiveCaptures()` 筛掉回收站（核心不改口径，回收站页负责另一半）；
// - 逐条 `sources.extract` 失败时显示「转写状态未知」+ 行内原因与重试，不当成空记录。

import { common } from '@kit.AbilityKit';
import { Capture, CapturePage, ExtractedContent, ExtractParams, ListCapturesParams } from '../core/Models';
import { selectActiveCaptures } from '../state/RecycleStore';
import { CoreGateway, CoreRequest, CoreResult, uiErrorOf } from '../ui/CoreGateway';
import { EMPTY_UI_ERROR, UiError } from '../ui/Errors';
import {
  captureStateLabel,
  coverageLabel,
  databasePath,
  dayKeyAtOffset,
  dayLabelAtOffset,
  extractStatusLabel,
  extractStatusTone,
  formatTime,
} from '../ui/AppRuntime';
import { EmptyState, ErrorBanner, LoadingRow, PageTitle, StatePill } from '../ui/Common';

/** 列表里的一行：capture + 第一条来源的抽取状态。 */
interface CaptureRow {
  capture: Capture;
  extracted: ExtractedContent | null;
  extractionError: UiError;
  sourceKind: string;
}

function sourceKindOf(extracted: ExtractedContent | null): string {
  if (extracted === null) {
    return 'unknown';
  }
  const id: string = extracted.extractor_id;
  if (id === 'local_transcription' || id === 'media_metadata_only') {
    return 'audio';
  }
  if (id === 'plain_text' || id === 'text_direct' || id === 'markdown') {
    return 'text';
  }
  return 'file';
}

@Component
export struct SnippetsPage {
  pathStack: NavPathStack = new NavPathStack();
  @State rows: CaptureRow[] = [];
  /** 至少成功读过一页。「false + error」时不能把空数组说成「今天还没有片段」。 */
  @State loaded: boolean = false;
  @State loading: boolean = true;
  @State refreshing: boolean = false;
  @State loadingMore: boolean = false;
  @State error: UiError = EMPTY_UI_ERROR;
  @State nextCursor: string = '';
  @State hasMore: boolean = false;
  @State dayOffset: number = 0;
  @StorageLink('ohosDataVersion') @Watch('onDataVersionChanged') dataVersion: number = 0;

  private dbPath: string = '';
  private seq: number = 0;

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
    this.nextCursor = '';
    this.hasMore = false;
    if (this.rows.length === 0) {
      this.loading = true;
    }
    await this.loadPage(token, '', false);
    if (token === this.seq) {
      this.loading = false;
      this.refreshing = false;
    }
  }

  private async refresh(): Promise<void> {
    this.seq++;
    const token: number = this.seq;
    this.error = EMPTY_UI_ERROR;
    await this.loadPage(token, '', false);
    if (token === this.seq) {
      this.loading = false;
      this.refreshing = false;
    }
  }

  private async loadMore(): Promise<void> {
    if (!this.hasMore || this.loadingMore || this.loading || this.nextCursor.length === 0) {
      return;
    }
    this.loadingMore = true;
    const token: number = this.seq;
    await this.loadPage(token, this.nextCursor, true);
    if (token === this.seq) {
      this.loadingMore = false;
    }
  }

  private async loadPage(token: number, cursor: string, append: boolean): Promise<void> {
    const params: ListCapturesParams = {
      day_key: dayKeyAtOffset(this.dayOffset),
      limit: 20,
    };
    if (cursor.length > 0) {
      params.cursor = cursor;
    }
    const result = await CoreGateway.call(this.dbPath, 'captures.list', params);
    if (token !== this.seq) {
      return;
    }
    if (!result.ok) {
      this.error = uiErrorOf(result);
      this.loading = false;
      this.refreshing = false;
      this.loadingMore = false;
      return;
    }
    const page: CapturePage = result.data as CapturePage;
    // `captures.list` **不按 state 过滤**（核心把回收站里的记录也返回）。片段是「主列表」，
    // 必须排除 `state === 'trashed'`：回收站有自己的页面，不能让已删除的记录继续出现在
    // 时间线里（否则「删除」在界面上就是假的）。过滤用 store 的 `selectActiveCaptures`，
    // 与回收站页的 `selectTrashedCaptures` 是同一个口径的两半。
    // 先过滤再建行：trashed 的记录不发起 `sources.extract`，少一批无谓调用。
    const captures: Capture[] = selectActiveCaptures(page.captures);

    // 每条的转写状态：只取第一条来源；多来源在详情页展开。
    const positions: number[] = [];
    const requests: CoreRequest[] = [];
    for (let i = 0; i < captures.length; i++) {
      positions.push(-1);
      if (captures[i].ordered_source_ids.length > 0) {
        const extractParams: ExtractParams = { source_ref: captures[i].ordered_source_ids[0] };
        const request: CoreRequest = { method: 'sources.extract', params: extractParams };
        positions[i] = requests.length;
        requests.push(request);
      }
    }
    let extractResults: CoreResult[] = [];
    if (requests.length > 0) {
      extractResults = await CoreGateway.callMany(this.dbPath, requests);
      if (token !== this.seq) {
        return;
      }
    }

    const fresh: CaptureRow[] = [];
    for (let i = 0; i < captures.length; i++) {
      const capture: Capture = captures[i];
      let extracted: ExtractedContent | null = null;
      let extractionError: UiError = EMPTY_UI_ERROR;
      const pos: number = positions[i];
      if (pos >= 0 && pos < extractResults.length) {
        const extractResult = extractResults[pos];
        if (extractResult.ok) {
          extracted = extractResult.data as ExtractedContent;
        } else {
          extractionError = uiErrorOf(extractResult);
        }
      }
      fresh.push({
        capture: capture,
        extracted: extracted,
        extractionError: extractionError,
        sourceKind: sourceKindOf(extracted),
      });
    }

    this.rows = append ? this.rows.concat(fresh) : fresh;
    this.loaded = true;
    this.nextCursor = page.next_cursor === null ? '' : page.next_cursor;
    this.hasMore = page.next_cursor !== null;
    this.loading = false;
    this.refreshing = false;
    this.loadingMore = false;
  }

  private changeDay(delta: number): void {
    const next: number = this.dayOffset + delta;
    if (next > 0) {
      return;
    }
    if (next === this.dayOffset) {
      return;
    }
    this.dayOffset = next;
    this.rows = [];
    this.loading = true;
    this.reload();
  }

  private typeIcon(row: CaptureRow): string {
    if (row.sourceKind === 'audio') {
      return '声';
    }
    if (row.sourceKind === 'text') {
      return '文';
    }
    if (row.sourceKind === 'file') {
      return '件';
    }
    if (row.capture.draft_text.trim().length > 0) {
      return '文';
    }
    return '·';
  }

  private typeLabel(row: CaptureRow): string {
    if (row.extractionError.code.length > 0) {
      // 抽取调用失败时我们**不知道**这条来源是什么类型，不能写成「空记录」。
      return '来源状态未知';
    }
    if (row.capture.ordered_source_ids.length > 1) {
      return `多来源 · ${row.capture.ordered_source_ids.length}`;
    }
    if (row.sourceKind === 'audio') {
      return '语音';
    }
    if (row.sourceKind === 'text') {
      return '文字';
    }
    if (row.sourceKind === 'file') {
      return '文件';
    }
    if (row.capture.draft_text.trim().length > 0) {
      return '文字';
    }
    return '空记录';
  }

  private summaryOf(row: CaptureRow): string {
    const draft: string = row.capture.draft_text.trim();
    if (draft.length > 0) {
      return draft;
    }
    if (row.extracted !== null) {
      const text: string = row.extracted.text.trim();
      if (text.length > 0) {
        return text;
      }
    }
    return '（还没有正文）';
  }

  /**
   * 只有含音频来源的记录才谈得上「转写」。
   *
   * 依据是本页已加载的第一条来源的类型（`sourceKindOf` 由 extractor_id 判定）：
   * `local_transcription` / `media_metadata_only` 来自音频来源；
   * `plain_text` / `text_direct` / `markdown` 是文字来源。
   * 纯文字记录不能显示「转写完成」——那是在说一件没有发生的事。
   */
  private hasAudioSource(row: CaptureRow): boolean {
    return row.sourceKind === 'audio';
  }

  private transcribeLabel(row: CaptureRow): string {
    // ⚠️ 顺序很重要：`extractionError` 必须**先**判。
    // `sourceKind` 由 `extracted` 推出，而 `extracted` 与 `extractionError` 互斥
    // （成功才有 extracted）⇒ 先判 `hasAudioSource` 时，抽取失败的音频记录一定是
    // `sourceKind === 'unknown'`，错误分支**永远走不到**，界面就把它显示成「空记录」，
    // 逐条抽取的错误被整条丢掉。现在只要这次抽取失败，就先说「转写状态未知」。
    if (row.extractionError.code.length > 0) {
      return '转写状态未知';
    }
    if (!this.hasAudioSource(row)) {
      // 空串 = 这条记录没有音频来源，界面不渲染转写标签（类型仍由 typeLabel 显示「文字」）。
      return '';
    }
    if (row.capture.ordered_source_ids.length === 0) {
      return row.capture.draft_text.trim().length > 0 ? '纯文字' : '无内容';
    }
    if (row.extracted === null) {
      return '等待处理';
    }
    const status: string = row.extracted.status;
    if (status === 'ready' && row.extracted.coverage === 'complete') {
      return '转写完成';
    }
    if (status === 'ready') {
      return `可读（${coverageLabel(row.extracted.coverage)}）`;
    }
    if (status === 'partial') {
      return '部分完成';
    }
    return extractStatusLabel(status);
  }

  private transcribeTone(row: CaptureRow): string {
    if (row.extractionError.code.length > 0) {
      return 'warn';
    }
    if (row.extracted === null) {
      return 'muted';
    }
    return extractStatusTone(row.extracted.status);
  }

  /**
   * 只重试这一条的第一条来源抽取（`sources.extract`），不动其它行。
   *
   * 逐条失败是本页已知的现象（模型没配好、音频文件缺失等）；给一个行内重试，
   * 用户不用整页刷新，也能看到失败原因。
   */
  private async retryExtract(row: CaptureRow): Promise<void> {
    if (row.capture.ordered_source_ids.length === 0) {
      return;
    }
    const params: ExtractParams = { source_ref: row.capture.ordered_source_ids[0] };
    const result: CoreResult = await CoreGateway.call(this.dbPath, 'sources.extract', params);
    let next: CaptureRow[] = [];
    for (let i = 0; i < this.rows.length; i++) {
      const item: CaptureRow = this.rows[i];
      if (item.capture.id !== row.capture.id) {
        next.push(item);
        continue;
      }
      if (result.ok) {
        const extracted: ExtractedContent = result.data as ExtractedContent;
        next.push({
          capture: item.capture,
          extracted: extracted,
          extractionError: EMPTY_UI_ERROR,
          sourceKind: sourceKindOf(extracted),
        });
      } else {
        next.push({
          capture: item.capture,
          extracted: null,
          extractionError: uiErrorOf(result),
          sourceKind: 'unknown',
        });
      }
    }
    // 整数组换引用，ArkUI 才会重算 ForEach（key 里含错误码/状态）。
    this.rows = next;
  }

  @Builder
  captureCard(row: CaptureRow) {
    Row({ space: 12 }) {
      Text(this.typeIcon(row))
        .fontSize(16)
        .fontWeight(FontWeight.Medium)
        .fontColor(SketchColors.muted)
        .width(38)
        .height(38)
        .textAlign(TextAlign.Center)
        .backgroundColor(SketchColors.soft)
        .borderRadius(0)
      Column({ space: 6 }) {
        Row({ space: 8 }) {
          Text(formatTime(row.capture.occurred_at))
            .fontSize(13)
            .fontColor(SketchColors.muted)
          StatePill({
            text: captureStateLabel(row.capture.state),
            tone: row.capture.state === 'draft'
              ? 'warn'
              : (row.capture.state === 'trashed' ? 'alert' : 'ok'),
          })
          Blank()
          if (this.transcribeLabel(row).length > 0) {
            StatePill({ text: this.transcribeLabel(row), tone: this.transcribeTone(row) })
          }
        }
        .width('100%')
        Text(this.summaryOf(row))
          .fontSize(16)
          .fontColor(SketchColors.text)
          .maxLines(2)
          .textOverflow({ overflow: TextOverflow.Ellipsis })
          .width('100%')
        if (row.extractionError.code.length > 0) {
          // 逐条抽取失败：不能只把它藏进类型标签，原因与重试都要在行内可见。
          Row({ space: 8 }) {
            Text(`来源状态没读出来（${row.extractionError.code}）：${row.extractionError.message}`)
              .fontSize(12)
              .fontColor(SketchColors.warning)
              .layoutWeight(1)
              .maxLines(3)
              .textOverflow({ overflow: TextOverflow.Ellipsis })
            Button('重试读取')
              .attributeModifier(sketchButton).drawModifier(sketchOutline())
              .fontSize(12)
              .backgroundColor(SketchColors.soft)
              .fontColor(SketchColors.text)
              .onClick(() => {
                this.retryExtract(row);
              })
          }
          .width('100%')
          .alignItems(VerticalAlign.Top)
        }
        Row({ space: 6 }) {
          Text(this.typeLabel(row))
            .fontSize(12)
            .fontColor(SketchColors.muted)
          if (row.extracted !== null && row.extracted.coverage !== 'complete') {
            Text('·')
              .fontSize(12)
              .fontColor(SketchColors.muted)
            Text(coverageLabel(row.extracted.coverage))
              .fontSize(12)
              .fontColor(SketchColors.warning)
          }
          if (row.capture.state === 'draft') {
            Text('·')
              .fontSize(12)
              .fontColor(SketchColors.muted)
            Text('未提交')
              .fontSize(12)
              .fontColor(SketchColors.warning)
          }
        }
        .width('100%')
      }
      .layoutWeight(1)
      .alignItems(HorizontalAlign.Start)
    }
    .width('100%')
    .padding(14)
    .backgroundColor(SketchColors.paper)
    .drawModifier(sketchOutline())
    .alignItems(VerticalAlign.Top)
    .onClick(() => {
      this.pathStack.pushPathByName('capture', row.capture.id);
    })
  }

  build() {
    Column({ space: 10 }) {
      Row({ space: 8 }) {
        Column() {
          PageTitle({
            title: '留下的片段',
            subtitle: `${dayLabelAtOffset(this.dayOffset)} · 留下的片段（原始记录与转写状态）`,
          })
        }
        .layoutWeight(1)
        .alignItems(HorizontalAlign.Start)
        Text('原始材料')
          .fontSize(12)
          .fontColor(SketchColors.muted)
      }
      .width('100%')
      .padding({ left: 16, right: 16, top: 12 })

      Row({ space: 8 }) {
        Button('前一天')
          .attributeModifier(sketchButton).drawModifier(sketchOutline())
          .fontSize(13)
          .height(32)
          .backgroundColor(SketchColors.soft)
          .fontColor(SketchColors.text)
          .onClick(() => {
            this.changeDay(-1);
          })
        Button('后一天')
          .attributeModifier(sketchButton).drawModifier(sketchOutline())
          .fontSize(13)
          .height(32)
          .enabled(this.dayOffset < 0)
          .backgroundColor(SketchColors.soft)
          .fontColor(SketchColors.text)
          .onClick(() => {
            this.changeDay(1);
          })
        Button('回到今天')
          .attributeModifier(sketchButton).drawModifier(sketchOutline())
          .fontSize(13)
          .height(32)
          .enabled(this.dayOffset !== 0)
          .backgroundColor(SketchColors.soft)
          .fontColor(SketchColors.text)
          .onClick(() => {
            this.changeDay(-this.dayOffset);
          })
        Blank()
        Text(this.loading ? '读取中' : `${this.rows.length} 条`)
          .fontSize(12)
          .fontColor(SketchColors.muted)
      }
      .width('100%')
      .padding({ left: 16, right: 16 })

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

      Refresh({ refreshing: $$this.refreshing }) {
        List({ space: 10 }) {
          if (this.loading) {
            ListItem() {
              LoadingRow({ text: '正在读取这一天的片段…' })
            }
          } else if (this.rows.length === 0 && this.loaded) {
            ListItem() {
              EmptyState({
                icon: '—',
                title: this.dayOffset === 0 ? '今天还没有片段' : '这一天没有片段',
                hint: this.dayOffset === 0
                  ? '去「记录」写一条或录一段，原始材料会按天出现在这里。'
                  : '可以点「后一天」回到今天。',
              })
            }
          } else if (this.rows.length === 0) {
            // 一次都没读成功：这是「读失败」，不是「今天没有片段」。
            ListItem() {
              Text('这一天的片段没有读出来：这不代表今天没有片段；原因见上面的错误。')
                .fontSize(12)
                .fontColor(SketchColors.muted)
                .width('100%')
                .padding(12)
            }
          } else {
            ForEach(this.rows, (row: CaptureRow) => {
              ListItem() {
                this.captureCard(row)
              }
              // key 里必须带 `state` / `revision` / 转写状态：ArkUI 的 ForEach 在 key 不变时
              // 复用旧节点，只用 `capture.id` 时「草稿 → 已提交」、转写完成这类推进不会
              // 重新渲染（#150 真机抓到）。
            }, (row: CaptureRow) => `${row.capture.id}-${row.capture.state}-${row.capture.revision}-` +
              `${row.extracted === null ? 'none' : row.extracted.status}-` +
              `${row.extracted === null ? '' : row.extracted.coverage}-${row.extractionError.code}`)
            if (this.loadingMore) {
              ListItem() {
                LoadingRow({ text: '加载更早的片段…' })
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

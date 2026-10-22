import { SketchColors, SketchFonts, SketchType, SketchCard, sketchButton, sketchInput, sketchArea, sketchOutline } from '../ui/Sketch';
// 搜索：关键词 / 语义 / 混合三种模式。
//
// 用到的 C ABI：`search.start`、`search.nextPage`、`search.upgrade`、`search.cancel`、
// `indexes.status`（显示语义索引覆盖，不拿它挡搜索）。
//
// 三个必须如实处理的点：
// 1. `semantic_index_ready == false` **不等于**没有语义结果：已嵌入的块仍会被检索，
//    覆盖不全只在 `warnings` 里说；结果照常显示，另外加一条覆盖提示；
// 2. keyword 的 `phase` 可能是 `keyword_ready`（还有下一页），不恒为 `done`；
//    能不能翻页看 **cursor**，phase 不是结束标志；
// 3. hybrid 的 `start` 只算关键词阶段；`upgrade` 成功才 `hybrid_ready`，
//    升不了级时仍停在 `keyword_ready`，不能把标签写成「混合结果」；
// 4. `error` 有值时**不显示「没有结果」**：检索失败不等于库里没有匹配内容。

import { common } from '@kit.AbilityKit';
import {
  IndexStatus,
  SearchHit,
  SearchNextPageParams,
  SearchParams,
  SearchRequestParams,
  SearchSessionParams,
  SearchSnapshot,
} from '../core/Models';
import { CoreGateway, uiErrorOf } from '../ui/CoreGateway';
import { EMPTY_UI_ERROR, UiError } from '../ui/Errors';
import {
  coverageLabel,
  databasePath,
  formatDuration,
  hitSourceKindLabel,
  matchedByLabel,
  searchPhaseLabel,
} from '../ui/AppRuntime';
import { EmptyState, ErrorBanner, LoadingRow, PageTitle, StatePill } from '../ui/Common';

@Component
export struct SearchPage {
  pathStack: NavPathStack = new NavPathStack();
  @State query: string = '';
  @State mode: string = 'keyword';
  @State results: SearchHit[] = [];
  @State warnings: string[] = [];
  @State phase: string = '';
  @State indexCoverage: string = '';
  @State sessionId: string = '';
  @State cursor: string = '';
  @State hasMore: boolean = false;
  @State queryRevision: number = 1;
  @State loading: boolean = false;
  @State loadingMore: boolean = false;
  @State upgrading: boolean = false;
  @State upgradeAttempted: boolean = false;
  @State searched: boolean = false;
  @State error: UiError = EMPTY_UI_ERROR;
  @State statusError: UiError = EMPTY_UI_ERROR;
  @State indexStatus: IndexStatus | null = null;

  private dbPath: string = '';
  private seq: number = 0;

  aboutToAppear(): void {
    const context = getContext(this) as common.UIAbilityContext;
    this.dbPath = databasePath(context);
    this.loadIndexStatus();
  }

  private async loadIndexStatus(): Promise<void> {
    const result = await CoreGateway.call(this.dbPath, 'indexes.status', null);
    if (!result.ok) {
      this.statusError = uiErrorOf(result);
      return;
    }
    this.statusError = EMPTY_UI_ERROR;
    this.indexStatus = result.data as IndexStatus;
  }

  private async cancelPrevious(): Promise<void> {
    const previous: string = this.sessionId;
    this.sessionId = '';
    if (previous.length === 0) {
      return;
    }
    const params: SearchSessionParams = { session_id: previous };
    await CoreGateway.call(this.dbPath, 'search.cancel', params);
  }

  private async startSearch(): Promise<void> {
    const text: string = this.query.trim();
    if (text.length === 0) {
      this.error = {
        code: 'invalid_argument',
        title: '还没有输入内容',
        message: '搜索需要至少一个关键词。',
        hint: '输入关键词后按回车或点「搜索」。',
        retryable: false,
        native: false,
      };
      return;
    }
    this.seq++;
    const token: number = this.seq;
    this.loading = true;
    this.loadingMore = false;
    this.upgrading = false;
    this.error = EMPTY_UI_ERROR;
    this.searched = true;
    this.upgradeAttempted = false;
    this.results = [];
    this.warnings = [];
    this.phase = '';
    this.indexCoverage = '';
    this.cursor = '';
    this.hasMore = false;
    await this.cancelPrevious();
    if (token !== this.seq) {
      return;
    }
    this.queryRevision++;
    const request: SearchRequestParams = {
      query: text,
      mode: this.mode,
      page_size: 20,
    };
    const params: SearchParams = {
      request: request,
      query_revision: this.queryRevision,
    };
    const result = await CoreGateway.call(this.dbPath, 'search.start', params);
    if (token !== this.seq) {
      return;
    }
    this.loading = false;
    if (!result.ok) {
      this.error = uiErrorOf(result);
      return;
    }
    this.applySnapshot(result.data as SearchSnapshot, false);
    // 语义就绪度可能在这次查询之后变化（自动重建/换模型），顺手刷新一次。
    this.loadIndexStatus();
  }

  private applySnapshot(snapshot: SearchSnapshot, append: boolean): void {
    this.sessionId = snapshot.session_id;
    this.phase = snapshot.phase;
    this.indexCoverage = snapshot.index_coverage;
    this.warnings = snapshot.warnings;
    this.cursor = snapshot.cursor === null ? '' : snapshot.cursor;
    this.hasMore = snapshot.cursor !== null;
    this.queryRevision = snapshot.query_revision;
    this.results = append ? this.results.concat(snapshot.results) : snapshot.results;
  }

  private async loadMore(): Promise<void> {
    if (!this.hasMore || this.loadingMore || this.loading || this.cursor.length === 0) {
      return;
    }
    if (this.sessionId.length === 0) {
      return;
    }
    this.loadingMore = true;
    const token: number = this.seq;
    const params: SearchNextPageParams = {
      session_id: this.sessionId,
      cursor: this.cursor,
    };
    const result = await CoreGateway.call(this.dbPath, 'search.nextPage', params);
    if (token !== this.seq) {
      return;
    }
    this.loadingMore = false;
    if (!result.ok) {
      this.error = uiErrorOf(result);
      return;
    }
    this.applySnapshot(result.data as SearchSnapshot, true);
  }

  private async upgrade(): Promise<void> {
    if (this.sessionId.length === 0 || this.upgrading) {
      return;
    }
    this.upgrading = true;
    this.upgradeAttempted = true;
    this.error = EMPTY_UI_ERROR;
    const token: number = this.seq;
    const params: SearchSessionParams = { session_id: this.sessionId };
    const result = await CoreGateway.call(this.dbPath, 'search.upgrade', params);
    if (token !== this.seq) {
      return;
    }
    this.upgrading = false;
    if (!result.ok) {
      this.error = uiErrorOf(result);
      return;
    }
    this.applySnapshot(result.data as SearchSnapshot, false);
  }

  private async cancel(): Promise<void> {
    if (this.sessionId.length === 0) {
      return;
    }
    const token: number = this.seq;
    const params: SearchSessionParams = { session_id: this.sessionId };
    const result = await CoreGateway.call(this.dbPath, 'search.cancel', params);
    if (token !== this.seq) {
      return;
    }
    if (!result.ok) {
      this.error = uiErrorOf(result);
      return;
    }
    this.applySnapshot(result.data as SearchSnapshot, false);
  }

  private setMode(mode: string): void {
    if (this.mode === mode) {
      return;
    }
    this.mode = mode;
    if (this.searched && this.query.trim().length > 0) {
      this.startSearch();
    }
  }

  private matchedTone(matchedBy: string): string {
    if (matchedBy === 'semantic') {
      return 'active';
    }
    if (matchedBy === 'keyword') {
      return 'ok';
    }
    return 'muted';
  }

  private locatorLabel(hit: SearchHit): string {
    const locator = hit.locator;
    if (locator === null) {
      return '';
    }
    if (locator.start_ms !== null && locator.end_ms !== null) {
      return `${formatDuration(locator.start_ms)} – ${formatDuration(locator.end_ms)}`;
    }
    if (locator.text_start !== null && locator.text_end !== null) {
      return `第 ${locator.text_start}–${locator.text_end} 字`;
    }
    return '';
  }

  private semanticReady(): boolean {
    return this.indexStatus !== null && this.indexStatus.semantic_index_ready;
  }

  private semanticBanner(): string {
    if (this.indexStatus === null) {
      return '';
    }
    const status: IndexStatus = this.indexStatus;
    if (status.semantic_index_ready) {
      return '';
    }
    const base: string = `语义索引未完全就绪：已嵌入 ${status.embedded_chunks} / ${status.total_chunks} 块。` +
      '语义/混合模式仍会检索已嵌入的块，结果可能不全（不是「没有结果」）。';
    if (status.reasons.length > 0) {
      return `${base}\n${status.reasons[0]}`;
    }
    return base;
  }

  @Builder
  modeButton(label: string, value: string) {
    Button(label)
      .attributeModifier(sketchButton).drawModifier(sketchOutline())
      .fontSize(14)
      .backgroundColor(this.mode === value
        ? SketchColors.button
        : SketchColors.soft)
      .fontColor(this.mode === value
        ? SketchColors.ink
        : SketchColors.text)
      .onClick(() => {
        this.setMode(value);
      })
  }

  @Builder
  warningBox(text: string) {
    // 背景**不能**用 warning_transparent：本机主题把它解析成不透明红，
    // 再配 warning 文字就是红底红字（真机 12c/29/30 的证据）。警告色只做
    // 左侧「!」强调，正文用 text_primary，保证任何主题下都能读。
    Row({ space: 8 }) {
      Text('!')
        .fontSize(12)
        .fontWeight(FontWeight.Bold)
        .fontColor(SketchColors.warning)
        .width(16)
        .height(16)
        .textAlign(TextAlign.Center)
        .flexShrink(0)
      Text(text)
        .fontSize(12)
        .fontColor(SketchColors.text)
        .layoutWeight(1)
    }
    .width('100%')
    .padding(10)
    .backgroundColor(SketchColors.soft)
    .borderRadius(0)
    .alignItems(VerticalAlign.Top)
  }

  @Builder
  resultCard(hit: SearchHit) {
    Column({ space: 8 }) {
      Row({ space: 6 }) {
        Text(hit.day_key === null ? '日期未知' : hit.day_key)
          .fontSize(12)
          .fontColor(SketchColors.muted)
        StatePill({ text: hitSourceKindLabel(hit.source_kind), tone: 'muted' })
        Blank()
        ForEach(hit.matched_by, (matched: string) => {
          StatePill({ text: matchedByLabel(matched), tone: this.matchedTone(matched) })
        }, (matched: string) => matched)
      }
      .width('100%')
      if (hit.title !== null && hit.title.length > 0) {
        Text(hit.title)
          .fontSize(14)
          .fontWeight(FontWeight.Medium)
          .fontColor(SketchColors.text)
          .width('100%')
          .maxLines(1)
          .textOverflow({ overflow: TextOverflow.Ellipsis })
      }
      Text(hit.snippet === null ? '（没有摘录）' : hit.snippet)
        .fontSize(15)
        .fontColor(SketchColors.text)
        .width('100%')
        .maxLines(4)
        .textOverflow({ overflow: TextOverflow.Ellipsis })
      Row({ space: 8 }) {
        Text(`覆盖：${coverageLabel(hit.coverage)}`)
          .fontSize(11)
          .fontColor(hit.coverage === 'complete'
            ? SketchColors.muted
            : SketchColors.warning)
        if (this.locatorLabel(hit).length > 0) {
          Text(this.locatorLabel(hit))
            .fontSize(11)
            .fontColor(SketchColors.muted)
        }
      }
      .width('100%')
    }
    .width('100%')
    .padding(14)
    .backgroundColor(SketchColors.paper)
    .drawModifier(sketchOutline())
    .alignItems(HorizontalAlign.Start)
  }

  build() {
    Column({ space: 10 }) {
      Column() {
        PageTitle({ title: '搜索', subtitle: '找回本机的文字与原始材料。' })
      }
      .width('100%')
      .padding({ left: 16, right: 16, top: 12 })
      .alignItems(HorizontalAlign.Start)

      Row({ space: 8 }) {
        this.modeButton('关键词', 'keyword')
        this.modeButton('语义', 'semantic')
        this.modeButton('混合', 'hybrid')
      }
      .width('100%')
      .padding({ left: 16, right: 16 })

      Row({ space: 8 }) {
        TextInput({ placeholder: '搜索一个词，或一段记忆…', text: this.query }).attributeModifier(sketchInput)
          .layoutWeight(1)
          .height(42)
          .fontSize(15)
          .backgroundColor(SketchColors.soft)
          .onChange((value: string) => {
            this.query = value;
          })
          .onSubmit(() => {
            this.startSearch();
          })
        Button('搜索')
          .attributeModifier(sketchButton).drawModifier(sketchOutline())
          .fontSize(15)
          .enabled(!this.loading)
          .backgroundColor(SketchColors.button)
          .fontColor(SketchColors.ink)
          .onClick(() => {
            this.startSearch();
          })
      }
      .width('100%')
      .padding({ left: 16, right: 16 })

      if (this.semanticBanner().length > 0) {
        // 同 warningBox：背景用 sub_background，warning 只用于左侧「!」，
        // 正文 text_primary；按钮用主色，别再用 warning_transparent 叠 warning。
        Column({ space: 8 }) {
          Row({ space: 8 }) {
            Text('!')
              .fontSize(12)
              .fontWeight(FontWeight.Bold)
              .fontColor(SketchColors.warning)
              .width(16)
              .height(16)
              .textAlign(TextAlign.Center)
              .flexShrink(0)
            Text(this.semanticBanner())
              .fontSize(12)
              .fontColor(SketchColors.text)
              .layoutWeight(1)
          }
          .width('100%')
          .alignItems(VerticalAlign.Top)
          Button('去设置查看/重建语义索引')
            .attributeModifier(sketchButton).drawModifier(sketchOutline())
            .fontSize(12)
            .backgroundColor(SketchColors.button)
            .fontColor(SketchColors.ink)
            .onClick(() => {
              this.pathStack.pushPathByName('settings', '');
            })
        }
        .width('100%')
        .padding(12)
        .backgroundColor(SketchColors.soft)
        .borderRadius(0)
        .margin({ left: 16, right: 16 })
        .alignItems(HorizontalAlign.Start)
      }

      if (this.statusError.code.length > 0) {
        Text(`索引状态读取失败（${this.statusError.code}）；搜索仍可继续，但无法判断语义覆盖。`)
          .fontSize(12)
          .fontColor(SketchColors.muted)
          .width('100%')
          .padding({ left: 16, right: 16 })
      }

      if (this.error.code.length > 0) {
        Column() {
          ErrorBanner({
            error: this.error,
            showReload: false,
            onReload: () => {
            },
          })
        }
        .padding({ left: 16, right: 16 })
      }

      if (this.searched) {
        Row({ space: 8 }) {
          Text(`阶段：${searchPhaseLabel(this.phase)}`)
            .fontSize(12)
            .fontColor(SketchColors.muted)
          if (this.indexCoverage.length > 0) {
            Text(`覆盖：${coverageLabel(this.indexCoverage)}`)
              .fontSize(12)
              .fontColor(this.indexCoverage === 'complete'
                ? SketchColors.muted
                : SketchColors.warning)
          }
          if (this.hasMore) {
            Text('还有下一页')
              .fontSize(12)
              .fontColor(SketchColors.muted)
          }
          Blank()
          if (this.mode === 'hybrid' && this.phase === 'keyword_ready' && !this.upgradeAttempted) {
            Button(this.upgrading ? '融合中…' : '升级为混合检索')
              .attributeModifier(sketchButton).drawModifier(sketchOutline())
              .fontSize(12)
              .enabled(!this.upgrading)
              .backgroundColor(SketchColors.soft)
              .fontColor(SketchColors.text)
              .onClick(() => {
                this.upgrade();
              })
          }
          if (this.phase !== 'cancelled' && this.sessionId.length > 0) {
            Button('取消本次搜索')
              .attributeModifier(sketchButton).drawModifier(sketchOutline())
              .fontSize(12)
              .backgroundColor(SketchColors.soft)
              .fontColor(SketchColors.muted)
              .onClick(() => {
                this.cancel();
              })
          }
        }
        .width('100%')
        .padding({ left: 16, right: 16 })
      }

      if (this.warnings.length > 0) {
        Column({ space: 6 }) {
          ForEach(this.warnings, (warning: string, index: number) => {
            this.warningBox(warning)
          }, (warning: string, index: number) => `${index}-${warning}`)
        }
        .width('100%')
        .padding({ left: 16, right: 16 })
      }

      if (this.loading) {
        LoadingRow({ text: '正在检索…（语义/混合可能要加载模型）' })
      } else if (this.results.length === 0) {
        if (this.searched) {
          if (this.error.code.length > 0) {
            // `error` 有值 = 这次检索**没有完整完成**（`search.start` 失败，或混合升级
            // 失败）。这时不能说「没有结果 / 换一个关键词」：那是把失败说成「库里没有」，
            // 用户会白改关键词。错误条已经在上面显示，这里只补一句边界说明。
            Text('这次检索没有完成，结果不代表「没有匹配的内容」；原因见上面的错误。' +
              '可以重试搜索，或先确认索引状态。')
              .fontSize(12)
              .fontColor(SketchColors.muted)
              .width('100%')
              .padding({ left: 16, right: 16 })
          } else {
            EmptyState({
              icon: '—',
              title: '没有结果',
              hint: this.warnings.length > 0
                ? '原因见上方的核心提示；不要把「没有结果」当成「没有内容」。'
                : (this.mode === 'semantic' && this.indexStatus !== null && !this.semanticReady()
                  ? '语义索引还没有可用向量；先到设置页确认模型与重建状态。'
                  : '换一个关键词，或切换检索模式。'),
            })
          }
        } else {
          EmptyState({
            icon: '⌕',
            title: '搜一搜已经记下的内容',
            hint: '输入关键词后按回车。切换「语义/混合」会走不同的检索路径。',
          })
        }
      } else {
        List({ space: 10 }) {
          ForEach(this.results, (hit: SearchHit) => {
            ListItem() {
              this.resultCard(hit)
            }
          }, (hit: SearchHit) => hit.hit_id)
          if (this.loadingMore) {
            ListItem() {
              LoadingRow({ text: '加载下一页…' })
            }
          } else if (this.hasMore) {
            ListItem() {
              Button('加载更多')
                .attributeModifier(sketchButton).drawModifier(sketchOutline())
                .fontSize(13)
                .backgroundColor(SketchColors.soft)
                .fontColor(SketchColors.text)
                .onClick(() => {
                  this.loadMore();
                })
            }
          } else {
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
        .width('100%')
        .layoutWeight(1)
        .padding({ left: 16, right: 16 })
        .onReachEnd(() => {
          this.loadMore();
        })
      }
    }
    .width('100%')
    .height('100%')
    .backgroundColor(SketchColors.canvas)
  }
}

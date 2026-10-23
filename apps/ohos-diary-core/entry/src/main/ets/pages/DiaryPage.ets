import { SketchColors, SketchFonts, SketchType, SketchCard, sketchButton, sketchOutline } from '../ui/Sketch';
// 与 Windows 共用视觉组件，业务语义由现有 Store 提供。
// 本文件只做「把 DiaryStore 的状态显示出来 + 把按钮点下去」。
// 请勿在此加入业务逻辑——那会让重做 UI 时不得不搬逻辑。
//
// ── 重做 UI 时这一页可以直接删掉，下面三件事不用搬 ───────────────────────────
// 1. 业务口径全在 `state/DiaryStore.ets`：候选 ≠ 当前、`waiting_configuration`
//    不是生成失败、`not_found` 是空态——都是 store 算好的结论，本页只做镜像与渲染，
//    不猜、不判断、不重算。
// 2. **正文读取是页面级的临时做法**：`DiaryStore` 只给版本 id 与结论（它没有包
//    `diary.getVersion`）。这里用 `diary.getVersion`——与
//    `DiaryCore.rawCall('diary.getVersion', { version_id })` 是同一个 C ABI 方法，
//    只是走 `CoreGateway` 放到 worker 线程，不在 UI 线程碰 SQLite——**只在页面里读、
//    不做缓存**：每次打开某天 / 切换当前版本 / 点「重读正文」各读一次，结果只落在本页
//    `@State` 里。重做 UI 时请换成正规接口，不要把这十几行搬走。
// 3. 样式只用现成组件（PageTitle / StatePill / ErrorBanner / EmptyState / LoadingRow）
//    与 `ui/Sketch.ets` 的 SketchColors / SketchType：没有配色、圆角、阴影、动画。

import { common } from '@kit.AbilityKit';
import {
  DiaryDay,
  DiaryGenerationTicket,
  DiaryVersion,
  DiaryVersionParams,
} from '../core/Models';
import { DiaryStore } from '../state/DiaryStore';
import { CoreGateway, CoreResult, uiErrorOf } from '../ui/CoreGateway';
import { EMPTY_UI_ERROR, UiError } from '../ui/Errors';
import { databasePath } from '../ui/AppRuntime';
import { EmptyState, ErrorBanner, LoadingRow, PageTitle, SectionTitle, StatePill } from '../ui/Common';

/**
 * 日记（临时功能界面）：上面是这一天，下面是「有材料的天」列表。
 *
 * 结构与 store 的调用一一对应，方便重做 UI 时按块替换：
 *
 * | 界面块 | 调 store / 核心 | 显示什么 |
 * |---|---|---|
 * | 天列表 | `refresh()` → `days` | `day_key` + 材料数 + 当前/候选徽标 |
 * | 点某一天 | `openDay(dayKey)` | `day` 的 `material_counts` / `revision` / `coverage` |
 * | 当前版本 | `currentVersionId()` | 「当前版本」+ id + 正文（页面级读取） |
 * | 只有候选 | `candidateVersionIds()` / `candidatesOnly()` | 「候选版本（未采用）」+ 候选列表 |
 * | 生成 | `generate()` → `runDueJobs()` → `openDay()` | 票据/任务结果原话 |
 * | 采用 | `setCurrent(versionId)` | 新的 `current_version_id` |
 * | 等配置 | `waitingConfiguration` | 「还没配置模型服务，不是生成失败」 |
 */
@Component
export struct DiaryPage {
  @StorageProp('ohosWideLayout') desktop: boolean = false;
  /** 页面不再 push 详情页（当天内容就显示在本页），但 Index 仍按这个签名构造本组件。 */
  pathStack: NavPathStack = new NavPathStack();

  // ---- store 字段的镜像（store 不依赖 ArkUI，页面自己同步） ----
  @State days: DiaryDay[] = [];
  @State daysTruncated: boolean = false;
  /** store 的 `daysLoaded`：至少成功读过一次天列表。false 时不能把空数组说成「没有天」。 */
  @State daysLoaded: boolean = false;
  @State dayKey: string = '';
  @State day: DiaryDay | null = null;
  @State dayEmpty: boolean = false;
  /** store 的 `currentVersionId()` 结论（空串 = 没有当前版本）。页面不自己从版本列表里猜。 */
  @State currentVersionId: string = '';
  /** store 的 `candidateVersionIds()` 结论（最新在前；空数组 = 没有候选）。 */
  @State candidateVersionIds: string[] = [];
  /** store 的 `candidatesOnly()` 结论：只有候选、没有当前——这才是「候选版本（未采用）」。 */
  @State candidatesOnly: boolean = false;
  @State waitingConfiguration: boolean = false;
  @State waitingCount: number = 0;
  /** store 的 `jobsStatusKnown`：`waitingConfiguration` 背后那次 `jobs.status` 是否读成功过。 */
  @State jobsStatusKnown: boolean = false;
  @State storeError: UiError = EMPTY_UI_ERROR;
  @State storeMessage: string = '';
  @State busy: boolean = false;

  // ---- 页面自己的临时提示（只是把刚发生的动作用人话说一遍，不是业务结论） ----
  @State acceptedNote: string = '';
  @State runNote: string = '';

  // ---- 正文：页面级临时读取，不缓存（见文件头第 2 条） ----
  @State bodyVersionId: string = '';
  @State bodyLabel: string = '';
  @State bodyText: string = '';
  @State bodyError: string = '';

  @StorageLink('ohosDataVersion') @Watch('onDataVersionChanged') dataVersion: number = 0;

  private dbPath: string = '';
  private store: DiaryStore | null = null;
  /** 正文读取的令牌：快速点两版时丢掉先到的旧结果（纯显示问题，不涉及业务）。 */
  private bodySeq: number = 0;

  aboutToAppear(): void {
    const context = getContext(this) as common.UIAbilityContext;
    this.dbPath = databasePath(context);
    this.store = new DiaryStore(this.dbPath);
    this.reload();
  }

  onDataVersionChanged(): void {
    if (this.store !== null) {
      this.reload();
    }
  }

  // ---------------------------------------------------------------- store 调用

  /** 天列表 + （已选那一天的）当天状态 + 任务状态，一次 `refresh()` 拿齐。 */
  private async reload(): Promise<void> {
    const store: DiaryStore | null = this.store;
    if (store === null) {
      return;
    }
    this.busy = true;
    await store.refresh();
    this.sync(store);
    this.busy = false;
    await this.readCurrentBody();
  }

  /** 点天列表里的一天：`openDay` 之后当天块显示它的真实字段。 */
  private async openDay(dayKey: string): Promise<void> {
    const store: DiaryStore | null = this.store;
    if (store === null) {
      return;
    }
    this.acceptedNote = '';
    this.runNote = '';
    this.busy = true;
    await store.openDay(dayKey);
    this.sync(store);
    this.busy = false;
    await this.readCurrentBody();
  }

  /**
   * 生成按钮：`generate` 只受理（返回票据），真正生成要 `runDueJobs`。
   *
   * 顺序照任务要求：受理 → 提示「已受理，要跑任务才会生成」→ 跑任务 → 再 `openDay`
   * 重读这一天。三步的结论都在 store 的 `message` / `error` / `waitingConfiguration`
   * 里，页面只把它们显示出来。
   */
  private async generateAndRun(): Promise<void> {
    const store: DiaryStore | null = this.store;
    if (store === null) {
      return;
    }
    const dayKey: string = this.dayKey;
    this.acceptedNote = '';
    this.runNote = '';
    this.busy = true;
    const ticket: DiaryGenerationTicket | null = await store.generate(dayKey);
    this.sync(store);
    if (ticket === null) {
      // 没受理：原因已经由 store 写进 error / message，原样显示，页面不另编一套。
      this.busy = false;
      return;
    }
    this.acceptedNote = '已受理，要跑任务才会生成。';
    await store.runDueJobs();
    this.sync(store);
    // runDueJobs 的说明在下面这次 openDay 里会被 store 清掉（它清 message），先留住原话。
    this.runNote = store.message;
    await store.openDay(dayKey);
    this.sync(store);
    this.busy = false;
    await this.readCurrentBody();
  }

  /** 候选 → 当前：`setCurrent(versionId)`，其余判定（冲突、不属于这一天）都在 store 里。 */
  private async adopt(versionId: string): Promise<void> {
    const store: DiaryStore | null = this.store;
    if (store === null) {
      return;
    }
    this.busy = true;
    await store.setCurrent(versionId);
    this.sync(store);
    this.busy = false;
    await this.readCurrentBody();
  }

  /** store → 页面 `@State` 的唯一同步点。 */
  private sync(store: DiaryStore): void {
    this.days = store.days;
    this.daysLoaded = store.daysLoaded;
    this.daysTruncated = store.daysTruncated;
    this.dayKey = store.dayKey;
    this.day = store.day;
    this.dayEmpty = store.dayEmpty;
    // 「候选 vs 当前」只认 store 的三个只读结论，页面不自己拼。
    const current: string | null = store.currentVersionId();
    this.currentVersionId = current === null ? '' : current;
    this.candidateVersionIds = store.candidateVersionIds();
    this.candidatesOnly = store.candidatesOnly();
    this.waitingConfiguration = store.waitingConfiguration;
    this.waitingCount = store.waitingConfigurationCount;
    this.jobsStatusKnown = store.jobsStatusKnown;
    this.storeError = store.error;
    this.storeMessage = store.message;
    this.busy = store.loading || store.working;
  }

  // ---------------------------------------------------------------- 正文（页面级临时读取）

  /** 有当前版本就读它的正文；没有就不读（候选正文要点「看这一版正文」才读）。 */
  private async readCurrentBody(): Promise<void> {
    const id: string = this.currentVersionId;
    if (id.length === 0) {
      this.clearBody();
      return;
    }
    await this.readBody(id, `当前版本 ${id}`);
  }

  private async previewBody(versionId: string): Promise<void> {
    await this.readBody(versionId, `候选版本 ${versionId}`);
  }

  private async reloadBody(): Promise<void> {
    if (this.bodyVersionId.length === 0) {
      await this.readCurrentBody();
      return;
    }
    await this.readBody(this.bodyVersionId, this.bodyLabel);
  }

  /**
   * 读一版正文并显示。
   *
   * `diary.getVersion` 与 `DiaryCore.rawCall('diary.getVersion', { version_id })` 同一
   * 方法、同一参数（snake_case `version_id`）；差别只是走 worker，不阻塞 UI 线程。
   * 读到的正文只放本页 `@State`，不写 store、不跨页缓存。
   */
  private async readBody(versionId: string, label: string): Promise<void> {
    this.bodySeq++;
    const token: number = this.bodySeq;
    this.bodyVersionId = versionId;
    this.bodyLabel = label;
    this.bodyText = '';
    this.bodyError = '';
    if (versionId.length === 0) {
      return;
    }
    const params: DiaryVersionParams = { version_id: versionId };
    const result: CoreResult = await CoreGateway.call(this.dbPath, 'diary.getVersion', params);
    if (token !== this.bodySeq) {
      // 期间又点了别的版本：先到的旧结果直接丢掉，避免正文和标签对不上。
      return;
    }
    if (!result.ok) {
      const failure: UiError = uiErrorOf(result);
      this.bodyError = `${failure.message}（${failure.code}）`;
      return;
    }
    const version: DiaryVersion = result.data as DiaryVersion;
    this.bodyText = version.body;
  }

  private clearBody(): void {
    this.bodySeq++;
    this.bodyVersionId = '';
    this.bodyLabel = '';
    this.bodyText = '';
    this.bodyError = '';
  }

  // ---------------------------------------------------------------- 只读小工具（不判断业务，只取值/拼串）

  private materialLine(): string {
    const day: DiaryDay | null = this.day;
    if (day === null) {
      return '';
    }
    return `材料 ${day.material_counts.captures} 条 · 已处理 ${day.material_counts.processed} 条 · ` +
      `待处理 ${day.material_counts.pending} 条 · 未完成 ${day.material_counts.failed} 条`;
  }

  private metaLine(): string {
    const day: DiaryDay | null = this.day;
    if (day === null) {
      return '';
    }
    return `coverage=${day.coverage} · revision=${day.revision} · ` +
      `time_zone=${day.time_zone} · updated_at=${day.updated_at}`;
  }

  private dayRowKey(day: DiaryDay): string {
    const current: string = day.current_version_id === null ? 'none' : day.current_version_id;
    return `${day.day_key}-${current}-${day.candidate_version_ids.length}-` +
      `${day.material_counts.captures}-${day.revision}`;
  }

  // ---------------------------------------------------------------- 界面

  @Builder
  header() {
    Row({ space: 8 }) {
      Column() {
        PageTitle({
          title: '日记',
          subtitle: '把留下的片段，慢慢整理成一页。',
        })
      }
      .layoutWeight(1)
      .alignItems(HorizontalAlign.Start)

      Button(this.busy ? '处理中…' : '刷新')
        .attributeModifier(sketchButton).drawModifier(sketchOutline())
        .enabled(!this.busy)
        .onClick(() => {
          this.reload();
        })
    }
    .width('100%')
    .alignItems(VerticalAlign.Top)
    .padding({ left: 16, right: 16, top: 12, bottom: 8 })
  }

  /**
   * 状态条：等待配置 / 错误 / 提示。
   *
   * **两者不是互斥关系，必须都显示**：
   * - `waitingConfiguration` 解释「为什么没有生成」（全队列口径，含索引等本地模型任务）；
   * - `storeError` 说明「这次调用失败了」。
   *
   * 以前这里是 `if (waiting) {…} else if (error) {…}`：只要队列里有一个任务在等配置，
   * 任何真实错误都会被 `else` 吃掉，页面还断言「不是生成失败」。现在两个块各自渲染。
   * 队列状态读失败时 store 已复位 `waitingConfiguration`；下面再补一句「无法判断」，
   * 不让人把「没读到」当成「没有任务在等配置」。
   */
  @Builder
  statusStrip() {
    Column({ space: 6 }) {
      Button('配置 AI 模型服务').attributeModifier(sketchButton).drawModifier(sketchOutline())
        .onClick(() => { this.pathStack.pushPathByName('llm', ''); })
      if (this.waitingConfiguration) {
        Text('还没配置模型服务，不是生成失败')
          .fontSize(SketchType.section)
          .fontColor(SketchColors.ink)
          .width('100%')
        Text(`队列里有 ${this.waitingCount} 个任务停在 waiting_configuration` +
          '（requires_user_action=true；这是「全队列」口径，含语义索引等本地模型任务，' +
          '不是「这一天这次生成」的结论）。配置好后任务会自动继续，不用重新发起。')
          .fontSize(SketchType.caption)
          .fontColor(SketchColors.muted)
          .width('100%')
      }
      if (this.storeError.code.length > 0) {
        ErrorBanner({
          error: this.storeError,
          showReload: true,
          onReload: () => {
            this.reload();
          },
        })
      }
      if (!this.jobsStatusKnown && this.storeError.code.length > 0) {
        Text('队列状态这次没读出来：现在无法判断有没有任务在等配置模型服务（原因见上面的错误）。')
          .fontSize(SketchType.caption)
          .fontColor(SketchColors.muted)
          .width('100%')
      }

      if (this.acceptedNote.length > 0) {
        Text(this.acceptedNote)
          .fontSize(SketchType.body)
          .fontColor(SketchColors.text)
          .width('100%')
      }
      if (this.runNote.length > 0) {
        Text(this.runNote)
          .fontSize(SketchType.caption)
          .fontColor(SketchColors.muted)
          .width('100%')
      }
      if (this.storeMessage.length > 0) {
        Text(this.storeMessage)
          .fontSize(SketchType.caption)
          .fontColor(SketchColors.muted)
          .width('100%')
      }
    }
    .width('100%')
    .alignItems(HorizontalAlign.Start)
    .padding({ left: 16, right: 16, bottom: 8 })
  }

  /** 当天块：材料真实字段 + 当前/候选两个状态分开写 + 生成 + 候选采用 + 正文。 */
  @Builder
  dayPanel() {
    Column({ space: 8 }) {
      Row({ space: 8 }) {
        Text(`这一天：${this.dayKey}`)
          .fontSize(SketchType.section)
          .fontColor(SketchColors.ink)
        Blank()
        Button('重读这一天')
          .attributeModifier(sketchButton).drawModifier(sketchOutline())
          .enabled(!this.busy)
          .onClick(() => {
            this.openDay(this.dayKey);
          })
      }
      .width('100%')

      if (this.day === null) {
        Text(this.dayEmpty
          ? '这一天还没有材料，也没有成篇日记。'
          : '这一天的状态没有读出来（原因见上面的错误）。')
          .fontSize(SketchType.body)
          .fontColor(SketchColors.muted)
          .width('100%')
      } else {
        // ---- 当前 vs 候选：两个状态分开显示，候选绝不写成「已生成」 ----
        if (this.currentVersionId.length > 0) {
          Row({ space: 8 }) {
            StatePill({ text: '当前版本', tone: 'ok' })
            Text(this.currentVersionId)
              .fontSize(SketchType.body)
              .fontColor(SketchColors.ink)
          }
          .width('100%')
        } else if (this.candidatesOnly) {
          Row({ space: 8 }) {
            StatePill({ text: '候选版本（未采用）', tone: 'warn' })
            Text(`共 ${this.candidateVersionIds.length} 个，还没有当前版本`)
              .fontSize(SketchType.body)
              .fontColor(SketchColors.ink)
          }
          .width('100%')
          Text('候选不是当前：采用其中一版（下面的「采用为当前」）才会成为这一天的正文。')
            .fontSize(SketchType.caption)
            .fontColor(SketchColors.muted)
            .width('100%')
        } else {
          Row({ space: 8 }) {
            StatePill({ text: '还没有版本', tone: 'muted' })
            Text('这一天还没有任何正文版本')
              .fontSize(SketchType.body)
              .fontColor(SketchColors.ink)
          }
          .width('100%')
        }

        // ---- 材料与这一天本身的真实字段（snake_case 原样） ----
        Text(this.materialLine())
          .fontSize(SketchType.caption)
          .fontColor(SketchColors.text)
          .width('100%')
        Text(this.day === null ? '' : `最近更新：${(this.day as DiaryDay).updated_at}`)
          .fontSize(SketchType.caption)
          .fontColor(SketchColors.muted)
          .width('100%')

        // ---- 生成：先受理，再跑任务 ----
        Row({ space: 8 }) {
          Button('整理这一天')
            .attributeModifier(sketchButton).drawModifier(sketchOutline())
            .enabled(!this.busy)
            .onClick(() => {
              this.generateAndRun();
            })
          Text('先受理，再在任务完成后查看正文。')
            .fontSize(SketchType.caption)
            .fontColor(SketchColors.muted)
            .layoutWeight(1)
        }
        .width('100%')

        // ---- 候选版本：每版一行，可看正文、可显式采用 ----
        if (this.candidateVersionIds.length > 0) {
          SectionTitle({ text: '候选版本（未采用）' })
          ForEach(this.candidateVersionIds, (versionId: string) => {
            this.candidateRow(versionId)
          }, (versionId: string) => versionId)
        }

        // ---- 正文：页面级临时读取，不缓存 ----
        Column({ space: 6 }) {
          Row({ space: 8 }) {
            Text('正文')
              .fontSize(SketchType.section)
              .fontColor(SketchColors.ink)
            Text(this.bodyLabel.length > 0 ? this.bodyLabel : '（未选版本）')
              .fontSize(SketchType.caption)
              .fontColor(SketchColors.muted)
            Blank()
            Button('重读正文')
              .attributeModifier(sketchButton).drawModifier(sketchOutline())
              .enabled(!this.busy)
              .onClick(() => {
                this.reloadBody();
              })
          }
          .width('100%')

          if (this.bodyError.length > 0) {
            Text(`正文没有读出来：${this.bodyError}`)
              .fontSize(SketchType.caption)
              .fontColor(SketchColors.ink)
              .width('100%')
          } else if (this.bodyVersionId.length === 0) {
            Text('还没有当前版本。可以先阅读候选，再决定是否采用。')
              .fontSize(SketchType.caption)
              .fontColor(SketchColors.muted)
              .width('100%')
          } else {
            Text(this.bodyText.length > 0 ? this.bodyText : '（这一版正文为空）')
              .fontSize(18)
              .lineHeight(30)
              .fontColor(SketchColors.text)
              .width('100%')
          }
        }
        .width('100%')
        .alignItems(HorizontalAlign.Start)
      }
    }
    .width('100%')
    .alignItems(HorizontalAlign.Start)
    .padding(14)
    .backgroundColor(SketchColors.paper)
    .drawModifier(sketchOutline())
  }

  @Builder
  candidateRow(versionId: string) {
    Flex({ wrap: FlexWrap.Wrap, alignItems: ItemAlign.Center }) {
      Text(versionId)
        .fontSize(SketchType.caption)
        .fontColor(SketchColors.text)
        .width('100%')
        .maxLines(1)
        .textOverflow({ overflow: TextOverflow.Ellipsis })
      Button('看这一版正文')
        .attributeModifier(sketchButton).drawModifier(sketchOutline()).margin({ right: 8, top: 8 })
        .enabled(!this.busy)
        .onClick(() => {
          this.previewBody(versionId);
        })
      Button('采用为当前')
        .attributeModifier(sketchButton).drawModifier(sketchOutline()).margin({ right: 8, top: 8 })
        .enabled(!this.busy)
        .onClick(() => {
          this.adopt(versionId);
        })
    }
    .width('100%')
  }

  @Builder
  dayRow(day: DiaryDay) {
    Column({ space: 4 }) {
      Row({ space: 8 }) {
        Text(day.day_key)
          .fontSize(SketchType.section)
          .fontColor(SketchColors.ink)
        if (day.current_version_id !== null) {
          StatePill({ text: '当前', tone: 'ok' })
        }
        if (day.candidate_version_ids.length > 0) {
          StatePill({ text: `候选 ${day.candidate_version_ids.length}`, tone: 'warn' })
        }
        Blank()
        Text(this.dayKey === day.day_key ? '（已打开）' : '点开')
          .fontSize(SketchType.caption)
          .fontColor(SketchColors.muted)
      }
      .width('100%')

      Text(`材料 ${day.material_counts.captures} 条 · ` +
        `已处理 ${day.material_counts.processed} 条 · ` +
        `待处理 ${day.material_counts.pending} 条 · 未完成 ${day.material_counts.failed} 条`)
        .fontSize(SketchType.caption)
        .fontColor(SketchColors.muted)
        .width('100%')
    }
    .width('100%')
    .alignItems(HorizontalAlign.Start)
    .padding({ top: 10, bottom: 10 })
    .onClick(() => {
      this.openDay(day.day_key);
    })
  }

  @Builder
  listHeading() {
    Row({ space: 8 }) {
      SectionTitle({ text: '留下的日子' })
      Blank()
      if (this.daysTruncated) {
        Text('还有未加载的日子')
          .fontSize(SketchType.caption)
          .fontColor(SketchColors.muted)
      }
    }
    .width('100%')
  }

  @Builder
  dayList() {
      List({ space: 8 }) {
        if (!this.desktop && this.dayKey.length > 0) {
          ListItem() { this.dayPanel() }
        }

        ListItem() {
          this.listHeading()
        }

        if (this.busy && this.days.length === 0) {
          ListItem() {
            LoadingRow({ text: '正在读取日记列表…' })
          }
        } else if (this.days.length === 0 && this.daysLoaded) {
          ListItem() {
            EmptyState({
              icon: '日',
              title: '这里还没有日记',
              hint: '先留下一点今天的片段，再慢慢整理成一页。',
            })
          }
        } else if (this.days.length === 0) {
          // 一次都没读成功：这里是「读失败」，不是「没有天」。错误条已经在上面显示，
          // 这里不能再说「还没有有材料的天」——那是把失败说成空。
          ListItem() {
            Text('天列表没有读出来：这不代表没有有材料的天；原因见上面的错误。')
              .fontSize(SketchType.caption)
              .fontColor(SketchColors.muted)
              .width('100%')
          }
        } else {
          ForEach(this.days, (day: DiaryDay) => {
            ListItem() {
              this.dayRow(day)
            }
          }, (day: DiaryDay) => this.dayRowKey(day))
        }

      }
      .width('100%')
      .layoutWeight(1)
      .padding({ left: 16, right: 16 })
  }

  build() {
    Column() {
      this.header()
      this.statusStrip()
      if (this.desktop) {
        Row({ space: 24 }) {
          Column() { this.dayList() }.width(280).height('100%')
          Divider().vertical(true).color(SketchColors.line)
          Scroll() {
            Column() {
              if (this.dayKey.length > 0) { this.dayPanel() }
              else { EmptyState({ title: '选一天，慢慢回看', hint: '从左侧打开留下的日子。' }) }
            }.width('100%').padding({ right: 16, bottom: 24 })
          }.layoutWeight(1).height('100%')
        }.width('100%').layoutWeight(1).alignItems(VerticalAlign.Top)
      } else {
        this.dayList()
      }
    }
    .width('100%').height('100%').backgroundColor(SketchColors.canvas)
  }
}

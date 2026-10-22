import { SketchColors, SketchFonts, SketchType, SketchCard, sketchButton, sketchOutline } from '../ui/Sketch';
// 与 Windows 共用视觉组件，业务语义由现有 Store 提供。
// 只做「把 RecycleStore 的状态显示出来 + 把按钮点下去」，请勿在此加业务逻辑。
//
// 数据与并发全在 `state/RecycleStore.ets`：本页只把 store 的字段镜像到 @State，
// 调 refresh / restore / previewPurge / purge，然后把结果原样摆出来。
//
// 三条界面口径（对应 store 文件头的硬约束）：
// 1. 列表只显示 `state === 'trashed'`（store 已经筛过；核心的 captures.list 不按状态过滤）；
// 2. 永久删除必须先生成预览，再把三类版本清单**分开**摆出来，绝不合并成一句：
//    - `diary_versions_still_referencing`：开关打开时才删；
//    - `derived_diary_version_ids`：**不删**，但正文可能仍含这条记录的文字；
//    - `undetermined_diary_version_ids`：**不删**，判不出归属就如实说「有 N 份无法确定归属」；
// 3. purge 失败（令牌过期 / 清单变了）时只说「没有删除、请重新预览后再确认」，
//    绝不显示成清理成功——store 已经把旧令牌作废，并按需要重新预览。
//
// 这是测试与推进用的功能界面：不做视觉设计，颜色最多用 `ui/Sketch.ets`；
// 也没有新增 ui_strings key（直接写中文）。
//
// ✅ 历史缺口（2026-10-22 真机复现，已由 `ui/CoreSessions.ets` 修掉；页面逻辑不变）：
// 以前 `RecycleStore.previewPurge()` 与 `purge()` 是两次 `CoreGateway.call`，每次调用都在
// TaskPool worker 里 open → 调核心 → close；而核心的 `preview_token` 只存在**当前核心
// 会话的内存里**（`crates/diary_core/src/recycle.rs` 明确不持久化）。最后一个句柄关闭后
// 核心析构，令牌随之消失，所以本页点「确认永久删除」必然拿到 `invalid_state`。
// 现在 `CoreGateway` 按资料库路径缓存 worker 会话，跨调用复用同一个 Core（预览与清理即使
// 落在不同 taskpool 线程，底层也是同一个核心），本页不用改。
// store 仍按原口径处理失败：invalid_state 时作废旧令牌并只读重预览，绝不伪装成功。

import { common } from '@kit.AbilityKit';
import { Capture, PurgePreview, PurgeResult } from '../core/Models';
import { EMPTY_UI_ERROR, UiError } from '../ui/Errors';
import { RecycleStore } from '../state/RecycleStore';
import { AppState } from '../ui/AppState';
import { confirmAction, databasePath, formatDateTime } from '../ui/AppRuntime';
import { EmptyState, ErrorBanner, LoadingRow, PageTitle, StatePill } from '../ui/Common';

@Component
export struct RecyclePage {
  // ---- store 的字段镜像（页面不自己推导事实，只显示这些） --------------------
  @State items: Capture[] = [];
  @State preview: PurgePreview | null = null;
  @State lastPurge: PurgeResult | null = null;
  @State previewStale: boolean = false;
  @State busy: boolean = false;
  @State loading: boolean = true;
  @State error: UiError = EMPTY_UI_ERROR;
  @State message: string = '';
  @State scanned: number = 0;
  @State truncated: boolean = false;
  /** store 的 `loaded`：至少成功扫过一次回收站。false 时不能说「回收站是空的」。 */
  @State loaded: boolean = false;

  // ---- 只属于这一屏的选择 ----------------------------------------------------
  /** 「同时删除日记版本」开关；默认 false = 保留仍引用这条记录的版本。 */
  @State deleteDiaryVersions: boolean = false;
  /** 上次成功 purge 时用的选择，用来解释 lastPurge 里哪一类版本被删/被留。 */
  @State lastPurgeDeletedVersions: boolean = false;
  /** purge 失败后的界面结论（尤其是令牌作废）；成功时不显示。 */
  @State purgeNote: string = '';

  private dbPath: string = '';
  private store: RecycleStore | null = null;

  aboutToAppear(): void {
    const context = getContext(this) as common.UIAbilityContext;
    this.dbPath = databasePath(context);
    this.store = new RecycleStore(this.dbPath);
    this.reload();
  }

  private mirror(): void {
    const store: RecycleStore | null = this.store;
    if (store === null) {
      return;
    }
    this.items = store.captures;
    this.preview = store.preview;
    this.lastPurge = store.lastPurge;
    this.previewStale = store.previewStale;
    this.busy = store.loading || store.working;
    this.loading = store.loading;
    this.error = store.error;
    this.message = store.message;
    this.scanned = store.scanned;
    this.truncated = store.truncated;
    this.loaded = store.loaded;
  }

  private async reload(): Promise<void> {
    const store: RecycleStore | null = this.store;
    if (store === null) {
      return;
    }
    this.purgeNote = '';
    await store.refresh();
    this.mirror();
  }

  private async doRestore(capture: Capture): Promise<void> {
    const store: RecycleStore | null = this.store;
    if (store === null || this.busy) {
      return;
    }
    this.purgeNote = '';
    const ok: boolean = await store.restore(capture);
    this.mirror();
    if (ok) {
      // 恢复后这条记录会回到主列表/片段；让常驻页重读一次。
      AppState.bumpDataVersion();
    }
  }

  private async doPreview(capture: Capture): Promise<void> {
    const store: RecycleStore | null = this.store;
    if (store === null || this.busy) {
      return;
    }
    this.purgeNote = '';
    await store.previewPurge(capture.id);
    this.mirror();
  }

  private async doPurge(): Promise<void> {
    const store: RecycleStore | null = this.store;
    const preview: PurgePreview | null = this.preview;
    if (store === null || preview === null || this.busy) {
      return;
    }
    const deleteVersions: boolean = this.deleteDiaryVersions;
    const confirmed: boolean = await confirmAction(
      '确认永久删除？',
      this.purgeConfirmText(preview, deleteVersions),
      '永久删除',
    );
    if (!confirmed) {
      return;
    }
    const result: PurgeResult | null = await store.purge(deleteVersions);
    this.mirror();
    this.purgeNote = '';
    if (result !== null) {
      this.lastPurgeDeletedVersions = deleteVersions;
      // 成功：开关回到默认关（下一次清理仍默认保留日记版本）。
      this.deleteDiaryVersions = false;
      AppState.bumpDataVersion();
      return;
    }
    if (this.error.code.length > 0) {
      // 其它失败原因由 ErrorBanner 显示；这里不编造「令牌过期」。
      return;
    }
    // store 对 invalid_state（令牌过期 / 预览后清单变了）不写 error，只写 message。
    // 这句话必须让用户知道**没有删成功**，并且下一步是重新预览，不是重试。
    if (this.preview === null) {
      this.purgeNote = '永久删除没有发生，记录没有被删除：预览令牌已作废，请重新预览后再确认。';
    } else {
      this.purgeNote = '永久删除没有发生，记录没有被删除：旧预览令牌已作废，已重新读取最新的清理影响；' +
        '请核对上面的清单后再确认。';
    }
  }

  private purgeConfirmText(preview: PurgePreview, deleteVersions: boolean): string {
    const referencing: number = preview.diary_versions_still_referencing.length;
    let text: string = `会永久删除这条记录，以及原件 ${preview.asset_ids.length} 个、` +
      `派生内容 ${preview.derived_content_ids.length} 个、AI 回应 ${preview.ai_response_ids.length} 个。`;
    if (deleteVersions) {
      text += `同时删除 ${referencing} 个有可判定关联的日记版本。`;
    } else {
      text += `${referencing} 个仍引用这条记录的日记版本会保留。`;
    }
    text += `另有 ${preview.derived_diary_version_ids.length} 个派生版本、` +
      `${preview.undetermined_diary_version_ids.length} 个归属未定版本不会删除。此操作不可逆。`;
    return text;
  }

  /** 正文摘要：只取 `draft_text`，压成一行，截断到 80 个字符。 */
  private summaryOf(capture: Capture): string {
    const text: string = capture.draft_text.trim();
    if (text.length === 0) {
      return '（这条记录没有正文；可能只有音频或文件）';
    }
    const flat: string = text.split('\r').join(' ').split('\n').join(' ').split('\t').join(' ');
    if (flat.length <= 80) {
      return flat;
    }
    return `${flat.substring(0, 80)}…`;
  }

  private previewMatches(captureId: string): boolean {
    const preview: PurgePreview | null = this.preview;
    return preview !== null && preview.capture_id === captureId;
  }

  /** 预览字段的读取口（kind 见 previewCount / previewIds）；页面只读 store 的原样清单。 */
  private previewCaptureId(): string {
    const preview: PurgePreview | null = this.preview;
    return preview === null ? '' : preview.capture_id;
  }

  private previewCount(kind: number): number {
    const preview: PurgePreview | null = this.preview;
    if (preview === null) {
      return 0;
    }
    if (kind === 1) {
      return preview.diary_versions_still_referencing.length;
    }
    if (kind === 2) {
      return preview.derived_diary_version_ids.length;
    }
    if (kind === 3) {
      return preview.undetermined_diary_version_ids.length;
    }
    if (kind === 4) {
      return preview.retained_source_snapshot_ids.length;
    }
    if (kind === 5) {
      return preview.asset_ids.length;
    }
    if (kind === 6) {
      return preview.derived_content_ids.length;
    }
    if (kind === 7) {
      return preview.ai_response_ids.length;
    }
    return 0;
  }

  private previewIds(kind: number): string[] {
    const preview: PurgePreview | null = this.preview;
    if (preview === null) {
      return [];
    }
    if (kind === 1) {
      return preview.diary_versions_still_referencing;
    }
    if (kind === 2) {
      return preview.derived_diary_version_ids;
    }
    if (kind === 3) {
      return preview.undetermined_diary_version_ids;
    }
    if (kind === 4) {
      return preview.retained_source_snapshot_ids;
    }
    return [];
  }

  /** 第 ① 类的口径说明：必须跟开关同一步更新（别让界面说「默认保留」却删了版本）。 */
  private referencingNote(): string {
    if (this.deleteDiaryVersions) {
      return '开关已打开（delete_diary_versions=true）：它们会随这次清理一起删除；只删有可判定关联的版本。';
    }
    return '默认保留、这次不删除（delete_diary_versions=false）；打开下面的开关才会删它们。';
  }

  // ---------------------------------------------------------------- 组件

  @Builder
  idList(ids: string[]) {
    Column({ space: 2 }) {
      if (ids.length === 0) {
        Text('（无）')
          .fontSize(SketchType.caption)
          .fontColor(SketchColors.muted)
          .width('100%')
      } else {
        ForEach(ids, (id: string, index: number) => {
          Text(`· ${id}`)
            .fontSize(SketchType.caption)
            .fontColor(SketchColors.text)
            .width('100%')
        }, (id: string, index: number) => `${index}-${id}`)
      }
    }
    .width('100%')
    .alignItems(HorizontalAlign.Start)
  }

  /** 一类版本 = 一个独立小节：标题、口径说明、id 清单都不与其它类合并。 */
  @Builder
  versionBlock(heading: string, note: string, ids: string[]) {
    Column({ space: 2 }) {
      Text(heading)
        .fontSize(SketchType.body)
        .fontWeight(FontWeight.Medium)
        .fontColor(SketchColors.ink)
        .width('100%')
      Text(note)
        .fontSize(SketchType.caption)
        .fontColor(SketchColors.muted)
        .width('100%')
      this.idList(ids)
    }
    .width('100%')
    .padding({ top: 6, bottom: 6 })
    .alignItems(HorizontalAlign.Start)
  }

  /**
   * 预览面板（没有参数：所有字段都从 @State `preview` / `deleteDiaryVersions` 读）。
   *
   * 不把动态文案/清单当 @Builder 参数传：ArkUI 的 @Builder 参数是值传递、不随状态
   * 刷新——真机验证时开关打开后「①」的说明还停在「默认保留」，正是这么来的。
   * 这里四类清单各自一个 Column，标题/说明/id 都直接读状态，保证界面不说谎。
   */
  @Builder
  previewPanel() {
    if (this.preview !== null) {
      Column({ space: 6 }) {
        Text('永久删除预览（还没有删除任何东西）')
          .fontSize(SketchType.section)
          .fontWeight(FontWeight.Medium)
          .fontColor(SketchColors.ink)
          .width('100%')

        Text(`记录 ${this.previewCaptureId()}`)
          .fontSize(SketchType.caption)
          .fontColor(SketchColors.muted)
          .width('100%')

        Text('预览令牌：一次性，绑定这条记录；过期或预览后内容变了就会作废，必须重新预览。')
          .fontSize(SketchType.caption)
          .fontColor(SketchColors.muted)
          .width('100%')

        Text(`会随记录一起删除：原件 ${this.previewCount(5)} 个、` +
          `派生内容 ${this.previewCount(6)} 个、AI 回应 ${this.previewCount(7)} 个。`)
          .fontSize(SketchType.caption)
          .fontColor(SketchColors.text)
          .width('100%')

        // ① 有可判定关联的版本：删不删由开关决定（说明直接读状态，不经过 Builder 参数）。
        Column({ space: 2 }) {
          Text(`① 仍引用这条记录的日记版本：${this.previewCount(1)} 个`)
            .fontSize(SketchType.body)
            .fontWeight(FontWeight.Medium)
            .fontColor(SketchColors.ink)
            .width('100%')
          Text(this.referencingNote())
            .fontSize(SketchType.caption)
            .fontColor(SketchColors.muted)
            .width('100%')
          if (this.previewCount(1) === 0) {
            Text('（无）')
              .fontSize(SketchType.caption)
              .fontColor(SketchColors.muted)
              .width('100%')
          } else {
            ForEach(this.previewIds(1), (id: string, index: number) => {
              Text(`· ${id}`)
                .fontSize(SketchType.caption)
                .fontColor(SketchColors.text)
                .width('100%')
            }, (id: string, index: number) => `ref-${index}-${id}`)
          }
        }
        .width('100%')
        .padding({ top: 6, bottom: 6 })
        .alignItems(HorizontalAlign.Start)

        // ② 派生版本：永远不删，必须告诉用户它们还在。
        Column({ space: 2 }) {
          Text(`② 由这些版本派生的日记版本：${this.previewCount(2)} 个`)
            .fontSize(SketchType.body)
            .fontWeight(FontWeight.Medium)
            .fontColor(SketchColors.ink)
            .width('100%')
          Text('不删除，仍然保留；它们的正文可能仍含这条记录的文字，清理后仍能读到。')
            .fontSize(SketchType.caption)
            .fontColor(SketchColors.muted)
            .width('100%')
          if (this.previewCount(2) === 0) {
            Text('（无）')
              .fontSize(SketchType.caption)
              .fontColor(SketchColors.muted)
              .width('100%')
          } else {
            ForEach(this.previewIds(2), (id: string, index: number) => {
              Text(`· ${id}`)
                .fontSize(SketchType.caption)
                .fontColor(SketchColors.text)
                .width('100%')
            }, (id: string, index: number) => `derived-${index}-${id}`)
          }
        }
        .width('100%')
        .padding({ top: 6, bottom: 6 })
        .alignItems(HorizontalAlign.Start)

        // ③ 归属未定：判不出来就不删，也不许假装没有。
        Column({ space: 2 }) {
          Text(`③ 无法确定归属的日记版本：${this.previewCount(3)} 个`)
            .fontSize(SketchType.body)
            .fontWeight(FontWeight.Medium)
            .fontColor(SketchColors.ink)
            .width('100%')
          Text(`有 ${this.previewCount(3)} 份版本无法确定归属，未删除；判不出来就不删。`)
            .fontSize(SketchType.caption)
            .fontColor(SketchColors.muted)
            .width('100%')
          if (this.previewCount(3) === 0) {
            Text('（无）')
              .fontSize(SketchType.caption)
              .fontColor(SketchColors.muted)
              .width('100%')
          } else {
            ForEach(this.previewIds(3), (id: string, index: number) => {
              Text(`· ${id}`)
                .fontSize(SketchType.caption)
                .fontColor(SketchColors.text)
                .width('100%')
            }, (id: string, index: number) => `undetermined-${index}-${id}`)
          }
        }
        .width('100%')
        .padding({ top: 6, bottom: 6 })
        .alignItems(HorizontalAlign.Start)

        // ④ 输入快照：purge 不删（#161 方案 A），正文可能仍有这条记录的文字。
        Column({ space: 2 }) {
          Text(`④ 清理后仍保留的输入快照：${this.previewCount(4)} 份`)
            .fontSize(SketchType.body)
            .fontWeight(FontWeight.Medium)
            .fontColor(SketchColors.ink)
            .width('100%')
          Text('不删除；快照正文可能仍含这条记录的文字。永久清理不等于「所有副本里的字节都消失」。')
            .fontSize(SketchType.caption)
            .fontColor(SketchColors.muted)
            .width('100%')
          if (this.previewCount(4) === 0) {
            Text('（无）')
              .fontSize(SketchType.caption)
              .fontColor(SketchColors.muted)
              .width('100%')
          } else {
            ForEach(this.previewIds(4), (id: string, index: number) => {
              Text(`· ${id}`)
                .fontSize(SketchType.caption)
                .fontColor(SketchColors.text)
                .width('100%')
            }, (id: string, index: number) => `snapshot-${index}-${id}`)
          }
        }
        .width('100%')
        .padding({ top: 6, bottom: 6 })
        .alignItems(HorizontalAlign.Start)

        Row({ space: 10 }) {
          Column({ space: 2 }) {
            Text('同时删除日记版本（delete_diary_versions）')
              .fontSize(SketchType.body)
              .fontColor(SketchColors.ink)
              .width('100%')
            Text('默认关。开启只影响上面第 ① 类；第 ②③ 类无论开关如何都不会被删除。')
              .fontSize(SketchType.caption)
              .fontColor(SketchColors.muted)
              .width('100%')
          }
          .layoutWeight(1)
          .alignItems(HorizontalAlign.Start)

          Toggle({ type: ToggleType.Switch, isOn: this.deleteDiaryVersions })
            .selectedColor(SketchColors.ink)
            .enabled(!this.busy)
            .onChange((isOn: boolean) => {
              this.deleteDiaryVersions = isOn;
              this.purgeNote = '';
            })
        }
        .width('100%')
        .padding({ top: 6 })

        Button(this.busy ? '处理中…' : (this.deleteDiaryVersions
          ? '确认永久删除（同时删除第 ① 类版本）'
          : '确认永久删除（保留日记版本）'))
          .attributeModifier(sketchButton).drawModifier(sketchOutline())
          .fontSize(SketchType.body)
          .width('100%')
          .enabled(!this.busy)
          .backgroundColor(SketchColors.paper)
          .fontColor(SketchColors.alert)
          .onClick(() => {
            this.doPurge();
          })

        Text('永久删除不可逆：先看清上面四类清单，再点确认。')
          .fontSize(SketchType.caption)
          .fontColor(SketchColors.alert)
          .width('100%')
      }
      .width('100%')
      .padding(12)
      .backgroundColor(SketchColors.soft)
      .border({ width: 1, color: SketchColors.line })
      .alignItems(HorizontalAlign.Start)
    }
  }

  @Builder
  captureCard(capture: Capture) {
    SketchCard() {
      Column({ space: 6 }) {
        Row({ space: 8 }) {
          StatePill({ text: '回收站', tone: 'alert' })
          Text(`revision ${capture.revision}`)
            .fontSize(SketchType.caption)
            .fontColor(SketchColors.muted)
          Blank()
          Text(formatDateTime(capture.updated_at))
            .fontSize(SketchType.caption)
            .fontColor(SketchColors.muted)
        }
        .width('100%')

        Text(this.summaryOf(capture))
          .fontSize(SketchType.body)
          .fontColor(SketchColors.text)
          .maxLines(3)
          .textOverflow({ overflow: TextOverflow.Ellipsis })
          .width('100%')

        // trashed 状态是只读的（核心的写入口对回收站报 invalid_state），所以
        // updated_at 就是移入回收站的时间。
        Text(`移入时间：${formatDateTime(capture.updated_at)} · id ${capture.id}`)
          .fontSize(SketchType.caption)
          .fontColor(SketchColors.muted)
          .width('100%')

        Row({ space: 8 }) {
          Button('恢复')
            .attributeModifier(sketchButton).drawModifier(sketchOutline())
            .fontSize(SketchType.caption)
            .enabled(!this.busy)
            .backgroundColor(SketchColors.soft)
            .fontColor(SketchColors.text)
            .onClick(() => {
              this.doRestore(capture);
            })
          Button('预览永久删除')
            .attributeModifier(sketchButton).drawModifier(sketchOutline())
            .fontSize(SketchType.caption)
            .enabled(!this.busy)
            .backgroundColor(SketchColors.soft)
            .fontColor(SketchColors.text)
            .onClick(() => {
              this.doPreview(capture);
            })
          Blank()
        }
        .width('100%')

        if (this.previewMatches(capture.id)) {
          this.previewPanel()
        }
      }
      .width('100%')
      .alignItems(HorizontalAlign.Start)
    }
  }

  @Builder
  purgeResultCard(result: PurgeResult) {
    SketchCard() {
      Column({ space: 4 }) {
        Text('最近一次永久清理结果（核心原样）')
          .fontSize(SketchType.section)
          .fontWeight(FontWeight.Medium)
          .fontColor(SketchColors.ink)
          .width('100%')
        Text(`已清理：原件 ${result.purged_asset_ids.length} 个、` +
          `派生内容 ${result.purged_derived_content_ids.length} 个、` +
          `AI 回应 ${result.purged_ai_response_ids.length} 个。`)
          .fontSize(SketchType.caption)
          .fontColor(SketchColors.text)
          .width('100%')
        Text(this.lastPurgeDeletedVersions
          ? `日记版本：按选择删除了 ${result.purged_diary_version_ids.length} 个有可判定关联的版本。`
          : `日记版本：按默认保留了 ${result.retained_diary_version_ids.length} 个仍引用这条记录的版本。`)
          .fontSize(SketchType.caption)
          .fontColor(SketchColors.text)
          .width('100%')

        // 三类「没删」的清单仍然分开显示，与预览口径一致。
        this.versionBlock(
          `由它派生、仍然保留的版本：${result.derived_diary_version_ids.length} 个`,
          '不删除；正文可能仍含这条记录的文字。',
          result.derived_diary_version_ids)
        this.versionBlock(
          `无法确定归属、未删除的版本：${result.undetermined_diary_version_ids.length} 个`,
          `有 ${result.undetermined_diary_version_ids.length} 份版本无法确定归属，未删除。`,
          result.undetermined_diary_version_ids)
        this.versionBlock(
          `仍然保留的输入快照：${result.retained_source_snapshot_ids.length} 份`,
          '不删除；快照正文可能仍含这条记录的文字。',
          result.retained_source_snapshot_ids)
      }
      .width('100%')
      .alignItems(HorizontalAlign.Start)
    }
  }

  build() {
    Column({ space: 8 }) {
      Column({ space: 8 }) {
        Row({ space: 8 }) {
          Column() {
            PageTitle({
              title: '回收站',
              subtitle: '移入回收站的记录（state === trashed）。恢复会把记录放回移入前的状态。',
            })
          }
          .layoutWeight(1)
          .alignItems(HorizontalAlign.Start)
          Button(this.busy ? '处理中…' : '刷新')
            .attributeModifier(sketchButton).drawModifier(sketchOutline())
            .fontSize(SketchType.caption)
            .enabled(!this.busy)
            .backgroundColor(SketchColors.soft)
            .fontColor(SketchColors.text)
            .onClick(() => {
              this.reload();
            })
        }
        .width('100%')

        Row({ space: 8 }) {
          Text(this.loading ? '读取中…' : `${this.items.length} 条在回收站`)
            .fontSize(SketchType.caption)
            .fontColor(SketchColors.muted)
          Blank()
          Text(`本次扫描 ${this.scanned} 条记录`)
            .fontSize(SketchType.caption)
            .fontColor(SketchColors.muted)
        }
        .width('100%')
      }
      .width('100%')
      .padding({ left: 16, right: 16, top: 12 })

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

      if (this.truncated) {
        Column() {
          Text(`记录很多：这次只扫描了前 ${this.scanned} 条，回收站列表不完整（truncated=true）；` +
            '后面的记录没有检查，不能当作「回收站只有这些」。')
            .fontSize(SketchType.caption)
            .fontColor(SketchColors.warning)
            .width('100%')
            .padding(10)
            .backgroundColor(SketchColors.soft)
        }
        .padding({ left: 16, right: 16 })
      }

      if (this.message.length > 0) {
        Column() {
          Text(this.message)
            .fontSize(SketchType.caption)
            .fontColor(SketchColors.muted)
            .width('100%')
        }
        .padding({ left: 16, right: 16 })
      }

      if (this.previewStale) {
        Column() {
          Text('预览令牌已作废：要永久删除，请重新预览后再确认；记录没有被删除。')
            .fontSize(SketchType.caption)
            .fontColor(SketchColors.warning)
            .width('100%')
            .padding(10)
            .backgroundColor(SketchColors.soft)
        }
        .padding({ left: 16, right: 16 })
      }

      if (this.purgeNote.length > 0) {
        Column() {
          Text(this.purgeNote)
            .fontSize(SketchType.body)
            .fontColor(SketchColors.alert)
            .width('100%')
            .padding(10)
            .backgroundColor(SketchColors.soft)
        }
        .padding({ left: 16, right: 16 })
      }

      List({ space: 10 }) {
        if (this.lastPurge !== null) {
          ListItem() {
            this.purgeResultCard(this.lastPurge as PurgeResult)
          }
        }
        if (this.loading && this.items.length === 0) {
          ListItem() {
            LoadingRow({ text: '正在扫描回收站…' })
          }
        } else if (this.items.length === 0 && this.loaded) {
          ListItem() {
            Column({ space: 8 }) {
              EmptyState({
                icon: '—',
                title: '回收站是空的',
                hint: '移入回收站的记录会出现在这里；恢复后回到移入前的状态。',
              })
              Text(`这次扫描了 ${this.scanned} 条记录，其中 state === 'trashed' 的 0 条。`)
                .fontSize(SketchType.caption)
                .fontColor(SketchColors.muted)
                .width('100%')
                .textAlign(TextAlign.Center)
            }
            .width('100%')
            .alignItems(HorizontalAlign.Center)
          }
        } else if (this.items.length === 0) {
          // 一次都没扫成功：这是「读失败」，不是「回收站是空的」。
          // 错误条已经在上面显示；这里既不报 0 条，也不说空。
          ListItem() {
            Text('回收站没有读出来：这不代表回收站是空的；原因见上面的错误。')
              .fontSize(SketchType.caption)
              .fontColor(SketchColors.muted)
              .width('100%')
          }
        } else {
          ForEach(this.items, (capture: Capture) => {
            ListItem() {
              this.captureCard(capture)
            }
          }, (capture: Capture) => `${capture.id}-${capture.revision}`)
        }
      }
      .width('100%')
      .layoutWeight(1)
      .padding({ left: 16, right: 16, bottom: 16 })
    }
    .width('100%')
    .height('100%')
    .backgroundColor(SketchColors.canvas)
  }
}

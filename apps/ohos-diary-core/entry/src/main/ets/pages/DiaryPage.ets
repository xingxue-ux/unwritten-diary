// 日记（契约 §2.2）：按天倒序读成篇正文，候选版本在详情里切换。
//
// 数据来源（全部是 C ABI，不读 JSON 之外的任何东西）：
// - `diary.getDay`：某一天的当前版本 / 候选版本 / 材料计数 / revision；
// - `diary.getVersion`：当前版本的正文（列表摘要用）；
// - 详情页 `DiaryDayPage` 再用 `diary.listVersions` / `diary.setCurrent`。
//
// 列表日期的来源（重要，必须诚实）：
// 核心**还没有**按天枚举的入口（`diary.listDays` 未接），所以这里用「最近 60 天 ∪
// 最近 100 条记录覆盖到的日期」当候选，逐天问 `diary.getDay`。只有版本、没有记录、
// 且超出这个范围的日子不会出现在列表里——页脚写明了这一点，不假装是全部。
//
// 空态：`diary.getDay` 对「既没有材料也没有版本」的一天报 `not_found`。那是**空态**，
// 不是读取失败；今天没有成篇正文时显示「今天还没有成篇日记」。
//
// 能力缺失：核心有 `diary.generate`，但 C ABI 还没接（见 m6 C ABI 文档 §6.1），
// 所以本页**不显示**「生成/整理」按钮，只如实说明；不摆按了没反应的假入口。

import { common } from '@kit.AbilityKit';
import {
  CapturePage,
  DiaryDay,
  DiaryDayParams,
  DiaryVersion,
  DiaryVersionParams,
  ListCapturesParams,
} from '../core/Models';
import { CoreGateway, CoreRequest, CoreResult, uiErrorOf } from '../ui/CoreGateway';
import { EMPTY_UI_ERROR, UiError } from '../ui/Errors';
import {
  databasePath,
  dayKeyAtOffset,
  diaryOriginLabel,
  diaryOriginTone,
} from '../ui/AppRuntime';
import { EmptyState, ErrorBanner, LoadingRow, PageTitle, StatePill } from '../ui/Common';

/** 列表里的一天：`day === null` 表示核心报 not_found（这一天没有材料也没有版本）。 */
interface DiaryRow {
  dayKey: string;
  day: DiaryDay | null;
  /** 当前版本的正文；没有当前版本或读取失败时为 null。 */
  current: DiaryVersion | null;
  versionError: UiError;
  isToday: boolean;
}

@Component
export struct DiaryPage {
  pathStack: NavPathStack = new NavPathStack();
  @State rows: DiaryRow[] = [];
  @State loading: boolean = true;
  @State refreshing: boolean = false;
  @State error: UiError = EMPTY_UI_ERROR;
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
    if (this.rows.length === 0) {
      this.loading = true;
    }
    await this.loadRows(token);
    if (token === this.seq) {
      this.loading = false;
      this.refreshing = false;
    }
  }

  private async refresh(): Promise<void> {
    this.seq++;
    const token: number = this.seq;
    this.error = EMPTY_UI_ERROR;
    await this.loadRows(token);
    if (token === this.seq) {
      this.loading = false;
      this.refreshing = false;
    }
  }

  /**
   * 候选日期：最近 60 天 + 最近 100 条记录覆盖到的日期，倒序后最多取 45 天。
   *
   * 60 天是「没有 listDays 时的可见窗口」：本机扫描 60 次 `diary.getDay` 是可接受的
   * 本地 SQLite 开销，同时覆盖「记录被清掉、只剩版本」的日子。
   */
  private async candidateDays(): Promise<string[]> {
    const days: string[] = [];
    const today: string = dayKeyAtOffset(0);
    for (let i = 0; i < 60; i++) {
      days.push(dayKeyAtOffset(-i));
    }
    const params: ListCapturesParams = { limit: 100 };
    const result = await CoreGateway.call(this.dbPath, 'captures.list', params);
    if (result.ok) {
      const page: CapturePage = result.data as CapturePage;
      for (let i = 0; i < page.captures.length; i++) {
        const key: string = page.captures[i].day_key;
        if (days.indexOf(key) < 0) {
          days.push(key);
        }
      }
    }
    // 倒序（YYYY-MM-DD 可以直接按字符串比较），最多 45 天。
    days.sort((left: string, right: string) => {
      if (left === right) {
        return 0;
      }
      return left < right ? 1 : -1;
    });
    const capped: string[] = [];
    for (let i = 0; i < days.length && capped.length < 45; i++) {
      if (capped.indexOf(days[i]) < 0) {
        capped.push(days[i]);
      }
    }
    if (capped.indexOf(today) < 0) {
      capped.unshift(today);
    }
    return capped;
  }

  private async loadRows(token: number): Promise<void> {
    const days: string[] = await this.candidateDays();
    if (token !== this.seq) {
      return;
    }

    const requests: CoreRequest[] = [];
    for (let i = 0; i < days.length; i++) {
      const params: DiaryDayParams = { day_key: days[i] };
      requests.push({ method: 'diary.getDay', params: params });
    }
    const dayResults: CoreResult[] = await CoreGateway.callMany(this.dbPath, requests);
    if (token !== this.seq) {
      return;
    }

    const dayValues: DiaryDay[] = [];
    const dayMissing: boolean[] = [];
    let firstError: UiError = EMPTY_UI_ERROR;
    for (let i = 0; i < days.length; i++) {
      const result: CoreResult = dayResults[i];
      if (result.ok) {
        dayValues.push(result.data as DiaryDay);
        dayMissing.push(false);
        continue;
      }
      dayValues.push({
        day_key: days[i],
        time_zone: '',
        current_version_id: null,
        candidate_version_ids: [],
        revision: 0,
        material_counts: { captures: 0, processed: 0, pending: 0, failed: 0 },
        coverage: 'unavailable',
        updated_at: '',
      });
      dayMissing.push(true);
      const failure: UiError = uiErrorOf(result);
      // `not_found` 是空态，不是错误；其余错误在页面上如实展示。
      if (failure.code !== 'not_found' && firstError.code.length === 0) {
        firstError = failure;
      }
    }
    this.error = firstError;

    // 有当前版本的日期：批量读正文，给列表做摘要。
    const versionRequests: CoreRequest[] = [];
    const versionPositions: number[] = [];
    for (let i = 0; i < days.length; i++) {
      versionPositions.push(-1);
      const day: DiaryDay = dayValues[i];
      if (!dayMissing[i] && day.current_version_id !== null) {
        const params: DiaryVersionParams = { version_id: day.current_version_id };
        versionPositions[i] = versionRequests.length;
        versionRequests.push({ method: 'diary.getVersion', params: params });
      }
    }
    let versionResults: CoreResult[] = [];
    if (versionRequests.length > 0) {
      versionResults = await CoreGateway.callMany(this.dbPath, versionRequests);
      if (token !== this.seq) {
        return;
      }
    }

    const today: string = dayKeyAtOffset(0);
    const fresh: DiaryRow[] = [];
    for (let i = 0; i < days.length; i++) {
      const isToday: boolean = days[i] === today;
      // 没有材料、也没有版本的日子不占列表（今天除外：今天永远显示，哪怕是空的）。
      if (dayMissing[i] && !isToday) {
        continue;
      }
      let current: DiaryVersion | null = null;
      let versionError: UiError = EMPTY_UI_ERROR;
      const pos: number = versionPositions[i];
      if (pos >= 0 && pos < versionResults.length) {
        if (versionResults[pos].ok) {
          current = versionResults[pos].data as DiaryVersion;
        } else {
          versionError = uiErrorOf(versionResults[pos]);
        }
      }
      fresh.push({
        dayKey: days[i],
        day: dayMissing[i] ? null : dayValues[i],
        current: current,
        versionError: versionError,
        isToday: isToday,
      });
    }
    this.rows = fresh;
  }

  /** `YYYY-MM-DD` → 人话日期（今天 / 昨天 / 10月17日 周五）。 */
  private dayLabel(dayKey: string): string {
    const parts: string[] = dayKey.split('-');
    if (parts.length !== 3) {
      return dayKey;
    }
    const year: number = parseInt(parts[0], 10);
    const month: number = parseInt(parts[1], 10);
    const day: number = parseInt(parts[2], 10);
    if (dayKey === dayKeyAtOffset(0)) {
      return `今天 · ${month}月${day}日`;
    }
    if (dayKey === dayKeyAtOffset(-1)) {
      return `昨天 · ${month}月${day}日`;
    }
    const date: Date = new Date(year, month - 1, day);
    const weekdays: string[] = ['周日', '周一', '周二', '周三', '周四', '周五', '周六'];
    return `${month}月${day}日 ${weekdays[date.getDay()]}`;
  }

  private summaryOf(row: DiaryRow): string {
    if (row.current === null) {
      return row.isToday ? '今天还没有成篇日记' : '这一天还没有成篇日记';
    }
    const body: string = row.current.body.split('\n').join(' ').trim();
    return body.length > 0 ? body : '（这一版正文为空）';
  }

  private metaOf(row: DiaryRow): string {
    if (row.day === null) {
      return '这一天没有材料，也没有版本';
    }
    const counts = row.day.material_counts;
    let text: string = `材料 ${counts.captures} 条 · 已处理 ${counts.processed}`;
    if (counts.pending > 0) {
      text += ` · 待处理 ${counts.pending}`;
    }
    if (counts.failed > 0) {
      text += ` · 未完成 ${counts.failed}`;
    }
    if (row.day.candidate_version_ids.length > 0) {
      text += ` · ${row.day.candidate_version_ids.length} 个候选版本`;
    }
    return text;
  }

  @Builder
  diaryCard(row: DiaryRow) {
    Column({ space: 8 }) {
      Row({ space: 8 }) {
        Text(this.dayLabel(row.dayKey))
          .fontSize(14)
          .fontWeight(FontWeight.Medium)
          .fontColor($r('sys.color.ohos_id_color_text_primary'))
        if (row.current !== null) {
          StatePill({
            text: diaryOriginLabel(row.current.origin),
            tone: diaryOriginTone(row.current.origin),
          })
        }
        Blank()
        if (row.day !== null && row.day.candidate_version_ids.length > 0) {
          StatePill({ text: `候选 ${row.day.candidate_version_ids.length}`, tone: 'active' })
        }
      }
      .width('100%')

      Text(this.summaryOf(row))
        .fontSize(16)
        .fontColor($r('sys.color.ohos_id_color_text_primary'))
        .maxLines(3)
        .textOverflow({ overflow: TextOverflow.Ellipsis })
        .width('100%')

      if (row.versionError.code.length > 0) {
        Text(`当前版本读取失败：${row.versionError.message}`)
          .fontSize(12)
          .fontColor($r('sys.color.ohos_id_color_alert'))
          .width('100%')
      }

      Text(this.metaOf(row))
        .fontSize(12)
        .fontColor($r('sys.color.ohos_id_color_text_tertiary'))
        .width('100%')
    }
    .width('100%')
    .padding(14)
    .backgroundColor($r('sys.color.ohos_id_color_card_bg'))
    .borderRadius(14)
    .alignItems(HorizontalAlign.Start)
    .onClick(() => {
      this.pathStack.pushPathByName('diaryDay', row.dayKey);
    })
  }

  build() {
    Column({ space: 10 }) {
      Row({ space: 8 }) {
        Column() {
          PageTitle({
            title: '日记',
            subtitle: '按天倒序读成篇正文；候选版本在详情里看、在当前版本之间切换。',
          })
        }
        .layoutWeight(1)
        .alignItems(HorizontalAlign.Start)
        Button('刷新')
          .fontSize(13)
          .height(34)
          .backgroundColor($r('sys.color.ohos_id_color_sub_background'))
          .fontColor($r('sys.color.ohos_id_color_text_primary'))
          .onClick(() => {
            this.reload();
          })
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

      Refresh({ refreshing: $$this.refreshing }) {
        List({ space: 10 }) {
          if (this.loading) {
            ListItem() {
              LoadingRow({ text: '正在读取日记…' })
            }
          } else if (this.rows.length === 0) {
            ListItem() {
              EmptyState({
                icon: '日',
                title: '今天还没有成篇日记',
                hint: '先在「记录」里留下材料；已有版本会按天列在这里。',
              })
            }
          } else {
            ForEach(this.rows, (row: DiaryRow) => {
              ListItem() {
                this.diaryCard(row)
              }
              // key 必须覆盖这一行会变的全部内容：当前版本 id、候选数、材料数、revision。
              // ArkUI 的 ForEach 在 key 不变时复用旧节点；只按 dayKey 时，新记录/新版本
              // 不会让「材料 N 条」「当前版本摘要」重新渲染（#150 真机抓到）。
            }, (row: DiaryRow) => `${row.dayKey}-${row.current === null ? 'none' : row.current.id}-` +
              `${row.day === null ? 0 : row.day.candidate_version_ids.length}-` +
              `${row.day === null ? 0 : row.day.material_counts.captures}-` +
              `${row.day === null ? 0 : row.day.revision}`)

            ListItem() {
              Column({ space: 6 }) {
                Text('列表日期来自最近 60 天与最近 100 条记录；核心还没有按天枚举接口' +
                  '（diary.listDays 未接），更早的、只剩版本的日子不会出现在这里。')
                  .fontSize(11)
                  .fontColor($r('sys.color.ohos_id_color_text_tertiary'))
                  .width('100%')
                Text('生成/整理入口（diary.generate）也还没接：本页只读已有版本，' +
                  '不显示按了没反应的按钮。')
                  .fontSize(11)
                  .fontColor($r('sys.color.ohos_id_color_text_tertiary'))
                  .width('100%')
              }
              .width('100%')
              .padding({ top: 6, bottom: 24 })
            }
          }
        }
        .width('100%')
        .height('100%')
        .padding({ left: 16, right: 16 })
      }
      .onRefreshing(() => {
        this.refresh();
      })
      .layoutWeight(1)
    }
    .width('100%')
    .height('100%')
    .backgroundColor($r('sys.color.ohos_id_color_background'))
  }
}
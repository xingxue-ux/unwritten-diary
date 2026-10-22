// 日记某一天（契约 §2.2 / §4.3）：读正文、看版本列表、切换当前版本。
//
// 数据全部来自 C ABI：
// - `diary.getDay`：当前/候选、材料计数、`revision`（切换当前版本要原样回传）；
// - `diary.listVersions`：版本列表（最新在前，游标不透明；这里取第一页 50 条）；
// - `diary.setCurrent`：切换当前版本。核心有这个方法，所以按钮是真的能按的；
//   切完重读 `diary.getDay`，不自己猜新状态。
//
// 空态：「既没有材料也没有版本」的一天核心报 `not_found`，显示「这一天还没有成篇日记」，
// 不当读取失败。
//
// 能力缺失：`diary.saveEdit` / `diary.merge` / `diary.generate` 的 C ABI 都已就绪，但这一版
// 界面没有做编辑/合并/生成交互。这些都如实写在页脚，不摆假按钮。

import { common } from '@kit.AbilityKit';
import {
  DiaryDay,
  DiaryDayParams,
  DiaryVersion,
  DiaryVersionPage,
  DiaryVersionParams,
  ListDiaryVersionsParams,
  SetDiaryCurrentParams,
} from '../core/Models';
import { CoreGateway, CoreRequest, CoreResult, uiErrorOf } from '../ui/CoreGateway';
import { EMPTY_UI_ERROR, UiError } from '../ui/Errors';
import { AppState } from '../ui/AppState';
import {
  coverageLabel,
  databasePath,
  diaryOriginLabel,
  diaryOriginTone,
  formatDateTime,
  toast,
} from '../ui/AppRuntime';
import { EmptyState, ErrorBanner, InfoRow, LoadingRow, PageTitle, SectionTitle, StatePill } from '../ui/Common';

@Component
export struct DiaryDayPage {
  pathStack: NavPathStack = new NavPathStack();
  dayKey: string = '';
  @State day: DiaryDay | null = null;
  @State versions: DiaryVersion[] = [];
  @State current: DiaryVersion | null = null;
  @State loading: boolean = true;
  @State refreshing: boolean = false;
  @State error: UiError = EMPTY_UI_ERROR;
  @State actionError: UiError = EMPTY_UI_ERROR;
  @State switching: boolean = false;
  @State selectedVersionId: string = '';

  private dbPath: string = '';

  aboutToAppear(): void {
    const context = getContext(this) as common.UIAbilityContext;
    this.dbPath = databasePath(context);
    this.load();
  }

  private async load(): Promise<void> {
    this.loading = true;
    this.error = EMPTY_UI_ERROR;
    this.actionError = EMPTY_UI_ERROR;
    const dayParams: DiaryDayParams = { day_key: this.dayKey };
    const listParams: ListDiaryVersionsParams = { day_key: this.dayKey, limit: 50 };
    const requests: CoreRequest[] = [
      { method: 'diary.getDay', params: dayParams },
      { method: 'diary.listVersions', params: listParams },
    ];
    const results: CoreResult[] = await CoreGateway.callMany(this.dbPath, requests);
    this.loading = false;
    this.refreshing = false;

    if (results.length > 0) {
      if (results[0].ok) {
        this.day = results[0].data as DiaryDay;
      } else {
        const failure: UiError = uiErrorOf(results[0]);
        if (failure.code === 'not_found') {
          // 空态：这一天既没有材料也没有版本。不是错误。
          this.day = null;
        } else {
          this.error = failure;
          this.day = null;
        }
      }
    }

    if (results.length > 1) {
      if (results[1].ok) {
        const page: DiaryVersionPage = results[1].data as DiaryVersionPage;
        this.versions = page.versions;
      } else {
        this.versions = [];
        if (this.error.code.length === 0) {
          this.error = uiErrorOf(results[1]);
        }
      }
    }

    this.current = null;
    if (this.day !== null && this.day.current_version_id !== null) {
      for (let i = 0; i < this.versions.length; i++) {
        if (this.versions[i].id === this.day.current_version_id) {
          this.current = this.versions[i];
          break;
        }
      }
      // 当前版本不在第一页（版本很多）时单独取一次，别把「没取到」显示成「没有正文」。
      if (this.current === null && this.day.current_version_id !== null) {
        const params: DiaryVersionParams = { version_id: this.day.current_version_id };
        const result = await CoreGateway.call(this.dbPath, 'diary.getVersion', params);
        if (result.ok) {
          this.current = result.data as DiaryVersion;
        } else {
          this.error = uiErrorOf(result);
        }
      }
    }
  }

  private async refresh(): Promise<void> {
    this.refreshing = true;
    await this.load();
  }

  private isCurrent(version: DiaryVersion): boolean {
    return this.day !== null && this.day.current_version_id === version.id;
  }

  private async setCurrent(version: DiaryVersion): Promise<void> {
    if (this.day === null || this.switching) {
      return;
    }
    this.switching = true;
    this.actionError = EMPTY_UI_ERROR;
    const params: SetDiaryCurrentParams = {
      day_key: this.day.day_key,
      version_id: version.id,
      expected_revision: this.day.revision,
    };
    const result = await CoreGateway.call(this.dbPath, 'diary.setCurrent', params);
    this.switching = false;
    if (!result.ok) {
      this.actionError = uiErrorOf(result);
      return;
    }
    this.selectedVersionId = '';
    toast('已切换当前版本');
    AppState.bumpDataVersion();
    await this.load();
  }

  private dayLabel(dayKey: string): string {
    const parts: string[] = dayKey.split('-');
    if (parts.length !== 3) {
      return dayKey;
    }
    const year: number = parseInt(parts[0], 10);
    const month: number = parseInt(parts[1], 10);
    const day: number = parseInt(parts[2], 10);
    const date: Date = new Date(year, month - 1, day);
    const weekdays: string[] = ['周日', '周一', '周二', '周三', '周四', '周五', '周六'];
    return `${year}年${month}月${day}日 ${weekdays[date.getDay()]}`;
  }

  private materialLine(): string {
    if (this.day === null) {
      return '—';
    }
    const counts = this.day.material_counts;
    let text: string = `${counts.captures} 条材料 · 已处理 ${counts.processed}`;
    if (counts.pending > 0) {
      text += ` · 待处理 ${counts.pending}`;
    }
    if (counts.failed > 0) {
      text += ` · 未完成 ${counts.failed}`;
    }
    return text;
  }

  @Builder
  versionRow(version: DiaryVersion) {
    Column({ space: 6 }) {
      Row({ space: 8 }) {
        Text(formatDateTime(version.created_at))
          .fontSize(13)
          .fontColor($r('sys.color.ohos_id_color_text_secondary'))
        StatePill({
          text: diaryOriginLabel(version.origin),
          tone: diaryOriginTone(version.origin),
        })
        if (this.isCurrent(version)) {
          StatePill({ text: '当前版本', tone: 'ok' })
        }
        Blank()
        if (!this.isCurrent(version)) {
          Button(this.switching ? '切换中…' : '设为当前')
            .fontSize(12)
            .height(30)
            .enabled(!this.switching)
            .backgroundColor($r('sys.color.ohos_id_color_sub_background'))
            .fontColor($r('sys.color.ohos_id_color_text_primary'))
            .onClick(() => {
              this.setCurrent(version);
            })
        }
      }
      .width('100%')

      Text(version.body.length > 0 ? version.body : '（这一版正文为空）')
        .fontSize(14)
        .fontColor($r('sys.color.ohos_id_color_text_primary'))
        .maxLines(this.selectedVersionId === version.id ? 100 : 2)
        .textOverflow({ overflow: TextOverflow.Ellipsis })
        .width('100%')

      Text(this.selectedVersionId === version.id ? '点一下收起' : '点一下看全文')
        .fontSize(11)
        .fontColor($r('sys.color.ohos_id_color_text_tertiary'))
        .width('100%')
    }
    .width('100%')
    .padding(12)
    .backgroundColor($r('sys.color.ohos_id_color_card_bg'))
    .borderRadius(12)
    .alignItems(HorizontalAlign.Start)
    .onClick(() => {
      this.selectedVersionId = this.selectedVersionId === version.id ? '' : version.id;
    })
  }

  build() {
    Column({ space: 10 }) {
      Row({ space: 8 }) {
        Column() {
          PageTitle({ title: this.dayLabel(this.dayKey), subtitle: '成篇正文、版本与当前版本' })
        }
        .layoutWeight(1)
        .alignItems(HorizontalAlign.Start)
        Button('刷新')
          .fontSize(13)
          .height(34)
          .backgroundColor($r('sys.color.ohos_id_color_sub_background'))
          .fontColor($r('sys.color.ohos_id_color_text_primary'))
          .onClick(() => {
            this.refresh();
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
              this.load();
            },
          })
        }
        .padding({ left: 16, right: 16 })
      }
      if (this.actionError.code.length > 0) {
        Column() {
          ErrorBanner({ error: this.actionError, showReload: false, onReload: () => {
          } })
        }
        .padding({ left: 16, right: 16 })
      }

      if (this.loading) {
        LoadingRow({ text: '正在读取这一天的日记…' })
      } else if (this.day === null) {
        EmptyState({
          icon: '日',
          title: '这一天还没有成篇日记',
          hint: '这一天既没有材料，也没有版本。先去「记录」留下点什么。',
        })
      } else {
        Scroll() {
          Column({ space: 10 }) {
            // ---------------------------------------------------------- 这一天的材料
            Column({ space: 4 }) {
              InfoRow({ label: '材料', value: this.materialLine() })
              InfoRow({ label: '覆盖', value: coverageLabel(this.day.coverage) })
              InfoRow({
                label: '版本数',
                value: `${this.versions.length} 个 · revision ${this.day.revision}`,
              })
            }
            .width('100%')
            .padding(14)
            .backgroundColor($r('sys.color.ohos_id_color_card_bg'))
            .borderRadius(14)
            .alignItems(HorizontalAlign.Start)

            // ---------------------------------------------------------- 当前正文
            if (this.current !== null) {
              Column({ space: 8 }) {
                SectionTitle({ text: '当前正文' })
                Row({ space: 8 }) {
                  StatePill({
                    text: diaryOriginLabel((this.current as DiaryVersion).origin),
                    tone: diaryOriginTone((this.current as DiaryVersion).origin),
                  })
                  Blank()
                }
                .width('100%')
                Text((this.current as DiaryVersion).body)
                  .fontSize(17)
                  .lineHeight(28)
                  .fontColor($r('sys.color.ohos_id_color_text_primary'))
                  .width('100%')
                Text(`更新于 ${formatDateTime((this.current as DiaryVersion).created_at)}`)
                  .fontSize(11)
                  .fontColor($r('sys.color.ohos_id_color_text_tertiary'))
                  .width('100%')
              }
              .width('100%')
              .padding(14)
              .backgroundColor($r('sys.color.ohos_id_color_card_bg'))
              .borderRadius(14)
              .alignItems(HorizontalAlign.Start)
            }

            // ---------------------------------------------------------- 版本
            if (this.versions.length === 0) {
              EmptyState({
                icon: '—',
                title: '这一天还没有成篇日记',
                hint: '有材料、但还没有成篇正文。生成接口（diary.generate）已就绪，生成按钮界面还没做，这里不能生成。',
              })
            } else {
              SectionTitle({ text: `版本（${this.versions.length}）` })
              if (this.day.current_version_id === null) {
                Text('这一天还没有指定当前版本；核心只给了候选版本。')
                  .fontSize(12)
                  .fontColor($r('sys.color.ohos_id_color_warning'))
                  .width('100%')
              }
              ForEach(this.versions, (version: DiaryVersion) => {
                this.versionRow(version)
                // key 里带「是不是当前版本」：切换当前版本后这一行要重建（徽标与按钮会变）。
              }, (version: DiaryVersion) => `${version.id}-${this.isCurrent(version) ? 'current' : 'other'}`)
            }

            Text('本页可以读正文、看版本、切换当前版本。编辑正文/合并版本（diary.saveEdit / ' +
              'diary.merge）的 C ABI 已经接了，界面还没做；生成（diary.generate）的 C ABI 也已就绪，' +
              '只差入口按钮。')
              .fontSize(11)
              .fontColor($r('sys.color.ohos_id_color_text_tertiary'))
              .width('100%')
              .padding({ bottom: 24 })
          }
          .width('100%')
          .padding({ left: 16, right: 16 })
        }
        .width('100%')
        .layoutWeight(1)
      }
    }
    .width('100%')
    .height('100%')
    .backgroundColor($r('sys.color.ohos_id_color_background'))
  }
}
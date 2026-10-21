// 跨页面的一点点应用状态。
//
// 只有一件事：`dataVersion`。录音保存、提交、重建索引、重试任务之后 +1，
// 今日/任务页 `@StorageLink` + `@Watch` 到它，回到页面时自动重读。
// 不用事件总线：当前没有持续事件通道，轮询/前台重读已经是仓库的既有口径。

export const KEY_DATA_VERSION: string = 'ohosDataVersion';

export class AppState {
  static init(): void {
    AppStorage.setOrCreate(KEY_DATA_VERSION, 0);
  }

  /** 标记「资料库有变化，请重读」。 */
  static bumpDataVersion(): void {
    const current: number | undefined = AppStorage.get<number>(KEY_DATA_VERSION);
    const next: number = (current === undefined ? 0 : current) + 1;
    AppStorage.setOrCreate(KEY_DATA_VERSION, next);
  }
}

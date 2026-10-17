import { AbilityConstant, UIAbility, Want } from '@kit.AbilityKit';
import { hilog } from '@kit.PerformanceAnalysisKit';
import { window } from '@kit.ArkUI';
import { fileIo as fs } from '@kit.CoreFileKit';

const DOMAIN = 0x0000;
// 自定义 tag：真机侧用 `hdc shell hilog | grep DiarySmoke` 即可看到本次冒烟的全部证据。
const TAG = 'DiarySmoke';
// 一行机器可读的标记，便于 grep / CI 断言。
const MARK = 'SMOKE_MARKER';

export default class EntryAbility extends UIAbility {
  private tCreate: number = 0;

  onCreate(want: Want, launchParam: AbilityConstant.LaunchParam): void {
    this.tCreate = Date.now();
    hilog.info(DOMAIN, TAG, '%{public}s onCreate', MARK);
  }

  onWindowStageCreate(windowStage: window.WindowStage): void {
    const ctx = this.context;

    // ── ① 沙箱路径（第 2 步「核心 .so 塞进 HAP 跑 SQLite」要用的路径策略）
    hilog.info(DOMAIN, TAG, '%{public}s filesDir=%{public}s', MARK, ctx.filesDir);
    hilog.info(DOMAIN, TAG, '%{public}s cacheDir=%{public}s', MARK, ctx.cacheDir);
    hilog.info(DOMAIN, TAG, '%{public}s tempDir=%{public}s', MARK, ctx.tempDir);
    hilog.info(DOMAIN, TAG, '%{public}s databaseDir=%{public}s', MARK, ctx.databaseDir);
    hilog.info(DOMAIN, TAG, '%{public}s bundleCodeDir=%{public}s', MARK, ctx.bundleCodeDir);
    // 注：第 2 步（.so 进 HAP）要用的是 bundleCodeDir + '/libs/arm64-v8a'（nativeLibraryPath）。
    // UIAbilityContext 上没有 nativeLibraryDir 属性（实测编译报错），这里不猜、留给第 2 步实测。

    // ── ② 沙箱可写性 + 写入耗时（证明应用进程有真实文件系统权限）
    try {
      const p = ctx.filesDir + '/smoke.txt';
      const payload = 'hello from hap smoke @ ' + Date.now();
      const t0 = Date.now();
      const f = fs.openSync(p, fs.OpenMode.READ_WRITE | fs.OpenMode.CREATE | fs.OpenMode.TRUNC);
      fs.writeSync(f.fd, payload);
      fs.closeSync(f);
      const writeMs = Date.now() - t0;
      const st = fs.statSync(p);
      const back = fs.readTextSync(p);
      hilog.info(DOMAIN, TAG,
        '%{public}s sandbox_write path=%{public}s bytes=%{public}d writeMs=%{public}d readback=%{public}s',
        MARK, p, st.size, writeMs, back);
    } catch (e) {
      hilog.error(DOMAIN, TAG, '%{public}s sandbox_write FAILED %{public}s', MARK, JSON.stringify(e));
    }

    const t0 = Date.now();
    windowStage.loadContent('pages/Index', (err) => {
      if (err.code) {
        hilog.error(DOMAIN, TAG, '%{public}s loadContent failed %{public}s', MARK, JSON.stringify(err));
        return;
      }
      hilog.info(DOMAIN, TAG, '%{public}s loadContent ok contentMs=%{public}d',
        MARK, Date.now() - t0);
      hilog.info(DOMAIN, TAG, '%{public}s abilityStartMs=%{public}d',
        MARK, Date.now() - this.tCreate);
    });
  }

  onDestroy(): void {
    hilog.info(DOMAIN, TAG, '%{public}s onDestroy', MARK);
  }
}
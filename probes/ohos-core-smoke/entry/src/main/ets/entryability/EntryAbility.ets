import { AbilityConstant, bundleManager, UIAbility, Want } from '@kit.AbilityKit';
import { hilog } from '@kit.PerformanceAnalysisKit';
import { window } from '@kit.ArkUI';
import { fileIo as fs } from '@kit.CoreFileKit';
import diaryNapi from 'libdiary_napi.so';

const DOMAIN = 0x0000;
// 自定义 tag：真机侧 `hdc shell hilog -x -T DiaryCore` 就能拿到本次探针的全部证据。
const TAG = 'DiaryCore';
// 一行机器可读的标记，便于 grep / CI 断言。
const MARK = 'CORE_MARKER';
// hilog 单行长度有限，长 JSON 分片打，前缀 CORE_JSON[label][i/n]，拼接即可还原完整结果。
const CHUNK = 900;

export default class EntryAbility extends UIAbility {
  private tCreate: number = 0;

  onCreate(want: Want, launchParam: AbilityConstant.LaunchParam): void {
    this.tCreate = Date.now();
    hilog.info(DOMAIN, TAG, '%{public}s onCreate', MARK);
  }

  onWindowStageCreate(windowStage: window.WindowStage): void {
    const ctx = this.context;

    // ── ① 沙箱路径（与第 1 步一致，这次多了原生库落点）
    hilog.info(DOMAIN, TAG, '%{public}s filesDir=%{public}s', MARK, ctx.filesDir);
    hilog.info(DOMAIN, TAG, '%{public}s databaseDir=%{public}s', MARK, ctx.databaseDir);
    hilog.info(DOMAIN, TAG, '%{public}s bundleCodeDir=%{public}s', MARK, ctx.bundleCodeDir);
    hilog.info(DOMAIN, TAG, '%{public}s cacheDir=%{public}s', MARK, ctx.cacheDir);

    // ── ② 原生库落点：第 1 步没测到的 nativeLibraryPath，这次从 ApplicationInfo 拿
    try {
      const flags = bundleManager.BundleFlag.GET_BUNDLE_INFO_WITH_APPLICATION
        | bundleManager.BundleFlag.GET_BUNDLE_INFO_WITH_HAP_MODULE;
      const info = bundleManager.getBundleInfoForSelfSync(flags);
      hilog.info(DOMAIN, TAG, '%{public}s app.nativeLibraryPath=%{public}s',
        MARK, info.appInfo.nativeLibraryPath);
      hilog.info(DOMAIN, TAG, '%{public}s app.codePath=%{public}s', MARK, info.appInfo.codePath);
      for (const h of info.hapModulesInfo) {
        hilog.info(DOMAIN, TAG, '%{public}s hapModule[%{public}s].nativeLibraryPath=%{public}s',
          MARK, h.name, h.nativeLibraryPath);
      }
    } catch (e) {
      hilog.error(DOMAIN, TAG, '%{public}s bundleInfo FAILED %{public}s', MARK, this.errText(e));
    }

    // ── ③ NAPI shim 自己被加载的目录（dladdr 结论，排障用）
    try {
      hilog.info(DOMAIN, TAG, '%{public}s shim.selfDir=%{public}s', MARK, diaryNapi.selfDir());
    } catch (e) {
      hilog.error(DOMAIN, TAG, '%{public}s shim.selfDir FAILED %{public}s', MARK, this.errText(e));
    }

    // ── ③b 从**可写沙箱**（el2）dlopen 一份原生库的副本：回答「将来能不能动态下发原生库」
    try {
      const j = diaryNapi.dlopenFromDir(ctx.filesDir);
      hilog.info(DOMAIN, TAG, '%{public}s DLOPEN_FROM_EL2 %{public}s', MARK, j);
    } catch (e) {
      hilog.error(DOMAIN, TAG, '%{public}s dlopenFromDir THREW %{public}s', MARK, this.errText(e));
    }

    // ── ③c ONNX Runtime：把 rawfile 里的 tiny_add.onnx 落到沙箱，再在沙箱里 dlopen ORT
    try {
      const raw = ctx.resourceManager.getRawFileContentSync('tiny_add.onnx');
      const modelPath = ctx.filesDir + '/tiny_add.onnx';
      const mf = fs.openSync(modelPath, fs.OpenMode.READ_WRITE | fs.OpenMode.CREATE | fs.OpenMode.TRUNC);
      fs.writeSync(mf.fd, raw.buffer as ArrayBuffer);
      fs.closeSync(mf);
      hilog.info(DOMAIN, TAG, '%{public}s model written path=%{public}s bytes=%{public}d',
        MARK, modelPath, raw.byteLength);
      const t0 = Date.now();
      const j = diaryNapi.ortProbe(ctx.filesDir);
      hilog.info(DOMAIN, TAG, '%{public}s ORT_PROBE wallMs=%{public}d bytes=%{public}d',
        MARK, Date.now() - t0, j.length);
      const n = Math.max(1, Math.ceil(j.length / CHUNK));
      for (let i = 0; i < n; i++) {
        hilog.info(DOMAIN, TAG, '%{public}s ORT_JSON[%{public}d/%{public}d] %{public}s',
          MARK, i + 1, n, j.substring(i * CHUNK, (i + 1) * CHUNK));
      }
      // ③d 同一个 .so，走产品真实路径（ort crate 的 load-dynamic）
      const jc = diaryNapi.ortCrateProbe(ctx.filesDir);
      hilog.info(DOMAIN, TAG, '%{public}s ORT_CRATE %{public}s', MARK, jc);
    } catch (e) {
      hilog.error(DOMAIN, TAG, '%{public}s ortProbe THREW %{public}s', MARK, this.errText(e));
    }

    // ── ④ 核心跑起来：filesDir 一轮、databaseDir 一轮
    //     （两轮都完整跑「裸 SQLite + diary_core 迁移 + 写入 + 读回」，用来比较两个目录）
    this.runOne('filesDir', ctx.filesDir + '/coredb');
    this.runOne('databaseDir', ctx.databaseDir + '/coredb');

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

  private runOne(label: string, dir: string): void {
    const t0 = Date.now();
    try {
      const json = diaryNapi.runSmoke(dir);
      const wallMs = Date.now() - t0;
      hilog.info(DOMAIN, TAG, '%{public}s run %{public}s dir=%{public}s wallMs=%{public}d bytes=%{public}d',
        MARK, label, dir, wallMs, json.length);
      // 分片打，保证 hilog 不截断
      const n = Math.max(1, Math.ceil(json.length / CHUNK));
      for (let i = 0; i < n; i++) {
        hilog.info(DOMAIN, TAG, '%{public}s CORE_JSON[%{public}s][%{public}d/%{public}d] %{public}s',
          MARK, label, i + 1, n, json.substring(i * CHUNK, (i + 1) * CHUNK));
      }
    } catch (e) {
      hilog.error(DOMAIN, TAG, '%{public}s run %{public}s THREW %{public}s',
        MARK, label, this.errText(e));
    }
  }

  onDestroy(): void {
    hilog.info(DOMAIN, TAG, '%{public}s onDestroy', MARK);
  }

  // JSON.stringify(Error) 在 ArkTS 里是 "{}"（错误对象没有可枚举属性），
  // 所以显式取 message/stack——排障时这几行就是全部线索。
  private errText(e: Object): string {
    const err = e as Error;
    return (err && err.message) ? (err.message + ' | ' + (err.stack ?? '')) : String(e);
  }
}
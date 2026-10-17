import { AbilityConstant, bundleManager, common, UIAbility, Want } from '@kit.AbilityKit';
import { hilog } from '@kit.PerformanceAnalysisKit';
import { window } from '@kit.ArkUI';
import { fileIo as fs } from '@kit.CoreFileKit';
import diaryBge from 'libdiary_bge.so';

const DOMAIN = 0x0000;
// 自定义 tag：真机侧 `hdc shell hilog -x -T DiaryBge` 就能拿到本次探针的全部证据。
const TAG = 'DiaryBge';
// 一行机器可读的标记，便于 grep / CI 断言。
const MARK = 'BGE_MARKER';
// hilog 单行长度有限，长 JSON 分片打，前缀 BGE_JSON[i/n]，拼接即可还原完整结果。
const CHUNK = 900;

// 随 HAP 打进 rawfile 的两个模型文件（构建时由 scripts/build-hap.sh 从缓存目录拷进来）。
const MODEL_RAWFILE = 'model_quantized.onnx';
const TOKENIZER_RAWFILE = 'tokenizer.json';

export default class EntryAbility extends UIAbility {
  private tCreate: number = 0;

  onCreate(want: Want, launchParam: AbilityConstant.LaunchParam): void {
    this.tCreate = Date.now();
    hilog.info(DOMAIN, TAG, '%{public}s onCreate', MARK);
  }

  onWindowStageCreate(windowStage: window.WindowStage): void {
    const ctx = this.context;

    // ── ① 沙箱路径 + 原生库落点（第 2 步已测到，这里复述一遍做上下文）
    hilog.info(DOMAIN, TAG, '%{public}s filesDir=%{public}s', MARK, ctx.filesDir);
    hilog.info(DOMAIN, TAG, '%{public}s bundleCodeDir=%{public}s', MARK, ctx.bundleCodeDir);
    try {
      const flags = bundleManager.BundleFlag.GET_BUNDLE_INFO_WITH_APPLICATION;
      const info = bundleManager.getBundleInfoForSelfSync(flags);
      hilog.info(DOMAIN, TAG, '%{public}s nativeLibraryPath=%{public}s',
        MARK, info.appInfo.nativeLibraryPath);
    } catch (e) {
      hilog.error(DOMAIN, TAG, '%{public}s bundleInfo FAILED %{public}s', MARK, this.errText(e));
    }
    try {
      hilog.info(DOMAIN, TAG, '%{public}s shim.selfDir=%{public}s', MARK, diaryBge.selfDir());
    } catch (e) {
      hilog.error(DOMAIN, TAG, '%{public}s shim.selfDir FAILED %{public}s', MARK, this.errText(e));
    }

    // ── ② 把 rawfile 里的真模型 / tokenizer 落到可写沙箱（filesDir），再交给核心加载。
    //     与第 2 步 tiny_add.onnx 完全相同的路径策略：**HAP → filesDir → ORT 读文件**。
    const modelDir = ctx.filesDir + '/bge-model';
    const copyOk = this.stageModel(ctx, modelDir);

    // ── ③ 真嵌入（产品路径：ort crate load-dynamic + embedding.rs 口径）
    if (copyOk) {
      const t0 = Date.now();
      try {
        const json = diaryBge.runBge(modelDir);
        const wallMs = Date.now() - t0;
        hilog.info(DOMAIN, TAG, '%{public}s runBge wallMs=%{public}d bytes=%{public}d',
          MARK, wallMs, json.length);
        const n = Math.max(1, Math.ceil(json.length / CHUNK));
        for (let i = 0; i < n; i++) {
          hilog.info(DOMAIN, TAG, '%{public}s BGE_JSON[%{public}d/%{public}d] %{public}s',
            MARK, i + 1, n, json.substring(i * CHUNK, (i + 1) * CHUNK));
        }
      } catch (e) {
        hilog.error(DOMAIN, TAG, '%{public}s runBge THREW %{public}s', MARK, this.errText(e));
      }
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

  /**
   * 把 rawfile 里的两个模型文件写到 filesDir/bge-model/ 下。
   *
   * 每次都完整写一遍（24 MB，失败也好排查）；写完后把「rawfile 字节数 vs 落盘字节数」
   * 打出来，落盘这一步才算有证据。
   */
  private stageModel(ctx: common.UIAbilityContext, modelDir: string): boolean {
    try {
      if (!fs.accessSync(modelDir)) {
        fs.mkdirSync(modelDir, true);
      }
    } catch (e) {
      hilog.error(DOMAIN, TAG, '%{public}s mkdir %{public}s FAILED %{public}s',
        MARK, modelDir, this.errText(e));
      return false;
    }

    const plan: string[][] = [
      [MODEL_RAWFILE, 'model_quantized.onnx'],
      [TOKENIZER_RAWFILE, 'tokenizer.json']
    ];
    for (const item of plan) {
      const rawName = item[0];
      const dst = modelDir + '/' + item[1];
      const t0 = Date.now();
      try {
        const raw = ctx.resourceManager.getRawFileContentSync(rawName);
        const buf = raw.buffer as ArrayBuffer;
        const mf = fs.openSync(dst, fs.OpenMode.READ_WRITE | fs.OpenMode.CREATE | fs.OpenMode.TRUNC);
        fs.writeSync(mf.fd, buf, { offset: 0, length: raw.byteLength });
        fs.closeSync(mf);
        const st = fs.statSync(dst);
        hilog.info(DOMAIN, TAG,
          '%{public}s staged %{public}s bytes=%{public}d onDisk=%{public}d writeMs=%{public}d',
          MARK, rawName, raw.byteLength, st.size, Date.now() - t0);
      } catch (e) {
        hilog.error(DOMAIN, TAG, '%{public}s stage %{public}s FAILED %{public}s',
          MARK, rawName, this.errText(e));
        return false;
      }
    }
    return true;
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
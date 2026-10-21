import { AbilityConstant, UIAbility, Want } from '@kit.AbilityKit';
import { hilog } from '@kit.PerformanceAnalysisKit';
import { window } from '@kit.ArkUI';
import { process } from '@kit.ArkTS';
import BuildProfile from 'BuildProfile';
import { DiaryCoreSmoke } from '../core/Smoke';
import { DeviceVerify } from '../verify/DeviceVerify';
import { RecordingVerify } from '../recording/RecordingVerify';
import { TranscriptionVerify } from '../recording/TranscriptionVerify';
import { HapBothVerify } from '../verify/HapBothVerify';

const DOMAIN: number = 0x0000;
const TAG: string = 'DiaryCore';
const MARK: string = 'DIARY_SMOKE';

/**
 * 构建期旗标：探针只在 debug 构建里跑。
 *
 * ArkTS 不支持条件 import，所以 release 包里**仍有探针代码**（`BuildProfile.DEBUG`
 * 在编译期是常量，打包器可能内联/裁剪，但不保证）；运行时这里 early-return，保证
 * release 不会执行任何探针（不写库、不下载、不 dlopen 测试库）。诚实口径见 README。
 */
const DEBUG_BUILD: boolean = BuildProfile.DEBUG;

/**
 * 最小示例 Ability：应用启动时跑一遍「打开库 → coreInfo → 建草稿 → 保存 → 提交 → 抽取 →
 * 检索 → 排索引任务 → 跑任务 → 关闭」，结果全部打到 hilog（tag `DiaryCore`）。
 *
 * 真机验证入口（#122）：
 *   aa start -a EntryAbility -b <bundle> -m entry --ps verify_mode smoke
 *   aa start -a EntryAbility -b <bundle> -m entry --ps verify_mode bench
 *   aa start -a EntryAbility -b <bundle> -m entry --ps verify_mode errors
 *   aa start -a EntryAbility -b <bundle> -m entry --ps verify_mode concurrency
 *   aa start -a EntryAbility -b <bundle> -m entry --ps verify_mode persist
 *   aa start -a EntryAbility -b <bundle> -m entry --ps verify_mode diag
 *
 * 录音验证入口（#108 ⑦，实现在 `recording/`）：
 *   --ps verify_mode record        录 5 秒 → finalize → 试着跑转写任务
 *   --ps verify_mode record_bg     录 8 秒（配合 Home 键验证后台仍能录）
 *   --ps verify_mode record_hold   一直录（配合 aa force-stop 验证下次启动 recover）
 *   --ps verify_mode record_crash  录 6 秒后 process.abort()（真·进程崩溃）
 *   --ps verify_mode record_crash_tail  录 3.5 秒（第 3 段未封闭）后 process.abort()，
 *                                  恢复时应只有 3.0 秒已登记、尾巴如实报告
 *   --ps verify_mode recover       只查「上次没走完的录音」
 *   --ps verify_mode recover_finalize  查询并把恢复出来的会话最终化
 *
 * 转写验证入口（#108 转写未验项，实现在 `recording/TranscriptionVerify.ets`）：
 *   --ps verify_mode asr_setup     建沙箱模型目录（应用默认权限）+ 保存本机引擎 provider 配置
 *   --ps verify_mode asr_test      连接测试（真加载模型，不联网）
 *   --ps verify_mode asr_jobs      跑一轮 jobs.runDue（worker 里，量耗时/内存）
 *   --ps verify_mode asr_warm      先加载模型，再跑一轮 jobs.runDue（量热态耗时）
 *   --ps verify_mode asr_search --ps query=<词>   关键词检索
 *   --ps verify_mode asr_remove    删除本探针的 provider 配置
 *   --ps verify_mode asr_download  经 hdc rport 从宿主机下载模型/词表到沙箱
 *       可加 --ps base=http://127.0.0.1:8001 指向另一个宿主端口（默认 8000）；
 *       验证「Content-Length / sha256 不对时保留旧文件并报错」时用它避免挤占 8000。
 *   --ps verify_mode gen_remote --ps base=http://127.0.0.1:8001  下载 deepseek.json →
 *       保存/测试远端服务 → 跑 jobs.runDue 生成日记 → 打印正文（debug-only）
 *   --ps verify_mode gen_remove    删除 `deepseek_gen` 服务配置与沙箱里的密钥文件（清理用）
 *   --ps verify_mode asr_inject    把沙箱里的 zh.wav 按平台层录音约定注入成录音，
 *       跑转写并与已知答案比对（DIARY_ASR INJECT_TEXT / INJECT_RESULT）；
 *       绕开「扬声器→麦克风」的系统 AEC，量的是引擎准确率，不是回声消除效果。
 *
 * 「配置 + 两个 ORT 共存」入口（#108 收尾片，实现在 `verify/HapBothVerify.ets`）：
 *   --ps verify_mode hap_both --ps query=<语义查询>   下 core-config.json 与 bge 模型
 *       → 语义索引/语义检索 → 录音转写 → 再重建语义索引并检索转写文本（同一次启动）
 *
 * 不带参数时进入**产品界面**（`pages/Index` 的 Tabs），不跑任何探针。
 * 探针仍然只在显式给 `--ps verify_mode <mode>` 时执行。
 */
export default class EntryAbility extends UIAbility {
  private mode: string = '';
  /** `asr_search` 的检索词；其它模式忽略。 */
  private query: string = '';
  /**
   * `asr_download` / `asr_record_play` / `asr_inject` 的模型服务基址。
   *
   * 默认走 `hdc rport` 的 8000；验证下载完整性时可以指到另一个端口（宿主机起
   * 不同的 HTTP 服务，故意返回错的 Content-Length / sha256），不影响默认路径。
   */
  private baseUrl: string = 'http://127.0.0.1:8000';
  /** `hap_both` 的加载顺序：空 = 语义先跑；`asr_first` = 先转写再语义。 */
  private order: string = '';

  onCreate(want: Want, launchParam: AbilityConstant.LaunchParam): void {
    const params = want.parameters;
    if (params !== undefined && params !== null) {
      const value = params['verify_mode'];
      if (typeof value === 'string') {
        this.mode = value;
      }
      const query = params['query'];
      if (typeof query === 'string') {
        this.query = query;
      }
      const base = params['base'];
      if (typeof base === 'string' && base.length > 0) {
        this.baseUrl = base;
      }
      const order = params['order'];
      if (typeof order === 'string') {
        this.order = order;
      }
    }
    hilog.info(DOMAIN, TAG, '%{public}s onCreate mode=%{public}s pid=%{public}d',
      MARK, this.mode, process.pid);
  }

  onWindowStageCreate(windowStage: window.WindowStage): void {
    const filesDir = this.context.filesDir;
    if (!DEBUG_BUILD) {
      // release：所有探针（smoke / record* / asr_* / hap_both / verify）一律不跑。
      // 代码还在包里（ArkTS 无条件 import），但这条路径不可达；不写库、不联网。
      hilog.warn(DOMAIN, TAG,
        '%{public}s PROBE_DISABLED mode=%{public}s buildMode=%{public}s reason=release（探针代码在包里但不可达）',
        MARK, this.mode, BuildProfile.BUILD_MODE_NAME);
    } else if (this.mode.length === 0) {
      // 产品路径：没有 verify_mode ⇒ 只加载界面，不跑 Smoke/录音/转写探针。
      hilog.info(DOMAIN, TAG,
        '%{public}s PRODUCT_UI mode=(none) buildMode=%{public}s（未跑探针）',
        MARK, BuildProfile.BUILD_MODE_NAME);
    } else if (this.mode === 'hap_both') {
      HapBothVerify.run(this.mode, this.context, filesDir, this.query, this.order).catch((err: Object) => {
        hilog.error(DOMAIN, TAG, '%{public}s HAP_BOTH failed %{public}s',
          MARK, JSON.stringify(err));
      });
    } else if (this.mode.startsWith('asr_') || this.mode === 'gen_remote' || this.mode === 'gen_remove') {
      TranscriptionVerify.run(this.mode, this.context, filesDir, this.query, this.baseUrl).catch((err: Object) => {
        hilog.error(DOMAIN, TAG, '%{public}s ASR_VERIFY failed %{public}s',
          MARK, JSON.stringify(err));
      });
    } else if (this.mode === 'record' || this.mode === 'record_bg' ||
      this.mode === 'record_hold' || this.mode === 'record_crash' ||
      this.mode === 'record_crash_tail' ||
      this.mode === 'recover' || this.mode === 'recover_finalize') {
      RecordingVerify.run(this.mode, this.context, filesDir).catch((err: Object) => {
        hilog.error(DOMAIN, TAG, '%{public}s RECORDING_VERIFY failed %{public}s',
          MARK, JSON.stringify(err));
      });
    } else if (this.mode === 'smoke') {
      this.runSmokeWithMetrics(filesDir);
    } else {
      DeviceVerify.run(this.mode, filesDir);
    }

    // gen_* 探针**不加载产品界面**：`pages/Index.onPageShow` 会顺手跑一轮
    // `jobs.runDue`（budget 1500 ms），与探针自己那一轮抢同两条任务——第一次真机
    // 验证时它把 waiting_configuration 的任务抢先跑掉，探针的摘要成了 processed=0。
    // 探针不需要界面；不加载页面也就没有前台驱动，队列只由探针自己推进。
    if (DEBUG_BUILD && (this.mode === 'gen_remote' || this.mode === 'gen_remove')) {
      hilog.info(DOMAIN, TAG,
        '%{public}s PROBE_NO_UI mode=%{public}s（gen_* 探针不加载 pages/Index，避免前台 runDue 抢任务）',
        MARK, this.mode);
      return;
    }

    const t0 = Date.now();
    windowStage.loadContent('pages/Index', (err) => {
      if (err.code) {
        hilog.error(DOMAIN, TAG, '%{public}s loadContent failed %{public}s',
          MARK, JSON.stringify(err));
        return;
      }
      hilog.info(DOMAIN, TAG, '%{public}s loadContent ok contentMs=%{public}d',
        MARK, Date.now() - t0);
    });
  }

  /**
   * 同步跑 Smoke，量「主线程被占多久」。
   *
   * 方法：在 run() 之前排一个 setTimeout(0)，它最早也要等当前同步任务让出主线程才会执行；
   * 因此 `callback 触发时刻 - 开始时刻 - smokeMs` 就是事件循环被阻塞的额外延迟。
   * 60 Hz 下一帧 16.7 ms，据此估算掉帧数。ANR 由系统 AppFreeze 判定（hilog 里查）。
   */
  private runSmokeWithMetrics(filesDir: string): void {
    const dbPath = `${filesDir}/library.sqlite`;
    const t0 = Date.now();
    let smokeMs = 0;
    setTimeout(() => {
      const fired = Date.now();
      const delay = fired - t0 - smokeMs;
      const frames = Math.ceil(smokeMs / 16.7);
      hilog.info(DOMAIN, TAG, '%{public}s MAIN_THREAD smokeMs=%{public}d eventLoopDelayMs=%{public}d droppedFramesAt60Hz=%{public}d',
        MARK, smokeMs, delay, frames);
      DeviceVerify.reportDbStats(dbPath);
    }, 0);

    DiaryCoreSmoke.run(filesDir);
    smokeMs = Date.now() - t0;
    hilog.info(DOMAIN, TAG, '%{public}s MAIN_THREAD syncReturned smokeMs=%{public}d',
      MARK, smokeMs);
  }

  onDestroy(): void {
    hilog.info(DOMAIN, TAG, '%{public}s onDestroy', MARK);
    RecordingVerify.shutdown().catch((err: Object) => {
      hilog.warn(DOMAIN, TAG, '%{public}s SHUTDOWN failed %{public}s',
        MARK, JSON.stringify(err));
    });
  }
}

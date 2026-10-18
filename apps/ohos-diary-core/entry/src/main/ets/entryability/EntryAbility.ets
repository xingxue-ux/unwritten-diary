import { AbilityConstant, UIAbility, Want } from '@kit.AbilityKit';
import { hilog } from '@kit.PerformanceAnalysisKit';
import { window } from '@kit.ArkUI';
import { process } from '@kit.ArkTS';
import { DiaryCoreSmoke } from '../core/Smoke';
import { DeviceVerify } from '../verify/DeviceVerify';
import { RecordingVerify } from '../recording/RecordingVerify';
import { TranscriptionVerify } from '../recording/TranscriptionVerify';

const DOMAIN: number = 0x0000;
const TAG: string = 'DiaryCore';
const MARK: string = 'DIARY_SMOKE';

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
 *   --ps verify_mode asr_setup     建 0777 模型目录 + 保存本机引擎 provider 配置
 *   --ps verify_mode asr_test      连接测试（真加载模型，不联网）
 *   --ps verify_mode asr_jobs      跑一轮 jobs.runDue（worker 里，量耗时/内存）
 *   --ps verify_mode asr_warm      先加载模型，再跑一轮 jobs.runDue（量热态耗时）
 *   --ps verify_mode asr_search --ps query=<词>   关键词检索
 *   --ps verify_mode asr_remove    删除本探针的 provider 配置
 *   --ps verify_mode asr_download  经 hdc rport 从宿主机下载模型/词表到沙箱
 *
 * 不带参数时就是原来的同步 Smoke（用于量主线程阻塞）。
 */
export default class EntryAbility extends UIAbility {
  private mode: string = 'smoke';
  /** `asr_search` 的检索词；其它模式忽略。 */
  private query: string = '';

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
    }
    hilog.info(DOMAIN, TAG, '%{public}s onCreate mode=%{public}s pid=%{public}d',
      MARK, this.mode, process.pid);
  }

  onWindowStageCreate(windowStage: window.WindowStage): void {
    const filesDir = this.context.filesDir;
    if (this.mode.startsWith('asr_')) {
      TranscriptionVerify.run(this.mode, this.context, filesDir, this.query).catch((err: Object) => {
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

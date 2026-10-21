// `DiaryGenWorker` 的辅助函数。
//
// 为什么单独一个文件：ArkTS 的 `@Concurrent` 函数只允许使用 **import 进来的**或
// 本地变量/函数；同文件里未导出的顶层 helper 会被 es2abc 拒绝
// （`Concurrent function should only use import variable or local variable`）。
// 把 helper 放进独立模块、由 worker `import`，既满足这条限制，也避免把
// `@Concurrent` 函数写成几百行。

import { DiaryCore } from '../core/DiaryCore';
import { DiaryError } from '../core/DiaryError';
import {
  Job,
  JobPage,
  ProviderIdParams,
  Settings,
} from '../core/Models';

/** 只列 diary_generate，按创建时间升序（新造的那条在最后）。 */
export function diaryGenJobs(core: DiaryCore): Job[] {
  const page: JobPage = core.listJobs({
    states: ['succeeded', 'failed', 'waiting_configuration', 'retry_wait', 'queued', 'running'],
    limit: 100,
  });
  const jobs: Job[] = [];
  for (let i = 0; i < page.jobs.length; i++) {
    if (page.jobs[i].kind === 'diary_generate') {
      jobs.push(page.jobs[i]);
    }
  }
  jobs.sort((left: Job, right: Job): number => {
    if (left.created_at === right.created_at) {
      return left.id < right.id ? -1 : 1;
    }
    return left.created_at < right.created_at ? -1 : 1;
  });
  return jobs;
}

/**
 * 只改「自动整理开关 / 计划时刻」；时区与默认风格原样带上（不覆盖用户设置）。
 * `revision` 冲突时重新读一次再试，最多 3 次。
 */
export function writeOrganizeSettings(
  core: DiaryCore,
  original: Settings,
  enabled: boolean,
  time: string,
): Settings {
  let lastError: DiaryError | null = null;
  for (let attempt = 0; attempt < 3; attempt++) {
    const current: Settings = core.getSettings();
    try {
      return core.updateSettings({
        settings: {
          diary_time_zone: original.diary_time_zone,
          auto_organize_enabled: enabled,
          auto_organize_time: time,
          default_style_id: original.default_style_id,
        },
        expected_revision: current.revision,
      });
    } catch (e) {
      const err = e as DiaryError;
      lastError = err;
      if (err.code !== 'revision_conflict') {
        throw new Error(err.message);
      }
    }
  }
  throw new Error(lastError === null ? 'settings.update 失败' : lastError.message);
}

/** 恢复自动整理开关与计划时刻；成功返回 true。 */
export function restoreOrganizeSettings(core: DiaryCore, original: Settings): boolean {
  try {
    writeOrganizeSettings(core, original, original.auto_organize_enabled, original.auto_organize_time);
    return true;
  } catch (e) {
    return false;
  }
}

/** 让第二轮（生成）领得到任务：先把 provider 删掉，任务才会停在 waiting_configuration。 */
export function removeProviderQuietly(core: DiaryCore, providerId: string): void {
  const params: ProviderIdParams = { provider_id: providerId };
  try {
    core.rawCall('providers.remove', params);
  } catch (e) {
    const err = e as DiaryError;
    if (err.code !== 'not_found') {
      throw new Error(err.message);
    }
  }
}
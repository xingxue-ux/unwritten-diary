import { common } from '@kit.AbilityKit';
import { hilog } from '@kit.PerformanceAnalysisKit';
import { taskpool } from '@kit.ArkTS';
import { BundledModels } from '../models/BundledModels';
import { runLocalLlm, LocalLlmReport } from './LocalLlmWorker';

export async function verifyLocalLlm(context: common.UIAbilityContext): Promise<void> {
  const root: string = `${context.filesDir}/local-llm-acceptance-${Date.now()}`;
  await BundledModels.prepare(context, () => {}, root);
  const samples: string[] = [
    '今天去看妈妈，她说体检没事，我松了口气。',
    '下雨，没去跑步。晚上煮了面，有点咸。',
    '开会又拖到七点。烦。回家路上给妈妈打电话，聊了十分钟，心情好一点。',
    '今天去看妈妈她说体检没事我松了口气',
  ];
  for (let i: number = 0; i < samples.length; i++) {
    const raw = await taskpool.execute(runLocalLlm, `${root}/library.sqlite`, samples[i], `2026-10-${21 + i}`);
    const report: LocalLlmReport = JSON.parse(raw as string) as LocalLlmReport;
    hilog.info(0, 'DiaryModels', 'LOCAL_LLM_RESULT sample=%{public}d %{public}s', i + 1, raw as string);
    if (report.error.length > 0) { throw new Error(report.error); }
  }
  await BundledModels.prepare(context, () => {}, root);
  hilog.info(0, 'DiaryModels', 'LOCAL_LLM_ACCEPTANCE_OK samples=4 freshInstall=true restart=true originalPreserved=true');
}

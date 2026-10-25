import { common } from '@kit.AbilityKit';
import { hilog } from '@kit.PerformanceAnalysisKit';
import { taskpool } from '@kit.ArkTS';
import { BundledModels } from '../models/BundledModels';
import { CoreGateway } from '../ui/CoreGateway';
import { LibraryInfo, ProviderConfig, SearchSnapshot } from '../core/Models';
import { runSemanticPhase } from '../recording/HapBothWorker';
import { HapPhaseReport } from '../recording/HapBothOps';

/** Debug-only fresh-install acceptance in an isolated directory, never the user's library. */
export async function verifyBundledModels(context: common.UIAbilityContext): Promise<void> {
  const root: string = `${context.filesDir}/bundled-models-acceptance-${Date.now()}`;
  await BundledModels.prepare(context, () => {}, root);
  const db: string = `${root}/library.sqlite`;
  // An empty library has no vector generation and does not run the embedder.
  // Reuse the existing two fictional fixtures in this isolated library, build
  // their index, and require a real semantic hit from the bundled BGE model.
  const raw: Object = await taskpool.execute(runSemanticPhase, db, '母亲体检的结果怎么样', `bundled-${Date.now()}`);
  const phase: HapPhaseReport = JSON.parse(raw as string) as HapPhaseReport;
  if (phase.error.length > 0) { throw new Error(`随包语义模型推理失败：${phase.error}`); }
  const snapshot: SearchSnapshot = JSON.parse(phase.search_semantic) as SearchSnapshot;
  if (!snapshot.results.some((hit) => hit.matched_by.includes('semantic'))) {
    throw new Error('随包语义模型没有返回语义命中');
  }
  const info = await CoreGateway.call(db, 'core.info', null);
  const providers = await CoreGateway.call(db, 'providers.list', null);
  if (!info.ok || !providers.ok) { throw new Error('无法读取离线模型验收结果'); }
  const config = (info.data as LibraryInfo).runtime_config;
  if (config === undefined || config.semantic.state !== 'ready') {
    throw new Error('随包语义模型未加载成功');
  }
  let asrOk: boolean = false;
  for (const provider of providers.data as ProviderConfig[]) {
    if (provider.id === 'builtin_local_asr' && provider.test_status === 'ok') { asrOk = true; }
  }
  if (!asrOk) { throw new Error('随包转写模型未测试通过'); }
  await BundledModels.prepare(context, () => {}, root);
  hilog.info(0, 'DiaryModels', 'BUNDLED_MODELS_ACCEPTANCE_OK freshInstall=true asrInference=true semanticInference=true restart=true isolated=true');
}

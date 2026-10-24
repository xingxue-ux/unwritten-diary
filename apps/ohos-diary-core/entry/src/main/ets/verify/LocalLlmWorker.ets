import { hidebug } from '@kit.PerformanceAnalysisKit';
import { DiaryCore } from '../core/DiaryCore';
import { describe } from '../recording/HapBothOps';

export interface LocalLlmReport {
  body: string;
  model: string;
  provider: string;
  elapsedMs: number;
  pssKb: number;
  jobState: string;
  originalPreserved: boolean;
  keptOriginal: boolean;
  error: string;
}

/** Fictional inputs, isolated library, same queue and generation code as the UI. */
@Concurrent
export function runLocalLlm(dbPath: string, sample: string, dayKey: string): string {
  const report: LocalLlmReport = { body: '', model: '', provider: '', elapsedMs: 0,
    pssKb: 0, jobState: '', originalPreserved: false, keptOriginal: false, error: '' };
  let core: DiaryCore | null = null;
  const started: number = Date.now();
  try {
    core = DiaryCore.open(dbPath);
    const draft = core.createDraft({ occurred_at: `${dayKey}T12:00:00+08:00`,
      time_zone: 'Asia/Shanghai', utc_offset_minutes: 480, operation_id: `${dayKey}-create` });
    const saved = core.saveDraft({ capture_id: draft.id, text: sample,
      expected_revision: draft.revision, operation_id: `${dayKey}-save` });
    core.commit({ capture_id: draft.id, expected_revision: saved.revision, operation_id: `${dayKey}-commit` });
    const ticket = core.generateDiary({ day_key: dayKey, reason: 'manual', operation_id: `${dayKey}-generate` });
    core.runDueJobs({ budget_ms: 180000, trigger: 'local_llm_acceptance' });
    const job = core.getJob({ job_id: ticket.job_id });
    report.jobState = job.state;
    if (job.state !== 'succeeded') { throw new Error(`生成任务未成功：${job.state}/${job.error_code}`); }
    const day = core.getDiaryDay({ day_key: dayKey });
    if (day.current_version_id === null) { throw new Error('没有生成日记正文'); }
    const version = core.getDiaryVersion({ version_id: day.current_version_id });
    report.body = version.body;
    if (version.generation_info !== null) {
      report.model = version.generation_info.model === null ? '' : version.generation_info.model;
      report.provider = version.generation_info.provider_id === null ? '' : version.generation_info.provider_id;
      const options = version.generation_info.generation_options as Record<string, string>;
      report.keptOriginal = options['keptOriginal'] === 'true';
    }
    report.originalPreserved = core.getCapture({ capture_id: draft.id }).draft_text === sample;
    if (report.provider !== 'builtin_local_llm' || !report.originalPreserved || report.body.length === 0) {
      throw new Error('本机生成或原文保留核验失败');
    }
  } catch (error) { report.error = describe(error as Object); }
  finally { if (core !== null) { core.close(); } }
  report.elapsedMs = Date.now() - started;
  report.pssKb = Number(hidebug.getPss());
  return JSON.stringify(report);
}

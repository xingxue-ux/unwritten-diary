// 「配置 + 两个 ORT」探针的共享类型与小工具。
//
// 为什么单独一个模块：ArkTS 的 `@Concurrent` 函数**只能用 import 进来的变量和局部
// 变量**（es2abc 的硬约束）。把 helper 放在同一个文件里会报
// `Concurrent function should only use import variable or local variable`。
// 所以报告类型、建记录/检索/取音频来源这些都在这里，worker 只 import。

import { hidebug } from '@kit.PerformanceAnalysisKit';
import { DiaryCore } from '../core/DiaryCore';
import { SearchHit, SearchSnapshot } from '../core/Models';

/** 主线程打印用的报告；字段全是字符串（TaskPool 只回传可序列化的值）。 */
export interface HapPhaseReport {
  phase: string;
  open_ms: number;
  info_before: string;
  entries: string[];
  keyword_job: string;
  semantic_job: string;
  rebuild_ms: number;
  run_ms: number;
  summary: string;
  status: string;
  search_semantic: string;
  search_keyword: string;
  /** 阶段一那条语义查询在转写之后再跑一次（证明 1.16.3 载入后 1.29.1 仍正常）。 */
  search_diary_after_asr: string;
  extract_audio: string;
  info_after: string;
  pss_before_kb: number;
  pss_after_kb: number;
  private_before_kb: number;
  private_after_kb: number;
  /** 排障用：asset_id 是否传到 worker、命中里有没有它。 */
  debug: string;
  error: string;
}

/** 两条待检索的日记：一条是语义目标，一条是干扰项（两篇同一天，会进同一块/相邻块）。 */
export const DIARY_TARGET: string =
  '今天下午陪妈妈去医院复查，医生说骨头愈合得不错，下周再去拍一次片子。';
export const DIARY_DISTRACTOR: string =
  '晚上把厨房的水龙头修好了，换了一个新的阀芯，从此不再滴水。';

export function emptyReport(phase: string): HapPhaseReport {
  return {
    phase: phase,
    open_ms: 0,
    info_before: '',
    entries: [],
    keyword_job: '',
    semantic_job: '',
    rebuild_ms: 0,
    run_ms: 0,
    summary: '',
    status: '',
    search_semantic: '',
    search_keyword: '',
    search_diary_after_asr: '',
    extract_audio: '',
    info_after: '',
    pss_before_kb: Number(hidebug.getPss()),
    pss_after_kb: 0,
    private_before_kb: Number(hidebug.getPrivateDirty()),
    private_after_kb: 0,
    debug: '',
    error: '',
  };
}

export function describe(error: Object): string {
  const err = error as Error;
  if (err !== undefined && err !== null && err.message !== undefined) {
    return err.message;
  }
  return JSON.stringify(error);
}

/** 建草稿 → 保存 → 提交，返回 capture id。operationId 带后缀，重复跑不会撞幂等键。 */
export function createCommitted(
  core: DiaryCore,
  text: string,
  suffix: string,
  index: number,
): string {
  const op = `op-hap-${suffix}-${index}`;
  const draft = core.createDraft({
    // **刻意写在前一天**：沙箱里已经有上一轮验证留下的十几条「同一天」音频篇
    // （很多转写结果是「.」）。切块按 day_key 打包，同一天的短篇会被塞进同一块，
    // 块的相似度对块内每一篇共享——不隔离的话，语义命中会先铺满那些「.」篇，
    // 真正的目标篇被挤出首页，看起来像「语义没命中」。
    occurred_at: '2026-10-17T10:00:00+08:00',
    time_zone: 'Asia/Shanghai',
    utc_offset_minutes: 480,
    operation_id: `${op}-draft`,
  });
  const saved = core.saveDraft({
    capture_id: draft.id,
    text: text,
    expected_revision: draft.revision,
    operation_id: `${op}-save`,
  });
  core.commit({
    capture_id: draft.id,
    expected_revision: saved.revision,
    operation_id: `${op}-commit`,
  });
  return draft.id;
}

export function search(core: DiaryCore, query: string, mode: string): SearchSnapshot {
  return core.search({
    // 50 而不是 20：同一天的旧数据会被打进同一个块，块内每一篇都是一条命中；
    // 要看的是「目标篇有没有在结果里」，不是只取首页。
    request: { query: query, mode: mode, page_size: 50 },
    query_revision: 1,
  });
}

/**
 * 从命中里挑**这一次新录的**音频来源：按 locator.asset_id 精确匹配。
 *
 * 不能「取第一篇音频」：沙箱里还有上一轮留下的音频来源，语义分是块级共享的，
 * 排序不保证新的那条在最前。finalize 返回的 asset_id 是唯一可靠的锚点。
 */
export function audioSourceForAsset(snapshot: SearchSnapshot, assetId: string): string {
  if (assetId.length === 0) {
    return '';
  }
  for (let i = 0; i < snapshot.results.length; i++) {
    const hit: SearchHit = snapshot.results[i];
    if (hit.source_kind === 'audio' && hit.source_id !== null &&
      hit.locator !== null && hit.locator.asset_id === assetId) {
      return hit.source_id;
    }
  }
  return '';
}

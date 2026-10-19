// 「配置 + 两个 ORT 共存」真机探针的 TaskPool worker（#108 收尾片）。
//
// 为什么在 worker 里跑：`jobs.runDue` 会**同步**加载模型（两个 ORT、bge 与 SenseVoice），
// 放主线程就是几秒的阻塞，能触发 AppFreeze。现有的 `AsrWorker` 已经证明 C ABI 句柄可以
// 跨线程调用（`Arc<Mutex<Core>>`），worker 重新 `open` 同一个库文件拿到的是**同一个
// Core**（`diary_capi` 的进程内登记表按路径共享）。
//
// 两个阶段分开导出，是为了让主线程能在中间插入「录音 + 转写」：
//
//   1. `runSemanticPhase`：建日记 → 排关键词/语义索引 → 跑任务 → **语义检索**。
//      这一步先加载自编 ORT 1.29.1（`libonnxruntime-diary-1.29.1.so`）；
//   2. 主线程：`recordWithPlayback`（设备扬声器回放 zh.wav）+ 录音，转写任务加载
//      sherpa 与它自带的 ORT 1.16.3（`libonnxruntime.so`）——**先语义后转写**；
//   3. `runSemanticAfterAsr`：再建一次语义索引（把转写文本也切块、用 1.29.1 算向量），
//      再跑一次语义检索 → 证明 1.16.3 加载进来之后 1.29.1 仍然正常（两个方向都不踩）。
//
// ⚠️ ArkTS 的 `@Concurrent` 函数**只能用 import 进来的变量与局部变量**（es2abc 的
// 硬约束），所以类型、常量与 helper 全在 `HapBothOps` 里，本文件只保留两个入口。
//
// 返回一条 JSON，由主线程按 900 字符分片打进 hilog；worker 自己不写日志
// （两个线程的日志会交织，拼不回去）。

import { hidebug } from '@kit.PerformanceAnalysisKit';
import { DiaryCore } from '../core/DiaryCore';
import { ExtractedContent, SearchSnapshot } from '../core/Models';
import {
  audioSourceForAsset,
  createCommitted,
  describe,
  DIARY_DISTRACTOR,
  DIARY_TARGET,
  emptyReport,
  search,
} from './HapBothOps';

/**
 * 只跑一轮任务（不建索引、不检索）：给「**先转写**再语义」的顺序用。
 *
 * 这一轮会把 sherpa 与它自带的 ORT 1.16.3 先加载进进程；之后阶段一再加载我们
 * 自编的 1.29.1。两个方向都验，才说得清「互不踩」不是靠加载顺序碰巧成立的。
 */
@Concurrent
export function runJobsOnly(dbPath: string): string {
  const report = emptyReport('jobs_only');
  let core: DiaryCore | null = null;
  try {
    const openStart = Date.now();
    core = DiaryCore.open(dbPath);
    report.open_ms = Date.now() - openStart;
    report.info_before = JSON.stringify(core.coreInfo());
    const runStart = Date.now();
    report.summary = JSON.stringify(core.runDueJobs({ budget_ms: 180000, trigger: 'hap_both_asr_first' }));
    report.run_ms = Date.now() - runStart;
    report.status = JSON.stringify(core.jobsStatus());
    report.info_after = JSON.stringify(core.coreInfo());
  } catch (e) {
    report.error = describe(e as Object);
  } finally {
    if (core !== null) {
      try {
        core.close();
      } catch (e) {
        if (report.error.length === 0) {
          report.error = `close: ${describe(e as Object)}`;
        }
      }
    }
  }
  report.pss_after_kb = Number(hidebug.getPss());
  report.private_after_kb = Number(hidebug.getPrivateDirty());
  return JSON.stringify(report);
}

/**
 * 阶段一：语义先跑（加载自编 ORT 1.29.1），不碰转写。
 *
 * `query` 与 `DIARY_TARGET` **没有共同词**（「母亲体检的结果」vs「妈妈…复查…医生」）：
 * 能命中就只可能是语义那一路，而不是关键词碰巧覆盖。
 */
@Concurrent
export function runSemanticPhase(dbPath: string, query: string, suffix: string): string {
  const report = emptyReport('semantic_first');
  let core: DiaryCore | null = null;
  try {
    const openStart = Date.now();
    core = DiaryCore.open(dbPath);
    report.open_ms = Date.now() - openStart;
    report.info_before = JSON.stringify(core.coreInfo());

    report.entries.push(createCommitted(core, DIARY_TARGET, suffix, 0));
    report.entries.push(createCommitted(core, DIARY_DISTRACTOR, suffix, 1));

    const rebuildStart = Date.now();
    const keywordJob = core.rebuildIndex({
      scope: 'all',
      reason: 'hap_both',
      operation_id: `${suffix}-keyword`,
    });
    const semanticJob = core.rebuildSemanticIndex({
      reason: 'hap_both',
      operation_id: `${suffix}-semantic`,
    });
    report.keyword_job = keywordJob.id;
    report.semantic_job = semanticJob.id;
    report.rebuild_ms = Date.now() - rebuildStart;

    const runStart = Date.now();
    report.summary = JSON.stringify(core.runDueJobs({ budget_ms: 180000, trigger: 'hap_both' }));
    report.run_ms = Date.now() - runStart;
    report.status = JSON.stringify(core.jobsStatus());

    report.search_semantic = JSON.stringify(search(core, query, 'semantic'));
    report.info_after = JSON.stringify(core.coreInfo());
  } catch (e) {
    report.error = describe(e as Object);
  } finally {
    if (core !== null) {
      try {
        core.close();
      } catch (e) {
        if (report.error.length === 0) {
          report.error = `close: ${describe(e as Object)}`;
        }
      }
    }
  }
  report.pss_after_kb = Number(hidebug.getPss());
  report.private_after_kb = Number(hidebug.getPrivateDirty());
  return JSON.stringify(report);
}

/**
 * 阶段二：转写已经发生（sherpa + ORT 1.16.3 已在进程里）之后，再验证语义那一侧。
 *
 * - 再建一次索引：新出现的转写文本会被切块、用 1.29.1 算向量（复用旧向量）；
 * - 语义检索 `asrQuery`：命中音频来源，抽出**转写正文**；
 * - 再跑一次阶段一的 `diaryQuery`：证明 1.16.3 加载后 1.29.1 仍然正常；
 * - 关键词检索 `asrKeyword`：上一轮验过的旧路径，作为对照。
 */
@Concurrent
export function runSemanticAfterAsr(
  dbPath: string,
  assetId: string,
  diaryQuery: string,
  asrQuery: string,
  asrKeyword: string,
  suffix: string,
): string {
  const report = emptyReport('semantic_after_asr');
  let core: DiaryCore | null = null;
  try {
    const openStart = Date.now();
    core = DiaryCore.open(dbPath);
    report.open_ms = Date.now() - openStart;
    report.info_before = JSON.stringify(core.coreInfo());

    const rebuildStart = Date.now();
    const keywordJob = core.rebuildIndex({
      scope: 'all',
      reason: 'hap_both_after_asr',
      operation_id: `${suffix}-keyword2`,
    });
    const semanticJob = core.rebuildSemanticIndex({
      reason: 'hap_both_after_asr',
      operation_id: `${suffix}-semantic2`,
    });
    report.keyword_job = keywordJob.id;
    report.semantic_job = semanticJob.id;
    report.rebuild_ms = Date.now() - rebuildStart;

    const runStart = Date.now();
    report.summary = JSON.stringify(core.runDueJobs({ budget_ms: 180000, trigger: 'hap_both' }));
    report.run_ms = Date.now() - runStart;
    report.status = JSON.stringify(core.jobsStatus());

    // 转写文本的语义命中：查询用「开饭时间」，与常见识别结果（开饭/快饭/派饭…）
    // 在语义上最近。**按 asset_id 定位这一条**（不能取第一篇音频：沙箱里有旧数据）。
    const asrSnapshot = search(core, asrQuery, 'semantic');
    report.search_semantic = JSON.stringify(asrSnapshot);
    const sourceId = audioSourceForAsset(asrSnapshot, assetId);
    // 排障：把 asset_id 是否传到 worker、命中里有哪些音频 asset 打出去。
    // （TaskPool 的实参如果没按顺序到，这里会直接暴露成 `assetId=` 为空。）
    let audioCount = 0;
    let audioAssets = '';
    for (let i = 0; i < asrSnapshot.results.length && audioAssets.length < 600; i++) {
      const hit = asrSnapshot.results[i];
      if (hit.source_kind === 'audio') {
        audioCount++;
        if (hit.locator !== null) {
          audioAssets += `${hit.locator.asset_id};`;
        }
      }
    }
    report.debug = `assetId=${assetId} audioCount=${audioCount} sourceFound=${sourceId}|audioAssets=${audioAssets}`;
    if (sourceId.length > 0) {
      const extracted: ExtractedContent = core.extract({ source_ref: sourceId });
      report.extract_audio = JSON.stringify(extracted);
    }

    report.search_keyword = JSON.stringify(search(core, asrKeyword, 'keyword'));
    // 阶段一的那条查询再跑一次（1.16.3 已在进程里）。
    report.search_diary_after_asr = JSON.stringify(search(core, diaryQuery, 'semantic'));
    report.info_after = JSON.stringify(core.coreInfo());
  } catch (e) {
    report.error = describe(e as Object);
  } finally {
    if (core !== null) {
      try {
        core.close();
      } catch (e) {
        if (report.error.length === 0) {
          report.error = `close: ${describe(e as Object)}`;
        }
      }
    }
  }
  report.pss_after_kb = Number(hidebug.getPss());
  report.private_after_kb = Number(hidebug.getPrivateDirty());
  return JSON.stringify(report);
}

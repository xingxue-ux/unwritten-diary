//! 转写链（#91）的测试。
//!
//! **CI 里没有模型、没有原生引擎库**：所有测试都用 stub 引擎（与真引擎同一个
//! `LocalEngine` trait），走的是同一条产品路径（录音 → 最终化 → 任务驱动 →
//! 派生内容 → 检索）。真引擎那一条是 `#[ignore]` 的手动量测，见文件末尾。
//!
//! 覆盖的验收点（编号与 issue #91 的验收标准对应）：
//!
//! | 测什么 | 测试 |
//! |---|---|
//! | 转写文本进库、关键词能搜到、时间戳与字符区间对得上 | `transcription_lands_as_derived_content_and_is_searchable` |
//! | 进程被杀 → 续跑不重头、不重复插入 | `an_interrupted_run_resumes_without_redoing_windows` |
//! | 同片段重放只有一份文本 | `replaying_the_same_audio_produces_one_copy_of_the_text` |
//! | 引擎/模型缺失 → 等配置 + 可执行原因、没有空文本 | `a_missing_engine_waits_for_configuration_without_writing_anything` |
//! | 中途失败不把半截结果当完整文本 | `a_failed_run_leaves_partial_text_marked_partial` |
//! | 旧 `no_handler` 任务的历史包袱 | `legacy_no_handler_jobs_are_requeued_once_then_run` |
//! | 转写全程不联网 | `transcription_never_touches_the_network` |
//! | 接 #93 的质量集接缝 | `quality_seam_runs_through_the_local_engine` |

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use chrono::Utc;
use diary_core::model::{Coverage, ProcessingStatus};
use diary_core::provider::local_engine::{
    EngineConfig, EngineReadiness, LocalEngine, ModelKind, PcmOutcome, PcmRequest,
};
use diary_core::{
    Core, CreateDraftInput, JobPriority, JobState, NewJob, RecordingTicket, SearchFilters,
    SearchMode, SearchRequest, SegmentManifest, TranscriptEngine,
};
use sha2::{Digest, Sha256};

// ------------------------------------------------------------------ 测试脚手架

fn sha_of(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

fn new_core(dir: &std::path::Path) -> Core {
    // 内存库 + 文件系统根目录：传输层默认是「一调用就 panic」的替身，
    // 所以**转写全链跑通本身就证明了没有出网**。
    Core::open_in_memory_at(dir).unwrap()
}

/// 造一段合法的 16 位 PCM WAV（全零＝静音，测试不伪造人声）。
fn wav_of(duration_ms: i64, sample_rate: u32) -> Vec<u8> {
    let frames = (sample_rate as u64 * duration_ms as u64 / 1000).max(1);
    let data_bytes = (frames * 2) as u32;
    let mut wav = Vec::with_capacity(44 + data_bytes as usize);
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_bytes).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16_u32.to_le_bytes());
    wav.extend_from_slice(&1_u16.to_le_bytes()); // PCM
    wav.extend_from_slice(&1_u16.to_le_bytes()); // 单声道
    wav.extend_from_slice(&sample_rate.to_le_bytes());
    let byte_rate = sample_rate * 2;
    wav.extend_from_slice(&byte_rate.to_le_bytes());
    wav.extend_from_slice(&2_u16.to_le_bytes());
    wav.extend_from_slice(&16_u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_bytes.to_le_bytes());
    wav.resize(44 + data_bytes as usize, 0);
    wav
}

/// 录一段音（一个片段就是完整 WAV）并最终化，返回（资产 id、来源 id、记录 id）。
fn record_and_finalize(
    core: &mut Core,
    duration_ms: i64,
) -> (String, String, String) {
    let capture_id = core
        .create_draft(CreateDraftInput {
            occurred_at: None,
            time_zone: "Asia/Shanghai",
            utc_offset_minutes: 480,
            operation_id: "op-create",
        })
        .unwrap()
        .id;
    let ticket: RecordingTicket = core.prepare_recording(&capture_id, "op-rec").unwrap();
    let bytes = wav_of(duration_ms, 16_000);
    let name = "seg-0.wav";
    std::fs::write(
        std::path::Path::new(&ticket.staging_ticket).join(name),
        &bytes,
    )
    .unwrap();
    core.register_segment(
        &ticket.recording_id,
        0,
        SegmentManifest {
            segment_id: "seg0".to_owned(),
            relative_file_name: name.to_owned(),
            duration_ms,
            sha256: sha_of(&bytes),
            byte_size: bytes.len() as i64,
            closed_at: None,
        },
    )
    .unwrap();
    let result = core
        .finalize_recording(&ticket.recording_id, 0, "user")
        .unwrap();
    assert!(result.transcription_queued);
    let source_id = core.get_capture(&capture_id).unwrap().ordered_source_ids[0].clone();
    (result.asset_id, source_id, capture_id)
}

/// 一个可编程的 stub 引擎：与真引擎同一个 trait。
///
/// - `text_for` 决定第 n 次调用返回什么；
/// - `fail_on` 让第 n 次调用报错（模拟「进程被杀/引擎中途失败」）；
/// - `calls` 数每一次调用，用来钉住「恢复时不重头」。
struct StubEngine {
    calls: AtomicUsize,
    seen_samples: Mutex<Vec<usize>>,
    /// 第 n 次调用（从 0 数）返回的文本。
    words: Vec<String>,
    /// 第 n 次调用直接失败（retryable）。
    fail_on: Option<usize>,
    ready: bool,
    /// 「模型身份」：换掉它会触发重建。
    identity: String,
    /// 就绪检查是否**真的看配置里的模型目录**（默认 false：stub 只看 `ready`）。
    ///
    /// 用在「配置行指定的目录说了算」那条测试上：那里要能区分「目录在」与「目录不在」。
    honor_model_dir: bool,
    /// 最后一次调用时用的模型目录（钉住「闸门说用哪个、执行者就用哪个」）。
    seen_model_dir: Mutex<Option<std::path::PathBuf>>,
}

impl StubEngine {
    fn new(words: &[&str]) -> Arc<Self> {
        Arc::new(Self {
            calls: AtomicUsize::new(0),
            seen_samples: Mutex::new(Vec::new()),
            words: words.iter().map(|word| (*word).to_owned()).collect(),
            fail_on: None,
            ready: true,
            identity: "stub-model-v1".to_owned(),
            honor_model_dir: false,
            seen_model_dir: Mutex::new(None),
        })
    }

    fn with_failure_at(mut self: Arc<Self>, index: usize) -> Arc<Self> {
        Arc::get_mut(&mut self).unwrap().fail_on = Some(index);
        self
    }

    fn not_ready() -> Arc<Self> {
        let mut engine = Self {
            calls: AtomicUsize::new(0),
            seen_samples: Mutex::new(Vec::new()),
            words: vec!["不该被调用".to_owned()],
            fail_on: None,
            ready: false,
            identity: "stub-model-missing".to_owned(),
            honor_model_dir: false,
            seen_model_dir: Mutex::new(None),
        };
        engine.ready = false;
        Arc::new(engine)
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    fn with_model_dir_check(mut self: Arc<Self>) -> Arc<Self> {
        Arc::get_mut(&mut self).unwrap().honor_model_dir = true;
        self
    }

    fn seen_model_dir(&self) -> Option<std::path::PathBuf> {
        self.seen_model_dir.lock().unwrap().clone()
    }
}

impl LocalEngine for StubEngine {
    fn id(&self) -> &'static str {
        "stub_asr"
    }

    fn capabilities(&self) -> &'static [diary_core::ProviderCapability] {
        &[diary_core::ProviderCapability::TranscribeAudio]
    }

    fn readiness(&self, config: &EngineConfig) -> EngineReadiness {
        let files_ok = !self.honor_model_dir
            || (config.model_dir.join("model.int8.onnx").is_file()
                && config.model_dir.join("tokens.txt").is_file());
        if self.ready && files_ok {
            EngineReadiness {
                ready: true,
                code: String::new(),
                reason: format!("stub 引擎可用（{}）", self.identity),
                next_step: None,
            }
        } else {
            EngineReadiness {
                ready: false,
                code: "provider_not_configured".to_owned(),
                reason: format!(
                    "stub 引擎不可用：模型目录 {} 里没有模型文件",
                    config.model_dir.display()
                ),
                next_step: Some(format!(
                    "把模型目录（model.int8.onnx 与 tokens.txt）放到 {}，或设 DIARY_ASR_MODEL_DIR",
                    config.model_dir.display()
                )),
            }
        }
    }

    fn transcribe(&self, config: &EngineConfig, request: &PcmRequest) -> diary_core::Result<PcmOutcome> {
        *self.seen_model_dir.lock().unwrap() = Some(config.model_dir.clone());
        let index = self.calls.fetch_add(1, Ordering::SeqCst);
        self.seen_samples.lock().unwrap().push(request.samples.len());
        if self.fail_on == Some(index) {
            return Err(diary_core::CoreError::AssetMissing {
                asset_id: "stub".to_owned(),
                reason: format!("stub 在第 {index} 次调用上模拟了中断"),
            });
        }
        Ok(PcmOutcome {
            text: self
                .words
                .get(index)
                .cloned()
                .unwrap_or_else(|| format!("第{index}段")),
            model: Some(self.identity.clone()),
            language: Some("zh".to_owned()),
        })
    }

    fn probe(&self, _config: &EngineConfig) -> diary_core::Result<String> {
        Ok("stub 探针：没有联网".to_owned())
    }
}

/// 引擎配置：model_dir 只要不是空串就行（stub 不看文件）。
fn stub_config() -> EngineConfig {
    EngineConfig {
        engine_lib: None,
        model_dir: std::path::PathBuf::from("/stub-model"),
        model_file: None,
        kind: ModelKind::SenseVoice,
        language: Some("zh".to_owned()),
        num_threads: 1,
        itn: true,
    }
}

/// 让 `transcription` 用 stub 的配置。
///
/// **不设环境变量**：环境变量是进程级的，并行跑的测试会互相踩。钉死的配置只影响
/// 这一个 `Core`（见 `Core::set_asr_engine_config_for_test`）。
fn use_stub_config(core: &mut Core) {
    core.set_asr_engine_config_for_test(stub_config());
}

/// 跑一轮任务驱动。
fn run_due(core: &mut Core) {
    let summary = core.run_due_jobs(Utc::now(), 60_000, "test", 16).unwrap();
    let _ = summary;
}

/// 关键词检索一次，返回命中。
fn search(core: &mut Core, query: &str) -> Vec<diary_core::SearchHit> {
    let snapshot = core
        .start_search(
            SearchRequest {
                query: query.to_owned(),
                mode: SearchMode::Keyword,
                filters: SearchFilters::default(),
                page_size: 20,
            },
            1,
        )
        .unwrap();
    snapshot.results
}

// ------------------------------------------------------------------ 主链

/// 一段录音 → 转写 → 派生内容（带时间戳与字符区间）→ 关键词能搜到。
#[test]
fn transcription_lands_as_derived_content_and_is_searchable() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    // 70 秒 → 30 + 30 + 10，三段。
    let (_asset_id, source_id, capture_id) = record_and_finalize(&mut core, 70_000);
    use_stub_config(&mut core);
    core.set_asr_engine_for_test(StubEngine::new(&[
        "今天早上去了菜市场",
        "买了两斤西红柿",
        "回来做了番茄炒蛋",
    ]));

    run_due(&mut core);

    // 1) 任务成功，进度是真实值。
    let jobs = core.list_jobs(None, 10).unwrap();
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].kind, "transcribe");
    assert_eq!(jobs[0].state, JobState::Succeeded, "{:?}", jobs[0]);
    let progress = jobs[0].progress.as_ref().expect("跑完了要有真实进度");
    assert_eq!((progress.completed, progress.total), (3, 3));

    // 2) 落成派生内容：不覆盖原始录音，正文就是转写文本。
    let content = core.extracted_content(&source_id).unwrap().unwrap();
    assert_eq!(content.extractor_id, "local_transcription");
    assert_eq!(content.status, ProcessingStatus::Ready);
    assert_eq!(content.coverage, Coverage::Complete);
    assert_eq!(
        content.text,
        "今天早上去了菜市场\n买了两斤西红柿\n回来做了番茄炒蛋"
    );
    assert_eq!(content.segments.len(), 3, "一段一个片段，不多不少");

    // 3) 时间戳与字符区间对得上（「哪段文字来自哪段时间」）。
    let segments = &content.segments;
    assert_eq!(segments[0].locator.start_ms, Some(0));
    assert_eq!(segments[0].locator.end_ms, Some(30_000));
    assert_eq!(segments[1].locator.start_ms, Some(30_000));
    assert_eq!(segments[1].locator.end_ms, Some(60_000));
    assert_eq!(segments[2].locator.start_ms, Some(60_000));
    assert_eq!(segments[2].locator.end_ms, Some(70_000));
    for segment in segments {
        assert_eq!(
            segment.locator.locator_type,
            diary_core::LocatorType::Audio,
            "音频的来源要用 audio 定位"
        );
        assert_eq!(segment.locator.source_revision_id, content.source_revision_id);
    }
    // 字符区间：第 2 段起点 = 第 1 段长度 + 一个换行。
    assert_eq!(segments[0].locator.text_start, Some(0));
    assert_eq!(segments[0].locator.text_end, Some(9), "「今天早上去了菜市场」是 9 个字");
    assert_eq!(segments[1].locator.text_start, Some(10));
    assert_eq!(segments[1].locator.text_end, Some(17));

    // 4) 原始录音那条修订没有被动过：它仍然是用户原件、仍然带着资产。
    let capture = core.get_capture(&capture_id).unwrap();
    assert_eq!(capture.draft_text, "", "用户没有写文字，就不该凭空多出文字");
    assert_eq!(capture.ordered_source_ids.len(), 1);

    // 5) 关键词检索能搜到它，并且带着可以回溯的时间戳。
    let hits = search(&mut core, "西红柿");
    assert_eq!(hits.len(), 1, "转写出来的正文必须能被关键词搜到");
    assert_eq!(hits[0].coverage, Coverage::Complete);
    let locator = hits[0].locator.as_ref().expect("命中要能定位回原件");
    assert_eq!(locator.locator_type, diary_core::LocatorType::Audio);
    assert_eq!(locator.start_ms, Some(30_000));
    assert_eq!(locator.end_ms, Some(60_000));
    assert_eq!(hits[0].source_id.as_deref(), Some(source_id.as_str()));

    // 6) 语义分块这一路也看得到它（「成为材料的当前正文」）。
    let chunks = core
        .rebuild_text_chunks(Some(std::slice::from_ref(&source_id)))
        .unwrap();
    assert!(chunks > 0, "转写正文要能进分块");
}

/// 进程被杀：续跑不重头、不重复插入。
#[test]
fn an_interrupted_run_resumes_without_redoing_windows() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    let (_asset_id, source_id, _capture) = record_and_finalize(&mut core, 70_000);

    // 第一次：第 3 段（index 2）失败——等价于「进程在这里被杀」。
    let failing = StubEngine::new(&["甲", "乙", "丙"]).with_failure_at(2);
    use_stub_config(&mut core);
    core.set_asr_engine_for_test(failing.clone());
    run_due(&mut core);

    let job = core.list_jobs(None, 10).unwrap().remove(0);
    assert_eq!(
        job.state,
        JobState::RetryWait,
        "暂时性失败要退避重试，不是终态：{:?}",
        job
    );
    assert_eq!(failing.calls(), 3, "失败前跑了 3 次（0、1、2）");

    // 半截结果如实可见：partial + 写明跑到哪。
    let content = core.extracted_content(&source_id).unwrap().unwrap();
    assert_eq!(content.status, ProcessingStatus::Partial);
    assert_eq!(content.coverage, Coverage::Partial);
    assert_eq!(content.segments.len(), 2, "只有前两段落了库");
    assert!(content.text.contains("甲") && content.text.contains("乙"));
    assert!(!content.text.contains("丙"));
    let reason = content.coverage_reason.clone().unwrap_or_default();
    assert!(reason.contains("2/3"), "{reason}");
    assert!(reason.contains("停下原因"), "{reason}");

    // 第二次：换一个能跑完的 stub（模拟「重启后」），并把业务时钟推到退避到点。
    // 用业务时钟而不是睡眠：`run_due_jobs` 的「到期」判据看的就是它。
    // 这个 stub 只会被调用**一次**——正是缺的那一段，所以它给的文本就是那一段的。
    let healthy = StubEngine::new(&["丙"]);
    use_stub_config(&mut core);
    core.set_asr_engine_for_test(healthy.clone());
    let later = Utc::now() + chrono::Duration::hours(2);
    let summary = core.run_due_jobs(later, 60_000, "test", 16).unwrap();
    assert!(summary.processed >= 1, "退避到点之后要真的续跑：{summary:?}");

    assert_eq!(
        healthy.calls(),
        1,
        "只重跑缺的那一段：已经转好的两段不能再调用引擎一次"
    );
    let content = core.extracted_content(&source_id).unwrap().unwrap();
    assert_eq!(content.status, ProcessingStatus::Ready);
    assert_eq!(content.coverage, Coverage::Complete);
    assert_eq!(content.segments.len(), 3, "三段各一行，没有重复");
    assert_eq!(content.text, "甲\n乙\n丙");
    // 时间戳仍然对齐（重跑那一段落在它自己的窗口上）。
    assert_eq!(content.segments[2].locator.start_ms, Some(60_000));
    assert_eq!(content.segments[2].locator.end_ms, Some(70_000));
    assert_eq!(content.segments[1].locator.text_start, Some(2));
    assert_eq!(content.segments[2].locator.text_start, Some(4));
}

/// 幂等：同一份音频重放（再来一条同目标的任务）只有一份文本。
#[test]
fn replaying_the_same_audio_produces_one_copy_of_the_text() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    let (asset_id, source_id, _capture) = record_and_finalize(&mut core, 40_000);
    let engine = StubEngine::new(&["第一段", "第二段"]);
    use_stub_config(&mut core);
    core.set_asr_engine_for_test(engine.clone());
    run_due(&mut core);
    assert_eq!(engine.calls(), 2);

    let before = core.extracted_content(&source_id).unwrap().unwrap();
    assert_eq!(before.text, "第一段\n第二段");

    // 再排一条同目标的任务（旧任务已经成功，业务键查重不再命中它）。
    core.enqueue_job(
        NewJob::new("transcribe", JobPriority::BackgroundExtract).targeting(vec![asset_id]),
    )
    .unwrap();
    run_due(&mut core);

    assert_eq!(
        engine.calls(),
        2,
        "已经转好的片段不该再调用引擎一次（同片段重放不重复计算）"
    );
    let after = core.extracted_content(&source_id).unwrap().unwrap();
    assert_eq!(after.text, before.text);
    assert_eq!(after.segments.len(), before.segments.len());
    let hits = search(&mut core, "第二段");
    assert_eq!(hits.len(), 1, "同一段文本在索引里也只有一份");
}

/// 引擎/模型缺失：等配置、原因可执行、**任何内容都不写**。
#[test]
fn a_missing_engine_waits_for_configuration_without_writing_anything() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    let (_asset_id, source_id, _capture) = record_and_finalize(&mut core, 10_000);
    use_stub_config(&mut core);
    core.set_asr_engine_for_test(StubEngine::not_ready());

    run_due(&mut core);

    let job = core.list_jobs(None, 10).unwrap().remove(0);
    assert_eq!(job.state, JobState::WaitingConfiguration, "{job:?}");
    assert_eq!(job.error_code.as_deref(), Some("provider_not_configured"));
    assert!(job.requires_user_action, "要出现在状态里");
    assert_eq!(job.attempt_count, 0, "等配置不消耗重试额度");
    assert!(job.next_attempt_at.is_none(), "等配置不排退避");

    // 没有空文本冒充成功：一条派生内容都没有。
    assert!(
        core.extracted_content(&source_id).unwrap().is_none(),
        "引擎不可用时不能写出任何派生内容"
    );
    assert!(core.count_search_matches("段").unwrap() == 0);

    // 原因可执行：闸门给出的理由里带「缺什么 + 怎么办」。
    let gate = core
        .provider_capability_gate(diary_core::ProviderCapability::TranscribeAudio)
        .unwrap();
    assert!(!gate.ready);
    let next = gate.next_step.clone().unwrap_or_default();
    assert!(
        next.contains("DIARY_ASR_MODEL_DIR") || next.contains("模型目录"),
        "下一步要可执行：{next}"
    );
}

/// 中途失败：半截结果**不当完整文本**（partial + 原因），已完成的部分仍可搜。
#[test]
fn a_failed_run_leaves_partial_text_marked_partial() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    let (_asset_id, source_id, _capture) = record_and_finalize(&mut core, 70_000);
    let engine = StubEngine::new(&["前半句", "中间句", "后半句"]).with_failure_at(1);
    use_stub_config(&mut core);
    core.set_asr_engine_for_test(engine);
    run_due(&mut core);

    let content = core.extracted_content(&source_id).unwrap().unwrap();
    assert_eq!(content.coverage, Coverage::Partial, "不能报成 complete");
    assert_ne!(content.status, ProcessingStatus::Ready);
    assert!(content.text.contains("前半句"));
    assert!(!content.text.contains("后半句"), "没跑到的段不能出现在正文里");

    // 已完成的那一段现在就能搜到（用户不用等整段跑完）。
    let hits = search(&mut core, "前半句");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].coverage, Coverage::Partial, "检索结果要如实报覆盖程度");
}

/// 全段静音：引擎真的跑了，但没有文字——如实报 metadata_only，不假装有正文。
#[test]
fn silence_is_reported_as_no_text_instead_of_a_fake_transcript() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    let (_asset_id, source_id, _capture) = record_and_finalize(&mut core, 10_000);
    use_stub_config(&mut core);
    core.set_asr_engine_for_test(StubEngine::new(&["", ""]));
    run_due(&mut core);

    let content = core.extracted_content(&source_id).unwrap().unwrap();
    assert_eq!(content.text, "");
    assert_eq!(content.coverage, Coverage::MetadataOnly);
    assert_eq!(content.status, ProcessingStatus::Ready);
    assert!(content
        .coverage_reason
        .as_deref()
        .unwrap_or_default()
        .contains("没有识别出文字"));
    // 任务本身是成功的：引擎确实跑完了，只是这段音频没有人声。
    let job = core.list_jobs(None, 10).unwrap().remove(0);
    assert_eq!(job.state, JobState::Succeeded);
}

/// 不认识的容器：如实报不支持（不是「转写成功但文本为空」）。
#[test]
fn an_unsupported_container_fails_honestly() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    // 片段内容不是 WAV：finalize 会把它当成「逻辑音频」拼起来。
    let capture_id = core
        .create_draft(CreateDraftInput {
            occurred_at: None,
            time_zone: "Asia/Shanghai",
            utc_offset_minutes: 480,
            operation_id: "op-create",
        })
        .unwrap()
        .id;
    let ticket = core.prepare_recording(&capture_id, "op-rec").unwrap();
    std::fs::write(
        std::path::Path::new(&ticket.staging_ticket).join("seg-0.bin"),
        b"not a wav at all",
    )
    .unwrap();
    core.register_segment(
        &ticket.recording_id,
        0,
        SegmentManifest {
            segment_id: "seg0".to_owned(),
            relative_file_name: "seg-0.bin".to_owned(),
            duration_ms: 1_000,
            sha256: sha_of(b"not a wav at all"),
            byte_size: 16,
            closed_at: None,
        },
    )
    .unwrap();
    let result = core.finalize_recording(&ticket.recording_id, 0, "user").unwrap();
    let source_id = core.get_capture(&capture_id).unwrap().ordered_source_ids[0].clone();
    use_stub_config(&mut core);
    core.set_asr_engine_for_test(StubEngine::new(&["不该出现"]));
    run_due(&mut core);

    let job = core.list_jobs(None, 10).unwrap().remove(0);
    assert_eq!(job.state, JobState::Failed);
    assert_eq!(job.error_code.as_deref(), Some("unsupported_format"));
    assert!(core.extracted_content(&source_id).unwrap().is_none());
    let _ = result.asset_id;
}

/// 旧账：以前因 `no_handler` 放弃的转写任务，开库时一次性放回队列。
#[test]
fn legacy_no_handler_jobs_are_requeued_once_then_run() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.sqlite");
    let (asset_id, source_id) = {
        let mut core = Core::open(&path).unwrap();
        let (asset_id, source_id, _capture) = record_and_finalize(&mut core, 10_000);
        // 模拟「旧构建留下的账」：把任务改成旧行为下的终态（failed + no_handler）。
        let job = core.list_jobs(None, 10).unwrap().remove(0);
        let raw = rusqlite::Connection::open(&path).unwrap();
        raw.execute(
            "UPDATE jobs SET state = 'failed', error_code = 'no_handler', \
             requires_user_action = 1, attempt_count = 1 WHERE id = ?1",
            rusqlite::params![job.id],
        )
        .unwrap();
        (asset_id, source_id)
    };
    let _ = &asset_id;

    // 重开资料库：这一次开库把旧账放回队列。
    let mut core = Core::open(&path).unwrap();
    let job = core.list_jobs(None, 10).unwrap().remove(0);
    assert_eq!(job.state, JobState::Queued, "有执行者了就该放回队列");
    assert!(job.error_code.is_none(), "「没有执行者」这个理由不再成立");
    assert_eq!(job.attempt_count, 0, "与人工重试同一套清零语义");
    assert!(!job.requires_user_action);

    // 再开一次：判据自带幂等，不会重复动作（也不会重复发事件）。
    let events_before = core.last_event_sequence().unwrap();
    let reopened = Core::open(&path).unwrap();
    assert_eq!(
        reopened.last_event_sequence().unwrap(),
        events_before,
        "放回队列只该发生一次"
    );
    let job = reopened.list_jobs(None, 10).unwrap().remove(0);
    assert_eq!(job.state, JobState::Queued);
    drop(reopened);

    // 现在真的能跑完。
    use_stub_config(&mut core);
    core.set_asr_engine_for_test(StubEngine::new(&["旧账补上了"]));
    run_due(&mut core);
    let content = core.extracted_content(&source_id).unwrap().unwrap();
    assert_eq!(content.text, "旧账补上了");
}

/// 没有执行者的 kind 仍然是诚实的「已放弃 + no_handler」（这条口径没被放松）。
#[test]
fn a_kind_without_a_handler_is_still_terminal() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    let queued = core
        .enqueue_job(
            NewJob::new("future_kind", JobPriority::BackgroundExtract)
                .targeting(vec!["whatever".to_owned()]),
        )
        .unwrap();
    run_due(&mut core);
    let job = core.get_job(&queued.id).unwrap();
    assert_eq!(job.state, JobState::Failed);
    assert_eq!(job.error_code.as_deref(), Some("no_handler"));
    assert!(job.requires_user_action);
}

/// 「转写 → 索引」的前置：转写还没跑时，索引任务**等**而不是终态失败。
///
/// m4 文档在 `transcribe` 还没有执行者时留了一句「B4a 到位后要把 `transcribe`
/// 算进前置判断」——这一条就是那个口径的测试：判据要认**资产 id** 形式的转写任务。
#[test]
fn an_index_job_waits_for_a_live_transcription_instead_of_failing() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    let (_asset_id, source_id, _capture) = record_and_finalize(&mut core, 10_000);
    use_stub_config(&mut core);
    core.set_asr_engine_for_test(StubEngine::new(&["等转写的正文"]));

    // 转写任务此刻还在排队（finalize 排的），它就是索引任务要等的那条活。
    let transcribe_job = core.list_jobs(None, 10).unwrap().remove(0);
    assert_eq!(transcribe_job.kind, "transcribe");
    core.enqueue_job(
        NewJob::new("index", JobPriority::Maintenance).targeting(vec![source_id.clone()]),
    )
    .unwrap();

    // 预算 0：一条都不跑，但闸门仍然会把在等前置的索引任务如实标出来。
    let summary = core.run_due_jobs(Utc::now(), 0, "test", 16).unwrap();
    assert_eq!(summary.processed, 0);
    let index_job = core
        .list_jobs(None, 10)
        .unwrap()
        .into_iter()
        .find(|job| job.kind == "index")
        .unwrap();
    assert_eq!(
        index_job.state,
        JobState::WaitingDependency,
        "转写还活着，索引就该等：{index_job:?}"
    );
    assert_eq!(index_job.error_code.as_deref(), Some("waiting_transcribe"));

    // 转写跑完（同一条驱动会先跑转写、再把索引拉起），索引任务成功。
    let summary = core.run_due_jobs(Utc::now(), 60_000, "test", 16).unwrap();
    assert!(summary.succeeded >= 1, "{summary:?}");
    let index_job = core
        .list_jobs(None, 10)
        .unwrap()
        .into_iter()
        .find(|job| job.kind == "index")
        .unwrap();
    assert_eq!(index_job.state, JobState::Succeeded, "{index_job:?}");
    assert!(core.count_search_matches("转写的正文").unwrap() > 0);
}

/// 显式配置的本机引擎**说了算**，而且闸门与执行者用的是同一个目录。
///
/// 两条口径一起钉住：
/// 1. 配了一条本机引擎配置行之后，**不再回落到随包/内建引擎**——哪怕那行配错了；
/// 2. 闸门与执行者读的是**同一份**配置（`local_engine::transcription_choice` +
///    `config_for`），不会出现「闸门说可用、执行者去读另一个目录」。
#[test]
fn an_explicit_local_engine_config_decides_for_both_gate_and_executor() {
    use diary_core::ProviderCapability;

    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    let broken_dir = dir.path().join("no-such-model");
    let local_provider = |model: &std::path::Path| diary_core::Provider {
        id: "prv_local_asr".to_owned(),
        label: "我的本机引擎".to_owned(),
        adapter_id: "sherpa_onnx".to_owned(),
        endpoint: None,
        model: Some(model.display().to_string()),
        capabilities: [ProviderCapability::TranscribeAudio].into_iter().collect(),
        credential_ref: None,
        enabled: true,
        test_status: diary_core::ProviderTestStatus::Untested,
    };

    core.save_provider(&local_provider(&broken_dir), None).unwrap();
    let engine = StubEngine::new(&["不该跑"]).with_model_dir_check();
    core.set_asr_engine_for_test(engine.clone());
    let (_asset_id, _source_id, _capture) = record_and_finalize(&mut core, 10_000);
    run_due(&mut core);

    // 配置行指了一个不存在的目录：任务等配置，**不回落**到别的目录去转。
    let job = core.list_jobs(None, 10).unwrap().remove(0);
    assert_eq!(job.state, JobState::WaitingConfiguration, "{job:?}");
    assert_eq!(job.error_code.as_deref(), Some("provider_not_configured"));
    assert_eq!(engine.calls(), 0, "配错目录时一次引擎调用都不该发生");
    let gate = core
        .provider_capability_gate(ProviderCapability::TranscribeAudio)
        .unwrap();
    assert!(!gate.ready);
    assert!(
        gate.reason.contains("no-such-model"),
        "理由要指向用户配的那个目录：{}",
        gate.reason
    );

    // 把目录放好（并保存一次，触发等配置重评）：闸门与执行者都用它。
    let good_dir = dir.path().join("my-model");
    std::fs::create_dir_all(&good_dir).unwrap();
    std::fs::write(good_dir.join("model.int8.onnx"), b"stub").unwrap();
    std::fs::write(good_dir.join("tokens.txt"), b"a\n").unwrap();
    core.save_provider(&local_provider(&good_dir), None).unwrap();
    run_due(&mut core);

    let job = core.list_jobs(None, 10).unwrap().remove(0);
    assert_eq!(job.state, JobState::Succeeded, "{job:?}");
    assert_eq!(
        engine.seen_model_dir().as_deref(),
        Some(good_dir.as_path()),
        "执行者要读配置行里那个目录，而不是随包默认"
    );
}

/// 转写全链**不联网**：默认传输层是「一调用就 panic」的替身。
#[test]
fn transcription_never_touches_the_network() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    let (_asset_id, source_id, _capture) = record_and_finalize(&mut core, 10_000);
    use_stub_config(&mut core);
    core.set_asr_engine_for_test(StubEngine::new(&["离线也能转"]));
    core.set_transport_for_test(Arc::new(diary_core::PanicTransport {}));
    run_due(&mut core);
    let content = core.extracted_content(&source_id).unwrap().unwrap();
    assert_eq!(content.text, "离线也能转");
}

/// 质量集（#93）的 `TranscriptEngine` 接缝：本机引擎这一侧真的跑得通。
#[test]
fn quality_seam_runs_through_the_local_engine() {
    let stub = StubEngine::new(&["质量集回放"]);
    let engine = diary_core::LocalTranscriptEngine::new(stub.clone(), stub_config());
    let outcome = engine
        .transcribe(&diary_core::TranscriptRequest {
            case_id: "tr-01".to_owned(),
            audio: wav_of(1_000, 16_000),
            sample_rate: 16_000,
            channels: 1,
            language: Some("zh".to_owned()),
        })
        .unwrap();
    assert_eq!(outcome.text, "质量集回放");
    assert!(!engine.is_replay(), "本机引擎不是回放");
    assert!(engine.name().contains("stub_asr"));
    assert_eq!(stub.calls(), 1);
}

// ------------------------------------------------------------------ 真引擎（手动）

/// 真引擎 + **真人语音**：证明 FFI（C API 的 ABI、加载、推理）真的能跑通。
///
/// 这一步是 `#[ignore]` 的：CI 没有模型、没有引擎库。手动跑法：
///
/// ```bash
/// DIARY_ASR_ENGINE_LIB=/path/libsherpa-onnx-c-api.so \
/// DIARY_ASR_MODEL_DIR=/path/sherpa-onnx-sense-voice-zh-en-ja-ko-yue-2024-07-17 \
/// DIARY_ASR_TEST_WAV=/path/zh.wav \
/// cargo +1.94.1 test -p diary_core --test transcription_test -- --ignored --nocapture
/// ```
///
/// 没有环境变量时它**明确报「要什么」**，不装作跑过。
#[test]
#[ignore = "需要真引擎、模型与一段真实语音：手动跑，命令见测试文档"]
fn real_engine_transcribes_real_speech() {
    let model_dir = std::env::var("DIARY_ASR_MODEL_DIR").expect("要 DIARY_ASR_MODEL_DIR");
    let engine_lib = std::env::var("DIARY_ASR_ENGINE_LIB").expect("要 DIARY_ASR_ENGINE_LIB");
    let wav_path = std::env::var("DIARY_ASR_TEST_WAV")
        .unwrap_or_else(|_| format!("{model_dir}/zh.wav"));
    let bytes = std::fs::read(&wav_path).expect("读不到测试语音（DIARY_ASR_TEST_WAV）");
    let decoded =
        diary_core::transcription::wav::decode_bytes(&bytes, &wav_path).expect("测试语音不是可解码的 WAV");

    let config = diary_core::AsrEngineConfig {
        engine_lib: Some(std::path::PathBuf::from(&engine_lib)),
        model_dir: std::path::PathBuf::from(&model_dir),
        model_file: None,
        kind: diary_core::AsrModelKind::SenseVoice,
        language: Some("zh".to_owned()),
        num_threads: 2,
        itn: true,
    };
    let engine = diary_core::provider::local_engine::local_engine_for("sherpa_onnx")
        .expect("本构建里应当有 sherpa_onnx 引擎");
    let readiness = engine.readiness(&config);
    assert!(readiness.ready, "引擎要就绪：{}", readiness.reason);
    println!("就绪：{}", readiness.reason);
    let probe = engine.probe(&config).expect("探针要能加载模型并跑一次推理");
    println!("探针：{probe}");

    let outcome = engine
        .transcribe(
            &config,
            &diary_core::PcmRequest {
                samples: decoded.samples,
                sample_rate: decoded.sample_rate,
                language: Some("zh".to_owned()),
            },
        )
        .expect("真实语音转写不该失败");
    println!(
        "真引擎转写（{} 毫秒音频）：{:?}（模型 {}）",
        decoded.duration_ms, outcome.text, outcome.model.as_deref().unwrap_or("?")
    );
    assert!(
        !outcome.text.trim().is_empty(),
        "真人语音必须转出文字——转出空串说明 FFI 或模型配置有问题"
    );
}

/// 真引擎跑**整条产品链**（录音 → 任务 → 派生内容 → 检索）。
///
/// 测试音频是合成的静音 WAV，所以正文必然是空的——这条证明的是「链路通」，
/// 不是「转写准」。真实语音的那一条在上面。
///
/// 真引擎的量测：**有模型才跑**，没有就明确跳过。
///
/// 手动跑法（见 `docs/architecture/m3-转写链.md`）：
///
/// ```bash
/// DIARY_ASR_ENGINE_LIB=/path/to/libsherpa-onnx-c-api.so \
/// DIARY_ASR_MODEL_DIR=/path/to/sherpa-onnx-sense-voice-zh-en-ja-ko-yue-2024-07-17 \
/// cargo +1.94.1 test -p diary_core --test transcription_test -- --ignored --nocapture
/// ```
///
/// **它不在 CI 里**：CI 没有模型、没有引擎库。缺环境变量时它明确报出要什么，
/// 而不是装作跑过。
#[test]
#[ignore = "需要真引擎与模型：手动跑，见测试文档里的命令（DIARY_ASR_ENGINE_LIB / DIARY_ASR_MODEL_DIR）"]
fn real_engine_transcribes_a_real_recording() {
    let model_dir = std::env::var("DIARY_ASR_MODEL_DIR").expect("要 DIARY_ASR_MODEL_DIR");
    let engine_lib = std::env::var("DIARY_ASR_ENGINE_LIB").expect("要 DIARY_ASR_ENGINE_LIB");
    let dir = tempfile::tempdir().unwrap();
    let mut core = new_core(dir.path());
    std::env::set_var("DIARY_ASR_MODEL_DIR", &model_dir);
    std::env::set_var("DIARY_ASR_ENGINE_LIB", &engine_lib);
    let (_asset_id, source_id, _capture) = record_and_finalize(&mut core, 3_000);
    run_due(&mut core);
    let _ = &model_dir;
    let content = core.extracted_content(&source_id).unwrap().unwrap();
    println!(
        "真引擎（{}）覆盖={:?} 正文={:?}",
        content.extractor_version, content.coverage, content.text
    );
    assert_eq!(content.segments.len(), 1);
    assert_eq!(content.segments[0].locator.start_ms, Some(0));
    assert_eq!(content.segments[0].locator.end_ms, Some(3_000));
}
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use chrono::Utc;
use parking_lot::Mutex;
use tauri::Emitter;
use uuid::Uuid;

use crate::asr::dashscope_multimodal::{
    DashScopeAsyncRequestOptions, DashScopeAsyncTranscript, DashScopeMultimodalASR,
};
use crate::asr::local::speaker_diarization_runtime::{
    run_local_diarization, LocalDiarizationOutput,
};
use crate::asr::meeting_audio_source::{MeetingAudioSource, MeetingAudioValidationError};
use crate::persistence::{
    meeting_recording_existing_path_for_id, CredentialAccount, CredentialsVault, MeetingStore,
};
use crate::types::{
    ExpectedSpeakerCountOverride, MeetingAsrModelRef, MeetingAsrRuntimeKind,
    MeetingDiarizationMode, MeetingImportConfig, MeetingImportStatus,
    MeetingPostProcessingConfig, MeetingPostProcessingEvent, MeetingPostProcessingState,
    MeetingPostProcessingStatus, MeetingRecord, MeetingStatus, PostMeetingAsrModelDescriptor,
    ProcessingHold,
    RetryMeetingPostProcessingOptions, SpeakerProfile, SpeakerTurn, StartMeetingRecordingOptions,
    TranscriptRevision, TranscriptRevisionSource, TranscriptRevisionStatus, TranscriptSegment,
    TranscriptSegmentMetadata, TranscriptSegmentSource, UserPreferences,
};

use super::{
    derive_bailian_endpoint, prepare_and_spawn_auto_meeting_summary, BailianEndpointProtocol, Inner,
};
use super::meeting_organizer::prepare_and_spawn_auto_meeting_organized_draft;

const POST_MEETING_PROVIDER_ID: &str = "bailian";
const FUN_ASR_MODEL_ID: &str = "fun-asr";
const PARAFORMER_V2_MODEL_ID: &str = "paraformer-v2";
const REALTIME_TRANSCRIPT_REVISION: u32 = 0;
const FIRST_POST_PROCESSING_REVISION: u32 = 1;
const POST_MEETING_POLL_MIN_SECS: u64 = 600;
const POST_MEETING_POLL_MAX_SECS: u64 = 8 * 60 * 60;
const POST_PROCESSING_WORKER_STOP_TIMEOUT: Duration = Duration::from_secs(30);
static POST_PROCESSING_CANCEL_FLAGS: LazyLock<Mutex<HashMap<String, Arc<AtomicBool>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

type MeetingAudioPathResolver<'a> = dyn Fn(&str) -> anyhow::Result<PathBuf> + Sync + 'a;

#[cfg(target_os = "windows")]
struct LocalMeetingAsrContext<'a> {
    runtime: &'a Arc<crate::asr::local::SherpaOnnxRuntime>,
    language_hint: &'a str,
}

struct PostProcessingJobContext<'a> {
    inner: Option<&'a Arc<Inner>>,
    store: &'a MeetingStore,
    audio_path_for_id: &'a MeetingAudioPathResolver<'a>,
    #[cfg(target_os = "windows")]
    local_asr: Option<LocalMeetingAsrContext<'a>>,
}

#[derive(Debug, Clone)]
pub(crate) struct PostProcessingCancellationRequest {
    job_id: String,
    model_ref: MeetingAsrModelRef,
    provider_task_id: Option<String>,
}

pub(super) fn list_post_meeting_asr_models() -> Vec<PostMeetingAsrModelDescriptor> {
    vec![
        descriptor(FUN_ASR_MODEL_ID, "Fun-ASR", true),
        descriptor(PARAFORMER_V2_MODEL_ID, "Paraformer V2", false),
    ]
}

fn descriptor(
    model_id: &str,
    display_name: &str,
    is_default: bool,
) -> PostMeetingAsrModelDescriptor {
    PostMeetingAsrModelDescriptor {
        provider_id: POST_MEETING_PROVIDER_ID.to_string(),
        model_id: model_id.to_string(),
        display_name: display_name.to_string(),
        runtime_kind: MeetingAsrRuntimeKind::Cloud,
        supports_file_transcription: true,
        supports_diarization: true,
        supports_speaker_count: true,
        is_default,
    }
}

pub(super) fn resolve_post_meeting_asr_model(
    model_ref: &MeetingAsrModelRef,
) -> Result<PostMeetingAsrModelDescriptor, String> {
    let provider_id = model_ref.provider_id.trim();
    let model_id = model_ref.model_id.trim();
    list_post_meeting_asr_models()
        .into_iter()
        .find(|descriptor| descriptor.provider_id == provider_id && descriptor.model_id == model_id)
        .ok_or_else(|| format!("unsupported post-meeting ASR model: {provider_id}/{model_id}"))
}

pub(super) fn resolve_initial_post_processing_config(
    prefs: &UserPreferences,
    options: Option<&StartMeetingRecordingOptions>,
    realtime_provider_id: &str,
    realtime_model_id: Option<String>,
) -> Result<MeetingPostProcessingConfig, String> {
    let configured_ref = MeetingAsrModelRef {
        provider_id: prefs.post_meeting_asr.provider_id.clone(),
        model_id: prefs.post_meeting_asr.model_id.clone(),
    };
    let model_ref = options
        .and_then(|options| options.post_meeting_asr_model_ref.clone())
        .unwrap_or(configured_ref);
    let descriptor = resolve_post_meeting_asr_model(&model_ref)?;
    let diarization_mode = options
        .and_then(|options| options.diarization_mode)
        .unwrap_or(prefs.post_meeting_asr.diarization.mode);
    let local_diarization_model_id = normalized_optional_string(
        options
            .and_then(|options| options.local_diarization_model_id.clone())
            .or_else(|| prefs.post_meeting_asr.diarization.local_model_id.clone()),
    );
    if diarization_mode == MeetingDiarizationMode::Local {
        let model_id = local_diarization_model_id
            .as_deref()
            .ok_or_else(|| "local diarization model is required".to_string())?;
        crate::asr::local::speaker_diarization::ensure_package_ready(model_id)
            .map_err(|error| format!("localDiarizationModelNotReady: {error:#}"))?;
    }
    let expected_speaker_count = normalize_expected_speaker_count(
        options.and_then(|options| options.expected_speaker_count),
    )?;

    Ok(MeetingPostProcessingConfig {
        diarization_mode,
        realtime_provider_id: realtime_provider_id.to_string(),
        realtime_model_id,
        post_meeting_asr_model_ref: MeetingAsrModelRef {
            provider_id: descriptor.provider_id,
            model_id: descriptor.model_id,
        },
        resolved_asr_runtime_kind: descriptor.runtime_kind,
        local_diarization_model_id: if diarization_mode == MeetingDiarizationMode::Local {
            local_diarization_model_id
        } else {
            None
        },
        expected_speaker_count,
        model_version: None,
        processing_revision: FIRST_POST_PROCESSING_REVISION,
    })
}

fn normalize_expected_speaker_count(value: Option<u32>) -> Result<Option<u32>, String> {
    match value {
        Some(0) => Err("expected speaker count must be greater than zero".to_string()),
        Some(value) if value > 20 => {
            Err("expected speaker count must not exceed twenty".to_string())
        }
        other => Ok(other),
    }
}

fn normalized_optional_string(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let trimmed = value.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_string())
    })
}

pub(super) fn lock_realtime_model_in_post_processing_config(
    record: &mut MeetingRecord,
    provider_id: &str,
    model_id: Option<String>,
) {
    if let Some(config) = record.post_processing_config.as_mut() {
        config.realtime_provider_id = provider_id.to_string();
        config.realtime_model_id = model_id;
    }
}

pub(super) fn prepare_post_processing_after_stop(
    record: &mut MeetingRecord,
    now: &str,
) -> Result<bool, String> {
    let Some(config) = record.post_processing_config.as_ref() else {
        return Ok(false);
    };
    if record.transcript_revisions.is_empty() {
        record.transcript_revisions.push(TranscriptRevision {
            revision: REALTIME_TRANSCRIPT_REVISION,
            source: TranscriptRevisionSource::Realtime,
            status: TranscriptRevisionStatus::Active,
            segments: record.transcript_segments.clone(),
            created_at: now.to_string(),
        });
        record.active_transcript_revision = Some(REALTIME_TRANSCRIPT_REVISION);
    }
    if record.post_processing.as_ref().is_some_and(|state| {
        !matches!(
            state.status,
            MeetingPostProcessingStatus::Completed | MeetingPostProcessingStatus::Cancelled
        )
    }) {
        return Err("post-processing job already active".to_string());
    }

    let job_id = Uuid::new_v4().to_string();
    record.post_processing = Some(MeetingPostProcessingState {
        status: MeetingPostProcessingStatus::Pending,
        job_id: job_id.clone(),
        model_ref: config.post_meeting_asr_model_ref.clone(),
        resolved_runtime_kind: config.resolved_asr_runtime_kind,
        diarization_mode: config.diarization_mode,
        expected_speaker_count: config.expected_speaker_count,
        processing_revision: config.processing_revision,
        provider_task_id: None,
        progress: Some(0.0),
        attempt: 1,
        error_code: None,
        error_message: None,
        created_at: now.to_string(),
        updated_at: now.to_string(),
        started_at: None,
        completed_at: None,
    });
    record.processing_hold = Some(ProcessingHold {
        job_id,
        acquired_at: now.to_string(),
    });
    record.updated_at = now.to_string();
    debug_assert_eq!(config.processing_revision, FIRST_POST_PROCESSING_REVISION);
    Ok(true)
}

pub(super) fn stage_transcript_revision(
    record: &mut MeetingRecord,
    revision: u32,
    source: TranscriptRevisionSource,
    segments: Vec<TranscriptSegment>,
    created_at: &str,
) -> Result<(), String> {
    if let Some(existing) = record
        .transcript_revisions
        .iter()
        .find(|existing| existing.revision == revision)
    {
        if existing.status == TranscriptRevisionStatus::Staging
            && existing.source == source
            && existing.segments == segments
        {
            return Ok(());
        }
        return Err(format!("transcript revision {revision} already exists"));
    }
    validate_transcript_segments(&segments)?;
    record.transcript_revisions.push(TranscriptRevision {
        revision,
        source,
        status: TranscriptRevisionStatus::Staging,
        segments,
        created_at: created_at.to_string(),
    });
    Ok(())
}

pub(super) fn activate_staging_transcript_revision(
    record: &mut MeetingRecord,
    revision: u32,
) -> Result<(), String> {
    if record.active_transcript_revision == Some(revision) {
        return Ok(());
    }
    let index = record
        .transcript_revisions
        .iter()
        .position(|candidate| candidate.revision == revision)
        .ok_or_else(|| format!("transcript revision {revision} not found"))?;
    if record.transcript_revisions[index].status != TranscriptRevisionStatus::Staging {
        return Err(format!("transcript revision {revision} is not staging"));
    }
    validate_transcript_segments(&record.transcript_revisions[index].segments)?;
    record.transcript_revisions[index].status = TranscriptRevisionStatus::Active;
    record.transcript_segments = record.transcript_revisions[index].segments.clone();
    record.active_transcript_revision = Some(revision);
    Ok(())
}

fn validate_transcript_segments(segments: &[TranscriptSegment]) -> Result<(), String> {
    if segments.is_empty() {
        return Err("transcript revision is empty".to_string());
    }
    let mut previous_start = None;
    for segment in segments {
        if segment.text.trim().is_empty() {
            return Err("transcript revision contains empty text".to_string());
        }
        if segment
            .end_ms
            .is_some_and(|end_ms| end_ms < segment.start_ms)
        {
            return Err("transcript revision contains invalid time range".to_string());
        }
        if previous_start.is_some_and(|start_ms| segment.start_ms < start_ms) {
            return Err("transcript revision is not time ordered".to_string());
        }
        previous_start = Some(segment.start_ms);
    }
    Ok(())
}

pub(super) fn spawn_post_processing_job(inner: &Arc<Inner>, meeting_id: String, job_id: String) {
    let inner = Arc::clone(inner);
    let Some(cancelled) = register_post_processing_job(&job_id) else {
        log::debug!("[meeting-post-processing] job {job_id} already has a worker");
        return;
    };
    tauri::async_runtime::spawn(async move {
        if let Err(error) = run_post_processing_job(&inner, &meeting_id, &job_id, cancelled).await {
            log::warn!("[meeting-post-processing] job {job_id} failed: {error}");
            if let Err(persist_error) =
                fail_post_processing_job(&inner, &meeting_id, &job_id, &error)
            {
                log::warn!(
                    "[meeting-post-processing] persist failure for job {job_id} failed: {persist_error}"
                );
            }
        }
        POST_PROCESSING_CANCEL_FLAGS.lock().remove(&job_id);
    });
}

fn register_post_processing_job(job_id: &str) -> Option<Arc<AtomicBool>> {
    let mut flags = POST_PROCESSING_CANCEL_FLAGS.lock();
    if flags.contains_key(job_id) {
        return None;
    }
    let cancelled = Arc::new(AtomicBool::new(false));
    flags.insert(job_id.to_string(), Arc::clone(&cancelled));
    Some(cancelled)
}

fn signal_post_processing_cancel(job_id: &str) {
    if let Some(cancelled) = POST_PROCESSING_CANCEL_FLAGS.lock().get(job_id).cloned() {
        cancelled.store(true, Ordering::Release);
    }
}

fn spawn_remote_post_processing_cancel(
    model_ref: MeetingAsrModelRef,
    provider_task_id: Option<String>,
) {
    let Some(provider_task_id) = provider_task_id else {
        return;
    };
    if model_ref.provider_id != POST_MEETING_PROVIDER_ID {
        return;
    }
    tauri::async_runtime::spawn(async move {
        let client = match build_post_meeting_dashscope_client(&model_ref.model_id) {
            Ok(client) => client,
            Err(error) => {
                log::debug!(
                    "[meeting-post-processing] remote cancellation setup skipped for task {provider_task_id}: {error}"
                );
                return;
            }
        };
        if let Err(error) = client.cancel_async_task(&provider_task_id).await {
            log::debug!(
                "[meeting-post-processing] remote cancellation was not accepted for task {provider_task_id}: {error}"
            );
        }
    });
}

pub(crate) fn dispatch_post_processing_cancellation(request: PostProcessingCancellationRequest) {
    signal_post_processing_cancel(&request.job_id);
    spawn_remote_post_processing_cancel(request.model_ref, request.provider_task_id);
}

pub(crate) fn request_post_processing_stop_for_deletion(
    record: &MeetingRecord,
) -> Option<PostProcessingCancellationRequest> {
    let state = record.post_processing.as_ref()?;
    if !post_processing_status_is_active(state.status) {
        return None;
    }
    let request = PostProcessingCancellationRequest {
        job_id: state.job_id.clone(),
        model_ref: state.model_ref.clone(),
        provider_task_id: state.provider_task_id.clone(),
    };
    dispatch_post_processing_cancellation(request.clone());
    Some(request)
}

pub(crate) async fn wait_for_post_processing_worker_exit(
    request: &PostProcessingCancellationRequest,
) -> Result<(), String> {
    let started_at = std::time::Instant::now();
    loop {
        if !POST_PROCESSING_CANCEL_FLAGS
            .lock()
            .contains_key(&request.job_id)
        {
            return Ok(());
        }
        if started_at.elapsed() >= POST_PROCESSING_WORKER_STOP_TIMEOUT {
            return Err(
                "meetingPostProcessingStillStopping: 会后处理任务仍在停止，请稍后重试删除"
                    .to_string(),
            );
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

async fn run_post_processing_job(
    inner: &Arc<Inner>,
    meeting_id: &str,
    job_id: &str,
    cancelled: Arc<AtomicBool>,
) -> Result<(), String> {
    let store = MeetingStore::new().map_err(|error| error.to_string())?;
    let audio_path_for_id = |id: &str| meeting_recording_existing_path_for_id(id);
    #[cfg(target_os = "windows")]
    let language_hint = inner.prefs.get().sherpa_onnx_language_hint;
    let context = PostProcessingJobContext {
        inner: Some(inner),
        store: &store,
        audio_path_for_id: &audio_path_for_id,
        #[cfg(target_os = "windows")]
        local_asr: Some(LocalMeetingAsrContext {
            runtime: &inner.sherpa_onnx_runtime,
            language_hint: language_hint.trim(),
        }),
    };
    run_post_processing_job_with_context(&context, meeting_id, job_id, cancelled).await
}

async fn run_post_processing_job_with_context(
    context: &PostProcessingJobContext<'_>,
    meeting_id: &str,
    job_id: &str,
    cancelled: Arc<AtomicBool>,
) -> Result<(), String> {
    let store = context.store;
    let record = current_post_processing_record(store, meeting_id, job_id)?;
    let state = record.post_processing.as_ref().unwrap().clone();
    let model_id = if let Some(import_config) = record.import_config.as_ref() {
        validate_import_post_processing_route(&state, import_config)?
    } else {
        if state.resolved_runtime_kind != MeetingAsrRuntimeKind::Cloud {
            return Err("postMeetingAsrRuntimeUnsupported: 会后 ASR 运行时不是云端".to_string());
        }
        resolve_post_meeting_asr_model(&state.model_ref)?.model_id
    };

    if state.resolved_runtime_kind == MeetingAsrRuntimeKind::Local {
        #[cfg(target_os = "windows")]
        {
            let local_asr = context.local_asr.as_ref().ok_or_else(|| {
                "postMeetingAsrRuntimeUnsupported: 本地会后 ASR 缺少运行时上下文".to_string()
            })?;
            return run_local_import_post_processing_job(
                context, local_asr, record, state, meeting_id, job_id, cancelled,
            )
            .await;
        }
        #[cfg(not(target_os = "windows"))]
        return Err(
            "meetingAsrModelUnavailable: 当前平台暂不支持本地会议文件识别".to_string(),
        );
    }
    let request_options = cloud_diarization_options(&state)?;
    let resume_task_id = resume_provider_task_id(&state)?;
    let mut prepared_source = None;
    if resume_task_id.is_none() {
        update_post_processing_job_progress(
            context,
            meeting_id,
            job_id,
            MeetingPostProcessingStatus::PreparingAudio,
            Some(0.05),
            None,
        )?;
        let path = (context.audio_path_for_id)(meeting_id)
            .map_err(|error| format!("meetingAudioUnavailable: {error}"))?;
        let source = MeetingAudioSource::from_path(&path)
            .map_err(|error| format!("meetingAudioInvalid: {error}"))?;
        source
            .inspect()
            .map_err(|error| format!("meetingAudioInvalid: {error}"))?;
        if !validate_meeting_audio_for_asr(context, meeting_id, job_id, &source, &cancelled)? {
            return Ok(());
        }
        prepared_source = Some(source);
    }

    let client = build_post_meeting_dashscope_client(&model_id)?;
    let provider_task_id = if let Some(task_id) = resume_task_id {
        update_post_processing_job_progress(
            context,
            meeting_id,
            job_id,
            MeetingPostProcessingStatus::Running,
            Some(0.55),
            None,
        )?;
        task_id
    } else {
        let source = prepared_source
            .take()
            .ok_or_else(|| "meetingAudioInvalid: prepared audio is missing".to_string())?;
        ensure_job_not_cancelled(store, meeting_id, job_id, &cancelled)?;
        update_post_processing_job_progress(
            context,
            meeting_id,
            job_id,
            MeetingPostProcessingStatus::Uploading,
            Some(0.1),
            None,
        )?;
        let file_url = client
            .upload_meeting_audio(source, Arc::clone(&cancelled))
            .await
            .map_err(|error| format!("postMeetingAsrUploadFailed: {error}"))?;
        ensure_job_not_cancelled(store, meeting_id, job_id, &cancelled)?;
        update_post_processing_job_progress(
            context,
            meeting_id,
            job_id,
            MeetingPostProcessingStatus::Running,
            Some(0.45),
            None,
        )?;
        let task_id = client
            .submit_async_task(&file_url, request_options)
            .await
            .map_err(|error| format!("postMeetingAsrSubmitFailed: {error}"))?;
        update_post_processing_job_progress(
            context,
            meeting_id,
            job_id,
            MeetingPostProcessingStatus::Running,
            Some(0.55),
            Some(task_id.clone()),
        )?;
        task_id
    };

    ensure_job_not_cancelled(store, meeting_id, job_id, &cancelled)?;
    let poll_timeout = post_meeting_poll_timeout(record.duration_ms);
    let transcript = client
        .poll_async_task(&provider_task_id, poll_timeout, Arc::clone(&cancelled))
        .await
        .map_err(|error| format!("postMeetingAsrTaskFailed: {error}"))?;
    ensure_job_not_cancelled(store, meeting_id, job_id, &cancelled)?;

    let mut completed = if state.diarization_mode == MeetingDiarizationMode::Local {
        update_post_processing_job_progress(
            context,
            meeting_id,
            job_id,
            MeetingPostProcessingStatus::LocalAnalyzing,
            Some(0.75),
            Some(provider_task_id.clone()),
        )?;
        let local_model_id = record
            .post_processing_config
            .as_ref()
            .and_then(|config| config.local_diarization_model_id.clone())
            .ok_or_else(|| {
                "localDiarizationModelNotReady: 本场会议没有本地说话人模型快照".to_string()
            })?;
        crate::asr::local::speaker_diarization::ensure_package_ready(&local_model_id)
            .map_err(|error| format!("localDiarizationModelNotReady: {error:#}"))?;
        let path = (context.audio_path_for_id)(meeting_id)
            .map_err(|error| format!("meetingAudioUnavailable: {error}"))?;
        let source = MeetingAudioSource::from_path(&path)
            .map_err(|error| format!("meetingAudioInvalid: {error}"))?;
        let expected_speaker_count = state.expected_speaker_count;
        let cancelled_for_runtime = Arc::clone(&cancelled);
        let local_model_for_runtime = local_model_id.clone();
        let local_output = tauri::async_runtime::spawn_blocking(move || {
            run_local_diarization(
                source,
                &local_model_for_runtime,
                expected_speaker_count,
                cancelled_for_runtime,
            )
        })
        .await
        .map_err(|error| format!("localDiarizationRuntimeFailed: worker join failed: {error}"))?
        .map_err(|error| format!("localDiarizationRuntimeFailed: {error:#}"))?;
        ensure_job_not_cancelled(store, meeting_id, job_id, &cancelled)?;
        update_post_processing_job_progress(
            context,
            meeting_id,
            job_id,
            MeetingPostProcessingStatus::Applying,
            Some(0.95),
            Some(provider_task_id.clone()),
        )?;
        apply_local_post_processing_result(
            store,
            meeting_id,
            job_id,
            &model_id,
            &provider_task_id,
            &local_model_id,
            transcript,
            local_output,
        )?
    } else {
        update_post_processing_job_progress(
            context,
            meeting_id,
            job_id,
            MeetingPostProcessingStatus::Applying,
            Some(0.9),
            Some(provider_task_id.clone()),
        )?;
        apply_cloud_post_processing_result(
            store,
            meeting_id,
            job_id,
            &model_id,
            &provider_task_id,
            transcript,
        )?
    };
    finish_post_processing_job(context, &mut completed)?;
    Ok(())
}

#[cfg(test)]
pub(super) async fn run_post_processing_job_for_test(
    store: &MeetingStore,
    meeting_id: &str,
    job_id: &str,
    audio_path: PathBuf,
) -> Result<(), String> {
    let audio_path_for_id = |_id: &str| Ok(audio_path.clone());
    let context = PostProcessingJobContext {
        inner: None,
        store,
        audio_path_for_id: &audio_path_for_id,
        #[cfg(target_os = "windows")]
        local_asr: None,
    };
    run_post_processing_job_with_context(
        &context,
        meeting_id,
        job_id,
        Arc::new(AtomicBool::new(false)),
    )
    .await
}

#[cfg(all(test, target_os = "windows"))]
pub(super) async fn run_local_post_processing_job_for_test(
    store: &MeetingStore,
    meeting_id: &str,
    job_id: &str,
    audio_path: PathBuf,
    runtime: &Arc<crate::asr::local::SherpaOnnxRuntime>,
    language_hint: &str,
) -> Result<(), String> {
    let audio_path_for_id = |_id: &str| Ok(audio_path.clone());
    let context = PostProcessingJobContext {
        inner: None,
        store,
        audio_path_for_id: &audio_path_for_id,
        local_asr: Some(LocalMeetingAsrContext {
            runtime,
            language_hint,
        }),
    };
    run_post_processing_job_with_context(
        &context,
        meeting_id,
        job_id,
        Arc::new(AtomicBool::new(false)),
    )
    .await
}

fn validate_import_post_processing_route(
    state: &MeetingPostProcessingState,
    import_config: &MeetingImportConfig,
) -> Result<String, String> {
    let descriptor = super::meeting_audio_import::resolve_meeting_asr_model(&state.model_ref)?;
    if descriptor.runtime_kind != state.resolved_runtime_kind
        || descriptor.runtime_kind != import_config.resolved_asr_runtime_kind
        || descriptor.provider_id != import_config.asr_model_ref.provider_id
        || descriptor.model_id != import_config.asr_model_ref.model_id
    {
        return Err(
            "meetingAsrRouteMismatch: 导入任务的模型快照与后端注册表不一致".to_string(),
        );
    }
    super::meeting_audio_import::validate_meeting_asr_combination(
        &descriptor,
        state.diarization_mode,
        import_config.local_diarization_model_id.as_deref(),
    )?;
    Ok(descriptor.model_id)
}

#[cfg(target_os = "windows")]
async fn run_local_import_post_processing_job(
    context: &PostProcessingJobContext<'_>,
    local_asr: &LocalMeetingAsrContext<'_>,
    record: MeetingRecord,
    state: MeetingPostProcessingState,
    meeting_id: &str,
    job_id: &str,
    cancelled: Arc<AtomicBool>,
) -> Result<(), String> {
    let store = context.store;
    let import_config = record.import_config.as_ref().ok_or_else(|| {
        "postMeetingAsrRuntimeUnsupported: 现场会议不支持本地会后 ASR".to_string()
    })?;
    ensure_job_not_cancelled(store, meeting_id, job_id, &cancelled)?;
    update_post_processing_job_progress(
        context,
        meeting_id,
        job_id,
        MeetingPostProcessingStatus::LocalAnalyzing,
        Some(0.1),
        None,
    )?;
    let path = (context.audio_path_for_id)(meeting_id)
        .map_err(|error| format!("meetingAudioUnavailable: {error}"))?;
    let source = MeetingAudioSource::from_path(&path)
        .map_err(|error| format!("meetingAudioInvalid: {error}"))?;
    source
        .inspect()
        .map_err(|error| format!("meetingAudioInvalid: {error}"))?;
    if !validate_meeting_audio_for_asr(context, meeting_id, job_id, &source, &cancelled)? {
        return Ok(());
    }
    let (segments, profiles, turns) = super::meeting_audio_import::run_local_meeting_file_asr(
        local_asr.runtime,
        local_asr.language_hint,
        source,
        &state.model_ref,
        state.diarization_mode,
        import_config.local_diarization_model_id.as_deref(),
        state.expected_speaker_count,
        job_id,
        Arc::clone(&cancelled),
    )
    .await?;
    ensure_job_not_cancelled(store, meeting_id, job_id, &cancelled)?;
    update_post_processing_job_progress(
        context,
        meeting_id,
        job_id,
        MeetingPostProcessingStatus::Applying,
        Some(0.95),
        None,
    )?;
    let mut completed =
        apply_import_local_asr_result(store, meeting_id, job_id, segments, profiles, turns)?;
    finish_post_processing_job(context, &mut completed)?;
    Ok(())
}

fn finish_post_processing_job(
    context: &PostProcessingJobContext<'_>,
    completed: &mut MeetingRecord,
) -> Result<(), String> {
    let Some(inner) = context.inner else {
        return Ok(());
    };
    emit_post_processing_event(inner, completed);
    finish_post_processing_success(inner, context.store, completed)?;
    context
        .store
        .prune_audio_retention(inner.prefs.get().meeting_audio_retention_count)
        .map_err(|error| error.to_string())?;
    Ok(())
}

fn update_post_processing_job_progress(
    context: &PostProcessingJobContext<'_>,
    meeting_id: &str,
    job_id: &str,
    status: MeetingPostProcessingStatus,
    progress: Option<f32>,
    provider_task_id: Option<String>,
) -> Result<MeetingRecord, String> {
    let updated = persist_post_processing_progress(
        context.store,
        meeting_id,
        job_id,
        status,
        progress,
        provider_task_id,
    )?;
    if let Some(inner) = context.inner {
        emit_post_processing_event(inner, &updated);
    }
    Ok(updated)
}

fn finish_post_processing_success(
    inner: &Arc<Inner>,
    _store: &MeetingStore,
    completed: &mut MeetingRecord,
) -> Result<(), String> {
    let generate_summary = completed
        .import_config
        .as_ref()
        .map(|config| config.generate_summary)
        .unwrap_or(true);
    if generate_summary {
        if let Err(error) = prepare_and_spawn_auto_meeting_summary(inner, completed) {
            log::warn!("[meeting-post-processing] summary preparation failed: {error}");
        }
    }
    if let Err(error) = prepare_and_spawn_auto_meeting_organized_draft(inner, completed) {
        log::warn!("[meeting-post-processing] organized draft preparation failed: {error}");
    }
    if completed.import_state.is_some() {
        super::meeting_audio_import::emit_import_event(inner, completed);
    }
    Ok(())
}

fn resume_provider_task_id(state: &MeetingPostProcessingState) -> Result<Option<String>, String> {
    if let Some(task_id) = state.provider_task_id.clone() {
        return Ok(Some(task_id));
    }
    if matches!(
        state.status,
        MeetingPostProcessingStatus::Running
            | MeetingPostProcessingStatus::LocalAnalyzing
            | MeetingPostProcessingStatus::Applying
    ) {
        return Err(
            "postMeetingAsrSubmissionOutcomeUnknown: 云端任务提交结果未知；为避免重复提交，请手动重试"
                .to_string(),
        );
    }
    Ok(None)
}

fn current_post_processing_record(
    store: &MeetingStore,
    meeting_id: &str,
    job_id: &str,
) -> Result<MeetingRecord, String> {
    let record = store
        .get(meeting_id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "meeting not found".to_string())?;
    let state = record
        .post_processing
        .as_ref()
        .ok_or_else(|| "meeting has no post-processing job".to_string())?;
    if state.job_id != job_id || !post_processing_status_is_active(state.status) {
        return Err("postMeetingAsrJobCancelled: 后处理任务已取消或被替换".to_string());
    }
    Ok(record)
}

fn post_processing_status_is_active(status: MeetingPostProcessingStatus) -> bool {
    matches!(
        status,
        MeetingPostProcessingStatus::Pending
            | MeetingPostProcessingStatus::PreparingAudio
            | MeetingPostProcessingStatus::Uploading
            | MeetingPostProcessingStatus::Running
            | MeetingPostProcessingStatus::LocalAnalyzing
            | MeetingPostProcessingStatus::Applying
    )
}

fn validate_meeting_audio_for_asr(
    context: &PostProcessingJobContext<'_>,
    meeting_id: &str,
    job_id: &str,
    source: &MeetingAudioSource,
    cancelled: &Arc<AtomicBool>,
) -> Result<bool, String> {
    match source.validate_speech_energy(cancelled.as_ref()) {
        Ok(_) => Ok(true),
        Err(MeetingAudioValidationError::NoSpeech(energy)) => {
            fail_post_processing_job_with_details(
                context,
                meeting_id,
                job_id,
                "meetingAudioNoSpeech",
                &format!(
                    "peak={:.1} dB, activeFrames={}/{}, longestActiveFrames={}",
                    20.0 * energy.peak.max(f32::MIN_POSITIVE).log10(),
                    energy.active_frames,
                    energy.total_frames,
                    energy.longest_active_frames,
                ),
            )?;
            Ok(false)
        }
        Err(MeetingAudioValidationError::Read(error)) => {
            Err(format!("meetingAudioInvalid: {error:#}"))
        }
    }
}

fn build_post_meeting_dashscope_client(model_id: &str) -> Result<DashScopeMultimodalASR, String> {
    let api_key = CredentialsVault::get_asr_for_provider(
        POST_MEETING_PROVIDER_ID,
        CredentialAccount::AsrApiKey,
    )
    .map_err(|error| error.to_string())?
    .unwrap_or_default();
    if api_key.trim().is_empty() {
        return Err("postMeetingAsrCredentialsMissing: 请先配置阿里云百炼 API Key".to_string());
    }
    let stored_endpoint = CredentialsVault::get_asr_for_provider(
        POST_MEETING_PROVIDER_ID,
        CredentialAccount::AsrEndpoint,
    )
    .map_err(|error| error.to_string())?
    .unwrap_or_else(|| crate::asr::dashscope_multimodal::ASYNC_DEFAULT_ENDPOINT.to_string());
    let endpoint = derive_bailian_endpoint(
        &stored_endpoint,
        BailianEndpointProtocol::AsyncTranscription,
    )
    .map_err(|error| format!("postMeetingAsrEndpointInvalid: {error}"))?;
    Ok(DashScopeMultimodalASR::new(
        api_key,
        endpoint,
        model_id.to_string(),
    ))
}

fn cloud_diarization_options(
    state: &MeetingPostProcessingState,
) -> Result<DashScopeAsyncRequestOptions, String> {
    let diarization_enabled = state.diarization_mode == MeetingDiarizationMode::Cloud;
    let speaker_count = if diarization_enabled {
        state.expected_speaker_count.filter(|count| *count >= 2)
    } else {
        None
    };
    DashScopeAsyncRequestOptions {
        diarization_enabled,
        speaker_count,
    }
    .validate()
    .map_err(|error| error.to_string())
}

fn post_meeting_poll_timeout(duration_ms: Option<u64>) -> Duration {
    let duration_secs = duration_ms.unwrap_or_default() / 1000;
    let timeout_secs = duration_secs
        .saturating_add(300)
        .clamp(POST_MEETING_POLL_MIN_SECS, POST_MEETING_POLL_MAX_SECS);
    Duration::from_secs(timeout_secs)
}

fn ensure_job_not_cancelled(
    store: &MeetingStore,
    meeting_id: &str,
    job_id: &str,
    cancelled: &AtomicBool,
) -> Result<(), String> {
    if cancelled.load(Ordering::Acquire) {
        return Err("postMeetingAsrJobCancelled: 后处理任务已取消".to_string());
    }
    current_post_processing_record(store, meeting_id, job_id).map(|_| ())
}

fn persist_post_processing_progress(
    store: &MeetingStore,
    meeting_id: &str,
    job_id: &str,
    status: MeetingPostProcessingStatus,
    progress: Option<f32>,
    provider_task_id: Option<String>,
) -> Result<MeetingRecord, String> {
    let now = Utc::now().to_rfc3339();
    let updated = store
        .update_if(meeting_id, |record| {
            let Some(state) = record.post_processing.as_mut() else {
                return false;
            };
            if state.job_id != job_id || !post_processing_status_is_active(state.status) {
                return false;
            }
            state.status = status;
            state.progress = progress;
            if let Some(provider_task_id) = provider_task_id {
                state.provider_task_id = Some(provider_task_id);
            }
            state.started_at.get_or_insert_with(|| now.clone());
            state.updated_at = now.clone();
            state.error_code = None;
            state.error_message = None;
            if let Some(import_state) = record.import_state.as_mut() {
                import_state.status = if status == MeetingPostProcessingStatus::Applying {
                    MeetingImportStatus::Applying
                } else {
                    MeetingImportStatus::Transcribing
                };
                import_state.progress = progress.map(|value| 0.25 + value.clamp(0.0, 1.0) * 0.75);
                import_state.updated_at = now.clone();
            }
            record.updated_at = now.clone();
            true
        })
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "postMeetingAsrJobCancelled: 后处理任务已取消或被替换".to_string())?;
    Ok(updated)
}

fn apply_cloud_post_processing_result(
    store: &MeetingStore,
    meeting_id: &str,
    job_id: &str,
    model_id: &str,
    provider_task_id: &str,
    transcript: DashScopeAsyncTranscript,
) -> Result<MeetingRecord, String> {
    let now = Utc::now().to_rfc3339();
    let mut transition_error = None;
    let updated = store
        .update_if(meeting_id, |record| {
            match apply_cloud_result_transition(
                record,
                job_id,
                model_id,
                provider_task_id,
                &transcript,
                &now,
            ) {
                Ok(applied) => applied,
                Err(error) => {
                    transition_error = Some(error);
                    false
                }
            }
        })
        .map_err(|error| error.to_string())?;
    if let Some(error) = transition_error {
        return Err(format!("postMeetingAsrResultInvalid: {error}"));
    }
    updated.ok_or_else(|| "postMeetingAsrJobCancelled: 后处理任务已取消或被替换".to_string())
}

fn apply_local_post_processing_result(
    store: &MeetingStore,
    meeting_id: &str,
    job_id: &str,
    model_id: &str,
    provider_task_id: &str,
    local_model_id: &str,
    transcript: DashScopeAsyncTranscript,
    local_output: LocalDiarizationOutput,
) -> Result<MeetingRecord, String> {
    let now = Utc::now().to_rfc3339();
    let mut transition_error = None;
    let updated = store
        .update_if(meeting_id, |record| {
            match apply_local_result_transition(
                record,
                job_id,
                model_id,
                provider_task_id,
                local_model_id,
                &transcript,
                &local_output,
                &now,
            ) {
                Ok(applied) => applied,
                Err(error) => {
                    transition_error = Some(error);
                    false
                }
            }
        })
        .map_err(|error| error.to_string())?;
    if let Some(error) = transition_error {
        return Err(format!("localDiarizationResultInvalid: {error}"));
    }
    updated.ok_or_else(|| "postMeetingAsrJobCancelled: 后处理任务已取消或被替换".to_string())
}

fn apply_import_local_asr_result(
    store: &MeetingStore,
    meeting_id: &str,
    job_id: &str,
    segments: Vec<TranscriptSegment>,
    profiles: Vec<SpeakerProfile>,
    turns: Vec<SpeakerTurn>,
) -> Result<MeetingRecord, String> {
    let now = Utc::now().to_rfc3339();
    let mut transition_error = None;
    let updated = store
        .update_if(meeting_id, |record| {
            let result = (|| -> Result<bool, String> {
                let state = record
                    .post_processing
                    .as_ref()
                    .ok_or_else(|| "meeting has no post-processing job".to_string())?;
                if state.job_id != job_id
                    || state.status != MeetingPostProcessingStatus::Applying
                    || state.resolved_runtime_kind != MeetingAsrRuntimeKind::Local
                    || record.import_config.is_none()
                {
                    return Ok(false);
                }
                let revision = state.processing_revision;
                stage_transcript_revision(
                    record,
                    revision,
                    TranscriptRevisionSource::Imported,
                    segments.clone(),
                    &now,
                )?;
                record.speaker_profiles = profiles.clone();
                record.speaker_turns = turns.clone();
                activate_staging_transcript_revision(record, revision)?;
                complete_processing_transition(record, &now);
                Ok(true)
            })();
            match result {
                Ok(applied) => applied,
                Err(error) => {
                    transition_error = Some(error);
                    false
                }
            }
        })
        .map_err(|error| error.to_string())?;
    if let Some(error) = transition_error {
        return Err(format!("meetingLocalAsrResultInvalid: {error}"));
    }
    updated.ok_or_else(|| "postMeetingAsrJobCancelled: 后处理任务已取消或被替换".to_string())
}

fn apply_cloud_result_transition(
    record: &mut MeetingRecord,
    job_id: &str,
    model_id: &str,
    provider_task_id: &str,
    transcript: &DashScopeAsyncTranscript,
    now: &str,
) -> Result<bool, String> {
    let state = record
        .post_processing
        .as_ref()
        .ok_or_else(|| "meeting has no post-processing job".to_string())?;
    if state.job_id != job_id || state.status != MeetingPostProcessingStatus::Applying {
        return Ok(false);
    }
    if state.model_ref.provider_id != POST_MEETING_PROVIDER_ID
        || state.model_ref.model_id != model_id
        || state.provider_task_id.as_deref() != Some(provider_task_id)
    {
        return Ok(false);
    }
    let diarization_mode = state.diarization_mode;
    let revision = state.processing_revision;
    let (segments, profiles, turns) =
        normalize_cloud_transcript(model_id, provider_task_id, diarization_mode, transcript)?;
    let source = if record.import_config.is_some() {
        TranscriptRevisionSource::Imported
    } else {
        TranscriptRevisionSource::CloudPostprocess
    };
    stage_transcript_revision(record, revision, source, segments, now)?;
    record.speaker_profiles = profiles;
    record.speaker_turns = turns;
    activate_staging_transcript_revision(record, revision)?;
    complete_processing_transition(record, now);
    Ok(true)
}

fn apply_local_result_transition(
    record: &mut MeetingRecord,
    job_id: &str,
    model_id: &str,
    provider_task_id: &str,
    local_model_id: &str,
    transcript: &DashScopeAsyncTranscript,
    local_output: &LocalDiarizationOutput,
    now: &str,
) -> Result<bool, String> {
    let state = record
        .post_processing
        .as_ref()
        .ok_or_else(|| "meeting has no post-processing job".to_string())?;
    if state.job_id != job_id || state.status != MeetingPostProcessingStatus::Applying {
        return Ok(false);
    }
    if state.model_ref.provider_id != POST_MEETING_PROVIDER_ID
        || state.model_ref.model_id != model_id
        || state.provider_task_id.as_deref() != Some(provider_task_id)
        || state.diarization_mode != MeetingDiarizationMode::Local
    {
        return Ok(false);
    }
    let config = record
        .post_processing_config
        .as_ref()
        .ok_or_else(|| "meeting post-processing config is missing".to_string())?;
    if config.local_diarization_model_id.as_deref() != Some(local_model_id) {
        return Ok(false);
    }
    let revision = state.processing_revision;
    let (cloud_segments, _, _) = normalize_cloud_transcript(
        model_id,
        provider_task_id,
        MeetingDiarizationMode::Local,
        transcript,
    )?;
    let (segments, profiles) = align_local_speakers(cloud_segments, &local_output.turns)?;
    if profiles.len() != local_output.detected_speaker_count as usize {
        return Err(
            "local diarization speaker count is inconsistent with speaker turns".to_string(),
        );
    }
    let source = if record.import_config.is_some() {
        TranscriptRevisionSource::Imported
    } else {
        TranscriptRevisionSource::LocalPostprocess
    };
    stage_transcript_revision(record, revision, source, segments, now)?;
    record.speaker_profiles = profiles;
    record.speaker_turns = local_output.turns.clone();
    activate_staging_transcript_revision(record, revision)?;
    complete_processing_transition(record, now);
    Ok(true)
}

fn complete_processing_transition(record: &mut MeetingRecord, now: &str) {
    let state = record.post_processing.as_mut().unwrap();
    state.status = MeetingPostProcessingStatus::Completed;
    state.progress = Some(1.0);
    state.error_code = None;
    state.error_message = None;
    state.updated_at = now.to_string();
    state.completed_at = Some(now.to_string());
    let generate_summary = record
        .import_config
        .as_ref()
        .map(|config| config.generate_summary)
        .unwrap_or(false);
    if let Some(import_state) = record.import_state.as_mut() {
        import_state.status = if generate_summary {
            MeetingImportStatus::Summarizing
        } else {
            MeetingImportStatus::Completed
        };
        import_state.progress = Some(1.0);
        import_state.error_code = None;
        import_state.error_message = None;
        import_state.updated_at = now.to_string();
        import_state.completed_at = (!generate_summary).then(|| now.to_string());
        record.status = MeetingStatus::Completed;
    }
    if !generate_summary {
        record.processing_hold = None;
    }
    record.updated_at = now.to_string();
}

fn normalize_cloud_transcript(
    model_id: &str,
    provider_task_id: &str,
    diarization_mode: MeetingDiarizationMode,
    transcript: &DashScopeAsyncTranscript,
) -> Result<
    (
        Vec<TranscriptSegment>,
        Vec<SpeakerProfile>,
        Vec<SpeakerTurn>,
    ),
    String,
> {
    let use_speakers = diarization_mode == MeetingDiarizationMode::Cloud;
    let mut speaker_ids = Vec::<String>::new();
    let mut segments = Vec::with_capacity(transcript.sentences.len());
    let mut turns = Vec::new();
    for (index, sentence) in transcript.sentences.iter().enumerate() {
        let provider_speaker_id = if use_speakers {
            Some(
                sentence
                    .speaker_id
                    .clone()
                    .ok_or_else(|| "cloud diarization result is missing speaker_id".to_string())?,
            )
        } else {
            None
        };
        let stable_speaker_id = provider_speaker_id.as_ref().map(|provider_id| {
            let position = speaker_ids
                .iter()
                .position(|known| known == provider_id)
                .unwrap_or_else(|| {
                    speaker_ids.push(provider_id.clone());
                    speaker_ids.len() - 1
                });
            format!("speaker-{position}")
        });
        let speaker_label = stable_speaker_id
            .as_ref()
            .and_then(|id| id.strip_prefix("speaker-"))
            .and_then(|index| index.parse::<usize>().ok())
            .map(|index| format!("发言人 {}", index + 1))
            .unwrap_or_else(|| "未区分".to_string());
        if let Some(speaker_id) = stable_speaker_id.clone() {
            turns.push(SpeakerTurn {
                speaker_id,
                start_ms: sentence.begin_time_ms,
                end_ms: sentence.end_time_ms,
                confidence: None,
                overlapping: false,
            });
        }
        segments.push(TranscriptSegment {
            id: format!("post-{provider_task_id}-{}", index + 1),
            speaker_id: stable_speaker_id,
            speaker_label,
            start_ms: sentence.begin_time_ms,
            end_ms: Some(sentence.end_time_ms),
            text: sentence.text.clone(),
            source: TranscriptSegmentSource::RetranscribedAsr,
            metadata: Some(TranscriptSegmentMetadata {
                provider_id: Some(format!("{POST_MEETING_PROVIDER_ID}/{model_id}")),
                provider_session_id: Some(provider_task_id.to_string()),
                provider_segment_id: sentence.sentence_id.clone(),
                sentence_id: sentence.sentence_id.clone(),
                provider_start_ms: Some(sentence.begin_time_ms),
                provider_end_ms: Some(sentence.end_time_ms),
                ..TranscriptSegmentMetadata::default()
            }),
        });
    }
    let profiles = speaker_ids
        .into_iter()
        .enumerate()
        .map(|(index, provider_speaker_id)| SpeakerProfile {
            id: format!("speaker-{index}"),
            provider_speaker_id: Some(provider_speaker_id),
            display_name: format!("发言人 {}", index + 1),
            manually_named: false,
        })
        .collect();
    Ok((segments, profiles, turns))
}

const LOCAL_ALIGNMENT_MIN_PRIMARY_RATIO: f64 = 0.50;
const LOCAL_ALIGNMENT_SIGNIFICANT_SPEAKER_RATIO: f64 = 0.20;

fn align_local_speakers(
    mut segments: Vec<TranscriptSegment>,
    turns: &[SpeakerTurn],
) -> Result<(Vec<TranscriptSegment>, Vec<SpeakerProfile>), String> {
    if turns.is_empty() {
        return Err("local diarization returned no speaker turns".to_string());
    }
    let mut speaker_ids = Vec::<String>::new();
    for turn in turns {
        if turn.end_ms <= turn.start_ms {
            return Err("local diarization returned an invalid speaker turn".to_string());
        }
        if !speaker_ids.contains(&turn.speaker_id) {
            speaker_ids.push(turn.speaker_id.clone());
        }
    }

    for segment in &mut segments {
        let Some(end_ms) = segment.end_ms.filter(|end_ms| *end_ms > segment.start_ms) else {
            mark_segment_for_review(segment, false);
            continue;
        };
        let duration = end_ms - segment.start_ms;
        let mut overlaps_by_speaker = speaker_ids
            .iter()
            .map(|speaker_id| {
                let intervals = turns
                    .iter()
                    .filter(|turn| turn.speaker_id == *speaker_id)
                    .filter_map(|turn| {
                        let start = segment.start_ms.max(turn.start_ms);
                        let end = end_ms.min(turn.end_ms);
                        (end > start).then_some((start, end))
                    })
                    .collect::<Vec<_>>();
                (speaker_id.clone(), merged_interval_duration(intervals))
            })
            .filter(|(_, overlap)| *overlap > 0)
            .collect::<Vec<_>>();
        overlaps_by_speaker
            .sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
        let overlapping = turns.iter().enumerate().any(|(index, turn)| {
            turns.iter().skip(index + 1).any(|other| {
                turn.speaker_id != other.speaker_id
                    && ranges_overlap(
                        segment.start_ms,
                        end_ms,
                        turn.start_ms.max(other.start_ms),
                        turn.end_ms.min(other.end_ms),
                    ) > 0
            })
        });
        let Some((primary_speaker_id, primary_overlap)) = overlaps_by_speaker.first() else {
            mark_segment_for_review(segment, overlapping);
            continue;
        };
        let primary_ratio = *primary_overlap as f64 / duration as f64;
        let significant_speaker_count = overlaps_by_speaker
            .iter()
            .filter(|(_, overlap)| {
                *overlap as f64 / duration as f64 >= LOCAL_ALIGNMENT_SIGNIFICANT_SPEAKER_RATIO
            })
            .count();
        let needs_review = primary_ratio < LOCAL_ALIGNMENT_MIN_PRIMARY_RATIO
            || significant_speaker_count > 1
            || overlapping;
        segment.speaker_id = Some(primary_speaker_id.clone());
        segment.speaker_label = speaker_display_name(primary_speaker_id);
        let metadata = segment
            .metadata
            .get_or_insert_with(TranscriptSegmentMetadata::default);
        metadata.needs_review = needs_review;
        metadata.overlapping = overlapping;
    }

    let profiles = speaker_ids
        .into_iter()
        .map(|speaker_id| SpeakerProfile {
            display_name: speaker_display_name(&speaker_id),
            id: speaker_id,
            provider_speaker_id: None,
            manually_named: false,
        })
        .collect();
    Ok((segments, profiles))
}

fn mark_segment_for_review(segment: &mut TranscriptSegment, overlapping: bool) {
    segment.speaker_id = None;
    segment.speaker_label = "未确认".to_string();
    let metadata = segment
        .metadata
        .get_or_insert_with(TranscriptSegmentMetadata::default);
    metadata.needs_review = true;
    metadata.overlapping = overlapping;
}

fn speaker_display_name(speaker_id: &str) -> String {
    speaker_id
        .strip_prefix("speaker-")
        .and_then(|index| index.parse::<usize>().ok())
        .map(|index| format!("发言人 {}", index + 1))
        .unwrap_or_else(|| "未确认".to_string())
}

fn merged_interval_duration(mut intervals: Vec<(u64, u64)>) -> u64 {
    intervals.sort_by_key(|interval| interval.0);
    let mut total = 0u64;
    let mut current: Option<(u64, u64)> = None;
    for (start, end) in intervals {
        match current {
            Some((current_start, current_end)) if start <= current_end => {
                current = Some((current_start, current_end.max(end)));
            }
            Some((current_start, current_end)) => {
                total = total.saturating_add(current_end - current_start);
                current = Some((start, end));
            }
            None => current = Some((start, end)),
        }
    }
    if let Some((start, end)) = current {
        total = total.saturating_add(end - start);
    }
    total
}

fn ranges_overlap(a_start: u64, a_end: u64, b_start: u64, b_end: u64) -> u64 {
    a_end.min(b_end).saturating_sub(a_start.max(b_start))
}

fn fail_post_processing_job(
    inner: &Arc<Inner>,
    meeting_id: &str,
    job_id: &str,
    error: &str,
) -> Result<(), String> {
    if error.starts_with("postMeetingAsrJobCancelled:") {
        return Ok(());
    }
    let store = MeetingStore::new().map_err(|error| error.to_string())?;
    let now = Utc::now().to_rfc3339();
    let updated = store
        .update_if(meeting_id, |record| {
            fail_active_post_processing_job(record, job_id, error, &now)
        })
        .map_err(|error| error.to_string())?;
    if let Some(record) = updated {
        emit_post_processing_event(inner, &record);
    }
    Ok(())
}

fn fail_post_processing_job_with_details(
    context: &PostProcessingJobContext<'_>,
    meeting_id: &str,
    job_id: &str,
    error_code: &str,
    error_message: &str,
) -> Result<(), String> {
    let now = Utc::now().to_rfc3339();
    let updated = context
        .store
        .update_if(meeting_id, |record| {
            fail_active_post_processing_job_with_details(
                record,
                job_id,
                error_code,
                error_message,
                &now,
            )
        })
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "postMeetingAsrJobCancelled: 后处理任务已取消或被替换".to_string())?;
    if let Some(inner) = context.inner {
        emit_post_processing_event(inner, &updated);
    }
    Ok(())
}

fn fail_active_post_processing_job(
    record: &mut MeetingRecord,
    job_id: &str,
    error: &str,
    now: &str,
) -> bool {
    let (code, message) = error
        .split_once(':')
        .map(|(code, message)| (code.trim(), message.trim()))
        .unwrap_or(("postMeetingAsrFailed", error.trim()));
    fail_active_post_processing_job_with_details(record, job_id, code, message, now)
}

fn fail_active_post_processing_job_with_details(
    record: &mut MeetingRecord,
    job_id: &str,
    error_code: &str,
    error_message: &str,
    now: &str,
) -> bool {
    let Some(state) = record.post_processing.as_mut() else {
        return false;
    };
    if state.job_id != job_id || !post_processing_status_is_active(state.status) {
        return false;
    }
    state.status = MeetingPostProcessingStatus::Failed;
    state.error_code = Some(error_code.to_string());
    state.error_message = Some(error_message.to_string());
    state.progress = None;
    state.started_at.get_or_insert_with(|| now.to_string());
    state.updated_at = now.to_string();
    if let Some(import_state) = record.import_state.as_mut() {
        import_state.status = MeetingImportStatus::Failed;
        import_state.progress = None;
        import_state.error_code = state.error_code.clone();
        import_state.error_message = state.error_message.clone();
        import_state.updated_at = now.to_string();
    }
    record.updated_at = now.to_string();
    true
}

pub(crate) fn cancel_post_processing_for_deletion(
    record: &mut MeetingRecord,
    now: &str,
) -> Option<PostProcessingCancellationRequest> {
    let Some(state) = record.post_processing.as_mut() else {
        return None;
    };
    if !post_processing_status_is_active(state.status) {
        return None;
    }
    let request = PostProcessingCancellationRequest {
        job_id: state.job_id.clone(),
        model_ref: state.model_ref.clone(),
        provider_task_id: state.provider_task_id.clone(),
    };
    state.status = MeetingPostProcessingStatus::Cancelled;
    state.progress = None;
    state.error_code = None;
    state.error_message = None;
    state.updated_at = now.to_string();
    state.completed_at = Some(now.to_string());
    if let Some(import_state) = record.import_state.as_mut() {
        import_state.status = MeetingImportStatus::Cancelled;
        import_state.progress = None;
        import_state.error_code = None;
        import_state.error_message = None;
        import_state.updated_at = now.to_string();
        import_state.completed_at = Some(now.to_string());
    }
    record.processing_hold = None;
    record.updated_at = now.to_string();
    Some(request)
}

pub(super) fn retranscribe_meeting(
    inner: &Arc<Inner>,
    meeting_id: &str,
) -> Result<MeetingRecord, String> {
    let store = MeetingStore::new().map_err(|error| error.to_string())?;
    let record = store
        .get(meeting_id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "meeting not found".to_string())?;
    if record.import_config.is_some() {
        return Err("imported meeting must use the audio import retry flow".to_string());
    }
    if matches!(
        record.status,
        MeetingStatus::Recording | MeetingStatus::Paused | MeetingStatus::Summarizing
    ) {
        return Err("meeting is not available for retranscription".to_string());
    }
    if record.audio.state != crate::types::MeetingAudioState::Retained {
        return Err("meeting audio is not available for retranscription".to_string());
    }
    if record
        .post_processing
        .as_ref()
        .is_some_and(|state| post_processing_status_is_active(state.status))
    {
        return Err("post-processing job already active".to_string());
    }

    let expected_job = record
        .post_processing
        .as_ref()
        .map(|state| (state.job_id.clone(), state.status));
    let expected_active_revision = record.active_transcript_revision;
    let old_attempt = record
        .post_processing
        .as_ref()
        .map(|state| state.attempt)
        .unwrap_or_default();
    let next_revision = next_post_processing_revision(&record);
    let mut next_config = retranscription_config(inner, &record)?;
    next_config.processing_revision = next_revision;

    let now = Utc::now().to_rfc3339();
    let job_id = Uuid::new_v4().to_string();
    let next_state = MeetingPostProcessingState {
        status: MeetingPostProcessingStatus::Pending,
        job_id: job_id.clone(),
        model_ref: next_config.post_meeting_asr_model_ref.clone(),
        resolved_runtime_kind: next_config.resolved_asr_runtime_kind,
        diarization_mode: next_config.diarization_mode,
        expected_speaker_count: next_config.expected_speaker_count,
        processing_revision: next_revision,
        provider_task_id: None,
        progress: Some(0.0),
        attempt: old_attempt.saturating_add(1),
        error_code: None,
        error_message: None,
        created_at: now.clone(),
        updated_at: now.clone(),
        started_at: None,
        completed_at: None,
    };
    let updated = store
        .update_if(meeting_id, |record| {
            apply_retranscription_transition(
                record,
                expected_job.as_ref(),
                expected_active_revision,
                next_config,
                next_state,
                &now,
            )
        })
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "meeting changed; refresh and retry retranscription".to_string())?;
    emit_post_processing_event(inner, &updated);
    spawn_post_processing_job(inner, meeting_id.to_string(), job_id);
    Ok(updated)
}

fn retranscription_config(
    inner: &Arc<Inner>,
    record: &MeetingRecord,
) -> Result<MeetingPostProcessingConfig, String> {
    let mut config = if let Some(config) = record.post_processing_config.clone() {
        config
    } else {
        let prefs = inner.prefs.get();
        let realtime_provider_id = record
            .realtime_asr
            .as_ref()
            .map(|snapshot| snapshot.resolved_provider_id.as_str())
            .unwrap_or(prefs.active_asr_provider.as_str());
        let realtime_model_id = record
            .realtime_asr
            .as_ref()
            .and_then(|snapshot| snapshot.model_id.clone());
        resolve_initial_post_processing_config(
            &prefs,
            None,
            realtime_provider_id,
            realtime_model_id,
        )?
    };
    let descriptor = resolve_post_meeting_asr_model(&config.post_meeting_asr_model_ref)?;
    config.post_meeting_asr_model_ref = MeetingAsrModelRef {
        provider_id: descriptor.provider_id,
        model_id: descriptor.model_id,
    };
    config.resolved_asr_runtime_kind = descriptor.runtime_kind;
    if config.diarization_mode == MeetingDiarizationMode::Local {
        let model_id = config
            .local_diarization_model_id
            .as_deref()
            .ok_or_else(|| "local diarization model is required".to_string())?;
        crate::asr::local::speaker_diarization::ensure_package_ready(model_id)
            .map_err(|error| format!("localDiarizationModelNotReady: {error:#}"))?;
    }
    Ok(config)
}

fn next_post_processing_revision(record: &MeetingRecord) -> u32 {
    record
        .transcript_revisions
        .iter()
        .map(|revision| revision.revision)
        .chain(record.active_transcript_revision)
        .chain(
            record
                .post_processing_config
                .as_ref()
                .map(|config| config.processing_revision),
        )
        .chain(
            record
                .post_processing
                .as_ref()
                .map(|state| state.processing_revision),
        )
        .max()
        .unwrap_or_default()
        .saturating_add(1)
}

fn apply_retranscription_transition(
    record: &mut MeetingRecord,
    expected_job: Option<&(String, MeetingPostProcessingStatus)>,
    expected_active_revision: Option<u32>,
    next_config: MeetingPostProcessingConfig,
    next_state: MeetingPostProcessingState,
    now: &str,
) -> bool {
    let current_job_matches = match (record.post_processing.as_ref(), expected_job) {
        (None, None) => true,
        (Some(current), Some((expected_job_id, expected_status))) => {
            current.job_id == *expected_job_id && current.status == *expected_status
        }
        _ => false,
    };
    if !current_job_matches
        || record.active_transcript_revision != expected_active_revision
        || record.import_config.is_some()
        || record.audio.state != crate::types::MeetingAudioState::Retained
        || matches!(
            record.status,
            MeetingStatus::Recording | MeetingStatus::Paused | MeetingStatus::Summarizing
        )
        || record
            .post_processing
            .as_ref()
            .is_some_and(|state| post_processing_status_is_active(state.status))
    {
        return false;
    }
    for revision in &mut record.transcript_revisions {
        if revision.status == TranscriptRevisionStatus::Staging {
            revision.status = TranscriptRevisionStatus::Rejected;
        }
    }
    record.post_processing_config = Some(next_config);
    record.processing_hold = Some(ProcessingHold {
        job_id: next_state.job_id.clone(),
        acquired_at: now.to_string(),
    });
    record.post_processing = Some(next_state);
    record.updated_at = now.to_string();
    true
}

pub(super) fn retry_meeting_post_processing(
    inner: &Arc<Inner>,
    meeting_id: &str,
    options: Option<RetryMeetingPostProcessingOptions>,
) -> Result<MeetingRecord, String> {
    let store = MeetingStore::new().map_err(|error| error.to_string())?;
    let record = store
        .get(meeting_id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "meeting not found".to_string())?;
    let current_state = record
        .post_processing
        .as_ref()
        .ok_or_else(|| "meeting has no post-processing job".to_string())?;
    if !matches!(
        current_state.status,
        MeetingPostProcessingStatus::Failed | MeetingPostProcessingStatus::Cancelled
    ) {
        return Err("post-processing job is not retryable".to_string());
    }
    if record.audio.state != crate::types::MeetingAudioState::Retained {
        return Err("meeting audio is not available for retry".to_string());
    }

    let expected_job_id = current_state.job_id.clone();
    let expected_status = current_state.status;
    let previous_model_ref = current_state.model_ref.clone();
    let previous_provider_task_id = current_state.provider_task_id.clone();
    let old_attempt = current_state.attempt;
    let old_config = record
        .post_processing_config
        .clone()
        .ok_or_else(|| "meeting post-processing config is missing".to_string())?;
    let options = options.unwrap_or_default();
    let model_ref = options
        .post_meeting_asr_model_ref
        .unwrap_or(old_config.post_meeting_asr_model_ref);
    let descriptor = resolve_post_meeting_asr_model(&model_ref)?;
    let diarization_mode = options
        .diarization_mode
        .unwrap_or(old_config.diarization_mode);
    let local_model_id = normalized_optional_string(
        options
            .local_diarization_model_id
            .or(old_config.local_diarization_model_id),
    );
    if diarization_mode == MeetingDiarizationMode::Local {
        let model_id = local_model_id
            .as_deref()
            .ok_or_else(|| "local diarization model is required".to_string())?;
        crate::asr::local::speaker_diarization::ensure_package_ready(model_id)
            .map_err(|error| format!("localDiarizationModelNotReady: {error:#}"))?;
    }
    let expected_speaker_count = match options.expected_speaker_count {
        None => old_config.expected_speaker_count,
        Some(ExpectedSpeakerCountOverride::Auto) => None,
        Some(ExpectedSpeakerCountOverride::Fixed { count }) => {
            normalize_expected_speaker_count(Some(count))?
        }
    };
    let now = Utc::now().to_rfc3339();
    let job_id = Uuid::new_v4().to_string();
    let next_revision = old_config.processing_revision.saturating_add(1);
    let next_config = MeetingPostProcessingConfig {
        diarization_mode,
        realtime_provider_id: old_config.realtime_provider_id,
        realtime_model_id: old_config.realtime_model_id,
        post_meeting_asr_model_ref: MeetingAsrModelRef {
            provider_id: descriptor.provider_id,
            model_id: descriptor.model_id,
        },
        resolved_asr_runtime_kind: descriptor.runtime_kind,
        local_diarization_model_id: if diarization_mode == MeetingDiarizationMode::Local {
            local_model_id
        } else {
            None
        },
        expected_speaker_count,
        model_version: None,
        processing_revision: next_revision,
    };
    let next_state = MeetingPostProcessingState {
        status: MeetingPostProcessingStatus::Pending,
        job_id: job_id.clone(),
        model_ref: next_config.post_meeting_asr_model_ref.clone(),
        resolved_runtime_kind: descriptor.runtime_kind,
        diarization_mode,
        expected_speaker_count,
        processing_revision: next_revision,
        provider_task_id: None,
        progress: Some(0.0),
        attempt: old_attempt.saturating_add(1),
        error_code: None,
        error_message: None,
        created_at: now.clone(),
        updated_at: now.clone(),
        started_at: None,
        completed_at: None,
    };
    let updated = store
        .update_if(meeting_id, |record| {
            apply_retry_transition(
                record,
                &expected_job_id,
                expected_status,
                next_config,
                next_state,
                &now,
            )
        })
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "post-processing job changed; refresh and retry".to_string())?;
    dispatch_post_processing_cancellation(PostProcessingCancellationRequest {
        job_id: expected_job_id,
        model_ref: previous_model_ref,
        provider_task_id: previous_provider_task_id,
    });
    emit_post_processing_event(inner, &updated);
    spawn_post_processing_job(inner, meeting_id.to_string(), job_id);
    Ok(updated)
}

fn apply_retry_transition(
    record: &mut MeetingRecord,
    expected_job_id: &str,
    expected_status: MeetingPostProcessingStatus,
    next_config: MeetingPostProcessingConfig,
    next_state: MeetingPostProcessingState,
    now: &str,
) -> bool {
    let Some(current_state) = record.post_processing.as_ref() else {
        return false;
    };
    if current_state.job_id != expected_job_id
        || current_state.status != expected_status
        || !matches!(
            current_state.status,
            MeetingPostProcessingStatus::Failed | MeetingPostProcessingStatus::Cancelled
        )
        || record.audio.state != crate::types::MeetingAudioState::Retained
    {
        return false;
    }
    for revision in &mut record.transcript_revisions {
        if revision.status == TranscriptRevisionStatus::Staging {
            revision.status = TranscriptRevisionStatus::Rejected;
        }
    }
    record.post_processing_config = Some(next_config);
    record.processing_hold = Some(ProcessingHold {
        job_id: next_state.job_id.clone(),
        acquired_at: now.to_string(),
    });
    record.post_processing = Some(next_state);
    record.updated_at = now.to_string();
    true
}

pub(super) fn cancel_meeting_post_processing(
    inner: &Arc<Inner>,
    meeting_id: &str,
) -> Result<MeetingRecord, String> {
    let store = MeetingStore::new().map_err(|error| error.to_string())?;
    let record = store
        .get(meeting_id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "meeting not found".to_string())?;
    let state = record
        .post_processing
        .as_ref()
        .ok_or_else(|| "meeting has no post-processing job".to_string())?;
    if !post_processing_status_is_active(state.status) {
        return Err("terminal post-processing job cannot be cancelled".to_string());
    }
    let expected_job_id = state.job_id.clone();
    let model_ref = state.model_ref.clone();
    let provider_task_id = state.provider_task_id.clone();
    let now = Utc::now().to_rfc3339();
    let mut updated = store
        .update_if(meeting_id, |record| {
            apply_cancel_transition(record, &expected_job_id, &now)
        })
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "post-processing job changed; refresh and retry".to_string())?;
    dispatch_post_processing_cancellation(PostProcessingCancellationRequest {
        job_id: expected_job_id,
        model_ref,
        provider_task_id,
    });
    apply_retention_after_terminal_transition(&store, &mut updated, inner)?;
    Ok(updated)
}

fn apply_cancel_transition(record: &mut MeetingRecord, expected_job_id: &str, now: &str) -> bool {
    let Some(state) = record.post_processing.as_mut() else {
        return false;
    };
    if state.job_id != expected_job_id || state.status.is_terminal() {
        return false;
    }
    state.status = MeetingPostProcessingStatus::Cancelled;
    state.progress = None;
    state.error_code = None;
    state.error_message = None;
    state.updated_at = now.to_string();
    state.completed_at = Some(now.to_string());
    if let Some(import_state) = record.import_state.as_mut() {
        import_state.status = MeetingImportStatus::Cancelled;
        import_state.progress = None;
        import_state.error_code = None;
        import_state.error_message = None;
        import_state.updated_at = now.to_string();
        import_state.completed_at = Some(now.to_string());
    }
    record.processing_hold = None;
    record.updated_at = now.to_string();
    true
}

pub(super) fn use_realtime_transcript_and_summarize(
    inner: &Arc<Inner>,
    meeting_id: &str,
) -> Result<MeetingRecord, String> {
    let store = MeetingStore::new().map_err(|error| error.to_string())?;
    let record = store
        .get(meeting_id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "meeting not found".to_string())?;
    let state = record
        .post_processing
        .as_ref()
        .ok_or_else(|| "meeting has no post-processing job".to_string())?;
    if !matches!(
        state.status,
        MeetingPostProcessingStatus::Failed | MeetingPostProcessingStatus::Cancelled
    ) {
        return Err("post-processing job must be failed or cancelled".to_string());
    }
    let expected_job_id = state.job_id.clone();
    let expected_status = state.status;
    let model_ref = state.model_ref.clone();
    let provider_task_id = state.provider_task_id.clone();
    let now = Utc::now().to_rfc3339();
    let mut updated = store
        .update_if(meeting_id, |record| {
            apply_realtime_accept_transition(record, &expected_job_id, expected_status, &now)
        })
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "post-processing job changed; refresh and retry".to_string())?;
    dispatch_post_processing_cancellation(PostProcessingCancellationRequest {
        job_id: expected_job_id,
        model_ref,
        provider_task_id,
    });
    emit_post_processing_event(inner, &updated);
    if let Err(error) = prepare_and_spawn_auto_meeting_summary(inner, &mut updated) {
        log::warn!("[meeting-post-processing] realtime summary preparation failed: {error}");
    }
    if let Err(error) = prepare_and_spawn_auto_meeting_organized_draft(inner, &mut updated) {
        log::warn!("[meeting-post-processing] realtime organized draft preparation failed: {error}");
    }
    let retention_count = inner.prefs.get().meeting_audio_retention_count;
    store
        .prune_audio_retention(retention_count)
        .map_err(|error| error.to_string())?;
    Ok(updated)
}

fn apply_realtime_accept_transition(
    record: &mut MeetingRecord,
    expected_job_id: &str,
    expected_status: MeetingPostProcessingStatus,
    now: &str,
) -> bool {
    let Some(state) = record.post_processing.as_mut() else {
        return false;
    };
    if state.job_id != expected_job_id
        || state.status != expected_status
        || !matches!(
            state.status,
            MeetingPostProcessingStatus::Failed | MeetingPostProcessingStatus::Cancelled
        )
    {
        return false;
    }
    state.status = MeetingPostProcessingStatus::RealtimeAccepted;
    state.progress = None;
    state.error_code = None;
    state.error_message = None;
    state.updated_at = now.to_string();
    state.completed_at = Some(now.to_string());
    record.processing_hold = None;
    if let Some(realtime) = record
        .transcript_revisions
        .iter()
        .find(|revision| revision.source == TranscriptRevisionSource::Realtime)
    {
        record.transcript_segments = realtime.segments.clone();
        record.active_transcript_revision = Some(realtime.revision);
    }
    record.updated_at = now.to_string();
    true
}

pub(super) fn rename_meeting_speaker(
    inner: &Arc<Inner>,
    meeting_id: &str,
    speaker_id: &str,
    display_name: &str,
) -> Result<MeetingRecord, String> {
    let display_name = display_name.trim();
    if display_name.is_empty() {
        return Err("speaker display name cannot be empty".to_string());
    }
    let store = MeetingStore::new().map_err(|error| error.to_string())?;
    let now = Utc::now().to_rfc3339();
    let updated = store
        .update_if(meeting_id, |record| {
            let Some(profile) = record
                .speaker_profiles
                .iter_mut()
                .find(|profile| profile.id == speaker_id)
            else {
                return false;
            };
            profile.display_name = display_name.to_string();
            profile.manually_named = true;
            record.updated_at = now;
            true
        })
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "speaker not found or meeting changed".to_string())?;
    emit_post_processing_event(inner, &updated);
    Ok(updated)
}

fn apply_retention_after_terminal_transition(
    store: &MeetingStore,
    record: &mut MeetingRecord,
    inner: &Arc<Inner>,
) -> Result<(), String> {
    let retention_count = inner.prefs.get().meeting_audio_retention_count;
    store
        .prune_audio_retention(retention_count)
        .map_err(|error| error.to_string())?;
    if let Some(updated) = store.get(&record.id).map_err(|error| error.to_string())? {
        *record = updated;
    }
    emit_post_processing_event(inner, record);
    Ok(())
}

pub(super) fn resume_post_processing_jobs(inner: &Arc<Inner>) -> Result<usize, String> {
    let store = MeetingStore::new().map_err(|error| error.to_string())?;
    let jobs: Vec<(String, String)> = store
        .list()
        .map_err(|error| error.to_string())?
        .into_iter()
        .filter_map(resumable_post_processing_job)
        .collect();
    for (meeting_id, job_id) in &jobs {
        spawn_post_processing_job(inner, meeting_id.clone(), job_id.clone());
    }
    Ok(jobs.len())
}

fn resumable_post_processing_job(record: MeetingRecord) -> Option<(String, String)> {
    let state = record.post_processing.as_ref()?;
    matches!(
        state.status,
        MeetingPostProcessingStatus::Pending
            | MeetingPostProcessingStatus::PreparingAudio
            | MeetingPostProcessingStatus::Uploading
            | MeetingPostProcessingStatus::Running
            | MeetingPostProcessingStatus::LocalAnalyzing
            | MeetingPostProcessingStatus::Applying
    )
    .then(|| (record.id, state.job_id.clone()))
}

fn emit_post_processing_event(inner: &Arc<Inner>, record: &MeetingRecord) {
    if let (Some(app), Some(state)) = (inner.app.lock().clone(), record.post_processing.clone()) {
        let payload = MeetingPostProcessingEvent {
            meeting_id: record.id.clone(),
            state,
            meeting: record.clone(),
        };
        let _ = app.emit("meeting:post-processing-state", payload.clone());
        let _ = app.emit("meeting:record-updated", payload.meeting);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asr::dashscope_multimodal::DashScopeAsyncSentence;
    use crate::asr::wav::encode_wav_16k_mono;
    use crate::types::{
        MeetingAudioMeta, MeetingAudioState, MeetingStatus, MeetingSummary, SpeakerProfile,
        TranscriptSegmentSource,
    };

    #[cfg(target_os = "windows")]
    async fn run_configured_cloud_worker_fixture(
        store_path: &std::path::Path,
        audio_path: &std::path::Path,
        duration_ms: u64,
        model_id: &str,
        diarization_mode: MeetingDiarizationMode,
        local_model_id: Option<&str>,
        expected_speaker_count: Option<u32>,
        meeting_id: String,
    ) -> Result<MeetingRecord, &'static str> {
        let mut candidate = record();
        candidate.id = meeting_id.clone();
        candidate.duration_ms = Some(duration_ms);
        let config = candidate.post_processing_config.as_mut().unwrap();
        config.post_meeting_asr_model_ref.model_id = model_id.to_string();
        config.diarization_mode = diarization_mode;
        config.local_diarization_model_id = local_model_id.map(str::to_string);
        config.expected_speaker_count = expected_speaker_count;
        prepare_post_processing_after_stop(&mut candidate, "2026-08-13T00:00:00Z")
            .map_err(|_| "cloud worker revision setup failed")?;
        let job_id = candidate.post_processing.as_ref().unwrap().job_id.clone();
        let store = MeetingStore::new_for_path(store_path.to_path_buf());
        store
            .create(candidate)
            .map_err(|_| "cloud worker fixture persistence failed")?;
        let audio_path = audio_path.to_path_buf();
        let audio_path_for_id = |_id: &str| Ok(audio_path.clone());
        let context = PostProcessingJobContext {
            inner: None,
            store: &store,
            audio_path_for_id: &audio_path_for_id,
            #[cfg(target_os = "windows")]
            local_asr: None,
        };
        run_post_processing_job_with_context(
            &context,
            &meeting_id,
            &job_id,
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .map_err(|_| "cloud worker execution failed")?;

        MeetingStore::new_for_path(store_path.to_path_buf())
            .get(&meeting_id)
            .map_err(|_| "cloud worker result reload failed")?
            .ok_or("cloud worker result was not persisted")
    }

    #[cfg(target_os = "windows")]
    #[tokio::test]
    #[ignore = "requires configured Bailian credentials and OPENLESS_MEETING_CLOUD_ASR_TEST_WAV"]
    async fn configured_bailian_runs_both_post_meeting_models_with_cloud_diarization() {
        let path = std::env::var_os("OPENLESS_MEETING_CLOUD_ASR_TEST_WAV")
            .map(std::path::PathBuf::from)
            .expect("OPENLESS_MEETING_CLOUD_ASR_TEST_WAV must point to a two-speaker PCM WAV");
        let probe = crate::asr::meeting_audio_import::probe_pcm_wav(&path)
            .expect("cloud post-meeting test WAV must be readable");
        let managed_dir = std::env::temp_dir().join(format!(
            "meeting-cloud-asr-integration-{}",
            Uuid::new_v4()
        ));
        let partial_path = managed_dir.join("part-0001.wav.partial");
        let final_path = managed_dir.join("part-0001.wav");
        let store_path = managed_dir.join("meetings.json");
        let result = async {
            crate::asr::meeting_audio_import::normalize_pcm_wav(
                &probe,
                &partial_path,
                &final_path,
                &AtomicBool::new(false),
                |_| {},
            )
            .map_err(|_| "cloud post-meeting test WAV normalization failed")?;
            let source = MeetingAudioSource::from_path(&final_path)
                .map_err(|_| "normalized cloud post-meeting test WAV must be readable")?;
            let info = source
                .inspect()
                .map_err(|_| "cloud post-meeting test WAV must be valid")?;

            for model_id in [FUN_ASR_MODEL_ID, PARAFORMER_V2_MODEL_ID] {
                let persisted = run_configured_cloud_worker_fixture(
                    &store_path,
                    &final_path,
                    info.duration_ms,
                    model_id,
                    MeetingDiarizationMode::Cloud,
                    None,
                    Some(2),
                    format!("meeting-cloud-worker-{model_id}"),
                )
                .await?;
                let state = persisted
                    .post_processing
                    .as_ref()
                    .ok_or("cloud worker post-processing state is missing")?;
                let task_id = state
                    .provider_task_id
                    .as_deref()
                    .ok_or("cloud worker provider task id is missing")?;
                if state.status != MeetingPostProcessingStatus::Completed
                    || state.model_ref.model_id != model_id
                    || persisted.active_transcript_revision != Some(1)
                    || persisted.processing_hold.is_some()
                {
                    return Err("cloud worker result did not complete atomically");
                }
                let active_revision = persisted
                    .transcript_revisions
                    .iter()
                    .find(|revision| revision.revision == 1)
                    .ok_or("cloud worker revision is missing")?;
                if active_revision.source != TranscriptRevisionSource::CloudPostprocess
                    || active_revision.status != TranscriptRevisionStatus::Active
                    || active_revision.segments != persisted.transcript_segments
                {
                    return Err("cloud worker active revision is inconsistent");
                }
                let speaker_ids = persisted
                    .transcript_segments
                    .iter()
                    .filter_map(|segment| segment.speaker_id.as_deref())
                    .collect::<std::collections::HashSet<_>>();
                if speaker_ids.is_empty()
                    || persisted.speaker_profiles.len() != speaker_ids.len()
                    || persisted.speaker_turns.len() != persisted.transcript_segments.len()
                    || !persisted.transcript_segments.iter().all(|segment| {
                        !segment.text.trim().is_empty()
                            && segment.start_ms < segment.end_ms.unwrap_or_default()
                            && segment.end_ms.unwrap_or_default()
                                <= info.duration_ms.saturating_add(5_000)
                            && segment.metadata.as_ref().is_some_and(|metadata| {
                                metadata.provider_id.as_deref()
                                    == Some(format!("bailian/{model_id}").as_str())
                                    && metadata.provider_session_id.as_deref() == Some(task_id)
                                    && metadata.provider_start_ms == Some(segment.start_ms)
                                    && metadata.provider_end_ms == segment.end_ms
                            })
                    })
                {
                    return Err(match model_id {
                        "fun-asr" => "fun-asr cloud worker result is inconsistent",
                        "paraformer-v2" => "paraformer-v2 cloud worker result is inconsistent",
                        _ => "cloud worker result is inconsistent",
                    });
                }
            }
            Ok::<(), &str>(())
        }
        .await;
        let _ = std::fs::remove_dir_all(managed_dir);
        result.unwrap();
    }

    #[cfg(target_os = "windows")]
    #[tokio::test]
    #[ignore = "requires configured Bailian credentials and OPENLESS_MEETING_CLOUD_ASR_TEST_WAV"]
    async fn configured_bailian_runs_both_post_meeting_models_without_diarization() {
        let path = std::env::var_os("OPENLESS_MEETING_CLOUD_ASR_TEST_WAV")
            .map(std::path::PathBuf::from)
            .expect("OPENLESS_MEETING_CLOUD_ASR_TEST_WAV must point to a PCM WAV");
        let probe = crate::asr::meeting_audio_import::probe_pcm_wav(&path)
            .expect("cloud post-meeting test WAV must be readable");
        let managed_dir = std::env::temp_dir().join(format!(
            "meeting-cloud-asr-no-diarization-integration-{}",
            Uuid::new_v4()
        ));
        let partial_path = managed_dir.join("part-0001.wav.partial");
        let final_path = managed_dir.join("part-0001.wav");
        let store_path = managed_dir.join("meetings.json");
        let result = async {
            crate::asr::meeting_audio_import::normalize_pcm_wav(
                &probe,
                &partial_path,
                &final_path,
                &AtomicBool::new(false),
                |_| {},
            )
            .map_err(|_| "cloud no-diarization test WAV normalization failed")?;
            let source = MeetingAudioSource::from_path(&final_path)
                .map_err(|_| "normalized cloud no-diarization test WAV must be readable")?;
            let info = source
                .inspect()
                .map_err(|_| "cloud no-diarization test WAV must be valid")?;

            for model_id in [FUN_ASR_MODEL_ID, PARAFORMER_V2_MODEL_ID] {
                let persisted = run_configured_cloud_worker_fixture(
                    &store_path,
                    &final_path,
                    info.duration_ms,
                    model_id,
                    MeetingDiarizationMode::Off,
                    None,
                    None,
                    format!("meeting-cloud-worker-off-{model_id}"),
                )
                .await?;
                let state = persisted
                    .post_processing
                    .as_ref()
                    .ok_or("cloud no-diarization worker state is missing")?;
                let provider_task_id = state
                    .provider_task_id
                    .as_deref()
                    .ok_or("cloud no-diarization worker provider task id is missing")?;
                if state.status != MeetingPostProcessingStatus::Completed
                    || state.model_ref.model_id != model_id
                    || state.diarization_mode != MeetingDiarizationMode::Off
                    || persisted.active_transcript_revision != Some(1)
                    || persisted.processing_hold.is_some()
                {
                    return Err("cloud no-diarization worker result did not complete atomically");
                }
                let active_revision = persisted
                    .transcript_revisions
                    .iter()
                    .find(|revision| revision.revision == 1)
                    .ok_or("cloud no-diarization worker revision is missing")?;
                if active_revision.source != TranscriptRevisionSource::CloudPostprocess
                    || active_revision.status != TranscriptRevisionStatus::Active
                    || active_revision.segments != persisted.transcript_segments
                    || !persisted.speaker_profiles.is_empty()
                    || !persisted.speaker_turns.is_empty()
                    || persisted.transcript_segments.is_empty()
                    || !persisted.transcript_segments.iter().all(|segment| {
                        segment.speaker_id.is_none()
                            && segment.speaker_label == "未区分"
                            && !segment.text.trim().is_empty()
                            && segment.start_ms < segment.end_ms.unwrap_or_default()
                            && segment.end_ms.unwrap_or_default()
                                <= info.duration_ms.saturating_add(5_000)
                            && segment.metadata.as_ref().is_some_and(|metadata| {
                                metadata.provider_id.as_deref()
                                    == Some(format!("bailian/{model_id}").as_str())
                                    && metadata.provider_session_id.as_deref()
                                        == Some(provider_task_id)
                                    && metadata.provider_start_ms == Some(segment.start_ms)
                                    && metadata.provider_end_ms == segment.end_ms
                            })
                    })
                {
                    return Err("cloud no-diarization worker transcript is inconsistent");
                }
            }
            Ok::<(), &str>(())
        }
        .await;
        let _ = std::fs::remove_dir_all(managed_dir);
        result.unwrap();
    }

    #[cfg(target_os = "windows")]
    #[tokio::test]
    #[ignore = "requires configured Bailian credentials and explicit cloud benchmark WAV/model"]
    async fn configured_bailian_benchmarks_one_post_meeting_model() {
        let path = std::env::var_os("OPENLESS_MEETING_CLOUD_ASR_BENCH_WAV")
            .map(std::path::PathBuf::from)
            .expect("OPENLESS_MEETING_CLOUD_ASR_BENCH_WAV must point to a PCM WAV");
        let model_id = std::env::var("OPENLESS_MEETING_CLOUD_ASR_BENCH_MODEL")
            .expect("OPENLESS_MEETING_CLOUD_ASR_BENCH_MODEL must be set");
        assert!(
            matches!(model_id.as_str(), FUN_ASR_MODEL_ID | PARAFORMER_V2_MODEL_ID),
            "cloud benchmark model is not supported"
        );
        let probe = crate::asr::meeting_audio_import::probe_pcm_wav(&path)
            .expect("cloud benchmark WAV must be readable");
        let managed_dir = std::env::temp_dir().join(format!(
            "meeting-cloud-asr-benchmark-{}",
            Uuid::new_v4()
        ));
        let partial_path = managed_dir.join("part-0001.wav.partial");
        let final_path = managed_dir.join("part-0001.wav");
        let store_path = managed_dir.join("meetings.json");
        let started_at = std::time::Instant::now();
        let result = async {
            crate::asr::meeting_audio_import::normalize_pcm_wav(
                &probe,
                &partial_path,
                &final_path,
                &AtomicBool::new(false),
                |_| {},
            )
            .map_err(|_| "cloud benchmark WAV normalization failed")?;
            let source = MeetingAudioSource::from_path(&final_path)
                .map_err(|_| "normalized cloud benchmark WAV must be readable")?;
            let info = source
                .inspect()
                .map_err(|_| "cloud benchmark WAV must be valid")?;
            let persisted = run_configured_cloud_worker_fixture(
                &store_path,
                &final_path,
                info.duration_ms,
                &model_id,
                MeetingDiarizationMode::Off,
                None,
                None,
                format!("meeting-cloud-benchmark-{model_id}"),
            )
            .await?;
            let state = persisted
                .post_processing
                .as_ref()
                .ok_or("cloud benchmark state is missing")?;
            if state.status != MeetingPostProcessingStatus::Completed
                || persisted.active_transcript_revision != Some(1)
                || persisted.processing_hold.is_some()
                || persisted.transcript_segments.is_empty()
            {
                return Err("cloud benchmark result is inconsistent");
            }
            Ok::<(u64, usize), &str>((info.duration_ms, persisted.transcript_segments.len()))
        }
        .await;
        let elapsed_ms = started_at.elapsed().as_millis();
        let _ = std::fs::remove_dir_all(managed_dir);
        let (duration_ms, segment_count) = result.unwrap();
        eprintln!(
            "meeting_cloud_asr_benchmark model={model_id} duration_ms={duration_ms} elapsed_ms={elapsed_ms} segments={segment_count}"
        );
    }

    #[cfg(target_os = "windows")]
    #[tokio::test]
    #[ignore = "requires configured Bailian credentials, OPENLESS_MEETING_DIARIZATION_TEST_WAV, and an installed local diarization package"]
    async fn configured_bailian_runs_both_models_with_local_diarization() {
        let path = std::env::var_os("OPENLESS_MEETING_DIARIZATION_TEST_WAV")
            .map(std::path::PathBuf::from)
            .expect("OPENLESS_MEETING_DIARIZATION_TEST_WAV must point to a two-speaker PCM WAV");
        let probe = crate::asr::meeting_audio_import::probe_pcm_wav(&path)
            .expect("local diarization worker test WAV must be readable");
        let managed_dir = std::env::temp_dir().join(format!(
            "meeting-cloud-local-diarization-integration-{}",
            Uuid::new_v4()
        ));
        let partial_path = managed_dir.join("part-0001.wav.partial");
        let final_path = managed_dir.join("part-0001.wav");
        let store_path = managed_dir.join("meetings.json");
        let result = async {
            crate::asr::meeting_audio_import::normalize_pcm_wav(
                &probe,
                &partial_path,
                &final_path,
                &AtomicBool::new(false),
                |_| {},
            )
            .map_err(|_| "local diarization worker WAV normalization failed")?;
            let source = MeetingAudioSource::from_path(&final_path)
                .map_err(|_| "local diarization worker WAV must be readable")?;
            let info = source
                .inspect()
                .map_err(|_| "local diarization worker WAV must be valid")?;

            let local_model_id =
                crate::asr::local::speaker_diarization::DEFAULT_PACKAGE_ID.to_string();
            crate::asr::local::speaker_diarization::ensure_package_ready(&local_model_id)
                .map_err(|_| "local diarization package is not ready")?;
            for model_id in [FUN_ASR_MODEL_ID, PARAFORMER_V2_MODEL_ID] {
                let persisted = run_configured_cloud_worker_fixture(
                    &store_path,
                    &final_path,
                    info.duration_ms,
                    model_id,
                    MeetingDiarizationMode::Local,
                    Some(&local_model_id),
                    Some(2),
                    format!("meeting-cloud-local-worker-{model_id}"),
                )
                .await?;
                let state = persisted
                    .post_processing
                    .as_ref()
                    .ok_or("local diarization worker state is missing")?;
                let provider_task_id = state
                    .provider_task_id
                    .as_deref()
                    .ok_or("local diarization worker provider task id is missing")?;
                if state.status != MeetingPostProcessingStatus::Completed
                    || state.model_ref.model_id != model_id
                    || state.diarization_mode != MeetingDiarizationMode::Local
                    || persisted.active_transcript_revision != Some(1)
                    || persisted.processing_hold.is_some()
                {
                    return Err("local diarization worker result did not complete atomically");
                }
                let active_revision = persisted
                    .transcript_revisions
                    .iter()
                    .find(|revision| revision.revision == 1)
                    .ok_or("local diarization worker revision is missing")?;
                if active_revision.source != TranscriptRevisionSource::LocalPostprocess
                    || active_revision.status != TranscriptRevisionStatus::Active
                    || active_revision.segments != persisted.transcript_segments
                {
                    return Err("local diarization worker active revision is inconsistent");
                }
                let turn_speaker_ids = persisted
                    .speaker_turns
                    .iter()
                    .map(|turn| turn.speaker_id.as_str())
                    .collect::<std::collections::HashSet<_>>();
                if turn_speaker_ids.len() != 2
                    || persisted.speaker_profiles.len() != 2
                    || !persisted.speaker_turns.iter().all(|turn| {
                        turn.start_ms < turn.end_ms && turn.end_ms <= info.duration_ms
                    })
                    || persisted.transcript_segments.is_empty()
                    || !persisted
                        .transcript_segments
                        .iter()
                        .any(|segment| segment.speaker_id.is_some())
                    || !persisted.transcript_segments.iter().all(|segment| {
                        !segment.text.trim().is_empty()
                            && segment.start_ms < segment.end_ms.unwrap_or_default()
                            && segment.end_ms.unwrap_or_default()
                                <= info.duration_ms.saturating_add(5_000)
                            && segment.speaker_id.as_deref().is_none_or(|speaker_id| {
                                turn_speaker_ids.contains(speaker_id)
                            })
                            && segment.metadata.as_ref().is_some_and(|metadata| {
                                metadata.provider_id.as_deref()
                                    == Some(format!("bailian/{model_id}").as_str())
                                    && metadata.provider_session_id.as_deref()
                                        == Some(provider_task_id)
                                    && metadata.provider_start_ms == Some(segment.start_ms)
                                    && metadata.provider_end_ms == segment.end_ms
                            })
                    })
                {
                    return Err(
                        "local diarization worker speaker or timeline mapping is inconsistent",
                    );
                }
            }
            Ok::<(), &str>(())
        }
        .await;
        let _ = std::fs::remove_dir_all(managed_dir);
        result.unwrap();
    }

    #[cfg(target_os = "windows")]
    #[tokio::test]
    #[ignore = "requires configured Bailian credentials and OPENLESS_MEETING_CLOUD_ASR_TEST_WAV"]
    async fn configured_fun_asr_resumes_persisted_provider_task_after_store_reopen() {
        let path = std::env::var_os("OPENLESS_MEETING_CLOUD_ASR_TEST_WAV")
            .map(std::path::PathBuf::from)
            .expect("OPENLESS_MEETING_CLOUD_ASR_TEST_WAV must point to a PCM WAV");
        let probe = crate::asr::meeting_audio_import::probe_pcm_wav(&path)
            .expect("post-meeting resume test WAV must be readable");
        let managed_dir = std::env::temp_dir().join(format!(
            "meeting-cloud-asr-resume-integration-{}",
            Uuid::new_v4()
        ));
        let partial_path = managed_dir.join("part-0001.wav.partial");
        let final_path = managed_dir.join("part-0001.wav");
        let store_path = managed_dir.join("meetings.json");
        let result = async {
            crate::asr::meeting_audio_import::normalize_pcm_wav(
                &probe,
                &partial_path,
                &final_path,
                &AtomicBool::new(false),
                |_| {},
            )
            .map_err(|_| "post-meeting resume test WAV normalization failed")?;
            let source = MeetingAudioSource::from_path(&final_path)
                .map_err(|_| "normalized post-meeting resume test WAV must be readable")?;
            let info = source
                .inspect()
                .map_err(|_| "post-meeting resume test WAV must be valid")?;
            let client = build_post_meeting_dashscope_client(FUN_ASR_MODEL_ID)
                .map_err(|_| "post-meeting resume test client setup failed")?;
            let file_url = client
                .upload_meeting_audio(source, Arc::new(AtomicBool::new(false)))
                .await
                .map_err(|_| "post-meeting resume test upload failed")?;
            let provider_task_id = client
                .submit_async_task(
                    &file_url,
                    DashScopeAsyncRequestOptions {
                        diarization_enabled: false,
                        speaker_count: None,
                    },
                )
                .await
                .map_err(|_| "post-meeting resume test submission failed")?;

            let mut candidate = record();
            candidate.id = "meeting-cloud-worker-resume-fun-asr".to_string();
            candidate.duration_ms = Some(info.duration_ms);
            let config = candidate.post_processing_config.as_mut().unwrap();
            config.diarization_mode = MeetingDiarizationMode::Off;
            config.expected_speaker_count = None;
            prepare_post_processing_after_stop(&mut candidate, "2026-08-13T00:00:00Z")
                .map_err(|_| "post-meeting resume revision setup failed")?;
            let meeting_id = candidate.id.clone();
            let state = candidate.post_processing.as_mut().unwrap();
            state.status = MeetingPostProcessingStatus::Running;
            state.progress = Some(0.55);
            state.provider_task_id = Some(provider_task_id.clone());
            state.started_at = Some("2026-08-13T00:00:01Z".to_string());
            state.updated_at = "2026-08-13T00:00:01Z".to_string();
            let job_id = state.job_id.clone();
            MeetingStore::new_for_path(store_path.clone())
                .create(candidate)
                .map_err(|_| "post-meeting resume fixture persistence failed")?;

            let reopened = MeetingStore::new_for_path(store_path.clone());
            let resumable_jobs = reopened
                .list()
                .map_err(|_| "post-meeting resume scan failed")?
                .into_iter()
                .filter_map(resumable_post_processing_job)
                .collect::<Vec<_>>();
            if resumable_jobs != vec![(meeting_id.clone(), job_id.clone())] {
                return Err("post-meeting resume scan did not select the persisted running job");
            }

            let audio_resolver_calls = std::sync::atomic::AtomicUsize::new(0);
            let audio_path_for_id = |_id: &str| {
                audio_resolver_calls.fetch_add(1, Ordering::AcqRel);
                anyhow::bail!("resumed provider task must not reopen meeting audio")
            };
            let context = PostProcessingJobContext {
                inner: None,
                store: &reopened,
                audio_path_for_id: &audio_path_for_id,
                #[cfg(target_os = "windows")]
                local_asr: None,
            };
            run_post_processing_job_with_context(
                &context,
                &meeting_id,
                &job_id,
                Arc::new(AtomicBool::new(false)),
            )
            .await
            .map_err(|_| "post-meeting resumed worker execution failed")?;
            if audio_resolver_calls.load(Ordering::Acquire) != 0 {
                return Err("post-meeting resumed worker accessed meeting audio");
            }

            let persisted = MeetingStore::new_for_path(store_path)
                .get(&meeting_id)
                .map_err(|_| "post-meeting resumed result reload failed")?
                .ok_or("post-meeting resumed result was not persisted")?;
            let state = persisted
                .post_processing
                .as_ref()
                .ok_or("post-meeting resumed state is missing")?;
            let active_revision = persisted
                .transcript_revisions
                .iter()
                .find(|revision| revision.revision == 1)
                .ok_or("post-meeting resumed revision is missing")?;
            if state.status != MeetingPostProcessingStatus::Completed
                || state.provider_task_id.as_deref() != Some(provider_task_id.as_str())
                || state.model_ref.model_id != FUN_ASR_MODEL_ID
                || persisted.active_transcript_revision != Some(1)
                || persisted.processing_hold.is_some()
                || active_revision.source != TranscriptRevisionSource::CloudPostprocess
                || active_revision.status != TranscriptRevisionStatus::Active
                || active_revision.segments != persisted.transcript_segments
                || persisted.transcript_segments.is_empty()
                || !persisted.transcript_segments.iter().all(|segment| {
                    segment.speaker_id.is_none()
                        && segment.metadata.as_ref().is_some_and(|metadata| {
                            metadata.provider_id.as_deref() == Some("bailian/fun-asr")
                                && metadata.provider_session_id.as_deref()
                                    == Some(provider_task_id.as_str())
                        })
                })
            {
                return Err("post-meeting resumed worker result is inconsistent");
            }
            Ok::<(), &str>(())
        }
        .await;
        let _ = std::fs::remove_dir_all(managed_dir);
        result.unwrap();
    }

    fn record() -> MeetingRecord {
        MeetingRecord {
            id: "meeting-1".to_string(),
            title: "会议记录".to_string(),
            status: MeetingStatus::Completed,
            started_at: "2026-08-12T09:00:00Z".to_string(),
            ended_at: Some("2026-08-12T10:00:00Z".to_string()),
            duration_ms: Some(3_600_000),
            transcript_segments: vec![TranscriptSegment {
                id: "seg-1".to_string(),
                speaker_id: None,
                speaker_label: "未区分".to_string(),
                start_ms: 0,
                end_ms: Some(1_000),
                text: "开始会议".to_string(),
                source: TranscriptSegmentSource::RealtimeAsr,
                metadata: None,
            }],
            summary: MeetingSummary::default(),
            organized_draft: None,
            organized_draft_state: None,
            audio: MeetingAudioMeta {
                state: MeetingAudioState::Retained,
                retained: true,
                path: None,
            },
            realtime_asr: None,
            post_processing_config: Some(MeetingPostProcessingConfig {
                diarization_mode: MeetingDiarizationMode::Off,
                realtime_provider_id: "bailian".to_string(),
                realtime_model_id: Some("fun-asr-realtime".to_string()),
                post_meeting_asr_model_ref: MeetingAsrModelRef {
                    provider_id: "bailian".to_string(),
                    model_id: "fun-asr".to_string(),
                },
                resolved_asr_runtime_kind: MeetingAsrRuntimeKind::Cloud,
                local_diarization_model_id: None,
                expected_speaker_count: None,
                model_version: None,
                processing_revision: 1,
            }),
            post_processing: None,
            import_config: None,
            import_state: None,
            transcript_revisions: Vec::new(),
            active_transcript_revision: None,
            speaker_profiles: vec![SpeakerProfile {
                id: "speaker-0".to_string(),
                provider_speaker_id: Some("0".to_string()),
                display_name: "发言人 1".to_string(),
                manually_named: false,
            }],
            speaker_turns: Vec::new(),
            processing_hold: None,
            created_at: "2026-08-12T09:00:00Z".to_string(),
            updated_at: "2026-08-12T10:00:00Z".to_string(),
        }
    }

    #[test]
    fn registry_only_lists_fun_asr_and_paraformer_v2() {
        let models = list_post_meeting_asr_models();
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].model_id, "fun-asr");
        assert!(models[0].is_default);
        assert_eq!(models[1].model_id, "paraformer-v2");
        assert!(!models[1].is_default);
        assert!(models
            .iter()
            .all(|model| model.runtime_kind == MeetingAsrRuntimeKind::Cloud));
    }

    #[test]
    fn registry_rejects_unregistered_models() {
        let result = resolve_post_meeting_asr_model(&MeetingAsrModelRef {
            provider_id: "bailian".to_string(),
            model_id: "qwen-asr".to_string(),
        });
        assert_eq!(
            result,
            Err("unsupported post-meeting ASR model: bailian/qwen-asr".to_string())
        );
    }

    #[test]
    fn default_config_uses_fun_asr_and_diarization_off() {
        let prefs = UserPreferences::default();
        let config = resolve_initial_post_processing_config(
            &prefs,
            None,
            "bailian",
            Some("fun-asr-realtime".to_string()),
        )
        .unwrap();
        assert_eq!(config.post_meeting_asr_model_ref.model_id, "fun-asr");
        assert_eq!(config.diarization_mode, MeetingDiarizationMode::Off);
        assert_eq!(
            config.resolved_asr_runtime_kind,
            MeetingAsrRuntimeKind::Cloud
        );
        assert_eq!(config.processing_revision, 1);
    }

    #[test]
    fn options_override_settings_without_accepting_runtime_kind() {
        let prefs = UserPreferences::default();
        let options = StartMeetingRecordingOptions {
            post_meeting_asr_model_ref: Some(MeetingAsrModelRef {
                provider_id: "bailian".to_string(),
                model_id: "paraformer-v2".to_string(),
            }),
            diarization_mode: Some(MeetingDiarizationMode::Cloud),
            expected_speaker_count: Some(4),
            ..StartMeetingRecordingOptions::default()
        };
        let config =
            resolve_initial_post_processing_config(&prefs, Some(&options), "volcengine", None)
                .unwrap();
        assert_eq!(config.post_meeting_asr_model_ref.model_id, "paraformer-v2");
        assert_eq!(
            config.resolved_asr_runtime_kind,
            MeetingAsrRuntimeKind::Cloud
        );
        assert_eq!(config.diarization_mode, MeetingDiarizationMode::Cloud);
        assert_eq!(config.expected_speaker_count, Some(4));
    }

    #[test]
    fn initial_config_is_an_owned_snapshot_of_the_selected_meeting_options() {
        let mut prefs = UserPreferences::default();
        let options = StartMeetingRecordingOptions {
            post_meeting_asr_model_ref: Some(MeetingAsrModelRef {
                provider_id: "bailian".to_string(),
                model_id: "paraformer-v2".to_string(),
            }),
            diarization_mode: Some(MeetingDiarizationMode::Cloud),
            expected_speaker_count: Some(6),
            ..StartMeetingRecordingOptions::default()
        };
        let config = resolve_initial_post_processing_config(
            &prefs,
            Some(&options),
            "bailian",
            Some("fun-asr-realtime".to_string()),
        )
        .unwrap();

        prefs.post_meeting_asr.model_id = "fun-asr".to_string();
        prefs.post_meeting_asr.diarization.mode = MeetingDiarizationMode::Off;

        assert_eq!(config.post_meeting_asr_model_ref.model_id, "paraformer-v2");
        assert_eq!(config.diarization_mode, MeetingDiarizationMode::Cloud);
        assert_eq!(config.expected_speaker_count, Some(6));
        assert_eq!(config.realtime_model_id.as_deref(), Some("fun-asr-realtime"));
    }

    #[test]
    fn import_route_rejects_forged_or_stale_runtime_snapshots_before_dispatch() {
        let mut candidate = record();
        prepare_post_processing_after_stop(&mut candidate, "2026-08-13T01:00:00Z").unwrap();
        let state = candidate.post_processing.as_ref().unwrap().clone();
        let import_config = MeetingImportConfig {
            source_file_name: "meeting.wav".to_string(),
            source_format: "wav".to_string(),
            asr_model_ref: state.model_ref.clone(),
            resolved_asr_runtime_kind: MeetingAsrRuntimeKind::Cloud,
            diarization_mode: MeetingDiarizationMode::Off,
            local_diarization_model_id: None,
            expected_speaker_count: None,
            generate_summary: false,
            processing_revision: state.processing_revision,
        };

        assert_eq!(
            validate_import_post_processing_route(&state, &import_config).unwrap(),
            "fun-asr"
        );

        let mut forged_state = state.clone();
        forged_state.resolved_runtime_kind = MeetingAsrRuntimeKind::Local;
        assert!(validate_import_post_processing_route(&forged_state, &import_config)
            .unwrap_err()
            .contains("meetingAsrRouteMismatch"));

        let mut forged_config = import_config.clone();
        forged_config.resolved_asr_runtime_kind = MeetingAsrRuntimeKind::Local;
        assert!(validate_import_post_processing_route(&state, &forged_config)
            .unwrap_err()
            .contains("meetingAsrRouteMismatch"));

        let mut stale_config = import_config;
        stale_config.asr_model_ref.model_id = "paraformer-v2".to_string();
        assert!(validate_import_post_processing_route(&state, &stale_config)
            .unwrap_err()
            .contains("meetingAsrRouteMismatch"));
    }

    #[test]
    fn prepare_job_creates_realtime_revision_and_processing_hold() {
        let mut record = record();
        assert!(prepare_post_processing_after_stop(&mut record, "2026-08-12T10:00:01Z").unwrap());
        assert_eq!(record.active_transcript_revision, Some(0));
        assert_eq!(record.transcript_revisions.len(), 1);
        assert_eq!(
            record.transcript_revisions[0].source,
            TranscriptRevisionSource::Realtime
        );
        let state = record.post_processing.as_ref().unwrap();
        assert_eq!(state.status, MeetingPostProcessingStatus::Pending);
        assert_eq!(state.attempt, 1);
        assert_eq!(state.model_ref.model_id, "fun-asr");
        assert_eq!(state.resolved_runtime_kind, MeetingAsrRuntimeKind::Cloud);
        assert_eq!(state.diarization_mode, MeetingDiarizationMode::Off);
        assert_eq!(state.processing_revision, 1);
        assert_eq!(
            record.processing_hold.as_ref().unwrap().job_id,
            state.job_id
        );
    }

    #[test]
    fn retranscription_transition_preserves_active_content_until_new_revision_succeeds() {
        let mut candidate = record();
        prepare_post_processing_after_stop(&mut candidate, "2026-08-12T10:00:01Z").unwrap();
        let old_segments = candidate.transcript_segments.clone();
        let old_job_id = candidate.post_processing.as_ref().unwrap().job_id.clone();
        candidate.post_processing.as_mut().unwrap().status = MeetingPostProcessingStatus::Completed;
        candidate.processing_hold = None;
        let expected_job = Some((old_job_id, MeetingPostProcessingStatus::Completed));
        let mut next_config = candidate.post_processing_config.clone().unwrap();
        next_config.processing_revision = 2;
        let mut next_state = candidate.post_processing.clone().unwrap();
        next_state.status = MeetingPostProcessingStatus::Pending;
        next_state.job_id = "job-retranscribe".to_string();
        next_state.processing_revision = 2;
        next_state.provider_task_id = None;
        next_state.progress = Some(0.0);
        next_state.attempt = 2;
        next_state.error_code = None;
        next_state.error_message = None;

        assert!(apply_retranscription_transition(
            &mut candidate,
            expected_job.as_ref(),
            Some(0),
            next_config,
            next_state,
            "2026-08-12T10:05:00Z",
        ));
        assert_eq!(candidate.active_transcript_revision, Some(0));
        assert_eq!(candidate.transcript_segments, old_segments);
        assert_eq!(
            candidate.post_processing.as_ref().unwrap().job_id,
            "job-retranscribe"
        );
        assert_eq!(
            candidate
                .post_processing_config
                .as_ref()
                .unwrap()
                .processing_revision,
            2
        );
        assert_eq!(
            candidate.processing_hold.as_ref().unwrap().job_id,
            "job-retranscribe"
        );
    }

    #[test]
    fn legacy_meeting_without_revision_starts_at_revision_one() {
        let mut candidate = record();
        candidate.post_processing_config = None;
        candidate.post_processing = None;
        candidate.transcript_revisions.clear();
        candidate.active_transcript_revision = None;

        assert_eq!(next_post_processing_revision(&candidate), 1);
    }

    #[tokio::test]
    async fn silent_audio_fails_before_remote_asr_submission() {
        let dir = std::env::temp_dir().join(format!(
            "meeting-post-processing-silent-{}",
            Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let audio_path = dir.join("audio.wav");
        let store_path = dir.join("meetings.json");
        std::fs::write(&audio_path, encode_wav_16k_mono(&vec![0; 32_000])).unwrap();

        let mut candidate = record();
        candidate.id = "meeting-silent".to_string();
        candidate.duration_ms = Some(2_000);
        prepare_post_processing_after_stop(&mut candidate, "2026-08-12T10:00:01Z").unwrap();
        let old_segments = candidate.transcript_segments.clone();
        let job_id = candidate.post_processing.as_ref().unwrap().job_id.clone();
        let store = MeetingStore::new_for_path(store_path);
        store.create(candidate).unwrap();

        run_post_processing_job_for_test(
            &store,
            "meeting-silent",
            &job_id,
            audio_path,
        )
        .await
        .unwrap();

        let persisted = store.get("meeting-silent").unwrap().unwrap();
        let state = persisted.post_processing.as_ref().unwrap();
        assert_eq!(state.status, MeetingPostProcessingStatus::Failed);
        assert_eq!(state.error_code.as_deref(), Some("meetingAudioNoSpeech"));
        assert!(state.provider_task_id.is_none());
        assert_eq!(persisted.active_transcript_revision, Some(0));
        assert_eq!(persisted.transcript_segments, old_segments);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn pending_job_without_provider_task_id_can_submit() {
        let mut record = record();
        prepare_post_processing_after_stop(&mut record, "2026-08-12T10:00:01Z").unwrap();
        let state = record.post_processing.as_ref().unwrap();

        assert_eq!(resume_provider_task_id(state), Ok(None));
    }

    #[test]
    fn running_job_with_provider_task_id_resumes_polling() {
        let mut record = record();
        prepare_post_processing_after_stop(&mut record, "2026-08-12T10:00:01Z").unwrap();
        let state = record.post_processing.as_mut().unwrap();
        state.status = MeetingPostProcessingStatus::Running;
        state.provider_task_id = Some("task-existing".to_string());

        assert_eq!(
            resume_provider_task_id(state),
            Ok(Some("task-existing".to_string()))
        );
    }

    #[test]
    fn submitted_job_without_provider_task_id_rejects_automatic_resubmission() {
        for status in [
            MeetingPostProcessingStatus::Running,
            MeetingPostProcessingStatus::LocalAnalyzing,
            MeetingPostProcessingStatus::Applying,
        ] {
            let mut record = record();
            prepare_post_processing_after_stop(&mut record, "2026-08-12T10:00:01Z").unwrap();
            let state = record.post_processing.as_mut().unwrap();
            state.status = status;
            state.provider_task_id = None;

            assert!(resume_provider_task_id(state)
                .unwrap_err()
                .starts_with("postMeetingAsrSubmissionOutcomeUnknown:"));
        }
    }

    #[test]
    fn retry_expected_speaker_count_override_distinguishes_auto_and_fixed() {
        let auto: RetryMeetingPostProcessingOptions =
            serde_json::from_str(r#"{"expectedSpeakerCount":{"mode":"auto"}}"#).unwrap();
        assert_eq!(
            auto.expected_speaker_count,
            Some(ExpectedSpeakerCountOverride::Auto)
        );

        let fixed: RetryMeetingPostProcessingOptions =
            serde_json::from_str(r#"{"expectedSpeakerCount":{"mode":"fixed","count":4}}"#).unwrap();
        assert_eq!(
            fixed.expected_speaker_count,
            Some(ExpectedSpeakerCountOverride::Fixed { count: 4 })
        );
    }

    #[test]
    fn staging_revision_activation_is_atomic_and_idempotent() {
        let mut record = record();
        prepare_post_processing_after_stop(&mut record, "2026-08-12T10:00:01Z").unwrap();
        let segments = vec![TranscriptSegment {
            id: "post-1".to_string(),
            speaker_id: Some("speaker-0".to_string()),
            speaker_label: "发言人 1".to_string(),
            start_ms: 0,
            end_ms: Some(900),
            text: "整理后的原文".to_string(),
            source: TranscriptSegmentSource::RetranscribedAsr,
            metadata: None,
        }];
        stage_transcript_revision(
            &mut record,
            1,
            TranscriptRevisionSource::CloudPostprocess,
            segments.clone(),
            "2026-08-12T10:01:00Z",
        )
        .unwrap();
        assert_eq!(record.transcript_segments[0].text, "开始会议");
        activate_staging_transcript_revision(&mut record, 1).unwrap();
        activate_staging_transcript_revision(&mut record, 1).unwrap();
        assert_eq!(record.active_transcript_revision, Some(1));
        assert_eq!(record.transcript_segments, segments);
    }

    #[test]
    fn invalid_staging_revision_does_not_replace_active_transcript() {
        let mut record = record();
        prepare_post_processing_after_stop(&mut record, "2026-08-12T10:00:01Z").unwrap();
        let original = record.transcript_segments.clone();
        let result = stage_transcript_revision(
            &mut record,
            1,
            TranscriptRevisionSource::CloudPostprocess,
            Vec::new(),
            "2026-08-12T10:01:00Z",
        );
        assert_eq!(result, Err("transcript revision is empty".to_string()));
        assert_eq!(record.transcript_segments, original);
        assert_eq!(record.active_transcript_revision, Some(0));
    }

    #[test]
    fn failed_worker_write_ignores_cancelled_or_replaced_job() {
        let mut cancelled = record();
        prepare_post_processing_after_stop(&mut cancelled, "2026-08-12T10:00:01Z").unwrap();
        let cancelled_job_id = cancelled.post_processing.as_ref().unwrap().job_id.clone();
        assert!(
            cancel_post_processing_for_deletion(&mut cancelled, "2026-08-12T10:00:02Z",).is_some()
        );
        assert!(!fail_active_post_processing_job(
            &mut cancelled,
            &cancelled_job_id,
            "postMeetingAsrTaskFailed: late failure",
            "2026-08-12T10:00:03Z",
        ));
        assert_eq!(
            cancelled.post_processing.as_ref().unwrap().status,
            MeetingPostProcessingStatus::Cancelled
        );

        let mut retried = record();
        prepare_post_processing_after_stop(&mut retried, "2026-08-12T10:00:01Z").unwrap();
        let old_job_id = retried.post_processing.as_ref().unwrap().job_id.clone();
        retried.post_processing.as_mut().unwrap().job_id = "job-new".to_string();
        assert!(!fail_active_post_processing_job(
            &mut retried,
            &old_job_id,
            "postMeetingAsrTaskFailed: late failure",
            "2026-08-12T10:00:03Z",
        ));
        assert_eq!(
            retried.post_processing.as_ref().unwrap().status,
            MeetingPostProcessingStatus::Pending
        );
    }

    #[test]
    fn cancelling_import_post_processing_updates_import_state_and_releases_hold() {
        let mut record = record();
        prepare_post_processing_after_stop(&mut record, "2026-08-13T00:00:01Z").unwrap();
        let job_id = record.post_processing.as_ref().unwrap().job_id.clone();
        record.import_state = Some(crate::types::MeetingImportState {
            status: MeetingImportStatus::Transcribing,
            import_job_id: "import-job".to_string(),
            progress: Some(0.6),
            attempt: 1,
            error_code: Some("oldError".to_string()),
            error_message: Some("old message".to_string()),
            created_at: "2026-08-13T00:00:00Z".to_string(),
            updated_at: "2026-08-13T00:00:01Z".to_string(),
            completed_at: None,
        });

        assert!(apply_cancel_transition(
            &mut record,
            &job_id,
            "2026-08-13T00:00:02Z"
        ));
        let import_state = record.import_state.as_ref().unwrap();
        assert_eq!(import_state.status, MeetingImportStatus::Cancelled);
        assert_eq!(import_state.progress, None);
        assert_eq!(import_state.error_code, None);
        assert_eq!(import_state.error_message, None);
        assert_eq!(
            import_state.completed_at.as_deref(),
            Some("2026-08-13T00:00:02Z")
        );
        assert!(record.processing_hold.is_none());
    }

    #[tokio::test]
    async fn deletion_stop_signals_and_waits_for_post_processing_worker_unregister() {
        let mut record = record();
        prepare_post_processing_after_stop(&mut record, "2026-08-13T00:00:01Z").unwrap();
        let job_id = record.post_processing.as_ref().unwrap().job_id.clone();
        let cancelled = register_post_processing_job(&job_id).unwrap();

        let request = request_post_processing_stop_for_deletion(&record).unwrap();
        assert_eq!(request.job_id, job_id);
        assert!(cancelled.load(Ordering::Acquire));

        let job_id_for_cleanup = job_id.clone();
        let cleanup = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            POST_PROCESSING_CANCEL_FLAGS
                .lock()
                .remove(&job_id_for_cleanup);
        });
        wait_for_post_processing_worker_exit(&request).await.unwrap();
        cleanup.await.unwrap();
        assert!(!POST_PROCESSING_CANCEL_FLAGS.lock().contains_key(&job_id));
    }

    fn cloud_transcript(with_speakers: bool) -> DashScopeAsyncTranscript {
        DashScopeAsyncTranscript {
            sentences: vec![
                DashScopeAsyncSentence {
                    begin_time_ms: 100,
                    end_time_ms: 900,
                    text: "第一位发言".to_string(),
                    sentence_id: Some("1".to_string()),
                    speaker_id: with_speakers.then(|| "7".to_string()),
                },
                DashScopeAsyncSentence {
                    begin_time_ms: 900,
                    end_time_ms: 1_700,
                    text: "第二位发言".to_string(),
                    sentence_id: Some("2".to_string()),
                    speaker_id: with_speakers.then(|| "9".to_string()),
                },
                DashScopeAsyncSentence {
                    begin_time_ms: 1_700,
                    end_time_ms: 2_100,
                    text: "第一位补充".to_string(),
                    sentence_id: Some("3".to_string()),
                    speaker_id: with_speakers.then(|| "7".to_string()),
                },
            ],
        }
    }

    #[test]
    fn cloud_result_transition_activates_revision_for_both_models() {
        for model_id in [FUN_ASR_MODEL_ID, PARAFORMER_V2_MODEL_ID] {
            let mut candidate = record();
            candidate
                .post_processing_config
                .as_mut()
                .unwrap()
                .post_meeting_asr_model_ref
                .model_id = model_id.to_string();
            prepare_post_processing_after_stop(&mut candidate, "2026-08-12T10:00:01Z").unwrap();
            let state = candidate.post_processing.as_mut().unwrap();
            state.status = MeetingPostProcessingStatus::Applying;
            state.provider_task_id = Some("task-1".to_string());
            state.model_ref.model_id = model_id.to_string();
            let job_id = state.job_id.clone();

            assert!(apply_cloud_result_transition(
                &mut candidate,
                &job_id,
                model_id,
                "task-1",
                &cloud_transcript(false),
                "2026-08-12T10:02:00Z",
            )
            .unwrap());
            assert_eq!(candidate.active_transcript_revision, Some(1));
            assert_eq!(candidate.transcript_segments.len(), 3);
            assert_eq!(candidate.transcript_segments[0].start_ms, 100);
            assert_eq!(candidate.transcript_segments[0].end_ms, Some(900));
            assert_eq!(
                candidate.transcript_segments[0]
                    .metadata
                    .as_ref()
                    .unwrap()
                    .provider_id
                    .as_deref(),
                Some(format!("bailian/{model_id}").as_str())
            );
            assert_eq!(
                candidate.post_processing.as_ref().unwrap().status,
                MeetingPostProcessingStatus::Completed
            );
            assert!(candidate.processing_hold.is_none());
        }
    }

    #[test]
    fn auto_summary_waits_for_completed_post_processing_and_uses_active_revision() {
        let mut candidate = record();
        prepare_post_processing_after_stop(&mut candidate, "2026-08-12T10:00:01Z").unwrap();
        let state = candidate.post_processing.as_mut().unwrap();
        state.status = MeetingPostProcessingStatus::Applying;
        state.provider_task_id = Some("task-1".to_string());
        let job_id = state.job_id.clone();

        assert_eq!(candidate.active_transcript_revision, Some(0));
        assert_eq!(candidate.transcript_segments[0].text, "开始会议");
        assert_eq!(
            super::super::meeting_summary::validate_summary_mode_for_test(
                &candidate,
                super::super::meeting_summary::MeetingSummaryMode::Generate,
            ),
            Err("meeting post-processing is not completed".to_string())
        );

        assert!(apply_cloud_result_transition(
            &mut candidate,
            &job_id,
            FUN_ASR_MODEL_ID,
            "task-1",
            &cloud_transcript(false),
            "2026-08-12T10:02:00Z",
        )
        .unwrap());

        assert_eq!(candidate.active_transcript_revision, Some(1));
        assert_eq!(candidate.transcript_segments[0].text, "第一位发言");
        assert_eq!(
            candidate.post_processing.as_ref().unwrap().status,
            MeetingPostProcessingStatus::Completed
        );
        assert_eq!(
            super::super::meeting_summary::validate_summary_mode_for_test(
                &candidate,
                super::super::meeting_summary::MeetingSummaryMode::Generate,
            ),
            Ok(())
        );

        super::super::meeting_summary::prepare_summary_record(&mut candidate).unwrap();
        assert_eq!(candidate.status, MeetingStatus::Summarizing);
        assert_eq!(candidate.active_transcript_revision, Some(1));
        assert_eq!(candidate.transcript_segments[0].text, "第一位发言");
    }

    #[test]
    fn cloud_diarization_maps_provider_ids_to_stable_profiles_and_turns() {
        let (segments, profiles, turns) = normalize_cloud_transcript(
            FUN_ASR_MODEL_ID,
            "task-1",
            MeetingDiarizationMode::Cloud,
            &cloud_transcript(true),
        )
        .unwrap();

        assert_eq!(profiles.len(), 2);
        assert_eq!(profiles[0].id, "speaker-0");
        assert_eq!(profiles[0].provider_speaker_id.as_deref(), Some("7"));
        assert_eq!(profiles[1].provider_speaker_id.as_deref(), Some("9"));
        assert_eq!(segments[0].speaker_id.as_deref(), Some("speaker-0"));
        assert_eq!(segments[1].speaker_id.as_deref(), Some("speaker-1"));
        assert_eq!(segments[2].speaker_id.as_deref(), Some("speaker-0"));
        assert_eq!(turns.len(), 3);
        assert_eq!(turns[2].speaker_id, "speaker-0");
    }

    #[test]
    fn disabled_diarization_does_not_attach_provider_speaker_ids() {
        let (segments, profiles, turns) = normalize_cloud_transcript(
            PARAFORMER_V2_MODEL_ID,
            "task-2",
            MeetingDiarizationMode::Off,
            &cloud_transcript(true),
        )
        .unwrap();
        assert!(profiles.is_empty());
        assert!(turns.is_empty());
        assert!(segments.iter().all(|segment| segment.speaker_id.is_none()));
        assert!(segments
            .iter()
            .all(|segment| segment.speaker_label == "未区分"));
    }

    fn local_turn(speaker_id: &str, start_ms: u64, end_ms: u64) -> SpeakerTurn {
        SpeakerTurn {
            speaker_id: speaker_id.to_string(),
            start_ms,
            end_ms,
            confidence: None,
            overlapping: false,
        }
    }

    fn local_alignment_segments() -> Vec<TranscriptSegment> {
        normalize_cloud_transcript(
            FUN_ASR_MODEL_ID,
            "task-local",
            MeetingDiarizationMode::Local,
            &cloud_transcript(false),
        )
        .unwrap()
        .0
    }

    #[test]
    fn local_alignment_assigns_a_single_speaker_without_review() {
        let (segments, profiles) = align_local_speakers(
            local_alignment_segments(),
            &[local_turn("speaker-0", 0, 2_200)],
        )
        .unwrap();

        assert_eq!(profiles.len(), 1);
        assert!(segments
            .iter()
            .all(|segment| segment.speaker_id.as_deref() == Some("speaker-0")));
        assert!(segments.iter().all(|segment| {
            let metadata = segment.metadata.as_ref().unwrap();
            !metadata.needs_review && !metadata.overlapping
        }));
    }

    #[test]
    fn local_alignment_marks_low_coverage_and_missing_timestamps_for_review() {
        let mut segments = local_alignment_segments();
        segments[1].end_ms = None;
        let (segments, _) = align_local_speakers(
            segments,
            &[
                local_turn("speaker-0", 100, 400),
                local_turn("speaker-1", 1_700, 2_100),
            ],
        )
        .unwrap();

        assert_eq!(segments[0].speaker_id.as_deref(), Some("speaker-0"));
        assert!(segments[0].metadata.as_ref().unwrap().needs_review);
        assert!(segments[1].speaker_id.is_none());
        assert_eq!(segments[1].speaker_label, "未确认");
        assert!(segments[1].metadata.as_ref().unwrap().needs_review);
    }

    #[test]
    fn local_alignment_keeps_cross_speaker_sentence_whole_and_marks_review() {
        let original_text = local_alignment_segments()[0].text.clone();
        let (segments, profiles) = align_local_speakers(
            vec![local_alignment_segments().remove(0)],
            &[
                local_turn("speaker-0", 100, 600),
                local_turn("speaker-1", 600, 900),
            ],
        )
        .unwrap();

        assert_eq!(segments.len(), 1);
        assert_eq!(segments[0].text, original_text);
        assert_eq!(segments[0].speaker_id.as_deref(), Some("speaker-0"));
        assert!(segments[0].metadata.as_ref().unwrap().needs_review);
        assert_eq!(profiles.len(), 2);
    }

    #[test]
    fn local_alignment_preserves_overlap_marker() {
        let mut primary = local_turn("speaker-0", 100, 900);
        primary.overlapping = true;
        let mut secondary = local_turn("speaker-1", 500, 800);
        secondary.overlapping = true;
        let (segments, _) = align_local_speakers(
            vec![local_alignment_segments().remove(0)],
            &[primary, secondary],
        )
        .unwrap();

        let metadata = segments[0].metadata.as_ref().unwrap();
        assert!(metadata.needs_review);
        assert!(metadata.overlapping);
    }

    #[test]
    fn local_alignment_merges_duplicate_same_speaker_coverage() {
        assert_eq!(
            merged_interval_duration(vec![(100, 700), (300, 900), (900, 1_000)]),
            900
        );
    }

    #[test]
    fn local_result_transition_activates_only_the_current_job_atomically() {
        let mut candidate = record();
        let config = candidate.post_processing_config.as_mut().unwrap();
        config.diarization_mode = MeetingDiarizationMode::Local;
        config.local_diarization_model_id = Some("local-speaker-model".to_string());
        prepare_post_processing_after_stop(&mut candidate, "2026-08-12T10:00:01Z").unwrap();
        let state = candidate.post_processing.as_mut().unwrap();
        state.status = MeetingPostProcessingStatus::Applying;
        state.provider_task_id = Some("task-local".to_string());
        let job_id = state.job_id.clone();
        let output = LocalDiarizationOutput {
            turns: vec![local_turn("speaker-0", 0, 2_200)],
            detected_speaker_count: 1,
        };

        assert!(!apply_local_result_transition(
            &mut candidate,
            "stale-job",
            FUN_ASR_MODEL_ID,
            "task-local",
            "local-speaker-model",
            &cloud_transcript(false),
            &output,
            "2026-08-12T10:02:00Z",
        )
        .unwrap());
        assert_eq!(candidate.active_transcript_revision, Some(0));

        assert!(apply_local_result_transition(
            &mut candidate,
            &job_id,
            FUN_ASR_MODEL_ID,
            "task-local",
            "local-speaker-model",
            &cloud_transcript(false),
            &output,
            "2026-08-12T10:02:00Z",
        )
        .unwrap());
        assert_eq!(candidate.active_transcript_revision, Some(1));
        assert_eq!(
            candidate.transcript_revisions.last().unwrap().source,
            TranscriptRevisionSource::LocalPostprocess
        );
        assert_eq!(
            candidate.post_processing.as_ref().unwrap().status,
            MeetingPostProcessingStatus::Completed
        );
        assert!(candidate.processing_hold.is_none());
    }

    #[test]
    fn cloud_result_rejects_stale_job_or_missing_speaker_id() {
        let mut candidate = record();
        candidate
            .post_processing_config
            .as_mut()
            .unwrap()
            .diarization_mode = MeetingDiarizationMode::Cloud;
        prepare_post_processing_after_stop(&mut candidate, "2026-08-12T10:00:01Z").unwrap();
        let state = candidate.post_processing.as_mut().unwrap();
        state.status = MeetingPostProcessingStatus::Applying;
        state.provider_task_id = Some("task-1".to_string());
        let job_id = state.job_id.clone();

        assert!(!apply_cloud_result_transition(
            &mut candidate,
            "stale-job",
            FUN_ASR_MODEL_ID,
            "task-1",
            &cloud_transcript(true),
            "2026-08-12T10:02:00Z",
        )
        .unwrap());
        assert_eq!(candidate.active_transcript_revision, Some(0));
        let error = apply_cloud_result_transition(
            &mut candidate,
            &job_id,
            FUN_ASR_MODEL_ID,
            "task-1",
            &cloud_transcript(false),
            "2026-08-12T10:02:00Z",
        )
        .unwrap_err();
        assert!(error.contains("speaker_id"));
        assert_eq!(candidate.active_transcript_revision, Some(0));
    }

    #[test]
    fn cloud_speaker_count_one_is_not_sent_as_invalid_provider_hint() {
        let mut candidate = record();
        prepare_post_processing_after_stop(&mut candidate, "2026-08-12T10:00:01Z").unwrap();
        let state = candidate.post_processing.as_mut().unwrap();
        state.diarization_mode = MeetingDiarizationMode::Cloud;
        state.expected_speaker_count = Some(1);
        let options = cloud_diarization_options(state).unwrap();
        assert!(options.diarization_enabled);
        assert_eq!(options.speaker_count, None);
    }

    #[test]
    fn retry_transition_allows_only_one_replacement_of_the_same_job() {
        let mut candidate = record();
        prepare_post_processing_after_stop(&mut candidate, "2026-08-12T10:00:01Z").unwrap();
        let original_job_id = candidate.post_processing.as_ref().unwrap().job_id.clone();
        candidate.post_processing.as_mut().unwrap().status = MeetingPostProcessingStatus::Failed;
        stage_transcript_revision(
            &mut candidate,
            1,
            TranscriptRevisionSource::CloudPostprocess,
            vec![TranscriptSegment {
                id: "stale-retry-segment".to_string(),
                speaker_id: None,
                speaker_label: "未区分".to_string(),
                start_ms: 0,
                end_ms: Some(1_000),
                text: "旧任务暂存结果".to_string(),
                source: TranscriptSegmentSource::RetranscribedAsr,
                metadata: None,
            }],
            "2026-08-12T10:00:02Z",
        )
        .unwrap();
        let mut next_config = candidate.post_processing_config.clone().unwrap();
        next_config.post_meeting_asr_model_ref.model_id = "paraformer-v2".to_string();
        next_config.processing_revision = 2;
        let next_state = MeetingPostProcessingState {
            status: MeetingPostProcessingStatus::Pending,
            job_id: "job-new".to_string(),
            model_ref: next_config.post_meeting_asr_model_ref.clone(),
            resolved_runtime_kind: MeetingAsrRuntimeKind::Cloud,
            diarization_mode: MeetingDiarizationMode::Off,
            expected_speaker_count: None,
            processing_revision: 2,
            provider_task_id: None,
            progress: Some(0.0),
            attempt: 2,
            error_code: None,
            error_message: None,
            created_at: "2026-08-12T10:01:00Z".to_string(),
            updated_at: "2026-08-12T10:01:00Z".to_string(),
            started_at: None,
            completed_at: None,
        };

        assert!(apply_retry_transition(
            &mut candidate,
            &original_job_id,
            MeetingPostProcessingStatus::Failed,
            next_config.clone(),
            next_state.clone(),
            "2026-08-12T10:01:00Z",
        ));
        assert!(!apply_retry_transition(
            &mut candidate,
            &original_job_id,
            MeetingPostProcessingStatus::Failed,
            next_config,
            next_state,
            "2026-08-12T10:01:01Z",
        ));
        let state = candidate.post_processing.as_ref().unwrap();
        assert_eq!(state.job_id, "job-new");
        assert_eq!(state.model_ref.model_id, "paraformer-v2");
        assert_eq!(state.attempt, 2);
        assert_eq!(state.processing_revision, 2);
        assert_eq!(candidate.active_transcript_revision, Some(0));
        assert_eq!(candidate.post_processing_config.as_ref().unwrap().processing_revision, 2);
        assert_eq!(
            candidate
                .post_processing_config
                .as_ref()
                .unwrap()
                .post_meeting_asr_model_ref
                .model_id,
            "paraformer-v2"
        );
        assert_eq!(
            candidate
                .transcript_revisions
                .iter()
                .find(|revision| revision.revision == 1)
                .unwrap()
                .status,
            TranscriptRevisionStatus::Rejected
        );
        assert_eq!(
            candidate.processing_hold.as_ref().unwrap().job_id,
            "job-new"
        );
    }

    #[test]
    fn retry_preserves_manual_speaker_names_until_new_revision_is_activated() {
        let mut candidate = record();
        candidate
            .post_processing_config
            .as_mut()
            .unwrap()
            .diarization_mode = MeetingDiarizationMode::Cloud;
        candidate.transcript_segments[0].speaker_id = Some("speaker-0".to_string());
        candidate.transcript_segments[0].speaker_label = "发言人 1".to_string();
        candidate.speaker_profiles[0].display_name = "张三".to_string();
        candidate.speaker_profiles[0].manually_named = true;
        prepare_post_processing_after_stop(&mut candidate, "2026-08-12T10:00:01Z").unwrap();
        let original_job_id = candidate.post_processing.as_ref().unwrap().job_id.clone();
        candidate.post_processing.as_mut().unwrap().status = MeetingPostProcessingStatus::Failed;

        let mut next_config = candidate.post_processing_config.clone().unwrap();
        next_config.processing_revision = 2;
        let next_state = MeetingPostProcessingState {
            status: MeetingPostProcessingStatus::Pending,
            job_id: "job-new".to_string(),
            model_ref: next_config.post_meeting_asr_model_ref.clone(),
            resolved_runtime_kind: MeetingAsrRuntimeKind::Cloud,
            diarization_mode: MeetingDiarizationMode::Cloud,
            expected_speaker_count: None,
            processing_revision: 2,
            provider_task_id: None,
            progress: Some(0.0),
            attempt: 2,
            error_code: None,
            error_message: None,
            created_at: "2026-08-12T10:01:00Z".to_string(),
            updated_at: "2026-08-12T10:01:00Z".to_string(),
            started_at: None,
            completed_at: None,
        };

        assert!(apply_retry_transition(
            &mut candidate,
            &original_job_id,
            MeetingPostProcessingStatus::Failed,
            next_config,
            next_state,
            "2026-08-12T10:01:00Z",
        ));
        assert_eq!(candidate.speaker_profiles[0].display_name, "张三");
        assert!(candidate.speaker_profiles[0].manually_named);
        assert_eq!(candidate.active_transcript_revision, Some(0));

        let state = candidate.post_processing.as_mut().unwrap();
        state.status = MeetingPostProcessingStatus::Applying;
        state.provider_task_id = Some("task-2".to_string());
        assert!(apply_cloud_result_transition(
            &mut candidate,
            "job-new",
            FUN_ASR_MODEL_ID,
            "task-2",
            &cloud_transcript(true),
            "2026-08-12T10:02:00Z",
        )
        .unwrap());

        assert_eq!(candidate.active_transcript_revision, Some(2));
        assert_eq!(candidate.speaker_profiles.len(), 2);
        assert_eq!(candidate.speaker_profiles[0].display_name, "发言人 1");
        assert!(!candidate.speaker_profiles[0].manually_named);
    }

    #[test]
    fn cancel_transition_requires_current_job_and_is_idempotent() {
        let mut candidate = record();
        prepare_post_processing_after_stop(&mut candidate, "2026-08-12T10:00:01Z").unwrap();
        let job_id = candidate.post_processing.as_ref().unwrap().job_id.clone();

        assert!(!apply_cancel_transition(
            &mut candidate,
            "stale-job",
            "2026-08-12T10:00:02Z",
        ));
        assert!(apply_cancel_transition(
            &mut candidate,
            &job_id,
            "2026-08-12T10:00:02Z",
        ));
        assert!(!apply_cancel_transition(
            &mut candidate,
            &job_id,
            "2026-08-12T10:00:03Z",
        ));
        assert_eq!(
            candidate.post_processing.as_ref().unwrap().status,
            MeetingPostProcessingStatus::Cancelled
        );
        assert!(candidate.processing_hold.is_none());
    }

    #[test]
    fn realtime_accept_requires_failed_or_cancelled_current_job() {
        let mut candidate = record();
        prepare_post_processing_after_stop(&mut candidate, "2026-08-12T10:00:01Z").unwrap();
        let job_id = candidate.post_processing.as_ref().unwrap().job_id.clone();

        assert!(!apply_realtime_accept_transition(
            &mut candidate,
            &job_id,
            MeetingPostProcessingStatus::Pending,
            "2026-08-12T10:00:02Z",
        ));
        candidate.post_processing.as_mut().unwrap().status = MeetingPostProcessingStatus::Failed;
        assert!(apply_realtime_accept_transition(
            &mut candidate,
            &job_id,
            MeetingPostProcessingStatus::Failed,
            "2026-08-12T10:00:03Z",
        ));
        assert!(!apply_realtime_accept_transition(
            &mut candidate,
            &job_id,
            MeetingPostProcessingStatus::Failed,
            "2026-08-12T10:00:04Z",
        ));
        assert_eq!(
            candidate.post_processing.as_ref().unwrap().status,
            MeetingPostProcessingStatus::RealtimeAccepted
        );
        assert_eq!(candidate.active_transcript_revision, Some(0));
        assert!(candidate.processing_hold.is_none());
    }

    #[test]
    fn startup_resume_only_selects_non_terminal_active_states() {
        for status in [
            MeetingPostProcessingStatus::Pending,
            MeetingPostProcessingStatus::PreparingAudio,
            MeetingPostProcessingStatus::Uploading,
            MeetingPostProcessingStatus::Running,
            MeetingPostProcessingStatus::LocalAnalyzing,
            MeetingPostProcessingStatus::Applying,
        ] {
            let mut candidate = record();
            prepare_post_processing_after_stop(&mut candidate, "2026-08-12T10:00:01Z").unwrap();
            candidate.post_processing.as_mut().unwrap().status = status;
            assert!(resumable_post_processing_job(candidate).is_some());
        }

        for status in [
            MeetingPostProcessingStatus::Completed,
            MeetingPostProcessingStatus::Failed,
            MeetingPostProcessingStatus::Cancelled,
            MeetingPostProcessingStatus::RealtimeAccepted,
        ] {
            let mut candidate = record();
            prepare_post_processing_after_stop(&mut candidate, "2026-08-12T10:00:01Z").unwrap();
            candidate.post_processing.as_mut().unwrap().status = status;
            assert!(resumable_post_processing_job(candidate).is_none());
        }
    }

    #[test]
    fn realtime_model_lock_updates_frozen_post_processing_config() {
        let mut candidate = record();
        lock_realtime_model_in_post_processing_config(
            &mut candidate,
            "bailian",
            Some("fun-asr-realtime".to_string()),
        );
        let config = candidate.post_processing_config.unwrap();
        assert_eq!(config.realtime_provider_id, "bailian");
        assert_eq!(
            config.realtime_model_id.as_deref(),
            Some("fun-asr-realtime")
        );
        assert_eq!(config.post_meeting_asr_model_ref.model_id, "fun-asr");
    }

    #[test]
    fn initial_config_rejects_out_of_range_expected_speakers_and_missing_local_model() {
        let prefs = UserPreferences::default();
        let zero = StartMeetingRecordingOptions {
            expected_speaker_count: Some(0),
            ..StartMeetingRecordingOptions::default()
        };
        assert_eq!(
            resolve_initial_post_processing_config(&prefs, Some(&zero), "bailian", None),
            Err("expected speaker count must be greater than zero".to_string())
        );

        let too_many = StartMeetingRecordingOptions {
            expected_speaker_count: Some(21),
            ..StartMeetingRecordingOptions::default()
        };
        assert_eq!(
            resolve_initial_post_processing_config(&prefs, Some(&too_many), "bailian", None),
            Err("expected speaker count must not exceed twenty".to_string())
        );

        let local = StartMeetingRecordingOptions {
            diarization_mode: Some(MeetingDiarizationMode::Local),
            local_diarization_model_id: None,
            ..StartMeetingRecordingOptions::default()
        };
        assert_eq!(
            resolve_initial_post_processing_config(&prefs, Some(&local), "bailian", None),
            Err("local diarization model is required".to_string())
        );
    }
}

use std::collections::HashMap;
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
use crate::asr::MeetingAudioSource;
use crate::persistence::{
    meeting_recording_existing_path_for_id, CredentialAccount, CredentialsVault, MeetingStore,
};
use crate::types::{
    ExpectedSpeakerCountOverride, MeetingAsrModelRef, MeetingAsrRuntimeKind,
    MeetingDiarizationMode, MeetingPostProcessingConfig, MeetingPostProcessingEvent,
    MeetingPostProcessingState, MeetingPostProcessingStatus, MeetingRecord,
    PostMeetingAsrModelDescriptor, ProcessingHold, RetryMeetingPostProcessingOptions,
    SpeakerProfile, SpeakerTurn, StartMeetingRecordingOptions, TranscriptRevision,
    TranscriptRevisionSource, TranscriptRevisionStatus, TranscriptSegment,
    TranscriptSegmentMetadata, TranscriptSegmentSource, UserPreferences,
};

use super::{
    derive_bailian_endpoint, prepare_and_spawn_auto_meeting_summary, BailianEndpointProtocol, Inner,
};

const POST_MEETING_PROVIDER_ID: &str = "bailian";
const FUN_ASR_MODEL_ID: &str = "fun-asr";
const PARAFORMER_V2_MODEL_ID: &str = "paraformer-v2";
const REALTIME_TRANSCRIPT_REVISION: u32 = 0;
const FIRST_POST_PROCESSING_REVISION: u32 = 1;
const POST_MEETING_POLL_MIN_SECS: u64 = 600;
const POST_MEETING_POLL_MAX_SECS: u64 = 8 * 60 * 60;
static POST_PROCESSING_CANCEL_FLAGS: LazyLock<Mutex<HashMap<String, Arc<AtomicBool>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

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
    if diarization_mode == MeetingDiarizationMode::Local && local_diarization_model_id.is_none() {
        return Err("local diarization model is required".to_string());
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

async fn run_post_processing_job(
    inner: &Arc<Inner>,
    meeting_id: &str,
    job_id: &str,
    cancelled: Arc<AtomicBool>,
) -> Result<(), String> {
    let store = MeetingStore::new().map_err(|error| error.to_string())?;
    let record = current_post_processing_record(&store, meeting_id, job_id)?;
    let state = record.post_processing.as_ref().unwrap().clone();
    if state.diarization_mode == MeetingDiarizationMode::Local {
        return Err("localDiarizationPipelineUnavailable: 本地说话人处理管线尚未就绪".to_string());
    }
    if state.resolved_runtime_kind != MeetingAsrRuntimeKind::Cloud {
        return Err("postMeetingAsrRuntimeUnsupported: 会后 ASR 运行时不是云端".to_string());
    }
    let descriptor = resolve_post_meeting_asr_model(&state.model_ref)?;
    let client = build_post_meeting_dashscope_client(&descriptor.model_id)?;
    let request_options = cloud_diarization_options(&state)?;

    let provider_task_id = if let Some(task_id) = resume_provider_task_id(&state)? {
        update_post_processing_progress(
            inner,
            &store,
            meeting_id,
            job_id,
            MeetingPostProcessingStatus::Running,
            Some(0.55),
            None,
        )?;
        task_id
    } else {
        update_post_processing_progress(
            inner,
            &store,
            meeting_id,
            job_id,
            MeetingPostProcessingStatus::PreparingAudio,
            Some(0.05),
            None,
        )?;
        let path = meeting_recording_existing_path_for_id(meeting_id)
            .map_err(|error| format!("meetingAudioUnavailable: {error}"))?;
        let source = MeetingAudioSource::from_path(&path)
            .map_err(|error| format!("meetingAudioInvalid: {error}"))?;
        source
            .inspect()
            .map_err(|error| format!("meetingAudioInvalid: {error}"))?;
        ensure_job_not_cancelled(&store, meeting_id, job_id, &cancelled)?;
        update_post_processing_progress(
            inner,
            &store,
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
        ensure_job_not_cancelled(&store, meeting_id, job_id, &cancelled)?;
        update_post_processing_progress(
            inner,
            &store,
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
        update_post_processing_progress(
            inner,
            &store,
            meeting_id,
            job_id,
            MeetingPostProcessingStatus::Running,
            Some(0.55),
            Some(task_id.clone()),
        )?;
        task_id
    };

    ensure_job_not_cancelled(&store, meeting_id, job_id, &cancelled)?;
    let poll_timeout = post_meeting_poll_timeout(record.duration_ms);
    let transcript = client
        .poll_async_task(&provider_task_id, poll_timeout, Arc::clone(&cancelled))
        .await
        .map_err(|error| format!("postMeetingAsrTaskFailed: {error}"))?;
    ensure_job_not_cancelled(&store, meeting_id, job_id, &cancelled)?;
    update_post_processing_progress(
        inner,
        &store,
        meeting_id,
        job_id,
        MeetingPostProcessingStatus::Applying,
        Some(0.9),
        Some(provider_task_id.clone()),
    )?;
    let mut completed = apply_cloud_post_processing_result(
        &store,
        meeting_id,
        job_id,
        &descriptor.model_id,
        &provider_task_id,
        transcript,
    )?;
    emit_post_processing_event(inner, &completed);
    prepare_and_spawn_auto_meeting_summary(inner, &mut completed)?;
    let retention_count = inner.prefs.get().meeting_audio_retention_count;
    store
        .prune_audio_retention(retention_count)
        .map_err(|error| error.to_string())?;
    Ok(())
}

fn resume_provider_task_id(state: &MeetingPostProcessingState) -> Result<Option<String>, String> {
    if let Some(task_id) = state.provider_task_id.clone() {
        return Ok(Some(task_id));
    }
    if matches!(
        state.status,
        MeetingPostProcessingStatus::Running | MeetingPostProcessingStatus::Applying
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
            | MeetingPostProcessingStatus::Applying
    )
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

fn update_post_processing_progress(
    inner: &Arc<Inner>,
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
            record.updated_at = now.clone();
            true
        })
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "postMeetingAsrJobCancelled: 后处理任务已取消或被替换".to_string())?;
    emit_post_processing_event(inner, &updated);
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
    stage_transcript_revision(
        record,
        revision,
        TranscriptRevisionSource::CloudPostprocess,
        segments,
        now,
    )?;
    record.speaker_profiles = profiles;
    record.speaker_turns = turns;
    activate_staging_transcript_revision(record, revision)?;
    let state = record.post_processing.as_mut().unwrap();
    state.status = MeetingPostProcessingStatus::Completed;
    state.progress = Some(1.0);
    state.error_code = None;
    state.error_message = None;
    state.updated_at = now.to_string();
    state.completed_at = Some(now.to_string());
    record.processing_hold = None;
    record.updated_at = now.to_string();
    Ok(true)
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

fn fail_active_post_processing_job(
    record: &mut MeetingRecord,
    job_id: &str,
    error: &str,
    now: &str,
) -> bool {
    let Some(state) = record.post_processing.as_mut() else {
        return false;
    };
    if state.job_id != job_id || !post_processing_status_is_active(state.status) {
        return false;
    }
    state.status = MeetingPostProcessingStatus::Failed;
    let (code, message) = error
        .split_once(':')
        .map(|(code, message)| (code.trim(), message.trim()))
        .unwrap_or(("postMeetingAsrFailed", error.trim()));
    state.error_code = Some(code.to_string());
    state.error_message = Some(message.to_string());
    state.progress = None;
    state.started_at.get_or_insert_with(|| now.to_string());
    state.updated_at = now.to_string();
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
    record.processing_hold = None;
    record.updated_at = now.to_string();
    Some(request)
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
    if diarization_mode == MeetingDiarizationMode::Local && local_model_id.is_none() {
        return Err("local diarization model is required".to_string());
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
    prepare_and_spawn_auto_meeting_summary(inner, &mut updated)?;
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
    use crate::types::{
        MeetingAudioMeta, MeetingAudioState, MeetingStatus, MeetingSummary, SpeakerProfile,
        TranscriptSegmentSource,
    };

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
        assert_eq!(state.attempt, 2);
        assert_eq!(state.processing_revision, 2);
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
    fn initial_config_rejects_zero_expected_speakers_and_missing_local_model() {
        let prefs = UserPreferences::default();
        let zero = StartMeetingRecordingOptions {
            expected_speaker_count: Some(0),
            ..StartMeetingRecordingOptions::default()
        };
        assert_eq!(
            resolve_initial_post_processing_config(&prefs, Some(&zero), "bailian", None),
            Err("expected speaker count must be greater than zero".to_string())
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

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};

use chrono::Utc;
use parking_lot::Mutex;
use tauri::Emitter;
use uuid::Uuid;

use crate::asr::meeting_audio_import::{normalize_pcm_wav, probe_pcm_wav, PcmWavProbe};
use crate::asr::MeetingAudioSource;
use crate::persistence::{
    meeting_import_partial_path, meeting_recording_existing_path_for_id,
    meeting_recording_part_path_for_id, remove_meeting_audio_path, MeetingStore,
};
use crate::types::{
    ExpectedSpeakerCountOverride, MeetingAsrModelDescriptor, MeetingAsrModelReadiness,
    MeetingAsrModelRef, MeetingAsrRuntimeKind, MeetingAudioMeta, MeetingAudioSelection,
    MeetingAudioState, MeetingDiarizationMode, MeetingImportConfig, MeetingImportEvent,
    MeetingImportState, MeetingImportStatus, MeetingPostProcessingConfig,
    MeetingPostProcessingState, MeetingPostProcessingStatus, MeetingRecord, MeetingStatus,
    MeetingSummary, ProcessingHold, RetryMeetingAudioImportOptions, SpeakerProfile, SpeakerTurn,
    StartMeetingAudioImportOptions, TranscriptRevisionStatus, TranscriptSegment,
    TranscriptSegmentMetadata, TranscriptSegmentSource,
};

use super::Inner;

const CLOUD_PROVIDER_ID: &str = "bailian";
const LOCAL_PROVIDER_ID: &str = crate::asr::local::sherpa::PROVIDER_ID;
const LOCAL_WINDOW_MS: u64 = 30_000;
const SAME_SPEAKER_MERGE_GAP_MS: u64 = 500;
const SILENCE_SEARCH_MS: u64 = 5_000;
const SILENCE_FRAME_MS: u64 = 20;
const SILENCE_MAX_MEAN_AMPLITUDE: u64 = 500;
const SELECTION_TOKEN_TTL: Duration = Duration::from_secs(10 * 60);
const IMPORT_ASR_PROGRESS_START: f32 = 0.25;
const IMPORT_WORKER_STOP_TIMEOUT: Duration = Duration::from_secs(30);
static AUDIO_SELECTIONS: LazyLock<Mutex<SelectionRegistry>> =
    LazyLock::new(|| Mutex::new(SelectionRegistry::default()));
static IMPORT_CANCEL_FLAGS: LazyLock<Mutex<HashMap<String, Arc<AtomicBool>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

#[derive(Debug, Clone)]
struct AudioSelectionEntry {
    probe: PcmWavProbe,
    inserted_at: Instant,
}

#[derive(Debug, Default)]
struct SelectionRegistry {
    entries: HashMap<String, AudioSelectionEntry>,
}

impl SelectionRegistry {
    fn insert(&mut self, probe: PcmWavProbe, now: Instant) -> MeetingAudioSelection {
        self.retain_fresh(now);
        let selection_token = Uuid::new_v4().to_string();
        let selection = public_selection(&selection_token, &probe);
        self.entries.insert(
            selection_token,
            AudioSelectionEntry {
                probe,
                inserted_at: now,
            },
        );
        selection
    }

    fn consume(&mut self, token: &str, now: Instant) -> Result<PcmWavProbe, String> {
        let token = token.trim();
        if token.is_empty() {
            return Err("meetingAudioSelectionInvalid: 文件选择令牌为空".to_string());
        }
        let Some(entry) = self.entries.remove(token) else {
            self.retain_fresh(now);
            return Err(
                "meetingAudioSelectionInvalid: 文件选择已失效，请重新选择音频文件".to_string(),
            );
        };
        if now.duration_since(entry.inserted_at) > SELECTION_TOKEN_TTL {
            return Err(
                "meetingAudioSelectionExpired: 文件选择已过期，请重新选择音频文件".to_string(),
            );
        }
        self.retain_fresh(now);
        Ok(entry.probe)
    }

    fn retain_fresh(&mut self, now: Instant) {
        self.entries.retain(|_, entry| {
            now.checked_duration_since(entry.inserted_at)
                .unwrap_or_default()
                <= SELECTION_TOKEN_TTL
        });
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MeetingAsrWindow {
    start_ms: u64,
    end_ms: u64,
    speaker_id: Option<String>,
    overlapping: bool,
}

pub(super) fn register_meeting_audio_selection(
    path: PathBuf,
) -> Result<MeetingAudioSelection, String> {
    let probe = probe_pcm_wav(&path).map_err(|error| format!("meetingAudioInvalid: {error:#}"))?;
    Ok(AUDIO_SELECTIONS.lock().insert(probe, Instant::now()))
}

fn public_selection(token: &str, probe: &PcmWavProbe) -> MeetingAudioSelection {
    MeetingAudioSelection {
        selection_token: token.to_string(),
        file_name: probe.file_name.clone(),
        format: "wav".to_string(),
        size_bytes: probe.size_bytes,
        duration_ms: probe.duration_ms,
        channels: probe.channels,
        sample_rate: probe.sample_rate,
        bits_per_sample: probe.bits_per_sample,
    }
}

pub(super) fn start_meeting_audio_import(
    inner: &Arc<Inner>,
    options: StartMeetingAudioImportOptions,
) -> Result<MeetingRecord, String> {
    if inner.meeting_session.lock().is_some() {
        return Err("meetingAudioImportConflict: 请先结束当前会议录音".to_string());
    }
    let descriptor = resolve_meeting_asr_model(&options.asr_model_ref)?;
    let local_diarization_model_id = normalized_optional_string(options.local_diarization_model_id);
    validate_meeting_asr_combination(
        &descriptor,
        options.diarization_mode,
        local_diarization_model_id.as_deref(),
    )?;
    let expected_speaker_count = normalize_expected_speaker_count(options.expected_speaker_count)?;
    let probe = AUDIO_SELECTIONS
        .lock()
        .consume(&options.selection_token, Instant::now())?;
    let now = Utc::now().to_rfc3339();
    let meeting_id = Uuid::new_v4().to_string();
    let import_job_id = Uuid::new_v4().to_string();
    let partial_path = meeting_import_partial_path(&import_job_id)
        .map_err(|error| format!("meetingAudioStagingFailed: {error:#}"))?;
    let managed_path = meeting_recording_part_path_for_id(&meeting_id, 1)
        .map_err(|error| format!("meetingAudioStagingFailed: {error:#}"))?;
    create_staging_marker(&partial_path)?;

    let title = normalized_import_title(&options.title, &probe.file_name);
    let import_config = MeetingImportConfig {
        source_file_name: probe.file_name.clone(),
        source_format: "wav".to_string(),
        asr_model_ref: MeetingAsrModelRef {
            provider_id: descriptor.provider_id,
            model_id: descriptor.model_id,
        },
        resolved_asr_runtime_kind: descriptor.runtime_kind,
        diarization_mode: options.diarization_mode,
        local_diarization_model_id: if options.diarization_mode == MeetingDiarizationMode::Local {
            local_diarization_model_id
        } else {
            None
        },
        expected_speaker_count,
        generate_summary: options.generate_summary,
        processing_revision: 1,
    };
    let import_state = MeetingImportState {
        status: MeetingImportStatus::Importing,
        import_job_id: import_job_id.clone(),
        progress: Some(0.0),
        attempt: 1,
        error_code: None,
        error_message: None,
        created_at: now.clone(),
        updated_at: now.clone(),
        completed_at: None,
    };
    let record = MeetingRecord {
        id: meeting_id.clone(),
        title,
        status: MeetingStatus::Draft,
        started_at: now.clone(),
        ended_at: Some(now.clone()),
        duration_ms: Some(probe.duration_ms),
        transcript_segments: Vec::new(),
        summary: MeetingSummary::default(),
        audio: MeetingAudioMeta {
            state: MeetingAudioState::Temporary,
            retained: false,
            path: None,
        },
        realtime_asr: None,
        post_processing_config: None,
        post_processing: None,
        import_config: Some(import_config),
        import_state: Some(import_state),
        transcript_revisions: Vec::new(),
        active_transcript_revision: None,
        speaker_profiles: Vec::new(),
        speaker_turns: Vec::new(),
        processing_hold: Some(ProcessingHold {
            job_id: import_job_id.clone(),
            acquired_at: now.clone(),
        }),
        created_at: now.clone(),
        updated_at: now,
    };
    let store = MeetingStore::new().map_err(|error| error.to_string())?;
    if let Err(error) = store.create(record.clone()) {
        let _ = std::fs::remove_file(&partial_path);
        let _ = managed_path.parent().map(remove_meeting_audio_path);
        return Err(error.to_string());
    }
    emit_import_event(inner, &record);
    spawn_audio_normalization_job(
        inner,
        meeting_id,
        import_job_id,
        probe,
        partial_path,
        managed_path,
    );
    Ok(record)
}

pub(super) fn cancel_meeting_audio_import(
    inner: &Arc<Inner>,
    meeting_id: &str,
) -> Result<MeetingRecord, String> {
    let store = MeetingStore::new().map_err(|error| error.to_string())?;
    let record = store
        .get(meeting_id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "meeting not found".to_string())?;
    let import_state = record
        .import_state
        .as_ref()
        .ok_or_else(|| "meeting is not an audio import".to_string())?;
    if import_state.status.is_terminal() {
        return Err("terminal meeting audio import cannot be cancelled".to_string());
    }
    if matches!(
        import_state.status,
        MeetingImportStatus::Transcribing | MeetingImportStatus::Applying
    ) {
        return super::meeting_post_processing::cancel_meeting_post_processing(inner, meeting_id);
    }
    if import_state.status == MeetingImportStatus::Summarizing {
        return Err("meetingAudioImportSummaryActive: 总结生成中暂不支持取消".to_string());
    }

    let import_job_id = import_state.import_job_id.clone();
    let now = Utc::now().to_rfc3339();
    let updated = store
        .update_if(meeting_id, |record| {
            let Some(state) = record.import_state.as_mut() else {
                return false;
            };
            if state.import_job_id != import_job_id || state.status.is_terminal() {
                return false;
            }
            state.status = MeetingImportStatus::Cancelling;
            state.progress = None;
            state.updated_at = now.clone();
            record.updated_at = now.clone();
            true
        })
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "meeting audio import changed; refresh and retry".to_string())?;
    emit_import_event(inner, &updated);
    if let Some(cancelled) = IMPORT_CANCEL_FLAGS.lock().get(&import_job_id).cloned() {
        cancelled.store(true, Ordering::Release);
        return Ok(updated);
    }
    let _ = std::fs::remove_file(
        meeting_import_partial_path(&import_job_id).map_err(|error| error.to_string())?,
    );
    cancel_import_after_worker(inner, meeting_id, &import_job_id)
}

pub(crate) fn cancel_import_for_deletion(record: &mut MeetingRecord, now: &str) -> Option<String> {
    let state = record.import_state.as_mut()?;
    if state.status.is_terminal() {
        return None;
    }
    let import_job_id = state.import_job_id.clone();
    if let Some(cancelled) = IMPORT_CANCEL_FLAGS.lock().get(&import_job_id).cloned() {
        cancelled.store(true, Ordering::Release);
    }
    state.status = MeetingImportStatus::Cancelled;
    state.progress = None;
    state.error_code = None;
    state.error_message = None;
    state.updated_at = now.to_string();
    state.completed_at = Some(now.to_string());
    record.processing_hold = None;
    record.updated_at = now.to_string();
    Some(import_job_id)
}

pub(crate) fn request_import_stop_for_deletion(record: &MeetingRecord) -> Option<String> {
    let state = record.import_state.as_ref()?;
    if state.status.is_terminal() {
        return None;
    }
    let import_job_id = state.import_job_id.clone();
    if let Some(cancelled) = IMPORT_CANCEL_FLAGS.lock().get(&import_job_id).cloned() {
        cancelled.store(true, Ordering::Release);
    }
    Some(import_job_id)
}

pub(crate) fn cleanup_import_after_deletion(import_job_id: &str) {
    if let Ok(path) = meeting_import_partial_path(import_job_id) {
        remove_partial_staging_file(&path);
    }
    IMPORT_CANCEL_FLAGS.lock().remove(import_job_id);
}

pub(super) fn recover_meeting_audio_imports(inner: &Arc<Inner>) -> Result<usize, String> {
    let store = MeetingStore::new().map_err(|error| error.to_string())?;
    let records = store.list().map_err(|error| error.to_string())?;
    let mut recovered = 0usize;
    for record in records {
        let Some(import_state) = record.import_state.as_ref() else {
            continue;
        };
        if matches!(
            import_state.status,
            MeetingImportStatus::Selected
                | MeetingImportStatus::Validating
                | MeetingImportStatus::Importing
                | MeetingImportStatus::Ready
                | MeetingImportStatus::Cancelling
        ) {
            let import_job_id = import_state.import_job_id.clone();
            if let Ok(partial_path) = meeting_import_partial_path(&import_job_id) {
                remove_partial_staging_file(&partial_path);
            }
            let now = Utc::now().to_rfc3339();
            let updated = store
                .update_if(&record.id, |record| {
                    recover_interrupted_import_transition(record, &import_job_id, &now)
                })
                .map_err(|error| error.to_string())?;
            if let Some(updated) = updated {
                emit_import_event(inner, &updated);
                recovered += 1;
            }
        } else if import_state.status == MeetingImportStatus::Summarizing
            && record.status == MeetingStatus::SummaryFailed
        {
            let import_job_id = import_state.import_job_id.clone();
            let now = Utc::now().to_rfc3339();
            let updated = store
                .update_if(&record.id, |record| {
                    recover_interrupted_summary_transition(record, &import_job_id, &now)
                })
                .map_err(|error| error.to_string())?;
            if let Some(updated) = updated {
                emit_import_event(inner, &updated);
                recovered += 1;
            }
        }
    }
    Ok(recovered)
}

pub(super) fn retry_meeting_audio_import(
    inner: &Arc<Inner>,
    meeting_id: &str,
    options: Option<RetryMeetingAudioImportOptions>,
) -> Result<MeetingRecord, String> {
    if inner.meeting_session.lock().is_some() {
        return Err("meetingAudioImportConflict: 请先结束当前会议录音".to_string());
    }
    let store = MeetingStore::new().map_err(|error| error.to_string())?;
    let record = store
        .get(meeting_id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "meeting not found".to_string())?;
    let current_state = record
        .import_state
        .as_ref()
        .ok_or_else(|| "meeting is not an audio import".to_string())?;
    ensure_audio_import_retryable(current_state)?;
    let old_config = record
        .import_config
        .clone()
        .ok_or_else(|| "meeting import config is missing".to_string())?;
    let options = options.unwrap_or(RetryMeetingAudioImportOptions {
        selection_token: None,
        asr_model_ref: None,
        diarization_mode: None,
        local_diarization_model_id: None,
        expected_speaker_count: None,
        generate_summary: None,
    });
    let model_ref = options
        .asr_model_ref
        .unwrap_or_else(|| old_config.asr_model_ref.clone());
    let descriptor = resolve_meeting_asr_model(&model_ref)?;
    let diarization_mode = options
        .diarization_mode
        .unwrap_or(old_config.diarization_mode);
    let local_diarization_model_id = normalized_optional_string(
        options
            .local_diarization_model_id
            .or(old_config.local_diarization_model_id.clone()),
    );
    validate_meeting_asr_combination(
        &descriptor,
        diarization_mode,
        local_diarization_model_id.as_deref(),
    )?;
    let expected_speaker_count = match options.expected_speaker_count {
        None => old_config.expected_speaker_count,
        Some(ExpectedSpeakerCountOverride::Auto) => None,
        Some(ExpectedSpeakerCountOverride::Fixed { count }) => {
            normalize_expected_speaker_count(Some(count))?
        }
    };
    let next_config = MeetingImportConfig {
        source_file_name: old_config.source_file_name,
        source_format: old_config.source_format,
        asr_model_ref: MeetingAsrModelRef {
            provider_id: descriptor.provider_id,
            model_id: descriptor.model_id,
        },
        resolved_asr_runtime_kind: descriptor.runtime_kind,
        diarization_mode,
        local_diarization_model_id: if diarization_mode == MeetingDiarizationMode::Local {
            local_diarization_model_id
        } else {
            None
        },
        expected_speaker_count,
        generate_summary: options
            .generate_summary
            .unwrap_or(old_config.generate_summary),
        processing_revision: old_config.processing_revision.saturating_add(1),
    };
    let existing_managed_audio = meeting_recording_existing_path_for_id(meeting_id)
        .ok()
        .filter(|path| path.exists());
    let managed_audio_is_valid = existing_managed_audio.as_ref().is_some_and(|path| {
        MeetingAudioSource::from_path(path)
            .and_then(|source| source.inspect())
            .is_ok()
    });
    if managed_audio_is_valid {
        if options.selection_token.is_some() {
            return Err(
                "meetingAudioSelectionUnexpected: 已有完整受管音频，无需重新选择文件".to_string(),
            );
        }
        return retry_from_managed_audio(inner, &store, meeting_id, current_state, next_config);
    }

    let selection_token = options.selection_token.ok_or_else(|| {
        "meetingAudioReselectionRequired: 受管音频不完整，请重新选择源文件".to_string()
    })?;
    let probe = AUDIO_SELECTIONS
        .lock()
        .consume(&selection_token, Instant::now())?;
    retry_from_source_selection(
        inner,
        &store,
        meeting_id,
        current_state,
        next_config,
        probe,
        existing_managed_audio,
    )
}

fn retry_from_managed_audio(
    inner: &Arc<Inner>,
    store: &MeetingStore,
    meeting_id: &str,
    current_state: &MeetingImportState,
    next_config: MeetingImportConfig,
) -> Result<MeetingRecord, String> {
    let now = Utc::now().to_rfc3339();
    let post_job_id = Uuid::new_v4().to_string();
    let expected_import_job_id = current_state.import_job_id.clone();
    let expected_status = current_state.status;
    let attempt = current_state.attempt.saturating_add(1);
    let updated = store
        .update_if(meeting_id, |record| {
            if !current_import_state_matches(record, &expected_import_job_id, expected_status) {
                return false;
            }
            reject_staging_revisions(record);
            record.import_config = Some(next_config.clone());
            record.post_processing_config = Some(post_processing_config_from_import(&next_config));
            record.post_processing = Some(post_processing_state_from_import(
                &next_config,
                &post_job_id,
                attempt,
                &now,
            ));
            record.import_state = Some(MeetingImportState {
                status: MeetingImportStatus::Transcribing,
                import_job_id: expected_import_job_id.clone(),
                progress: Some(IMPORT_ASR_PROGRESS_START),
                attempt,
                error_code: None,
                error_message: None,
                created_at: current_state.created_at.clone(),
                updated_at: now.clone(),
                completed_at: None,
            });
            record.processing_hold = Some(ProcessingHold {
                job_id: post_job_id.clone(),
                acquired_at: now.clone(),
            });
            record.status = MeetingStatus::Draft;
            record.updated_at = now.clone();
            true
        })
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "meeting audio import changed; refresh and retry".to_string())?;
    emit_import_event(inner, &updated);
    super::meeting_post_processing::spawn_post_processing_job(
        inner,
        meeting_id.to_string(),
        post_job_id,
    );
    Ok(updated)
}

fn retry_from_source_selection(
    inner: &Arc<Inner>,
    store: &MeetingStore,
    meeting_id: &str,
    current_state: &MeetingImportState,
    mut next_config: MeetingImportConfig,
    probe: PcmWavProbe,
    stale_managed_audio: Option<PathBuf>,
) -> Result<MeetingRecord, String> {
    if let Some(stale_path) = stale_managed_audio.as_deref() {
        if path_is_within(&probe.path, stale_path) {
            return Err(
                "meetingAudioSourceConflict: 不能使用本会议的受管副本作为重选源文件".to_string(),
            );
        }
    }
    let import_job_id = Uuid::new_v4().to_string();
    let partial_path = meeting_import_partial_path(&import_job_id)
        .map_err(|error| format!("meetingAudioStagingFailed: {error:#}"))?;
    create_staging_marker(&partial_path)?;
    if let Some(stale_path) = stale_managed_audio.as_deref() {
        if let Err(error) = remove_meeting_audio_path(stale_path) {
            let _ = std::fs::remove_file(&partial_path);
            return Err(format!("meetingAudioManagedCleanupFailed: {error:#}"));
        }
    }
    let managed_path = match meeting_recording_part_path_for_id(meeting_id, 1) {
        Ok(path) => path,
        Err(error) => {
            let _ = std::fs::remove_file(&partial_path);
            return Err(format!("meetingAudioStagingFailed: {error:#}"));
        }
    };
    next_config.source_file_name = probe.file_name.clone();
    next_config.source_format = "wav".to_string();
    let expected_import_job_id = current_state.import_job_id.clone();
    let expected_status = current_state.status;
    let attempt = current_state.attempt.saturating_add(1);
    let now = Utc::now().to_rfc3339();
    let updated = store
        .update_if(meeting_id, |record| {
            if !current_import_state_matches(record, &expected_import_job_id, expected_status) {
                return false;
            }
            reject_staging_revisions(record);
            record.import_config = Some(next_config.clone());
            record.post_processing_config = None;
            record.post_processing = None;
            record.import_state = Some(MeetingImportState {
                status: MeetingImportStatus::Importing,
                import_job_id: import_job_id.clone(),
                progress: Some(0.0),
                attempt,
                error_code: None,
                error_message: None,
                created_at: current_state.created_at.clone(),
                updated_at: now.clone(),
                completed_at: None,
            });
            record.audio = MeetingAudioMeta {
                state: MeetingAudioState::Temporary,
                retained: false,
                path: None,
            };
            record.duration_ms = Some(probe.duration_ms);
            record.processing_hold = Some(ProcessingHold {
                job_id: import_job_id.clone(),
                acquired_at: now.clone(),
            });
            record.status = MeetingStatus::Draft;
            record.updated_at = now.clone();
            true
        })
        .map_err(|error| error.to_string())?;
    let Some(updated) = updated else {
        let _ = std::fs::remove_file(&partial_path);
        return Err("meeting audio import changed; refresh and retry".to_string());
    };
    emit_import_event(inner, &updated);
    spawn_audio_normalization_job(
        inner,
        meeting_id.to_string(),
        import_job_id,
        probe,
        partial_path,
        managed_path,
    );
    Ok(updated)
}

fn current_import_state_matches(
    record: &MeetingRecord,
    expected_job_id: &str,
    expected_status: MeetingImportStatus,
) -> bool {
    record.import_state.as_ref().is_some_and(|state| {
        state.import_job_id == expected_job_id
            && state.status == expected_status
            && matches!(
                state.status,
                MeetingImportStatus::Failed | MeetingImportStatus::Cancelled
            )
    })
}

fn reject_staging_revisions(record: &mut MeetingRecord) {
    for revision in &mut record.transcript_revisions {
        if revision.status == TranscriptRevisionStatus::Staging {
            revision.status = TranscriptRevisionStatus::Rejected;
        }
    }
}

fn post_processing_config_from_import(config: &MeetingImportConfig) -> MeetingPostProcessingConfig {
    MeetingPostProcessingConfig {
        diarization_mode: config.diarization_mode,
        realtime_provider_id: "audio-import".to_string(),
        realtime_model_id: None,
        post_meeting_asr_model_ref: config.asr_model_ref.clone(),
        resolved_asr_runtime_kind: config.resolved_asr_runtime_kind,
        local_diarization_model_id: config.local_diarization_model_id.clone(),
        expected_speaker_count: config.expected_speaker_count,
        model_version: None,
        processing_revision: config.processing_revision,
    }
}

fn post_processing_state_from_import(
    config: &MeetingImportConfig,
    job_id: &str,
    attempt: u32,
    now: &str,
) -> MeetingPostProcessingState {
    MeetingPostProcessingState {
        status: MeetingPostProcessingStatus::Pending,
        job_id: job_id.to_string(),
        model_ref: config.asr_model_ref.clone(),
        resolved_runtime_kind: config.resolved_asr_runtime_kind,
        diarization_mode: config.diarization_mode,
        expected_speaker_count: config.expected_speaker_count,
        processing_revision: config.processing_revision,
        provider_task_id: None,
        progress: Some(0.0),
        attempt,
        error_code: None,
        error_message: None,
        created_at: now.to_string(),
        updated_at: now.to_string(),
        started_at: None,
        completed_at: None,
    }
}

fn create_staging_marker(path: &std::path::Path) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("meetingAudioStagingFailed: {error}"))?;
    }
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map(|_| ())
        .map_err(|error| format!("meetingAudioStagingFailed: {error}"))
}

fn normalized_import_title(title: &str, file_name: &str) -> String {
    let title = title.trim();
    if !title.is_empty() {
        return title.to_string();
    }
    std::path::Path::new(file_name)
        .file_stem()
        .and_then(|value| value.to_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("导入会议")
        .to_string()
}

fn normalized_optional_string(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let value = value.trim();
        (!value.is_empty()).then(|| value.to_string())
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

fn spawn_audio_normalization_job(
    inner: &Arc<Inner>,
    meeting_id: String,
    import_job_id: String,
    probe: PcmWavProbe,
    partial_path: PathBuf,
    managed_path: PathBuf,
) {
    let Some(cancelled) = register_import_job(&import_job_id) else {
        return;
    };
    let inner = Arc::clone(inner);
    tauri::async_runtime::spawn(async move {
        let inner_for_worker = Arc::clone(&inner);
        let meeting_for_worker = meeting_id.clone();
        let job_for_worker = import_job_id.clone();
        let cancelled_for_worker = Arc::clone(&cancelled);
        let result = tauri::async_runtime::spawn_blocking(move || {
            let mut last_reported = -1i32;
            normalize_pcm_wav(
                &probe,
                &partial_path,
                &managed_path,
                &cancelled_for_worker,
                |progress| {
                    let bucket = (progress.clamp(0.0, 1.0) * 20.0).floor() as i32;
                    if bucket != last_reported {
                        last_reported = bucket;
                        let _ = update_import_progress(
                            &inner_for_worker,
                            &meeting_for_worker,
                            &job_for_worker,
                            MeetingImportStatus::Importing,
                            Some(progress * IMPORT_ASR_PROGRESS_START),
                        );
                    }
                },
            )
        })
        .await
        .map_err(|error| format!("meetingAudioImportFailed: worker join failed: {error}"))
        .and_then(|result| result.map_err(|error| format!("meetingAudioImportFailed: {error:#}")));

        match result {
            Ok(info) => {
                let transition = transition_import_to_post_processing(
                    &inner,
                    &meeting_id,
                    &import_job_id,
                    info.duration_ms,
                    &cancelled,
                );
                if let Err(error) = transition {
                    if cancelled.load(Ordering::Acquire) || is_import_cancellation_error(&error) {
                        let _ = cancel_import_after_worker(&inner, &meeting_id, &import_job_id);
                    } else {
                        log::warn!("[meeting-audio-import] transition failed: {error}");
                        let _ = fail_import_job(&inner, &meeting_id, &import_job_id, &error);
                    }
                }
            }
            Err(error) => {
                if cancelled.load(Ordering::Acquire) {
                    let _ = cancel_import_after_worker(&inner, &meeting_id, &import_job_id);
                } else if let Err(persist_error) =
                    fail_import_job(&inner, &meeting_id, &import_job_id, &error)
                {
                    log::warn!(
                        "[meeting-audio-import] persist failure for job {import_job_id} failed: {persist_error}"
                    );
                }
            }
        }
        IMPORT_CANCEL_FLAGS.lock().remove(&import_job_id);
    });
}

fn register_import_job(job_id: &str) -> Option<Arc<AtomicBool>> {
    let mut flags = IMPORT_CANCEL_FLAGS.lock();
    if flags.contains_key(job_id) {
        return None;
    }
    let cancelled = Arc::new(AtomicBool::new(false));
    flags.insert(job_id.to_string(), Arc::clone(&cancelled));
    Some(cancelled)
}

pub(crate) async fn wait_for_import_worker_exit(import_job_id: &str) -> Result<(), String> {
    let started_at = Instant::now();
    loop {
        if !IMPORT_CANCEL_FLAGS.lock().contains_key(import_job_id) {
            return Ok(());
        }
        if started_at.elapsed() >= IMPORT_WORKER_STOP_TIMEOUT {
            return Err(
                "meetingAudioImportStillStopping: 音频导入任务仍在停止，请稍后重试删除".to_string(),
            );
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

fn transition_import_to_post_processing(
    inner: &Arc<Inner>,
    meeting_id: &str,
    import_job_id: &str,
    duration_ms: u64,
    cancelled: &AtomicBool,
) -> Result<MeetingRecord, String> {
    if cancelled.load(Ordering::Acquire) {
        return Err("meetingAudioImportCancelled: 音频导入已取消".to_string());
    }
    let store = MeetingStore::new().map_err(|error| error.to_string())?;
    let now = Utc::now().to_rfc3339();
    let post_job_id = Uuid::new_v4().to_string();
    let updated = store
        .update_if(meeting_id, |record| {
            let Some(import_state) = record.import_state.as_ref() else {
                return false;
            };
            if import_state.import_job_id != import_job_id
                || import_state.status != MeetingImportStatus::Importing
            {
                return false;
            }
            let Some(import_config) = record.import_config.clone() else {
                return false;
            };
            record.duration_ms = Some(duration_ms);
            record.audio = MeetingAudioMeta {
                state: MeetingAudioState::Retained,
                retained: true,
                path: None,
            };
            record.status = MeetingStatus::Draft;
            record.post_processing_config = Some(MeetingPostProcessingConfig {
                diarization_mode: import_config.diarization_mode,
                realtime_provider_id: "audio-import".to_string(),
                realtime_model_id: None,
                post_meeting_asr_model_ref: import_config.asr_model_ref.clone(),
                resolved_asr_runtime_kind: import_config.resolved_asr_runtime_kind,
                local_diarization_model_id: import_config.local_diarization_model_id.clone(),
                expected_speaker_count: import_config.expected_speaker_count,
                model_version: None,
                processing_revision: import_config.processing_revision,
            });
            record.post_processing = Some(MeetingPostProcessingState {
                status: MeetingPostProcessingStatus::Pending,
                job_id: post_job_id.clone(),
                model_ref: import_config.asr_model_ref,
                resolved_runtime_kind: import_config.resolved_asr_runtime_kind,
                diarization_mode: import_config.diarization_mode,
                expected_speaker_count: import_config.expected_speaker_count,
                processing_revision: import_config.processing_revision,
                provider_task_id: None,
                progress: Some(0.0),
                attempt: import_state.attempt,
                error_code: None,
                error_message: None,
                created_at: now.clone(),
                updated_at: now.clone(),
                started_at: None,
                completed_at: None,
            });
            let import_state = record.import_state.as_mut().unwrap();
            import_state.status = MeetingImportStatus::Transcribing;
            import_state.progress = Some(IMPORT_ASR_PROGRESS_START);
            import_state.updated_at = now.clone();
            record.processing_hold = Some(ProcessingHold {
                job_id: post_job_id.clone(),
                acquired_at: now.clone(),
            });
            record.updated_at = now.clone();
            true
        })
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "meetingAudioImportCancelled: 音频导入任务已取消或被替换".to_string())?;
    emit_import_event(inner, &updated);
    super::meeting_post_processing::spawn_post_processing_job(
        inner,
        meeting_id.to_string(),
        post_job_id,
    );
    Ok(updated)
}

fn update_import_progress(
    inner: &Arc<Inner>,
    meeting_id: &str,
    import_job_id: &str,
    status: MeetingImportStatus,
    progress: Option<f32>,
) -> Result<MeetingRecord, String> {
    let store = MeetingStore::new().map_err(|error| error.to_string())?;
    let now = Utc::now().to_rfc3339();
    let updated = store
        .update_if(meeting_id, |record| {
            let Some(state) = record.import_state.as_mut() else {
                return false;
            };
            if state.import_job_id != import_job_id || state.status.is_terminal() {
                return false;
            }
            state.status = status;
            state.progress = progress;
            state.updated_at = now.clone();
            record.updated_at = now.clone();
            true
        })
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "meetingAudioImportCancelled: 音频导入任务已取消或被替换".to_string())?;
    emit_import_event(inner, &updated);
    Ok(updated)
}

fn fail_import_job(
    inner: &Arc<Inner>,
    meeting_id: &str,
    import_job_id: &str,
    error: &str,
) -> Result<MeetingRecord, String> {
    let store = MeetingStore::new().map_err(|store_error| store_error.to_string())?;
    let now = Utc::now().to_rfc3339();
    let (code, message) = split_error(error, "meetingAudioImportFailed");
    let updated = store
        .update_if(meeting_id, |record| {
            let Some(state) = record.import_state.as_mut() else {
                return false;
            };
            if state.import_job_id != import_job_id || state.status.is_terminal() {
                return false;
            }
            state.status = MeetingImportStatus::Failed;
            state.progress = None;
            state.error_code = Some(code.to_string());
            state.error_message = Some(message.to_string());
            state.updated_at = now.clone();
            record.processing_hold = None;
            record.updated_at = now.clone();
            true
        })
        .map_err(|store_error| store_error.to_string())?
        .ok_or_else(|| "meetingAudioImportCancelled: 音频导入任务已取消或被替换".to_string())?;
    emit_import_event(inner, &updated);
    Ok(updated)
}

fn cancel_import_after_worker(
    inner: &Arc<Inner>,
    meeting_id: &str,
    import_job_id: &str,
) -> Result<MeetingRecord, String> {
    let store = MeetingStore::new().map_err(|error| error.to_string())?;
    let now = Utc::now().to_rfc3339();
    if managed_audio_is_valid(meeting_id) {
        let mut updated = store
            .update_if(meeting_id, |record| {
                if !apply_import_cancelled_transition(record, import_job_id, &now) {
                    return false;
                }
                record.audio = MeetingAudioMeta {
                    state: MeetingAudioState::Retained,
                    retained: true,
                    path: None,
                };
                true
            })
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "meetingAudioImportCancelled: 音频导入任务已取消或被替换".to_string())?;
        store
            .prune_audio_retention(inner.prefs.get().meeting_audio_retention_count)
            .map_err(|error| error.to_string())?;
        if let Some(reloaded) = store.get(meeting_id).map_err(|error| error.to_string())? {
            updated = reloaded;
        }
        emit_import_event(inner, &updated);
        return Ok(updated);
    }

    let mut cancelled_record = None;
    let removed = store
        .delete_with_cleanup(
            meeting_id,
            |record| {
                if !apply_import_cancelled_transition(record, import_job_id, &now) {
                    anyhow::bail!("meeting is not an audio import");
                }
                cancelled_record = Some(record.clone());
                Ok(())
            },
            |delete_id| {
                let path = meeting_recording_existing_path_for_id(delete_id)?;
                remove_meeting_audio_path(&path)
            },
        )
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "meetingAudioImportCancelled: 音频导入记录已删除".to_string())?;
    let _ = removed;
    if let Ok(path) = meeting_import_partial_path(import_job_id) {
        let _ = std::fs::remove_file(path);
    }
    let cancelled_record = cancelled_record
        .ok_or_else(|| "meetingAudioImportCancelled: 音频导入记录已删除".to_string())?;
    emit_import_record_deleted(inner, meeting_id);
    Ok(cancelled_record)
}

fn apply_import_cancelled_transition(
    record: &mut MeetingRecord,
    import_job_id: &str,
    now: &str,
) -> bool {
    let Some(state) = record.import_state.as_mut() else {
        return false;
    };
    if state.import_job_id != import_job_id || state.status.is_terminal() {
        return false;
    }
    state.status = MeetingImportStatus::Cancelled;
    state.progress = None;
    state.error_code = None;
    state.error_message = None;
    state.updated_at = now.to_string();
    state.completed_at = Some(now.to_string());
    record.processing_hold = None;
    record.updated_at = now.to_string();
    true
}

fn managed_audio_is_valid(meeting_id: &str) -> bool {
    meeting_recording_existing_path_for_id(meeting_id)
        .ok()
        .filter(|path| path.exists())
        .and_then(|path| MeetingAudioSource::from_path(&path).ok())
        .and_then(|source| source.inspect().ok())
        .is_some()
}

fn is_import_cancellation_error(error: &str) -> bool {
    error.starts_with("meetingAudioImportCancelled:")
}

fn path_is_within(path: &std::path::Path, parent: &std::path::Path) -> bool {
    let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let parent = parent
        .canonicalize()
        .unwrap_or_else(|_| parent.to_path_buf());
    if parent.is_dir() {
        path.starts_with(parent)
    } else {
        path == parent
    }
}

fn split_error<'a>(error: &'a str, default_code: &'a str) -> (&'a str, &'a str) {
    error
        .split_once(':')
        .map(|(code, message)| (code.trim(), message.trim()))
        .unwrap_or((default_code, error.trim()))
}

pub(super) fn emit_import_event(inner: &Arc<Inner>, record: &MeetingRecord) {
    if let (Some(app), Some(state)) = (inner.app.lock().clone(), record.import_state.clone()) {
        let payload = MeetingImportEvent {
            meeting_id: record.id.clone(),
            state,
            meeting: record.clone(),
        };
        let _ = app.emit("meeting:import-state", payload.clone());
        let _ = app.emit("meeting:record-updated", payload.meeting);
    }
}

fn emit_import_record_deleted(inner: &Arc<Inner>, meeting_id: &str) {
    if let Some(app) = inner.app.lock().clone() {
        let _ = app.emit("meeting:record-deleted", meeting_id.to_string());
    }
}

pub(super) fn list_meeting_file_asr_models() -> Vec<MeetingAsrModelDescriptor> {
    let mut models = vec![
        cloud_descriptor("fun-asr", "Fun-ASR", true),
        cloud_descriptor("paraformer-v2", "Paraformer V2", false),
    ];
    models.extend(local_meeting_file_models());
    models
}

fn cloud_descriptor(
    model_id: &str,
    display_name: &str,
    is_default: bool,
) -> MeetingAsrModelDescriptor {
    MeetingAsrModelDescriptor {
        provider_id: CLOUD_PROVIDER_ID.to_string(),
        model_id: model_id.to_string(),
        display_name: display_name.to_string(),
        runtime_kind: MeetingAsrRuntimeKind::Cloud,
        supports_meeting_file: true,
        supports_diarization: true,
        supports_speaker_count: true,
        readiness: MeetingAsrModelReadiness::Ready,
        readiness_message: None,
        is_default,
    }
}

fn local_meeting_file_models() -> Vec<MeetingAsrModelDescriptor> {
    crate::asr::local::sherpa::MODELS
        .iter()
        .filter(|model| model.mode == crate::asr::local::sherpa::SherpaMode::Offline)
        .map(|model| {
            let (readiness, readiness_message) = local_model_readiness(model.alias);
            MeetingAsrModelDescriptor {
                provider_id: LOCAL_PROVIDER_ID.to_string(),
                model_id: model.alias.to_string(),
                display_name: model.display_name.to_string(),
                runtime_kind: MeetingAsrRuntimeKind::Local,
                supports_meeting_file: true,
                supports_diarization: false,
                supports_speaker_count: false,
                readiness,
                readiness_message,
                is_default: false,
            }
        })
        .collect()
}

fn local_model_readiness(alias: &str) -> (MeetingAsrModelReadiness, Option<String>) {
    #[cfg(target_os = "windows")]
    {
        let dir = match crate::asr::local::sherpa::model_dir_for_alias(alias) {
            Ok(dir) => dir,
            Err(error) => {
                return (
                    MeetingAsrModelReadiness::Unavailable,
                    Some(format!("无法定位本地模型目录：{error:#}")),
                );
            }
        };
        let ready = crate::asr::local::sherpa::required_files_for_alias(alias)
            .map(|files| {
                files.iter().all(|file| {
                    crate::asr::local::sherpa::required_path_is_valid(alias, file, &dir.join(file))
                })
            })
            .unwrap_or(false);
        if ready {
            (MeetingAsrModelReadiness::Ready, None)
        } else {
            (
                MeetingAsrModelReadiness::Missing,
                Some("本地模型尚未完整下载".to_string()),
            )
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = alias;
        (
            MeetingAsrModelReadiness::Unavailable,
            Some("当前平台暂不支持 sherpa-onnx 会议文件识别".to_string()),
        )
    }
}

pub(super) fn resolve_meeting_asr_model(
    model_ref: &MeetingAsrModelRef,
) -> Result<MeetingAsrModelDescriptor, String> {
    let provider_id = model_ref.provider_id.trim();
    let model_id = model_ref.model_id.trim();
    list_meeting_file_asr_models()
        .into_iter()
        .find(|model| model.provider_id == provider_id && model.model_id == model_id)
        .ok_or_else(|| format!("unsupported meeting file ASR model: {provider_id}/{model_id}"))
}

pub(super) fn validate_meeting_asr_combination(
    descriptor: &MeetingAsrModelDescriptor,
    diarization_mode: MeetingDiarizationMode,
    local_diarization_model_id: Option<&str>,
) -> Result<(), String> {
    if !descriptor.supports_meeting_file {
        return Err("meetingAsrModelUnsupported: 所选模型不支持会议音频文件".to_string());
    }
    match descriptor.readiness {
        MeetingAsrModelReadiness::Ready => {}
        MeetingAsrModelReadiness::Missing => {
            return Err(format!(
                "meetingAsrModelMissing: {}",
                descriptor
                    .readiness_message
                    .as_deref()
                    .unwrap_or("所选本地模型尚未下载")
            ));
        }
        MeetingAsrModelReadiness::Unavailable => {
            return Err(format!(
                "meetingAsrModelUnavailable: {}",
                descriptor
                    .readiness_message
                    .as_deref()
                    .unwrap_or("所选模型在当前设备不可用")
            ));
        }
    }
    if descriptor.runtime_kind == MeetingAsrRuntimeKind::Local
        && diarization_mode == MeetingDiarizationMode::Cloud
    {
        return Err(
            "meetingAsrCombinationUnsupported: 本地 ASR 暂不支持云端说话人处理；不会上传音频"
                .to_string(),
        );
    }
    if diarization_mode == MeetingDiarizationMode::Cloud && !descriptor.supports_diarization {
        return Err("meetingAsrDiarizationUnsupported: 所选云端模型不支持说话人处理".to_string());
    }
    if diarization_mode == MeetingDiarizationMode::Local {
        let model_id = local_diarization_model_id
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                "localDiarizationModelNotReady: 请选择已下载的本地说话人模型".to_string()
            })?;
        crate::asr::local::speaker_diarization::ensure_package_ready(model_id)
            .map_err(|error| format!("localDiarizationModelNotReady: {error:#}"))?;
    }
    Ok(())
}

pub(super) async fn run_local_meeting_file_asr(
    inner: &Arc<Inner>,
    source: MeetingAudioSource,
    model_ref: &MeetingAsrModelRef,
    diarization_mode: MeetingDiarizationMode,
    local_diarization_model_id: Option<&str>,
    expected_speaker_count: Option<u32>,
    job_id: &str,
    cancelled: Arc<AtomicBool>,
) -> Result<
    (
        Vec<TranscriptSegment>,
        Vec<SpeakerProfile>,
        Vec<SpeakerTurn>,
    ),
    String,
> {
    #[cfg(target_os = "windows")]
    {
        let info = source
            .inspect()
            .map_err(|error| format!("meetingAudioInvalid: {error:#}"))?;
        let (windows, turns, profiles) = if diarization_mode == MeetingDiarizationMode::Local {
            let local_model_id = local_diarization_model_id.ok_or_else(|| {
                "localDiarizationModelNotReady: 本次导入没有本地说话人模型快照".to_string()
            })?;
            let diarization_source = source.clone();
            let model_id = local_model_id.to_string();
            let diarization_cancelled = Arc::clone(&cancelled);
            let output = tauri::async_runtime::spawn_blocking(move || {
                crate::asr::local::speaker_diarization_runtime::run_local_diarization(
                    diarization_source,
                    &model_id,
                    expected_speaker_count,
                    diarization_cancelled,
                )
            })
            .await
            .map_err(|error| format!("localDiarizationRuntimeFailed: worker join failed: {error}"))?
            .map_err(|error| format!("localDiarizationRuntimeFailed: {error:#}"))?;
            let windows = build_speaker_windows(
                &output.turns,
                info.duration_ms,
                |window_start_ms, target_ms| {
                    find_silence_split_ms(&source, window_start_ms, target_ms, &cancelled)
                },
            )?;
            let profiles = profiles_from_turns(&output.turns);
            (windows, output.turns, profiles)
        } else {
            (bounded_windows(info.duration_ms), Vec::new(), Vec::new())
        };
        let language_hint = inner.prefs.get().sherpa_onnx_language_hint;
        let transcriber = MeetingBatchTranscriber {
            runtime: Arc::clone(&inner.sherpa_onnx_runtime),
            source,
            model_ref,
            language_hint: language_hint.trim(),
            job_id,
            cancelled,
        };
        let segments = transcriber.transcribe(windows).await?;
        Ok((segments, profiles, turns))
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = (
            inner,
            source,
            model_ref,
            diarization_mode,
            local_diarization_model_id,
            expected_speaker_count,
            job_id,
            cancelled,
        );
        Err("meetingAsrModelUnavailable: 当前平台暂不支持本地会议文件识别".to_string())
    }
}

#[cfg(target_os = "windows")]
struct MeetingBatchTranscriber<'a> {
    runtime: Arc<crate::asr::local::SherpaOnnxRuntime>,
    source: MeetingAudioSource,
    model_ref: &'a MeetingAsrModelRef,
    language_hint: &'a str,
    job_id: &'a str,
    cancelled: Arc<AtomicBool>,
}

#[cfg(target_os = "windows")]
impl MeetingBatchTranscriber<'_> {
    async fn transcribe(
        &self,
        windows: Vec<MeetingAsrWindow>,
    ) -> Result<Vec<TranscriptSegment>, String> {
        let mut segments = Vec::new();
        for (index, window) in windows.into_iter().enumerate() {
            if self.cancelled.load(Ordering::Acquire) {
                return Err("meetingAudioImportCancelled: 音频导入已取消".to_string());
            }
            let pcm = self
                .source
                .read_pcm_range(window.start_ms, window.end_ms, &self.cancelled)
                .map_err(|error| format!("meetingAudioReadFailed: {error:#}"))?;
            let timeout = local_window_timeout(window.end_ms.saturating_sub(window.start_ms));
            let text = self
                .runtime
                .transcribe_pcm(
                    &self.model_ref.model_id,
                    &pcm,
                    (!self.language_hint.is_empty()).then_some(self.language_hint),
                    timeout,
                )
                .await
                .map_err(|error| format!("meetingLocalAsrFailed: {error:#}"))?;
            let text = text.trim();
            if text.is_empty() {
                continue;
            }
            let speaker_label = window
                .speaker_id
                .as_deref()
                .map(speaker_display_name)
                .unwrap_or_else(|| "未区分".to_string());
            segments.push(TranscriptSegment {
                id: format!("import-{}-{}", self.job_id, index + 1),
                speaker_id: window.speaker_id,
                speaker_label,
                start_ms: window.start_ms,
                end_ms: Some(window.end_ms),
                text: text.to_string(),
                source: TranscriptSegmentSource::RetranscribedAsr,
                metadata: Some(TranscriptSegmentMetadata {
                    provider_id: Some(format!(
                        "{}/{}",
                        self.model_ref.provider_id, self.model_ref.model_id
                    )),
                    provider_session_id: Some(self.job_id.to_string()),
                    provider_segment_id: Some(Uuid::new_v4().to_string()),
                    provider_start_ms: Some(window.start_ms),
                    provider_end_ms: Some(window.end_ms),
                    overlapping: window.overlapping,
                    needs_review: window.overlapping,
                    ..TranscriptSegmentMetadata::default()
                }),
            });
        }
        if segments.is_empty() {
            return Err("meetingLocalAsrEmpty: 本地模型没有返回可用文字".to_string());
        }
        Ok(segments)
    }
}

fn local_window_timeout(duration_ms: u64) -> Duration {
    Duration::from_secs(duration_ms.div_ceil(1000).saturating_add(20).max(30))
}

fn bounded_windows(duration_ms: u64) -> Vec<MeetingAsrWindow> {
    let mut windows = Vec::new();
    let mut start_ms = 0;
    while start_ms < duration_ms {
        let end_ms = start_ms.saturating_add(LOCAL_WINDOW_MS).min(duration_ms);
        windows.push(MeetingAsrWindow {
            start_ms,
            end_ms,
            speaker_id: None,
            overlapping: false,
        });
        start_ms = end_ms;
    }
    windows
}

fn build_speaker_windows<F>(
    turns: &[SpeakerTurn],
    duration_ms: u64,
    mut find_split: F,
) -> Result<Vec<MeetingAsrWindow>, String>
where
    F: FnMut(u64, u64) -> Result<Option<u64>, String>,
{
    if turns.is_empty() {
        return Err("localDiarizationRuntimeFailed: 本地说话人处理没有返回时间段".to_string());
    }
    let mut sorted = turns.to_vec();
    sorted.sort_by_key(|turn| (turn.start_ms, turn.end_ms));
    let mut merged = Vec::<MeetingAsrWindow>::new();
    for turn in sorted {
        let start_ms = turn.start_ms.min(duration_ms);
        let end_ms = turn.end_ms.min(duration_ms);
        if end_ms <= start_ms {
            continue;
        }
        if let Some(previous) = merged.last_mut() {
            let gap_ms = start_ms.saturating_sub(previous.end_ms);
            if previous.speaker_id.as_deref() == Some(turn.speaker_id.as_str())
                && gap_ms <= SAME_SPEAKER_MERGE_GAP_MS
                && end_ms.saturating_sub(previous.start_ms) <= LOCAL_WINDOW_MS
            {
                previous.end_ms = previous.end_ms.max(end_ms);
                previous.overlapping |= turn.overlapping;
                continue;
            }
        }
        merged.push(MeetingAsrWindow {
            start_ms,
            end_ms,
            speaker_id: Some(turn.speaker_id),
            overlapping: turn.overlapping,
        });
    }
    let mut windows = Vec::new();
    for window in merged {
        let mut start_ms = window.start_ms;
        while start_ms < window.end_ms {
            let hard_end_ms = start_ms.saturating_add(LOCAL_WINDOW_MS).min(window.end_ms);
            let end_ms = if hard_end_ms < window.end_ms {
                find_split(start_ms, hard_end_ms)?
                    .filter(|split_ms| *split_ms > start_ms && *split_ms <= hard_end_ms)
                    .unwrap_or(hard_end_ms)
            } else {
                hard_end_ms
            };
            windows.push(MeetingAsrWindow {
                start_ms,
                end_ms,
                speaker_id: window.speaker_id.clone(),
                overlapping: window.overlapping,
            });
            start_ms = end_ms;
        }
    }
    if windows.is_empty() {
        return Err("localDiarizationRuntimeFailed: 本地说话人时间段无效".to_string());
    }
    Ok(windows)
}

fn find_silence_split_ms(
    source: &MeetingAudioSource,
    window_start_ms: u64,
    target_ms: u64,
    cancelled: &AtomicBool,
) -> Result<Option<u64>, String> {
    let search_start_ms = target_ms
        .saturating_sub(SILENCE_SEARCH_MS)
        .max(window_start_ms.saturating_add(SILENCE_FRAME_MS));
    if search_start_ms >= target_ms {
        return Ok(None);
    }
    let pcm = source
        .read_pcm_range(search_start_ms, target_ms, cancelled)
        .map_err(|error| format!("meetingAudioReadFailed: {error:#}"))?;
    Ok(quietest_split_offset_ms(&pcm).and_then(|offset_ms| {
        let split_ms = search_start_ms.saturating_add(offset_ms);
        (split_ms > window_start_ms && split_ms <= target_ms).then_some(split_ms)
    }))
}

fn quietest_split_offset_ms(pcm: &[u8]) -> Option<u64> {
    let samples_per_frame = (16_000 * SILENCE_FRAME_MS / 1_000) as usize;
    let frame_bytes = samples_per_frame * 2;
    if pcm.len() < frame_bytes {
        return None;
    }
    let (frame_index, mean_amplitude) = pcm
        .chunks_exact(frame_bytes)
        .enumerate()
        .map(|(frame_index, frame)| {
            let amplitude_sum = frame
                .chunks_exact(2)
                .map(|bytes| i16::from_le_bytes([bytes[0], bytes[1]]).unsigned_abs() as u64)
                .sum::<u64>();
            (frame_index, amplitude_sum / samples_per_frame as u64)
        })
        .min_by_key(|(_, mean_amplitude)| *mean_amplitude)?;
    (mean_amplitude <= SILENCE_MAX_MEAN_AMPLITUDE)
        .then(|| frame_index as u64 * SILENCE_FRAME_MS + SILENCE_FRAME_MS / 2)
}

fn ensure_audio_import_retryable(state: &MeetingImportState) -> Result<(), String> {
    if !matches!(
        state.status,
        MeetingImportStatus::Failed | MeetingImportStatus::Cancelled
    ) {
        return Err("meeting audio import is not retryable".to_string());
    }
    if state.status == MeetingImportStatus::Failed
        && state
            .error_code
            .as_deref()
            .is_some_and(|code| code.starts_with("summary"))
    {
        return Err(
            "meetingAudioImportSummaryRetryRequired: ASR 已完成，请仅重试会议总结".to_string(),
        );
    }
    Ok(())
}

fn recover_interrupted_import_transition(
    record: &mut MeetingRecord,
    import_job_id: &str,
    now: &str,
) -> bool {
    let Some(state) = record.import_state.as_mut() else {
        return false;
    };
    if state.import_job_id != import_job_id || state.status.is_terminal() {
        return false;
    }
    state.status = MeetingImportStatus::Failed;
    state.progress = None;
    state.error_code = Some("meetingAudioReselectionRequired".to_string());
    state.error_message = Some("应用在导入音频时退出，请重新选择源文件后重试".to_string());
    state.updated_at = now.to_string();
    record.processing_hold = None;
    record.status = MeetingStatus::Draft;
    record.updated_at = now.to_string();
    true
}

fn recover_interrupted_summary_transition(
    record: &mut MeetingRecord,
    import_job_id: &str,
    now: &str,
) -> bool {
    let Some(state) = record.import_state.as_mut() else {
        return false;
    };
    if record.status != MeetingStatus::SummaryFailed
        || state.import_job_id != import_job_id
        || state.status != MeetingImportStatus::Summarizing
    {
        return false;
    }
    state.status = MeetingImportStatus::Failed;
    state.progress = Some(1.0);
    state.error_code = Some("summaryInterrupted".to_string());
    state.error_message = Some("应用在生成会议总结时退出，请重试总结".to_string());
    state.updated_at = now.to_string();
    state.completed_at = Some(now.to_string());
    record.processing_hold = None;
    record.updated_at = now.to_string();
    true
}

fn remove_partial_staging_file(path: &std::path::Path) {
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => log::warn!(
            "[meeting-audio-import] remove partial staging {} failed: {error}",
            path.display()
        ),
    }
}

fn profiles_from_turns(turns: &[SpeakerTurn]) -> Vec<SpeakerProfile> {
    let mut speaker_ids = Vec::<String>::new();
    for turn in turns {
        if !speaker_ids.contains(&turn.speaker_id) {
            speaker_ids.push(turn.speaker_id.clone());
        }
    }
    speaker_ids
        .into_iter()
        .map(|speaker_id| SpeakerProfile {
            display_name: speaker_display_name(&speaker_id),
            id: speaker_id,
            provider_speaker_id: None,
            manually_named: false,
        })
        .collect()
}

fn speaker_display_name(speaker_id: &str) -> String {
    speaker_id
        .strip_prefix("speaker-")
        .and_then(|index| index.parse::<usize>().ok())
        .map(|index| format!("发言人 {}", index + 1))
        .unwrap_or_else(|| "未确认".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn import_record_for_test(job_id: &str) -> MeetingRecord {
        MeetingRecord {
            id: "meeting-import-test".to_string(),
            title: "导入测试".to_string(),
            status: MeetingStatus::Draft,
            started_at: "2026-08-13T00:00:00Z".to_string(),
            ended_at: Some("2026-08-13T00:00:01Z".to_string()),
            duration_ms: Some(1_000),
            transcript_segments: Vec::new(),
            summary: MeetingSummary::default(),
            audio: MeetingAudioMeta {
                state: MeetingAudioState::Temporary,
                retained: false,
                path: None,
            },
            realtime_asr: None,
            post_processing_config: None,
            post_processing: None,
            import_config: None,
            import_state: Some(MeetingImportState {
                status: MeetingImportStatus::Importing,
                import_job_id: job_id.to_string(),
                progress: Some(0.5),
                attempt: 1,
                error_code: Some("oldError".to_string()),
                error_message: Some("old message".to_string()),
                created_at: "2026-08-13T00:00:00Z".to_string(),
                updated_at: "2026-08-13T00:00:00Z".to_string(),
                completed_at: None,
            }),
            transcript_revisions: Vec::new(),
            active_transcript_revision: None,
            speaker_profiles: Vec::new(),
            speaker_turns: Vec::new(),
            processing_hold: Some(ProcessingHold {
                job_id: job_id.to_string(),
                acquired_at: "2026-08-13T00:00:00Z".to_string(),
            }),
            created_at: "2026-08-13T00:00:00Z".to_string(),
            updated_at: "2026-08-13T00:00:00Z".to_string(),
        }
    }

    fn probe_for_test() -> (PcmWavProbe, PathBuf) {
        let dir = std::env::temp_dir().join(format!("meeting-selection-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("会议录音.wav");
        std::fs::write(&path, crate::asr::wav::encode_wav_16k_mono(&[0; 16_000])).unwrap();
        (probe_pcm_wav(&path).unwrap(), dir)
    }

    #[test]
    fn registry_only_exposes_supported_cloud_and_offline_local_models() {
        let models = list_meeting_file_asr_models();
        let cloud = models
            .iter()
            .filter(|model| model.runtime_kind == MeetingAsrRuntimeKind::Cloud)
            .collect::<Vec<_>>();
        assert_eq!(cloud.len(), 2);
        assert_eq!(cloud[0].model_id, "fun-asr");
        assert!(cloud[0].is_default);
        assert_eq!(cloud[1].model_id, "paraformer-v2");
        assert!(!cloud[1].is_default);
        assert!(models.iter().any(|model| {
            model.provider_id == LOCAL_PROVIDER_ID && model.model_id == "sense-voice-small-zh"
        }));
        assert!(!models.iter().any(|model| {
            model.provider_id == LOCAL_PROVIDER_ID
                && model.model_id == crate::asr::local::sherpa::DEFAULT_ONLINE_MODEL_ALIAS
        }));
    }

    #[test]
    fn resolver_rejects_unregistered_model_without_accepting_runtime_kind() {
        let error = resolve_meeting_asr_model(&MeetingAsrModelRef {
            provider_id: "bailian".to_string(),
            model_id: "qwen-audio-asr".to_string(),
        })
        .unwrap_err();
        assert!(error.contains("unsupported meeting file ASR model"));
    }

    #[test]
    fn selection_token_is_one_time_and_does_not_expose_source_path() {
        let (probe, dir) = probe_for_test();
        let source_path = probe.path.to_string_lossy().to_string();
        let mut registry = SelectionRegistry::default();
        let selection = registry.insert(probe, Instant::now());
        let json = serde_json::to_string(&selection).unwrap();
        assert!(!json.contains(&source_path));
        assert!(registry
            .consume(&selection.selection_token, Instant::now())
            .is_ok());
        assert!(registry
            .consume(&selection.selection_token, Instant::now())
            .unwrap_err()
            .contains("已失效"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn selection_token_expires_without_returning_source_path() {
        let (probe, dir) = probe_for_test();
        let mut registry = SelectionRegistry::default();
        let inserted_at = Instant::now();
        let selection = registry.insert(probe, inserted_at);
        let error = registry
            .consume(
                &selection.selection_token,
                inserted_at + SELECTION_TOKEN_TTL + Duration::from_secs(1),
            )
            .unwrap_err();
        assert!(error.contains("已过期"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn managed_audio_source_conflict_is_detected_without_deleting_source() {
        let (probe, dir) = probe_for_test();
        assert!(path_is_within(&probe.path, &dir));
        assert!(probe.path.exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn transition_cancellation_is_not_reported_as_import_failure() {
        assert!(is_import_cancellation_error(
            "meetingAudioImportCancelled: 音频导入任务已取消或被替换"
        ));
        assert!(!is_import_cancellation_error(
            "meetingAudioImportFailed: rename failed"
        ));
    }

    #[test]
    fn cancelled_import_transition_clears_errors_and_processing_hold() {
        let mut record = import_record_for_test("import-cancel-test");
        assert!(apply_import_cancelled_transition(
            &mut record,
            "import-cancel-test",
            "2026-08-13T00:00:02Z"
        ));
        let state = record.import_state.as_ref().unwrap();
        assert_eq!(state.status, MeetingImportStatus::Cancelled);
        assert_eq!(state.progress, None);
        assert_eq!(state.error_code, None);
        assert_eq!(state.error_message, None);
        assert_eq!(state.completed_at.as_deref(), Some("2026-08-13T00:00:02Z"));
        assert!(record.processing_hold.is_none());
    }

    #[tokio::test]
    async fn deletion_stop_signals_and_waits_for_import_worker_unregister() {
        let job_id = format!("import-delete-{}", Uuid::new_v4());
        let cancelled = register_import_job(&job_id).unwrap();
        let record = import_record_for_test(&job_id);
        assert_eq!(
            request_import_stop_for_deletion(&record).as_deref(),
            Some(job_id.as_str())
        );
        assert!(cancelled.load(Ordering::Acquire));

        let job_id_for_cleanup = job_id.clone();
        let cleanup = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            IMPORT_CANCEL_FLAGS.lock().remove(&job_id_for_cleanup);
        });
        wait_for_import_worker_exit(&job_id).await.unwrap();
        cleanup.await.unwrap();
        assert!(!IMPORT_CANCEL_FLAGS.lock().contains_key(&job_id));
    }

    #[test]
    fn local_asr_with_cloud_diarization_is_rejected_before_processing() {
        let descriptor = MeetingAsrModelDescriptor {
            provider_id: LOCAL_PROVIDER_ID.to_string(),
            model_id: "sense-voice-small-zh".to_string(),
            display_name: "SenseVoice".to_string(),
            runtime_kind: MeetingAsrRuntimeKind::Local,
            supports_meeting_file: true,
            supports_diarization: false,
            supports_speaker_count: false,
            readiness: MeetingAsrModelReadiness::Ready,
            readiness_message: None,
            is_default: false,
        };
        let error =
            validate_meeting_asr_combination(&descriptor, MeetingDiarizationMode::Cloud, None)
                .unwrap_err();
        assert!(error.contains("不会上传音频"));
    }

    #[test]
    fn two_hour_audio_is_split_into_bounded_absolute_windows() {
        let windows = bounded_windows(2 * 60 * 60 * 1000);
        assert_eq!(windows.len(), 240);
        assert_eq!(windows.first().unwrap().start_ms, 0);
        assert_eq!(windows.last().unwrap().end_ms, 7_200_000);
        assert!(windows
            .iter()
            .all(|window| window.end_ms - window.start_ms <= LOCAL_WINDOW_MS));
    }

    #[test]
    fn speaker_windows_merge_short_gaps_and_split_long_turns() {
        let turns = vec![
            SpeakerTurn {
                speaker_id: "speaker-0".to_string(),
                start_ms: 0,
                end_ms: 10_000,
                confidence: None,
                overlapping: false,
            },
            SpeakerTurn {
                speaker_id: "speaker-0".to_string(),
                start_ms: 10_300,
                end_ms: 25_000,
                confidence: None,
                overlapping: true,
            },
            SpeakerTurn {
                speaker_id: "speaker-1".to_string(),
                start_ms: 26_000,
                end_ms: 90_000,
                confidence: None,
                overlapping: false,
            },
        ];
        let windows = build_speaker_windows(&turns, 90_000, |_, _| Ok(None)).unwrap();
        assert_eq!(windows[0].start_ms, 0);
        assert_eq!(windows[0].end_ms, 25_000);
        assert!(windows[0].overlapping);
        assert_eq!(windows[1].speaker_id.as_deref(), Some("speaker-1"));
        assert_eq!(windows.last().unwrap().end_ms, 90_000);
        assert!(windows
            .iter()
            .all(|window| window.end_ms - window.start_ms <= LOCAL_WINDOW_MS));
    }

    #[test]
    fn speaker_windows_prefer_a_nearby_silence_split_and_keep_absolute_timestamps() {
        let turns = vec![SpeakerTurn {
            speaker_id: "speaker-0".to_string(),
            start_ms: 10_000,
            end_ms: 80_000,
            confidence: None,
            overlapping: false,
        }];
        let mut requested_targets = Vec::new();

        let windows = build_speaker_windows(&turns, 80_000, |start_ms, target_ms| {
            requested_targets.push((start_ms, target_ms));
            Ok(Some(target_ms - 1_000))
        })
        .unwrap();

        assert_eq!(requested_targets, vec![(10_000, 40_000), (39_000, 69_000)]);
        assert_eq!(windows[0].start_ms, 10_000);
        assert_eq!(windows[0].end_ms, 39_000);
        assert_eq!(windows[1].start_ms, 39_000);
        assert_eq!(windows[1].end_ms, 68_000);
        assert_eq!(windows[2].start_ms, 68_000);
        assert_eq!(windows[2].end_ms, 80_000);
        assert!(windows
            .iter()
            .all(|window| window.end_ms - window.start_ms <= LOCAL_WINDOW_MS));
    }

    #[test]
    fn silence_detector_uses_the_quietest_twenty_millisecond_frame() {
        let samples_per_frame = (16_000 * SILENCE_FRAME_MS / 1_000) as usize;
        let mut pcm = Vec::new();
        for amplitude in [2_000i16, 1_500, 100, 1_000] {
            for _ in 0..samples_per_frame {
                pcm.extend_from_slice(&amplitude.to_le_bytes());
            }
        }

        assert_eq!(quietest_split_offset_ms(&pcm), Some(50));

        let loud_pcm = 2_000i16.to_le_bytes().repeat(samples_per_frame * 2);
        assert_eq!(quietest_split_offset_ms(&loud_pcm), None);
    }

    #[test]
    fn interrupted_import_removes_partial_and_requires_reselection() {
        let job_id = format!("import-recovery-{}", Uuid::new_v4());
        let partial_path = std::env::temp_dir().join(format!("{job_id}.partial"));
        std::fs::write(&partial_path, b"partial").unwrap();
        let mut record = import_record_for_test(&job_id);

        remove_partial_staging_file(&partial_path);
        assert!(recover_interrupted_import_transition(
            &mut record,
            &job_id,
            "2026-08-13T00:00:03Z"
        ));

        assert!(!partial_path.exists());
        let state = record.import_state.as_ref().unwrap();
        assert_eq!(state.status, MeetingImportStatus::Failed);
        assert_eq!(
            state.error_code.as_deref(),
            Some("meetingAudioReselectionRequired")
        );
        assert!(record.processing_hold.is_none());
    }

    #[test]
    fn interrupted_summary_releases_hold_and_only_allows_summary_retry() {
        let job_id = "import-summary-recovery";
        let mut record = import_record_for_test(job_id);
        record.status = MeetingStatus::SummaryFailed;
        let state = record.import_state.as_mut().unwrap();
        state.status = MeetingImportStatus::Summarizing;
        state.progress = Some(1.0);

        assert!(recover_interrupted_summary_transition(
            &mut record,
            job_id,
            "2026-08-13T00:00:04Z"
        ));

        let state = record.import_state.as_ref().unwrap();
        assert_eq!(state.status, MeetingImportStatus::Failed);
        assert_eq!(state.error_code.as_deref(), Some("summaryInterrupted"));
        assert!(record.processing_hold.is_none());
        assert!(ensure_audio_import_retryable(state)
            .unwrap_err()
            .contains("仅重试会议总结"));
    }
}

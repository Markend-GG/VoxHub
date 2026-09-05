use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use tauri::Emitter;
use uuid::Uuid;

use crate::asr::{
    AsrDraftSegment, AsrDraftSegmentSink, AsrFinalSegment, AsrFinalSegmentSink,
    AsrInterruptionSink, AsrSessionMetadata, RawTranscript,
};
use crate::coordinator_state::SessionPhase;
use crate::persistence::{
    meeting_recording_part_path_for_id, CredentialsVault, MeetingStore, PreferencesStore,
};
use crate::recorder::{Recorder, RecorderError};
use crate::types::{
    MeetingAsrMode, MeetingAudioLevelEvent, MeetingAudioMeta, MeetingAudioState, MeetingErrorEvent,
    MeetingRealtimeAsrSnapshot, MeetingRecord, MeetingRecordingPhase, MeetingRecordingSnapshot,
    MeetingStatus, MeetingSummary, MeetingTranscriptDraftEvent, MeetingTranscriptSegmentEvent,
    MeetingVadSilencePreset, StartMeetingRecordingOptions, TranscriptSegment,
    TranscriptSegmentMetadata, TranscriptSegmentSource,
};

use super::meeting_post_processing::{
    lock_realtime_model_in_post_processing_config, prepare_post_processing_after_stop,
    resolve_initial_post_processing_config, spawn_post_processing_job,
};
use super::{
    acquire_recording_mute, asr_transcribe_uses_global_timeout,
    build_meeting_asr_start_with_options, cancel_active_asr, ensure_asr_credentials_for_provider,
    ensure_microphone_permission, release_recording_mute, selected_microphone_device_name,
    stop_microphone_preview_monitor, ActiveAsr, AsrCallLabel, Inner, MeetingAsrStartOptions,
    QaAsrStart, COORDINATOR_GLOBAL_TIMEOUT_SECS,
};

const MEETING_AUDIO_LEVEL_INTERVAL: Duration = Duration::from_millis(100);
const MEETING_STATE_TICK_INTERVAL: Duration = Duration::from_millis(500);

struct DiscardingMeetingAudioConsumer;

impl crate::recorder::AudioConsumer for DiscardingMeetingAudioConsumer {
    fn consume_pcm_chunk(&self, _pcm: &[u8]) {}
}

#[derive(Debug, Default)]
struct MeetingAudioLevelThrottle {
    last_emit_elapsed: Option<Duration>,
}

impl MeetingAudioLevelThrottle {
    fn should_emit(&mut self, elapsed: Duration) -> bool {
        if matches!(
            self.last_emit_elapsed,
            Some(previous) if elapsed.saturating_sub(previous) < MEETING_AUDIO_LEVEL_INTERVAL
        ) {
            return false;
        }
        self.last_emit_elapsed = Some(elapsed);
        true
    }
}

fn normalize_meeting_audio_level(level: f32) -> f32 {
    if level.is_finite() {
        level.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

fn try_queue_meeting_audio_level(sender: &mpsc::SyncSender<f32>, level: f32) -> bool {
    sender
        .try_send(normalize_meeting_audio_level(level))
        .is_ok()
}

fn meeting_audio_level_delivery_allowed(
    meeting_matches: bool,
    recording: bool,
    recorder_active: bool,
    companion_visible: bool,
) -> bool {
    meeting_matches && recording && recorder_active && companion_visible
}

#[cfg(not(mobile))]
fn meeting_companion_audio_level_reporting_enabled() -> bool {
    crate::meeting_companion::audio_level_reporting_enabled()
}

#[cfg(mobile)]
fn meeting_companion_audio_level_reporting_enabled() -> bool {
    false
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MeetingSessionPhase {
    Recording,
    Paused,
}

#[derive(Debug, Clone)]
pub(super) struct MeetingSession {
    record: MeetingRecord,
    phase: MeetingSessionPhase,
    started_at: DateTime<Utc>,
    paused_at: Option<DateTime<Utc>>,
    accumulated_paused_ms: u64,
    next_segment_index: u64,
    asr_interrupted: bool,
    active_provider: String,
    silence_preset: MeetingVadSilencePreset,
    model_override: Option<String>,
    active_provider_session_id: Option<String>,
    active_audio_part_index: Option<u32>,
    active_session_start_ms: Option<u64>,
}

impl MeetingSession {
    #[cfg(test)]
    pub(super) fn new(
        meeting_id: String,
        started_at: DateTime<Utc>,
        active_provider: String,
    ) -> Self {
        Self::new_with_asr_settings(
            meeting_id,
            started_at,
            active_provider,
            MeetingVadSilencePreset::Standard,
            None,
        )
    }

    pub(super) fn new_with_asr_settings(
        meeting_id: String,
        started_at: DateTime<Utc>,
        active_provider: String,
        silence_preset: MeetingVadSilencePreset,
        model_override: Option<String>,
    ) -> Self {
        let timestamp = started_at.to_rfc3339();
        let title = started_at.format("会议记录 %Y-%m-%d %H:%M").to_string();
        Self {
            record: MeetingRecord {
                id: meeting_id,
                title,
                status: MeetingStatus::Recording,
                started_at: timestamp.clone(),
                ended_at: None,
                duration_ms: None,
                transcript_segments: Vec::new(),
                summary: MeetingSummary::default(),
                organized_draft: None,
                organized_draft_state: None,
                audio: MeetingAudioMeta {
                    state: MeetingAudioState::Temporary,
                    retained: false,
                    path: None,
                },
                realtime_asr: None,
                post_processing_config: None,
                post_processing: None,
                import_config: None,
                import_state: None,
                transcript_revisions: Vec::new(),
                active_transcript_revision: None,
                speaker_profiles: Vec::new(),
                speaker_turns: Vec::new(),
                processing_hold: None,
                created_at: timestamp.clone(),
                updated_at: timestamp,
            },
            phase: MeetingSessionPhase::Recording,
            started_at,
            paused_at: None,
            accumulated_paused_ms: 0,
            next_segment_index: 1,
            asr_interrupted: false,
            active_provider,
            silence_preset,
            model_override,
            active_provider_session_id: None,
            active_audio_part_index: None,
            active_session_start_ms: None,
        }
    }

    #[cfg(test)]
    pub(super) fn phase(&self) -> MeetingSessionPhase {
        self.phase
    }

    pub(super) fn elapsed_ms_at(&self, now: DateTime<Utc>) -> u64 {
        let total_ms = duration_ms_between(self.started_at, now);
        let current_pause_ms = self
            .paused_at
            .map(|paused_at| duration_ms_between(paused_at, now))
            .unwrap_or(0);
        total_ms
            .saturating_sub(self.accumulated_paused_ms)
            .saturating_sub(current_pause_ms)
    }

    #[cfg(test)]
    pub(super) fn ensure_can_start_another(&self) -> Result<(), String> {
        Err("meeting recording already active".to_string())
    }

    pub(super) fn record(&self) -> &MeetingRecord {
        &self.record
    }

    pub(super) fn record_mut(&mut self) -> &mut MeetingRecord {
        &mut self.record
    }

    pub(super) fn active_provider(&self) -> &str {
        &self.active_provider
    }

    pub(super) fn silence_preset(&self) -> MeetingVadSilencePreset {
        self.silence_preset.clone()
    }

    pub(super) fn model_override(&self) -> Option<String> {
        self.model_override.clone()
    }

    fn lock_realtime_asr_label(&mut self, label: &AsrCallLabel, now: DateTime<Utc>) {
        self.model_override = label.model.clone();
        self.record.realtime_asr = Some(MeetingRealtimeAsrSnapshot {
            provider_id: self.active_provider.clone(),
            resolved_provider_id: label.provider.clone(),
            model_id: label.model.clone(),
            silence_preset: self.silence_preset.clone(),
        });
        lock_realtime_model_in_post_processing_config(
            &mut self.record,
            &label.provider,
            label.model.clone(),
        );
        self.record.updated_at = now.to_rfc3339();
    }

    pub(super) fn set_active_asr_session(
        &mut self,
        provider_session_id: String,
        audio_part_index: u32,
        session_start_ms: u64,
    ) {
        self.active_provider_session_id = Some(provider_session_id);
        self.active_audio_part_index = Some(audio_part_index);
        self.active_session_start_ms = Some(session_start_ms);
    }

    pub(super) fn clear_active_asr_session(&mut self) {
        self.active_provider_session_id = None;
        self.active_audio_part_index = None;
        self.active_session_start_ms = None;
    }

    fn current_asr_metadata(&self) -> AsrSessionMetadata {
        AsrSessionMetadata {
            provider_session_id: self.active_provider_session_id.clone(),
            audio_part_index: self.active_audio_part_index,
            session_start_ms: self.active_session_start_ms,
        }
    }

    pub(super) fn mark_persisted_now(&mut self, now: DateTime<Utc>) {
        self.record.updated_at = now.to_rfc3339();
    }

    pub(super) fn pause(&mut self, now: DateTime<Utc>) -> Result<(), String> {
        if self.phase != MeetingSessionPhase::Recording {
            return Err("meeting recording is not active".into());
        }
        self.phase = MeetingSessionPhase::Paused;
        self.paused_at = Some(now);
        self.record.status = if self.asr_interrupted {
            MeetingStatus::TranscribingInterrupted
        } else {
            MeetingStatus::Paused
        };
        self.mark_persisted_now(now);
        Ok(())
    }

    pub(super) fn ensure_can_resume(&self) -> Result<(), String> {
        if self.phase == MeetingSessionPhase::Paused {
            Ok(())
        } else if self.asr_interrupted {
            Err("meeting recording already continues after ASR interruption".into())
        } else {
            Err("meeting recording is already active".into())
        }
    }

    pub(super) fn resume(&mut self, now: DateTime<Utc>) -> Result<(), String> {
        match self.phase {
            MeetingSessionPhase::Paused => {
                if let Some(paused_at) = self.paused_at.take() {
                    self.accumulated_paused_ms = self
                        .accumulated_paused_ms
                        .saturating_add(duration_ms_between(paused_at, now));
                }
            }
            MeetingSessionPhase::Recording if self.asr_interrupted => {}
            MeetingSessionPhase::Recording => {
                return Err("meeting recording is already active".into())
            }
        }
        self.phase = MeetingSessionPhase::Recording;
        if self.asr_interrupted {
            self.record.status = MeetingStatus::TranscribingInterrupted;
        } else {
            self.record.status = MeetingStatus::Recording;
        }
        self.mark_persisted_now(now);
        Ok(())
    }

    pub(super) fn mark_asr_interrupted(&mut self, now: DateTime<Utc>) {
        self.asr_interrupted = true;
        self.record.status = MeetingStatus::TranscribingInterrupted;
        self.mark_persisted_now(now);
    }

    pub(super) fn append_transcript_segment(
        &mut self,
        text: &str,
        start_ms: Option<u64>,
        end_ms: Option<u64>,
        observed_at: DateTime<Utc>,
        metadata: Option<TranscriptSegmentMetadata>,
    ) -> Option<TranscriptSegment> {
        let text = text.trim();
        if text.is_empty() {
            return None;
        }
        let fallback_end_ms = self.elapsed_ms_at(observed_at);
        let end_ms = end_ms.or(Some(fallback_end_ms));
        let segment = TranscriptSegment {
            id: format!("seg-{:06}", self.next_segment_index),
            speaker_id: None,
            speaker_label: "未区分".to_string(),
            start_ms: start_ms.unwrap_or_else(|| {
                self.record
                    .transcript_segments
                    .last()
                    .and_then(|segment| segment.end_ms)
                    .unwrap_or(0)
            }),
            end_ms,
            text: text.to_string(),
            source: TranscriptSegmentSource::RealtimeAsr,
            metadata,
        };
        self.next_segment_index = self.next_segment_index.saturating_add(1);
        self.record.transcript_segments.push(segment.clone());
        Some(segment)
    }

    pub(super) fn finish(&mut self, ended_at: DateTime<Utc>, interrupted: bool) {
        if let Some(paused_at) = self.paused_at.take() {
            self.accumulated_paused_ms = self
                .accumulated_paused_ms
                .saturating_add(duration_ms_between(paused_at, ended_at));
        }
        let total_ms = duration_ms_between(self.started_at, ended_at);
        self.record.ended_at = Some(ended_at.to_rfc3339());
        self.record.duration_ms = Some(total_ms.saturating_sub(self.accumulated_paused_ms));
        self.record.status = if interrupted || self.asr_interrupted {
            MeetingStatus::TranscribingInterrupted
        } else {
            MeetingStatus::Completed
        };
        self.mark_persisted_now(ended_at);
    }

    pub(super) fn snapshot(&self, now: DateTime<Utc>) -> MeetingRecordingSnapshot {
        MeetingRecordingSnapshot {
            meeting: self.record.clone(),
            phase: match self.phase {
                MeetingSessionPhase::Recording if self.asr_interrupted => {
                    MeetingRecordingPhase::TranscribingInterrupted
                }
                MeetingSessionPhase::Recording => MeetingRecordingPhase::Recording,
                MeetingSessionPhase::Paused => MeetingRecordingPhase::Paused,
            },
            elapsed_ms: self.elapsed_ms_at(now),
            active_asr_provider: self.active_provider.clone(),
            active_provider_session_id: self.active_provider_session_id.clone(),
            asr_interrupted: self.asr_interrupted,
        }
    }
}

pub(super) fn new_meeting_id() -> String {
    Uuid::new_v4().to_string()
}

pub(super) fn has_active_meeting(inner: &Arc<Inner>) -> bool {
    inner.meeting_session.lock().is_some()
}

#[derive(Debug, Clone)]
struct EffectiveMeetingAsr {
    provider_id: String,
    silence_preset: MeetingVadSilencePreset,
    model_override: Option<String>,
}

fn meeting_model_override_for_provider(
    settings: &crate::types::MeetingAsrSettings,
    provider_id: &str,
) -> Option<String> {
    if settings.mode != MeetingAsrMode::InheritGlobal {
        return None;
    }
    let model_provider_id = settings
        .model_provider_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())?;
    if model_provider_id != provider_id {
        return None;
    }
    settings
        .model_override
        .as_deref()
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .map(ToOwned::to_owned)
}

fn resolve_effective_meeting_asr(inner: &Arc<Inner>) -> EffectiveMeetingAsr {
    let prefs = inner.prefs.get();
    let provider_id = match prefs.meeting_asr.mode {
        MeetingAsrMode::ProviderSpecific => prefs
            .meeting_asr
            .provider_id
            .as_deref()
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .map(ToOwned::to_owned)
            .unwrap_or_else(CredentialsVault::get_active_asr),
        MeetingAsrMode::InheritGlobal => CredentialsVault::get_active_asr(),
    };
    EffectiveMeetingAsr {
        model_override: meeting_model_override_for_provider(&prefs.meeting_asr, &provider_id),
        provider_id,
        silence_preset: prefs.meeting_asr.silence_preset,
    }
}

fn new_provider_session_id() -> String {
    Uuid::new_v4().to_string()
}

fn next_meeting_audio_part_index(inner: &Arc<Inner>) -> u32 {
    (*inner.meeting_next_part_index.lock()).min(u32::MAX as u64) as u32
}

pub(super) async fn start_meeting_recording(
    inner: &Arc<Inner>,
    options: Option<StartMeetingRecordingOptions>,
) -> Result<MeetingRecordingSnapshot, String> {
    if inner.meeting_session.lock().is_some() {
        return Err("meeting recording already active".to_string());
    }
    if !matches!(inner.state.lock().phase, SessionPhase::Idle) {
        return Err("dictation is active".to_string());
    }
    let effective_asr = resolve_effective_meeting_asr(inner);
    let post_processing_config = resolve_initial_post_processing_config(
        &inner.prefs.get(),
        options.as_ref(),
        &effective_asr.provider_id,
        effective_asr.model_override.clone(),
    )?;
    ensure_asr_credentials_for_provider(&effective_asr.provider_id, true)?;
    ensure_microphone_permission(inner)?;

    let meeting_id = new_meeting_id();
    let started_at = Utc::now();
    let audio_part_index = next_meeting_audio_part_index(inner);
    let provider_session_id = new_provider_session_id();
    let mut session = MeetingSession::new_with_asr_settings(
        meeting_id.clone(),
        started_at,
        effective_asr.provider_id.clone(),
        effective_asr.silence_preset.clone(),
        effective_asr.model_override.clone(),
    );
    session.record_mut().post_processing_config = Some(post_processing_config);
    session.set_active_asr_session(provider_session_id.clone(), audio_part_index, 0);
    MeetingStore::new()
        .map_err(|e| e.to_string())?
        .create(session.record().clone())
        .map_err(|e| e.to_string())?;
    session.mark_persisted_now(started_at);
    *inner.meeting_session.lock() = Some(session);
    *inner.meeting_segment_count_at_asr_start.lock() = 0;
    reset_meeting_audio_archive_state(inner);

    let asr_release_token =
        prepare_meeting_asr_release_token_for_provider(inner, &effective_asr.provider_id);
    let sink = meeting_final_segment_sink(inner, meeting_id.clone());
    let draft_sink = meeting_draft_segment_sink(inner, meeting_id.clone());
    let interruption_sink = meeting_asr_interruption_sink(
        inner,
        meeting_id.clone(),
        effective_asr.provider_id.clone(),
        Some(provider_session_id.clone()),
    );
    let (asr_start, asr_label) = match build_meeting_asr_start(
        inner,
        MeetingAsrStartOptions {
            provider_id: effective_asr.provider_id.clone(),
            final_segment_sink: Some(sink),
            draft_segment_sink: Some(draft_sink),
            interruption_sink: Some(interruption_sink),
            session_metadata: AsrSessionMetadata {
                provider_session_id: Some(provider_session_id),
                audio_part_index: Some(audio_part_index),
                session_start_ms: Some(0),
            },
            silence_preset: effective_asr.silence_preset,
            model_override: effective_asr.model_override.clone(),
        },
    )
    .await
    {
        Ok(asr_start) => asr_start,
        Err(error) => {
            schedule_meeting_local_asr_release_for_provider(
                inner,
                &effective_asr.provider_id,
                asr_release_token.clone(),
            );
            mark_start_failed_record(inner, &meeting_id, &error)?;
            return Err(error);
        }
    };

    let label_persist_result = {
        let mut session_guard = inner.meeting_session.lock();
        let session = session_guard
            .as_mut()
            .ok_or_else(|| "meeting recording not active".to_string())?;
        commit_meeting_realtime_asr_label(session, &asr_label, Utc::now(), persist_meeting_record)
    };
    if let Err(error) = label_persist_result {
        cleanup_unstored_meeting_asr_start(inner, &asr_start, asr_release_token.clone());
        mark_start_failed_record(inner, &meeting_id, &error)?;
        return Err(error);
    }
    let (asr_start, asr_interruption) = match asr_start.open_streaming_session().await {
        Ok(()) => (Some(asr_start), None),
        Err(error) => {
            cleanup_unstored_meeting_asr_start(inner, &asr_start, asr_release_token.clone());
            (None, Some(error))
        }
    };
    if asr_interruption.is_some() {
        let result = {
            let mut session_guard = inner.meeting_session.lock();
            let session = session_guard
                .as_mut()
                .ok_or_else(|| "meeting recording not active".to_string())?;
            commit_meeting_asr_interruption(session, Utc::now(), persist_meeting_record)
        };
        if let Err(error) = result {
            mark_start_failed_record(inner, &meeting_id, &error)?;
            return Err(error);
        }
    }
    let recorder_consumer = asr_start
        .as_ref()
        .map(QaAsrStart::recorder_consumer)
        .unwrap_or_else(|| Arc::new(DiscardingMeetingAudioConsumer));
    let recorder = match start_meeting_recorder(inner, &meeting_id, recorder_consumer).await {
        Ok(recorder) => recorder,
        Err(error) => {
            if let Some(asr_start) = asr_start.as_ref() {
                cleanup_unstored_meeting_asr_start(inner, asr_start, asr_release_token.clone());
            }
            mark_start_failed_record(inner, &meeting_id, &error)?;
            return Err(error);
        }
    };

    *inner.meeting_asr.lock() = asr_start.as_ref().map(QaAsrStart::active_asr);
    *inner.meeting_recorder.lock() = Some(recorder);

    let snapshot = meeting_snapshot(inner, Utc::now())?
        .ok_or_else(|| "meeting recording not active".to_string())?;
    emit_meeting_state(inner, &snapshot);
    if let Some(error) = asr_interruption {
        emit_meeting_error(inner, Some(meeting_id), "asrInterrupted", &error);
    }
    Ok(snapshot)
}

pub(super) async fn pause_meeting_recording(
    inner: &Arc<Inner>,
    meeting_id: &str,
) -> Result<MeetingRecordingSnapshot, String> {
    ensure_active_meeting_id(inner, meeting_id)?;
    stop_meeting_recorder(inner);
    let flush_result = flush_current_meeting_asr(inner).await;
    emit_meeting_transcript_draft_clear(inner, meeting_id);
    {
        let mut session_guard = inner.meeting_session.lock();
        let session = session_guard
            .as_mut()
            .ok_or_else(|| "meeting recording not active".to_string())?;
        let now = Utc::now();
        match flush_result {
            Ok(Some(outcome)) => {
                let session_start_count = *inner.meeting_segment_count_at_asr_start.lock();
                if let Some(error) =
                    apply_meeting_asr_flush_outcome(session, &outcome, now, session_start_count)
                {
                    emit_meeting_error(
                        inner,
                        Some(meeting_id.to_string()),
                        "asrInterrupted",
                        &error,
                    );
                }
            }
            Ok(None) => {}
            Err(error) => {
                session.mark_asr_interrupted(now);
                emit_meeting_error(
                    inner,
                    Some(meeting_id.to_string()),
                    "asrInterrupted",
                    &error,
                );
            }
        }
        session.pause(now)?;
        session.clear_active_asr_session();
        persist_meeting_record(session.record())?;
    }
    let snapshot = meeting_snapshot(inner, Utc::now())?
        .ok_or_else(|| "meeting recording not active".to_string())?;
    emit_meeting_state(inner, &snapshot);
    Ok(snapshot)
}

pub(super) async fn resume_meeting_recording(
    inner: &Arc<Inner>,
    meeting_id: &str,
) -> Result<MeetingRecordingSnapshot, String> {
    ensure_active_meeting_id(inner, meeting_id)?;
    ensure_meeting_can_resume(inner)?;
    let (
        active_provider,
        silence_preset,
        model_override,
        provider_session_id,
        audio_part_index,
        session_start_ms,
    ) = {
        let guard = inner.meeting_session.lock();
        let session = guard
            .as_ref()
            .ok_or_else(|| "meeting recording not active".to_string())?;
        let now = Utc::now();
        (
            session.active_provider().to_string(),
            session.silence_preset(),
            session.model_override(),
            new_provider_session_id(),
            next_meeting_audio_part_index(inner),
            session.elapsed_ms_at(now),
        )
    };
    let asr_release_token = prepare_meeting_asr_release_token_for_provider(inner, &active_provider);
    let sink = meeting_final_segment_sink(inner, meeting_id.to_string());
    let draft_sink = meeting_draft_segment_sink(inner, meeting_id.to_string());
    let interruption_sink = meeting_asr_interruption_sink(
        inner,
        meeting_id.to_string(),
        active_provider.clone(),
        Some(provider_session_id.clone()),
    );
    let (asr_start, _asr_label) = match build_meeting_asr_start(
        inner,
        MeetingAsrStartOptions {
            provider_id: active_provider.clone(),
            final_segment_sink: Some(sink),
            draft_segment_sink: Some(draft_sink),
            interruption_sink: Some(interruption_sink),
            session_metadata: AsrSessionMetadata {
                provider_session_id: Some(provider_session_id.clone()),
                audio_part_index: Some(audio_part_index),
                session_start_ms: Some(session_start_ms),
            },
            silence_preset: silence_preset.clone(),
            model_override,
        },
    )
    .await
    {
        Ok(asr_start) => asr_start,
        Err(error) => {
            schedule_meeting_local_asr_release_for_provider(
                inner,
                &active_provider,
                asr_release_token.clone(),
            );
            return Err(error);
        }
    };
    let (asr_start, asr_interruption) = match asr_start.open_streaming_session().await {
        Ok(()) => (Some(asr_start), None),
        Err(error) => {
            cleanup_unstored_meeting_asr_start(inner, &asr_start, asr_release_token.clone());
            (None, Some(error))
        }
    };
    let recorder_consumer = asr_start
        .as_ref()
        .map(QaAsrStart::recorder_consumer)
        .unwrap_or_else(|| Arc::new(DiscardingMeetingAudioConsumer));
    let recorder = match start_meeting_recorder(inner, meeting_id, recorder_consumer).await {
        Ok(recorder) => recorder,
        Err(error) => {
            if let Some(asr_start) = asr_start.as_ref() {
                cleanup_unstored_meeting_asr_start(inner, asr_start, asr_release_token.clone());
            }
            return Err(error);
        }
    };
    let commit_result = {
        let mut session_guard = inner.meeting_session.lock();
        match session_guard.as_mut() {
            Some(session) => {
                let now = Utc::now();
                commit_meeting_resume(
                    session,
                    now,
                    provider_session_id,
                    audio_part_index,
                    session_start_ms,
                    asr_start.is_some(),
                    persist_meeting_record,
                )
            }
            None => Err("meeting recording not active".to_string()),
        }
    };
    match commit_result {
        Ok(segment_count) => {
            *inner.meeting_segment_count_at_asr_start.lock() = segment_count;
        }
        Err(error) => {
            recorder.stop();
            release_recording_mute(inner, "meeting");
            if let Some(asr_start) = asr_start.as_ref() {
                cleanup_unstored_meeting_asr_start(inner, asr_start, asr_release_token);
            }
            return Err(error);
        }
    }
    *inner.meeting_asr.lock() = asr_start.as_ref().map(QaAsrStart::active_asr);
    *inner.meeting_recorder.lock() = Some(recorder);
    let snapshot = meeting_snapshot(inner, Utc::now())?
        .ok_or_else(|| "meeting recording not active".to_string())?;
    emit_meeting_state(inner, &snapshot);
    if let Some(error) = asr_interruption {
        emit_meeting_error(
            inner,
            Some(meeting_id.to_string()),
            "asrInterrupted",
            &error,
        );
    }
    Ok(snapshot)
}

fn ensure_meeting_can_resume(inner: &Arc<Inner>) -> Result<(), String> {
    let guard = inner.meeting_session.lock();
    let session = guard
        .as_ref()
        .ok_or_else(|| "meeting recording not active".to_string())?;
    session.ensure_can_resume()
}

pub(super) async fn stop_meeting_recording(
    inner: &Arc<Inner>,
    meeting_id: &str,
) -> Result<MeetingRecord, String> {
    ensure_active_meeting_id(inner, meeting_id)?;
    stop_meeting_recorder(inner);
    if let Some(mut snapshot) = meeting_snapshot(inner, Utc::now())? {
        snapshot.phase = MeetingRecordingPhase::Stopping;
        emit_meeting_state(inner, &snapshot);
    }
    let flush_result = flush_current_meeting_asr(inner).await;
    emit_meeting_transcript_draft_clear(inner, meeting_id);

    let now = Utc::now();
    let (record, completed_provider, post_processing_started) = {
        let mut session_guard = inner.meeting_session.lock();
        let session = session_guard
            .as_mut()
            .ok_or_else(|| "meeting recording not active".to_string())?;
        let completed_provider = session.active_provider().to_string();

        let mut interrupted = false;
        match flush_result {
            Ok(Some(outcome)) => {
                let session_start_count = *inner.meeting_segment_count_at_asr_start.lock();
                if let Some(error) =
                    apply_meeting_asr_flush_outcome(session, &outcome, now, session_start_count)
                {
                    interrupted = true;
                    emit_meeting_error(
                        inner,
                        Some(meeting_id.to_string()),
                        "asrInterrupted",
                        &error,
                    );
                }
            }
            Ok(None) => {}
            Err(error) => {
                interrupted = true;
                session.mark_asr_interrupted(now);
                emit_meeting_error(
                    inner,
                    Some(meeting_id.to_string()),
                    "asrInterrupted",
                    &error,
                );
            }
        }

        session.finish(now, interrupted);
        session.clear_active_asr_session();
        let post_processing_started =
            prepare_post_processing_after_stop(session.record_mut(), &now.to_rfc3339())?;
        if let Some(error) = apply_meeting_audio_retention_state(inner, session.record_mut()) {
            emit_meeting_error(
                inner,
                Some(meeting_id.to_string()),
                "audioRetentionFailed",
                &error,
            );
        }
        (
            session.record().clone(),
            completed_provider,
            post_processing_started,
        )
    };
    persist_meeting_record(&record)?;
    if let Err(error) = prune_meeting_audio_retention_with_current_preference() {
        emit_meeting_error(
            inner,
            Some(meeting_id.to_string()),
            "audioRetentionPruneFailed",
            &error,
        );
        log::warn!("[meeting] audio retention prune failed: {error}");
    }

    clear_active_meeting_runtime(inner);
    if post_processing_started {
        if let Some(job_id) = record
            .post_processing
            .as_ref()
            .map(|state| state.job_id.clone())
        {
            spawn_post_processing_job(inner, meeting_id.to_string(), job_id);
        }
    }
    emit_meeting_state(
        inner,
        &MeetingRecordingSnapshot {
            meeting: record.clone(),
            phase: if record.status == MeetingStatus::TranscribingInterrupted {
                MeetingRecordingPhase::TranscribingInterrupted
            } else {
                MeetingRecordingPhase::Stopping
            },
            elapsed_ms: record.duration_ms.unwrap_or_default(),
            active_asr_provider: completed_provider,
            active_provider_session_id: None,
            asr_interrupted: record.status == MeetingStatus::TranscribingInterrupted,
        },
    );
    Ok(record)
}

pub(super) fn active_meeting_recording(
    inner: &Arc<Inner>,
) -> Result<Option<MeetingRecordingSnapshot>, String> {
    meeting_snapshot(inner, Utc::now())
}

fn meeting_snapshot(
    inner: &Arc<Inner>,
    now: DateTime<Utc>,
) -> Result<Option<MeetingRecordingSnapshot>, String> {
    Ok(inner
        .meeting_session
        .lock()
        .as_ref()
        .map(|session| session.snapshot(now)))
}

fn duration_ms_between(start: DateTime<Utc>, end: DateTime<Utc>) -> u64 {
    end.signed_duration_since(start).num_milliseconds().max(0) as u64
}

async fn build_meeting_asr_start(
    inner: &Arc<Inner>,
    options: MeetingAsrStartOptions,
) -> Result<(QaAsrStart, AsrCallLabel), String> {
    build_meeting_asr_start_with_options(inner, options).await
}

async fn start_meeting_recorder(
    inner: &Arc<Inner>,
    meeting_id: &str,
    consumer: Arc<dyn crate::recorder::AudioConsumer>,
) -> Result<Recorder, String> {
    let part_index = *inner.meeting_next_part_index.lock();
    let path =
        meeting_recording_part_path_for_id(meeting_id, part_index).map_err(|e| e.to_string())?;
    let microphone_device_name = selected_microphone_device_name(inner);
    let (level_sender, level_receiver) = mpsc::sync_channel(1);
    spawn_meeting_audio_level_reporter(inner, meeting_id.to_string(), level_receiver);
    let level_handler: Arc<dyn Fn(f32) + Send + Sync> = Arc::new(move |level| {
        let _ = try_queue_meeting_audio_level(&level_sender, level);
    });

    stop_microphone_preview_monitor(inner, "meeting recorder");
    acquire_recording_mute(inner, "meeting").await;
    match Recorder::start(microphone_device_name, consumer, level_handler, Some(path)) {
        Ok((recorder, runtime_errors, archive_active)) => {
            record_meeting_audio_archive_result(inner, archive_active);
            *inner.meeting_next_part_index.lock() = part_index.saturating_add(1);
            spawn_meeting_recorder_error_monitor(inner, runtime_errors);
            Ok(recorder)
        }
        Err(error) => {
            release_recording_mute(inner, "meeting");
            Err(error.to_string())
        }
    }
}

fn spawn_meeting_audio_level_reporter(
    inner: &Arc<Inner>,
    meeting_id: String,
    receiver: mpsc::Receiver<f32>,
) {
    let inner = Arc::clone(inner);
    if let Err(error) = std::thread::Builder::new()
        .name("openless-meeting-audio-level".into())
        .spawn(move || {
            let started = Instant::now();
            let mut audio_throttle = MeetingAudioLevelThrottle::default();
            let mut last_state_tick: Option<Duration> = None;

            for level in receiver {
                let elapsed = started.elapsed();
                if !matches!(
                    last_state_tick,
                    Some(previous) if elapsed.saturating_sub(previous) < MEETING_STATE_TICK_INTERVAL
                ) {
                    last_state_tick = Some(elapsed);
                    if inner.meeting_recorder.lock().is_some() {
                        if let Ok(Some(snapshot)) = meeting_snapshot(&inner, Utc::now()) {
                            emit_meeting_state(&inner, &snapshot);
                        }
                    }
                }

                if !audio_throttle.should_emit(elapsed) {
                    continue;
                }
                let (meeting_matches, recording) = {
                    let guard = inner.meeting_session.lock();
                    guard
                        .as_ref()
                        .map(|session| {
                            (
                                session.record().id == meeting_id,
                                session.phase == MeetingSessionPhase::Recording,
                            )
                        })
                        .unwrap_or((false, false))
                };
                let recorder_active = inner.meeting_recorder.lock().is_some();
                if !meeting_audio_level_delivery_allowed(
                    meeting_matches,
                    recording,
                    recorder_active,
                    meeting_companion_audio_level_reporting_enabled(),
                ) {
                    continue;
                }
                emit_meeting_audio_level(&inner, &meeting_id, level);
            }
        })
    {
        log::warn!("[meeting] audio level reporter spawn failed: {error}");
    }
}

fn spawn_meeting_recorder_error_monitor(inner: &Arc<Inner>, rx: mpsc::Receiver<RecorderError>) {
    let inner = Arc::clone(inner);
    std::thread::spawn(move || {
        for error in rx {
            let meeting_id = inner
                .meeting_session
                .lock()
                .as_ref()
                .map(|session| session.record().id.clone());
            emit_meeting_error(
                &inner,
                meeting_id.clone(),
                "recorderInterrupted",
                &error.to_string(),
            );
            if let Some(id) = meeting_id {
                let mut guard = inner.meeting_session.lock();
                if let Some(session) = guard.as_mut().filter(|session| session.record().id == id) {
                    session.mark_asr_interrupted(Utc::now());
                    let _ = persist_meeting_record(session.record());
                }
            }
        }
    });
}

fn stop_meeting_recorder(inner: &Arc<Inner>) {
    if let Some(recorder) = inner.meeting_recorder.lock().take() {
        recorder.stop();
        release_recording_mute(inner, "meeting");
    }
}

fn clear_active_meeting_runtime(inner: &Arc<Inner>) {
    *inner.meeting_session.lock() = None;
    *inner.meeting_asr.lock() = None;
    *inner.meeting_recorder.lock() = None;
    *inner.meeting_next_part_index.lock() = 1;
    *inner.meeting_segment_count_at_asr_start.lock() = 0;
    *inner.meeting_asr_release_token.lock() = None;
    clear_meeting_audio_archive_state(inner);
}

fn reset_meeting_audio_archive_state(inner: &Arc<Inner>) {
    inner
        .meeting_audio_archive_active
        .store(true, std::sync::atomic::Ordering::Relaxed);
}

fn clear_meeting_audio_archive_state(inner: &Arc<Inner>) {
    inner
        .meeting_audio_archive_active
        .store(false, std::sync::atomic::Ordering::Relaxed);
}

fn record_meeting_audio_archive_result(inner: &Arc<Inner>, archive_active: bool) {
    if !archive_active {
        inner
            .meeting_audio_archive_active
            .store(false, std::sync::atomic::Ordering::Relaxed);
    }
}

enum MeetingAsrFlushOutcome {
    Raw(RawTranscript),
    InterruptedWithRaw { raw: RawTranscript, error: String },
}

impl MeetingAsrFlushOutcome {
    fn raw(&self) -> &RawTranscript {
        match self {
            MeetingAsrFlushOutcome::Raw(raw) => raw,
            MeetingAsrFlushOutcome::InterruptedWithRaw { raw, .. } => raw,
        }
    }

    fn interrupted_error(&self) -> Option<&str> {
        match self {
            MeetingAsrFlushOutcome::Raw(_) => None,
            MeetingAsrFlushOutcome::InterruptedWithRaw { error, .. } => Some(error),
        }
    }
}

async fn flush_current_meeting_asr(
    inner: &Arc<Inner>,
) -> Result<Option<MeetingAsrFlushOutcome>, String> {
    let Some(asr) = inner.meeting_asr.lock().take() else {
        return Ok(None);
    };
    flush_meeting_asr(inner, asr).await.map(Some)
}

fn new_meeting_asr_release_token() -> String {
    Uuid::new_v4().to_string()
}

#[cfg(target_os = "windows")]
fn is_meeting_local_asr(asr: &ActiveAsr) -> bool {
    matches!(
        asr,
        ActiveAsr::FoundryLocalWhisper(_) | ActiveAsr::SherpaOnnxLocal(_)
    )
}

#[cfg(not(target_os = "windows"))]
fn is_meeting_local_asr(_asr: &ActiveAsr) -> bool {
    false
}

#[cfg(target_os = "windows")]
fn provider_uses_meeting_local_asr(provider_id: &str) -> bool {
    crate::asr::local::foundry::is_foundry_local_whisper(provider_id)
        || crate::asr::local::sherpa::is_sherpa_onnx_local(provider_id)
}

#[cfg(not(target_os = "windows"))]
fn provider_uses_meeting_local_asr(_provider_id: &str) -> bool {
    false
}

fn prepare_meeting_asr_release_token_for_provider(
    inner: &Arc<Inner>,
    provider_id: &str,
) -> Option<String> {
    if !provider_uses_meeting_local_asr(provider_id) {
        return None;
    }
    let token = new_meeting_asr_release_token();
    *inner.meeting_asr_release_token.lock() = Some(token.clone());
    Some(token)
}

fn current_or_new_meeting_asr_release_token(inner: &Arc<Inner>) -> String {
    let mut guard = inner.meeting_asr_release_token.lock();
    guard
        .get_or_insert_with(new_meeting_asr_release_token)
        .clone()
}

fn cleanup_unstored_meeting_asr_start(
    inner: &Arc<Inner>,
    asr_start: &QaAsrStart,
    release_token: Option<String>,
) {
    let active_asr = asr_start.active_asr();
    schedule_meeting_local_asr_release_for(inner, &active_asr, release_token);
    cancel_active_asr(active_asr);
}

fn schedule_meeting_local_asr_release_for(
    inner: &Arc<Inner>,
    asr: &ActiveAsr,
    release_token: Option<String>,
) {
    if !is_meeting_local_asr(asr) {
        return;
    }
    let token = release_token.unwrap_or_else(|| current_or_new_meeting_asr_release_token(inner));
    match asr {
        #[cfg(target_os = "windows")]
        ActiveAsr::FoundryLocalWhisper(_) => super::schedule_foundry_local_asr_release(
            inner,
            super::AsrReleaseSession::Meeting(token),
            None,
        ),
        #[cfg(target_os = "windows")]
        ActiveAsr::SherpaOnnxLocal(_) => {
            super::schedule_sherpa_onnx_release(inner, super::AsrReleaseSession::Meeting(token))
        }
        _ => {}
    }
}

fn schedule_meeting_local_asr_release_for_provider(
    inner: &Arc<Inner>,
    provider_id: &str,
    release_token: Option<String>,
) {
    let Some(token) = release_token else {
        return;
    };
    #[cfg(target_os = "windows")]
    {
        if crate::asr::local::foundry::is_foundry_local_whisper(provider_id) {
            super::schedule_foundry_local_asr_release(
                inner,
                super::AsrReleaseSession::Meeting(token),
                None,
            );
        } else if crate::asr::local::sherpa::is_sherpa_onnx_local(provider_id) {
            super::schedule_sherpa_onnx_release(inner, super::AsrReleaseSession::Meeting(token));
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = (inner, provider_id, token);
    }
}

#[cfg(target_os = "windows")]
fn schedule_foundry_meeting_release(
    inner: &Arc<Inner>,
    primary_recovery: Option<crate::asr::local::foundry_runtime::FoundryPrimaryRecoveryToken>,
) {
    let token = current_or_new_meeting_asr_release_token(inner);
    super::schedule_foundry_local_asr_release(
        inner,
        super::AsrReleaseSession::Meeting(token),
        primary_recovery,
    );
}

#[cfg(target_os = "windows")]
fn schedule_sherpa_meeting_release(inner: &Arc<Inner>) {
    let token = current_or_new_meeting_asr_release_token(inner);
    super::schedule_sherpa_onnx_release(inner, super::AsrReleaseSession::Meeting(token));
}

async fn flush_meeting_asr(
    inner: &Arc<Inner>,
    asr: ActiveAsr,
) -> Result<MeetingAsrFlushOutcome, String> {
    let uses_global_timeout = asr_transcribe_uses_global_timeout(&asr);
    match asr {
        ActiveAsr::Volcengine(asr) => {
            if let Err(error) = asr.send_last_frame().await {
                log::warn!("[meeting] volcengine send last frame failed: {error}");
            }
            let timeout = std::time::Duration::from_secs(COORDINATOR_GLOBAL_TIMEOUT_SECS);
            tokio::time::timeout(timeout, asr.await_final_result())
                .await
                .map_err(|_| "volcengine transcribe timeout".to_string())?
                .map(MeetingAsrFlushOutcome::Raw)
                .map_err(|e| e.to_string())
        }
        ActiveAsr::Bailian(asr) => {
            if let Err(error) = asr.send_last_frame().await {
                log::warn!("[meeting] bailian send last frame failed: {error}");
            }
            let timeout = std::time::Duration::from_secs(COORDINATOR_GLOBAL_TIMEOUT_SECS);
            let raw = tokio::time::timeout(timeout, asr.await_final_result())
                .await
                .map_err(|_| "bailian transcribe timeout".to_string())?
                .map_err(|e| e.to_string())?;
            if let Some(error) = asr.interrupted_error() {
                Ok(MeetingAsrFlushOutcome::InterruptedWithRaw { raw, error })
            } else {
                Ok(MeetingAsrFlushOutcome::Raw(raw))
            }
        }
        ActiveAsr::Qwen3Realtime(asr) => {
            if let Err(error) = asr.send_last_frame().await {
                log::warn!("[meeting] Qwen3 realtime send last frame failed: {error}");
            }
            let timeout = std::time::Duration::from_secs(COORDINATOR_GLOBAL_TIMEOUT_SECS);
            tokio::time::timeout(timeout, asr.await_final_result())
                .await
                .map_err(|_| "qwen3 realtime transcribe timeout".to_string())?
                .map(MeetingAsrFlushOutcome::Raw)
                .map_err(|e| e.to_string())
        }
        ActiveAsr::StepfunRealtime(asr) => {
            if let Err(error) = asr.send_last_frame().await {
                log::warn!("[meeting] StepFun realtime send last frame failed: {error}");
            }
            let timeout = std::time::Duration::from_secs(COORDINATOR_GLOBAL_TIMEOUT_SECS);
            tokio::time::timeout(timeout, asr.await_final_result())
                .await
                .map_err(|_| "stepfun realtime transcribe timeout".to_string())?
                .map(MeetingAsrFlushOutcome::Raw)
                .map_err(|e| e.to_string())
        }
        ActiveAsr::Xfyun(asr) => {
            if let Err(error) = asr.send_last_frame().await {
                log::warn!("[meeting] iFlytek ASR send last frame failed: {error}");
            }
            let timeout = std::time::Duration::from_secs(COORDINATOR_GLOBAL_TIMEOUT_SECS);
            tokio::time::timeout(timeout, asr.await_final_result())
                .await
                .map_err(|_| "xfyun transcribe timeout".to_string())?
                .map(MeetingAsrFlushOutcome::Raw)
                .map_err(|e| e.to_string())
        }
        ActiveAsr::Whisper(whisper) => {
            debug_assert!(uses_global_timeout);
            let timeout =
                super::whisper_transcribe_timeout((whisper.buffer_duration_ms() as f64) / 1000.0);
            tokio::time::timeout(timeout, whisper.transcribe())
                .await
                .map_err(|_| "whisper transcribe timeout".to_string())?
                .map(MeetingAsrFlushOutcome::Raw)
                .map_err(|e| e.to_string())
        }
        ActiveAsr::DashScopeMultimodal(asr) => {
            debug_assert!(uses_global_timeout);
            let audio_secs = asr.buffer_duration_ms() as f64 / 1000.0;
            let timeout = asr.transcribe_timeout(audio_secs);
            tokio::time::timeout(timeout, asr.transcribe())
                .await
                .map_err(|_| "dashscope multimodal transcribe timeout".to_string())?
                .map(MeetingAsrFlushOutcome::Raw)
                .map_err(|e| e.to_string())
        }
        ActiveAsr::ElevenLabs(asr) => {
            debug_assert!(uses_global_timeout);
            let audio_secs = asr.buffer_duration_ms() as f64 / 1000.0;
            let timeout = crate::asr::elevenlabs::transcribe_timeout(audio_secs);
            tokio::time::timeout(timeout, asr.transcribe())
                .await
                .map_err(|_| "elevenlabs transcribe timeout".to_string())?
                .map(MeetingAsrFlushOutcome::Raw)
                .map_err(|e| e.to_string())
        }
        ActiveAsr::Mimo(mimo) => {
            debug_assert!(uses_global_timeout);
            let timeout = std::time::Duration::from_secs(COORDINATOR_GLOBAL_TIMEOUT_SECS);
            tokio::time::timeout(timeout, mimo.transcribe())
                .await
                .map_err(|_| "mimo transcribe timeout".to_string())?
                .map(MeetingAsrFlushOutcome::Raw)
                .map_err(|e| e.to_string())
        }
        #[cfg(target_os = "windows")]
        ActiveAsr::FoundryLocalWhisper(local) => {
            debug_assert!(!uses_global_timeout);
            let audio_secs = (local.buffer_duration_ms() as f64) / 1000.0;
            match local
                .transcribe_with_fallback_notice(
                    super::windows_local_asr_transcribe_timeout(audio_secs),
                    Arc::new(|_| {}),
                )
                .await
            {
                Ok(outcome) => {
                    schedule_foundry_meeting_release(inner, outcome.primary_recovery);
                    Ok(MeetingAsrFlushOutcome::Raw(outcome.raw))
                }
                Err(error) => {
                    schedule_foundry_meeting_release(inner, None);
                    Err(error.to_string())
                }
            }
        }
        #[cfg(target_os = "windows")]
        ActiveAsr::SherpaOnnxLocal(local) => {
            debug_assert!(!uses_global_timeout);
            let audio_secs = (local.buffer_duration_ms() as f64) / 1000.0;
            let result = local
                .transcribe(super::windows_local_asr_transcribe_timeout(audio_secs))
                .await
                .map(MeetingAsrFlushOutcome::Raw)
                .map_err(|e| e.to_string());
            schedule_sherpa_meeting_release(inner);
            result
        }
        #[cfg(target_os = "macos")]
        ActiveAsr::Local(local) => {
            debug_assert!(uses_global_timeout);
            let timeout =
                super::local_qwen_transcribe_timeout((local.buffer_duration_ms() as f64) / 1000.0);
            let result = tokio::time::timeout(timeout, local.transcribe())
                .await
                .map_err(|_| "local qwen transcribe timeout".to_string())?
                .map(MeetingAsrFlushOutcome::Raw)
                .map_err(|e| e.to_string());
            inner.local_asr_cache.touch();
            super::schedule_local_asr_release(inner);
            result
        }
        #[cfg(target_os = "macos")]
        ActiveAsr::AppleSpeech(local) => {
            debug_assert!(uses_global_timeout);
            let timeout = std::time::Duration::from_secs(COORDINATOR_GLOBAL_TIMEOUT_SECS);
            tokio::time::timeout(timeout, local.transcribe())
                .await
                .map_err(|_| "apple speech transcribe timeout".to_string())?
                .map(MeetingAsrFlushOutcome::Raw)
                .map_err(|e| e.to_string())
        }
    }
}

fn apply_meeting_asr_flush_outcome(
    session: &mut MeetingSession,
    outcome: &MeetingAsrFlushOutcome,
    observed_at: DateTime<Utc>,
    session_start_count: usize,
) -> Option<String> {
    append_raw_transcript_if_needed(session, outcome.raw(), observed_at, session_start_count);
    outcome.interrupted_error().map(|error| {
        session.mark_asr_interrupted(observed_at);
        error.to_string()
    })
}

fn append_raw_transcript_if_needed(
    session: &mut MeetingSession,
    raw: &RawTranscript,
    observed_at: DateTime<Utc>,
    session_start_count: usize,
) {
    let raw_text = raw.text.trim();
    if raw_text.is_empty() {
        return;
    }
    let already_has_same_text = session
        .record()
        .transcript_segments
        .iter()
        .any(|segment| segment.text.trim() == raw_text);
    if already_has_same_text {
        return;
    }

    let existing_since_start = session
        .record()
        .transcript_segments
        .iter()
        .skip(session_start_count)
        .map(|segment| segment.text.trim())
        .collect::<String>();
    let text_to_append = if existing_since_start.is_empty() {
        raw_text
    } else if let Some(tail) = raw_text.strip_prefix(&existing_since_start) {
        tail.trim()
    } else {
        ""
    };
    if text_to_append.is_empty() {
        return;
    }
    let session_metadata = session.current_asr_metadata();
    let start_ms = if existing_since_start.is_empty() {
        session_metadata.session_start_ms
    } else {
        session
            .record()
            .transcript_segments
            .last()
            .and_then(|segment| segment.end_ms)
            .or(session_metadata.session_start_ms)
    };
    let end_ms = meeting_relative_segment_ms(
        session_metadata.session_start_ms,
        Some(raw.duration_ms),
        Some(raw.duration_ms),
    );
    let provider_start_ms = match (session_metadata.session_start_ms, start_ms) {
        (Some(session_start_ms), Some(start_ms)) if start_ms >= session_start_ms => {
            Some(start_ms.saturating_sub(session_start_ms))
        }
        _ => None,
    };
    let metadata = TranscriptSegmentMetadata {
        provider_id: Some(session.active_provider().to_string()),
        provider_session_id: session_metadata.provider_session_id,
        audio_part_index: session_metadata.audio_part_index,
        session_start_ms: session_metadata.session_start_ms,
        provider_start_ms,
        provider_end_ms: Some(raw.duration_ms),
        ..Default::default()
    };
    let _ = session.append_transcript_segment(
        text_to_append,
        start_ms,
        end_ms,
        observed_at,
        Some(metadata),
    );
}

fn meeting_final_segment_sink(inner: &Arc<Inner>, meeting_id: String) -> AsrFinalSegmentSink {
    let inner = Arc::clone(inner);
    Arc::new(move |segment: AsrFinalSegment| {
        let (emitted_segment, clear_provider_id, clear_session_id) = {
            let mut guard = inner.meeting_session.lock();
            let Some(session) = guard.as_mut() else {
                return;
            };
            if session.record().id != meeting_id {
                return;
            }
            let metadata = metadata_from_asr_segment(
                &segment,
                session.active_provider(),
                &session.current_asr_metadata(),
            );
            let start_ms = meeting_relative_segment_ms(
                metadata.session_start_ms,
                metadata.provider_start_ms,
                segment.start_ms,
            );
            let end_ms = meeting_relative_segment_ms(
                metadata.session_start_ms,
                metadata.provider_end_ms,
                segment.end_ms,
            );
            if transcript_segment_metadata_exists(session.record(), &metadata)
                || transcript_segment_exists(session.record(), &segment.text, start_ms, end_ms)
            {
                return;
            }
            let now = Utc::now();
            let emitted_segment = session.append_transcript_segment(
                &segment.text,
                start_ms,
                end_ms,
                now,
                Some(metadata.clone()),
            );
            if emitted_segment.is_some() {
                session.mark_persisted_now(now);
                if let Err(error) = persist_meeting_record(session.record()) {
                    log::warn!("[meeting] persist realtime segment failed: {error}");
                }
            }
            (
                emitted_segment,
                metadata.provider_id.clone(),
                metadata.provider_session_id.clone(),
            )
        };
        if let Some(segment) = emitted_segment {
            emit_meeting_transcript_draft_clear_with(
                &inner,
                &meeting_id,
                clear_provider_id.as_deref(),
                clear_session_id,
            );
            emit_meeting_transcript_segment(&inner, &meeting_id, &segment);
            if let Ok(Some(snapshot)) = meeting_snapshot(&inner, Utc::now()) {
                emit_meeting_state(&inner, &snapshot);
            }
        }
    })
}

fn meeting_draft_segment_sink(inner: &Arc<Inner>, meeting_id: String) -> AsrDraftSegmentSink {
    let inner = Arc::clone(inner);
    Arc::new(move |draft: AsrDraftSegment| {
        let should_emit = {
            let guard = inner.meeting_session.lock();
            let Some(session) = guard.as_ref() else {
                return;
            };
            session.record().id == meeting_id
        };
        if !should_emit {
            return;
        }
        let _draft_timebase = (draft.audio_part_index, draft.session_start_ms);
        emit_meeting_transcript_draft(
            &inner,
            &MeetingTranscriptDraftEvent {
                meeting_id: meeting_id.clone(),
                provider_id: draft.provider_id,
                provider_session_id: draft.provider_session_id,
                text: draft.text,
                start_ms: draft.start_ms,
                end_ms: draft.end_ms,
                sequence: draft.sequence,
                clear: draft.clear,
            },
        );
    })
}

fn meeting_asr_interruption_sink(
    inner: &Arc<Inner>,
    meeting_id: String,
    provider_id: String,
    provider_session_id: Option<String>,
) -> AsrInterruptionSink {
    let inner = Arc::clone(inner);
    Arc::new(move |error| {
        handle_meeting_asr_interruption(
            &inner,
            &meeting_id,
            &provider_id,
            provider_session_id.clone(),
            &error,
        );
    })
}

fn handle_meeting_asr_interruption(
    inner: &Arc<Inner>,
    meeting_id: &str,
    provider_id: &str,
    provider_session_id: Option<String>,
    error: &str,
) {
    let now = Utc::now();
    let applied = {
        let mut guard = inner.meeting_session.lock();
        let Some(session) = guard.as_mut() else {
            return;
        };
        if session.record().id != meeting_id {
            return;
        }
        if !apply_realtime_asr_interruption(session, now, provider_session_id.as_deref()) {
            return;
        }
        if let Err(error) = persist_meeting_record(session.record()) {
            log::warn!("[meeting] persist realtime ASR interruption failed: {error}");
        }
        true
    };
    if !applied {
        return;
    }
    emit_meeting_transcript_draft_clear_with(
        inner,
        meeting_id,
        Some(provider_id),
        provider_session_id,
    );
    emit_meeting_error(inner, Some(meeting_id.to_string()), "asrInterrupted", error);
    if let Ok(Some(snapshot)) = meeting_snapshot(inner, now) {
        emit_meeting_state(inner, &snapshot);
    }
}

fn apply_realtime_asr_interruption(
    session: &mut MeetingSession,
    now: DateTime<Utc>,
    provider_session_id: Option<&str>,
) -> bool {
    if provider_session_id.is_some()
        && session.active_provider_session_id.as_deref() != provider_session_id
    {
        return false;
    }
    session.mark_asr_interrupted(now);
    session.clear_active_asr_session();
    true
}

fn metadata_from_asr_segment(
    segment: &AsrFinalSegment,
    active_provider: &str,
    session_metadata: &AsrSessionMetadata,
) -> TranscriptSegmentMetadata {
    let provider_start_ms = segment.provider_start_ms.or(segment.start_ms);
    let provider_end_ms = segment.provider_end_ms.or(segment.end_ms);
    TranscriptSegmentMetadata {
        provider_id: segment
            .provider_id
            .clone()
            .or_else(|| Some(active_provider.to_string())),
        provider_session_id: segment
            .provider_session_id
            .clone()
            .or_else(|| session_metadata.provider_session_id.clone()),
        provider_segment_id: segment.provider_segment_id.clone(),
        sentence_id: segment.sentence_id.clone(),
        sequence: segment.sequence,
        audio_part_index: segment
            .audio_part_index
            .or(session_metadata.audio_part_index),
        session_start_ms: segment
            .session_start_ms
            .or(session_metadata.session_start_ms),
        provider_start_ms,
        provider_end_ms,
        token_timestamps: segment.token_timestamps.clone(),
        needs_review: false,
        overlapping: false,
    }
}

fn meeting_relative_segment_ms(
    session_start_ms: Option<u64>,
    provider_ms: Option<u64>,
    fallback_ms: Option<u64>,
) -> Option<u64> {
    match (session_start_ms, provider_ms) {
        (Some(session_start_ms), Some(provider_ms)) => {
            Some(session_start_ms.saturating_add(provider_ms))
        }
        (None, Some(provider_ms)) => Some(provider_ms),
        _ => fallback_ms,
    }
}

fn ensure_active_meeting_id(inner: &Arc<Inner>, meeting_id: &str) -> Result<(), String> {
    let guard = inner.meeting_session.lock();
    match guard.as_ref() {
        Some(session) if session.record().id == meeting_id => Ok(()),
        Some(_) => Err("meeting id mismatch".to_string()),
        None => Err("meeting recording not active".to_string()),
    }
}

fn transcript_segment_exists(
    record: &MeetingRecord,
    text: &str,
    start_ms: Option<u64>,
    end_ms: Option<u64>,
) -> bool {
    let text = text.trim();
    record.transcript_segments.iter().any(|segment| {
        segment.text.trim() == text
            && start_ms
                .map(|value| value == segment.start_ms)
                .unwrap_or(true)
            && end_ms
                .map(|value| Some(value) == segment.end_ms)
                .unwrap_or(true)
    })
}

fn transcript_segment_metadata_exists(
    record: &MeetingRecord,
    metadata: &TranscriptSegmentMetadata,
) -> bool {
    let Some(provider_id) = metadata.provider_id.as_deref() else {
        return false;
    };
    let Some(provider_session_id) = metadata.provider_session_id.as_deref() else {
        return false;
    };

    if let Some(provider_segment_id) = metadata.provider_segment_id.as_deref() {
        if record.transcript_segments.iter().any(|segment| {
            segment.metadata.as_ref().is_some_and(|existing| {
                existing.provider_id.as_deref() == Some(provider_id)
                    && existing.provider_session_id.as_deref() == Some(provider_session_id)
                    && existing.provider_segment_id.as_deref() == Some(provider_segment_id)
            })
        }) {
            return true;
        }
    }

    if let Some(sentence_id) = metadata.sentence_id.as_deref() {
        if record.transcript_segments.iter().any(|segment| {
            segment.metadata.as_ref().is_some_and(|existing| {
                existing.provider_id.as_deref() == Some(provider_id)
                    && existing.provider_session_id.as_deref() == Some(provider_session_id)
                    && existing.sentence_id.as_deref() == Some(sentence_id)
            })
        }) {
            return true;
        }
    }

    if let Some(sequence) = metadata.sequence {
        if record.transcript_segments.iter().any(|segment| {
            segment.metadata.as_ref().is_some_and(|existing| {
                existing.provider_id.as_deref() == Some(provider_id)
                    && existing.provider_session_id.as_deref() == Some(provider_session_id)
                    && existing.sequence == Some(sequence)
            })
        }) {
            return true;
        }
    }

    false
}

fn persist_meeting_record(record: &MeetingRecord) -> Result<(), String> {
    MeetingStore::new()
        .map_err(|e| e.to_string())?
        .update(record.clone())
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "meeting not found".to_string())?;
    Ok(())
}

fn commit_meeting_realtime_asr_label(
    session: &mut MeetingSession,
    label: &AsrCallLabel,
    now: DateTime<Utc>,
    persist: impl FnOnce(&MeetingRecord) -> Result<(), String>,
) -> Result<(), String> {
    let original = session.clone();
    session.lock_realtime_asr_label(label, now);
    if let Err(error) = persist(session.record()) {
        *session = original;
        return Err(error);
    }
    Ok(())
}

fn prune_meeting_audio_retention_with_current_preference() -> Result<(), String> {
    MeetingStore::new()
        .map_err(|e| e.to_string())?
        .prune_audio_retention(
            PreferencesStore::new()
                .unwrap_or_else(|_| PreferencesStore::new_fallback())
                .get()
                .meeting_audio_retention_count,
        )
        .map(|_| ())
        .map_err(|e| e.to_string())
}

fn commit_meeting_resume(
    session: &mut MeetingSession,
    now: DateTime<Utc>,
    provider_session_id: String,
    audio_part_index: u32,
    session_start_ms: u64,
    asr_available: bool,
    persist: impl FnOnce(&MeetingRecord) -> Result<(), String>,
) -> Result<usize, String> {
    let original = session.clone();
    let result = (|| {
        session.resume(now)?;
        if asr_available {
            session.set_active_asr_session(provider_session_id, audio_part_index, session_start_ms);
        } else {
            session.mark_asr_interrupted(now);
            session.clear_active_asr_session();
        }
        persist(session.record())?;
        Ok(session.record().transcript_segments.len())
    })();
    if result.is_err() {
        *session = original;
    }
    result
}

fn commit_meeting_asr_interruption(
    session: &mut MeetingSession,
    now: DateTime<Utc>,
    persist: impl FnOnce(&MeetingRecord) -> Result<(), String>,
) -> Result<(), String> {
    let original = session.clone();
    session.mark_asr_interrupted(now);
    session.clear_active_asr_session();
    if let Err(error) = persist(session.record()) {
        *session = original;
        return Err(error);
    }
    Ok(())
}

fn apply_meeting_audio_retention_state(
    inner: &Arc<Inner>,
    record: &mut MeetingRecord,
) -> Option<String> {
    let archive_active = inner
        .meeting_audio_archive_active
        .load(std::sync::atomic::Ordering::Relaxed);
    let retention_count = PreferencesStore::new()
        .unwrap_or_else(|_| PreferencesStore::new_fallback())
        .get()
        .meeting_audio_retention_count;
    apply_meeting_audio_retention_state_with(record, archive_active, retention_count, |id| {
        let path = crate::persistence::meeting_recording_existing_path_for_id(id)
            .map_err(|e| e.to_string())?;
        crate::persistence::remove_meeting_audio_path(&path).map_err(|e| e.to_string())
    })
}

fn apply_meeting_audio_retention_state_with(
    record: &mut MeetingRecord,
    archive_active: bool,
    retention_count: u32,
    remove_audio: impl FnOnce(&str) -> Result<(), String>,
) -> Option<String> {
    if !archive_active {
        record.audio.state = MeetingAudioState::Unavailable;
        record.audio.retained = false;
        record.audio.path = None;
        return None;
    }
    if record.processing_hold.is_some() {
        record.audio.state = MeetingAudioState::Retained;
        record.audio.retained = true;
        record.audio.path = None;
        return None;
    }
    if retention_count == 0 {
        match remove_audio(&record.id) {
            Ok(()) => {
                record.audio.state = MeetingAudioState::Pruned;
                record.audio.retained = false;
                record.audio.path = None;
                None
            }
            Err(error) => {
                record.audio.state = MeetingAudioState::Unavailable;
                record.audio.retained = false;
                record.audio.path = None;
                Some(error)
            }
        }
    } else {
        record.audio.state = MeetingAudioState::Retained;
        record.audio.retained = true;
        record.audio.path = None;
        None
    }
}

fn mark_start_failed_record(
    inner: &Arc<Inner>,
    meeting_id: &str,
    error: &str,
) -> Result<(), String> {
    stop_meeting_recorder(inner);
    let mut session = inner
        .meeting_session
        .lock()
        .take()
        .ok_or_else(|| "meeting recording not active".to_string())?;
    let now = Utc::now();
    session.mark_asr_interrupted(now);
    session.finish(now, true);
    session.record_mut().audio.state = MeetingAudioState::Unavailable;
    session.record_mut().audio.retained = false;
    session.record_mut().audio.path = None;
    persist_meeting_record(session.record())?;
    *inner.meeting_asr.lock() = None;
    *inner.meeting_recorder.lock() = None;
    *inner.meeting_next_part_index.lock() = 1;
    *inner.meeting_segment_count_at_asr_start.lock() = 0;
    *inner.meeting_asr_release_token.lock() = None;
    clear_meeting_audio_archive_state(inner);
    emit_meeting_error(inner, Some(meeting_id.to_string()), "startFailed", error);
    Ok(())
}

fn emit_meeting_state(inner: &Arc<Inner>, snapshot: &MeetingRecordingSnapshot) {
    if let Some(app) = inner.app.lock().clone() {
        let _ = app.emit("meeting:state", snapshot);
    }
}

fn emit_meeting_audio_level(inner: &Arc<Inner>, meeting_id: &str, level: f32) {
    if let Some(app) = inner.app.lock().clone() {
        let _ = app.emit(
            "meeting:audio-level",
            MeetingAudioLevelEvent {
                meeting_id: meeting_id.to_string(),
                level: normalize_meeting_audio_level(level),
            },
        );
    }
}

fn emit_meeting_transcript_segment(
    inner: &Arc<Inner>,
    meeting_id: &str,
    segment: &TranscriptSegment,
) {
    if let Some(app) = inner.app.lock().clone() {
        let _ = app.emit(
            "meeting:transcript-segment",
            MeetingTranscriptSegmentEvent {
                meeting_id: meeting_id.to_string(),
                segment: segment.clone(),
            },
        );
    }
}

fn emit_meeting_transcript_draft(inner: &Arc<Inner>, draft: &MeetingTranscriptDraftEvent) {
    if let Some(app) = inner.app.lock().clone() {
        let _ = app.emit("meeting:transcript-draft", draft);
    }
}

fn emit_meeting_transcript_draft_clear(inner: &Arc<Inner>, meeting_id: &str) {
    let (provider_id, provider_session_id) = {
        let guard = inner.meeting_session.lock();
        let Some(session) = guard.as_ref() else {
            return;
        };
        (
            session.active_provider().to_string(),
            session.active_provider_session_id.clone(),
        )
    };
    emit_meeting_transcript_draft_clear_with(
        inner,
        meeting_id,
        Some(&provider_id),
        provider_session_id,
    );
}

fn emit_meeting_transcript_draft_clear_with(
    inner: &Arc<Inner>,
    meeting_id: &str,
    provider_id: Option<&str>,
    provider_session_id: Option<String>,
) {
    emit_meeting_transcript_draft(
        inner,
        &MeetingTranscriptDraftEvent {
            meeting_id: meeting_id.to_string(),
            provider_id: provider_id.unwrap_or("unknown").to_string(),
            provider_session_id,
            text: String::new(),
            start_ms: None,
            end_ms: None,
            sequence: None,
            clear: true,
        },
    );
}

fn emit_meeting_error(inner: &Arc<Inner>, meeting_id: Option<String>, code: &str, message: &str) {
    if let Some(app) = inner.app.lock().clone() {
        let _ = app.emit(
            "meeting:error",
            MeetingErrorEvent {
                meeting_id,
                code: code.to_string(),
                message: message.to_string(),
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ProcessingHold, TranscriptSegmentSource};
    use chrono::{TimeZone, Utc};

    #[test]
    fn meeting_companion_audio_level_is_throttled_to_ten_hz() {
        let mut throttle = MeetingAudioLevelThrottle::default();
        let samples = [0, 25, 99, 100, 150, 199, 200, 299, 300];
        let emitted = samples
            .into_iter()
            .filter(|millis| throttle.should_emit(Duration::from_millis(*millis)))
            .collect::<Vec<_>>();

        assert_eq!(emitted, vec![0, 100, 200, 300]);
    }

    #[test]
    fn meeting_companion_audio_level_requires_active_visible_recording() {
        assert!(meeting_audio_level_delivery_allowed(true, true, true, true));
        assert!(!meeting_audio_level_delivery_allowed(
            true, false, true, true
        ));
        assert!(!meeting_audio_level_delivery_allowed(
            true, true, false, true
        ));
        assert!(!meeting_audio_level_delivery_allowed(
            true, true, true, false
        ));
        assert!(!meeting_audio_level_delivery_allowed(
            false, true, true, true
        ));
    }

    #[test]
    fn meeting_companion_audio_level_is_clamped() {
        assert_eq!(normalize_meeting_audio_level(-0.5), 0.0);
        assert_eq!(normalize_meeting_audio_level(0.4), 0.4);
        assert_eq!(normalize_meeting_audio_level(1.5), 1.0);
        assert_eq!(normalize_meeting_audio_level(f32::NAN), 0.0);
    }

    #[test]
    fn meeting_companion_audio_level_callback_never_blocks_on_full_queue() {
        let (sender, receiver) = mpsc::sync_channel(1);
        assert!(try_queue_meeting_audio_level(&sender, 0.25));
        assert!(!try_queue_meeting_audio_level(&sender, 0.75));
        assert_eq!(receiver.recv().expect("queued level"), 0.25);
    }

    #[test]
    fn meeting_session_starts_from_idle() {
        let now = Utc.with_ymd_and_hms(2026, 7, 4, 9, 30, 0).unwrap();
        let session = MeetingSession::new(
            "550e8400-e29b-41d4-a716-446655440000".to_string(),
            now,
            "whisper".to_string(),
        );

        assert_eq!(session.phase(), MeetingSessionPhase::Recording);
        assert_eq!(
            session.record().status,
            crate::types::MeetingStatus::Recording
        );
        assert_eq!(session.record().title, "会议记录 2026-07-04 09:30");
    }

    #[test]
    fn meeting_session_rejects_double_start() {
        let now = Utc.with_ymd_and_hms(2026, 7, 4, 9, 30, 0).unwrap();
        let session = MeetingSession::new(
            "550e8400-e29b-41d4-a716-446655440000".to_string(),
            now,
            "whisper".to_string(),
        );

        assert_eq!(
            session.ensure_can_start_another(),
            Err("meeting recording already active".to_string())
        );
    }

    #[test]
    fn meeting_session_pause_resume_accumulates_paused_duration() {
        let started = Utc.with_ymd_and_hms(2026, 7, 4, 9, 30, 0).unwrap();
        let paused = Utc.with_ymd_and_hms(2026, 7, 4, 9, 35, 0).unwrap();
        let resumed = Utc.with_ymd_and_hms(2026, 7, 4, 9, 37, 0).unwrap();
        let stopped = Utc.with_ymd_and_hms(2026, 7, 4, 9, 40, 0).unwrap();
        let mut session = MeetingSession::new(
            "550e8400-e29b-41d4-a716-446655440000".to_string(),
            started,
            "whisper".to_string(),
        );

        session.pause(paused).unwrap();
        session.resume(resumed).unwrap();
        session.finish(stopped, false);

        assert_eq!(session.record().duration_ms, Some(8 * 60 * 1000));
    }

    #[test]
    fn meeting_session_stop_computes_duration_excluding_pause() {
        let started = Utc.with_ymd_and_hms(2026, 7, 4, 9, 30, 0).unwrap();
        let paused = Utc.with_ymd_and_hms(2026, 7, 4, 9, 34, 0).unwrap();
        let stopped = Utc.with_ymd_and_hms(2026, 7, 4, 9, 40, 0).unwrap();
        let mut session = MeetingSession::new(
            "550e8400-e29b-41d4-a716-446655440000".to_string(),
            started,
            "whisper".to_string(),
        );

        session.pause(paused).unwrap();
        session.finish(stopped, false);

        assert_eq!(session.record().duration_ms, Some(4 * 60 * 1000));
    }

    #[test]
    fn meeting_session_asr_interruption_preserves_segments_and_allows_resume() {
        let started = Utc.with_ymd_and_hms(2026, 7, 4, 9, 30, 0).unwrap();
        let interrupted = Utc.with_ymd_and_hms(2026, 7, 4, 9, 31, 0).unwrap();
        let resumed = Utc.with_ymd_and_hms(2026, 7, 4, 9, 32, 0).unwrap();
        let mut session = MeetingSession::new(
            "550e8400-e29b-41d4-a716-446655440000".to_string(),
            started,
            "whisper".to_string(),
        );
        session.append_transcript_segment("已经识别的内容", Some(0), Some(1000), interrupted, None);

        session.mark_asr_interrupted(interrupted);
        session.resume(resumed).unwrap();

        assert_eq!(session.record().transcript_segments.len(), 1);
        assert_eq!(
            session.record().status,
            crate::types::MeetingStatus::TranscribingInterrupted
        );
        assert_eq!(session.phase(), MeetingSessionPhase::Recording);
    }

    #[test]
    fn meeting_session_resume_allowed_after_transcribing_interrupted() {
        let started = Utc.with_ymd_and_hms(2026, 7, 4, 9, 30, 0).unwrap();
        let interrupted = Utc.with_ymd_and_hms(2026, 7, 4, 9, 31, 0).unwrap();
        let paused = Utc.with_ymd_and_hms(2026, 7, 4, 9, 32, 0).unwrap();
        let resumed = Utc.with_ymd_and_hms(2026, 7, 4, 9, 33, 0).unwrap();
        let mut session = MeetingSession::new(
            "550e8400-e29b-41d4-a716-446655440000".to_string(),
            started,
            "whisper".to_string(),
        );

        session.mark_asr_interrupted(interrupted);
        session.pause(paused).unwrap();
        session.resume(resumed).unwrap();

        assert_eq!(session.phase(), MeetingSessionPhase::Recording);
        assert_eq!(
            session.record().status,
            crate::types::MeetingStatus::TranscribingInterrupted
        );
    }

    #[test]
    fn meeting_session_pause_keeps_control_phase_when_asr_is_interrupted() {
        let started = Utc.with_ymd_and_hms(2026, 7, 4, 9, 30, 0).unwrap();
        let interrupted = Utc.with_ymd_and_hms(2026, 7, 4, 9, 31, 0).unwrap();
        let paused = Utc.with_ymd_and_hms(2026, 7, 4, 9, 32, 0).unwrap();
        let mut session = MeetingSession::new(
            "550e8400-e29b-41d4-a716-446655440000".to_string(),
            started,
            "whisper".to_string(),
        );

        session.mark_asr_interrupted(interrupted);
        session.pause(paused).unwrap();
        let snapshot = session.snapshot(paused);

        assert_eq!(snapshot.phase, MeetingRecordingPhase::Paused);
        assert!(snapshot.asr_interrupted);
    }

    #[test]
    fn meeting_session_resume_command_rejects_recording_after_asr_interruption() {
        let started = Utc.with_ymd_and_hms(2026, 7, 4, 9, 30, 0).unwrap();
        let interrupted = Utc.with_ymd_and_hms(2026, 7, 4, 9, 31, 0).unwrap();
        let mut session = MeetingSession::new(
            "550e8400-e29b-41d4-a716-446655440000".to_string(),
            started,
            "whisper".to_string(),
        );
        session.mark_asr_interrupted(interrupted);

        assert_eq!(
            session.ensure_can_resume(),
            Err("meeting recording already continues after ASR interruption".into())
        );
    }

    #[test]
    fn transcript_segment_builder_defaults_speaker_and_source() {
        let started = Utc.with_ymd_and_hms(2026, 7, 4, 9, 30, 0).unwrap();
        let mut session = MeetingSession::new(
            "550e8400-e29b-41d4-a716-446655440000".to_string(),
            started,
            "whisper".to_string(),
        );

        let segment = session
            .append_transcript_segment("我们接下来确认计划", Some(1200), Some(3400), started, None)
            .expect("non-empty transcript creates segment");

        assert_eq!(segment.id, "seg-000001");
        assert_eq!(segment.speaker_label, "未区分");
        assert_eq!(segment.source, TranscriptSegmentSource::RealtimeAsr);
        assert_eq!(segment.start_ms, 1200);
        assert_eq!(segment.end_ms, Some(3400));
    }

    #[test]
    fn transcript_segment_builder_ignores_empty_text() {
        let started = Utc.with_ymd_and_hms(2026, 7, 4, 9, 30, 0).unwrap();
        let mut session = MeetingSession::new(
            "550e8400-e29b-41d4-a716-446655440000".to_string(),
            started,
            "whisper".to_string(),
        );

        assert!(session
            .append_transcript_segment("   ", Some(0), Some(100), started, None)
            .is_none());
        assert!(session.record().transcript_segments.is_empty());
    }

    #[test]
    fn transcript_segment_builder_uses_elapsed_end_when_provider_time_missing() {
        let started = Utc.with_ymd_and_hms(2026, 7, 4, 9, 30, 0).unwrap();
        let observed = Utc.with_ymd_and_hms(2026, 7, 4, 9, 30, 5).unwrap();
        let mut session = MeetingSession::new(
            "550e8400-e29b-41d4-a716-446655440000".to_string(),
            started,
            "whisper".to_string(),
        );

        let segment = session
            .append_transcript_segment("没有 provider 时间戳", None, None, observed, None)
            .expect("segment");

        assert_eq!(segment.start_ms, 0);
        assert_eq!(segment.end_ms, Some(5_000));
    }

    #[test]
    fn transcript_metadata_dedup_respects_provider_session() {
        let started = Utc.with_ymd_and_hms(2026, 7, 4, 9, 30, 0).unwrap();
        let mut session = MeetingSession::new(
            "550e8400-e29b-41d4-a716-446655440000".to_string(),
            started,
            "bailian".to_string(),
        );
        let metadata = TranscriptSegmentMetadata {
            provider_id: Some("bailian".to_string()),
            provider_session_id: Some("session-1".to_string()),
            sentence_id: Some("sentence-1".to_string()),
            ..Default::default()
        };
        session.append_transcript_segment(
            "first final",
            Some(1_000),
            Some(1_500),
            started,
            Some(metadata.clone()),
        );

        assert!(transcript_segment_metadata_exists(
            session.record(),
            &metadata
        ));

        let same_sentence_next_session = TranscriptSegmentMetadata {
            provider_session_id: Some("session-2".to_string()),
            ..metadata
        };
        assert!(!transcript_segment_metadata_exists(
            session.record(),
            &same_sentence_next_session
        ));
    }

    #[test]
    fn provider_time_is_shifted_by_session_start_ms() {
        let session_metadata = AsrSessionMetadata {
            provider_session_id: Some("session-2".to_string()),
            audio_part_index: Some(3),
            session_start_ms: Some(60_000),
        };
        let segment = AsrFinalSegment {
            text: "second part".to_string(),
            start_ms: Some(99),
            end_ms: Some(199),
            provider_id: None,
            provider_session_id: None,
            provider_segment_id: None,
            sentence_id: Some("sentence-2".to_string()),
            sequence: Some(2),
            audio_part_index: None,
            session_start_ms: None,
            provider_start_ms: Some(2_000),
            provider_end_ms: Some(2_800),
            token_timestamps: Vec::new(),
        };

        let metadata = metadata_from_asr_segment(&segment, "bailian", &session_metadata);

        assert_eq!(metadata.provider_id.as_deref(), Some("bailian"));
        assert_eq!(metadata.provider_session_id.as_deref(), Some("session-2"));
        assert_eq!(metadata.audio_part_index, Some(3));
        assert_eq!(metadata.session_start_ms, Some(60_000));
        assert_eq!(
            meeting_relative_segment_ms(
                metadata.session_start_ms,
                metadata.provider_start_ms,
                segment.start_ms
            ),
            Some(62_000)
        );
        assert_eq!(
            meeting_relative_segment_ms(
                metadata.session_start_ms,
                metadata.provider_end_ms,
                segment.end_ms
            ),
            Some(62_800)
        );
    }

    #[test]
    fn meeting_session_snapshot_clears_active_provider_session_id() {
        let started = Utc.with_ymd_and_hms(2026, 7, 4, 9, 30, 0).unwrap();
        let mut session = MeetingSession::new(
            "550e8400-e29b-41d4-a716-446655440000".to_string(),
            started,
            "bailian".to_string(),
        );
        session.set_active_asr_session("session-1".to_string(), 1, 0);

        assert_eq!(
            session
                .snapshot(started)
                .active_provider_session_id
                .as_deref(),
            Some("session-1")
        );

        session.clear_active_asr_session();

        assert_eq!(session.snapshot(started).active_provider_session_id, None);
    }

    #[test]
    fn realtime_asr_interruption_clears_matching_active_session_id() {
        let started = Utc.with_ymd_and_hms(2026, 7, 4, 9, 30, 0).unwrap();
        let interrupted = Utc.with_ymd_and_hms(2026, 7, 4, 9, 31, 0).unwrap();
        let mut session = MeetingSession::new(
            "550e8400-e29b-41d4-a716-446655440000".to_string(),
            started,
            "bailian".to_string(),
        );
        session.set_active_asr_session("session-1".to_string(), 1, 0);

        assert!(apply_realtime_asr_interruption(
            &mut session,
            interrupted,
            Some("session-1")
        ));

        assert_eq!(
            session.record().status,
            MeetingStatus::TranscribingInterrupted
        );
        assert_eq!(
            session.snapshot(interrupted).active_provider_session_id,
            None
        );
    }

    #[test]
    fn realtime_asr_interruption_ignores_stale_session_id() {
        let started = Utc.with_ymd_and_hms(2026, 7, 4, 9, 30, 0).unwrap();
        let interrupted = Utc.with_ymd_and_hms(2026, 7, 4, 9, 31, 0).unwrap();
        let mut session = MeetingSession::new(
            "550e8400-e29b-41d4-a716-446655440000".to_string(),
            started,
            "bailian".to_string(),
        );
        session.set_active_asr_session("session-2".to_string(), 2, 60_000);

        assert!(!apply_realtime_asr_interruption(
            &mut session,
            interrupted,
            Some("session-1")
        ));

        assert_eq!(session.record().status, MeetingStatus::Recording);
        assert_eq!(
            session
                .snapshot(interrupted)
                .active_provider_session_id
                .as_deref(),
            Some("session-2")
        );
    }

    #[test]
    fn meeting_model_override_only_applies_to_matching_provider() {
        let settings = crate::types::MeetingAsrSettings {
            model_override: Some("fun-asr-realtime".to_string()),
            model_provider_id: Some("bailian".to_string()),
            ..Default::default()
        };

        assert_eq!(
            meeting_model_override_for_provider(&settings, "bailian").as_deref(),
            Some("fun-asr-realtime")
        );
        assert_eq!(
            meeting_model_override_for_provider(&settings, "whisper"),
            None
        );
    }

    #[test]
    fn meeting_session_locks_actual_asr_label_for_resume_and_audit() {
        let started = Utc.with_ymd_and_hms(2026, 7, 4, 9, 30, 0).unwrap();
        let observed = Utc.with_ymd_and_hms(2026, 7, 4, 9, 30, 1).unwrap();
        let mut session = MeetingSession::new_with_asr_settings(
            "550e8400-e29b-41d4-a716-446655440000".to_string(),
            started,
            "bailian".to_string(),
            MeetingVadSilencePreset::Long,
            None,
        );
        session.lock_realtime_asr_label(
            &AsrCallLabel::new("bailian", Some("fun-asr-realtime".to_string())),
            observed,
        );

        assert_eq!(
            session.model_override().as_deref(),
            Some("fun-asr-realtime")
        );
        assert_eq!(
            session.record().realtime_asr,
            Some(MeetingRealtimeAsrSnapshot {
                provider_id: "bailian".to_string(),
                resolved_provider_id: "bailian".to_string(),
                model_id: Some("fun-asr-realtime".to_string()),
                silence_preset: MeetingVadSilencePreset::Long,
            })
        );
        assert_eq!(session.record().updated_at, observed.to_rfc3339());
    }

    #[test]
    fn meeting_realtime_asr_label_rolls_back_when_persist_fails() {
        let started = Utc.with_ymd_and_hms(2026, 7, 4, 9, 30, 0).unwrap();
        let observed = Utc.with_ymd_and_hms(2026, 7, 4, 9, 30, 1).unwrap();
        let mut session = MeetingSession::new_with_asr_settings(
            "550e8400-e29b-41d4-a716-446655440000".to_string(),
            started,
            "bailian".to_string(),
            MeetingVadSilencePreset::Standard,
            None,
        );
        let original = session.clone();

        let result = commit_meeting_realtime_asr_label(
            &mut session,
            &AsrCallLabel::new("bailian", Some("fun-asr-realtime".to_string())),
            observed,
            |_record| Err("persist failed".to_string()),
        );

        assert_eq!(result, Err("persist failed".to_string()));
        assert_eq!(session.model_override(), original.model_override());
        assert_eq!(session.record(), original.record());
    }

    #[test]
    fn raw_transcript_append_only_adds_tail_after_streaming_segments() {
        let started = Utc.with_ymd_and_hms(2026, 7, 4, 9, 30, 0).unwrap();
        let observed = Utc.with_ymd_and_hms(2026, 7, 4, 9, 30, 5).unwrap();
        let mut session = MeetingSession::new(
            "550e8400-e29b-41d4-a716-446655440000".to_string(),
            started,
            "volcengine".to_string(),
        );
        let session_start_count = session.record().transcript_segments.len();
        session.append_transcript_segment("已经实时追加", Some(0), Some(1000), observed, None);

        append_raw_transcript_if_needed(
            &mut session,
            &RawTranscript {
                text: "已经实时追加 后续批量尾部".to_string(),
                duration_ms: 5_000,
            },
            observed,
            session_start_count,
        );

        assert_eq!(session.record().transcript_segments.len(), 2);
        assert_eq!(session.record().transcript_segments[1].text, "后续批量尾部");
    }

    #[test]
    fn raw_transcript_append_uses_active_session_timebase_and_metadata() {
        let started = Utc.with_ymd_and_hms(2026, 7, 4, 9, 30, 0).unwrap();
        let observed = Utc.with_ymd_and_hms(2026, 7, 4, 9, 31, 5).unwrap();
        let mut session = MeetingSession::new(
            "550e8400-e29b-41d4-a716-446655440000".to_string(),
            started,
            "whisper".to_string(),
        );
        session.set_active_asr_session("session-2".to_string(), 2, 60_000);

        append_raw_transcript_if_needed(
            &mut session,
            &RawTranscript {
                text: "second part fallback".to_string(),
                duration_ms: 5_000,
            },
            observed,
            0,
        );

        let segment = session
            .record()
            .transcript_segments
            .first()
            .expect("raw fallback creates segment");
        assert_eq!(segment.start_ms, 60_000);
        assert_eq!(segment.end_ms, Some(65_000));
        let metadata = segment.metadata.as_ref().expect("metadata");
        assert_eq!(metadata.provider_id.as_deref(), Some("whisper"));
        assert_eq!(metadata.provider_session_id.as_deref(), Some("session-2"));
        assert_eq!(metadata.audio_part_index, Some(2));
        assert_eq!(metadata.session_start_ms, Some(60_000));
        assert_eq!(metadata.provider_start_ms, Some(0));
        assert_eq!(metadata.provider_end_ms, Some(5_000));
    }

    #[test]
    fn interrupted_flush_preserves_raw_text_and_marks_status() {
        let started = Utc.with_ymd_and_hms(2026, 7, 4, 9, 30, 0).unwrap();
        let observed = Utc.with_ymd_and_hms(2026, 7, 4, 9, 30, 5).unwrap();
        let mut session = MeetingSession::new(
            "550e8400-e29b-41d4-a716-446655440000".to_string(),
            started,
            "bailian".to_string(),
        );

        let error = apply_meeting_asr_flush_outcome(
            &mut session,
            &MeetingAsrFlushOutcome::InterruptedWithRaw {
                raw: RawTranscript {
                    text: "partial text".to_string(),
                    duration_ms: 5_000,
                },
                error: "connection failed".to_string(),
            },
            observed,
            0,
        );

        assert_eq!(error.as_deref(), Some("connection failed"));
        assert_eq!(
            session.record().status,
            crate::types::MeetingStatus::TranscribingInterrupted
        );
        assert_eq!(session.record().transcript_segments.len(), 1);
        assert_eq!(session.record().transcript_segments[0].text, "partial text");
    }

    #[test]
    fn commit_meeting_resume_rolls_back_when_persist_fails() {
        let started = Utc.with_ymd_and_hms(2026, 7, 4, 9, 30, 0).unwrap();
        let paused = Utc.with_ymd_and_hms(2026, 7, 4, 9, 31, 0).unwrap();
        let resumed = Utc.with_ymd_and_hms(2026, 7, 4, 9, 32, 0).unwrap();
        let mut session = MeetingSession::new(
            "550e8400-e29b-41d4-a716-446655440000".to_string(),
            started,
            "whisper".to_string(),
        );
        session.pause(paused).unwrap();

        let result = commit_meeting_resume(
            &mut session,
            resumed,
            "session-rollback".to_string(),
            2,
            60_000,
            true,
            |_record| Err("persist failed".into()),
        );

        assert_eq!(result, Err("persist failed".into()));
        assert_eq!(session.phase(), MeetingSessionPhase::Paused);
        assert_eq!(session.record().status, crate::types::MeetingStatus::Paused);
        assert_eq!(session.accumulated_paused_ms, 0);
        assert_eq!(session.paused_at, Some(paused));
    }

    #[test]
    fn commit_meeting_resume_without_asr_keeps_recording_and_marks_interrupted() {
        let started = Utc.with_ymd_and_hms(2026, 7, 4, 9, 30, 0).unwrap();
        let paused = Utc.with_ymd_and_hms(2026, 7, 4, 9, 31, 0).unwrap();
        let resumed = Utc.with_ymd_and_hms(2026, 7, 4, 9, 32, 0).unwrap();
        let mut session = MeetingSession::new(
            "550e8400-e29b-41d4-a716-446655440000".to_string(),
            started,
            "bailian".to_string(),
        );
        session.pause(paused).unwrap();

        let result = commit_meeting_resume(
            &mut session,
            resumed,
            "unused-session".to_string(),
            2,
            60_000,
            false,
            |_record| Ok(()),
        );

        assert_eq!(result, Ok(0));
        assert_eq!(session.phase(), MeetingSessionPhase::Recording);
        assert_eq!(
            session.record().status,
            crate::types::MeetingStatus::TranscribingInterrupted
        );
        assert!(session.snapshot(resumed).asr_interrupted);
        assert_eq!(session.snapshot(resumed).active_provider_session_id, None);
    }

    #[test]
    fn commit_meeting_asr_interruption_keeps_active_recording_session() {
        let started = Utc.with_ymd_and_hms(2026, 7, 4, 9, 30, 0).unwrap();
        let interrupted = Utc.with_ymd_and_hms(2026, 7, 4, 9, 30, 1).unwrap();
        let mut session = MeetingSession::new(
            "550e8400-e29b-41d4-a716-446655440000".to_string(),
            started,
            "bailian".to_string(),
        );
        session.set_active_asr_session("failed-session".to_string(), 1, 0);

        commit_meeting_asr_interruption(&mut session, interrupted, |_record| Ok(())).unwrap();

        let snapshot = session.snapshot(interrupted);
        assert_eq!(session.phase(), MeetingSessionPhase::Recording);
        assert_eq!(
            snapshot.phase,
            MeetingRecordingPhase::TranscribingInterrupted
        );
        assert!(snapshot.asr_interrupted);
        assert_eq!(snapshot.active_provider_session_id, None);
    }

    #[test]
    fn apply_meeting_audio_retention_state_marks_unavailable_when_any_part_failed() {
        let started = Utc.with_ymd_and_hms(2026, 7, 4, 9, 30, 0).unwrap();
        let mut record = MeetingSession::new(
            "550e8400-e29b-41d4-a716-446655440000".to_string(),
            started,
            "whisper".to_string(),
        )
        .record()
        .clone();

        let error = apply_meeting_audio_retention_state_with(&mut record, false, 20, |_id| {
            panic!("remove should not run when archive inactive")
        });

        assert_eq!(error, None);
        assert_eq!(record.audio.state, MeetingAudioState::Unavailable);
        assert!(!record.audio.retained);
        assert_eq!(record.audio.path, None);
    }

    #[test]
    fn apply_meeting_audio_retention_state_keeps_text_when_zero_retention_delete_fails() {
        let started = Utc.with_ymd_and_hms(2026, 7, 4, 9, 30, 0).unwrap();
        let mut record = MeetingSession::new(
            "550e8400-e29b-41d4-a716-446655440000".to_string(),
            started,
            "whisper".to_string(),
        )
        .record()
        .clone();
        record.append_transcript_segment_for_test("已识别原文", started);

        let error = apply_meeting_audio_retention_state_with(&mut record, true, 0, |_id| {
            Err("delete failed".into())
        });

        assert_eq!(error, Some("delete failed".into()));
        assert_eq!(record.audio.state, MeetingAudioState::Unavailable);
        assert!(!record.audio.retained);
        assert_eq!(record.audio.path, None);
        assert_eq!(record.transcript_segments.len(), 1);
    }

    #[test]
    fn apply_meeting_audio_retention_state_keeps_audio_while_processing_hold_exists() {
        let started = Utc.with_ymd_and_hms(2026, 7, 4, 9, 30, 0).unwrap();
        let mut record = MeetingSession::new(
            "550e8400-e29b-41d4-a716-446655440000".to_string(),
            started,
            "bailian".to_string(),
        )
        .record()
        .clone();
        record.processing_hold = Some(ProcessingHold {
            job_id: "job-1".to_string(),
            acquired_at: "2026-07-04T09:31:00Z".to_string(),
        });

        let error = apply_meeting_audio_retention_state_with(&mut record, true, 0, |_id| {
            panic!("processing hold must prevent audio deletion")
        });

        assert_eq!(error, None);
        assert_eq!(record.audio.state, MeetingAudioState::Retained);
        assert!(record.audio.retained);
    }

    trait MeetingRecordTestExt {
        fn append_transcript_segment_for_test(&mut self, text: &str, observed_at: DateTime<Utc>);
    }

    impl MeetingRecordTestExt for MeetingRecord {
        fn append_transcript_segment_for_test(&mut self, text: &str, observed_at: DateTime<Utc>) {
            self.transcript_segments.push(TranscriptSegment {
                id: "seg-000001".into(),
                speaker_id: None,
                speaker_label: "未区分".into(),
                start_ms: 0,
                end_ms: Some(duration_ms_between(
                    DateTime::parse_from_rfc3339(&self.started_at)
                        .unwrap()
                        .with_timezone(&Utc),
                    observed_at,
                )),
                text: text.into(),
                source: TranscriptSegmentSource::RealtimeAsr,
                metadata: None,
            });
        }
    }
}

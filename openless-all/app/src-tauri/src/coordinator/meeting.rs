use std::sync::mpsc;
use std::sync::Arc;
use std::time::Instant;

use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use tauri::Emitter;
use uuid::Uuid;

use crate::asr::{AsrFinalSegment, AsrFinalSegmentSink, RawTranscript};
use crate::coordinator_state::SessionPhase;
use crate::persistence::{
    meeting_recording_part_path_for_id, CredentialsVault, MeetingStore, PreferencesStore,
};
use crate::recorder::{Recorder, RecorderError};
use crate::types::{
    MeetingAudioMeta, MeetingAudioState, MeetingErrorEvent, MeetingRecord, MeetingRecordingPhase,
    MeetingRecordingSnapshot, MeetingStatus, MeetingSummary, MeetingTranscriptSegmentEvent,
    TranscriptSegment, TranscriptSegmentSource,
};

use super::{
    acquire_recording_mute, asr_transcribe_uses_global_timeout,
    build_qa_asr_start_with_final_segment_sink, cancel_active_asr, ensure_asr_credentials,
    ensure_microphone_permission, prepare_and_spawn_auto_meeting_summary, release_recording_mute,
    selected_microphone_device_name, stop_microphone_preview_monitor, ActiveAsr, Inner, QaAsrStart,
    COORDINATOR_GLOBAL_TIMEOUT_SECS,
};

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
    #[allow(dead_code)]
    active_provider: String,
}

impl MeetingSession {
    pub(super) fn new(
        meeting_id: String,
        started_at: DateTime<Utc>,
        active_provider: String,
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
                audio: MeetingAudioMeta {
                    state: MeetingAudioState::Temporary,
                    retained: false,
                    path: None,
                },
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
    ) -> Option<TranscriptSegment> {
        let text = text.trim();
        if text.is_empty() {
            return None;
        }
        let fallback_end_ms = self.elapsed_ms_at(observed_at);
        let end_ms = end_ms.or(Some(fallback_end_ms));
        let segment = TranscriptSegment {
            id: format!("seg-{:06}", self.next_segment_index),
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
                MeetingSessionPhase::Paused if self.asr_interrupted => {
                    MeetingRecordingPhase::TranscribingInterrupted
                }
                MeetingSessionPhase::Paused => MeetingRecordingPhase::Paused,
            },
            elapsed_ms: self.elapsed_ms_at(now),
            active_asr_provider: self.active_provider.clone(),
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

pub(super) async fn start_meeting_recording(
    inner: &Arc<Inner>,
) -> Result<MeetingRecordingSnapshot, String> {
    if inner.meeting_session.lock().is_some() {
        return Err("meeting recording already active".to_string());
    }
    if !matches!(inner.state.lock().phase, SessionPhase::Idle) {
        return Err("dictation is active".to_string());
    }
    ensure_asr_credentials()?;
    ensure_microphone_permission(inner)?;

    let active_provider = CredentialsVault::get_active_asr();
    let meeting_id = new_meeting_id();
    let started_at = Utc::now();
    let mut session = MeetingSession::new(meeting_id.clone(), started_at, active_provider.clone());
    MeetingStore::new()
        .map_err(|e| e.to_string())?
        .create(session.record().clone())
        .map_err(|e| e.to_string())?;
    session.mark_persisted_now(started_at);
    *inner.meeting_session.lock() = Some(session);
    *inner.meeting_segment_count_at_asr_start.lock() = 0;
    reset_meeting_audio_archive_state(inner);

    let asr_release_token = prepare_meeting_asr_release_token_for_provider(inner, &active_provider);
    let sink = meeting_final_segment_sink(inner, meeting_id.clone());
    let asr_start = match build_meeting_asr_start(inner, &active_provider, Some(sink)).await {
        Ok(asr_start) => asr_start,
        Err(error) => {
            schedule_meeting_local_asr_release_for_provider(
                inner,
                &active_provider,
                asr_release_token.clone(),
            );
            mark_start_failed_record(inner, &meeting_id, &error)?;
            return Err(error);
        }
    };
    if let Err(error) = asr_start.open_streaming_session().await {
        cleanup_unstored_meeting_asr_start(inner, &asr_start, asr_release_token.clone());
        mark_start_failed_record(inner, &meeting_id, &error)?;
        return Err(error);
    }
    let recorder =
        match start_meeting_recorder(inner, &meeting_id, asr_start.recorder_consumer()).await {
            Ok(recorder) => recorder,
            Err(error) => {
                cleanup_unstored_meeting_asr_start(inner, &asr_start, asr_release_token.clone());
                mark_start_failed_record(inner, &meeting_id, &error)?;
                return Err(error);
            }
        };

    *inner.meeting_asr.lock() = Some(asr_start.active_asr());
    *inner.meeting_recorder.lock() = Some(recorder);
    *inner.meeting_next_part_index.lock() = 2;

    let snapshot = meeting_snapshot(inner, Utc::now())?
        .ok_or_else(|| "meeting recording not active".to_string())?;
    emit_meeting_state(inner, &snapshot);
    Ok(snapshot)
}

pub(super) async fn pause_meeting_recording(
    inner: &Arc<Inner>,
    meeting_id: &str,
) -> Result<MeetingRecordingSnapshot, String> {
    ensure_active_meeting_id(inner, meeting_id)?;
    stop_meeting_recorder(inner);
    let flush_result = flush_current_meeting_asr(inner).await;
    {
        let mut session_guard = inner.meeting_session.lock();
        let session = session_guard
            .as_mut()
            .ok_or_else(|| "meeting recording not active".to_string())?;
        let now = Utc::now();
        match flush_result {
            Ok(Some(raw)) => {
                let session_start_count = *inner.meeting_segment_count_at_asr_start.lock();
                append_raw_transcript_if_needed(session, &raw, now, session_start_count);
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
    let active_provider = CredentialsVault::get_active_asr();
    let asr_release_token = prepare_meeting_asr_release_token_for_provider(inner, &active_provider);
    let sink = meeting_final_segment_sink(inner, meeting_id.to_string());
    let asr_start = match build_meeting_asr_start(inner, &active_provider, Some(sink)).await {
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
    if let Err(error) = asr_start.open_streaming_session().await {
        cleanup_unstored_meeting_asr_start(inner, &asr_start, asr_release_token.clone());
        return Err(error);
    }
    let recorder =
        match start_meeting_recorder(inner, meeting_id, asr_start.recorder_consumer()).await {
            Ok(recorder) => recorder,
            Err(error) => {
                cleanup_unstored_meeting_asr_start(inner, &asr_start, asr_release_token.clone());
                return Err(error);
            }
        };
    let commit_result = {
        let mut session_guard = inner.meeting_session.lock();
        match session_guard.as_mut() {
            Some(session) => {
                let now = Utc::now();
                commit_meeting_resume(session, now, persist_meeting_record)
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
            cleanup_unstored_meeting_asr_start(inner, &asr_start, asr_release_token);
            return Err(error);
        }
    }
    *inner.meeting_asr.lock() = Some(asr_start.active_asr());
    *inner.meeting_recorder.lock() = Some(recorder);
    let snapshot = meeting_snapshot(inner, Utc::now())?
        .ok_or_else(|| "meeting recording not active".to_string())?;
    emit_meeting_state(inner, &snapshot);
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
    let flush_result = flush_current_meeting_asr(inner).await;

    let now = Utc::now();
    let mut record = {
        let mut session_guard = inner.meeting_session.lock();
        let session = session_guard
            .as_mut()
            .ok_or_else(|| "meeting recording not active".to_string())?;

        let mut interrupted = false;
        match flush_result {
            Ok(Some(raw)) => {
                let session_start_count = *inner.meeting_segment_count_at_asr_start.lock();
                append_raw_transcript_if_needed(session, &raw, now, session_start_count);
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
        if let Some(error) = apply_meeting_audio_retention_state(inner, session.record_mut()) {
            emit_meeting_error(
                inner,
                Some(meeting_id.to_string()),
                "audioRetentionFailed",
                &error,
            );
        }
        session.record().clone()
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
    if record.status == MeetingStatus::Completed {
        if let Err(error) = prepare_and_spawn_auto_meeting_summary(inner, &mut record) {
            emit_meeting_error(
                inner,
                Some(meeting_id.to_string()),
                "summaryPrepareFailed",
                &error,
            );
            log::warn!("[meeting] summary prepare failed: {error}");
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
            active_asr_provider: CredentialsVault::get_active_asr(),
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
    active_asr: &str,
    final_segment_sink: Option<AsrFinalSegmentSink>,
) -> Result<QaAsrStart, String> {
    build_qa_asr_start_with_final_segment_sink(inner, active_asr, final_segment_sink).await
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
    let inner_for_level = Arc::clone(inner);
    let last_emit_at = Arc::new(Mutex::new(None::<Instant>));
    let level_handler: Arc<dyn Fn(f32) + Send + Sync> = Arc::new(move |_level| {
        let now = Instant::now();
        let mut last = last_emit_at.lock();
        if matches!(*last, Some(prev) if now.duration_since(prev).as_millis() < 500) {
            return;
        }
        *last = Some(now);
        if let Ok(Some(snapshot)) = meeting_snapshot(&inner_for_level, Utc::now()) {
            emit_meeting_state(&inner_for_level, &snapshot);
        }
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

async fn flush_current_meeting_asr(inner: &Arc<Inner>) -> Result<Option<RawTranscript>, String> {
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
fn schedule_foundry_meeting_release(inner: &Arc<Inner>) {
    let token = current_or_new_meeting_asr_release_token(inner);
    super::schedule_foundry_local_asr_release(inner, super::AsrReleaseSession::Meeting(token));
}

#[cfg(target_os = "windows")]
fn schedule_sherpa_meeting_release(inner: &Arc<Inner>) {
    let token = current_or_new_meeting_asr_release_token(inner);
    super::schedule_sherpa_onnx_release(inner, super::AsrReleaseSession::Meeting(token));
}

async fn flush_meeting_asr(inner: &Arc<Inner>, asr: ActiveAsr) -> Result<RawTranscript, String> {
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
                .map_err(|e| e.to_string())
        }
        ActiveAsr::Bailian(asr) => {
            if let Err(error) = asr.send_last_frame().await {
                log::warn!("[meeting] bailian send last frame failed: {error}");
            }
            let timeout = std::time::Duration::from_secs(COORDINATOR_GLOBAL_TIMEOUT_SECS);
            tokio::time::timeout(timeout, asr.await_final_result())
                .await
                .map_err(|_| "bailian transcribe timeout".to_string())?
                .map_err(|e| e.to_string())
        }
        ActiveAsr::Whisper(whisper) => {
            debug_assert!(uses_global_timeout);
            let timeout =
                super::whisper_transcribe_timeout((whisper.buffer_duration_ms() as f64) / 1000.0);
            tokio::time::timeout(timeout, whisper.transcribe())
                .await
                .map_err(|_| "whisper transcribe timeout".to_string())?
                .map_err(|e| e.to_string())
        }
        ActiveAsr::Mimo(mimo) => {
            debug_assert!(uses_global_timeout);
            let timeout = std::time::Duration::from_secs(COORDINATOR_GLOBAL_TIMEOUT_SECS);
            tokio::time::timeout(timeout, mimo.transcribe())
                .await
                .map_err(|_| "mimo transcribe timeout".to_string())?
                .map_err(|e| e.to_string())
        }
        #[cfg(target_os = "windows")]
        ActiveAsr::FoundryLocalWhisper(local) => {
            debug_assert!(!uses_global_timeout);
            let result = local
                .transcribe(super::foundry_audio_transcribe_timeout_duration())
                .await
                .map_err(|e| e.to_string());
            schedule_foundry_meeting_release(inner);
            result
        }
        #[cfg(target_os = "windows")]
        ActiveAsr::SherpaOnnxLocal(local) => {
            debug_assert!(!uses_global_timeout);
            let result = local
                .transcribe(super::sherpa_audio_transcribe_timeout_duration())
                .await
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
                .map_err(|e| e.to_string())
        }
    }
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
    let _ =
        session.append_transcript_segment(text_to_append, None, Some(raw.duration_ms), observed_at);
}

fn meeting_final_segment_sink(inner: &Arc<Inner>, meeting_id: String) -> AsrFinalSegmentSink {
    let inner = Arc::clone(inner);
    Arc::new(move |segment: AsrFinalSegment| {
        let emitted_segment = {
            let mut guard = inner.meeting_session.lock();
            let Some(session) = guard.as_mut() else {
                return;
            };
            if session.record().id != meeting_id {
                return;
            }
            if transcript_segment_exists(
                session.record(),
                &segment.text,
                segment.start_ms,
                segment.end_ms,
            ) {
                return;
            }
            let now = Utc::now();
            let emitted_segment = session.append_transcript_segment(
                &segment.text,
                segment.start_ms,
                segment.end_ms,
                now,
            );
            if emitted_segment.is_some() {
                session.mark_persisted_now(now);
                if let Err(error) = persist_meeting_record(session.record()) {
                    log::warn!("[meeting] persist realtime segment failed: {error}");
                }
            }
            emitted_segment
        };
        if let Some(segment) = emitted_segment {
            emit_meeting_transcript_segment(&inner, &meeting_id, &segment);
            if let Ok(Some(snapshot)) = meeting_snapshot(&inner, Utc::now()) {
                emit_meeting_state(&inner, &snapshot);
            }
        }
    })
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

fn persist_meeting_record(record: &MeetingRecord) -> Result<(), String> {
    MeetingStore::new()
        .map_err(|e| e.to_string())?
        .update(record.clone())
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "meeting not found".to_string())?;
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
    persist: impl FnOnce(&MeetingRecord) -> Result<(), String>,
) -> Result<usize, String> {
    let original = session.clone();
    let result = (|| {
        session.resume(now)?;
        persist(session.record())?;
        Ok(session.record().transcript_segments.len())
    })();
    if result.is_err() {
        *session = original;
    }
    result
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
    use crate::types::TranscriptSegmentSource;
    use chrono::{TimeZone, Utc};

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
        session.append_transcript_segment("已经识别的内容", Some(0), Some(1000), interrupted);

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
            .append_transcript_segment("我们接下来确认计划", Some(1200), Some(3400), started)
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
            .append_transcript_segment("   ", Some(0), Some(100), started)
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
            .append_transcript_segment("没有 provider 时间戳", None, None, observed)
            .expect("segment");

        assert_eq!(segment.start_ms, 0);
        assert_eq!(segment.end_ms, Some(5_000));
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
        session.append_transcript_segment("已经实时追加", Some(0), Some(1000), observed);

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

        let result =
            commit_meeting_resume(
                &mut session,
                resumed,
                |_record| Err("persist failed".into()),
            );

        assert_eq!(result, Err("persist failed".into()));
        assert_eq!(session.phase(), MeetingSessionPhase::Paused);
        assert_eq!(session.record().status, crate::types::MeetingStatus::Paused);
        assert_eq!(session.accumulated_paused_ms, 0);
        assert_eq!(session.paused_at, Some(paused));
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

    trait MeetingRecordTestExt {
        fn append_transcript_segment_for_test(&mut self, text: &str, observed_at: DateTime<Utc>);
    }

    impl MeetingRecordTestExt for MeetingRecord {
        fn append_transcript_segment_for_test(&mut self, text: &str, observed_at: DateTime<Utc>) {
            self.transcript_segments.push(TranscriptSegment {
                id: "seg-000001".into(),
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
            });
        }
    }
}

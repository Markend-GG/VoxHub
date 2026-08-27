use std::collections::HashSet;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use chrono::Utc;
use serde::Deserialize;
use tauri::Emitter;

use crate::persistence::{CredentialAccount, CredentialsVault, MeetingStore, PreferencesStore};
use crate::polish::{CODEX_DEFAULT_MODEL, CODEX_OAUTH_PROVIDER_ID};
use crate::types::{
    MeetingOrganizedDraft, MeetingOrganizedDraftEvent, MeetingOrganizedDraftItem,
    MeetingOrganizedDraftState, MeetingOrganizedDraftStatus, MeetingPostProcessingStatus,
    MeetingRecord, MeetingStatus, TranscriptSegment, UserPreferences,
};

use super::{complete_text_with_active_llm_with_timeout, Inner};

pub(super) const MEETING_ORGANIZED_CHUNK_TARGET_CHARS: usize = 12_000;
const MEETING_ORGANIZED_REQUEST_TIMEOUT_SECS: u64 = 180;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum MeetingOrganizedDraftMode {
    Auto,
    Generate,
    Retry,
    Regenerate,
}

type MeetingOrganizerLlm = dyn Fn(
        String,
        String,
        UserPreferences,
    ) -> Pin<Box<dyn Future<Output = Result<String, String>> + Send>>
    + Send
    + Sync;

#[derive(Debug, Clone, PartialEq, Eq)]
struct SourceGroup {
    source_segment_ids: Vec<String>,
    speaker_id: Option<String>,
    speaker_label: String,
    start_ms: u64,
    end_ms: Option<u64>,
    text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SourceChunk {
    groups: Vec<SourceGroup>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OrganizedChunkResponse {
    items: Vec<OrganizedChunkResponseItem>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OrganizedChunkResponseItem {
    source_segment_ids: Vec<String>,
    text: String,
}

pub(super) async fn generate_meeting_organized_draft(
    inner: &Arc<Inner>,
    meeting_id: &str,
    mode: MeetingOrganizedDraftMode,
) -> Result<MeetingRecord, String> {
    if has_active_recording_meeting(inner, meeting_id) {
        return Err("meeting recording is active".to_string());
    }
    let record = prepare_organized_draft(meeting_id, mode)?;
    emit_organized_draft(inner, &record);
    spawn_prepared_organized_draft(inner, meeting_id.to_string());
    Ok(record)
}

pub(super) fn prepare_and_spawn_auto_meeting_organized_draft(
    inner: &Arc<Inner>,
    record: &mut MeetingRecord,
) -> Result<(), String> {
    let source_revision = record.active_transcript_revision;
    if record
        .organized_draft
        .as_ref()
        .is_some_and(|draft| draft.source_transcript_revision == source_revision)
        || record.organized_draft_state.as_ref().is_some_and(|state| {
            state.source_transcript_revision == source_revision
                && matches!(
                    state.status,
                    MeetingOrganizedDraftStatus::Pending | MeetingOrganizedDraftStatus::Running
                )
        })
    {
        return Ok(());
    }
    let prepared = prepare_organized_draft(&record.id, MeetingOrganizedDraftMode::Auto)?;
    *record = prepared;
    emit_organized_draft(inner, record);
    spawn_prepared_organized_draft(inner, record.id.clone());
    Ok(())
}

pub(super) fn recover_meeting_organized_draft_jobs(inner: &Arc<Inner>) -> Result<(), String> {
    let store = MeetingStore::new().map_err(|error| error.to_string())?;
    for record in store.list().map_err(|error| error.to_string())? {
        let Some(state) = record.organized_draft_state.as_ref() else {
            continue;
        };
        if !matches!(
            state.status,
            MeetingOrganizedDraftStatus::Pending | MeetingOrganizedDraftStatus::Running
        ) {
            continue;
        }
        let job_id = state.job_id.clone();
        let now = Utc::now().to_rfc3339();
        if let Some(updated) = store
            .update_if(&record.id, |current| {
                apply_interrupted_organized_draft(current, &job_id, &now)
            })
            .map_err(|error| error.to_string())?
        {
            emit_organized_draft(inner, &updated);
        }
    }
    Ok(())
}

fn apply_interrupted_organized_draft(
    record: &mut MeetingRecord,
    expected_job_id: &str,
    now: &str,
) -> bool {
    let Some(state) = record.organized_draft_state.as_mut() else {
        return false;
    };
    if state.job_id != expected_job_id
        || !matches!(
            state.status,
            MeetingOrganizedDraftStatus::Pending | MeetingOrganizedDraftStatus::Running
        )
    {
        return false;
    }
    state.status = MeetingOrganizedDraftStatus::Failed;
    state.error_code = Some("organizedInterrupted".to_string());
    state.error_message = Some("organized draft generation was interrupted".to_string());
    state.updated_at = now.to_string();
    state.completed_at = Some(now.to_string());
    record.updated_at = now.to_string();
    true
}

fn prepare_organized_draft(
    meeting_id: &str,
    mode: MeetingOrganizedDraftMode,
) -> Result<MeetingRecord, String> {
    let store = MeetingStore::new().map_err(|error| error.to_string())?;
    let job_id = uuid::Uuid::new_v4().to_string();
    let now = Utc::now().to_rfc3339();
    let mut validation_error = None;
    let updated = store
        .update_if(
            meeting_id,
            |record| match apply_organized_draft_preparation(record, mode, &job_id, &now) {
                Ok(()) => true,
                Err(error) => {
                    validation_error = Some(error);
                    false
                }
            },
        )
        .map_err(|error| error.to_string())?;
    updated.ok_or_else(|| validation_error.unwrap_or_else(|| "meeting not found".to_string()))
}

fn apply_organized_draft_preparation(
    record: &mut MeetingRecord,
    mode: MeetingOrganizedDraftMode,
    job_id: &str,
    now: &str,
) -> Result<(), String> {
    validate_organized_draft_mode(record, mode)?;
    let source_revision = record.active_transcript_revision;
    let previous_state = record.organized_draft_state.as_ref();
    let processing_revision = previous_state
        .map(|state| state.processing_revision.saturating_add(1))
        .unwrap_or(1);
    let attempt = previous_state
        .map(|state| state.attempt.saturating_add(1))
        .unwrap_or(1);
    record.organized_draft_state = Some(MeetingOrganizedDraftState {
        status: MeetingOrganizedDraftStatus::Pending,
        job_id: job_id.to_string(),
        processing_revision,
        source_transcript_revision: source_revision,
        attempt,
        error_code: None,
        error_message: None,
        created_at: now.to_string(),
        updated_at: now.to_string(),
        started_at: None,
        completed_at: None,
    });
    record.updated_at = now.to_string();
    Ok(())
}

fn validate_organized_draft_mode(
    record: &MeetingRecord,
    mode: MeetingOrganizedDraftMode,
) -> Result<(), String> {
    if matches!(
        record.status,
        MeetingStatus::Draft
            | MeetingStatus::Recording
            | MeetingStatus::Paused
            | MeetingStatus::TranscribingInterrupted
    ) {
        return Err("meeting transcript is not finalized".to_string());
    }
    if record.post_processing.as_ref().is_some_and(|state| {
        !matches!(
            state.status,
            MeetingPostProcessingStatus::Completed | MeetingPostProcessingStatus::RealtimeAccepted
        )
    }) {
        return Err("meeting post-processing is not completed".to_string());
    }
    if record
        .transcript_segments
        .iter()
        .all(|segment| segment.text.trim().is_empty())
    {
        return Err("meeting transcript is empty".to_string());
    }
    if let Some(state) = record.organized_draft_state.as_ref().filter(|state| {
        matches!(
            state.status,
            MeetingOrganizedDraftStatus::Pending | MeetingOrganizedDraftStatus::Running
        )
    }) {
        let supersedes_old_revision = mode == MeetingOrganizedDraftMode::Auto
            && state.source_transcript_revision != record.active_transcript_revision;
        if !supersedes_old_revision {
            return Err("meeting organized draft already running".to_string());
        }
    }
    match mode {
        MeetingOrganizedDraftMode::Auto | MeetingOrganizedDraftMode::Generate => {
            let current_source = record.active_transcript_revision;
            if record
                .organized_draft
                .as_ref()
                .is_some_and(|draft| draft.source_transcript_revision == current_source)
            {
                return Err("meeting organized draft already exists".to_string());
            }
            Ok(())
        }
        MeetingOrganizedDraftMode::Retry => {
            if record
                .organized_draft_state
                .as_ref()
                .is_some_and(|state| state.status == MeetingOrganizedDraftStatus::Failed)
            {
                Ok(())
            } else {
                Err("meeting organized draft has not failed".to_string())
            }
        }
        MeetingOrganizedDraftMode::Regenerate => {
            if record.organized_draft.is_some() {
                Ok(())
            } else {
                Err("meeting organized draft does not exist".to_string())
            }
        }
    }
}

fn spawn_prepared_organized_draft(inner: &Arc<Inner>, meeting_id: String) {
    let inner = Arc::clone(inner);
    tauri::async_runtime::spawn(async move {
        if let Err(error) =
            run_prepared_organized_draft(&inner, &meeting_id, &call_active_llm).await
        {
            log::warn!("[meeting-organizer] generation failed: {error}");
        }
    });
}

async fn run_prepared_organized_draft(
    inner: &Arc<Inner>,
    meeting_id: &str,
    llm: &MeetingOrganizerLlm,
) -> Result<MeetingRecord, String> {
    let store = MeetingStore::new().map_err(|error| error.to_string())?;
    let pending = store
        .get(meeting_id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "meeting not found".to_string())?;
    let pending_state = pending
        .organized_draft_state
        .as_ref()
        .filter(|state| state.status == MeetingOrganizedDraftStatus::Pending)
        .cloned()
        .ok_or_else(|| "meeting organized draft is not prepared".to_string())?;
    let now = Utc::now().to_rfc3339();
    let running = store
        .update_if(meeting_id, |record| {
            let Some(state) = record.organized_draft_state.as_mut() else {
                return false;
            };
            if state.job_id != pending_state.job_id
                || state.status != MeetingOrganizedDraftStatus::Pending
                || record.active_transcript_revision != state.source_transcript_revision
            {
                return false;
            }
            state.status = MeetingOrganizedDraftStatus::Running;
            state.started_at = Some(now.clone());
            state.updated_at = now.clone();
            record.updated_at = now.clone();
            true
        })
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "meeting organized draft job changed".to_string())?;
    emit_organized_draft(inner, &running);

    let prefs = PreferencesStore::new()
        .unwrap_or_else(|_| PreferencesStore::new_fallback())
        .get();
    let (provider_id, model_id) = active_llm_identity();
    let result = organize_record(&running, &prefs, llm).await;
    match result {
        Ok(items) => {
            complete_organized_draft(inner, &store, &running, provider_id, model_id, items)
        }
        Err(error) => fail_organized_draft(inner, &store, &running, &error),
    }
}

async fn organize_record(
    record: &MeetingRecord,
    prefs: &UserPreferences,
    llm: &MeetingOrganizerLlm,
) -> Result<Vec<MeetingOrganizedDraftItem>, String> {
    validate_source_segment_ids(&record.transcript_segments)?;
    let chunks = build_source_chunks(&record.transcript_segments);
    if chunks.is_empty() {
        return Err("meeting transcript is empty".to_string());
    }
    let mut items = Vec::new();
    for (index, chunk) in chunks.iter().enumerate() {
        let (system, user) = build_organized_draft_prompt(chunk, index + 1, chunks.len());
        let raw = llm(system, user, prefs.clone()).await?;
        items.extend(parse_organized_chunk_response(&raw, chunk)?);
    }
    Ok(items)
}

fn validate_source_segment_ids(segments: &[TranscriptSegment]) -> Result<(), String> {
    let mut seen = HashSet::new();
    for segment in segments
        .iter()
        .filter(|segment| !segment.text.trim().is_empty())
    {
        if segment.id.trim().is_empty() || !seen.insert(segment.id.as_str()) {
            return Err(
                "invalid organized draft mapping: source segment ids must be non-empty and unique"
                    .to_string(),
            );
        }
    }
    Ok(())
}

fn build_source_chunks(segments: &[TranscriptSegment]) -> Vec<SourceChunk> {
    let mut groups = Vec::<SourceGroup>::new();
    for segment in segments
        .iter()
        .filter(|segment| !segment.text.trim().is_empty())
    {
        let text = segment.text.trim();
        let same_speaker = groups.last().is_some_and(|group| {
            group.speaker_id == segment.speaker_id && group.speaker_label == segment.speaker_label
        });
        let can_append = same_speaker
            && groups.last().is_some_and(|group| {
                group.text.chars().count() + text.chars().count() + 1
                    <= MEETING_ORGANIZED_CHUNK_TARGET_CHARS
            });
        if can_append {
            let group = groups.last_mut().expect("checked above");
            group.source_segment_ids.push(segment.id.clone());
            group.text.push('\n');
            group.text.push_str(text);
            group.end_ms = segment.end_ms;
        } else {
            groups.push(SourceGroup {
                source_segment_ids: vec![segment.id.clone()],
                speaker_id: segment.speaker_id.clone(),
                speaker_label: segment.speaker_label.clone(),
                start_ms: segment.start_ms,
                end_ms: segment.end_ms,
                text: text.to_string(),
            });
        }
    }

    let mut chunks = Vec::new();
    let mut current = SourceChunk { groups: Vec::new() };
    let mut current_chars = 0usize;
    for group in groups {
        let group_chars = group.text.chars().count();
        if !current.groups.is_empty()
            && current_chars + group_chars > MEETING_ORGANIZED_CHUNK_TARGET_CHARS
        {
            chunks.push(current);
            current = SourceChunk { groups: Vec::new() };
            current_chars = 0;
        }
        current_chars += group_chars;
        current.groups.push(group);
    }
    if !current.groups.is_empty() {
        chunks.push(current);
    }
    chunks
}

fn build_organized_draft_prompt(
    chunk: &SourceChunk,
    index: usize,
    total: usize,
) -> (String, String) {
    let system = "你是 OpenLess 的会议整理助手。你的任务是把 ASR 会议转写改写成保真、自然的书面表达。删除无意义填充词、口头重复、断裂句和无意义自我修正；修正标点、断句、语序，以及上下文足够明确的明显 ASR 错词。不得总结、翻译、补充信息或改变数字、专有名词、否定关系、观点归属和不确定语气。只输出 JSON，不输出 Markdown 或解释。".to_string();
    let source = chunk
        .groups
        .iter()
        .map(|group| {
            format!(
                "[sourceSegmentIds={}][speaker={}][startMs={}][endMs={}]\n{}",
                group.source_segment_ids.join(","),
                group.speaker_label,
                group.start_ms,
                group
                    .end_ms
                    .map(|value| value.to_string())
                    .unwrap_or_default(),
                group.text
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    let user = format!(
        "这是会议转写的第 {index}/{total} 个分块。请逐组整理，保持输入组数量和顺序不变。每个输出项的 sourceSegmentIds 必须与对应输入完全一致，text 必须非空。\n\n输入：\n{source}\n\n只输出以下 JSON 结构：\n{{\"items\":[{{\"sourceSegmentIds\":[\"seg-1\"],\"text\":\"整理后的文本\"}}]}}"
    );
    (system, user)
}

fn parse_organized_chunk_response(
    raw: &str,
    chunk: &SourceChunk,
) -> Result<Vec<MeetingOrganizedDraftItem>, String> {
    let cleaned = strip_json_code_fence(raw);
    let response: OrganizedChunkResponse = serde_json::from_str(&cleaned)
        .map_err(|error| format!("invalid organized draft json: {error}"))?;
    if response.items.len() != chunk.groups.len() {
        return Err("invalid organized draft mapping: item count changed".to_string());
    }
    response
        .items
        .into_iter()
        .zip(&chunk.groups)
        .map(|(item, group)| {
            if item.source_segment_ids != group.source_segment_ids {
                return Err(
                    "invalid organized draft mapping: source segment ids changed".to_string(),
                );
            }
            let text = item.text.trim().to_string();
            if text.is_empty() {
                return Err("invalid organized draft mapping: empty text".to_string());
            }
            Ok(MeetingOrganizedDraftItem {
                source_segment_ids: group.source_segment_ids.clone(),
                speaker_id: group.speaker_id.clone(),
                speaker_label: group.speaker_label.clone(),
                start_ms: group.start_ms,
                end_ms: group.end_ms,
                text,
            })
        })
        .collect()
}

fn complete_organized_draft(
    inner: &Arc<Inner>,
    store: &MeetingStore,
    snapshot: &MeetingRecord,
    provider_id: String,
    model_id: Option<String>,
    items: Vec<MeetingOrganizedDraftItem>,
) -> Result<MeetingRecord, String> {
    let snapshot_state = snapshot
        .organized_draft_state
        .as_ref()
        .ok_or_else(|| "meeting organized draft state is missing".to_string())?;
    let job_id = snapshot_state.job_id.clone();
    let source_revision = snapshot_state.source_transcript_revision;
    let now = Utc::now().to_rfc3339();
    let updated = store
        .update_if(&snapshot.id, |record| {
            apply_organized_draft_completion(
                record,
                &job_id,
                source_revision,
                &provider_id,
                model_id.as_deref(),
                &items,
                &now,
            )
        })
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "meeting organized draft job changed".to_string())?;
    emit_organized_draft(inner, &updated);
    Ok(updated)
}

fn apply_organized_draft_completion(
    record: &mut MeetingRecord,
    expected_job_id: &str,
    expected_source_revision: Option<u32>,
    provider_id: &str,
    model_id: Option<&str>,
    items: &[MeetingOrganizedDraftItem],
    now: &str,
) -> bool {
    let Some(state) = record.organized_draft_state.as_mut() else {
        return false;
    };
    if state.job_id != expected_job_id
        || state.status != MeetingOrganizedDraftStatus::Running
        || record.active_transcript_revision != expected_source_revision
    {
        return false;
    }
    record.organized_draft = Some(MeetingOrganizedDraft {
        source_transcript_revision: expected_source_revision,
        provider_id: provider_id.to_string(),
        model_id: model_id.map(str::to_string),
        items: items.to_vec(),
        generated_at: now.to_string(),
    });
    state.status = MeetingOrganizedDraftStatus::Completed;
    state.error_code = None;
    state.error_message = None;
    state.updated_at = now.to_string();
    state.completed_at = Some(now.to_string());
    record.updated_at = now.to_string();
    true
}

fn fail_organized_draft(
    inner: &Arc<Inner>,
    store: &MeetingStore,
    snapshot: &MeetingRecord,
    error: &str,
) -> Result<MeetingRecord, String> {
    let snapshot_state = snapshot
        .organized_draft_state
        .as_ref()
        .ok_or_else(|| "meeting organized draft state is missing".to_string())?;
    let job_id = snapshot_state.job_id.clone();
    let now = Utc::now().to_rfc3339();
    let updated = store
        .update_if(&snapshot.id, |record| {
            apply_organized_draft_failure(record, &job_id, error, &now)
        })
        .map_err(|store_error| store_error.to_string())?
        .ok_or_else(|| "meeting organized draft job changed".to_string())?;
    emit_organized_draft(inner, &updated);
    Ok(updated)
}

fn apply_organized_draft_failure(
    record: &mut MeetingRecord,
    expected_job_id: &str,
    error: &str,
    now: &str,
) -> bool {
    let Some(state) = record.organized_draft_state.as_mut() else {
        return false;
    };
    if state.job_id != expected_job_id || state.status != MeetingOrganizedDraftStatus::Running {
        return false;
    }
    state.status = MeetingOrganizedDraftStatus::Failed;
    state.error_code = Some(organized_error_code(error).to_string());
    state.error_message = Some(error.to_string());
    state.updated_at = now.to_string();
    state.completed_at = Some(now.to_string());
    record.updated_at = now.to_string();
    true
}

fn organized_error_code(error: &str) -> &'static str {
    if error.starts_with("invalid organized draft json") {
        "organizedInvalidJson"
    } else if error.starts_with("invalid organized draft mapping") {
        "organizedInvalidMapping"
    } else {
        "organizedLlmFailed"
    }
}

fn emit_organized_draft(inner: &Arc<Inner>, record: &MeetingRecord) {
    let Some(state) = record.organized_draft_state.clone() else {
        return;
    };
    if let Some(app) = inner.app.lock().clone() {
        let _ = app.emit(
            "meeting:organized-draft",
            MeetingOrganizedDraftEvent {
                meeting_id: record.id.clone(),
                state,
                meeting: record.clone(),
            },
        );
    }
}

fn active_llm_identity() -> (String, Option<String>) {
    let provider_id = CredentialsVault::get_active_llm();
    let configured_model = CredentialsVault::get(CredentialAccount::ArkModelId)
        .ok()
        .flatten()
        .map(|model| model.trim().to_string())
        .filter(|model| !model.is_empty());
    let model_id = configured_model.unwrap_or_else(|| match provider_id.as_str() {
        CODEX_OAUTH_PROVIDER_ID => CODEX_DEFAULT_MODEL.to_string(),
        "gemini" => "gemini-2.5-flash".to_string(),
        _ => "deepseek-v3-2".to_string(),
    });
    (provider_id, Some(model_id))
}

fn call_active_llm(
    system_prompt: String,
    user_prompt: String,
    prefs: UserPreferences,
) -> Pin<Box<dyn Future<Output = Result<String, String>> + Send>> {
    Box::pin(async move {
        complete_text_with_active_llm_with_timeout(
            &system_prompt,
            &user_prompt,
            &prefs.working_languages,
            prefs.chinese_script_preference,
            prefs.output_language_preference,
            prefs.llm_thinking_enabled,
            Some(MEETING_ORGANIZED_REQUEST_TIMEOUT_SECS),
        )
        .await
        .map_err(|error| error.to_string())
    })
}

fn strip_json_code_fence(raw: &str) -> String {
    let trimmed = raw.trim();
    if !trimmed.starts_with("```") {
        return trimmed.to_string();
    }
    let without_start = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```JSON"))
        .or_else(|| trimmed.strip_prefix("```"))
        .unwrap_or(trimmed)
        .trim_start();
    without_start
        .strip_suffix("```")
        .unwrap_or(without_start)
        .trim()
        .to_string()
}

fn has_active_recording_meeting(inner: &Arc<Inner>, meeting_id: &str) -> bool {
    inner
        .meeting_session
        .lock()
        .as_ref()
        .map(|session| session.record().id == meeting_id)
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use crate::types::{
        MeetingAudioMeta, MeetingAudioState, MeetingSummary, TranscriptSegmentSource,
    };

    fn segment(id: &str, speaker: &str, start_ms: u64, text: &str) -> TranscriptSegment {
        TranscriptSegment {
            id: id.to_string(),
            speaker_id: Some(speaker.to_string()),
            speaker_label: speaker.to_string(),
            start_ms,
            end_ms: Some(start_ms + 500),
            text: text.to_string(),
            source: TranscriptSegmentSource::RetranscribedAsr,
            metadata: None,
        }
    }

    fn record_with_segments(segments: Vec<TranscriptSegment>) -> MeetingRecord {
        MeetingRecord {
            id: "meeting-organizer-test".to_string(),
            title: "Organizer test".to_string(),
            status: MeetingStatus::Completed,
            started_at: "2026-08-25T08:00:00Z".to_string(),
            ended_at: Some("2026-08-25T09:00:00Z".to_string()),
            duration_ms: Some(3_600_000),
            transcript_segments: segments,
            summary: MeetingSummary::default(),
            organized_draft: None,
            organized_draft_state: None,
            audio: MeetingAudioMeta {
                state: MeetingAudioState::Retained,
                retained: true,
                path: None,
            },
            realtime_asr: None,
            post_processing_config: None,
            post_processing: None,
            import_config: None,
            import_state: None,
            transcript_revisions: Vec::new(),
            active_transcript_revision: Some(1),
            speaker_profiles: Vec::new(),
            speaker_turns: Vec::new(),
            processing_hold: None,
            created_at: "2026-08-25T08:00:00Z".to_string(),
            updated_at: "2026-08-25T09:00:00Z".to_string(),
        }
    }

    fn organized_state(
        job_id: &str,
        status: MeetingOrganizedDraftStatus,
    ) -> MeetingOrganizedDraftState {
        MeetingOrganizedDraftState {
            status,
            job_id: job_id.to_string(),
            processing_revision: 1,
            source_transcript_revision: Some(1),
            attempt: 1,
            error_code: None,
            error_message: None,
            created_at: "2026-08-25T09:00:00Z".to_string(),
            updated_at: "2026-08-25T09:00:00Z".to_string(),
            started_at: Some("2026-08-25T09:00:00Z".to_string()),
            completed_at: None,
        }
    }

    fn existing_draft(text: &str) -> MeetingOrganizedDraft {
        MeetingOrganizedDraft {
            source_transcript_revision: Some(1),
            provider_id: "old-provider".to_string(),
            model_id: Some("old-model".to_string()),
            items: vec![MeetingOrganizedDraftItem {
                source_segment_ids: vec!["s1".to_string()],
                speaker_id: Some("A".to_string()),
                speaker_label: "A".to_string(),
                start_ms: 0,
                end_ms: Some(500),
                text: text.to_string(),
            }],
            generated_at: "2026-08-25T08:30:00Z".to_string(),
        }
    }

    #[test]
    fn chunks_merge_only_adjacent_segments_from_same_speaker() {
        let chunks = build_source_chunks(&[
            segment("s1", "A", 0, "嗯，先开始"),
            segment("s2", "A", 600, "然后看发布计划"),
            segment("s3", "B", 1200, "我来确认测试"),
        ]);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].groups.len(), 2);
        assert_eq!(chunks[0].groups[0].source_segment_ids, ["s1", "s2"]);
        assert_eq!(chunks[0].groups[1].source_segment_ids, ["s3"]);
    }

    #[test]
    fn organizer_uses_long_form_request_timeout() {
        assert_eq!(MEETING_ORGANIZED_REQUEST_TIMEOUT_SECS, 180);
    }

    #[test]
    fn parses_valid_mapping_and_copies_server_metadata() {
        let chunk = build_source_chunks(&[segment("s1", "A", 0, "嗯，先开始")]).remove(0);
        let items = parse_organized_chunk_response(
            r#"{"items":[{"sourceSegmentIds":["s1"],"text":"先开始。"}]}"#,
            &chunk,
        )
        .unwrap();
        assert_eq!(items[0].speaker_id.as_deref(), Some("A"));
        assert_eq!(items[0].start_ms, 0);
        assert_eq!(items[0].text, "先开始。");
    }

    #[test]
    fn rejects_changed_source_mapping() {
        let chunk = build_source_chunks(&[segment("s1", "A", 0, "开始")]).remove(0);
        let error = parse_organized_chunk_response(
            r#"{"items":[{"sourceSegmentIds":["forged"],"text":"开始。"}]}"#,
            &chunk,
        )
        .unwrap_err();
        assert!(error.contains("source segment ids changed"));
    }

    #[test]
    fn rejects_invalid_json_and_empty_text() {
        let chunk = build_source_chunks(&[segment("s1", "A", 0, "开始")]).remove(0);
        assert!(parse_organized_chunk_response("not json", &chunk)
            .unwrap_err()
            .starts_with("invalid organized draft json"));
        assert!(parse_organized_chunk_response(
            r#"{"items":[{"sourceSegmentIds":["s1"],"text":""}]}"#,
            &chunk,
        )
        .unwrap_err()
        .contains("empty text"));
    }

    #[test]
    fn splits_long_meetings_without_splitting_segments() {
        let first = "a".repeat(MEETING_ORGANIZED_CHUNK_TARGET_CHARS - 10);
        let second = "b".repeat(20);
        let chunks = build_source_chunks(&[
            segment("s1", "A", 0, &first),
            segment("s2", "B", 1000, &second),
        ]);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].groups[0].source_segment_ids, ["s1"]);
        assert_eq!(chunks[1].groups[0].source_segment_ids, ["s2"]);
    }

    #[test]
    fn rejects_duplicate_or_empty_source_segment_ids() {
        assert!(validate_source_segment_ids(&[
            segment("s1", "A", 0, "first"),
            segment("s1", "A", 600, "second"),
        ])
        .unwrap_err()
        .contains("non-empty and unique"));
        assert!(validate_source_segment_ids(&[segment("", "A", 0, "first")])
            .unwrap_err()
            .contains("non-empty and unique"));
    }

    #[tokio::test]
    async fn partial_chunk_failure_returns_no_partial_draft() {
        let record = record_with_segments(vec![
            segment(
                "s1",
                "A",
                0,
                &"a".repeat(MEETING_ORGANIZED_CHUNK_TARGET_CHARS - 10),
            ),
            segment("s2", "B", 1_000, &"b".repeat(20)),
        ]);
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_for_llm = Arc::clone(&calls);
        let llm = move |_system: String, _user: String, _prefs: UserPreferences| {
            let call = calls_for_llm.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move {
                if call == 0 {
                    Ok(r#"{"items":[{"sourceSegmentIds":["s1"],"text":"first"}]}"#.to_string())
                } else {
                    Err("second chunk failed".to_string())
                }
            }) as Pin<Box<dyn Future<Output = Result<String, String>> + Send>>
        };

        let result = organize_record(&record, &UserPreferences::default(), &llm).await;

        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(result, Err("second chunk failed".to_string()));
    }

    #[test]
    fn automatic_new_revision_supersedes_old_running_job() {
        let mut record = record_with_segments(vec![segment("s1", "A", 0, "first")]);
        record.active_transcript_revision = Some(2);
        record.summary.overview = "concurrent summary".to_string();
        record.organized_draft = Some(existing_draft("old draft"));
        record.organized_draft_state = Some(organized_state(
            "job-old",
            MeetingOrganizedDraftStatus::Running,
        ));

        apply_organized_draft_preparation(
            &mut record,
            MeetingOrganizedDraftMode::Auto,
            "job-new",
            "2026-08-25T09:30:00Z",
        )
        .unwrap();

        let state = record.organized_draft_state.as_ref().unwrap();
        assert_eq!(state.status, MeetingOrganizedDraftStatus::Pending);
        assert_eq!(state.job_id, "job-new");
        assert_eq!(state.source_transcript_revision, Some(2));
        assert_eq!(state.processing_revision, 2);
        assert_eq!(state.attempt, 2);
        assert_eq!(
            record.organized_draft.as_ref().unwrap().items[0].text,
            "old draft"
        );
        assert_eq!(record.summary.overview, "concurrent summary");
    }

    #[test]
    fn automatic_same_revision_does_not_duplicate_running_job() {
        let mut record = record_with_segments(vec![segment("s1", "A", 0, "first")]);
        record.organized_draft_state = Some(organized_state(
            "job-current",
            MeetingOrganizedDraftStatus::Running,
        ));

        assert_eq!(
            apply_organized_draft_preparation(
                &mut record,
                MeetingOrganizedDraftMode::Auto,
                "job-new",
                "2026-08-25T09:30:00Z",
            ),
            Err("meeting organized draft already running".to_string())
        );
        assert_eq!(record.organized_draft_state.unwrap().job_id, "job-current");
    }

    #[test]
    fn stale_job_completion_cannot_replace_existing_draft() {
        let mut record = record_with_segments(vec![segment("s1", "A", 0, "first")]);
        record.organized_draft = Some(existing_draft("old draft"));
        record.organized_draft_state = Some(organized_state(
            "job-new",
            MeetingOrganizedDraftStatus::Running,
        ));
        let replacement = existing_draft("new draft").items;

        assert!(!apply_organized_draft_completion(
            &mut record,
            "job-old",
            Some(1),
            "new-provider",
            Some("new-model"),
            &replacement,
            "2026-08-25T09:30:00Z",
        ));
        assert_eq!(record.organized_draft.unwrap().items[0].text, "old draft");
    }

    #[test]
    fn failed_regeneration_keeps_last_successful_draft() {
        let mut record = record_with_segments(vec![segment("s1", "A", 0, "first")]);
        record.organized_draft = Some(existing_draft("old draft"));
        record.organized_draft_state = Some(organized_state(
            "job-current",
            MeetingOrganizedDraftStatus::Running,
        ));

        assert!(apply_organized_draft_failure(
            &mut record,
            "job-current",
            "LLM unavailable",
            "2026-08-25T09:30:00Z",
        ));
        assert_eq!(record.organized_draft.unwrap().items[0].text, "old draft");
        let state = record.organized_draft_state.unwrap();
        assert_eq!(state.status, MeetingOrganizedDraftStatus::Failed);
        assert_eq!(state.error_code.as_deref(), Some("organizedLlmFailed"));
    }

    #[test]
    fn interrupted_job_becomes_retryable_without_removing_old_draft() {
        let mut record = record_with_segments(vec![segment("s1", "A", 0, "first")]);
        record.organized_draft = Some(existing_draft("old draft"));
        record.organized_draft_state = Some(organized_state(
            "job-current",
            MeetingOrganizedDraftStatus::Pending,
        ));

        assert!(apply_interrupted_organized_draft(
            &mut record,
            "job-current",
            "2026-08-25T09:30:00Z",
        ));
        assert_eq!(record.organized_draft.unwrap().items[0].text, "old draft");
        let state = record.organized_draft_state.unwrap();
        assert_eq!(state.status, MeetingOrganizedDraftStatus::Failed);
        assert_eq!(state.error_code.as_deref(), Some("organizedInterrupted"));
    }

    #[test]
    fn auto_generation_accepts_a_stale_successful_draft() {
        let mut record = record_with_segments(vec![segment("s1", "A", 0, "first")]);
        record.active_transcript_revision = Some(2);
        record.organized_draft = Some(existing_draft("old draft"));
        record.organized_draft_state = Some(organized_state(
            "job-old",
            MeetingOrganizedDraftStatus::Completed,
        ));

        assert_eq!(
            validate_organized_draft_mode(&record, MeetingOrganizedDraftMode::Auto),
            Ok(())
        );
    }
}

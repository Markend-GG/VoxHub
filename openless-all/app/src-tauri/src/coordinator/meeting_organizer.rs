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
const MEETING_ORGANIZED_CHUNK_MAX_GROUPS: usize = 48;
const MEETING_ORGANIZED_REQUEST_TIMEOUT_SECS: u64 = 180;
const MEETING_ORGANIZED_FORMAT_ATTEMPTS: usize = 2;

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
    group_id: String,
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
        let mut parsed_items = None;
        for attempt in 0..MEETING_ORGANIZED_FORMAT_ATTEMPTS {
            let (system, user) =
                build_organized_draft_prompt(chunk, index + 1, chunks.len(), attempt > 0);
            let raw = llm(system, user, prefs.clone()).await?;
            match parse_organized_chunk_response(&raw, chunk) {
                Ok(chunk_items) => {
                    parsed_items = Some(chunk_items);
                    break;
                }
                Err(error) if attempt + 1 < MEETING_ORGANIZED_FORMAT_ATTEMPTS => {
                    log::warn!(
                        "[meeting-organizer] chunk {}/{} response rejected, retrying format once: {}",
                        index + 1,
                        chunks.len(),
                        error
                    );
                }
                Err(error) => return Err(error),
            }
        }
        items.extend(
            parsed_items.ok_or_else(|| {
                "meeting organizer format retry attempts were exhausted".to_string()
            })?,
        );
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
    let groups = segments
        .iter()
        .filter(|segment| !segment.text.trim().is_empty())
        .map(|segment| SourceGroup {
            source_segment_ids: vec![segment.id.clone()],
            speaker_id: segment.speaker_id.clone(),
            speaker_label: segment.speaker_label.clone(),
            start_ms: segment.start_ms,
            end_ms: segment.end_ms,
            text: segment.text.trim().to_string(),
        })
        .collect::<Vec<_>>();

    let mut chunks = Vec::new();
    let mut current = SourceChunk { groups: Vec::new() };
    let mut current_chars = 0usize;
    for group in groups {
        let group_chars = group.text.chars().count();
        if !current.groups.is_empty()
            && (current_chars + group_chars > MEETING_ORGANIZED_CHUNK_TARGET_CHARS
                || current.groups.len() >= MEETING_ORGANIZED_CHUNK_MAX_GROUPS)
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
    format_retry: bool,
) -> (String, String) {
    let system = "你是 OpenLess 的会议整理助手。你的任务是把 ASR 会议转写逐段改写成保真、自然的书面表达。删除无意义填充词、口头重复、断裂句和无意义自我修正；修正标点、断句、语序，以及上下文足够明确的明显 ASR 错词。可以参考相邻段落理解上下文，但不得总结、翻译、补充信息，不得改变数字、专有名词、否定关系、观点归属和不确定语气，也不得在段落之间移动内容。每个输入 groupId 代表一个原文段落，必须且只能对应一个输出项，不得拆分、合并、遗漏或新增 groupId。只输出 JSON，不输出 Markdown 或解释。".to_string();
    let source = chunk
        .groups
        .iter()
        .enumerate()
        .map(|(group_index, group)| {
            format!(
                "[groupId=g{}][speaker={}][startMs={}][endMs={}]\n{}",
                group_index + 1,
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
    let retry_instruction = if format_retry {
        "\n\n上一次响应未通过格式校验。请特别检查：items 数量必须等于输入组数量；groupId 必须从 g1 开始按输入顺序逐项出现且只出现一次；每项 text 必须是非空字符串。"
    } else {
        ""
    };
    let user = format!(
        "这是会议转写的第 {index}/{total} 个分块。请逐段整理，保持原文段落数量和顺序不变。每个输出项的 groupId 必须与对应输入完全一致，text 必须非空。{retry_instruction}\n\n输入：\n{source}\n\n只输出以下 JSON 结构：\n{{\"items\":[{{\"groupId\":\"g1\",\"text\":\"整理后的文本\"}}]}}"
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
        .zip(chunk.groups.iter().enumerate())
        .map(|(item, (group_index, group))| {
            let expected_group_id = format!("g{}", group_index + 1);
            if item.group_id != expected_group_id {
                return Err("invalid organized draft mapping: group ids changed".to_string());
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
    if error.contains("output truncated") {
        "organizedOutputTruncated"
    } else if error.starts_with("invalid organized draft json") {
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
    fn chunks_preserve_source_segments_from_same_speaker() {
        let chunks = build_source_chunks(&[
            segment("s1", "A", 0, "嗯，先开始"),
            segment("s2", "A", 600, "然后看发布计划"),
            segment("s3", "B", 1200, "我来确认测试"),
        ]);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].groups.len(), 3);
        assert_eq!(chunks[0].groups[0].source_segment_ids, ["s1"]);
        assert_eq!(chunks[0].groups[1].source_segment_ids, ["s2"]);
        assert_eq!(chunks[0].groups[2].source_segment_ids, ["s3"]);
        assert_eq!(chunks[0].groups[0].speaker_label, "A");
        assert_eq!(chunks[0].groups[1].start_ms, 600);
    }

    #[test]
    fn organizer_uses_long_form_request_timeout() {
        assert_eq!(MEETING_ORGANIZED_REQUEST_TIMEOUT_SECS, 180);
    }

    #[test]
    fn parses_valid_mapping_and_copies_server_metadata() {
        let chunk = build_source_chunks(&[segment("s1", "A", 0, "嗯，先开始")]).remove(0);
        let items = parse_organized_chunk_response(
            r#"{"items":[{"groupId":"g1","text":"先开始。"}]}"#,
            &chunk,
        )
        .unwrap();
        assert_eq!(items[0].speaker_id.as_deref(), Some("A"));
        assert_eq!(items[0].start_ms, 0);
        assert_eq!(items[0].text, "先开始。");
    }

    #[test]
    fn rejects_changed_group_mapping() {
        let chunk = build_source_chunks(&[segment("s1", "A", 0, "开始")]).remove(0);
        let error = parse_organized_chunk_response(
            r#"{"items":[{"groupId":"g2","text":"开始。"}]}"#,
            &chunk,
        )
        .unwrap_err();
        assert!(error.contains("group ids changed"));
    }

    #[test]
    fn rejects_invalid_json_and_empty_text() {
        let chunk = build_source_chunks(&[segment("s1", "A", 0, "开始")]).remove(0);
        assert!(parse_organized_chunk_response("not json", &chunk)
            .unwrap_err()
            .starts_with("invalid organized draft json"));
        assert!(parse_organized_chunk_response(
            r#"{"items":[{"groupId":"g1","text":""}]}"#,
            &chunk,
        )
        .unwrap_err()
        .contains("empty text"));
        assert!(
            parse_organized_chunk_response(r#"{"items":[{"groupId":"g1"}]}"#, &chunk,)
                .unwrap_err()
                .contains("missing field `text`")
        );
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
    fn each_group_maps_to_exactly_one_source_segment() {
        let segments = (0..65)
            .map(|index| segment(&format!("s{index}"), "A", index * 500, "short"))
            .collect::<Vec<_>>();
        let chunks = build_source_chunks(&segments);
        let groups = chunks
            .iter()
            .flat_map(|chunk| chunk.groups.iter())
            .collect::<Vec<_>>();

        assert_eq!(groups.len(), segments.len());
        assert!(groups
            .iter()
            .all(|group| group.source_segment_ids.len() == 1));
    }

    #[test]
    fn long_same_speaker_meeting_does_not_echo_uuid_segment_ids() {
        let segments = (1..=604)
            .map(|index| {
                segment(
                    &format!("post-2ec5e526-c0bc-4cc5-8e87-accb94b6dbe6-{index}"),
                    "未区分",
                    index * 500,
                    "这是一段用于验证长会议分组边界的转写文本。",
                )
            })
            .collect::<Vec<_>>();
        let chunks = build_source_chunks(&segments);

        assert!(chunks.len() >= 2);
        assert_eq!(
            chunks.iter().map(|chunk| chunk.groups.len()).sum::<usize>(),
            segments.len()
        );
        assert!(chunks.iter().all(|chunk| {
            chunk.groups.len() <= MEETING_ORGANIZED_CHUNK_MAX_GROUPS
                && chunk
                    .groups
                    .iter()
                    .all(|group| group.source_segment_ids.len() == 1)
        }));
        for (index, chunk) in chunks.iter().enumerate() {
            let (_, user) = build_organized_draft_prompt(chunk, index + 1, chunks.len(), false);
            assert!(!user.contains("post-2ec5e526"));
        }
    }

    #[test]
    fn caps_chunk_group_count() {
        let segments = (0..49)
            .map(|index| {
                segment(
                    &format!("s{index}"),
                    if index % 2 == 0 { "A" } else { "B" },
                    index * 500,
                    "short",
                )
            })
            .collect::<Vec<_>>();
        let chunks = build_source_chunks(&segments);

        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].groups.len(), MEETING_ORGANIZED_CHUNK_MAX_GROUPS);
        assert_eq!(chunks[1].groups.len(), 1);
    }

    #[test]
    fn prompt_uses_short_group_ids_without_source_segment_ids() {
        let chunk = build_source_chunks(&[
            segment("long-source-segment-id-1", "A", 0, "first"),
            segment("long-source-segment-id-2", "B", 500, "second"),
        ])
        .remove(0);
        let (_, user) = build_organized_draft_prompt(&chunk, 1, 1, false);

        assert!(user.contains("groupId=g1"));
        assert!(user.contains("groupId=g2"));
        assert!(!user.contains("long-source-segment-id"));
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
                    Ok(r#"{"items":[{"groupId":"g1","text":"first"}]}"#.to_string())
                } else {
                    Err("second chunk failed".to_string())
                }
            }) as Pin<Box<dyn Future<Output = Result<String, String>> + Send>>
        };

        let result = organize_record(&record, &UserPreferences::default(), &llm).await;

        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(result, Err("second chunk failed".to_string()));
    }

    #[tokio::test]
    async fn retries_one_invalid_format_response() {
        let record = record_with_segments(vec![segment("s1", "A", 0, "first")]);
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_for_llm = Arc::clone(&calls);
        let llm = move |_system: String, user: String, _prefs: UserPreferences| {
            let call = calls_for_llm.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move {
                if call == 0 {
                    Ok(r#"{"items":[]}"#.to_string())
                } else {
                    assert!(user.contains("上一次响应未通过格式校验"));
                    Ok(r#"{"items":[{"groupId":"g1","text":"first"}]}"#.to_string())
                }
            }) as Pin<Box<dyn Future<Output = Result<String, String>> + Send>>
        };

        let result = organize_record(&record, &UserPreferences::default(), &llm)
            .await
            .unwrap();

        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(result[0].source_segment_ids, ["s1"]);
        assert_eq!(result[0].text, "first");
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

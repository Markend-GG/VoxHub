use std::collections::HashSet;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::Deserialize;
use tauri::Emitter;

use crate::persistence::{MeetingStore, PreferencesStore};
use crate::types::{
    MeetingErrorEvent, MeetingImportStatus, MeetingRecord, MeetingStatus, MeetingSummary,
    MeetingSummaryEvent, MeetingTodo, OutputLanguagePreference, UserPreferences,
};

use super::{complete_text_with_active_llm, Inner};

pub(super) const MEETING_SUMMARY_CHUNK_TARGET_CHARS: usize = 12_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct MeetingSummaryPrompt {
    pub system: String,
    pub user: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ParsedMeetingSummary {
    pub title: String,
    pub summary: MeetingSummary,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum MeetingSummaryMode {
    Generate,
    Retry,
}

type MeetingSummaryLlm = dyn Fn(
        String,
        String,
        UserPreferences,
    ) -> Pin<Box<dyn Future<Output = Result<String, String>> + Send>>
    + Send
    + Sync;

pub(super) async fn generate_meeting_summary(
    inner: &Arc<Inner>,
    meeting_id: &str,
    mode: MeetingSummaryMode,
) -> Result<MeetingRecord, String> {
    generate_meeting_summary_with_llm(inner, meeting_id, mode, &call_active_llm).await
}

pub(super) fn prepare_and_spawn_auto_meeting_summary(
    inner: &Arc<Inner>,
    record: &mut MeetingRecord,
) -> Result<(), String> {
    validate_summary_mode(record, MeetingSummaryMode::Generate)?;
    let store = MeetingStore::new().map_err(|e| e.to_string())?;
    let mut prepared = record.clone();

    if let Err(error) = prepare_summary_record(&mut prepared) {
        complete_import_summary(&mut prepared, Some(&error));
        persist_summary_record(&store, &prepared)?;
        apply_import_summary_retention(inner, &store, &mut prepared)?;
        *record = prepared;
        emit_meeting_summary_failed(inner, record, "emptyTranscript", &error);
        super::meeting_audio_import::emit_import_event(inner, record);
        return Ok(());
    }

    persist_summary_record(&store, &prepared)?;
    *record = prepared;
    emit_meeting_summary(inner, record, None);
    spawn_prepared_meeting_summary(inner, record.id.clone());
    Ok(())
}

fn spawn_prepared_meeting_summary(inner: &Arc<Inner>, meeting_id: String) {
    let inner = Arc::clone(inner);
    tauri::async_runtime::spawn(async move {
        if let Err(error) =
            complete_prepared_meeting_summary(&inner, &meeting_id, &call_active_llm).await
        {
            log::warn!("[meeting-summary] auto summary failed: {error}");
        }
    });
}

async fn generate_meeting_summary_with_llm(
    inner: &Arc<Inner>,
    meeting_id: &str,
    mode: MeetingSummaryMode,
    llm: &MeetingSummaryLlm,
) -> Result<MeetingRecord, String> {
    let job = MeetingSummaryJob::acquire(inner, meeting_id)?;
    let result = run_meeting_summary_job(inner, meeting_id, mode, llm).await;
    drop(job);
    result
}

async fn run_meeting_summary_job(
    inner: &Arc<Inner>,
    meeting_id: &str,
    mode: MeetingSummaryMode,
    llm: &MeetingSummaryLlm,
) -> Result<MeetingRecord, String> {
    if has_active_recording_meeting(inner, meeting_id) {
        return Err("meeting recording is active".to_string());
    }

    let store = MeetingStore::new().map_err(|e| e.to_string())?;
    let mut record = store
        .get(meeting_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "meeting not found".to_string())?;

    validate_summary_mode(&record, mode)?;
    if let Err(error) = prepare_summary_record(&mut record) {
        persist_summary_record(&store, &record)?;
        emit_meeting_summary_failed(inner, &record, "emptyTranscript", &error);
        return Ok(record);
    }

    persist_summary_record(&store, &record)?;
    emit_meeting_summary(inner, &record, None);

    finish_summarizing_record(inner, &store, record, llm).await
}

async fn complete_prepared_meeting_summary(
    inner: &Arc<Inner>,
    meeting_id: &str,
    llm: &MeetingSummaryLlm,
) -> Result<MeetingRecord, String> {
    let job = MeetingSummaryJob::acquire(inner, meeting_id)?;
    let result = run_prepared_meeting_summary_job(inner, meeting_id, llm).await;
    drop(job);
    result
}

async fn run_prepared_meeting_summary_job(
    inner: &Arc<Inner>,
    meeting_id: &str,
    llm: &MeetingSummaryLlm,
) -> Result<MeetingRecord, String> {
    if has_active_recording_meeting(inner, meeting_id) {
        return Err("meeting recording is active".to_string());
    }

    let store = MeetingStore::new().map_err(|e| e.to_string())?;
    let record = store
        .get(meeting_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "meeting not found".to_string())?;
    if record.status != MeetingStatus::Summarizing {
        return Err("meeting summary is not prepared".to_string());
    }

    finish_summarizing_record(inner, &store, record, llm).await
}

async fn finish_summarizing_record(
    inner: &Arc<Inner>,
    store: &MeetingStore,
    mut record: MeetingRecord,
    llm: &MeetingSummaryLlm,
) -> Result<MeetingRecord, String> {
    let prefs = PreferencesStore::new()
        .unwrap_or_else(|_| PreferencesStore::new_fallback())
        .get();

    let llm_result = summarize_record(&record, &prefs, llm).await;
    match llm_result {
        Ok(parsed) => {
            apply_parsed_summary(&mut record, parsed);
            complete_import_summary(&mut record, None);
            persist_summary_record(&store, &record)?;
            apply_import_summary_retention(inner, store, &mut record)?;
            emit_meeting_summary(inner, &record, None);
            super::meeting_audio_import::emit_import_event(inner, &record);
            Ok(record)
        }
        Err(error) => {
            record.status = MeetingStatus::SummaryFailed;
            record.updated_at = Utc::now().to_rfc3339();
            complete_import_summary(&mut record, Some(&error));
            persist_summary_record(&store, &record)?;
            apply_import_summary_retention(inner, store, &mut record)?;
            emit_meeting_summary_failed(inner, &record, summary_error_code(&error), &error);
            super::meeting_audio_import::emit_import_event(inner, &record);
            Ok(record)
        }
    }
}

fn complete_import_summary(record: &mut MeetingRecord, error: Option<&str>) {
    let Some(import_state) = record.import_state.as_mut() else {
        return;
    };
    let now = Utc::now().to_rfc3339();
    import_state.progress = Some(1.0);
    import_state.updated_at = now.clone();
    import_state.completed_at = Some(now.clone());
    if let Some(error) = error {
        import_state.status = MeetingImportStatus::Failed;
        import_state.error_code = Some(summary_error_code(error).to_string());
        import_state.error_message = Some(error.to_string());
    } else {
        import_state.status = MeetingImportStatus::Completed;
        import_state.error_code = None;
        import_state.error_message = None;
    }
    record.processing_hold = None;
    record.updated_at = now;
}

fn apply_import_summary_retention(
    inner: &Arc<Inner>,
    store: &MeetingStore,
    record: &mut MeetingRecord,
) -> Result<(), String> {
    if record.import_state.is_none() {
        return Ok(());
    }
    store
        .prune_audio_retention(inner.prefs.get().meeting_audio_retention_count)
        .map_err(|error| error.to_string())?;
    if let Some(updated) = store.get(&record.id).map_err(|error| error.to_string())? {
        *record = updated;
    }
    Ok(())
}

fn summary_error_code(error: &str) -> &'static str {
    if error.starts_with("invalid summary json") {
        "summaryInvalidJson"
    } else {
        "summaryLlmFailed"
    }
}

async fn summarize_record(
    record: &MeetingRecord,
    prefs: &UserPreferences,
    llm: &MeetingSummaryLlm,
) -> Result<ParsedMeetingSummary, String> {
    let chunks = transcript_chunks(record);
    let valid_segment_ids = record
        .transcript_segments
        .iter()
        .map(|segment| segment.id.clone())
        .collect::<Vec<_>>();
    let output_language = output_language_label(prefs.output_language_preference);
    if chunks.len() <= 1 {
        let prompt =
            build_meeting_summary_prompt(record, &prefs.working_languages, output_language);
        let response = llm(prompt.system, prompt.user, prefs.clone()).await?;
        return parse_meeting_summary_response(&response, &valid_segment_ids);
    }

    let mut notes = String::new();
    for (index, chunk) in chunks.iter().enumerate() {
        let prompt = build_meeting_summary_chunk_prompt(
            record,
            &notes,
            chunk,
            index + 1,
            chunks.len(),
            output_language,
        );
        notes = llm(prompt.system, prompt.user, prefs.clone()).await?;
    }
    let prompt = build_meeting_summary_final_prompt(record, &notes, output_language);
    let response = llm(prompt.system, prompt.user, prefs.clone()).await?;
    parse_meeting_summary_response(&response, &valid_segment_ids)
}

fn validate_summary_mode(record: &MeetingRecord, mode: MeetingSummaryMode) -> Result<(), String> {
    if record.post_processing.as_ref().is_some_and(|state| {
        !matches!(
            state.status,
            crate::types::MeetingPostProcessingStatus::Completed
                | crate::types::MeetingPostProcessingStatus::RealtimeAccepted
        )
    }) {
        return Err("meeting post-processing is not completed".to_string());
    }
    match (record.status.clone(), mode) {
        (
            MeetingStatus::Recording
            | MeetingStatus::Paused
            | MeetingStatus::TranscribingInterrupted,
            _,
        ) => Err("meeting recording is active".to_string()),
        (MeetingStatus::Summarizing, _) => Err("meeting summary already running".to_string()),
        (MeetingStatus::Completed, MeetingSummaryMode::Generate) => Ok(()),
        (MeetingStatus::SummaryFailed, MeetingSummaryMode::Retry) => Ok(()),
        (MeetingStatus::SummaryFailed, MeetingSummaryMode::Generate) => {
            Err("meeting summary retry required".to_string())
        }
        (MeetingStatus::Completed, MeetingSummaryMode::Retry) => {
            Err("meeting summary has not failed".to_string())
        }
        (MeetingStatus::Draft, _) => Err("meeting is not completed".to_string()),
    }
}

pub(super) fn prepare_summary_record(record: &mut MeetingRecord) -> Result<(), String> {
    if record
        .transcript_segments
        .iter()
        .all(|segment| segment.text.trim().is_empty())
    {
        record.status = MeetingStatus::SummaryFailed;
        record.updated_at = Utc::now().to_rfc3339();
        return Err("meeting transcript is empty".to_string());
    }
    record.status = MeetingStatus::Summarizing;
    record.updated_at = Utc::now().to_rfc3339();
    Ok(())
}

pub(super) fn build_meeting_summary_prompt(
    record: &MeetingRecord,
    _working_languages: &[String],
    output_language: &str,
) -> MeetingSummaryPrompt {
    MeetingSummaryPrompt {
        system: build_summary_system_prompt(output_language),
        user: format!(
            "请根据下面的会议原文生成会议纪要。只输出一个 JSON object，不要输出 Markdown。\n\n\
             会议元信息：\n\
             - startedAt: {}\n\
             - endedAt: {}\n\
             - durationMs: {}\n\n\
             会议原文：\n{}\n\n\
             输出 JSON schema：\n{}\n\n\
             约束：\n\
             - title 必须简短。\n\
             - todos 中没有负责人或截止日期时填 null。\n\
             - sourceSegmentIds 只能使用输入中出现的 segment id。\n\
             - 不要编造原文没有的信息。",
            record.started_at,
            record.ended_at.as_deref().unwrap_or(""),
            record
                .duration_ms
                .map(|value| value.to_string())
                .unwrap_or_default(),
            format_transcript_segments(record),
            summary_json_schema(),
        ),
    }
}

fn build_meeting_summary_chunk_prompt(
    record: &MeetingRecord,
    previous_notes: &str,
    chunk: &str,
    index: usize,
    total: usize,
    output_language: &str,
) -> MeetingSummaryPrompt {
    MeetingSummaryPrompt {
        system: build_summary_notes_system_prompt(output_language),
        user: format!(
            "这是一场长会议的第 {index}/{total} 个 transcript chunk（原文分块）。\n\
             请基于 previous notes（前文累计笔记）和当前 chunk 更新累计笔记。\n\
             只输出纯文本 notes，不要输出最终 JSON。\n\n\
             meetingId: {}\n\n\
             previous notes:\n{}\n\n\
             current chunk:\n{}",
            record.id, previous_notes, chunk
        ),
    }
}

fn build_summary_notes_system_prompt(output_language: &str) -> String {
    format!(
        "你是 OpenLess 的会议总结助手。请输出纯文本 rolling context notes（滚动上下文笔记），不要输出 JSON、Markdown 或解释。\n\
         不要编造原文没有的信息。输出语言偏好：{output_language}。"
    )
}

fn build_meeting_summary_final_prompt(
    record: &MeetingRecord,
    notes: &str,
    output_language: &str,
) -> MeetingSummaryPrompt {
    MeetingSummaryPrompt {
        system: build_summary_system_prompt(output_language),
        user: format!(
            "请根据下面的 rolling context notes（滚动上下文笔记）生成最终会议纪要。\n\
             只输出一个 JSON object，不要输出 Markdown。\n\n\
             meetingId: {}\n\n\
             rolling context notes:\n{}\n\n\
             输出 JSON schema：\n{}",
            record.id,
            notes,
            summary_json_schema()
        ),
    }
}

fn build_summary_system_prompt(output_language: &str) -> String {
    format!(
        "你是 OpenLess 的会议总结助手。只输出 JSON，不输出 Markdown 或解释。\
         不要编造原文没有的信息。输出语言偏好：{output_language}。"
    )
}

fn output_language_label(preference: OutputLanguagePreference) -> &'static str {
    match preference {
        OutputLanguagePreference::Auto => "auto",
        OutputLanguagePreference::ZhCn => "Simplified Chinese",
        OutputLanguagePreference::ZhTw => "Traditional Chinese",
        OutputLanguagePreference::En => "English",
        OutputLanguagePreference::Ja => "Japanese",
        OutputLanguagePreference::Ko => "Korean",
    }
}

fn summary_json_schema() -> &'static str {
    r#"{
  "title": "string",
  "overview": "string",
  "keyDecisions": ["string"],
  "todos": [
    {
      "content": "string",
      "owner": null,
      "dueDate": null,
      "sourceSegmentIds": ["seg-000001"],
      "sourceQuote": "string"
    }
  ],
  "risksAndOpenQuestions": ["string"]
}"#
}

fn format_transcript_segments(record: &MeetingRecord) -> String {
    record
        .transcript_segments
        .iter()
        .filter(|segment| !segment.text.trim().is_empty())
        .map(|segment| {
            format!(
                "[{}][{}][{}] {}",
                segment.id,
                record.speaker_display_name(segment),
                format_segment_timestamp(segment.start_ms),
                segment.text.trim()
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn format_segment_timestamp(ms: u64) -> String {
    let total_secs = ms / 1000;
    let hours = total_secs / 3600;
    let minutes = (total_secs % 3600) / 60;
    let seconds = total_secs % 60;
    format!("{hours:02}:{minutes:02}:{seconds:02}")
}

pub(super) fn transcript_chunks(record: &MeetingRecord) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut current = String::new();
    for segment in &record.transcript_segments {
        if segment.text.trim().is_empty() {
            continue;
        }
        let line = format!(
            "[{}][{}][{}] {}\n",
            segment.id,
            record.speaker_display_name(segment),
            format_segment_timestamp(segment.start_ms),
            segment.text.trim()
        );
        if !current.is_empty()
            && current.chars().count() + line.chars().count() > MEETING_SUMMARY_CHUNK_TARGET_CHARS
        {
            chunks.push(current);
            current = String::new();
        }
        current.push_str(&line);
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

pub(super) fn parse_meeting_summary_response(
    raw: &str,
    valid_segment_ids: &[String],
) -> Result<ParsedMeetingSummary, String> {
    let cleaned = strip_json_code_fence(raw);
    let response: MeetingSummaryResponse =
        serde_json::from_str(&cleaned).map_err(|e| format!("invalid summary json: {e}"))?;
    let title = response.title.trim().to_string();
    let overview = response.overview.trim().to_string();
    if title.is_empty() || overview.is_empty() {
        return Err("invalid summary json: title and overview are required".to_string());
    }
    let valid_ids: HashSet<&str> = valid_segment_ids.iter().map(String::as_str).collect();
    let todos = response
        .todos
        .into_iter()
        .filter_map(|todo| {
            let content = todo.content.trim().to_string();
            if content.is_empty() {
                return None;
            }
            let source_segment_ids = todo
                .source_segment_ids
                .into_iter()
                .filter(|id| valid_ids.contains(id.as_str()))
                .collect::<Vec<_>>();
            Some(MeetingTodo {
                id: uuid::Uuid::new_v4().to_string(),
                content,
                owner: normalize_optional(todo.owner),
                due_date: normalize_optional(todo.due_date),
                source_segment_ids,
                source_quote: normalize_optional(todo.source_quote).map(|quote| {
                    const LIMIT: usize = 240;
                    quote.chars().take(LIMIT).collect()
                }),
            })
        })
        .collect::<Vec<_>>();

    Ok(ParsedMeetingSummary {
        title,
        summary: MeetingSummary {
            overview,
            key_decisions: normalize_string_list(response.key_decisions),
            todos,
            risks_and_open_questions: normalize_string_list(response.risks_and_open_questions),
        },
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

fn normalize_optional(value: Option<String>) -> Option<String> {
    value
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn normalize_string_list(values: Vec<String>) -> Vec<String> {
    values
        .into_iter()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .collect()
}

pub(super) fn is_default_meeting_title(title: &str, started_at: &str) -> bool {
    parse_datetime(started_at)
        .map(|started| title == started.format("会议记录 %Y-%m-%d %H:%M").to_string())
        .unwrap_or(false)
}

fn parse_datetime(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .map(|dt| dt.with_timezone(&Utc))
        .ok()
}

fn apply_parsed_summary(record: &mut MeetingRecord, parsed: ParsedMeetingSummary) {
    if is_default_meeting_title(&record.title, &record.started_at) {
        record.title = parsed.title;
    }
    record.summary = parsed.summary;
    record.status = MeetingStatus::Completed;
    record.updated_at = Utc::now().to_rfc3339();
}

fn persist_summary_record(store: &MeetingStore, record: &MeetingRecord) -> Result<(), String> {
    store
        .update(record.clone())
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "meeting not found".to_string())?;
    Ok(())
}

fn emit_meeting_summary(
    inner: &Arc<Inner>,
    record: &MeetingRecord,
    error: Option<MeetingErrorEvent>,
) {
    if let Some(app) = inner.app.lock().clone() {
        let _ = app.emit(
            "meeting:summary",
            MeetingSummaryEvent {
                meeting_id: record.id.clone(),
                status: record.status.clone(),
                meeting: Some(record.clone()),
                error,
            },
        );
        #[cfg(not(mobile))]
        match record.status {
            MeetingStatus::Completed => {
                crate::meeting_companion::schedule_completed_fallback_dismissal(&app, &record.id);
            }
            MeetingStatus::SummaryFailed => {
                crate::meeting_companion::schedule_failed_dismissal(&app, &record.id);
            }
            _ => {}
        }
    }
}

fn emit_meeting_summary_failed(
    inner: &Arc<Inner>,
    record: &MeetingRecord,
    code: &str,
    message: &str,
) {
    let error = MeetingErrorEvent {
        meeting_id: Some(record.id.clone()),
        code: code.to_string(),
        message: message.to_string(),
    };
    emit_meeting_summary(inner, record, Some(error.clone()));
    if let Some(app) = inner.app.lock().clone() {
        let _ = app.emit("meeting:error", error);
    }
}

fn has_active_recording_meeting(inner: &Arc<Inner>, meeting_id: &str) -> bool {
    inner
        .meeting_session
        .lock()
        .as_ref()
        .map(|session| session.record().id == meeting_id)
        .unwrap_or(false)
}

fn call_active_llm(
    system_prompt: String,
    user_prompt: String,
    prefs: UserPreferences,
) -> Pin<Box<dyn Future<Output = Result<String, String>> + Send>> {
    Box::pin(async move {
        complete_text_with_active_llm(
            &system_prompt,
            &user_prompt,
            &prefs.working_languages,
            prefs.chinese_script_preference,
            prefs.output_language_preference,
            prefs.llm_thinking_enabled,
        )
        .await
        .map_err(|e| e.to_string())
    })
}

struct MeetingSummaryJob<'a> {
    inner: &'a Arc<Inner>,
    meeting_id: String,
}

impl<'a> MeetingSummaryJob<'a> {
    fn acquire(inner: &'a Arc<Inner>, meeting_id: &str) -> Result<Self, String> {
        let mut jobs = inner.meeting_summary_jobs.lock();
        if !jobs.insert(meeting_id.to_string()) {
            return Err("meeting summary already running".to_string());
        }
        Ok(Self {
            inner,
            meeting_id: meeting_id.to_string(),
        })
    }
}

impl Drop for MeetingSummaryJob<'_> {
    fn drop(&mut self) {
        self.inner
            .meeting_summary_jobs
            .lock()
            .remove(&self.meeting_id);
    }
}

#[derive(Debug, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct MeetingSummaryResponse {
    title: String,
    overview: String,
    key_decisions: Vec<String>,
    todos: Vec<MeetingTodoResponse>,
    risks_and_open_questions: Vec<String>,
}

impl Default for MeetingSummaryResponse {
    fn default() -> Self {
        Self {
            title: String::new(),
            overview: String::new(),
            key_decisions: Vec::new(),
            todos: Vec::new(),
            risks_and_open_questions: Vec::new(),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct MeetingTodoResponse {
    content: String,
    owner: Option<String>,
    due_date: Option<String>,
    source_segment_ids: Vec<String>,
    source_quote: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{
        MeetingAudioMeta, MeetingAudioState, MeetingRecord, TranscriptSegment,
        TranscriptSegmentSource,
    };

    fn record_with_segments(segments: Vec<TranscriptSegment>) -> MeetingRecord {
        MeetingRecord {
            id: "550e8400-e29b-41d4-a716-446655440000".to_string(),
            title: "会议记录 2026-07-04 09:30".to_string(),
            status: MeetingStatus::Completed,
            started_at: "2026-07-04T09:30:00+00:00".to_string(),
            ended_at: Some("2026-07-04T10:00:00+00:00".to_string()),
            duration_ms: Some(1_800_000),
            transcript_segments: segments,
            summary: MeetingSummary::default(),
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
            active_transcript_revision: None,
            speaker_profiles: Vec::new(),
            speaker_turns: Vec::new(),
            processing_hold: None,
            created_at: "2026-07-04T09:30:00+00:00".to_string(),
            updated_at: "2026-07-04T10:00:00+00:00".to_string(),
        }
    }

    fn segment(id: &str, text: &str) -> TranscriptSegment {
        TranscriptSegment {
            id: id.to_string(),
            speaker_id: None,
            speaker_label: "未区分".to_string(),
            start_ms: 12_000,
            end_ms: Some(18_000),
            text: text.to_string(),
            source: TranscriptSegmentSource::RealtimeAsr,
            metadata: None,
        }
    }

    #[test]
    fn meeting_summary_builds_prompt_with_segment_ids() {
        let record = record_with_segments(vec![segment("seg-000001", "确认 V1-4 做总结生成。")]);

        let prompt = build_meeting_summary_prompt(&record, &[], "auto");

        assert!(prompt.system.contains("只输出 JSON"));
        assert!(prompt.user.contains("[seg-000001][未区分][00:00:12]"));
        assert!(prompt.user.contains("确认 V1-4 做总结生成。"));
    }

    #[test]
    fn meeting_summary_uses_renamed_speaker_profile_in_full_prompt() {
        let mut renamed_segment = segment("seg-000001", "确认下一步");
        renamed_segment.speaker_id = Some("speaker-0".to_string());
        renamed_segment.speaker_label = "发言人 1".to_string();
        let mut record = record_with_segments(vec![renamed_segment]);
        record.speaker_profiles = vec![crate::types::SpeakerProfile {
            id: "speaker-0".to_string(),
            provider_speaker_id: Some("0".to_string()),
            display_name: "张三".to_string(),
            manually_named: true,
        }];

        let prompt = build_meeting_summary_prompt(&record, &[], "auto");

        assert!(prompt.user.contains("[seg-000001][张三][00:00:12]"));
        assert!(!prompt.user.contains("[seg-000001][发言人 1]"));
    }

    #[test]
    fn meeting_summary_formats_segment_timestamp_as_hh_mm_ss() {
        assert_eq!(format_segment_timestamp(3_723_000), "01:02:03");
    }

    #[test]
    fn meeting_summary_parse_json_response() {
        let parsed = parse_meeting_summary_response(
            r#"{
              "title": "V1-4 会议总结",
              "overview": "讨论 V1-4 总结生成。",
              "keyDecisions": ["停止后自动总结"],
              "todos": [{
                "content": "实现 retry",
                "owner": null,
                "dueDate": null,
                "sourceSegmentIds": ["seg-000001"],
                "sourceQuote": "失败后只支持 retry"
              }],
              "risksAndOpenQuestions": ["长原文需要 rolling context"]
            }"#,
            &["seg-000001".to_string()],
        )
        .expect("parse summary");

        assert_eq!(parsed.title, "V1-4 会议总结");
        assert_eq!(parsed.summary.overview, "讨论 V1-4 总结生成。");
        assert_eq!(parsed.summary.key_decisions, vec!["停止后自动总结"]);
        assert_eq!(
            parsed.summary.todos[0].source_segment_ids,
            vec!["seg-000001"]
        );
        assert_eq!(
            parsed.summary.risks_and_open_questions,
            vec!["长原文需要 rolling context"]
        );
    }

    #[test]
    fn meeting_summary_parse_code_fenced_json() {
        let parsed = parse_meeting_summary_response(
            "```json\n{\"title\":\"标题\",\"overview\":\"概览\"}\n```",
            &[],
        )
        .expect("parse fenced json");

        assert_eq!(parsed.title, "标题");
        assert_eq!(parsed.summary.overview, "概览");
        assert!(parsed.summary.key_decisions.is_empty());
    }

    #[test]
    fn meeting_summary_rejects_invalid_json() {
        let error = parse_meeting_summary_response("not json", &[]).unwrap_err();

        assert!(error.contains("invalid summary json"));
    }

    #[test]
    fn meeting_summary_invalid_json_uses_parse_error_code() {
        assert_eq!(
            summary_error_code("invalid summary json: expected value"),
            "summaryInvalidJson"
        );
        assert_eq!(summary_error_code("network failed"), "summaryLlmFailed");
    }

    #[test]
    fn meeting_summary_chunk_prompt_uses_notes_system_prompt() {
        let record = record_with_segments(vec![segment("seg-000001", "内容")]);

        let prompt =
            build_meeting_summary_chunk_prompt(&record, "previous notes", "chunk", 1, 2, "auto");

        assert!(prompt.system.contains("rolling context notes"));
        assert!(!prompt.system.contains("只输出 JSON"));
        assert!(prompt.user.contains("不要输出最终 JSON"));
    }

    #[test]
    fn meeting_summary_filters_unknown_source_segment_ids() {
        let parsed = parse_meeting_summary_response(
            r#"{
              "title": "标题",
              "overview": "概览",
              "todos": [{
                "content": "实现测试",
                "sourceSegmentIds": ["seg-000001", "seg-missing"]
              }]
            }"#,
            &["seg-000001".to_string()],
        )
        .expect("parse summary");

        assert_eq!(
            parsed.summary.todos[0].source_segment_ids,
            vec!["seg-000001"]
        );
    }

    #[test]
    fn meeting_summary_default_title_can_be_replaced() {
        assert!(is_default_meeting_title(
            "会议记录 2026-07-04 09:30",
            "2026-07-04T09:30:00+00:00"
        ));
    }

    #[test]
    fn meeting_summary_custom_title_is_preserved() {
        assert!(!is_default_meeting_title(
            "产品周会",
            "2026-07-04T09:30:00+00:00"
        ));
    }

    #[test]
    fn meeting_summary_empty_transcript_fails_without_llm() {
        let mut record = record_with_segments(Vec::new());

        let outcome = prepare_summary_record(&mut record);

        assert_eq!(outcome, Err("meeting transcript is empty".to_string()));
        assert_eq!(record.status, MeetingStatus::SummaryFailed);
        assert!(record.transcript_segments.is_empty());
    }

    #[test]
    fn meeting_summary_generate_allows_existing_summary_rewrite_and_retry_only_allows_failed() {
        let mut completed = record_with_segments(vec![segment("seg-000001", "内容")]);
        completed.summary.overview = "已有总结".to_string();
        let mut failed = completed.clone();
        failed.status = MeetingStatus::SummaryFailed;

        assert_eq!(
            validate_summary_mode(&completed, MeetingSummaryMode::Generate),
            Ok(())
        );
        assert_eq!(
            validate_summary_mode(&completed, MeetingSummaryMode::Retry),
            Err("meeting summary has not failed".to_string())
        );
        assert_eq!(
            validate_summary_mode(&failed, MeetingSummaryMode::Generate),
            Err("meeting summary retry required".to_string())
        );
        assert_eq!(
            validate_summary_mode(&failed, MeetingSummaryMode::Retry),
            Ok(())
        );
    }

    #[test]
    fn meeting_summary_waits_for_post_processing_or_explicit_realtime_acceptance() {
        let mut record = record_with_segments(vec![segment("seg-000001", "内容")]);
        let now = "2026-08-12T10:00:00Z".to_string();
        record.post_processing = Some(crate::types::MeetingPostProcessingState {
            status: crate::types::MeetingPostProcessingStatus::Failed,
            job_id: "job-1".to_string(),
            model_ref: crate::types::MeetingAsrModelRef {
                provider_id: "bailian".to_string(),
                model_id: "fun-asr".to_string(),
            },
            resolved_runtime_kind: crate::types::MeetingAsrRuntimeKind::Cloud,
            diarization_mode: crate::types::MeetingDiarizationMode::Off,
            expected_speaker_count: None,
            processing_revision: 1,
            provider_task_id: None,
            progress: None,
            attempt: 1,
            error_code: Some("network".to_string()),
            error_message: Some("网络失败".to_string()),
            created_at: now.clone(),
            updated_at: now,
            started_at: None,
            completed_at: None,
        });

        assert_eq!(
            validate_summary_mode(&record, MeetingSummaryMode::Generate),
            Err("meeting post-processing is not completed".to_string())
        );

        record.post_processing.as_mut().unwrap().status =
            crate::types::MeetingPostProcessingStatus::RealtimeAccepted;
        assert_eq!(
            validate_summary_mode(&record, MeetingSummaryMode::Generate),
            Ok(())
        );
    }

    #[test]
    fn meeting_summary_long_transcript_uses_rolling_context() {
        let record = record_with_segments(vec![
            segment(
                "seg-000001",
                &"一".repeat(MEETING_SUMMARY_CHUNK_TARGET_CHARS),
            ),
            segment(
                "seg-000002",
                &"二".repeat(MEETING_SUMMARY_CHUNK_TARGET_CHARS),
            ),
        ]);

        let chunks = transcript_chunks(&record);

        assert!(chunks.len() > 1);
        assert!(chunks[0].contains("seg-000001"));
        assert!(chunks[1].contains("seg-000002"));
    }

    #[test]
    fn meeting_summary_uses_renamed_speaker_profile_in_rolling_chunks() {
        let mut renamed_segment = segment("seg-000001", "确认下一步");
        renamed_segment.speaker_id = Some("speaker-0".to_string());
        renamed_segment.speaker_label = "发言人 1".to_string();
        let mut record = record_with_segments(vec![renamed_segment]);
        record.speaker_profiles = vec![crate::types::SpeakerProfile {
            id: "speaker-0".to_string(),
            provider_speaker_id: Some("0".to_string()),
            display_name: "张三".to_string(),
            manually_named: true,
        }];

        let chunks = transcript_chunks(&record);

        assert_eq!(chunks.len(), 1);
        assert!(chunks[0].contains("[seg-000001][张三][00:00:12]"));
        assert!(!chunks[0].contains("[seg-000001][发言人 1]"));
    }

    #[test]
    fn meeting_summary_failure_preserves_transcript() {
        let mut record = record_with_segments(vec![segment("seg-000001", "原文保留")]);

        record.status = MeetingStatus::SummaryFailed;

        assert_eq!(record.transcript_segments[0].text, "原文保留");
    }

    #[test]
    fn meeting_summary_applies_title_only_when_default() {
        let parsed = ParsedMeetingSummary {
            title: "AI 生成标题".to_string(),
            summary: MeetingSummary {
                overview: "概览".to_string(),
                key_decisions: vec!["决定".to_string()],
                todos: Vec::new(),
                risks_and_open_questions: Vec::new(),
            },
        };
        let mut default_title = record_with_segments(vec![segment("seg-000001", "内容")]);
        let mut custom_title = MeetingRecord {
            title: "产品周会".to_string(),
            ..default_title.clone()
        };

        apply_parsed_summary(&mut default_title, parsed.clone());
        apply_parsed_summary(&mut custom_title, parsed);

        assert_eq!(default_title.title, "AI 生成标题");
        assert_eq!(custom_title.title, "产品周会");
        assert_eq!(default_title.status, MeetingStatus::Completed);
        assert_eq!(custom_title.status, MeetingStatus::Completed);
    }
}

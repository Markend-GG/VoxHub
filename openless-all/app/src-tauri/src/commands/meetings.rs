use super::*;
use chrono::Utc;
use std::path::{Path, PathBuf};
use uuid::Uuid;

use crate::types::{MeetingAudioState, MeetingStatus, TranscriptSegment, TranscriptSegmentSource};

#[tauri::command]
pub fn list_meetings() -> Result<Vec<MeetingRecord>, String> {
    MeetingStore::new()
        .map_err(|e| e.to_string())?
        .list()
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn get_meeting(id: String) -> Result<MeetingRecord, String> {
    validate_meeting_id(&id)?;
    MeetingStore::new()
        .map_err(|e| e.to_string())?
        .get(&id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "meeting not found".to_string())
}

#[tauri::command]
pub fn create_meeting_record(record: MeetingRecord) -> Result<MeetingRecord, String> {
    validate_meeting_id(&record.id)?;
    let id = record.id.clone();
    let store = MeetingStore::new().map_err(|e| e.to_string())?;
    store.create(record).map_err(|e| e.to_string())?;
    prune_with_current_preference(&store)?;
    store
        .get(&id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "meeting not found".to_string())
}

#[tauri::command]
pub fn update_meeting_record(record: MeetingRecord) -> Result<MeetingRecord, String> {
    validate_meeting_id(&record.id)?;
    ensure_meeting_record_is_not_active(&record)?;
    let id = record.id.clone();
    let store = MeetingStore::new().map_err(|e| e.to_string())?;
    let existing = store.get(&id).map_err(|e| e.to_string())?;
    ensure_existing_meeting_can_be_updated(existing.as_ref())?;
    store
        .update(record)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "meeting not found".to_string())?;
    prune_with_current_preference(&store)?;
    store
        .get(&id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "meeting not found".to_string())
}

#[tauri::command]
pub fn delete_meeting_record(id: String) -> Result<(), String> {
    validate_meeting_id(&id)?;
    let store = MeetingStore::new().map_err(|e| e.to_string())?;
    delete_meeting_record_with_cleanup(
        &id,
        || store.get(&id).map_err(|e| e.to_string()),
        |delete_id| {
            store
                .delete(delete_id)
                .map_err(|e| e.to_string())
                .map(|_| ())
        },
        |delete_id| {
            let path = crate::persistence::meeting_recording_existing_path_for_id(delete_id)
                .map_err(|e| e.to_string())?;
            crate::persistence::remove_meeting_audio_path(&path).map_err(|e| e.to_string())
        },
    )
}

#[tauri::command]
pub async fn start_meeting_recording(
    coord: CoordinatorState<'_>,
) -> Result<MeetingRecordingSnapshot, String> {
    coord.start_meeting_recording().await
}

#[tauri::command]
pub async fn pause_meeting_recording(
    id: String,
    coord: CoordinatorState<'_>,
) -> Result<MeetingRecordingSnapshot, String> {
    validate_meeting_id(&id)?;
    coord.pause_meeting_recording(id).await
}

#[tauri::command]
pub async fn resume_meeting_recording(
    id: String,
    coord: CoordinatorState<'_>,
) -> Result<MeetingRecordingSnapshot, String> {
    validate_meeting_id(&id)?;
    coord.resume_meeting_recording(id).await
}

#[tauri::command]
pub async fn stop_meeting_recording(
    id: String,
    coord: CoordinatorState<'_>,
) -> Result<MeetingRecord, String> {
    validate_meeting_id(&id)?;
    coord.stop_meeting_recording(id).await
}

#[tauri::command]
pub fn get_active_meeting_recording(
    coord: CoordinatorState<'_>,
) -> Result<Option<MeetingRecordingSnapshot>, String> {
    coord.active_meeting_recording()
}

#[tauri::command]
pub async fn generate_meeting_summary(
    id: String,
    coord: CoordinatorState<'_>,
) -> Result<MeetingRecord, String> {
    validate_meeting_id(&id)?;
    coord.generate_meeting_summary(id).await
}

#[tauri::command]
pub async fn retry_meeting_summary(
    id: String,
    coord: CoordinatorState<'_>,
) -> Result<MeetingRecord, String> {
    validate_meeting_id(&id)?;
    coord.retry_meeting_summary(id).await
}

#[tauri::command]
pub fn export_meeting_markdown(id: String, target_path: String) -> Result<(), String> {
    validate_meeting_id(&id)?;
    let record = get_meeting(id)?;
    let markdown = meeting_markdown(&record);
    std::fs::write(Path::new(&target_path), markdown)
        .map_err(|e| format!("export meeting markdown failed: {e}"))
}

#[tauri::command]
pub async fn retranscribe_meeting(
    id: String,
    coord: CoordinatorState<'_>,
) -> Result<MeetingRecord, String> {
    validate_meeting_id(&id)?;
    let active_meeting = coord.active_meeting_recording()?;
    ensure_no_active_meeting_recording(active_meeting.as_ref().map(|snapshot| &snapshot.meeting))?;

    let store = MeetingStore::new().map_err(|e| e.to_string())?;
    let mut record = store
        .get(&id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "meeting not found".to_string())?;
    if record.status == MeetingStatus::Summarizing {
        return Err("meeting summary already running".into());
    }
    if record.audio.state != MeetingAudioState::Retained {
        return Err("meeting audio is not retained".into());
    }

    let path = crate::persistence::meeting_recording_existing_path_for_id(&id)
        .map_err(|e| e.to_string())?;
    if !path.exists() {
        return Err("meeting recording not found".into());
    }
    let pcm = read_meeting_audio_pcm(&path).await?;
    let text = coord.retranscribe_pcm(pcm).await?;
    if text.trim().is_empty() {
        return Err("meeting retranscribe returned empty transcript".into());
    }

    replace_transcript_with_retranscribed_text(&mut record, text);
    store
        .update(record.clone())
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "meeting not found".to_string())?;
    Ok(record)
}

fn prune_with_current_preference(store: &MeetingStore) -> Result<(), String> {
    let retention_count = PreferencesStore::new()
        .unwrap_or_else(|_| PreferencesStore::new_fallback())
        .get()
        .meeting_audio_retention_count;
    store
        .prune_audio_retention(retention_count)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

fn validate_meeting_id(id: &str) -> Result<(), String> {
    if is_valid_session_id(id) {
        Ok(())
    } else {
        Err("invalid meeting id".into())
    }
}

fn ensure_meeting_record_is_not_active(record: &MeetingRecord) -> Result<(), String> {
    if matches!(
        record.status,
        MeetingStatus::Recording | MeetingStatus::Paused | MeetingStatus::Summarizing
    ) {
        Err("meeting recording is active".to_string())
    } else {
        Ok(())
    }
}

fn ensure_existing_meeting_can_be_updated(record: Option<&MeetingRecord>) -> Result<(), String> {
    let Some(record) = record else {
        return Err("meeting not found".to_string());
    };
    ensure_meeting_record_is_not_active(record)
}

fn ensure_no_active_meeting_recording(active: Option<&MeetingRecord>) -> Result<(), String> {
    if active.is_some() {
        Err("meeting recording is active".to_string())
    } else {
        Ok(())
    }
}

fn delete_meeting_record_with_cleanup<G, D, R>(
    id: &str,
    get_record: G,
    delete_record: D,
    remove_audio: R,
) -> Result<(), String>
where
    G: FnOnce() -> Result<Option<MeetingRecord>, String>,
    D: FnOnce(&str) -> Result<(), String>,
    R: FnOnce(&str) -> Result<(), String>,
{
    let Some(record) = get_record()? else {
        return Ok(());
    };
    ensure_meeting_record_is_not_active(&record)?;
    remove_audio(id)?;
    delete_record(id)
}

fn meeting_markdown(record: &MeetingRecord) -> String {
    let mut out = String::new();
    out.push_str("# ");
    out.push_str(&record.title);
    out.push_str("\n\n");
    out.push_str("## Metadata\n\n");
    out.push_str(&format!("- Started: {}\n", record.started_at));
    out.push_str(&format!(
        "- Ended: {}\n",
        record.ended_at.as_deref().unwrap_or("-")
    ));
    out.push_str(&format!(
        "- Duration: {}\n",
        record
            .duration_ms
            .map(format_duration_hms)
            .unwrap_or_else(|| "-".to_string())
    ));
    out.push_str(&format!("- Status: {:?}\n\n", record.status));

    out.push_str("## Overview\n\n");
    push_optional_block(&mut out, &record.summary.overview);

    out.push_str("## Key Decisions\n\n");
    push_markdown_list(&mut out, &record.summary.key_decisions);

    out.push_str("## Todos\n\n");
    if record.summary.todos.is_empty() {
        out.push_str("- None\n\n");
    } else {
        for todo in &record.summary.todos {
            out.push_str("- ");
            out.push_str(&todo.content);
            if let Some(owner) = todo.owner.as_deref().filter(|value| !value.is_empty()) {
                out.push_str(&format!(" | Owner: {owner}"));
            }
            if let Some(due) = todo.due_date.as_deref().filter(|value| !value.is_empty()) {
                out.push_str(&format!(" | Due: {due}"));
            }
            if !todo.source_segment_ids.is_empty() {
                out.push_str(&format!(
                    " | Sources: {}",
                    todo.source_segment_ids.join(", ")
                ));
            }
            if let Some(quote) = todo
                .source_quote
                .as_deref()
                .filter(|value| !value.is_empty())
            {
                out.push_str(&format!(" | Quote: {quote}"));
            }
            out.push('\n');
        }
        out.push('\n');
    }

    out.push_str("## Risks And Open Questions\n\n");
    push_markdown_list(&mut out, &record.summary.risks_and_open_questions);

    out.push_str("## Transcript\n\n");
    if record.transcript_segments.is_empty() {
        out.push_str("_No transcript._\n");
    } else {
        for segment in &record.transcript_segments {
            out.push_str(&format!(
                "[{}][{}] {}\n\n",
                segment.speaker_label,
                format_duration_hms(segment.start_ms),
                segment.text.trim()
            ));
        }
    }
    out
}

fn push_optional_block(out: &mut String, value: &str) {
    if value.trim().is_empty() {
        out.push_str("_None_\n\n");
    } else {
        out.push_str(value.trim());
        out.push_str("\n\n");
    }
}

fn push_markdown_list(out: &mut String, values: &[String]) {
    let values = values
        .iter()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    if values.is_empty() {
        out.push_str("- None\n\n");
        return;
    }
    for value in values {
        out.push_str("- ");
        out.push_str(value);
        out.push('\n');
    }
    out.push('\n');
}

fn format_duration_hms(ms: u64) -> String {
    let total_seconds = ms / 1000;
    let hours = total_seconds / 3600;
    let minutes = (total_seconds % 3600) / 60;
    let seconds = total_seconds % 60;
    format!("{hours:02}:{minutes:02}:{seconds:02}")
}

async fn read_meeting_audio_pcm(path: &Path) -> Result<Vec<u8>, String> {
    if path.is_dir() {
        read_segmented_meeting_audio_pcm(path).await
    } else {
        let wav = tokio::fs::read(path).await.map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                "meeting recording not found".to_string()
            } else {
                format!("read meeting wav failed: {e}")
            }
        })?;
        pcm_from_wav_bytes(&wav)
    }
}

async fn read_segmented_meeting_audio_pcm(dir: &Path) -> Result<Vec<u8>, String> {
    let mut parts = meeting_audio_part_paths(dir)?;
    if parts.is_empty() {
        return Err("meeting recording not found".into());
    }
    parts.sort();
    let mut pcm = Vec::new();
    for part in parts {
        let wav = tokio::fs::read(&part)
            .await
            .map_err(|e| format!("read meeting wav failed: {e}"))?;
        pcm.extend(pcm_from_wav_bytes(&wav)?);
    }
    Ok(pcm)
}

fn meeting_audio_part_paths(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let entries =
        std::fs::read_dir(dir).map_err(|e| format!("read meeting audio dir failed: {e}"))?;
    Ok(entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .map(|name| name.starts_with("part-") && name.ends_with(".wav"))
                .unwrap_or(false)
        })
        .collect())
}

fn pcm_from_wav_bytes(wav: &[u8]) -> Result<Vec<u8>, String> {
    if wav.len() <= 44 {
        return Err("meeting recording is empty or corrupt".into());
    }
    Ok(wav[44..].to_vec())
}

fn replace_transcript_with_retranscribed_text(record: &mut MeetingRecord, text: String) {
    let now = Utc::now().to_rfc3339();
    record.transcript_segments = vec![TranscriptSegment {
        id: format!("retranscribed-{}", Uuid::new_v4()),
        speaker_label: "未区分".to_string(),
        start_ms: 0,
        end_ms: record.duration_ms,
        text: text.trim().to_string(),
        source: TranscriptSegmentSource::RetranscribedAsr,
    }];
    if record.status != MeetingStatus::Summarizing {
        record.status = MeetingStatus::Completed;
    }
    record.updated_at = now;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asr::wav::encode_wav_16k_mono;
    use crate::types::{MeetingSummary, MeetingTodo};

    #[test]
    fn validate_meeting_id_rejects_path_traversal() {
        assert_eq!(
            validate_meeting_id("../../etc/passwd"),
            Err("invalid meeting id".into())
        );
        assert_eq!(
            validate_meeting_id("..\\..\\windows\\system32"),
            Err("invalid meeting id".into())
        );
    }

    #[test]
    fn validate_meeting_id_accepts_uuid_literal() {
        assert!(validate_meeting_id("550e8400-e29b-41d4-a716-446655440000").is_ok());
    }

    #[test]
    fn meeting_markdown_includes_summary_todos_and_transcript() {
        let mut record = fixture_record();
        record.summary = MeetingSummary {
            overview: "概览".to_string(),
            key_decisions: vec!["决定 A".to_string()],
            todos: vec![MeetingTodo {
                id: "todo-1".to_string(),
                content: "跟进事项".to_string(),
                owner: Some("Alice".to_string()),
                due_date: Some("2026-07-07".to_string()),
                source_segment_ids: vec!["seg-1".to_string()],
                source_quote: Some("原文引用".to_string()),
            }],
            risks_and_open_questions: vec!["风险 A".to_string()],
        };
        record.transcript_segments = vec![TranscriptSegment {
            id: "seg-1".to_string(),
            speaker_label: "未区分".to_string(),
            start_ms: 3_723_000,
            end_ms: None,
            text: "原文内容".to_string(),
            source: TranscriptSegmentSource::RealtimeAsr,
        }];

        let markdown = meeting_markdown(&record);

        assert!(markdown.contains("# 会议记录 2026-07-06 10:00"));
        assert!(markdown.contains("## Overview"));
        assert!(markdown.contains("概览"));
        assert!(markdown.contains("决定 A"));
        assert!(markdown.contains("跟进事项 | Owner: Alice | Due: 2026-07-07 | Sources: seg-1"));
        assert!(markdown.contains("风险 A"));
        assert!(markdown.contains("[未区分][01:02:03] 原文内容"));
    }

    #[test]
    fn segmented_meeting_audio_paths_sort_parts_and_ignore_other_files() {
        let dir = std::env::temp_dir().join(format!("meeting-audio-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("create dir");
        std::fs::write(dir.join("part-0002.wav"), b"wav").expect("write part");
        std::fs::write(dir.join("part-0001.wav"), b"wav").expect("write part");
        std::fs::write(dir.join("notes.txt"), b"skip").expect("write other");

        let mut parts = meeting_audio_part_paths(&dir).expect("read parts");
        parts.sort();

        assert_eq!(
            parts
                .iter()
                .map(|path| path.file_name().unwrap().to_string_lossy().to_string())
                .collect::<Vec<_>>(),
            vec!["part-0001.wav".to_string(), "part-0002.wav".to_string()]
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn pcm_from_wav_bytes_strips_header_and_rejects_empty() {
        let wav = encode_wav_16k_mono(&[1, -2]);
        assert_eq!(pcm_from_wav_bytes(&wav).unwrap(), wav[44..].to_vec());
        assert_eq!(
            pcm_from_wav_bytes(&[0u8; 44]),
            Err("meeting recording is empty or corrupt".to_string())
        );
    }

    #[test]
    fn replace_transcript_sets_retranscribed_source_and_preserves_summary() {
        let mut record = fixture_record();
        record.summary.overview = "保留总结".to_string();

        replace_transcript_with_retranscribed_text(&mut record, "  新原文  ".to_string());

        assert_eq!(record.transcript_segments.len(), 1);
        let segment = &record.transcript_segments[0];
        assert_eq!(segment.text, "新原文");
        assert_eq!(segment.source, TranscriptSegmentSource::RetranscribedAsr);
        assert_eq!(segment.speaker_label, "未区分");
        assert_eq!(segment.end_ms, record.duration_ms);
        assert_eq!(record.summary.overview, "保留总结");
        assert_eq!(record.status, MeetingStatus::Completed);
    }

    #[test]
    fn meeting_update_guard_rejects_active_recording_record() {
        let mut record = fixture_record();
        record.status = MeetingStatus::Recording;

        assert_eq!(
            ensure_meeting_record_is_not_active(&record),
            Err("meeting recording is active".to_string())
        );
    }

    #[test]
    fn meeting_update_rejects_when_existing_record_is_active() {
        let submitted = fixture_record();
        let mut existing = submitted.clone();
        existing.status = MeetingStatus::Recording;

        assert_eq!(
            ensure_existing_meeting_can_be_updated(Some(&existing)),
            Err("meeting recording is active".to_string())
        );
    }

    #[test]
    fn meeting_delete_guard_rejects_active_recording_record() {
        let mut record = fixture_record();
        record.status = MeetingStatus::Paused;

        assert_eq!(
            ensure_meeting_record_is_not_active(&record),
            Err("meeting recording is active".to_string())
        );
    }

    #[test]
    fn retranscribe_guard_rejects_any_active_meeting_recording() {
        let active = fixture_record();

        assert_eq!(
            ensure_no_active_meeting_recording(Some(&active)),
            Err("meeting recording is active".to_string())
        );
        assert_eq!(ensure_no_active_meeting_recording(None), Ok(()));
    }

    #[test]
    fn meeting_delete_removes_audio_before_record_so_cleanup_failure_preserves_record() {
        let record = fixture_record();
        let mut record_deleted = false;

        let result = delete_meeting_record_with_cleanup(
            &record.id,
            || Ok(Some(record.clone())),
            |_id| {
                record_deleted = true;
                Ok(())
            },
            |_id| Err("delete meeting audio failed: locked".to_string()),
        );

        assert_eq!(
            result,
            Err("delete meeting audio failed: locked".to_string())
        );
        assert!(!record_deleted);
    }

    fn fixture_record() -> MeetingRecord {
        MeetingRecord {
            id: "550e8400-e29b-41d4-a716-446655440000".to_string(),
            title: "会议记录 2026-07-06 10:00".to_string(),
            status: MeetingStatus::Completed,
            started_at: "2026-07-06T10:00:00Z".to_string(),
            ended_at: Some("2026-07-06T11:00:00Z".to_string()),
            duration_ms: Some(3_600_000),
            transcript_segments: Vec::new(),
            summary: MeetingSummary::default(),
            audio: crate::types::MeetingAudioMeta {
                state: MeetingAudioState::Retained,
                retained: true,
                path: None,
            },
            created_at: "2026-07-06T10:00:00Z".to_string(),
            updated_at: "2026-07-06T11:00:00Z".to_string(),
        }
    }
}

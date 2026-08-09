use super::*;
use chrono::Utc;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, OnceLock},
};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use uuid::Uuid;

use crate::types::{
    MeetingAudioState, MeetingListItem, MeetingStatus, TranscriptSegment, TranscriptSegmentSource,
};

const WAV_HEADER_BYTES: u64 = 44;
const RETRANSCRIBE_PCM_CHUNK_BYTES: usize = 16_000 * 2 * 60 * 5;
static PLAYBACK_CACHE_LOCKS: OnceLock<
    tokio::sync::Mutex<HashMap<PathBuf, Arc<tokio::sync::Mutex<()>>>>,
> = OnceLock::new();

#[tauri::command]
pub fn list_meetings() -> Result<Vec<MeetingListItem>, String> {
    let store = MeetingStore::new().map_err(|e| e.to_string())?;
    let mut records = store.list().map_err(|e| e.to_string())?;
    sync_missing_meeting_audio_states(&store, &mut records);
    Ok(records.iter().map(MeetingListItem::from).collect())
}

#[tauri::command]
pub fn get_meeting(id: String) -> Result<MeetingRecord, String> {
    validate_meeting_id(&id)?;
    let store = MeetingStore::new().map_err(|e| e.to_string())?;
    let mut record = store
        .get(&id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "meeting not found".to_string())?;
    sync_missing_meeting_audio_states(&store, std::slice::from_mut(&mut record));
    Ok(record)
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
    app: AppHandle,
) -> Result<MeetingRecordingSnapshot, String> {
    let snapshot = coord.start_meeting_recording().await?;
    #[cfg(not(mobile))]
    crate::meeting_companion::meeting_started(
        &app,
        &snapshot.meeting.id,
        coord.prefs().get().meeting_companion_enabled,
    );
    Ok(snapshot)
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
pub async fn prepare_meeting_audio_playback(id: String) -> Result<String, String> {
    validate_meeting_id(&id)?;
    let store = MeetingStore::new().map_err(|e| e.to_string())?;
    let mut record = store
        .get(&id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "meeting not found".to_string())?;
    if record.audio.state != MeetingAudioState::Retained {
        return Err("meeting audio is not retained".into());
    }

    let path = crate::persistence::meeting_recording_existing_path_for_id(&id)
        .map_err(|e| e.to_string())?;
    if !path.exists() {
        mark_meeting_audio_missing(&store, &mut record);
        return Err("meeting recording not found".into());
    }
    match meeting_audio_playback_path(&path).await {
        Ok(playback_path) => Ok(playback_path.to_string_lossy().into_owned()),
        Err(error) => {
            if error.contains("not found") {
                mark_meeting_audio_missing(&store, &mut record);
            }
            Err(error)
        }
    }
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
        mark_meeting_audio_missing(&store, &mut record);
        return Err("meeting recording not found".into());
    }
    let segments = match retranscribe_meeting_audio_in_chunks(&path, coord.inner().as_ref()).await {
        Ok(segments) => segments,
        Err(error) => {
            if error.contains("not found") {
                mark_meeting_audio_missing(&store, &mut record);
            }
            return Err(error);
        }
    };
    if segments.is_empty() {
        return Err("meeting retranscribe returned empty transcript".into());
    }

    replace_transcript_with_retranscribed_segments(&mut record, segments);
    store
        .update(record.clone())
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "meeting not found".to_string())?;
    Ok(record)
}

async fn retranscribe_meeting_audio_in_chunks(
    path: &Path,
    coord: &crate::coordinator::Coordinator,
) -> Result<Vec<TranscriptSegment>, String> {
    let part_paths = meeting_audio_wav_paths(path)?;
    let mut segments = Vec::new();
    let mut offset_ms = 0u64;
    for part_path in part_paths {
        let mut wav = open_meeting_wav(&part_path).await?;
        let mut remaining = read_wav_header(&mut wav, &part_path).await?;
        while remaining > 0 {
            let pcm =
                read_wav_pcm_chunk(&mut wav, &mut remaining, RETRANSCRIBE_PCM_CHUNK_BYTES).await?;
            if pcm.is_empty() {
                break;
            }
            let chunk_duration_ms = pcm_duration_ms(pcm.len());
            let (text, _asr_label) = coord.retranscribe_pcm(pcm).await?;
            let trimmed = text.trim();
            if !trimmed.is_empty() {
                segments.push(TranscriptSegment {
                    id: format!("retranscribed-{}", Uuid::new_v4()),
                    speaker_label: "未区分".to_string(),
                    start_ms: offset_ms,
                    end_ms: Some(offset_ms.saturating_add(chunk_duration_ms)),
                    text: trimmed.to_string(),
                    source: TranscriptSegmentSource::RetranscribedAsr,
                    metadata: None,
                });
            }
            offset_ms = offset_ms.saturating_add(chunk_duration_ms);
        }
    }
    Ok(segments)
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

fn mark_meeting_audio_missing(store: &MeetingStore, record: &mut MeetingRecord) {
    apply_meeting_audio_missing(record);
    if let Err(error) = store.update(record.clone()) {
        log::warn!(
            "[meetings] failed to persist missing audio state for {}: {error}",
            record.id
        );
    }
}

fn sync_missing_meeting_audio_states(store: &MeetingStore, records: &mut [MeetingRecord]) {
    for record in records {
        if !meeting_audio_file_missing(record) {
            continue;
        }
        mark_meeting_audio_missing(store, record);
    }
}

fn meeting_audio_file_missing(record: &MeetingRecord) -> bool {
    meeting_audio_file_missing_with_resolver(record, |id| {
        crate::persistence::meeting_recording_existing_path_for_id(id)
    })
}

fn meeting_audio_file_missing_with_resolver<F>(record: &MeetingRecord, path_for_id: F) -> bool
where
    F: Fn(&str) -> anyhow::Result<PathBuf>,
{
    if record.audio.state != MeetingAudioState::Retained {
        return false;
    }
    match path_for_id(&record.id) {
        Ok(path) => !path.exists(),
        Err(error) => {
            log::warn!(
                "[meetings] failed to resolve audio path for {}: {error}",
                record.id
            );
            false
        }
    }
}

fn apply_meeting_audio_missing(record: &mut MeetingRecord) {
    record.audio.state = MeetingAudioState::Missing;
    record.audio.retained = false;
    record.audio.path = None;
    record.updated_at = Utc::now().to_rfc3339();
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
    out.push_str("## 会议总结\n\n");
    out.push_str("### 元信息\n\n");
    out.push_str(&format!("- 开始时间: {}\n", record.started_at));
    out.push_str(&format!(
        "- 结束时间: {}\n",
        record.ended_at.as_deref().unwrap_or("-")
    ));
    out.push_str(&format!(
        "- 时长: {}\n",
        record
            .duration_ms
            .map(format_duration_hms)
            .unwrap_or_else(|| "-".to_string())
    ));
    out.push_str(&format!("- 状态: {:?}\n\n", record.status));

    out.push_str("### 概览\n\n");
    push_optional_block(&mut out, &record.summary.overview);

    out.push_str("### 关键决定\n\n");
    push_markdown_list(&mut out, &record.summary.key_decisions);

    out.push_str("### 待办事项\n\n");
    if record.summary.todos.is_empty() {
        out.push_str("- None\n\n");
    } else {
        for todo in &record.summary.todos {
            out.push_str("- ");
            out.push_str(&todo.content);
            if let Some(owner) = todo.owner.as_deref().filter(|value| !value.is_empty()) {
                out.push_str(&format!(" | 负责人: {owner}"));
            }
            if let Some(due) = todo.due_date.as_deref().filter(|value| !value.is_empty()) {
                out.push_str(&format!(" | 截止时间: {due}"));
            }
            if !todo.source_segment_ids.is_empty() {
                out.push_str(&format!(
                    " | 来源片段: {}",
                    todo.source_segment_ids.join(", ")
                ));
            }
            if let Some(quote) = todo
                .source_quote
                .as_deref()
                .filter(|value| !value.is_empty())
            {
                out.push_str(&format!(" | 引用: {quote}"));
            }
            out.push('\n');
        }
        out.push('\n');
    }

    out.push_str("### 风险与开放问题\n\n");
    push_markdown_list(&mut out, &record.summary.risks_and_open_questions);

    out.push_str("## 转写后的会议原文\n\n");
    if record.transcript_segments.is_empty() {
        out.push_str("_暂无会议原文。_\n");
    } else {
        for segment in &record.transcript_segments {
            out.push_str(&format!(
                "- 发言人: {} | 时间: {} | 内容: {}\n\n",
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

async fn meeting_audio_playback_path(path: &Path) -> Result<PathBuf, String> {
    if !path.is_dir() {
        return Ok(path.to_path_buf());
    }

    let mut parts = meeting_audio_part_paths(path)?;
    if parts.is_empty() {
        return Err("meeting recording not found".into());
    }
    parts.sort();
    if parts.len() == 1 {
        return Ok(parts.remove(0));
    }

    let playback_path = path.join("playback.wav");
    let cache_lock = playback_cache_lock(&playback_path).await;
    let _cache_guard = cache_lock.lock().await;
    let expected_data_size = expected_wav_data_size(&parts).await?;
    if playback_cache_is_valid(&playback_path, expected_data_size).await {
        return Ok(playback_path);
    }
    if playback_path.exists() {
        tokio::fs::remove_file(&playback_path)
            .await
            .map_err(|e| format!("remove invalid meeting playback wav failed: {e}"))?;
    }
    write_playback_cache_atomically(&parts, &playback_path, expected_data_size).await?;
    if !playback_cache_is_valid(&playback_path, expected_data_size).await {
        let _ = tokio::fs::remove_file(&playback_path).await;
        return Err("meeting playback cache is empty or corrupt".into());
    }
    Ok(playback_path)
}

async fn playback_cache_lock(path: &Path) -> Arc<tokio::sync::Mutex<()>> {
    let locks = PLAYBACK_CACHE_LOCKS.get_or_init(|| tokio::sync::Mutex::new(HashMap::new()));
    let mut locks = locks.lock().await;
    Arc::clone(
        locks
            .entry(path.to_path_buf())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(()))),
    )
}

fn meeting_audio_wav_paths(path: &Path) -> Result<Vec<PathBuf>, String> {
    if !path.is_dir() {
        return Ok(vec![path.to_path_buf()]);
    }
    let mut parts = meeting_audio_part_paths(path)?;
    if parts.is_empty() {
        return Err("meeting recording not found".into());
    }
    parts.sort();
    Ok(parts)
}

async fn expected_wav_data_size(parts: &[PathBuf]) -> Result<u64, String> {
    let mut total = 0u64;
    for part in parts {
        let mut file = open_meeting_wav(part).await?;
        total = total
            .checked_add(read_wav_header(&mut file, part).await?)
            .ok_or_else(|| "meeting recording is too large to play".to_string())?;
    }
    Ok(total)
}

async fn playback_cache_is_valid(path: &Path, expected_data_size: u64) -> bool {
    let Ok(mut file) = tokio::fs::File::open(path).await else {
        return false;
    };
    matches!(read_wav_header(&mut file, path).await, Ok(size) if size == expected_data_size)
}

async fn write_playback_cache_atomically(
    parts: &[PathBuf],
    playback_path: &Path,
    data_size: u64,
) -> Result<(), String> {
    let data_size_u32 = u32::try_from(data_size)
        .map_err(|_| "meeting recording is too large to play".to_string())?;
    let mut first = open_meeting_wav(&parts[0]).await?;
    let mut header = [0u8; WAV_HEADER_BYTES as usize];
    first
        .read_exact(&mut header)
        .await
        .map_err(|e| format!("read meeting wav header failed: {e}"))?;
    header[4..8].copy_from_slice(&(36 + data_size_u32).to_le_bytes());
    header[40..44].copy_from_slice(&data_size_u32.to_le_bytes());

    let temp_path =
        playback_path.with_file_name(format!(".playback-{}.tmp", Uuid::new_v4().simple()));
    let write_result = async {
        let mut output = tokio::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temp_path)
            .await
            .map_err(|e| format!("create meeting playback cache failed: {e}"))?;
        output
            .write_all(&header)
            .await
            .map_err(|e| format!("write meeting playback header failed: {e}"))?;
        for part in parts {
            let mut input = open_meeting_wav(part).await?;
            let part_data_size = read_wav_header(&mut input, part).await?;
            tokio::io::copy(&mut input.take(part_data_size), &mut output)
                .await
                .map_err(|e| format!("write meeting playback audio failed: {e}"))?;
        }
        output
            .flush()
            .await
            .map_err(|e| format!("flush meeting playback cache failed: {e}"))?;
        output
            .sync_all()
            .await
            .map_err(|e| format!("sync meeting playback cache failed: {e}"))?;
        tokio::fs::rename(&temp_path, playback_path)
            .await
            .map_err(|e| format!("commit meeting playback cache failed: {e}"))
    }
    .await;
    if write_result.is_err() {
        let _ = tokio::fs::remove_file(&temp_path).await;
    }
    write_result
}

async fn open_meeting_wav(path: &Path) -> Result<tokio::fs::File, String> {
    tokio::fs::File::open(path).await.map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            "meeting recording not found".to_string()
        } else {
            format!("read meeting wav failed: {e}")
        }
    })
}

async fn read_wav_header(file: &mut tokio::fs::File, path: &Path) -> Result<u64, String> {
    file.seek(std::io::SeekFrom::Start(0))
        .await
        .map_err(|e| format!("seek meeting wav failed: {e}"))?;
    let mut header = [0u8; WAV_HEADER_BYTES as usize];
    file.read_exact(&mut header)
        .await
        .map_err(|_| "meeting recording is empty or corrupt".to_string())?;
    if &header[0..4] != b"RIFF" || &header[8..12] != b"WAVE" || &header[36..40] != b"data" {
        return Err("meeting recording is empty or corrupt".into());
    }
    let data_size = u32::from_le_bytes(header[40..44].try_into().unwrap()) as u64;
    let file_size = file
        .metadata()
        .await
        .map_err(|e| format!("read meeting wav metadata failed: {e}"))?
        .len();
    if data_size == 0 || data_size % 2 != 0 || file_size != WAV_HEADER_BYTES + data_size {
        log::warn!("[meetings] invalid wav size for {}", path.display());
        return Err("meeting recording is empty or corrupt".into());
    }
    Ok(data_size)
}

async fn read_wav_pcm_chunk(
    file: &mut tokio::fs::File,
    remaining: &mut u64,
    max_bytes: usize,
) -> Result<Vec<u8>, String> {
    if max_bytes == 0 || max_bytes % 2 != 0 {
        return Err("meeting PCM chunk size must be a positive even number".into());
    }
    let chunk_size = (*remaining).min(max_bytes as u64) as usize;
    if chunk_size == 0 {
        return Ok(Vec::new());
    }
    let mut pcm = vec![0u8; chunk_size];
    file.read_exact(&mut pcm)
        .await
        .map_err(|_| "meeting recording is empty or corrupt".to_string())?;
    *remaining -= chunk_size as u64;
    Ok(pcm)
}

fn pcm_duration_ms(byte_len: usize) -> u64 {
    (byte_len as u64).saturating_mul(1000) / (16_000 * 2)
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

fn replace_transcript_with_retranscribed_segments(
    record: &mut MeetingRecord,
    segments: Vec<TranscriptSegment>,
) {
    let now = Utc::now().to_rfc3339();
    record.transcript_segments = segments;
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
            metadata: None,
        }];

        let markdown = meeting_markdown(&record);

        assert!(markdown.contains("# 会议记录 2026-07-06 10:00"));
        assert!(markdown.contains("## 会议总结"));
        assert!(markdown.contains("### 元信息"));
        assert!(markdown.contains("- 开始时间: 2026-07-06T10:00:00Z"));
        assert!(markdown.contains("### 概览"));
        assert!(markdown.contains("概览"));
        assert!(markdown.contains("### 关键决定"));
        assert!(markdown.contains("决定 A"));
        assert!(markdown.contains("### 待办事项"));
        assert!(
            markdown.contains("跟进事项 | 负责人: Alice | 截止时间: 2026-07-07 | 来源片段: seg-1")
        );
        assert!(markdown.contains("### 风险与开放问题"));
        assert!(markdown.contains("风险 A"));
        assert!(markdown.contains("## 转写后的会议原文"));
        assert!(markdown.contains("- 发言人: 未区分 | 时间: 01:02:03 | 内容: 原文内容"));
        assert!(
            markdown.find("## 会议总结").unwrap() < markdown.find("## 转写后的会议原文").unwrap()
        );
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

    #[tokio::test]
    async fn wav_pcm_reader_keeps_chunks_bounded() {
        let path = std::env::temp_dir().join(format!("meeting-audio-{}.wav", Uuid::new_v4()));
        std::fs::write(&path, encode_wav_16k_mono(&[1, 2, 3, 4, 5])).expect("write wav");
        let mut file = open_meeting_wav(&path).await.expect("open wav");
        let mut remaining = read_wav_header(&mut file, &path)
            .await
            .expect("read header");

        let first = read_wav_pcm_chunk(&mut file, &mut remaining, 4)
            .await
            .expect("first chunk");
        let second = read_wav_pcm_chunk(&mut file, &mut remaining, 4)
            .await
            .expect("second chunk");
        let third = read_wav_pcm_chunk(&mut file, &mut remaining, 4)
            .await
            .expect("third chunk");

        assert_eq!(first.len(), 4);
        assert_eq!(second.len(), 4);
        assert_eq!(third.len(), 2);
        assert_eq!(remaining, 0);
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn meeting_audio_playback_path_caches_segmented_parts() {
        let dir = std::env::temp_dir().join(format!("meeting-audio-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("create dir");
        std::fs::write(dir.join("part-0002.wav"), encode_wav_16k_mono(&[3, 4]))
            .expect("write part");
        std::fs::write(dir.join("part-0001.wav"), encode_wav_16k_mono(&[1, 2]))
            .expect("write part");
        std::fs::write(dir.join("playback.wav"), b"corrupt").expect("write corrupt cache");

        let playback_path = meeting_audio_playback_path(&dir)
            .await
            .expect("prepare playback");
        let wav = std::fs::read(&playback_path).expect("read playback wav");

        assert_eq!(playback_path, dir.join("playback.wav"));
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        let expected = encode_wav_16k_mono(&[1, 2, 3, 4]);
        assert_eq!(&wav[44..], &expected[44..]);
        assert!(std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .all(|entry| !entry.file_name().to_string_lossy().ends_with(".tmp")));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn meeting_audio_playback_path_serializes_concurrent_cache_writes() {
        let dir = std::env::temp_dir().join(format!("meeting-audio-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("create dir");
        std::fs::write(dir.join("part-0001.wav"), encode_wav_16k_mono(&[1, 2]))
            .expect("write part");
        std::fs::write(dir.join("part-0002.wav"), encode_wav_16k_mono(&[3, 4]))
            .expect("write part");

        let (first, second) = tokio::join!(
            meeting_audio_playback_path(&dir),
            meeting_audio_playback_path(&dir),
        );

        assert_eq!(first.unwrap(), dir.join("playback.wav"));
        assert_eq!(second.unwrap(), dir.join("playback.wav"));
        assert!(playback_cache_is_valid(&dir.join("playback.wav"), 8).await);
        assert!(std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .all(|entry| !entry.file_name().to_string_lossy().ends_with(".tmp")));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn playback_cache_locks_are_scoped_per_meeting_path() {
        let root = std::env::temp_dir().join(format!("meeting-audio-{}", Uuid::new_v4()));
        let first = playback_cache_lock(&root.join("first/playback.wav")).await;
        let first_again = playback_cache_lock(&root.join("first/playback.wav")).await;
        let second = playback_cache_lock(&root.join("second/playback.wav")).await;

        assert!(Arc::ptr_eq(&first, &first_again));
        assert!(!Arc::ptr_eq(&first, &second));
    }

    #[tokio::test]
    async fn meeting_audio_playback_path_reuses_single_part() {
        let dir = std::env::temp_dir().join(format!("meeting-audio-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("create dir");
        let part = dir.join("part-0001.wav");
        std::fs::write(&part, encode_wav_16k_mono(&[1, 2])).expect("write part");

        assert_eq!(meeting_audio_playback_path(&dir).await.unwrap(), part);
        assert!(!dir.join("playback.wav").exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn replace_transcript_sets_retranscribed_source_and_preserves_summary() {
        let mut record = fixture_record();
        record.summary.overview = "保留总结".to_string();
        let segment = TranscriptSegment {
            id: "retranscribed-1".to_string(),
            speaker_label: "未区分".to_string(),
            start_ms: 0,
            end_ms: Some(1_000),
            text: "新原文".to_string(),
            source: TranscriptSegmentSource::RetranscribedAsr,
            metadata: None,
        };

        replace_transcript_with_retranscribed_segments(&mut record, vec![segment]);

        assert_eq!(record.transcript_segments.len(), 1);
        let segment = &record.transcript_segments[0];
        assert_eq!(segment.text, "新原文");
        assert_eq!(segment.source, TranscriptSegmentSource::RetranscribedAsr);
        assert_eq!(segment.speaker_label, "未区分");
        assert_eq!(segment.end_ms, Some(1_000));
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
    fn missing_meeting_audio_state_clears_retention_flags() {
        let mut record = fixture_record();

        apply_meeting_audio_missing(&mut record);

        assert_eq!(record.audio.state, MeetingAudioState::Missing);
        assert!(!record.audio.retained);
        assert_eq!(record.audio.path, None);
    }

    #[test]
    fn retained_meeting_audio_missing_detects_deleted_file() {
        let record = fixture_record();
        let missing_path = std::env::temp_dir().join(format!("missing-{}.wav", Uuid::new_v4()));

        assert!(meeting_audio_file_missing_with_resolver(&record, |_id| {
            Ok(missing_path.clone())
        }));
    }

    #[test]
    fn non_retained_meeting_audio_does_not_report_missing() {
        let mut record = fixture_record();
        record.audio.state = MeetingAudioState::Pruned;
        let missing_path = std::env::temp_dir().join(format!("missing-{}.wav", Uuid::new_v4()));

        assert!(!meeting_audio_file_missing_with_resolver(&record, |_id| {
            Ok(missing_path.clone())
        }));
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

    #[test]
    fn meeting_list_item_serialization_omits_full_detail() {
        let mut record = fixture_record();
        record.summary.overview = "摘要概览".to_string();
        record.transcript_segments = vec![TranscriptSegment {
            id: "segment-1".to_string(),
            speaker_label: "未区分".to_string(),
            start_ms: 0,
            end_ms: Some(1_000),
            text: "原文预览".repeat(100),
            source: TranscriptSegmentSource::RealtimeAsr,
            metadata: None,
        }];

        let item = MeetingListItem::from(&record);
        let value = serde_json::to_value(&item).expect("serialize meeting list item");

        assert_eq!(item.summary_overview, "摘要概览");
        assert_eq!(item.transcript_segment_count, 1);
        assert_eq!(item.transcript_preview.chars().count(), 180);
        assert!(value.get("transcriptSegments").is_none());
        assert!(value.get("summary").is_none());
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

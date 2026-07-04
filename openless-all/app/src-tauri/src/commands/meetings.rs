use super::*;

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
    let id = record.id.clone();
    let store = MeetingStore::new().map_err(|e| e.to_string())?;
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
    let removed = store.delete(&id).map_err(|e| e.to_string())?;
    if removed.is_none() {
        return Ok(());
    }
    let path = crate::persistence::meeting_recording_existing_path_for_id(&id)
        .map_err(|e| e.to_string())?;
    crate::persistence::remove_meeting_audio_path(&path).map_err(|e| e.to_string())
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

#[cfg(test)]
mod tests {
    use super::validate_meeting_id;

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
}

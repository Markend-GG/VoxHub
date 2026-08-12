#![cfg_attr(target_os = "linux", allow(dead_code, unused_variables))]
//! Meeting record store: newest-first JSON list plus retained audio pruning.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use parking_lot::Mutex;

use super::{
    atomic_write, data_dir, ensure_dir, meeting_recording_dir_path_for_id,
    meeting_recording_path_for_id, read_or_default,
};
use crate::types::{
    clamp_meeting_audio_retention_count, MeetingAudioState, MeetingRecord, MeetingStatus,
};

const MEETINGS_FILE: &str = "meetings.json";
const WAV_HEADER_BYTES: u64 = 44;
static MEETING_STORE_LOCK: Mutex<()> = Mutex::new(());

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveredMeeting {
    pub id: String,
    pub previous_status: MeetingStatus,
    pub recovered_status: MeetingStatus,
}

struct RecoveredAudio {
    duration_ms: u64,
    latest_modified: Option<SystemTime>,
}

pub struct MeetingStore {
    path: PathBuf,
}

impl MeetingStore {
    pub fn new() -> Result<Self> {
        let dir = data_dir()?;
        ensure_dir(&dir)?;
        Ok(Self {
            path: dir.join(MEETINGS_FILE),
        })
    }

    #[allow(dead_code)]
    pub(crate) fn new_fallback() -> Self {
        Self {
            path: std::env::temp_dir().join("openless_meetings_fallback.json"),
        }
    }

    #[cfg(test)]
    fn new_for_path(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn list(&self) -> Result<Vec<MeetingRecord>> {
        let _guard = MEETING_STORE_LOCK.lock();
        self.read_locked()
    }

    pub fn get(&self, id: &str) -> Result<Option<MeetingRecord>> {
        let _guard = MEETING_STORE_LOCK.lock();
        Ok(self
            .read_locked()?
            .into_iter()
            .find(|record| record.id == id))
    }

    pub fn create(&self, record: MeetingRecord) -> Result<MeetingRecord> {
        let _guard = MEETING_STORE_LOCK.lock();
        let mut records = self.read_locked()?;
        records.retain(|existing| existing.id != record.id);
        records.insert(0, record.clone());
        self.write_locked(&records)?;
        Ok(record)
    }

    pub fn update(&self, record: MeetingRecord) -> Result<Option<MeetingRecord>> {
        let _guard = MEETING_STORE_LOCK.lock();
        let mut records = self.read_locked()?;
        let Some(slot) = records.iter_mut().find(|existing| existing.id == record.id) else {
            return Ok(None);
        };
        *slot = record.clone();
        self.write_locked(&records)?;
        Ok(Some(record))
    }

    pub fn update_if<F>(&self, id: &str, update: F) -> Result<Option<MeetingRecord>>
    where
        F: FnOnce(&mut MeetingRecord) -> bool,
    {
        let _guard = MEETING_STORE_LOCK.lock();
        let mut records = self.read_locked()?;
        let Some(record) = records.iter_mut().find(|record| record.id == id) else {
            return Ok(None);
        };
        if !update(record) {
            return Ok(None);
        }
        let updated = record.clone();
        self.write_locked(&records)?;
        Ok(Some(updated))
    }

    pub fn delete_with_cleanup<P, R>(
        &self,
        id: &str,
        prepare: P,
        remove_audio: R,
    ) -> Result<Option<MeetingRecord>>
    where
        P: FnOnce(&mut MeetingRecord) -> Result<()>,
        R: FnOnce(&str) -> Result<()>,
    {
        let _guard = MEETING_STORE_LOCK.lock();
        let mut records = self.read_locked()?;
        let Some(index) = records.iter().position(|record| record.id == id) else {
            return Ok(None);
        };
        prepare(&mut records[index])?;
        remove_audio(id)?;
        let removed = records.remove(index);
        self.write_locked(&records)?;
        Ok(Some(removed))
    }

    pub fn prune_audio_retention(&self, retention_count: u32) -> Result<usize> {
        let _guard = MEETING_STORE_LOCK.lock();
        let mut records = self.read_locked()?;
        let pruned = prune_meeting_audio(&mut records, retention_count)?;
        if pruned > 0 {
            self.write_locked(&records)?;
        }
        Ok(pruned)
    }

    pub fn recover_orphaned_runtime_states(&self) -> Result<Vec<RecoveredMeeting>> {
        let _guard = MEETING_STORE_LOCK.lock();
        let mut records = self.read_locked()?;
        let recovered_at = Utc::now().to_rfc3339();
        let recovered = recover_orphaned_runtime_records_with_path_resolver(
            &mut records,
            &recovered_at,
            meeting_recording_existing_path_for_id,
        );
        if !recovered.is_empty() {
            self.write_locked(&records)?;
        }
        Ok(recovered)
    }

    fn read_locked(&self) -> Result<Vec<MeetingRecord>> {
        read_or_default::<Vec<MeetingRecord>>(&self.path)
    }

    fn write_locked(&self, records: &[MeetingRecord]) -> Result<()> {
        let json = serde_json::to_vec_pretty(records).context("encode meetings failed")?;
        atomic_write(&self.path, &json)
    }
}

fn recover_orphaned_runtime_records_with_path_resolver<F>(
    records: &mut [MeetingRecord],
    recovered_at: &str,
    path_for_id: F,
) -> Vec<RecoveredMeeting>
where
    F: Fn(&str) -> Result<PathBuf>,
{
    let mut recovered = Vec::new();
    for record in records {
        let previous_status = record.status.clone();
        let recovered_status = match previous_status {
            MeetingStatus::Recording | MeetingStatus::Paused => {
                recover_orphaned_recording_audio(record, recovered_at, &path_for_id);
                MeetingStatus::TranscribingInterrupted
            }
            MeetingStatus::TranscribingInterrupted
                if record.ended_at.is_none()
                    || record.audio.state == MeetingAudioState::Temporary =>
            {
                recover_orphaned_recording_audio(record, recovered_at, &path_for_id);
                MeetingStatus::TranscribingInterrupted
            }
            MeetingStatus::Summarizing => {
                recover_orphaned_recording_audio(record, recovered_at, &path_for_id);
                MeetingStatus::SummaryFailed
            }
            _ => continue,
        };

        record.status = recovered_status.clone();
        record.updated_at = recovered_at.to_string();
        recovered.push(RecoveredMeeting {
            id: record.id.clone(),
            previous_status,
            recovered_status,
        });
    }
    recovered
}

fn recover_orphaned_recording_audio<F>(
    record: &mut MeetingRecord,
    recovered_at: &str,
    path_for_id: &F,
) where
    F: Fn(&str) -> Result<PathBuf>,
{
    let recovered_audio =
        path_for_id(&record.id).and_then(|path| inspect_recovered_meeting_audio(&path));
    match recovered_audio {
        Ok(audio) => {
            record.duration_ms = Some(audio.duration_ms);
            record.ended_at = Some(
                audio
                    .latest_modified
                    .map(|value| DateTime::<Utc>::from(value).to_rfc3339())
                    .unwrap_or_else(|| recovered_end_time_fallback(record, recovered_at)),
            );
            record.audio.state = MeetingAudioState::Retained;
            record.audio.retained = true;
            record.audio.path = None;
        }
        Err(error) => {
            log::warn!(
                "[meetings] orphaned meeting {} has no usable audio: {error}",
                record.id
            );
            if matches!(
                record.audio.state,
                MeetingAudioState::Temporary | MeetingAudioState::Retained
            ) {
                record.audio.state = MeetingAudioState::Missing;
                record.audio.retained = false;
                record.audio.path = None;
            }
            if record.ended_at.is_none() {
                record.ended_at = Some(recovered_end_time_fallback(record, recovered_at));
            }
        }
    }
}

fn recovered_end_time_fallback(record: &MeetingRecord, recovered_at: &str) -> String {
    DateTime::parse_from_rfc3339(&record.updated_at)
        .map(|value| value.with_timezone(&Utc).to_rfc3339())
        .unwrap_or_else(|_| recovered_at.to_string())
}

fn inspect_recovered_meeting_audio(path: &Path) -> Result<RecoveredAudio> {
    let wav_paths = recovered_meeting_wav_paths(path)?;
    let mut duration_ms = 0u64;
    let mut latest_modified = None;
    for wav_path in wav_paths {
        let (part_duration_ms, modified) = inspect_recovered_wav(&wav_path)?;
        duration_ms = duration_ms
            .checked_add(part_duration_ms)
            .context("meeting recording duration overflow")?;
        if modified > latest_modified {
            latest_modified = modified;
        }
    }
    Ok(RecoveredAudio {
        duration_ms,
        latest_modified,
    })
}

fn recovered_meeting_wav_paths(path: &Path) -> Result<Vec<PathBuf>> {
    if path.is_file() {
        return Ok(vec![path.to_path_buf()]);
    }
    if !path.is_dir() {
        anyhow::bail!("meeting recording not found");
    }

    let mut parts = fs::read_dir(path)
        .with_context(|| format!("read meeting recording dir failed: {}", path.display()))?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|entry_path| {
            entry_path.is_file()
                && entry_path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("part-") && name.ends_with(".wav"))
        })
        .collect::<Vec<_>>();
    parts.sort();
    if parts.is_empty() {
        anyhow::bail!("meeting recording not found");
    }
    Ok(parts)
}

fn inspect_recovered_wav(path: &Path) -> Result<(u64, Option<SystemTime>)> {
    let mut file = fs::File::open(path)
        .with_context(|| format!("read meeting wav failed: {}", path.display()))?;
    let mut header = [0u8; WAV_HEADER_BYTES as usize];
    file.read_exact(&mut header)
        .context("meeting recording is empty or corrupt")?;
    if &header[0..4] != b"RIFF" || &header[8..12] != b"WAVE" || &header[36..40] != b"data" {
        anyhow::bail!("meeting recording is empty or corrupt");
    }

    let byte_rate = u32::from_le_bytes(header[28..32].try_into().unwrap()) as u64;
    let data_size = u32::from_le_bytes(header[40..44].try_into().unwrap()) as u64;
    let metadata = file
        .metadata()
        .with_context(|| format!("read meeting wav metadata failed: {}", path.display()))?;
    if byte_rate == 0
        || data_size == 0
        || data_size % 2 != 0
        || metadata.len() != WAV_HEADER_BYTES + data_size
    {
        anyhow::bail!("meeting recording is empty or corrupt");
    }
    let duration_ms = data_size
        .checked_mul(1000)
        .context("meeting recording duration overflow")?
        / byte_rate;
    Ok((duration_ms, metadata.modified().ok()))
}

pub fn prune_meeting_audio(records: &mut [MeetingRecord], retention_count: u32) -> Result<usize> {
    prune_meeting_audio_with_path_resolver(records, retention_count, |id| {
        meeting_recording_existing_path_for_id(id)
    })
}

pub fn meeting_recording_existing_path_for_id(meeting_id: &str) -> Result<PathBuf> {
    meeting_recording_existing_path_for_id_with_resolver(
        meeting_id,
        meeting_recording_dir_path_for_id,
        meeting_recording_path_for_id,
    )
}

fn meeting_recording_existing_path_for_id_with_resolver<D, F>(
    meeting_id: &str,
    dir_for_id: D,
    file_for_id: F,
) -> Result<PathBuf>
where
    D: Fn(&str) -> Result<PathBuf>,
    F: Fn(&str) -> Result<PathBuf>,
{
    let dir = dir_for_id(meeting_id)?;
    if dir.exists() {
        return Ok(dir);
    }
    file_for_id(meeting_id)
}

fn prune_meeting_audio_with_path_resolver<F>(
    records: &mut [MeetingRecord],
    retention_count: u32,
    path_for_id: F,
) -> Result<usize>
where
    F: Fn(&str) -> Result<PathBuf>,
{
    let keep = clamp_meeting_audio_retention_count(retention_count) as usize;
    let mut retained: Vec<(usize, String)> = records
        .iter()
        .enumerate()
        .filter_map(|(index, record)| {
            if record.audio.state != MeetingAudioState::Retained {
                return None;
            }
            if record.processing_hold.is_some() {
                return None;
            }
            let path = path_for_id(&record.id).ok()?;
            if !path.exists() {
                return None;
            }
            let sort_key = record
                .ended_at
                .as_ref()
                .unwrap_or(&record.created_at)
                .to_string();
            Some((index, sort_key))
        })
        .collect();

    retained.sort_by(|a, b| b.1.cmp(&a.1));

    let mut pruned = 0;
    for (index, _) in retained.into_iter().skip(keep) {
        let path = path_for_id(&records[index].id)?;
        remove_meeting_audio_path(&path)?;
        records[index].audio.state = MeetingAudioState::Pruned;
        records[index].audio.retained = false;
        records[index].audio.path = None;
        pruned += 1;
    }
    Ok(pruned)
}

pub fn remove_meeting_audio_path(path: &std::path::Path) -> Result<()> {
    if path.is_dir() {
        fs::remove_dir_all(path)
            .with_context(|| format!("delete meeting audio failed: {}", path.display()))?;
    } else if path.is_file() {
        fs::remove_file(path)
            .with_context(|| format!("delete meeting audio failed: {}", path.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{
        MeetingAudioMeta, MeetingStatus, MeetingSummary, TranscriptSegment, TranscriptSegmentSource,
    };
    use std::time::{Duration, UNIX_EPOCH};

    fn temp_root(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("{name}-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).expect("create temp root");
        dir
    }

    fn record(id: &str, created_at: &str) -> MeetingRecord {
        MeetingRecord {
            id: id.to_string(),
            title: format!("Meeting {id}"),
            status: MeetingStatus::Draft,
            started_at: created_at.to_string(),
            ended_at: None,
            duration_ms: None,
            transcript_segments: Vec::new(),
            summary: MeetingSummary::default(),
            audio: MeetingAudioMeta {
                state: MeetingAudioState::Unavailable,
                retained: false,
                path: None,
            },
            realtime_asr: None,
            post_processing_config: None,
            post_processing: None,
            transcript_revisions: Vec::new(),
            active_transcript_revision: None,
            speaker_profiles: Vec::new(),
            speaker_turns: Vec::new(),
            processing_hold: None,
            created_at: created_at.to_string(),
            updated_at: created_at.to_string(),
        }
    }

    fn retained_record(id: &str, created_at: &str, ended_at: Option<&str>) -> MeetingRecord {
        let mut record = record(id, created_at);
        record.ended_at = ended_at.map(str::to_string);
        record.audio = MeetingAudioMeta {
            state: MeetingAudioState::Retained,
            retained: true,
            path: Some(format!("ignored/{id}.wav")),
        };
        record
    }

    fn temp_audio_path(root: &std::path::Path, id: &str) -> PathBuf {
        root.join("meeting-recordings").join(format!("{id}.wav"))
    }

    fn create_audio(root: &std::path::Path, id: &str) -> PathBuf {
        let path = temp_audio_path(root, id);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create meeting audio dir");
        }
        fs::write(&path, b"wav").expect("write meeting audio");
        path
    }

    fn create_valid_wav(path: &Path, data_size: u32, byte_rate: u32, modified: SystemTime) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create wav parent");
        }
        let mut wav = vec![0u8; WAV_HEADER_BYTES as usize + data_size as usize];
        wav[0..4].copy_from_slice(b"RIFF");
        wav[4..8].copy_from_slice(&(36 + data_size).to_le_bytes());
        wav[8..12].copy_from_slice(b"WAVE");
        wav[12..16].copy_from_slice(b"fmt ");
        wav[16..20].copy_from_slice(&16u32.to_le_bytes());
        wav[20..22].copy_from_slice(&1u16.to_le_bytes());
        wav[22..24].copy_from_slice(&1u16.to_le_bytes());
        wav[24..28].copy_from_slice(&16_000u32.to_le_bytes());
        wav[28..32].copy_from_slice(&byte_rate.to_le_bytes());
        wav[32..34].copy_from_slice(&2u16.to_le_bytes());
        wav[34..36].copy_from_slice(&16u16.to_le_bytes());
        wav[36..40].copy_from_slice(b"data");
        wav[40..44].copy_from_slice(&data_size.to_le_bytes());
        fs::write(path, wav).expect("write valid wav");
        fs::OpenOptions::new()
            .write(true)
            .open(path)
            .expect("open wav for mtime")
            .set_modified(modified)
            .expect("set wav mtime");
    }

    fn segment(id: &str, text: &str) -> TranscriptSegment {
        TranscriptSegment {
            id: id.to_string(),
            speaker_id: None,
            speaker_label: "Unknown".to_string(),
            start_ms: 0,
            end_ms: Some(500),
            text: text.to_string(),
            source: TranscriptSegmentSource::RealtimeAsr,
            metadata: None,
        }
    }

    #[test]
    fn recovers_orphaned_recording_with_valid_audio_without_losing_content() {
        let tmp = temp_root("openless-meeting-recovery");
        let id = "00000000-0000-4000-8000-000000000101";
        let audio_dir = tmp.join(id);
        let modified = UNIX_EPOCH + Duration::from_secs(1_720_080_000);
        create_valid_wav(&audio_dir.join("part-0001.wav"), 32_000, 32_000, modified);
        let mut orphan = record(id, "2026-07-04T01:00:00Z");
        orphan.status = MeetingStatus::Recording;
        orphan.audio.state = MeetingAudioState::Temporary;
        orphan.transcript_segments = vec![segment("segment-1", "preserved transcript")];
        orphan.summary.overview = "preserved summary".to_string();
        let mut records = vec![orphan];

        let recovered = recover_orphaned_runtime_records_with_path_resolver(
            &mut records,
            "2026-07-19T13:00:00Z",
            |_id| Ok(audio_dir.clone()),
        );

        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].previous_status, MeetingStatus::Recording);
        assert_eq!(records[0].status, MeetingStatus::TranscribingInterrupted);
        assert_eq!(records[0].duration_ms, Some(1_000));
        assert_eq!(
            records[0].ended_at,
            Some(DateTime::<Utc>::from(modified).to_rfc3339())
        );
        assert_eq!(records[0].audio.state, MeetingAudioState::Retained);
        assert!(records[0].audio.retained);
        assert_eq!(records[0].audio.path, None);
        assert_eq!(
            records[0].transcript_segments[0].text,
            "preserved transcript"
        );
        assert_eq!(records[0].summary.overview, "preserved summary");
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn recovers_orphaned_paused_meeting_from_multiple_wav_parts() {
        let tmp = temp_root("openless-meeting-recovery");
        let id = "00000000-0000-4000-8000-000000000102";
        let audio_dir = tmp.join(id);
        let older = UNIX_EPOCH + Duration::from_secs(1_720_080_000);
        let newer = older + Duration::from_secs(10);
        create_valid_wav(&audio_dir.join("part-0001.wav"), 16_000, 32_000, older);
        create_valid_wav(&audio_dir.join("part-0002.wav"), 48_000, 32_000, newer);
        let mut orphan = record(id, "2026-07-04T01:00:00Z");
        orphan.status = MeetingStatus::Paused;
        orphan.audio.state = MeetingAudioState::Temporary;
        let mut records = vec![orphan];

        let recovered = recover_orphaned_runtime_records_with_path_resolver(
            &mut records,
            "2026-07-19T13:00:00Z",
            |_id| Ok(audio_dir.clone()),
        );

        assert_eq!(recovered[0].previous_status, MeetingStatus::Paused);
        assert_eq!(records[0].duration_ms, Some(2_000));
        assert_eq!(
            records[0].ended_at,
            Some(DateTime::<Utc>::from(newer).to_rfc3339())
        );
        assert_eq!(records[0].audio.state, MeetingAudioState::Retained);
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn recovery_marks_missing_audio_and_is_idempotent() {
        let tmp = temp_root("openless-meeting-recovery");
        let mut recording = record(
            "00000000-0000-4000-8000-000000000103",
            "2026-07-04T01:00:00Z",
        );
        recording.status = MeetingStatus::Recording;
        recording.audio.state = MeetingAudioState::Temporary;
        let mut summarizing = record(
            "00000000-0000-4000-8000-000000000104",
            "2026-07-04T02:00:00Z",
        );
        summarizing.status = MeetingStatus::Summarizing;
        summarizing.audio.state = MeetingAudioState::Temporary;
        create_valid_wav(
            &tmp.join(&summarizing.id).join("part-0001.wav"),
            32_000,
            32_000,
            UNIX_EPOCH + Duration::from_secs(1_720_080_020),
        );
        let mut interrupted = record(
            "00000000-0000-4000-8000-000000000106",
            "2026-07-04T02:30:00Z",
        );
        interrupted.status = MeetingStatus::TranscribingInterrupted;
        interrupted.audio.state = MeetingAudioState::Temporary;
        let completed = MeetingRecord {
            status: MeetingStatus::Completed,
            ..record(
                "00000000-0000-4000-8000-000000000105",
                "2026-07-04T03:00:00Z",
            )
        };
        let mut records = vec![recording, summarizing, interrupted, completed.clone()];

        let recovered = recover_orphaned_runtime_records_with_path_resolver(
            &mut records,
            "2026-07-19T13:00:00Z",
            |id| Ok(tmp.join(id)),
        );

        assert_eq!(recovered.len(), 3);
        assert_eq!(records[0].status, MeetingStatus::TranscribingInterrupted);
        assert_eq!(records[0].audio.state, MeetingAudioState::Missing);
        assert!(!records[0].audio.retained);
        assert_eq!(
            records[0].ended_at.as_deref(),
            Some("2026-07-04T01:00:00+00:00")
        );
        assert_eq!(records[1].status, MeetingStatus::SummaryFailed);
        assert_eq!(records[1].audio.state, MeetingAudioState::Retained);
        assert_eq!(records[1].duration_ms, Some(1_000));
        assert_eq!(records[2].status, MeetingStatus::TranscribingInterrupted);
        assert_eq!(records[2].audio.state, MeetingAudioState::Missing);
        assert!(records[2].ended_at.is_some());
        assert_eq!(records[3], completed);

        let after_first_recovery = records.clone();
        let second = recover_orphaned_runtime_records_with_path_resolver(
            &mut records,
            "2026-07-19T14:00:00Z",
            |id| Ok(tmp.join(id)),
        );
        assert!(second.is_empty());
        assert_eq!(records, after_first_recovery);
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn meeting_store_create_lists_newest_first_and_replaces_duplicate_id() {
        let tmp = temp_root("openless-meeting-store");
        let store = MeetingStore::new_for_path(tmp.join("meetings.json"));
        let first = record(
            "00000000-0000-4000-8000-000000000001",
            "2026-07-04T01:00:00Z",
        );
        let second = record(
            "00000000-0000-4000-8000-000000000002",
            "2026-07-04T02:00:00Z",
        );
        let replacement = MeetingRecord {
            title: "Replacement".into(),
            ..first.clone()
        };

        store.create(first).expect("create first");
        store.create(second.clone()).expect("create second");
        store.create(replacement.clone()).expect("replace first");

        let listed = store.list().expect("list meetings");
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].id, replacement.id);
        assert_eq!(listed[0].title, "Replacement");
        assert_eq!(listed[1].id, second.id);

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn meeting_store_update_returns_none_for_missing_record() {
        let tmp = temp_root("openless-meeting-store");
        let store = MeetingStore::new_for_path(tmp.join("meetings.json"));
        let missing = record(
            "00000000-0000-4000-8000-000000000001",
            "2026-07-04T01:00:00Z",
        );

        let updated = store.update(missing).expect("update missing");

        assert!(updated.is_none());
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn meeting_store_delete_with_cleanup_keeps_record_when_audio_cleanup_fails() {
        let tmp = temp_root("openless-meeting-store");
        let store = MeetingStore::new_for_path(tmp.join("meetings.json"));
        let item = record(
            "00000000-0000-4000-8000-000000000001",
            "2026-07-04T01:00:00Z",
        );
        store.create(item.clone()).expect("create meeting");

        let result = store.delete_with_cleanup(
            &item.id,
            |record| {
                record.title = "prepared for deletion".to_string();
                Ok(())
            },
            |_id| anyhow::bail!("audio is locked"),
        );

        assert_eq!(result.unwrap_err().to_string(), "audio is locked");
        assert_eq!(store.get(&item.id).unwrap(), Some(item));
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn meeting_store_delete_with_cleanup_prepares_then_removes_record() {
        let tmp = temp_root("openless-meeting-store");
        let store = MeetingStore::new_for_path(tmp.join("meetings.json"));
        let item = record(
            "00000000-0000-4000-8000-000000000001",
            "2026-07-04T01:00:00Z",
        );
        store.create(item.clone()).expect("create meeting");
        let prepared = std::cell::Cell::new(false);

        let removed = store
            .delete_with_cleanup(
                &item.id,
                |record| {
                    prepared.set(true);
                    record.title = "prepared for deletion".to_string();
                    Ok(())
                },
                |_id| {
                    assert!(prepared.get());
                    Ok(())
                },
            )
            .expect("delete meeting")
            .expect("removed meeting");

        assert_eq!(removed.title, "prepared for deletion");
        assert!(store.get(&item.id).unwrap().is_none());
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn prune_meeting_audio_keeps_newest_n_and_preserves_text_records() {
        let tmp = temp_root("openless-meeting-audio");
        let ids = [
            "00000000-0000-4000-8000-000000000001",
            "00000000-0000-4000-8000-000000000002",
            "00000000-0000-4000-8000-000000000003",
        ];
        for id in ids {
            let _ = create_audio(&tmp, id);
        }
        let mut records = vec![
            retained_record(ids[0], "2026-07-04T01:00:00Z", Some("2026-07-04T01:30:00Z")),
            retained_record(ids[1], "2026-07-04T02:00:00Z", Some("2026-07-04T02:30:00Z")),
            retained_record(ids[2], "2026-07-04T03:00:00Z", Some("2026-07-04T03:30:00Z")),
        ];

        let pruned = prune_meeting_audio_with_path_resolver(&mut records, 2, |id| {
            Ok(temp_audio_path(&tmp, id))
        })
        .expect("prune audio");

        assert_eq!(pruned, 1);
        assert_eq!(records.len(), 3);
        assert_eq!(records[0].audio.state, MeetingAudioState::Pruned);
        assert_eq!(records[1].audio.state, MeetingAudioState::Retained);
        assert_eq!(records[2].audio.state, MeetingAudioState::Retained);
        assert!(!temp_audio_path(&tmp, ids[0]).exists());
        assert!(temp_audio_path(&tmp, ids[1]).exists());
        assert!(temp_audio_path(&tmp, ids[2]).exists());
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn prune_meeting_audio_with_zero_prunes_all_retained_audio() {
        let tmp = temp_root("openless-meeting-audio");
        let ids = [
            "00000000-0000-4000-8000-000000000011",
            "00000000-0000-4000-8000-000000000012",
        ];
        for id in ids {
            let _ = create_audio(&tmp, id);
        }
        let mut records = vec![
            retained_record(ids[0], "2026-07-04T01:00:00Z", None),
            retained_record(ids[1], "2026-07-04T02:00:00Z", None),
        ];

        let pruned = prune_meeting_audio_with_path_resolver(&mut records, 0, |id| {
            Ok(temp_audio_path(&tmp, id))
        })
        .expect("prune all audio");

        assert_eq!(pruned, 2);
        assert!(records.iter().all(|record| !record.audio.retained));
        assert!(records.iter().all(|record| record.audio.path.is_none()));
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn prune_meeting_audio_with_zero_skips_processing_hold() {
        let tmp = temp_root("openless-meeting-audio-hold");
        let held_id = "00000000-0000-4000-8000-000000000013";
        let unheld_id = "00000000-0000-4000-8000-000000000014";
        let _ = create_audio(&tmp, held_id);
        let _ = create_audio(&tmp, unheld_id);
        let mut held = retained_record(held_id, "2026-07-04T01:00:00Z", None);
        held.processing_hold = Some(crate::types::ProcessingHold {
            job_id: "job-1".to_string(),
            acquired_at: "2026-07-04T01:30:00Z".to_string(),
        });
        let mut records = vec![
            held,
            retained_record(unheld_id, "2026-07-04T02:00:00Z", None),
        ];

        let pruned = prune_meeting_audio_with_path_resolver(&mut records, 0, |id| {
            Ok(temp_audio_path(&tmp, id))
        })
        .expect("prune unheld audio");

        assert_eq!(pruned, 1);
        assert_eq!(records[0].audio.state, MeetingAudioState::Retained);
        assert!(temp_audio_path(&tmp, held_id).exists());
        assert_eq!(records[1].audio.state, MeetingAudioState::Pruned);
        assert!(!temp_audio_path(&tmp, unheld_id).exists());
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn prune_meeting_audio_deletes_retained_audio_directory() {
        let tmp = temp_root("openless-meeting-audio-dir");
        let id = "00000000-0000-4000-8000-000000000031";
        let dir = tmp.join(id);
        fs::create_dir_all(&dir).expect("create audio dir");
        fs::write(dir.join("part-0001.wav"), b"wav").expect("write part");
        let mut records = vec![retained_record(
            id,
            "2026-07-04T01:00:00Z",
            Some("2026-07-04T01:30:00Z"),
        )];

        let pruned = prune_meeting_audio_with_path_resolver(&mut records, 0, |_id| Ok(dir.clone()))
            .expect("prune audio dir");

        assert_eq!(pruned, 1);
        assert!(!dir.exists());
        assert_eq!(records[0].audio.state, MeetingAudioState::Pruned);
        assert!(!records[0].audio.retained);
        assert_eq!(records[0].audio.path, None);
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn meeting_audio_existing_path_prefers_segment_directory_over_legacy_file() {
        let tmp = temp_root("openless-meeting-existing-path");
        let id = "00000000-0000-4000-8000-000000000041";
        let dir = tmp.join("meeting-recordings").join(id);
        let file = tmp.join("meeting-recordings").join(format!("{id}.wav"));
        fs::create_dir_all(&dir).expect("create dir");
        fs::write(&file, b"legacy").expect("write legacy");

        let chosen = meeting_recording_existing_path_for_id_with_resolver(
            id,
            |_id| Ok(dir.clone()),
            |_id| Ok(file.clone()),
        )
        .expect("resolve existing path");

        assert_eq!(chosen, dir);
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn prune_meeting_audio_clamps_retention_to_one_hundred() {
        let tmp = temp_root("openless-meeting-audio");
        let ids = [
            "00000000-0000-4000-8000-000000000021",
            "00000000-0000-4000-8000-000000000022",
        ];
        for id in ids {
            let _ = create_audio(&tmp, id);
        }
        let mut records = vec![
            retained_record(ids[0], "2026-07-04T01:00:00Z", None),
            retained_record(ids[1], "2026-07-04T02:00:00Z", None),
        ];

        let pruned = prune_meeting_audio_with_path_resolver(&mut records, 150, |id| {
            Ok(temp_audio_path(&tmp, id))
        })
        .expect("clamp retention");

        assert_eq!(pruned, 0);
        assert!(records
            .iter()
            .all(|record| record.audio.state == MeetingAudioState::Retained));
        let _ = fs::remove_dir_all(&tmp);
    }
}

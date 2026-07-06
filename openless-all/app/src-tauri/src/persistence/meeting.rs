#![cfg_attr(target_os = "linux", allow(dead_code, unused_variables))]
//! Meeting record store: newest-first JSON list plus retained audio pruning.

use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use parking_lot::Mutex;

use super::{
    atomic_write, data_dir, ensure_dir, meeting_recording_dir_path_for_id,
    meeting_recording_path_for_id, read_or_default,
};
use crate::types::{clamp_meeting_audio_retention_count, MeetingAudioState, MeetingRecord};

const MEETINGS_FILE: &str = "meetings.json";

pub struct MeetingStore {
    path: PathBuf,
    lock: Mutex<()>,
}

impl MeetingStore {
    pub fn new() -> Result<Self> {
        let dir = data_dir()?;
        ensure_dir(&dir)?;
        Ok(Self {
            path: dir.join(MEETINGS_FILE),
            lock: Mutex::new(()),
        })
    }

    #[allow(dead_code)]
    pub(crate) fn new_fallback() -> Self {
        Self {
            path: std::env::temp_dir().join("openless_meetings_fallback.json"),
            lock: Mutex::new(()),
        }
    }

    #[cfg(test)]
    fn new_for_path(path: PathBuf) -> Self {
        Self {
            path,
            lock: Mutex::new(()),
        }
    }

    pub fn list(&self) -> Result<Vec<MeetingRecord>> {
        let _guard = self.lock.lock();
        self.read_locked()
    }

    pub fn get(&self, id: &str) -> Result<Option<MeetingRecord>> {
        let _guard = self.lock.lock();
        Ok(self
            .read_locked()?
            .into_iter()
            .find(|record| record.id == id))
    }

    pub fn create(&self, record: MeetingRecord) -> Result<MeetingRecord> {
        let _guard = self.lock.lock();
        let mut records = self.read_locked()?;
        records.retain(|existing| existing.id != record.id);
        records.insert(0, record.clone());
        self.write_locked(&records)?;
        Ok(record)
    }

    pub fn update(&self, record: MeetingRecord) -> Result<Option<MeetingRecord>> {
        let _guard = self.lock.lock();
        let mut records = self.read_locked()?;
        let Some(slot) = records.iter_mut().find(|existing| existing.id == record.id) else {
            return Ok(None);
        };
        *slot = record.clone();
        self.write_locked(&records)?;
        Ok(Some(record))
    }

    pub fn delete(&self, id: &str) -> Result<Option<MeetingRecord>> {
        let _guard = self.lock.lock();
        let mut records = self.read_locked()?;
        let Some(index) = records.iter().position(|record| record.id == id) else {
            return Ok(None);
        };
        let removed = records.remove(index);
        self.write_locked(&records)?;
        Ok(Some(removed))
    }

    pub fn prune_audio_retention(&self, retention_count: u32) -> Result<usize> {
        let _guard = self.lock.lock();
        let mut records = self.read_locked()?;
        let pruned = prune_meeting_audio(&mut records, retention_count)?;
        if pruned > 0 {
            self.write_locked(&records)?;
        }
        Ok(pruned)
    }

    fn read_locked(&self) -> Result<Vec<MeetingRecord>> {
        read_or_default::<Vec<MeetingRecord>>(&self.path)
    }

    fn write_locked(&self, records: &[MeetingRecord]) -> Result<()> {
        let json = serde_json::to_vec_pretty(records).context("encode meetings failed")?;
        atomic_write(&self.path, &json)
    }
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
    use crate::types::{MeetingAudioMeta, MeetingStatus, MeetingSummary};

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
    fn meeting_store_delete_returns_removed_record() {
        let tmp = temp_root("openless-meeting-store");
        let store = MeetingStore::new_for_path(tmp.join("meetings.json"));
        let item = record(
            "00000000-0000-4000-8000-000000000001",
            "2026-07-04T01:00:00Z",
        );
        store.create(item.clone()).expect("create meeting");

        let removed = store.delete(&item.id).expect("delete meeting");

        assert_eq!(removed, Some(item));
        assert!(store.list().expect("list meetings").is_empty());
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

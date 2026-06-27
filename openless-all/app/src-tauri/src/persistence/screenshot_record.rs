//! Screenshot record history: independent records created by the screenshot-record hotkey.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use parking_lot::Mutex;

use super::{atomic_write, data_dir, ensure_dir, read_or_default, HISTORY_CAP};
use crate::types::{
    ContextAnalysisResult, ContextCaptureEntry, ScreenshotRecord, ScreenshotRecordStatus,
    HISTORY_MAX_ENTRIES_DEFAULT,
};

const SCREENSHOT_RECORD_FILE: &str = "screenshot-records.json";

pub struct ScreenshotRecordStore {
    path: PathBuf,
    lock: Arc<Mutex<()>>,
}

impl Clone for ScreenshotRecordStore {
    fn clone(&self) -> Self {
        Self {
            path: self.path.clone(),
            lock: Arc::clone(&self.lock),
        }
    }
}

impl ScreenshotRecordStore {
    pub fn new() -> Result<Self> {
        let dir = data_dir()?;
        ensure_dir(&dir)?;
        Ok(Self {
            path: dir.join(SCREENSHOT_RECORD_FILE),
            lock: Arc::new(Mutex::new(())),
        })
    }

    pub(crate) fn new_fallback() -> Self {
        Self {
            path: std::env::temp_dir().join("openless_screenshot_records_fallback.json"),
            lock: Arc::new(Mutex::new(())),
        }
    }

    #[cfg(test)]
    fn new_test(name: &str) -> Self {
        Self {
            path: std::env::temp_dir().join(format!(
                "openless_screenshot_records_{name}_{}.json",
                uuid::Uuid::new_v4()
            )),
            lock: Arc::new(Mutex::new(())),
        }
    }

    pub fn list(&self) -> Result<Vec<ScreenshotRecord>> {
        let _guard = self.lock.lock();
        self.read_locked()
    }

    pub fn upsert_with_retention(
        &self,
        record: ScreenshotRecord,
        retention_days: u32,
        max_entries: Option<u32>,
    ) -> Result<()> {
        let _guard = self.lock.lock();
        let mut records = self.read_locked()?;
        records.retain(|entry| entry.id != record.id);
        records.insert(0, record);
        apply_record_retention(&mut records, retention_days, max_entries);
        self.write_locked(&records)
    }

    pub fn update_analysis_if_generation_newer(
        &self,
        record_id: &str,
        status: ScreenshotRecordStatus,
        analysis: ContextAnalysisResult,
    ) -> Result<bool> {
        let _guard = self.lock.lock();
        let mut records = self.read_locked()?;
        let mut updated = false;
        if let Some(record) = records.iter_mut().find(|entry| entry.id == record_id) {
            let current_generation = record
                .analysis
                .as_ref()
                .and_then(|analysis| analysis.analysis_generation.as_deref());
            let next_generation = analysis.analysis_generation.as_deref();
            if generation_is_newer_or_equal(current_generation, next_generation) {
                return Ok(false);
            }
            record.status = status;
            record.analysis = Some(analysis);
            record.error_code = None;
            record.error_message = None;
            record.updated_at = chrono::Utc::now().to_rfc3339();
            updated = true;
        }
        self.write_locked(&records)?;
        Ok(updated)
    }

    pub fn append_capture_result(
        &self,
        record_id: &str,
        context: &ContextCaptureEntry,
        retention_days: u32,
        max_entries: Option<u32>,
    ) -> Result<bool> {
        let _guard = self.lock.lock();
        let mut records = self.read_locked()?;
        let mut updated = false;
        if let Some(record) = records.iter_mut().find(|entry| entry.id == record_id) {
            if record.status != ScreenshotRecordStatus::Collecting {
                return Ok(false);
            }
            record.updated_at = chrono::Utc::now().to_rfc3339();
            record.trigger_count = record.trigger_count.saturating_add(1);
            if context.screenshot_ref.is_some()
                && !record.screenshot_ids.iter().any(|id| id == &context.id)
            {
                record.screenshot_ids.push(context.id.clone());
            }
            if record.context_app.is_none() {
                record.context_app = context.context_app.clone();
            }
            if record.conversation_window.is_none() {
                record.conversation_window = context.conversation_window.clone();
            }
            if record.window_title.is_none() {
                record.window_title = context.window_title.clone();
            }
            if context.screenshot_ref.is_none() {
                record.error_code = context.error_code.clone();
            }
            updated = true;
        }
        apply_record_retention(&mut records, retention_days, max_entries);
        self.write_locked(&records)?;
        Ok(updated)
    }

    pub fn update_analysis_if_current(
        &self,
        record_id: &str,
        expected_prompt_hash: Option<&str>,
        expected_generation: Option<&str>,
        status: ScreenshotRecordStatus,
        analysis: Option<ContextAnalysisResult>,
        error_code: Option<String>,
        error_message: Option<String>,
    ) -> Result<bool> {
        let _guard = self.lock.lock();
        let mut records = self.read_locked()?;
        let mut updated = false;
        if let Some(record) = records.iter_mut().find(|entry| entry.id == record_id) {
            let current_hash = record
                .analysis
                .as_ref()
                .and_then(|analysis| analysis.prompt_hash.as_deref());
            if current_hash != expected_prompt_hash {
                return Ok(false);
            }
            let current_generation = record
                .analysis
                .as_ref()
                .and_then(|analysis| analysis.analysis_generation.as_deref());
            if current_generation != expected_generation {
                return Ok(false);
            }
            record.status = status;
            record.analysis = analysis;
            record.error_code = error_code;
            record.error_message = error_message;
            record.updated_at = chrono::Utc::now().to_rfc3339();
            updated = true;
        }
        self.write_locked(&records)?;
        Ok(updated)
    }

    pub fn analysis_is_current(
        &self,
        record_id: &str,
        expected_prompt_hash: &str,
        expected_generation: &str,
        expected_status: ScreenshotRecordStatus,
    ) -> Result<bool> {
        let _guard = self.lock.lock();
        Ok(self
            .read_locked()?
            .into_iter()
            .find(|entry| entry.id == record_id)
            .map(|record| {
                let current_hash = record
                    .analysis
                    .as_ref()
                    .and_then(|analysis| analysis.prompt_hash.as_deref());
                let current_generation = record
                    .analysis
                    .as_ref()
                    .and_then(|analysis| analysis.analysis_generation.as_deref());
                record.status == expected_status
                    && current_hash == Some(expected_prompt_hash)
                    && current_generation == Some(expected_generation)
            })
            .unwrap_or(false))
    }

    pub fn delete(&self, id: &str) -> Result<()> {
        let _guard = self.lock.lock();
        let mut records = self.read_locked()?;
        records.retain(|entry| entry.id != id);
        self.write_locked(&records)
    }

    pub fn clear(&self) -> Result<()> {
        let _guard = self.lock.lock();
        self.write_locked(&[])
    }

    pub fn apply_retention(&self, retention_days: u32, max_entries: Option<u32>) -> Result<()> {
        let _guard = self.lock.lock();
        let mut records = self.read_locked()?;
        apply_record_retention(&mut records, retention_days, max_entries);
        self.write_locked(&records)
    }

    fn read_locked(&self) -> Result<Vec<ScreenshotRecord>> {
        read_or_default::<Vec<ScreenshotRecord>>(&self.path)
    }

    fn write_locked(&self, records: &[ScreenshotRecord]) -> Result<()> {
        let json = serde_json::to_vec_pretty(records).context("encode screenshot records failed")?;
        atomic_write(&self.path, &json)
    }
}

fn apply_record_retention(
    records: &mut Vec<ScreenshotRecord>,
    retention_days: u32,
    max_entries: Option<u32>,
) {
    if retention_days > 0 {
        let cutoff = chrono::Utc::now() - chrono::Duration::days(i64::from(retention_days));
        records.retain(|entry| {
            chrono::DateTime::parse_from_rfc3339(&entry.created_at)
                .map(|t| t.with_timezone(&chrono::Utc) >= cutoff)
                .unwrap_or(true)
        });
    }
    let cap = max_entries
        .map(|n| (n as usize).clamp(5, HISTORY_CAP))
        .unwrap_or(HISTORY_MAX_ENTRIES_DEFAULT as usize);
    if records.len() > cap {
        records.truncate(cap);
    }
}

fn generation_is_newer_or_equal(current: Option<&str>, next: Option<&str>) -> bool {
    match (current, next) {
        (Some(current), Some(next)) => current >= next,
        (Some(_), None) => true,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{
        ContextCaptureHistoryType, ContextCaptureSource, ContextCaptureStatus,
    };

    fn record(id: &str) -> ScreenshotRecord {
        ScreenshotRecord {
            id: id.into(),
            created_at: chrono::Utc::now().to_rfc3339(),
            updated_at: chrono::Utc::now().to_rfc3339(),
            window_started_at: chrono::Utc::now().to_rfc3339(),
            window_ended_at: None,
            status: ScreenshotRecordStatus::Collecting,
            context_app: Some("Notepad".into()),
            conversation_window: Some("note.txt".into()),
            window_title: Some("note.txt - Notepad".into()),
            screenshot_ids: Vec::new(),
            submitted_screenshot_ids: Vec::new(),
            trigger_count: 1,
            error_code: None,
            error_message: None,
            analysis: None,
        }
    }

    fn context(id: &str, history_id: &str) -> ContextCaptureEntry {
        ContextCaptureEntry {
            id: id.into(),
            created_at: chrono::Utc::now().to_rfc3339(),
            context_app: Some("Notepad".into()),
            conversation_window: Some("note.txt".into()),
            window_title: Some("note.txt - Notepad".into()),
            capture_status: ContextCaptureStatus::Success,
            capture_source: Some(ContextCaptureSource::ActiveWindow),
            screenshot_path: None,
            screenshot_ref: Some(format!("{id}.bmp")),
            linked_history_type: ContextCaptureHistoryType::ScreenshotRecord,
            linked_history_id: history_id.into(),
            error_code: None,
            analysis: None,
        }
    }

    fn analysis(record_id: &str, generation: &str) -> ContextAnalysisResult {
        let ctx = context("ctx-analysis", record_id);
        let mut result = crate::persistence::pending_analysis_result(&ctx);
        result.prompt_hash = Some("hash".into());
        result.analysis_generation = Some(generation.into());
        result
    }

    #[test]
    fn upsert_replaces_same_record() {
        let store = ScreenshotRecordStore::new_test("upsert");
        store.upsert_with_retention(record("a"), 0, None).unwrap();
        let mut next = record("a");
        next.trigger_count = 2;
        store.upsert_with_retention(next, 0, None).unwrap();
        let records = store.list().unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].trigger_count, 2);
    }

    #[test]
    fn append_capture_result_preserves_existing_screenshots_and_counts() {
        let store = ScreenshotRecordStore::new_test("append");
        let mut base = record("capture-append");
        base.trigger_count = 0;
        store.upsert_with_retention(base, 0, None).unwrap();

        store
            .append_capture_result("capture-append", &context("ctx-1", "capture-append"), 0, None)
            .unwrap();
        store
            .append_capture_result("capture-append", &context("ctx-2", "capture-append"), 0, None)
            .unwrap();

        let records = store.list().unwrap();
        assert_eq!(records[0].trigger_count, 2);
        assert_eq!(records[0].screenshot_ids, vec!["ctx-1", "ctx-2"]);
    }

    #[test]
    fn append_capture_result_ignores_closed_record() {
        let store = ScreenshotRecordStore::new_test("append-closed");
        let mut base = record("capture-append-closed");
        base.status = ScreenshotRecordStatus::Queued;
        base.screenshot_ids = vec!["ctx-old".into()];
        base.trigger_count = 0;
        store.upsert_with_retention(base, 0, None).unwrap();

        let changed = store
            .append_capture_result(
                "capture-append-closed",
                &context("ctx-new", "capture-append-closed"),
                0,
                None,
            )
            .unwrap();

        let records = store.list().unwrap();
        assert!(!changed);
        assert_eq!(records[0].trigger_count, 0);
        assert_eq!(records[0].screenshot_ids, vec!["ctx-old"]);
    }

    #[test]
    fn analysis_pending_only_moves_forward_by_generation() {
        let store = ScreenshotRecordStore::new_test("generation");
        store.upsert_with_retention(record("record-analysis"), 0, None).unwrap();

        assert!(store
            .update_analysis_if_generation_newer(
                "record-analysis",
                ScreenshotRecordStatus::Analyzing,
                analysis("record-analysis", "001"),
            )
            .unwrap());
        assert!(store
            .update_analysis_if_generation_newer(
                "record-analysis",
                ScreenshotRecordStatus::Analyzing,
                analysis("record-analysis", "002"),
            )
            .unwrap());
        assert!(!store
            .update_analysis_if_generation_newer(
                "record-analysis",
                ScreenshotRecordStatus::Analyzing,
                analysis("record-analysis", "001"),
            )
            .unwrap());

        let records = store.list().unwrap();
        assert_eq!(
            records[0]
                .analysis
                .as_ref()
                .and_then(|entry| entry.analysis_generation.as_deref()),
            Some("002")
        );
    }

    #[test]
    fn final_analysis_requires_matching_generation() {
        let store = ScreenshotRecordStore::new_test("final");
        store.upsert_with_retention(record("record-final"), 0, None).unwrap();
        store
            .update_analysis_if_generation_newer(
                "record-final",
                ScreenshotRecordStatus::Analyzing,
                analysis("record-final", "002"),
            )
            .unwrap();

        assert!(!store
            .update_analysis_if_current(
                "record-final",
                Some("hash"),
                Some("001"),
                ScreenshotRecordStatus::Success,
                Some(analysis("record-final", "001")),
                None,
                None,
            )
            .unwrap());
        assert!(store
            .update_analysis_if_current(
                "record-final",
                Some("hash"),
                Some("002"),
                ScreenshotRecordStatus::Success,
                Some(analysis("record-final", "002")),
                None,
                None,
            )
            .unwrap());
    }

    #[test]
    fn analysis_is_current_requires_matching_status_hash_and_generation() {
        let store = ScreenshotRecordStore::new_test("current");
        store.upsert_with_retention(record("record-current"), 0, None).unwrap();
        store
            .update_analysis_if_generation_newer(
                "record-current",
                ScreenshotRecordStatus::Analyzing,
                analysis("record-current", "002"),
            )
            .unwrap();

        assert!(store
            .analysis_is_current(
                "record-current",
                "hash",
                "002",
                ScreenshotRecordStatus::Analyzing,
            )
            .unwrap());
        assert!(!store
            .analysis_is_current(
                "record-current",
                "hash",
                "001",
                ScreenshotRecordStatus::Analyzing,
            )
            .unwrap());
        assert!(!store
            .analysis_is_current(
                "record-current",
                "hash",
                "002",
                ScreenshotRecordStatus::Success,
            )
            .unwrap());
    }
}

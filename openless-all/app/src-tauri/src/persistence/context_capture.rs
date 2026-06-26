//! Context capture store: independent metadata plus screenshot files.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use parking_lot::Mutex;

use super::{atomic_write, data_dir, ensure_dir, read_or_default, HISTORY_CAP};
use crate::types::{ContextCaptureEntry, ContextCaptureHistoryType};

const CONTEXT_CAPTURE_FILE: &str = "context-capture.json";
const CONTEXT_SCREENSHOT_DIR: &str = "context-screenshots";

pub struct ContextCaptureStore {
    path: PathBuf,
    screenshot_dir: PathBuf,
    lock: Arc<Mutex<()>>,
}

impl Clone for ContextCaptureStore {
    fn clone(&self) -> Self {
        Self {
            path: self.path.clone(),
            screenshot_dir: self.screenshot_dir.clone(),
            lock: Arc::clone(&self.lock),
        }
    }
}

impl ContextCaptureStore {
    pub fn new() -> Result<Self> {
        let dir = data_dir()?;
        ensure_dir(&dir)?;
        let screenshot_dir = dir.join(CONTEXT_SCREENSHOT_DIR);
        ensure_dir(&screenshot_dir)?;
        Ok(Self {
            path: dir.join(CONTEXT_CAPTURE_FILE),
            screenshot_dir,
            lock: Arc::new(Mutex::new(())),
        })
    }

    pub(crate) fn new_fallback() -> Self {
        let dir = std::env::temp_dir().join("openless_context_capture_fallback");
        let _ = ensure_dir(&dir);
        Self {
            path: dir.join(CONTEXT_CAPTURE_FILE),
            screenshot_dir: dir.join(CONTEXT_SCREENSHOT_DIR),
            lock: Arc::new(Mutex::new(())),
        }
    }

    pub fn list(&self) -> Result<Vec<ContextCaptureEntry>> {
        let _guard = self.lock.lock();
        self.read_locked()
    }

    pub fn append_with_retention(
        &self,
        entry: ContextCaptureEntry,
        retention_days: u32,
        max_entries: Option<u32>,
    ) -> Result<()> {
        let _guard = self.lock.lock();
        let mut entries = self.read_locked()?;
        entries.insert(0, entry);
        self.apply_retention_locked(&mut entries, retention_days, max_entries)?;
        self.write_locked(&entries)
    }

    pub fn latest_for_history(
        &self,
        history_type: ContextCaptureHistoryType,
        history_id: &str,
    ) -> Result<Option<ContextCaptureEntry>> {
        let _guard = self.lock.lock();
        Ok(self.read_locked()?.into_iter().find(|entry| {
            entry.linked_history_type == history_type && entry.linked_history_id == history_id
        }))
    }

    pub fn screenshot_path_for_id(&self, id: &str) -> PathBuf {
        self.screenshot_dir.join(format!("{id}.bmp"))
    }

    pub fn screenshot_path_for_ref(&self, screenshot_ref: &str) -> Result<PathBuf> {
        let path = self.screenshot_dir.join(screenshot_ref);
        if path.parent() != Some(self.screenshot_dir.as_path()) {
            anyhow::bail!("invalid screenshot reference");
        }
        Ok(path)
    }

    pub fn read_screenshot(&self, id: &str) -> Result<Vec<u8>> {
        let _guard = self.lock.lock();
        let entries = self.read_locked()?;
        let entry = entries
            .iter()
            .find(|entry| entry.id == id)
            .ok_or_else(|| anyhow::anyhow!("context capture not found"))?;
        let Some(screenshot_ref) = entry.screenshot_ref.as_deref() else {
            anyhow::bail!("screenshot not found");
        };
        let path = self.screenshot_dir.join(screenshot_ref);
        if path.parent() != Some(self.screenshot_dir.as_path()) {
            anyhow::bail!("invalid screenshot reference");
        }
        std::fs::read(&path).with_context(|| format!("read screenshot failed: {}", path.display()))
    }

    pub fn delete_for_history(
        &self,
        history_type: ContextCaptureHistoryType,
        history_id: &str,
    ) -> Result<()> {
        let _guard = self.lock.lock();
        let mut entries = self.read_locked()?;
        entries.retain(|entry| {
            !(entry.linked_history_type == history_type && entry.linked_history_id == history_id)
        });
        self.prune_screenshots_locked(&entries)?;
        self.write_locked(&entries)
    }

    pub fn clear_for_history_type(&self, history_type: ContextCaptureHistoryType) -> Result<()> {
        let _guard = self.lock.lock();
        let mut entries = self.read_locked()?;
        entries.retain(|entry| entry.linked_history_type != history_type);
        self.prune_screenshots_locked(&entries)?;
        self.write_locked(&entries)
    }

    fn apply_retention_locked(
        &self,
        entries: &mut Vec<ContextCaptureEntry>,
        retention_days: u32,
        max_entries: Option<u32>,
    ) -> Result<()> {
        if retention_days > 0 {
            let cutoff = chrono::Utc::now() - chrono::Duration::days(i64::from(retention_days));
            entries.retain(|entry| {
                chrono::DateTime::parse_from_rfc3339(&entry.created_at)
                    .map(|t| t.with_timezone(&chrono::Utc) >= cutoff)
                    .unwrap_or(true)
            });
        }
        let cap = max_entries
            .map(|n| (n as usize).clamp(5, HISTORY_CAP))
            .unwrap_or(HISTORY_CAP);
        if entries.len() > cap {
            entries.truncate(cap);
        }
        self.prune_screenshots_locked(entries)?;
        Ok(())
    }

    fn prune_screenshots_locked(&self, entries: &[ContextCaptureEntry]) -> Result<()> {
        ensure_dir(&self.screenshot_dir)?;
        let keep: HashSet<String> = entries
            .iter()
            .filter_map(|entry| entry.screenshot_ref.clone())
            .collect();
        for item in std::fs::read_dir(&self.screenshot_dir)
            .with_context(|| format!("read dir failed: {}", self.screenshot_dir.display()))?
        {
            let item = item?;
            let path = item.path();
            if !path.is_file() {
                continue;
            }
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if !keep.contains(name) {
                let _ = std::fs::remove_file(path);
            }
        }
        Ok(())
    }

    fn read_locked(&self) -> Result<Vec<ContextCaptureEntry>> {
        let mut entries = read_or_default::<Vec<ContextCaptureEntry>>(&self.path)?;
        for entry in &mut entries {
            entry.screenshot_path = entry.screenshot_ref.as_ref().map(|name| {
                self.screenshot_dir
                    .join(name)
                    .to_string_lossy()
                    .into_owned()
            });
        }
        Ok(entries)
    }

    fn write_locked(&self, entries: &[ContextCaptureEntry]) -> Result<()> {
        let json = serde_json::to_vec_pretty(entries).context("encode context capture failed")?;
        atomic_write(&self.path, &json)
    }
}

pub fn enrich_voice_history_with_context(
    sessions: &mut [crate::types::DictationSession],
    context_entries: &[ContextCaptureEntry],
) {
    for session in sessions {
        session.context_capture = context_entries
            .iter()
            .find(|entry| {
                entry.linked_history_type == ContextCaptureHistoryType::Voice
                    && entry.linked_history_id == session.id
            })
            .cloned();
    }
}

pub fn enrich_rewrite_history_with_context(
    entries: &mut [crate::types::RewriteHistoryEntry],
    context_entries: &[ContextCaptureEntry],
) {
    for rewrite in entries {
        rewrite.context_capture = context_entries
            .iter()
            .find(|entry| {
                entry.linked_history_type == ContextCaptureHistoryType::Rewrite
                    && entry.linked_history_id == rewrite.id
            })
            .cloned();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ContextCaptureSource, ContextCaptureStatus};

    fn entry(id: &str, linked_history_id: &str) -> ContextCaptureEntry {
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
            linked_history_type: ContextCaptureHistoryType::Voice,
            linked_history_id: linked_history_id.into(),
            error_code: None,
            analysis: None,
        }
    }

    #[test]
    fn append_then_lookup_by_history_id() {
        let store = ContextCaptureStore::new_fallback();
        let _ = store.append_with_retention(entry("ctx-a", "hist-a"), 0, None);
        let found = store
            .latest_for_history(ContextCaptureHistoryType::Voice, "hist-a")
            .unwrap();
        assert_eq!(found.unwrap().id, "ctx-a");
    }

    #[test]
    fn old_entries_without_context_do_not_break_enrichment() {
        let context = vec![entry("ctx-a", "hist-a")];
        let mut sessions = vec![crate::types::DictationSession {
            id: "hist-a".into(),
            created_at: chrono::Utc::now().to_rfc3339(),
            raw_transcript: "raw".into(),
            final_text: "final".into(),
            mode: crate::types::PolishMode::Structured,
            style_pack_id: None,
            translation_active: false,
            polish_source: None,
            app_bundle_id: None,
            app_name: None,
            insert_status: crate::types::InsertStatus::Inserted,
            error_code: None,
            duration_ms: None,
            dictionary_entry_count: None,
            has_audio_recording: None,
            context_capture: None,
        }];
        enrich_voice_history_with_context(&mut sessions, &context);
        assert_eq!(sessions[0].context_capture.as_ref().unwrap().id, "ctx-a");
    }
}

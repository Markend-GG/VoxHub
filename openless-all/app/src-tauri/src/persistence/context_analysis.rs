//! Context vision analysis store: independent LLM analysis results.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use parking_lot::Mutex;

use super::{atomic_write, data_dir, ensure_dir, read_or_default, HISTORY_CAP};
use crate::types::{
    ContextAnalysisResult, ContextAnalysisStatus, ContextCaptureEntry, ContextCaptureHistoryType,
};

const CONTEXT_ANALYSIS_FILE: &str = "context-analysis.json";

pub struct ContextAnalysisStore {
    path: PathBuf,
    lock: Arc<Mutex<()>>,
}

impl Clone for ContextAnalysisStore {
    fn clone(&self) -> Self {
        Self {
            path: self.path.clone(),
            lock: Arc::clone(&self.lock),
        }
    }
}

impl ContextAnalysisStore {
    pub fn new() -> Result<Self> {
        let dir = data_dir()?;
        ensure_dir(&dir)?;
        Ok(Self {
            path: dir.join(CONTEXT_ANALYSIS_FILE),
            lock: Arc::new(Mutex::new(())),
        })
    }

    pub(crate) fn new_fallback() -> Self {
        Self {
            path: std::env::temp_dir().join("openless_context_analysis_fallback.json"),
            lock: Arc::new(Mutex::new(())),
        }
    }

    pub fn list(&self) -> Result<Vec<ContextAnalysisResult>> {
        let _guard = self.lock.lock();
        self.read_locked()
    }

    pub fn upsert(&self, result: ContextAnalysisResult) -> Result<()> {
        let _guard = self.lock.lock();
        let mut entries = self.read_locked()?;
        entries.retain(|entry| {
            !(entry.linked_history_type == result.linked_history_type
                && entry.linked_history_id == result.linked_history_id
                && entry.context_capture_id == result.context_capture_id)
        });
        entries.insert(0, result);
        if entries.len() > HISTORY_CAP {
            entries.truncate(HISTORY_CAP);
        }
        self.write_locked(&entries)
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
        self.write_locked(&entries)
    }

    pub fn clear_for_history_type(&self, history_type: ContextCaptureHistoryType) -> Result<()> {
        let _guard = self.lock.lock();
        let mut entries = self.read_locked()?;
        entries.retain(|entry| entry.linked_history_type != history_type);
        self.write_locked(&entries)
    }

    pub fn apply_retention(
        &self,
        retention_days: u32,
        max_entries: Option<u32>,
    ) -> Result<()> {
        let _guard = self.lock.lock();
        let mut entries = self.read_locked()?;
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
        self.write_locked(&entries)
    }

    fn read_locked(&self) -> Result<Vec<ContextAnalysisResult>> {
        read_or_default::<Vec<ContextAnalysisResult>>(&self.path)
    }

    fn write_locked(&self, entries: &[ContextAnalysisResult]) -> Result<()> {
        let json = serde_json::to_vec_pretty(entries).context("encode context analysis failed")?;
        atomic_write(&self.path, &json)
    }
}

pub fn enrich_context_entries_with_analysis(
    contexts: &mut [ContextCaptureEntry],
    analysis_entries: &[ContextAnalysisResult],
) {
    for context in contexts {
        context.analysis = analysis_entries
            .iter()
            .find(|entry| {
                entry.context_capture_id == context.id
                    && entry.linked_history_type == context.linked_history_type
                    && entry.linked_history_id == context.linked_history_id
            })
            .cloned();
    }
}

pub fn pending_analysis_result(context: &ContextCaptureEntry) -> ContextAnalysisResult {
    ContextAnalysisResult {
        id: uuid::Uuid::new_v4().to_string(),
        context_capture_id: context.id.clone(),
        linked_history_type: context.linked_history_type,
        linked_history_id: context.linked_history_id.clone(),
        status: ContextAnalysisStatus::Pending,
        created_at: chrono::Utc::now().to_rfc3339(),
        analyzed_at: None,
        provider_id: None,
        model: None,
        prompt_version: crate::context_vision_analysis::PROMPT_VERSION.to_string(),
        schema_version: 1,
        input_mode: "screenshot_text".to_string(),
        image_mime_type: None,
        image_width: None,
        image_height: None,
        image_bytes: None,
        conversation_name: None,
        brief_summary: None,
        full_summary: None,
        detected_app: None,
        detected_context_type: Default::default(),
        topic: None,
        user_intent: None,
        activity_type: Default::default(),
        decision: None,
        action_items: Vec::new(),
        related_people: Vec::new(),
        project_or_domain: None,
        visual_evidence: Vec::new(),
        sensitive_content_visible: false,
        confidence: 0.0,
        uncertainty_reason: None,
        error_code: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ContextCaptureSource, ContextCaptureStatus};

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
            linked_history_type: ContextCaptureHistoryType::Voice,
            linked_history_id: history_id.into(),
            error_code: None,
            analysis: None,
        }
    }

    #[test]
    fn upsert_replaces_existing_analysis_for_same_context() {
        let store = ContextAnalysisStore::new_fallback();
        let _ = store.clear_for_history_type(ContextCaptureHistoryType::Voice);
        let ctx = context("ctx-a", "hist-a");
        let mut first = pending_analysis_result(&ctx);
        first.status = ContextAnalysisStatus::Skipped;
        first.error_code = Some("skipped:modelNotConfigured".into());
        let mut second = pending_analysis_result(&ctx);
        second.status = ContextAnalysisStatus::Failed;
        second.error_code = Some("failed:imagePrepareFailed".into());

        store.upsert(first).unwrap();
        store.upsert(second).unwrap();

        let list = store.list().unwrap();
        let matches: Vec<_> = list
            .into_iter()
            .filter(|entry| entry.context_capture_id == "ctx-a")
            .collect();
        assert_eq!(matches.len(), 1);
        assert_eq!(
            matches[0].error_code.as_deref(),
            Some("failed:imagePrepareFailed")
        );
    }

    #[test]
    fn enrich_contexts_attaches_matching_analysis() {
        let ctx = context("ctx-a", "hist-a");
        let result = pending_analysis_result(&ctx);
        let mut contexts = vec![ctx];
        enrich_context_entries_with_analysis(&mut contexts, &[result]);
        assert!(contexts[0].analysis.is_some());
    }
}

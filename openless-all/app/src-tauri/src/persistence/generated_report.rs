//! Generated report history.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use parking_lot::Mutex;

use super::{atomic_write, data_dir, ensure_dir, read_or_default, HISTORY_CAP};
use crate::types::GeneratedReport;

const GENERATED_REPORT_FILE: &str = "generated-reports.json";

pub struct GeneratedReportStore {
    path: PathBuf,
    lock: Arc<Mutex<()>>,
}

impl Clone for GeneratedReportStore {
    fn clone(&self) -> Self {
        Self {
            path: self.path.clone(),
            lock: Arc::clone(&self.lock),
        }
    }
}

impl GeneratedReportStore {
    pub fn new() -> Result<Self> {
        let dir = data_dir()?;
        ensure_dir(&dir)?;
        Ok(Self {
            path: dir.join(GENERATED_REPORT_FILE),
            lock: Arc::new(Mutex::new(())),
        })
    }

    pub(crate) fn new_fallback() -> Self {
        Self {
            path: std::env::temp_dir().join("openless_generated_reports_fallback.json"),
            lock: Arc::new(Mutex::new(())),
        }
    }

    pub fn list(&self) -> Result<Vec<GeneratedReport>> {
        let _guard = self.lock.lock();
        self.read_locked()
    }

    pub fn append(&self, report: GeneratedReport) -> Result<()> {
        let _guard = self.lock.lock();
        let mut reports = self.read_locked()?;
        reports.insert(0, report);
        if reports.len() > HISTORY_CAP {
            reports.truncate(HISTORY_CAP);
        }
        self.write_locked(&reports)
    }

    pub fn replace(&self, report: GeneratedReport) -> Result<()> {
        let _guard = self.lock.lock();
        let mut reports = self.read_locked()?;
        if let Some(existing) = reports.iter_mut().find(|entry| entry.id == report.id) {
            *existing = report;
        } else {
            reports.insert(0, report);
        }
        if reports.len() > HISTORY_CAP {
            reports.truncate(HISTORY_CAP);
        }
        self.write_locked(&reports)
    }

    pub fn get(&self, id: &str) -> Result<Option<GeneratedReport>> {
        let _guard = self.lock.lock();
        Ok(self.read_locked()?.into_iter().find(|entry| entry.id == id))
    }

    pub fn delete(&self, id: &str) -> Result<()> {
        let _guard = self.lock.lock();
        let mut reports = self.read_locked()?;
        reports.retain(|entry| entry.id != id);
        self.write_locked(&reports)
    }

    fn read_locked(&self) -> Result<Vec<GeneratedReport>> {
        read_or_default::<Vec<GeneratedReport>>(&self.path)
    }

    fn write_locked(&self, reports: &[GeneratedReport]) -> Result<()> {
        let json = serde_json::to_vec_pretty(reports).context("encode generated reports failed")?;
        atomic_write(&self.path, &json)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{GeneratedReportSourceStats, ReportGenerationStatus, ReportType};

    fn test_store() -> GeneratedReportStore {
        let path = std::env::temp_dir().join(format!(
            "openless_generated_reports_test_{}.json",
            uuid::Uuid::new_v4()
        ));
        GeneratedReportStore {
            path,
            lock: Arc::new(Mutex::new(())),
        }
    }

    fn report(id: &str, status: ReportGenerationStatus) -> GeneratedReport {
        GeneratedReport {
            id: id.into(),
            report_type: ReportType::Daily,
            title: "Daily".into(),
            range_start: "2026-06-27T00:00:00Z".into(),
            range_end: "2026-06-27T10:00:00Z".into(),
            template_id: "tpl".into(),
            template_name: "tpl".into(),
            template_content: "template".into(),
            user_main_work: None,
            status,
            content: None,
            source_stats: GeneratedReportSourceStats::default(),
            error_code: None,
            error_message: None,
            schedule_key: Some("daily:2026-06-27:tpl".into()),
            created_at: "2026-06-27T10:00:00Z".into(),
            updated_at: "2026-06-27T10:00:00Z".into(),
        }
    }

    #[test]
    fn replace_updates_existing_report_without_duplicating() {
        let store = test_store();
        store
            .append(report("report-1", ReportGenerationStatus::Pending))
            .unwrap();

        let mut completed = report("report-1", ReportGenerationStatus::Success);
        completed.content = Some("done".into());
        completed.updated_at = "2026-06-27T10:01:00Z".into();
        store.replace(completed).unwrap();

        let reports = store.list().unwrap();
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].id, "report-1");
        assert_eq!(reports[0].status, ReportGenerationStatus::Success);
        assert_eq!(reports[0].content.as_deref(), Some("done"));
    }
}

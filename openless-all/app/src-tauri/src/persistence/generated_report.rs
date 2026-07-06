//! Generated report history.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use parking_lot::Mutex;

use super::{atomic_write, data_dir, ensure_dir, read_or_default, HISTORY_CAP};
use crate::types::{GeneratedReport, ReportGenerationStatus};

const GENERATED_REPORT_FILE: &str = "generated-reports.json";
const STALE_PENDING_REPORT_AFTER_MINUTES: i64 = 30;

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

    pub fn recover_stale_pending_reports(&self) -> Result<usize> {
        self.recover_stale_pending_reports_at(
            chrono::Utc::now(),
            chrono::Duration::minutes(STALE_PENDING_REPORT_AFTER_MINUTES),
        )
    }

    pub fn recover_stale_pending_reports_at(
        &self,
        now: chrono::DateTime<chrono::Utc>,
        stale_after: chrono::Duration,
    ) -> Result<usize> {
        let _guard = self.lock.lock();
        let mut reports = self.read_locked()?;
        let threshold = now - stale_after;
        let now_text = now.to_rfc3339();
        let mut recovered = 0;
        for report in reports.iter_mut() {
            if report.status != ReportGenerationStatus::Pending {
                continue;
            }
            let timestamp = parse_report_timestamp(&report.updated_at)
                .or_else(|| parse_report_timestamp(&report.created_at));
            if timestamp.is_some_and(|value| value <= threshold) {
                report.status = ReportGenerationStatus::Failed;
                report.error_code = Some("failed:stalePending".into());
                report.error_message = Some("报告生成任务已中断，请重新生成。".into());
                report.updated_at = now_text.clone();
                recovered += 1;
            }
        }
        if recovered > 0 {
            self.write_locked(&reports)?;
        }
        Ok(recovered)
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

    /// 仅更新报告的 title、content 和 updatedAt。
    pub fn update(&self, id: &str, title: &str, content: &str) -> Result<bool> {
        let _guard = self.lock.lock();
        let mut reports = self.read_locked()?;
        let updated = if let Some(report) = reports.iter_mut().find(|entry| entry.id == id) {
            report.title = title.to_string();
            report.content = Some(content.to_string());
            report.updated_at = chrono::Utc::now().to_rfc3339();
            true
        } else {
            false
        };
        if updated {
            self.write_locked(&reports)?;
        }
        Ok(updated)
    }

    fn read_locked(&self) -> Result<Vec<GeneratedReport>> {
        read_or_default::<Vec<GeneratedReport>>(&self.path)
    }

    fn write_locked(&self, reports: &[GeneratedReport]) -> Result<()> {
        let json = serde_json::to_vec_pretty(reports).context("encode generated reports failed")?;
        atomic_write(&self.path, &json)
    }
}

fn parse_report_timestamp(value: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::parse_from_rfc3339(value)
        .map(|value| value.with_timezone(&chrono::Utc))
        .ok()
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

    #[test]
    fn stale_pending_reports_are_marked_failed() {
        let store = test_store();
        let mut stale = report("report-1", ReportGenerationStatus::Pending);
        stale.created_at = "2026-06-27T10:00:00Z".into();
        stale.updated_at = "2026-06-27T10:00:00Z".into();
        store.append(stale).unwrap();

        let recovered = store
            .recover_stale_pending_reports_at(
                chrono::DateTime::parse_from_rfc3339("2026-06-27T10:31:00Z")
                    .unwrap()
                    .with_timezone(&chrono::Utc),
                chrono::Duration::minutes(30),
            )
            .unwrap();

        let reports = store.list().unwrap();
        assert_eq!(recovered, 1);
        assert_eq!(reports[0].status, ReportGenerationStatus::Failed);
        assert_eq!(
            reports[0].error_code.as_deref(),
            Some("failed:stalePending")
        );
        assert_eq!(
            reports[0].error_message.as_deref(),
            Some("报告生成任务已中断，请重新生成。")
        );
        assert_eq!(reports[0].updated_at, "2026-06-27T10:31:00+00:00");
    }

    #[test]
    fn fresh_pending_reports_are_kept_pending() {
        let store = test_store();
        let mut fresh = report("report-1", ReportGenerationStatus::Pending);
        fresh.created_at = "2026-06-27T10:00:00Z".into();
        fresh.updated_at = "2026-06-27T10:20:00Z".into();
        store.append(fresh).unwrap();

        let recovered = store
            .recover_stale_pending_reports_at(
                chrono::DateTime::parse_from_rfc3339("2026-06-27T10:31:00Z")
                    .unwrap()
                    .with_timezone(&chrono::Utc),
                chrono::Duration::minutes(30),
            )
            .unwrap();

        let reports = store.list().unwrap();
        assert_eq!(recovered, 0);
        assert_eq!(reports[0].status, ReportGenerationStatus::Pending);
        assert!(reports[0].error_code.is_none());
    }

    #[test]
    fn successful_reports_are_not_changed_by_pending_recovery() {
        let store = test_store();
        let mut success = report("report-1", ReportGenerationStatus::Success);
        success.content = Some("done".into());
        success.created_at = "2026-06-27T10:00:00Z".into();
        success.updated_at = "2026-06-27T10:00:00Z".into();
        store.append(success).unwrap();

        let recovered = store
            .recover_stale_pending_reports_at(
                chrono::DateTime::parse_from_rfc3339("2026-06-27T11:00:00Z")
                    .unwrap()
                    .with_timezone(&chrono::Utc),
                chrono::Duration::minutes(30),
            )
            .unwrap();

        let reports = store.list().unwrap();
        assert_eq!(recovered, 0);
        assert_eq!(reports[0].status, ReportGenerationStatus::Success);
        assert_eq!(reports[0].content.as_deref(), Some("done"));
        assert!(reports[0].error_code.is_none());
    }
}

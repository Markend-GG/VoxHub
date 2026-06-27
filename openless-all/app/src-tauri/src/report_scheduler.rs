//! Daily report scheduler.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;

use crate::commands::GenerateReportRequest;
use crate::types::{ReportGenerationStatus, ReportType};

pub fn start_daily_report_scheduler(coordinator: Arc<crate::coordinator::Coordinator>) {
    if let Err(error) = std::thread::Builder::new()
        .name("openless-daily-report-scheduler".into())
        .spawn(move || {
            let in_flight_keys = Arc::new(Mutex::new(HashSet::new()));
            loop {
                std::thread::sleep(Duration::from_secs(30));
                let prefs = coordinator.prefs().get();
                if !prefs.daily_report_schedule_enabled {
                    continue;
                }
                let Some((hour, minute)) = parse_hhmm(&prefs.daily_report_schedule_time) else {
                    continue;
                };
                let now = chrono::Local::now();
                let today = now.format("%Y-%m-%d").to_string();
                if now.hour() != hour || now.minute() != minute {
                    continue;
                }
                let Some(template_id) = selected_daily_template(&coordinator) else {
                    log::warn!("[report-scheduler] no daily report template available");
                    continue;
                };
                let schedule_key = daily_schedule_key(&today, &template_id);
                if in_flight_keys.lock().contains(&schedule_key)
                    || schedule_key_already_completed(&coordinator, &schedule_key)
                {
                    continue;
                }
                in_flight_keys.lock().insert(schedule_key.clone());
                let coord = Arc::clone(&coordinator);
                let in_flight_for_task = Arc::clone(&in_flight_keys);
                tauri::async_runtime::spawn(async move {
                    let start = now
                        .date_naive()
                        .and_hms_opt(0, 0, 0)
                        .and_then(|local| local.and_local_timezone(chrono::Local).single())
                        .unwrap_or(now)
                        .with_timezone(&chrono::Utc);
                    let end = now.with_timezone(&chrono::Utc);
                    let request = GenerateReportRequest {
                        report_type: ReportType::Daily,
                        range_start: start.to_rfc3339(),
                        range_end: end.to_rfc3339(),
                        template_id,
                        user_main_work: None,
                        schedule_key: Some(schedule_key.clone()),
                    };
                    match crate::commands::generate_report_for_scheduler(&coord, request).await {
                        Ok(report) => {
                            if report.status == ReportGenerationStatus::Failed {
                                in_flight_for_task.lock().remove(&schedule_key);
                            }
                            log::info!("[report-scheduler] generated daily report {}", report.id);
                        }
                        Err(error) => {
                            in_flight_for_task.lock().remove(&schedule_key);
                            log::warn!("[report-scheduler] daily report failed: {error}");
                        }
                    }
                });
            }
        })
    {
        log::warn!("[report-scheduler] failed to start scheduler: {error}");
    }
}

fn selected_daily_template(coordinator: &crate::coordinator::Coordinator) -> Option<String> {
    let prefs = coordinator.prefs().get();
    let templates = coordinator.report_templates().list().ok()?;
    if templates
        .iter()
        .any(|template| template.id == prefs.selected_daily_report_template_id && template.report_type == ReportType::Daily)
    {
        return Some(prefs.selected_daily_report_template_id);
    }
    templates
        .into_iter()
        .find(|template| template.report_type == ReportType::Daily)
        .map(|template| template.id)
}

fn daily_schedule_key(day: &str, template_id: &str) -> String {
    format!("daily:{day}:{template_id}")
}

fn schedule_key_already_completed(
    coordinator: &crate::coordinator::Coordinator,
    schedule_key: &str,
) -> bool {
    coordinator
        .generated_reports()
        .list()
        .map(|reports| {
            reports.iter().any(|report| {
                report.schedule_key.as_deref() == Some(schedule_key)
                    && matches!(
                        report.status,
                        ReportGenerationStatus::Pending | ReportGenerationStatus::Success
                    )
            })
        })
        .unwrap_or(false)
}

fn parse_hhmm(value: &str) -> Option<(u32, u32)> {
    let mut parts = value.split(':');
    let hour = parts.next()?.parse::<u32>().ok()?;
    let minute = parts.next()?.parse::<u32>().ok()?;
    if parts.next().is_some() || hour > 23 || minute > 59 {
        return None;
    }
    Some((hour, minute))
}

trait DateTimeParts {
    fn hour(&self) -> u32;
    fn minute(&self) -> u32;
}

impl<Tz: chrono::TimeZone> DateTimeParts for chrono::DateTime<Tz> {
    fn hour(&self) -> u32 {
        chrono::Timelike::hour(self)
    }

    fn minute(&self) -> u32 {
        chrono::Timelike::minute(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{GeneratedReport, GeneratedReportSourceStats};

    fn report(schedule_key: Option<&str>, status: ReportGenerationStatus) -> GeneratedReport {
        GeneratedReport {
            id: uuid::Uuid::new_v4().to_string(),
            report_type: ReportType::Daily,
            title: "Daily".into(),
            range_start: "2026-06-27T00:00:00Z".into(),
            range_end: "2026-06-27T10:00:00Z".into(),
            template_id: "tpl".into(),
            template_name: "tpl".into(),
            template_content: "".into(),
            user_main_work: None,
            status,
            content: None,
            source_stats: GeneratedReportSourceStats::default(),
            error_code: None,
            error_message: None,
            schedule_key: schedule_key.map(str::to_string),
            created_at: "2026-06-27T10:00:00Z".into(),
            updated_at: "2026-06-27T10:00:00Z".into(),
        }
    }

    #[test]
    fn daily_schedule_key_includes_template() {
        assert_eq!(daily_schedule_key("2026-06-27", "tpl"), "daily:2026-06-27:tpl");
    }

    #[test]
    fn pending_or_success_blocks_duplicate_but_failed_does_not() {
        let key = "daily:2026-06-27:tpl";
        let reports = vec![
            report(Some(key), ReportGenerationStatus::Failed),
            report(Some("daily:2026-06-26:tpl"), ReportGenerationStatus::Success),
        ];
        assert!(!reports.iter().any(|entry| {
            entry.schedule_key.as_deref() == Some(key)
                && matches!(
                    entry.status,
                    ReportGenerationStatus::Pending | ReportGenerationStatus::Success
                )
        }));

        let reports = vec![report(Some(key), ReportGenerationStatus::Pending)];
        assert!(reports.iter().any(|entry| {
            entry.schedule_key.as_deref() == Some(key)
                && matches!(
                    entry.status,
                    ReportGenerationStatus::Pending | ReportGenerationStatus::Success
                )
        }));
    }
}

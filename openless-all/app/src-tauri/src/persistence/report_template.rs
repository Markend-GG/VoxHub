//! Report templates for daily / weekly / monthly reports.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use parking_lot::Mutex;

use super::{atomic_write, data_dir, ensure_dir, read_or_default};
use crate::types::{
    default_daily_report_template_id, default_monthly_report_template_id,
    default_weekly_report_template_id, ReportTemplate, ReportType,
};

const REPORT_TEMPLATE_FILE: &str = "report-templates.json";

pub struct ReportTemplateStore {
    path: PathBuf,
    lock: Arc<Mutex<()>>,
}

impl Clone for ReportTemplateStore {
    fn clone(&self) -> Self {
        Self {
            path: self.path.clone(),
            lock: Arc::clone(&self.lock),
        }
    }
}

impl ReportTemplateStore {
    pub fn new() -> Result<Self> {
        let dir = data_dir()?;
        ensure_dir(&dir)?;
        Ok(Self {
            path: dir.join(REPORT_TEMPLATE_FILE),
            lock: Arc::new(Mutex::new(())),
        })
    }

    pub(crate) fn new_fallback() -> Self {
        Self {
            path: std::env::temp_dir().join("openless_report_templates_fallback.json"),
            lock: Arc::new(Mutex::new(())),
        }
    }

    pub fn list(&self) -> Result<Vec<ReportTemplate>> {
        let _guard = self.lock.lock();
        Ok(merge_builtin_templates(self.read_locked()?))
    }

    pub fn save(&self, mut template: ReportTemplate) -> Result<ReportTemplate> {
        template.content = template.content.trim().to_string();
        if template.content.is_empty() {
            anyhow::bail!("report template content is empty");
        }
        let now = chrono::Utc::now().to_rfc3339();
        if template.created_at.trim().is_empty() {
            template.created_at = now.clone();
        }
        template.updated_at = now;
        template.is_builtin = false;

        let _guard = self.lock.lock();
        let mut templates = self.read_locked()?;
        templates.retain(|entry| entry.id != template.id);
        templates.insert(0, template.clone());
        self.write_locked(&templates)?;
        Ok(template)
    }

    pub fn delete(&self, id: &str) -> Result<()> {
        if builtin_templates().iter().any(|template| template.id == id) {
            anyhow::bail!("builtin report template cannot be deleted");
        }
        let _guard = self.lock.lock();
        let mut templates = self.read_locked()?;
        templates.retain(|entry| entry.id != id);
        self.write_locked(&templates)
    }

    fn read_locked(&self) -> Result<Vec<ReportTemplate>> {
        read_or_default::<Vec<ReportTemplate>>(&self.path)
    }

    fn write_locked(&self, templates: &[ReportTemplate]) -> Result<()> {
        let json =
            serde_json::to_vec_pretty(templates).context("encode report templates failed")?;
        atomic_write(&self.path, &json)
    }
}

pub fn builtin_templates() -> Vec<ReportTemplate> {
    let now = "1970-01-01T00:00:00Z".to_string();
    vec![
        ReportTemplate {
            id: default_daily_report_template_id(),
            report_type: ReportType::Daily,
            name: "默认日报模板".into(),
            content: default_daily_template(),
            is_builtin: true,
            created_at: now.clone(),
            updated_at: now.clone(),
        },
        ReportTemplate {
            id: default_weekly_report_template_id(),
            report_type: ReportType::Weekly,
            name: "默认周报模板".into(),
            content: default_weekly_template(),
            is_builtin: true,
            created_at: now.clone(),
            updated_at: now.clone(),
        },
        ReportTemplate {
            id: default_monthly_report_template_id(),
            report_type: ReportType::Monthly,
            name: "默认月报模板".into(),
            content: default_monthly_template(),
            is_builtin: true,
            created_at: now.clone(),
            updated_at: now,
        },
    ]
}

fn merge_builtin_templates(mut custom: Vec<ReportTemplate>) -> Vec<ReportTemplate> {
    for builtin in builtin_templates().into_iter().rev() {
        if !custom.iter().any(|entry| entry.id == builtin.id) {
            custom.insert(0, builtin);
        }
    }
    custom
}

fn default_daily_template() -> String {
    r#"请按以下结构生成日报：

1. 今日主要工作
2. 关键进展
3. 待办事项
4. 风险与阻塞
5. 明日计划
"#
    .trim()
    .to_string()
}

fn default_weekly_template() -> String {
    r#"请按以下结构生成周报：

1. 本周重点工作
2. 关键成果与进展
3. 重要决策
4. 待办事项
5. 风险与下周计划
"#
    .trim()
    .to_string()
}

fn default_monthly_template() -> String {
    r#"请按以下结构生成月报：

1. 本月工作概览
2. 重点成果
3. 项目进展
4. 风险与问题
5. 下月计划
"#
    .trim()
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_includes_builtin_templates() {
        let store = ReportTemplateStore::new_fallback();
        let templates = store.list().unwrap();
        assert!(templates
            .iter()
            .any(|template| template.id == default_daily_report_template_id()));
    }
}

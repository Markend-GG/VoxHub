use super::*;
use crate::types::{
    GeneratedReport, GeneratedReportSourceStats, ReportGenerationStatus, ReportTemplate,
    ReportType, ScreenshotRecord, ScreenshotRecordStatus,
};

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GenerateReportRequest {
    pub report_type: ReportType,
    pub range_start: String,
    pub range_end: String,
    pub template_id: String,
    pub user_main_work: Option<String>,
    #[serde(default)]
    pub schedule_key: Option<String>,
}

pub async fn generate_report_for_scheduler(
    coord: &crate::coordinator::Coordinator,
    request: GenerateReportRequest,
) -> Result<GeneratedReport, String> {
    generate_report_inner(coord, request).await
}

#[tauri::command]
pub fn list_screenshot_records(coord: CoordinatorState<'_>) -> Result<Vec<ScreenshotRecord>, String> {
    coord.screenshot_records().list().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn delete_screenshot_record(coord: CoordinatorState<'_>, id: String) -> Result<(), String> {
    if !is_valid_session_id(&id) {
        return Err("invalid screenshot record id".into());
    }
    coord.screenshot_records().delete(&id).map_err(|e| e.to_string())?;
    if let Err(error) = coord
        .context_capture()
        .delete_for_history(crate::types::ContextCaptureHistoryType::ScreenshotRecord, &id)
    {
        log::warn!("[screenshot-record] delete contexts failed: {error}");
    }
    if let Err(error) = coord
        .context_analysis()
        .delete_for_history(crate::types::ContextCaptureHistoryType::ScreenshotRecord, &id)
    {
        log::warn!("[screenshot-record] delete analysis failed: {error}");
    }
    Ok(())
}

#[tauri::command]
pub fn clear_screenshot_records(coord: CoordinatorState<'_>) -> Result<(), String> {
    coord.screenshot_records().clear().map_err(|e| e.to_string())?;
    if let Err(error) = coord
        .context_capture()
        .clear_for_history_type(crate::types::ContextCaptureHistoryType::ScreenshotRecord)
    {
        log::warn!("[screenshot-record] clear contexts failed: {error}");
    }
    if let Err(error) = coord
        .context_analysis()
        .clear_for_history_type(crate::types::ContextCaptureHistoryType::ScreenshotRecord)
    {
        log::warn!("[screenshot-record] clear analysis failed: {error}");
    }
    Ok(())
}

#[tauri::command]
pub fn reanalyze_screenshot_record(coord: CoordinatorState<'_>, id: String) -> Result<(), String> {
    if !is_valid_session_id(&id) {
        return Err("invalid screenshot record id".into());
    }
    let record = coord
        .screenshot_records()
        .list()
        .map_err(|e| e.to_string())?
        .into_iter()
        .find(|entry| entry.id == id)
        .ok_or_else(|| "screenshot record not found".to_string())?;
    if matches!(
        record.status,
        ScreenshotRecordStatus::Collecting
            | ScreenshotRecordStatus::Queued
            | ScreenshotRecordStatus::Analyzing
    ) {
        return Err("screenshot record analysis already in progress".into());
    }
    crate::context_vision_analysis::spawn_analysis_for_screenshot_record(
        coord.context_capture().clone(),
        coord.context_analysis().clone(),
        coord.screenshot_records().clone(),
        record,
    );
    Ok(())
}

#[tauri::command]
pub fn list_report_templates(coord: CoordinatorState<'_>) -> Result<Vec<ReportTemplate>, String> {
    coord.report_templates().list().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn save_report_template(
    coord: CoordinatorState<'_>,
    template: ReportTemplate,
) -> Result<ReportTemplate, String> {
    coord.report_templates().save(template).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn delete_report_template(coord: CoordinatorState<'_>, id: String) -> Result<(), String> {
    coord.report_templates().delete(&id).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn list_generated_reports(coord: CoordinatorState<'_>) -> Result<Vec<GeneratedReport>, String> {
    coord.generated_reports().list().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn get_generated_report(coord: CoordinatorState<'_>, id: String) -> Result<Option<GeneratedReport>, String> {
    if !is_valid_session_id(&id) {
        return Err("invalid report id".into());
    }
    coord.generated_reports().get(&id).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn delete_generated_report(coord: CoordinatorState<'_>, id: String) -> Result<(), String> {
    if !is_valid_session_id(&id) {
        return Err("invalid report id".into());
    }
    coord.generated_reports().delete(&id).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn generate_report(
    coord: CoordinatorState<'_>,
    request: GenerateReportRequest,
) -> Result<GeneratedReport, String> {
    generate_report_inner(&coord, request).await
}

async fn generate_report_inner(
    coord: &crate::coordinator::Coordinator,
    request: GenerateReportRequest,
) -> Result<GeneratedReport, String> {
    let range_start = parse_report_time(&request.range_start)?;
    let range_end = parse_report_time(&request.range_end)?;
    if range_end < range_start {
        return Err("结束时间不能早于开始时间".into());
    }
    let templates = coord.report_templates().list().map_err(|e| e.to_string())?;
    let template = templates
        .into_iter()
        .find(|entry| entry.id == request.template_id && entry.report_type == request.report_type)
        .ok_or_else(|| "report template not found".to_string())?;
    if template.content.trim().is_empty() {
        return Err("报告模板为空".into());
    }

    let material = build_report_material(&coord, range_start, range_end, request.user_main_work.as_deref())?;
    if material.source_stats.voice_count == 0
        && material.source_stats.rewrite_count == 0
        && material.source_stats.screenshot_record_count == 0
        && request.user_main_work.as_deref().unwrap_or("").trim().is_empty()
    {
        let report = skipped_report(request, template, material.source_stats, "skipped:noContent");
        coord.generated_reports().append(report.clone()).map_err(|e| e.to_string())?;
        return Ok(report);
    }

    let now = chrono::Utc::now().to_rfc3339();
    let mut report = GeneratedReport {
        id: uuid::Uuid::new_v4().to_string(),
        report_type: request.report_type,
        title: format!("{} {}", report_type_label(request.report_type), range_start.format("%Y-%m-%d")),
        range_start: range_start.to_rfc3339(),
        range_end: range_end.to_rfc3339(),
        template_id: template.id.clone(),
        template_name: template.name.clone(),
        template_content: template.content.clone(),
        user_main_work: request.user_main_work.clone(),
        status: ReportGenerationStatus::Pending,
        content: None,
        source_stats: material.source_stats.clone(),
        error_code: None,
        error_message: None,
        schedule_key: request.schedule_key.clone(),
        created_at: now.clone(),
        updated_at: now,
    };
    coord.generated_reports().append(report.clone()).map_err(|e| e.to_string())?;

    let content = request_report_generation(&request, &template, &material)
        .await
        .map_err(|error| error.to_string());
    match content {
        Ok(content) => {
            report.status = ReportGenerationStatus::Success;
            report.content = Some(content);
        }
        Err(error) => {
            report.status = ReportGenerationStatus::Failed;
            report.error_code = Some("failed:llmRequest".into());
            report.error_message = Some(error);
        }
    }
    report.updated_at = chrono::Utc::now().to_rfc3339();
    coord.generated_reports().replace(report.clone()).map_err(|e| e.to_string())?;
    Ok(report)
}

struct ReportMaterial {
    source_stats: GeneratedReportSourceStats,
    text: String,
}

struct ReportSummary {
    source_label: &'static str,
    text: String,
}

fn parse_report_time(value: &str) -> Result<chrono::DateTime<chrono::Utc>, String> {
    chrono::DateTime::parse_from_rfc3339(value)
        .map(|value| value.with_timezone(&chrono::Utc))
        .map_err(|_| "时间格式无效".to_string())
}

fn build_report_material(
    coord: &crate::coordinator::Coordinator,
    range_start: chrono::DateTime<chrono::Utc>,
    range_end: chrono::DateTime<chrono::Utc>,
    user_main_work: Option<&str>,
) -> Result<ReportMaterial, String> {
    let mut voice_lines = Vec::new();
    let mut rewrite_lines = Vec::new();
    let mut screenshot_lines = Vec::new();
    let mut data_gaps = Vec::new();
    let mut stats = GeneratedReportSourceStats::default();

    let mut contexts = coord.context_capture().list().map_err(|e| e.to_string())?;
    let analyses = coord.context_analysis().list().map_err(|e| e.to_string())?;
    crate::persistence::enrich_context_entries_with_analysis(&mut contexts, &analyses);

    let mut voice_history = coord.history().list().map_err(|e| e.to_string())?;
    crate::persistence::enrich_voice_history_with_context(&mut voice_history, &contexts);
    for entry in voice_history {
        if !time_in_range(&entry.created_at, range_start, range_end) {
            continue;
        }
        stats.voice_count += 1;
        let analysis = entry
            .context_capture
            .as_ref()
            .and_then(|context| context.analysis.as_ref())
            .filter(|analysis| analysis.status == crate::types::ContextAnalysisStatus::Success);
        let summary = analysis
            .and_then(summary_from_analysis)
            .unwrap_or_else(|| {
                data_gaps.push(format!("[{}][语音] 缺少完整摘要，已降级使用文本字段。", entry.created_at));
                ReportSummary {
                    source_label: "降级文本字段",
                    text: fallback_text(&entry.final_text, &entry.raw_transcript),
                }
            });
        voice_lines.push(format_report_line(
            &entry.created_at,
            "语音",
            entry
                .context_capture
                .as_ref()
                .and_then(|context| context.context_app.as_deref())
                .or(entry.app_name.as_deref()),
            entry
                .context_capture
                .as_ref()
                .and_then(|context| context.conversation_window.as_deref()),
            analysis,
            &summary,
        ));
    }

    let mut rewrite_history = coord.rewrite_history().list().map_err(|e| e.to_string())?;
    crate::persistence::enrich_rewrite_history_with_context(&mut rewrite_history, &contexts);
    for entry in rewrite_history {
        if !time_in_range(&entry.created_at, range_start, range_end) {
            continue;
        }
        stats.rewrite_count += 1;
        let analysis = entry
            .context_capture
            .as_ref()
            .and_then(|context| context.analysis.as_ref())
            .filter(|analysis| analysis.status == crate::types::ContextAnalysisStatus::Success);
        let summary = analysis
            .and_then(summary_from_analysis)
            .unwrap_or_else(|| {
                data_gaps.push(format!("[{}][重写] 缺少完整摘要，已降级使用文本字段。", entry.created_at));
                ReportSummary {
                    source_label: "降级文本字段",
                    text: fallback_text(&entry.rewritten_text, &entry.source_text),
                }
            });
        rewrite_lines.push(format_report_line(
            &entry.created_at,
            "重写",
            entry
                .context_capture
                .as_ref()
                .and_then(|context| context.context_app.as_deref())
                .or(entry.app_name.as_deref()),
            entry
                .context_capture
                .as_ref()
                .and_then(|context| context.conversation_window.as_deref()),
            analysis,
            &summary,
        ));
    }

    for entry in coord.screenshot_records().list().map_err(|e| e.to_string())? {
        if !time_in_range(&entry.created_at, range_start, range_end) {
            continue;
        }
        stats.screenshot_record_count += 1;
        let analysis = entry
            .analysis
            .as_ref()
            .filter(|analysis| analysis.status == crate::types::ContextAnalysisStatus::Success);
        if analysis.and_then(summary_from_analysis).is_some() {
            stats.analyzed_screenshot_record_count += 1;
        }
        let summary = analysis
            .and_then(summary_from_analysis)
            .unwrap_or_else(|| {
                data_gaps.push(format!(
                    "[{}][截图记录] 缺少完整摘要，已降级使用截图记录元数据。",
                    entry.created_at
                ));
                ReportSummary {
                    source_label: "降级截图元数据",
                    text: format!(
                        "截图记录：{}，截图 {} 张，触发 {} 次",
                        entry.window_title.as_deref().unwrap_or("未知窗口"),
                        entry.screenshot_ids.len(),
                        entry.trigger_count
                    ),
                }
            });
        screenshot_lines.push(format_report_line(
            &entry.created_at,
            "截图记录",
            entry.context_app.as_deref(),
            entry.conversation_window.as_deref(),
            analysis,
            &summary,
        ));
    }

    let text = format!(
        "用户主要工作：\n{}\n\n语音历史摘要：\n{}\n\n重写历史摘要：\n{}\n\n截图记录摘要：\n{}\n\n数据缺口：\n{}",
        user_main_work.unwrap_or("").trim(),
        if voice_lines.is_empty() { "无".into() } else { voice_lines.join("\n") },
        if rewrite_lines.is_empty() { "无".into() } else { rewrite_lines.join("\n") },
        if screenshot_lines.is_empty() { "无".into() } else { screenshot_lines.join("\n") },
        if data_gaps.is_empty() { "无".into() } else { data_gaps.join("\n") },
    );
    Ok(ReportMaterial { source_stats: stats, text })
}

fn summary_from_analysis(analysis: &crate::types::ContextAnalysisResult) -> Option<ReportSummary> {
    if let Some(value) = analysis
        .full_summary
        .as_ref()
        .filter(|value| !value.trim().is_empty())
    {
        return Some(ReportSummary {
            source_label: "fullSummary",
            text: value.trim().to_string(),
        });
    }
    analysis
        .brief_summary
        .as_ref()
        .filter(|value| !value.trim().is_empty())
        .map(|value| ReportSummary {
            source_label: "briefSummary",
            text: value.trim().to_string(),
        })
}

fn format_report_line(
    created_at: &str,
    source_type: &str,
    app: Option<&str>,
    conversation: Option<&str>,
    analysis: Option<&crate::types::ContextAnalysisResult>,
    summary: &ReportSummary,
) -> String {
    let topic = analysis.and_then(|analysis| analysis.topic.as_deref()).unwrap_or("未知主题");
    let decision = analysis
        .and_then(|analysis| analysis.decision.as_deref())
        .unwrap_or("无明确结论");
    let action_items = analysis
        .map(|analysis| {
            analysis
                .action_items
                .iter()
                .filter_map(|item| {
                    let text = item.text.trim();
                    if text.is_empty() {
                        None
                    } else {
                        Some(text.to_string())
                    }
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let source_note = match source_type {
        "截图记录" => "来源说明=屏幕中正在查看/讨论/记录的内容，不能直接当作用户已完成工作",
        "语音" => "来源说明=用户语音输入及其上下文",
        "重写" => "来源说明=用户选中文本重写及其上下文",
        _ => "来源说明=OpenLess 历史",
    };
    format!(
        "- [时间={}][来源={}][应用={}][对话={}][主题={}][摘要来源={}][摘要={}][结论={}][待办={}][{}]",
        created_at,
        source_type,
        app.filter(|value| !value.trim().is_empty()).unwrap_or("未知应用"),
        conversation
            .filter(|value| !value.trim().is_empty())
            .unwrap_or("未知对话"),
        topic,
        summary.source_label,
        summary.text,
        decision,
        if action_items.is_empty() {
            "无".into()
        } else {
            action_items.join("；")
        },
        source_note
    )
}

fn fallback_text(primary: &str, secondary: &str) -> String {
    let text = if primary.trim().is_empty() { secondary } else { primary };
    text.chars().take(240).collect()
}

fn time_in_range(value: &str, start: chrono::DateTime<chrono::Utc>, end: chrono::DateTime<chrono::Utc>) -> bool {
    chrono::DateTime::parse_from_rfc3339(value)
        .map(|time| {
            let time = time.with_timezone(&chrono::Utc);
            time >= start && time <= end
        })
        .unwrap_or(false)
}

async fn request_report_generation(
    request: &GenerateReportRequest,
    template: &ReportTemplate,
    material: &ReportMaterial,
) -> anyhow::Result<String> {
    let active_provider = crate::persistence::CredentialsVault::get_active_llm();
    if active_provider == crate::polish::CODEX_OAUTH_PROVIDER_ID || active_provider == "gemini" {
        anyhow::bail!("当前 LLM 服务暂不支持报告生成");
    }
    let api_key = crate::persistence::CredentialsVault::get(crate::persistence::CredentialAccount::ArkApiKey)?
        .unwrap_or_default();
    let endpoint = crate::persistence::CredentialsVault::get(crate::persistence::CredentialAccount::ArkEndpoint)?
        .unwrap_or_default();
    let model = crate::persistence::CredentialsVault::get(crate::persistence::CredentialAccount::ArkModelId)?
        .unwrap_or_default();
    if endpoint.trim().is_empty() || api_key.trim().is_empty() || model.trim().is_empty() {
        anyhow::bail!("LLM 服务未配置");
    }

    let prompt = format!(
        "你是 OpenLess 的工作报告生成助手。请严格根据用户输入和 OpenLess 内部历史摘要生成报告，不要编造未出现的事实。\n\n报告类型：{}\n时间范围：{} 到 {}\n模板：\n{}\n\n材料：\n{}\n\n材料优先级：\n1. 用户主要工作优先级最高，可作为报告主线；如果它与历史摘要冲突，以用户主要工作为准，并可用“历史记录显示...”补充差异。\n2. 摘要来源=fullSummary 的材料是主要事实材料，但必须结合来源类型判断事实强度。\n3. 语音历史代表用户主动表达、记录或确认的内容；重写历史代表用户对选中文本做表达调整，可从中提取写作任务、表达目标和已确认事项。\n4. 截图记录代表屏幕中正在查看、讨论或记录的上下文。即使摘要来源=fullSummary，也不能直接写成用户已完成工作；只有摘要中明确出现完成、提交、交付、确认结论等证据时，才可写为进展或成果。\n5. 摘要来源=briefSummary、降级文本字段或降级截图元数据的材料只作为低置信度参考，不要扩写成确定成果。\n6. 如果材料只有画面描述或上下文线索，没有明确动作、结论或交付物，请归入背景、待确认、风险或后续计划。\n\n要求：\n- 严格按模板输出。\n- 保留语音、重写、截图记录的来源差异，不要混合成同一种事实。\n- 优先从 fullSummary 中提取进展、结论、待办、风险和协作对象。\n- 对不确定事项使用“待确认”，不要用推测补齐报告。\n- 不输出无关 UI 描述，不复述敏感信息原文。",
        report_type_label(request.report_type),
        request.range_start,
        request.range_end,
        template.content,
        material.text
    );
    let body = serde_json::json!({
        "model": model,
        "stream": false,
        "temperature": 0.2,
        "messages": [
            { "role": "system", "content": "你是严谨的工作报告生成助手，只根据给定材料输出。" },
            { "role": "user", "content": prompt }
        ]
    });
    let url = {
        let trimmed = endpoint.trim().trim_end_matches('/');
        if trimmed.ends_with("/chat/completions") {
            trimmed.to_string()
        } else {
            format!("{trimmed}/chat/completions")
        }
    };
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()?;
    let response = client
        .post(url)
        .header("Content-Type", "application/json")
        .header("Authorization", format!("Bearer {api_key}"))
        .json(&body)
        .send()
        .await?;
    let status = response.status();
    let text = response.text().await?;
    if !status.is_success() {
        anyhow::bail!("report generation http {}", status.as_u16());
    }
    let value: serde_json::Value = serde_json::from_str(&text)?;
    let content = value["choices"]
        .as_array()
        .and_then(|choices| choices.first())
        .and_then(|choice| choice["message"]["content"].as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if content.is_empty() {
        anyhow::bail!("报告生成结果为空");
    }
    Ok(content)
}

fn skipped_report(
    request: GenerateReportRequest,
    template: ReportTemplate,
    source_stats: GeneratedReportSourceStats,
    error_code: &str,
) -> GeneratedReport {
    let now = chrono::Utc::now().to_rfc3339();
    GeneratedReport {
        id: uuid::Uuid::new_v4().to_string(),
        report_type: request.report_type,
        title: format!("{} {}", report_type_label(request.report_type), now),
        range_start: request.range_start,
        range_end: request.range_end,
        template_id: template.id,
        template_name: template.name,
        template_content: template.content,
        user_main_work: request.user_main_work,
        status: ReportGenerationStatus::Skipped,
        content: None,
        source_stats,
        error_code: Some(error_code.into()),
        error_message: None,
        schedule_key: request.schedule_key,
        created_at: now.clone(),
        updated_at: now,
    }
}

fn report_type_label(report_type: ReportType) -> &'static str {
    match report_type {
        ReportType::Daily => "日报",
        ReportType::Weekly => "周报",
        ReportType::Monthly => "月报",
    }
}

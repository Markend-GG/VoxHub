//! Screenshot record hotkey flow.

use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use uuid::Uuid;

use crate::context_capture::{RecentPrimaryCapture, WindowIdentity};
use crate::coordinator::Inner;
use crate::types::{
    ContextCaptureEntry, ContextCaptureHistoryType, ScreenshotRecord, ScreenshotRecordStatus,
};

struct ActiveScreenshotWindow {
    record_id: String,
    started_at: Instant,
}

const VOICE_REWRITE_DUPLICATE_SUPPRESSION_WINDOW: Duration = Duration::from_secs(30);

static ACTIVE_WINDOW: OnceLock<Mutex<Option<ActiveScreenshotWindow>>> = OnceLock::new();
static SUPPRESSED_CONTEXT_IDS: OnceLock<Mutex<std::collections::VecDeque<String>>> =
    OnceLock::new();

fn active_window() -> &'static Mutex<Option<ActiveScreenshotWindow>> {
    ACTIVE_WINDOW.get_or_init(|| Mutex::new(None))
}

fn suppressed_context_ids() -> &'static Mutex<std::collections::VecDeque<String>> {
    SUPPRESSED_CONTEXT_IDS.get_or_init(|| Mutex::new(std::collections::VecDeque::new()))
}

pub(crate) fn handle_screenshot_record_hotkey(inner: &Arc<Inner>) {
    let prefs = inner.prefs.get();
    if !prefs.screenshot_record_enabled || prefs.screenshot_record_paused {
        return;
    }
    // 白名单判断：在截图、压缩、LLM 分析、历史记录创建之前执行
    if !crate::screenshot_whitelist::screenshot_allowed_by_whitelist(&prefs) {
        return;
    }
    if should_suppress_recent_voice_or_rewrite_duplicate(
        inner,
        VOICE_REWRITE_DUPLICATE_SUPPRESSION_WINDOW,
    ) {
        log::info!("[screenshot-record] suppress duplicate enter capture after voice/rewrite");
        return;
    }

    // 按应用聚合分析模式：截图进入聚合队列而非立即创建正式记录
    if prefs.screenshot_app_aggregation_enabled {
        handle_aggregation_hotkey(inner);
        return;
    }

    // 传统即时截图记录流程（以下不变）

    let now = chrono::Utc::now().to_rfc3339();
    let merge_window = Duration::from_secs(u64::from(
        prefs.screenshot_record_merge_window_seconds.clamp(10, 300),
    ));

    let (record_id, is_new) = {
        let mut guard = active_window().lock();
        if let Some(active) = guard.as_ref() {
            if active.started_at.elapsed() <= merge_window {
                (active.record_id.clone(), false)
            } else {
                let id = Uuid::new_v4().to_string();
                *guard = Some(ActiveScreenshotWindow {
                    record_id: id.clone(),
                    started_at: Instant::now(),
                });
                (id, true)
            }
        } else {
            let id = Uuid::new_v4().to_string();
            *guard = Some(ActiveScreenshotWindow {
                record_id: id.clone(),
                started_at: Instant::now(),
            });
            (id, true)
        }
    };

    if is_new {
        let record = ScreenshotRecord {
            id: record_id.clone(),
            created_at: now.clone(),
            updated_at: now.clone(),
            window_started_at: now,
            window_ended_at: None,
            status: ScreenshotRecordStatus::Collecting,
            context_app: None,
            conversation_window: None,
            window_title: None,
            screenshot_ids: Vec::new(),
            submitted_screenshot_ids: Vec::new(),
            trigger_count: 0,
            error_code: None,
            error_message: None,
            analysis: None,
            aggregation_mode: Some("immediate".to_string()),
            aggregation_bucket_id: None,
            process_name: None,
        };
        if let Err(error) = inner.screenshot_records.upsert_with_retention(
            record,
            prefs.history_retention_days,
            prefs.history_max_entries,
        ) {
            log::warn!("[screenshot-record] create record failed: {error}");
            return;
        }
        // 通知前端历史列表刷新
        inner.emit_event("history:updated", "screenshot");
        schedule_finalize(Arc::clone(inner), record_id.clone(), merge_window);
    }

    let capture_inner = Arc::clone(inner);
    std::thread::Builder::new()
        .name("openless-screenshot-record-capture".into())
        .spawn(move || capture_one(capture_inner, record_id))
        .ok();
}

fn should_suppress_recent_voice_or_rewrite_duplicate(inner: &Arc<Inner>, window: Duration) -> bool {
    let Some(current) = crate::context_capture::current_window_identity() else {
        return false;
    };
    if let Some(entry) =
        crate::context_capture::take_recent_primary_capture_matching(window, |entry| {
            recent_primary_capture_matches_current(entry, &current)
        })
    {
        mark_context_suppressed_once(&entry.id);
        return true;
    }
    let Ok(contexts) = inner.context_capture.list() else {
        return false;
    };
    contexts
        .into_iter()
        .filter(|entry| {
            matches!(
                entry.linked_history_type,
                ContextCaptureHistoryType::Voice | ContextCaptureHistoryType::Rewrite
            )
        })
        .filter(|entry| context_created_within(entry, window))
        .any(|entry| {
            context_capture_entry_matches_current(&entry, &current)
                && mark_context_suppressed_once(&entry.id)
        })
}

fn recent_primary_capture_matches_current(
    entry: &RecentPrimaryCapture,
    current: &WindowIdentity,
) -> bool {
    if !matches!(
        entry.history_type,
        ContextCaptureHistoryType::Voice | ContextCaptureHistoryType::Rewrite
    ) {
        return false;
    }
    context_identity_matches(
        entry.window_title.as_deref(),
        entry.conversation_window.as_deref(),
        entry.context_app.as_deref(),
        current,
    )
}

fn context_capture_entry_matches_current(
    entry: &ContextCaptureEntry,
    current: &WindowIdentity,
) -> bool {
    context_identity_matches(
        entry.window_title.as_deref(),
        entry.conversation_window.as_deref(),
        entry.context_app.as_deref(),
        current,
    )
}

fn context_identity_matches(
    window_title: Option<&str>,
    conversation_window: Option<&str>,
    context_app: Option<&str>,
    current: &WindowIdentity,
) -> bool {
    if same_optional_text(window_title, current.window_title.as_deref()) {
        return true;
    }
    if same_optional_text(conversation_window, current.conversation_window.as_deref()) {
        return true;
    }
    if window_title
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .is_none()
        && conversation_window
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .is_none()
        && current
            .window_title
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .is_none()
        && current
            .conversation_window
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .is_none()
    {
        return same_optional_text(context_app, current.context_app.as_deref());
    }
    false
}

fn mark_context_suppressed_once(context_id: &str) -> bool {
    let mut suppressed = suppressed_context_ids().lock();
    if suppressed.iter().any(|id| id == context_id) {
        return false;
    }
    suppressed.push_back(context_id.to_string());
    while suppressed.len() > 64 {
        suppressed.pop_front();
    }
    true
}

fn context_created_within(entry: &ContextCaptureEntry, window: Duration) -> bool {
    let Ok(created_at) = chrono::DateTime::parse_from_rfc3339(&entry.created_at) else {
        return false;
    };
    let elapsed = chrono::Utc::now().signed_duration_since(created_at.with_timezone(&chrono::Utc));
    elapsed >= chrono::Duration::zero()
        && elapsed
            <= chrono::Duration::from_std(window).unwrap_or_else(|_| chrono::Duration::seconds(10))
}

fn same_optional_text(left: Option<&str>, right: Option<&str>) -> bool {
    let Some(left) = left.map(str::trim).filter(|value| !value.is_empty()) else {
        return false;
    };
    let Some(right) = right.map(str::trim).filter(|value| !value.is_empty()) else {
        return false;
    };
    left == right
}

pub(crate) fn cancel_active_window_without_analysis(inner: &Arc<Inner>, reason: &str) {
    let record_id = {
        let mut guard = active_window().lock();
        guard.take().map(|active| active.record_id)
    };
    let Some(record_id) = record_id else {
        return;
    };
    let prefs = inner.prefs.get();
    let Some(mut record) = inner
        .screenshot_records
        .list()
        .ok()
        .and_then(|records| records.into_iter().find(|entry| entry.id == record_id))
    else {
        return;
    };
    if record.status != ScreenshotRecordStatus::Collecting {
        return;
    }
    record.window_ended_at = Some(chrono::Utc::now().to_rfc3339());
    record.status = ScreenshotRecordStatus::Skipped;
    record.error_code = Some(reason.into());
    record.updated_at = chrono::Utc::now().to_rfc3339();
    let _ = inner.screenshot_records.upsert_with_retention(
        record,
        prefs.history_retention_days,
        prefs.history_max_entries,
    );
}

fn schedule_finalize(inner: Arc<Inner>, record_id: String, delay: Duration) {
    std::thread::Builder::new()
        .name("openless-screenshot-record-finalize".into())
        .spawn(move || {
            std::thread::sleep(delay);
            {
                let mut guard = active_window().lock();
                if guard
                    .as_ref()
                    .map(|active| active.record_id.as_str() == record_id)
                    .unwrap_or(false)
                {
                    *guard = None;
                }
            }
            finalize_record(inner, record_id);
        })
        .ok();
}

fn capture_one(inner: Arc<Inner>, record_id: String) {
    let prefs = inner.prefs.get();
    let context_id = Uuid::new_v4().to_string();
    if let Err(error) = crate::context_capture::capture_and_store_with_id(
        &inner.context_capture,
        context_id.clone(),
        ContextCaptureHistoryType::ScreenshotRecord,
        record_id.clone(),
        prefs.history_retention_days,
        prefs.history_max_entries,
    ) {
        log::warn!("[screenshot-record] capture failed: {error}");
    }

    let contexts = match inner.context_capture.list() {
        Ok(contexts) => contexts,
        Err(error) => {
            log::warn!("[screenshot-record] load context after capture failed: {error}");
            return;
        }
    };
    let Some(context) = contexts.into_iter().find(|entry| entry.id == context_id) else {
        return;
    };

    if let Err(error) = inner.screenshot_records.append_capture_result(
        &record_id,
        &context,
        prefs.history_retention_days,
        prefs.history_max_entries,
    ) {
        log::warn!("[screenshot-record] append capture result failed: {error}");
    }
}

// ─── 按应用聚合分析模式 ──────────────────────────────────────────────────────

/// 聚合模式下的截图热键处理：
/// 1. 读取前台进程名（无法识别则不进入队列）
/// 2. 捕获截图
/// 3. 截图进入聚合桶
/// 4. 检查 finalize 条件
fn handle_aggregation_hotkey(inner: &Arc<Inner>) {
    // 读取前台进程名
    #[cfg(target_os = "windows")]
    let (process_name, app_display_name) = {
        let Some(identity) = crate::screenshot_whitelist::current_foreground_app_identity() else {
            log::debug!("[agg] foreground process unknown, skip");
            return;
        };
        let name = identity.process_name.to_lowercase();
        if crate::screenshot_whitelist::is_openless_process(&name) {
            log::debug!("[agg] foreground is OpenLess, skip");
            return;
        }
        (name, identity.display_name)
    };
    #[cfg(not(target_os = "windows"))]
    let (process_name, app_display_name) = {
        // 非 Windows：使用窗口标题作为应用名
        let Some(identity) = crate::context_capture::current_window_identity() else {
            log::debug!("[agg] window identity unknown, skip");
            return;
        };
        let name = identity
            .context_app
            .unwrap_or_else(|| "unknown".to_string())
            .to_lowercase();
        (name, identity.conversation_window)
    };

    if process_name.is_empty() {
        log::debug!("[agg] empty process name, skip");
        return;
    }

    let capture_inner = Arc::clone(inner);
    let proc_name = process_name;
    let app_name = app_display_name;
    std::thread::Builder::new()
        .name("openless-agg-capture".into())
        .spawn(move || capture_and_aggregate(capture_inner, proc_name, app_name))
        .ok();
}

/// 聚合模式下单次截图：捕获后追加到聚合桶，检查 finalize。
fn capture_and_aggregate(
    inner: Arc<Inner>,
    process_name: String,
    app_display_name: Option<String>,
) {
    let prefs = inner.prefs.get();
    let context_id = Uuid::new_v4().to_string();

    // 捕获截图
    if let Err(error) = crate::context_capture::capture_and_store_with_id(
        &inner.context_capture,
        context_id.clone(),
        ContextCaptureHistoryType::ScreenshotRecord,
        "agg-pending".to_string(),
        prefs.history_retention_days,
        prefs.history_max_entries,
    ) {
        log::warn!("[agg] capture failed: {error}");
        return;
    }

    // 检查截图文件是否存在
    let contexts = match inner.context_capture.list() {
        Ok(list) => list,
        Err(e) => {
            log::warn!("[agg] load contexts failed: {e}");
            return;
        }
    };
    let Some(context) = contexts.into_iter().find(|entry| entry.id == context_id) else {
        log::warn!("[agg] context {} not found after capture", context_id);
        return;
    };
    if context.screenshot_ref.is_none() {
        log::debug!("[agg] capture {} has no screenshot file, skip", context_id);
        return;
    }

    // 追加到聚合桶
    let to_finalize = crate::screenshot_aggregation::append_screenshot(
        &inner,
        &process_name,
        app_display_name.as_deref(),
        &context_id,
    );

    // 通知前端聚合状态变化
    inner.emit_event("aggregation:updated", serde_json::Value::Null);

    // Finalize 已满足条件的桶
    for bucket in to_finalize {
        log::info!(
            "[agg] finalizing bucket {} ({}, {} screenshots)",
            bucket.id,
            bucket.process_name,
            bucket.screenshot_ids.len()
        );
        crate::screenshot_aggregation::finalize_bucket(&inner, &bucket);
    }
}

fn finalize_record(inner: Arc<Inner>, record_id: String) {
    std::thread::sleep(Duration::from_secs(2));
    let prefs = inner.prefs.get();
    let Some(mut record) = inner
        .screenshot_records
        .list()
        .ok()
        .and_then(|records| records.into_iter().find(|entry| entry.id == record_id))
    else {
        return;
    };

    if record.status != ScreenshotRecordStatus::Collecting {
        return;
    }

    record.window_ended_at = Some(chrono::Utc::now().to_rfc3339());
    if record.screenshot_ids.is_empty() {
        record.status = ScreenshotRecordStatus::Failed;
        record.error_code = Some("failed:noScreenshot".into());
        record.updated_at = chrono::Utc::now().to_rfc3339();
        let _ = inner.screenshot_records.upsert_with_retention(
            record,
            prefs.history_retention_days,
            prefs.history_max_entries,
        );
        return;
    }

    let max_images = prefs.screenshot_record_max_images_per_analysis.clamp(1, 5) as usize;
    record.submitted_screenshot_ids = select_screenshot_ids(&record.screenshot_ids, max_images);
    record.status = ScreenshotRecordStatus::Queued;
    record.updated_at = chrono::Utc::now().to_rfc3339();
    if let Err(error) = inner.screenshot_records.upsert_with_retention(
        record.clone(),
        prefs.history_retention_days,
        prefs.history_max_entries,
    ) {
        log::warn!("[screenshot-record] finalize save failed: {error}");
        return;
    }
    // 通知前端历史列表刷新
    inner.emit_event("history:updated", "screenshot");

    crate::context_vision_analysis::spawn_analysis_for_screenshot_record(
        inner.context_capture.clone(),
        inner.context_analysis.clone(),
        inner.screenshot_records.clone(),
        record,
    );
}

pub(crate) fn select_screenshot_ids(ids: &[String], max_images: usize) -> Vec<String> {
    if ids.len() <= max_images {
        return ids.to_vec();
    }
    if max_images <= 1 {
        return ids.first().cloned().into_iter().collect();
    }
    let mut selected = Vec::new();
    selected.push(ids[0].clone());
    if max_images > 2 {
        let middle_slots = max_images - 2;
        let last_middle = ids.len() - 2;
        for slot in 0..middle_slots {
            let index = 1 + ((slot + 1) * last_middle / (middle_slots + 1));
            if !selected.iter().any(|id| id == &ids[index]) {
                selected.push(ids[index].clone());
            }
        }
    }
    if let Some(last) = ids.last() {
        if !selected.iter().any(|id| id == last) {
            selected.push(last.clone());
        }
    }
    selected.truncate(max_images);
    selected
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn select_all_when_within_limit() {
        let ids = vec!["a".into(), "b".into(), "c".into()];
        assert_eq!(select_screenshot_ids(&ids, 5), ids);
    }

    #[test]
    fn select_first_last_and_middle_samples() {
        let ids = ["a", "b", "c", "d", "e", "f", "g"]
            .into_iter()
            .map(String::from)
            .collect::<Vec<_>>();
        let selected = select_screenshot_ids(&ids, 5);
        assert_eq!(selected.first().map(String::as_str), Some("a"));
        assert_eq!(selected.last().map(String::as_str), Some("g"));
        assert_eq!(selected.len(), 5);
    }

    #[test]
    fn suppress_context_only_once() {
        let id = format!("test-{}", Uuid::new_v4());
        assert!(mark_context_suppressed_once(&id));
        assert!(!mark_context_suppressed_once(&id));
    }

    #[test]
    fn same_optional_text_requires_non_empty_equal_values() {
        assert!(same_optional_text(Some("Codex"), Some("Codex")));
        assert!(!same_optional_text(Some("Codex"), Some("Chrome")));
        assert!(!same_optional_text(Some(""), Some("")));
        assert!(!same_optional_text(None, Some("Codex")));
    }

    #[test]
    fn identity_match_requires_specific_window_before_app_fallback() {
        let current = WindowIdentity {
            window_title: Some("需求讨论 - WorkBuddy".into()),
            context_app: Some("WorkBuddy".into()),
            conversation_window: Some("需求讨论".into()),
        };
        assert!(context_identity_matches(
            Some("需求讨论 - WorkBuddy"),
            Some("需求讨论"),
            Some("WorkBuddy"),
            &current
        ));
        assert!(!context_identity_matches(
            Some("日报 - WorkBuddy"),
            Some("日报"),
            Some("WorkBuddy"),
            &current
        ));

        let app_only_current = WindowIdentity {
            window_title: None,
            context_app: Some("WorkBuddy".into()),
            conversation_window: None,
        };
        assert!(context_identity_matches(
            None,
            None,
            Some("WorkBuddy"),
            &app_only_current
        ));
    }

    #[test]
    fn recent_primary_capture_uses_same_one_shot_guard_as_persisted_context() {
        let id = format!("test-{}", Uuid::new_v4());
        let current = WindowIdentity {
            window_title: Some("图片上下文 - Codex".into()),
            context_app: Some("Codex".into()),
            conversation_window: Some("图片上下文".into()),
        };
        let recent = RecentPrimaryCapture {
            id: id.clone(),
            captured_at: Instant::now(),
            history_type: ContextCaptureHistoryType::Rewrite,
            window_title: current.window_title.clone(),
            context_app: current.context_app.clone(),
            conversation_window: current.conversation_window.clone(),
        };

        assert!(recent_primary_capture_matches_current(&recent, &current));
        assert!(mark_context_suppressed_once(&recent.id));
        assert!(!mark_context_suppressed_once(&id));
    }
}

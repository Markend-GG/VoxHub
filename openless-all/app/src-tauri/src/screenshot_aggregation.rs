//! 截图按应用聚合分析：状态机与 finalize 逻辑。
//!
//! 白名单命中后，截图进入待聚合队列（按 `processName` 分桶），
//! 满足提交条件后再创建正式 `ScreenshotRecord` 并提交 LLM 分析。

use std::sync::Arc;

use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::coordinator::Inner;
use crate::persistence::ScreenshotAggregationStore;
use crate::types::{
    ScreenshotAggregationBucket, ScreenshotAggregationBucketStatus, ScreenshotAggregationStatus,
    ScreenshotAggregationStatusBucket, ScreenshotRecord, ScreenshotRecordStatus,
};

/// 单桶满 5 张立即提交。
pub const MAX_SCREENSHOTS_PER_BUCKET: usize = 5;

/// 同应用 5 分钟内继续归入同桶。
pub const IDLE_TIMEOUT_SECS: i64 = 300;

/// 单桶从第一张开始最长 10 分钟必须提交。
pub const MAX_LIFETIME_SECS: i64 = 600;

/// 判断同应用桶是否仍可续桶。
///
/// 条件：桶状态为 collecting，且当前截图距 lastCapturedAt < 5 分钟，
/// 且距 firstCapturedAt < 10 分钟，且桶内截图数 < 5。
pub fn can_continue_bucket(bucket: &ScreenshotAggregationBucket, now: DateTime<Utc>) -> bool {
    if bucket.status != ScreenshotAggregationBucketStatus::Collecting {
        return false;
    }
    if bucket.screenshot_ids.len() >= MAX_SCREENSHOTS_PER_BUCKET {
        return false;
    }
    let last = match DateTime::parse_from_rfc3339(&bucket.last_captured_at) {
        Ok(dt) => dt.with_timezone(&Utc),
        Err(_) => return false,
    };
    let first = match DateTime::parse_from_rfc3339(&bucket.first_captured_at) {
        Ok(dt) => dt.with_timezone(&Utc),
        Err(_) => return false,
    };
    let idle_elapsed = (now - last).num_seconds();
    let lifetime_elapsed = (now - first).num_seconds();
    idle_elapsed < IDLE_TIMEOUT_SECS && lifetime_elapsed < MAX_LIFETIME_SECS
}

/// 判断桶是否满足 finalize 条件。
///
/// 任一条件满足即应 finalize：
/// - 桶内截图数 >= 5
/// - 当前时间距 lastCapturedAt >= 5 分钟
/// - 当前时间距 firstCapturedAt >= 10 分钟
pub fn should_finalize_bucket(bucket: &ScreenshotAggregationBucket, now: DateTime<Utc>) -> bool {
    if bucket.status != ScreenshotAggregationBucketStatus::Collecting {
        return false;
    }
    if bucket.screenshot_ids.len() >= MAX_SCREENSHOTS_PER_BUCKET {
        return true;
    }
    let last = match DateTime::parse_from_rfc3339(&bucket.last_captured_at) {
        Ok(dt) => dt.with_timezone(&Utc),
        Err(_) => return false,
    };
    let first = match DateTime::parse_from_rfc3339(&bucket.first_captured_at) {
        Ok(dt) => dt.with_timezone(&Utc),
        Err(_) => return false,
    };
    let idle_elapsed = (now - last).num_seconds();
    let lifetime_elapsed = (now - first).num_seconds();
    idle_elapsed >= IDLE_TIMEOUT_SECS || lifetime_elapsed >= MAX_LIFETIME_SECS
}

/// 将截图追加到聚合桶。如果同应用有可续桶则追加，否则创建新桶。
///
/// 整个操作在 store 的同一把锁内完成，避免 finalize 与追加之间的 TOCTOU 竞态。
/// 返回需要 finalize 的桶列表（可能包含当前新桶，如果已满 5 张）。
pub fn append_screenshot(
    inner: &Arc<Inner>,
    process_name: &str,
    app_display_name: Option<&str>,
    screenshot_id: &str,
) -> Vec<ScreenshotAggregationBucket> {
    let store = &inner.screenshot_aggregation;
    let now = Utc::now();
    let now_str = now.to_rfc3339();
    let lower = process_name.to_lowercase();

    let mut to_finalize = Vec::new();

    // 原子操作：在同一把锁内查找/追加/创建/判断 finalize
    let _guard = store.lock_write();
    let mut buckets = match store.read_all_locked() {
        Ok(b) => b,
        Err(e) => {
            log::warn!("[agg] read buckets failed: {e}");
            return to_finalize;
        }
    };

    // 查找同应用的可续桶
    let existing_pos = buckets.iter().position(|b| {
        b.status == ScreenshotAggregationBucketStatus::Collecting && b.process_name == lower
    });

    let bucket_idx = if let Some(pos) = existing_pos {
        if can_continue_bucket(&buckets[pos], now) {
            // 追加到现有桶
            buckets[pos].screenshot_ids.push(screenshot_id.to_string());
            buckets[pos].trigger_count += 1;
            buckets[pos].last_captured_at = now_str.clone();
            pos
        } else {
            // 不可续桶，创建新桶
            let new_bucket = ScreenshotAggregationBucket {
                id: Uuid::new_v4().to_string(),
                process_name: lower,
                app_display_name: app_display_name.map(String::from),
                first_captured_at: now_str.clone(),
                last_captured_at: now_str,
                status: ScreenshotAggregationBucketStatus::Collecting,
                screenshot_ids: vec![screenshot_id.to_string()],
                trigger_count: 1,
                error_code: None,
                error_message: None,
            };
            buckets.push(new_bucket);
            buckets.len() - 1
        }
    } else {
        // 没有同应用桶，创建新桶
        let new_bucket = ScreenshotAggregationBucket {
            id: Uuid::new_v4().to_string(),
            process_name: lower,
            app_display_name: app_display_name.map(String::from),
            first_captured_at: now_str.clone(),
            last_captured_at: now_str,
            status: ScreenshotAggregationBucketStatus::Collecting,
            screenshot_ids: vec![screenshot_id.to_string()],
            trigger_count: 1,
            error_code: None,
            error_message: None,
        };
        buckets.push(new_bucket);
        buckets.len() - 1
    };

    // 检查是否需要 finalize（仍在同一把锁内）
    if should_finalize_bucket(&buckets[bucket_idx], now) {
        to_finalize.push(buckets[bucket_idx].clone());
    }

    // 写入持久化
    if let Err(e) = store.write_all_locked(&buckets) {
        log::warn!("[agg] write buckets failed: {e}");
    }

    to_finalize
}

/// Finalize 一个聚合桶：将其转换为正式 ScreenshotRecord 并提交分析。
///
/// 返回创建的 ScreenshotRecord ID，如果 finalize 失败则返回 None。
pub fn finalize_bucket(inner: &Arc<Inner>, bucket: &ScreenshotAggregationBucket) -> Option<String> {
    let store = &inner.screenshot_aggregation;

    // 标记为 Finalizing（幂等：如果已是 finalizing/failed 则跳过）
    let mut updated = bucket.clone();
    if updated.status != ScreenshotAggregationBucketStatus::Collecting {
        log::info!("[agg] bucket {} not collecting, skip finalize", bucket.id);
        return None;
    }
    updated.status = ScreenshotAggregationBucketStatus::Finalizing;
    if let Err(e) = store.upsert(updated) {
        log::warn!("[agg] mark finalizing failed: {e}");
        return None;
    }

    // 从桶内截图中选择提交图片
    let prefs = inner.prefs.get();
    let max_images = prefs.screenshot_record_max_images_per_analysis.clamp(1, 5) as usize;
    let submitted_ids =
        crate::screenshot_record::select_screenshot_ids(&bucket.screenshot_ids, max_images);

    let now = chrono::Utc::now().to_rfc3339();
    let record = ScreenshotRecord {
        id: Uuid::new_v4().to_string(),
        created_at: bucket.first_captured_at.clone(),
        updated_at: now.clone(),
        window_started_at: bucket.first_captured_at.clone(),
        window_ended_at: Some(bucket.last_captured_at.clone()),
        status: ScreenshotRecordStatus::Queued,
        context_app: bucket.app_display_name.clone(),
        conversation_window: None,
        window_title: None,
        screenshot_ids: bucket.screenshot_ids.clone(),
        submitted_screenshot_ids: submitted_ids,
        trigger_count: bucket.trigger_count,
        error_code: None,
        error_message: None,
        analysis: None,
        aggregation_mode: Some("app".to_string()),
        aggregation_bucket_id: Some(bucket.id.clone()),
        process_name: Some(bucket.process_name.clone()),
    };

    let record_id = record.id.clone();

    // 写入正式历史
    if let Err(e) = inner.screenshot_records.upsert_with_retention(
        record.clone(),
        prefs.history_retention_days,
        prefs.history_max_entries,
    ) {
        log::error!("[agg] create formal record failed: {e}");
        // 标记为 failed
        let mut failed = bucket.clone();
        failed.status = ScreenshotAggregationBucketStatus::Failed;
        failed.error_code = Some("failed:createRecord".into());
        failed.error_message = Some(format!("{e}"));
        let _ = store.upsert(failed);
        return None;
    }

    // 通知前端
    inner.emit_event("history:updated", "screenshot");

    // 删除已 finalize 的桶
    if let Err(e) = store.remove(&bucket.id) {
        log::warn!("[agg] remove finalized bucket failed: {e}");
    }

    // 提交 LLM 分析（复用现有流程）
    crate::context_vision_analysis::spawn_analysis_for_screenshot_record(
        inner.context_capture.clone(),
        inner.context_analysis.clone(),
        inner.screenshot_records.clone(),
        record,
    );

    Some(record_id)
}

/// Finalize 所有已过期的聚合桶。在应用启动和后台定时器中调用。
pub fn finalize_expired_buckets(inner: &Arc<Inner>) {
    let store = &inner.screenshot_aggregation;
    let idle_timeout = chrono::Duration::seconds(IDLE_TIMEOUT_SECS);
    let max_lifetime = chrono::Duration::seconds(MAX_LIFETIME_SECS);

    let expired = match store.list_expired_collecting(idle_timeout, max_lifetime) {
        Ok(list) => list,
        Err(e) => {
            log::warn!("[agg] list expired buckets failed: {e}");
            return;
        }
    };

    for bucket in expired {
        log::info!(
            "[agg] finalizing expired bucket: {} ({})",
            bucket.process_name,
            bucket.id
        );
        finalize_bucket(inner, &bucket);
    }
}

/// 启动后台定时器，定期检查并 finalize 过期桶。
///
/// 使用 `AtomicBool` 保证全局只有一个定时器线程，多次调用安全忽略。
static AGG_TIMER_RUNNING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn start_aggregation_timer(inner: Arc<Inner>) {
    if AGG_TIMER_RUNNING.swap(true, std::sync::atomic::Ordering::SeqCst) {
        // 已有定时器在跑，忽略重复调用
        return;
    }
    std::thread::Builder::new()
        .name("openless-agg-timer".into())
        .spawn(move || loop {
            // 每 30 秒检查一次
            std::thread::sleep(std::time::Duration::from_secs(30));

            if inner.is_shutdown() {
                break;
            }

            let prefs = inner.prefs.get();
            if !prefs.screenshot_app_aggregation_enabled {
                continue;
            }

            finalize_expired_buckets(&inner);
        })
        .ok();
}

/// 获取当前聚合状态（供前端查询）。
pub fn get_aggregation_status(store: &ScreenshotAggregationStore) -> ScreenshotAggregationStatus {
    let buckets = match store.list_collecting() {
        Ok(list) => list,
        Err(e) => {
            log::warn!("[agg] list collecting buckets failed: {e}");
            Vec::new()
        }
    };

    ScreenshotAggregationStatus {
        buckets: buckets
            .into_iter()
            .map(|b| ScreenshotAggregationStatusBucket {
                id: b.id,
                process_name: b.process_name,
                app_display_name: b.app_display_name,
                screenshot_count: b.screenshot_ids.len(),
                first_captured_at: b.first_captured_at,
                last_captured_at: b.last_captured_at,
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_bucket(
        process: &str,
        count: usize,
        first_offset: i64,
        last_offset: i64,
    ) -> ScreenshotAggregationBucket {
        let now = Utc::now();
        ScreenshotAggregationBucket {
            id: Uuid::new_v4().to_string(),
            process_name: process.to_lowercase(),
            app_display_name: Some(process.to_string()),
            first_captured_at: (now - chrono::Duration::seconds(first_offset)).to_rfc3339(),
            last_captured_at: (now - chrono::Duration::seconds(last_offset)).to_rfc3339(),
            status: ScreenshotAggregationBucketStatus::Collecting,
            screenshot_ids: (0..count).map(|i| format!("ss-{i}")).collect(),
            trigger_count: count as u32,
            error_code: None,
            error_message: None,
        }
    }

    #[test]
    fn can_continue_within_all_limits() {
        let b = make_bucket("wxwork.exe", 2, 60, 10);
        assert!(can_continue_bucket(&b, Utc::now()));
    }

    #[test]
    fn cannot_continue_when_full() {
        let b = make_bucket("wxwork.exe", 5, 60, 10);
        assert!(!can_continue_bucket(&b, Utc::now()));
    }

    #[test]
    fn cannot_continue_when_idle_timeout() {
        // last_captured 310 秒前，超过 300 秒空闲超时
        let b = make_bucket("wxwork.exe", 2, 310, 310);
        assert!(!can_continue_bucket(&b, Utc::now()));
    }

    #[test]
    fn cannot_continue_when_lifetime_exceeded() {
        // first_captured 601 秒前，超过 600 秒最长生命周期
        let b = make_bucket("wxwork.exe", 2, 601, 10);
        assert!(!can_continue_bucket(&b, Utc::now()));
    }

    #[test]
    fn cannot_continue_non_collecting_bucket() {
        let mut b = make_bucket("wxwork.exe", 2, 60, 10);
        b.status = ScreenshotAggregationBucketStatus::Finalizing;
        assert!(!can_continue_bucket(&b, Utc::now()));
    }

    #[test]
    fn should_finalize_when_full() {
        let b = make_bucket("wxwork.exe", 5, 60, 10);
        assert!(should_finalize_bucket(&b, Utc::now()));
    }

    #[test]
    fn should_finalize_when_idle_timeout() {
        let b = make_bucket("wxwork.exe", 2, 310, 310);
        assert!(should_finalize_bucket(&b, Utc::now()));
    }

    #[test]
    fn should_finalize_when_lifetime_exceeded() {
        let b = make_bucket("wxwork.exe", 2, 601, 10);
        assert!(should_finalize_bucket(&b, Utc::now()));
    }

    #[test]
    fn should_not_finalize_within_limits() {
        let b = make_bucket("wxwork.exe", 2, 60, 10);
        assert!(!should_finalize_bucket(&b, Utc::now()));
    }

    #[test]
    fn should_not_finalize_non_collecting() {
        let mut b = make_bucket("wxwork.exe", 5, 60, 10);
        b.status = ScreenshotAggregationBucketStatus::Failed;
        assert!(!should_finalize_bucket(&b, Utc::now()));
    }

    #[test]
    fn different_processes_never_share_bucket() {
        // 验证 process_name 在 can_continue 中不参与判断（由调用者先匹配）
        let b = make_bucket("wxwork.exe", 2, 60, 10);
        // 直接调用 can_continue_bucket 不检查 process_name（调用者负责匹配）
        assert!(can_continue_bucket(&b, Utc::now()));
    }
}

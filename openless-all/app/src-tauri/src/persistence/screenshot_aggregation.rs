#![cfg_attr(target_os = "linux", allow(dead_code, unused_variables))]
//! Screenshot aggregation bucket store: persists pending aggregation buckets
//! that collect screenshots per-process before finalize into formal records.
//!
//! Storage: `screenshot-aggregation-buffer.json` — a JSON array of buckets.

use std::path::PathBuf;

use anyhow::{Context, Result};
use parking_lot::Mutex;

use super::{atomic_write, data_dir, ensure_dir, read_or_default};
use crate::types::{ScreenshotAggregationBucket, ScreenshotAggregationBucketStatus};

const AGGREGATION_BUFFER_FILE: &str = "screenshot-aggregation-buffer.json";

pub struct ScreenshotAggregationStore {
    path: PathBuf,
    lock: Arc<Mutex<()>>,
}

use std::sync::Arc;

impl ScreenshotAggregationStore {
    pub fn new() -> Result<Self> {
        let dir = data_dir()?;
        ensure_dir(&dir)?;
        Ok(Self {
            path: dir.join(AGGREGATION_BUFFER_FILE),
            lock: Arc::new(Mutex::new(())),
        })
    }

    /// Fallback constructor pointing at temp dir (for tests / when data_dir is unavailable).
    pub(crate) fn new_fallback() -> Self {
        Self {
            path: std::env::temp_dir().join("openless_screenshot_aggregation_fallback.json"),
            lock: Arc::new(Mutex::new(())),
        }
    }

    /// Read all pending buckets.
    pub fn list(&self) -> Result<Vec<ScreenshotAggregationBucket>> {
        let _guard = self.lock.lock();
        self.read_locked()
    }

    /// Replace the entire bucket list (used after mutations).
    pub fn save_all(&self, buckets: &[ScreenshotAggregationBucket]) -> Result<()> {
        let _guard = self.lock.lock();
        self.write_locked(buckets)
    }

    /// Upsert a single bucket by id.
    pub fn upsert(&self, bucket: ScreenshotAggregationBucket) -> Result<()> {
        let _guard = self.lock.lock();
        let mut buckets = self.read_locked()?;
        if let Some(pos) = buckets.iter().position(|b| b.id == bucket.id) {
            buckets[pos] = bucket;
        } else {
            buckets.push(bucket);
        }
        self.write_locked(&buckets)
    }

    /// Remove a bucket by id.
    pub fn remove(&self, id: &str) -> Result<()> {
        let _guard = self.lock.lock();
        let mut buckets = self.read_locked()?;
        buckets.retain(|b| b.id != id);
        self.write_locked(&buckets)
    }

    /// Find a collecting bucket for the given process name that can still accept screenshots.
    /// Returns None if no such bucket exists.
    pub fn find_collecting_bucket(&self, process_name: &str) -> Result<Option<ScreenshotAggregationBucket>> {
        let _guard = self.lock.lock();
        let buckets = self.read_locked()?;
        let lower = process_name.to_lowercase();
        Ok(buckets.into_iter().find(|b| {
            b.status == ScreenshotAggregationBucketStatus::Collecting
                && b.process_name == lower
        }))
    }

    /// Return only collecting buckets (for status display).
    pub fn list_collecting(&self) -> Result<Vec<ScreenshotAggregationBucket>> {
        let _guard = self.lock.lock();
        let buckets = self.read_locked()?;
        Ok(buckets
            .into_iter()
            .filter(|b| b.status == ScreenshotAggregationBucketStatus::Collecting)
            .collect())
    }

    /// Return all buckets that are expired and should be finalized.
    pub fn list_expired_collecting(
        &self,
        idle_timeout: chrono::Duration,
        max_lifetime: chrono::Duration,
    ) -> Result<Vec<ScreenshotAggregationBucket>> {
        let _guard = self.lock.lock();
        let buckets = self.read_locked()?;
        let now = chrono::Utc::now();
        Ok(buckets
            .into_iter()
            .filter(|b| {
                if b.status != ScreenshotAggregationBucketStatus::Collecting {
                    return false;
                }
                let last = chrono::DateTime::parse_from_rfc3339(&b.last_captured_at)
                    .map(|dt| dt.with_timezone(&chrono::Utc))
                    .unwrap_or(now);
                let first = chrono::DateTime::parse_from_rfc3339(&b.first_captured_at)
                    .map(|dt| dt.with_timezone(&chrono::Utc))
                    .unwrap_or(now);
                now - last >= idle_timeout || now - first >= max_lifetime
            })
            .collect())
    }

    fn read_locked(&self) -> Result<Vec<ScreenshotAggregationBucket>> {
        read_or_default::<Vec<ScreenshotAggregationBucket>>(&self.path)
    }

    fn write_locked(&self, buckets: &[ScreenshotAggregationBucket]) -> Result<()> {
        let json =
            serde_json::to_vec_pretty(buckets).context("encode screenshot aggregation buffer failed")?;
        atomic_write(&self.path, &json)
    }

    /// 获取写锁。配合 `read_all_locked` / `write_all_locked` 实现跨多个操作的原子性。
    pub fn lock_write(&self) -> parking_lot::MutexGuard<'_, ()> {
        self.lock.lock()
    }

    /// 在持有外部锁时读取所有桶。调用者必须已通过 `lock_write` 获取锁。
    pub fn read_all_locked(&self) -> Result<Vec<ScreenshotAggregationBucket>> {
        self.read_locked()
    }

    /// 在持有外部锁时写入所有桶。调用者必须已通过 `lock_write` 获取锁。
    pub fn write_all_locked(&self, buckets: &[ScreenshotAggregationBucket]) -> Result<()> {
        self.write_locked(buckets)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_store() -> ScreenshotAggregationStore {
        ScreenshotAggregationStore {
            path: std::env::temp_dir().join(format!(
                "openless_agg_test_{}.json",
                uuid::Uuid::new_v4().simple()
            )),
            lock: Arc::new(Mutex::new(())),
        }
    }

    fn make_bucket(process: &str, count: usize) -> ScreenshotAggregationBucket {
        let now = chrono::Utc::now().to_rfc3339();
        ScreenshotAggregationBucket {
            id: uuid::Uuid::new_v4().to_string(),
            process_name: process.to_lowercase(),
            app_display_name: Some(process.to_string()),
            first_captured_at: now.clone(),
            last_captured_at: now,
            status: ScreenshotAggregationBucketStatus::Collecting,
            screenshot_ids: (0..count).map(|i| format!("ss-{i}")).collect(),
            trigger_count: count as u32,
            error_code: None,
            error_message: None,
        }
    }

    #[test]
    fn upsert_creates_and_updates() {
        let store = test_store();
        let b = make_bucket("wxwork.exe", 2);
        let id = b.id.clone();
        store.upsert(b.clone()).unwrap();
        let list = store.list().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, id);

        // Update same bucket
        let mut b2 = b;
        b2.screenshot_ids.push("ss-2".into());
        b2.trigger_count = 3;
        store.upsert(b2).unwrap();
        let list = store.list().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].trigger_count, 3);

        let _ = std::fs::remove_file(&store.path);
    }

    #[test]
    fn remove_deletes_matching_bucket() {
        let store = test_store();
        let b1 = make_bucket("wxwork.exe", 1);
        let b2 = make_bucket("alma.exe", 1);
        let id1 = b1.id.clone();
        store.upsert(b1).unwrap();
        store.upsert(b2).unwrap();
        assert_eq!(store.list().unwrap().len(), 2);
        store.remove(&id1).unwrap();
        assert_eq!(store.list().unwrap().len(), 1);
        let _ = std::fs::remove_file(&store.path);
    }

    #[test]
    fn find_collecting_bucket_matches_lowercase_process() {
        let store = test_store();
        store.upsert(make_bucket("wxwork.exe", 1)).unwrap();
        let found = store.find_collecting_bucket("WXWork.EXE").unwrap();
        assert!(found.is_some());
        assert_eq!(found.unwrap().process_name, "wxwork.exe");

        let not_found = store.find_collecting_bucket("unknown.exe").unwrap();
        assert!(not_found.is_none());
        let _ = std::fs::remove_file(&store.path);
    }

    #[test]
    fn find_collecting_ignores_non_collecting_buckets() {
        let store = test_store();
        let mut b = make_bucket("test.exe", 1);
        b.status = ScreenshotAggregationBucketStatus::Finalizing;
        store.upsert(b).unwrap();
        let found = store.find_collecting_bucket("test.exe").unwrap();
        assert!(found.is_none());
        let _ = std::fs::remove_file(&store.path);
    }

    #[test]
    fn different_processes_get_separate_buckets() {
        let store = test_store();
        store.upsert(make_bucket("wxwork.exe", 1)).unwrap();
        store.upsert(make_bucket("alma.exe", 1)).unwrap();
        let list = store.list().unwrap();
        assert_eq!(list.len(), 2);
        let processes: Vec<_> = list.iter().map(|b| &b.process_name).collect();
        assert!(processes.contains(&&"wxwork.exe".to_string()));
        assert!(processes.contains(&&"alma.exe".to_string()));
        let _ = std::fs::remove_file(&store.path);
    }

    #[test]
    fn save_all_replaces_entire_list() {
        let store = test_store();
        store.upsert(make_bucket("a.exe", 1)).unwrap();
        store.upsert(make_bucket("b.exe", 1)).unwrap();
        assert_eq!(store.list().unwrap().len(), 2);

        let new_buckets = vec![make_bucket("c.exe", 3)];
        store.save_all(&new_buckets).unwrap();
        let list = store.list().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].process_name, "c.exe");
        let _ = std::fs::remove_file(&store.path);
    }
}

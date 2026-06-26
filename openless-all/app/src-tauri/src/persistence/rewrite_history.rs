#![cfg_attr(target_os = "linux", allow(dead_code, unused_variables))]
//! Rewrite history store: newest-first JSON list with count cap.
//!
//! 与 `HistoryStore` 完全隔离，写入独立的 `rewrite-history.json`。
//! 失败请求也写入历史，`error_code` 非 None 时表示失败原因。

use std::path::PathBuf;

use anyhow::{Context, Result};
use parking_lot::Mutex;

use super::{atomic_write, data_dir, ensure_dir, read_or_default, HISTORY_CAP};
use crate::types::RewriteHistoryEntry;

const REWRITE_HISTORY_FILE: &str = "rewrite-history.json";

pub struct RewriteHistoryStore {
    path: PathBuf,
    lock: Mutex<()>,
}

impl RewriteHistoryStore {
    pub fn new() -> Result<Self> {
        let dir = data_dir()?;
        ensure_dir(&dir)?;
        Ok(Self {
            path: dir.join(REWRITE_HISTORY_FILE),
            lock: Mutex::new(()),
        })
    }

    /// 在 data_dir 不可用时构造一个降级实例（指向临时目录），与 `HistoryStore::new_fallback` 对齐。
    pub(crate) fn new_fallback() -> Self {
        Self {
            path: std::env::temp_dir().join("openless_rewrite_history_fallback.json"),
            lock: Mutex::new(()),
        }
    }

    pub fn list(&self) -> Result<Vec<RewriteHistoryEntry>> {
        let _guard = self.lock.lock();
        self.read_locked()
    }

    pub fn append(&self, entry: RewriteHistoryEntry) -> Result<()> {
        let _guard = self.lock.lock();
        let mut entries = self.read_locked()?;
        // Prepend so the newest entry is at index 0, matching `HistoryStore`.
        entries.insert(0, entry);
        if entries.len() > HISTORY_CAP {
            entries.truncate(HISTORY_CAP);
        }
        self.write_locked(&entries)
    }

    pub fn delete(&self, id: &str) -> Result<()> {
        let _guard = self.lock.lock();
        let mut entries = self.read_locked()?;
        let original_len = entries.len();
        entries.retain(|e| e.id != id);
        if entries.len() == original_len {
            return Ok(());
        }
        self.write_locked(&entries)
    }

    pub fn clear(&self) -> Result<()> {
        let _guard = self.lock.lock();
        self.write_locked(&Vec::<RewriteHistoryEntry>::new())
    }

    fn read_locked(&self) -> Result<Vec<RewriteHistoryEntry>> {
        read_or_default::<Vec<RewriteHistoryEntry>>(&self.path)
    }

    fn write_locked(&self, entries: &[RewriteHistoryEntry]) -> Result<()> {
        let json = serde_json::to_vec_pretty(entries).context("encode rewrite history failed")?;
        atomic_write(&self.path, &json)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::InsertStatus;

    fn entry(id: &str, source: &str) -> RewriteHistoryEntry {
        RewriteHistoryEntry {
            id: id.into(),
            created_at: "2026-01-01T00:00:00Z".into(),
            source_text: source.into(),
            rewritten_text: format!("rewritten:{source}"),
            style_pack_id: None,
            style_pack_name: None,
            app_name: None,
            insert_status: InsertStatus::Inserted,
            error_code: None,
            duration_ms: None,
            context_capture: None,
        }
    }

    #[test]
    fn append_then_list_is_newest_first() {
        let store = RewriteHistoryStore::new_fallback();
        let _ = store.clear();
        store.append(entry("a", "one")).unwrap();
        store.append(entry("b", "two")).unwrap();
        let list = store.list().unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].id, "b");
        assert_eq!(list[1].id, "a");
    }

    #[test]
    fn delete_removes_matching_entry() {
        let store = RewriteHistoryStore::new_fallback();
        let _ = store.clear();
        store.append(entry("a", "one")).unwrap();
        store.append(entry("b", "two")).unwrap();
        store.delete("a").unwrap();
        let list = store.list().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, "b");
    }

    #[test]
    fn clear_empties_all_entries() {
        let store = RewriteHistoryStore::new_fallback();
        store.append(entry("a", "one")).unwrap();
        store.clear().unwrap();
        let list = store.list().unwrap();
        assert!(list.is_empty());
    }

    #[test]
    fn truncate_at_history_cap() {
        let store = RewriteHistoryStore::new_fallback();
        let _ = store.clear();
        for i in 0..(HISTORY_CAP + 10) {
            store.append(entry(&format!("id-{i}"), "text")).unwrap();
        }
        let list = store.list().unwrap();
        assert_eq!(list.len(), HISTORY_CAP);
        // newest-first → 第一条是最后写入的
        assert_eq!(list[0].id, format!("id-{}", HISTORY_CAP + 9));
    }
}

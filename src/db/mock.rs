//! In-memory implementation of `HashStore` for fast, isolated unit testing.
//!
//! Provides thread-safe storage using interior mutability (`Arc<RwLock<MockStoreInner>>`),
//! call counters, and programmable error hook injection.

use super::traits::{BlockHash, FileRecord, HashStore, path_to_sqlite_key};
use crate::error::SyncError;
use crate::path_util::RelativePath;
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, RwLock};

/// Type alias for error hook injected into `MockHashStore`.
pub type MockStoreErrorHook = Box<dyn Fn(&str) -> Option<SyncError> + Send + Sync>;

#[derive(Default)]
struct MockStoreInner {
    records: HashMap<String, FileRecord>,
    hashes: HashMap<i64, Vec<BlockHash>>,
    next_id: i64,
    save_file_calls: usize,
    batch_save_calls: usize,
    error_hook: Option<MockStoreErrorHook>,
}

impl std::fmt::Debug for MockStoreInner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MockStoreInner")
            .field("records", &self.records)
            .field("hashes", &self.hashes)
            .field("next_id", &self.next_id)
            .field("save_file_calls", &self.save_file_calls)
            .field("batch_save_calls", &self.batch_save_calls)
            .field("error_hook", &self.error_hook.as_ref().map(|_| "<closure>"))
            .finish()
    }
}

/// In-memory implementation of `HashStore` for fast, isolated unit testing.
#[derive(Debug, Default, Clone)]
pub struct MockHashStore {
    inner: Arc<RwLock<MockStoreInner>>,
}

impl MockHashStore {
    /// Create a new empty in-memory hash store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of calls to `save_file`.
    pub fn save_file_count(&self) -> usize {
        self.inner.read().map(|i| i.save_file_calls).unwrap_or(0)
    }

    /// Number of calls to `save_files_batch`.
    pub fn batch_save_count(&self) -> usize {
        self.inner.read().map(|i| i.batch_save_calls).unwrap_or(0)
    }

    /// Reset call counters.
    pub fn reset_counts(&self) {
        if let Ok(mut inner) = self.inner.write() {
            inner.save_file_calls = 0;
            inner.batch_save_calls = 0;
            inner.next_id = 0;
        }
    }

    /// Set an error hook closure to inject failures for testing.
    pub fn set_error_hook(&self, hook: Option<MockStoreErrorHook>) {
        if let Ok(mut inner) = self.inner.write() {
            inner.error_hook = hook;
        }
    }
}

impl HashStore for MockHashStore {
    fn get_file(&self, path: &Path) -> Result<Option<FileRecord>, SyncError> {
        let key = path_to_sqlite_key(path)?.to_lowercase();
        let inner = self
            .inner
            .read()
            .map_err(|_| SyncError::lock_poison("Mock hash store lock poisoned"))?;
        if let Some(err) = inner.error_hook.as_ref().and_then(|h| h("get_file")) {
            return Err(err);
        }
        Ok(inner.records.get(&key).cloned())
    }

    fn save_file(&self, record: &FileRecord, hashes: &[BlockHash]) -> Result<(), SyncError> {
        let key = record
            .relative_path()
            .as_forward_slash_str()
            .to_ascii_lowercase();
        let mut inner = self
            .inner
            .write()
            .map_err(|_| SyncError::lock_poison("Mock hash store lock poisoned"))?;
        if let Some(err) = inner.error_hook.as_ref().and_then(|h| h("save_file")) {
            return Err(err);
        }
        inner.save_file_calls += 1;

        let id = if let Some(existing) = inner.records.get(&key) {
            existing.id().unwrap_or(1)
        } else {
            let assigned = inner.next_id;
            inner.next_id += 1;
            assigned
        };

        let updated = record.clone().with_id(id);
        inner.records.insert(key, updated);
        inner.hashes.insert(id, hashes.to_vec());
        Ok(())
    }

    fn get_block_hashes(&self, path: &Path) -> Result<Vec<BlockHash>, SyncError> {
        let key = path_to_sqlite_key(path)?.to_lowercase();
        let inner = self
            .inner
            .read()
            .map_err(|_| SyncError::lock_poison("Mock hash store lock poisoned"))?;
        if let Some(err) = inner
            .error_hook
            .as_ref()
            .and_then(|h| h("get_block_hashes"))
        {
            return Err(err);
        }
        if let Some(record) = inner.records.get(&key)
            && let Some(id) = record.id()
        {
            Ok(inner.hashes.get(&id).cloned().unwrap_or_default())
        } else {
            Ok(Vec::new())
        }
    }

    fn delete_file(&self, path: &Path) -> Result<(), SyncError> {
        let key = path_to_sqlite_key(path)?.to_lowercase();
        let mut inner = self
            .inner
            .write()
            .map_err(|_| SyncError::lock_poison("Mock hash store lock poisoned"))?;
        if let Some(err) = inner.error_hook.as_ref().and_then(|h| h("delete_file")) {
            return Err(err);
        }

        let prefix = format!("{}/", key);
        let keys_to_remove: Vec<String> = inner
            .records
            .keys()
            .filter(|k| *k == &key || k.starts_with(&prefix))
            .cloned()
            .collect();

        for k in keys_to_remove {
            if let Some(removed) = inner.records.remove(&k)
                && let Some(id) = removed.id()
            {
                inner.hashes.remove(&id);
            }
        }
        Ok(())
    }

    fn list_files(&self) -> Result<Vec<RelativePath>, SyncError> {
        let inner = self
            .inner
            .read()
            .map_err(|_| SyncError::lock_poison("Mock hash store lock poisoned"))?;
        if let Some(err) = inner.error_hook.as_ref().and_then(|h| h("list_files")) {
            return Err(err);
        }
        let mut keys: Vec<String> = inner.records.keys().cloned().collect();
        keys.sort();
        let mut paths = Vec::with_capacity(keys.len());
        for k in keys {
            paths.push(RelativePath::try_new(k)?);
        }
        Ok(paths)
    }

    fn list_all_records(&self) -> Result<Vec<FileRecord>, SyncError> {
        let inner = self
            .inner
            .read()
            .map_err(|_| SyncError::lock_poison("Mock hash store lock poisoned"))?;
        if let Some(err) = inner
            .error_hook
            .as_ref()
            .and_then(|h| h("list_all_records"))
        {
            return Err(err);
        }
        let records = inner.records.values().cloned().collect();
        Ok(records)
    }

    fn save_files_batch(&self, records: &[(&FileRecord, &[BlockHash])]) -> Result<(), SyncError> {
        let mut inner = self
            .inner
            .write()
            .map_err(|_| SyncError::lock_poison("Mock hash store lock poisoned"))?;
        if let Some(err) = inner
            .error_hook
            .as_ref()
            .and_then(|h| h("save_files_batch"))
        {
            return Err(err);
        }
        inner.batch_save_calls += 1;

        for (record, hashes) in records {
            let key = record
                .relative_path()
                .as_forward_slash_str()
                .to_ascii_lowercase();
            let id = if let Some(existing) = inner.records.get(&key) {
                existing.id().unwrap_or(1)
            } else {
                let assigned = inner.next_id;
                inner.next_id += 1;
                assigned
            };
            let updated = (*record).clone().with_id(id);
            inner.records.insert(key, updated);
            inner.hashes.insert(id, hashes.to_vec());
        }
        Ok(())
    }

    fn delete_files_batch(&self, paths: &[&Path]) -> Result<(), SyncError> {
        let mut inner = self
            .inner
            .write()
            .map_err(|_| SyncError::lock_poison("Mock hash store lock poisoned"))?;
        if let Some(err) = inner
            .error_hook
            .as_ref()
            .and_then(|h| h("delete_files_batch"))
        {
            return Err(err);
        }

        for path in paths {
            let key = path_to_sqlite_key(path)?.to_lowercase();
            let prefix = format!("{}/", key);
            let keys_to_remove: Vec<String> = inner
                .records
                .keys()
                .filter(|k| *k == &key || k.starts_with(&prefix))
                .cloned()
                .collect();

            for k in keys_to_remove {
                if let Some(removed) = inner.records.remove(&k)
                    && let Some(id) = removed.id()
                {
                    inner.hashes.remove(&id);
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mock_hash_store_crud() {
        let store = MockHashStore::new();

        assert!(store.list_files().unwrap().is_empty());

        let record = FileRecord::from_raw("docs/readme.txt", 1024, 999).unwrap();
        let hashes = vec![[0xAAu8; 32]];
        store.save_file(&record, &hashes).unwrap();

        let fetched = store
            .get_file(Path::new("docs/readme.txt"))
            .unwrap()
            .unwrap();
        assert_eq!(fetched.file_size(), 1024);

        let block_hashes = store
            .get_block_hashes(Path::new("docs/readme.txt"))
            .unwrap();
        assert_eq!(block_hashes, vec![[0xAAu8; 32]]);

        assert_eq!(
            store.list_files().unwrap(),
            vec![RelativePath::try_new("docs/readme.txt").unwrap()]
        );

        store.delete_file(Path::new("docs/readme.txt")).unwrap();
        assert!(
            store
                .get_file(Path::new("docs/readme.txt"))
                .unwrap()
                .is_none()
        );
        assert!(store.list_files().unwrap().is_empty());
    }

    #[test]
    fn test_mock_hash_store_list_all_records() {
        let store = MockHashStore::new();
        assert!(store.list_all_records().unwrap().is_empty());
        let r1 = FileRecord::from_raw("b.txt", 200, 2000).unwrap();
        let r2 = FileRecord::from_raw("a.txt", 100, 1000).unwrap();
        store.save_file(&r1, &[]).unwrap();
        store.save_file(&r2, &[]).unwrap();
        let records = store.list_all_records().unwrap();
        assert_eq!(records.len(), 2);
        assert!(
            records
                .iter()
                .any(|r| r.relative_path().as_os_str() == "a.txt" && r.file_size() == 100)
        );
        assert!(
            records
                .iter()
                .any(|r| r.relative_path().as_os_str() == "b.txt" && r.file_size() == 200)
        );
    }

    #[test]
    fn test_mock_hash_store_case_folding_and_counters() {
        let store = MockHashStore::new();
        let record = FileRecord::from_raw("TestFolder/File.TXT", 100, 123456).unwrap();
        store.save_file(&record, &[]).unwrap();
        assert_eq!(store.save_file_count(), 1);
        assert_eq!(store.batch_save_count(), 0);

        // Case-insensitive lookup (COLLATE NOCASE parity)
        let fetched = store.get_file(Path::new("testfolder/file.txt")).unwrap();
        assert!(fetched.is_some());
        assert_eq!(fetched.unwrap().file_size(), 100);

        // Failure hook injection on list_all_records
        store.set_error_hook(Some(Box::new(|op| {
            if op == "list_all_records" {
                Some(SyncError::lock_poison("Simulated list crash"))
            } else {
                None
            }
        })));
        assert!(store.list_all_records().is_err());
    }

    #[test]
    fn test_mock_hash_store_list_all_records_flat_vec() {
        let store = MockHashStore::new();
        let r1 = FileRecord::from_raw("folder/alpha.txt", 1024, 100).unwrap();
        let r2 = FileRecord::from_raw("folder/beta.txt", 2048, 200).unwrap();
        let r3 = FileRecord::from_raw("gamma.txt", 4096, 300).unwrap();

        store.save_file(&r1, &[]).unwrap();
        store.save_file(&r2, &[]).unwrap();
        store.save_file(&r3, &[]).unwrap();

        let records: Vec<FileRecord> = store.list_all_records().unwrap();
        assert_eq!(records.len(), 3);

        let mut paths: Vec<String> = records
            .iter()
            .map(|r| r.relative_path().to_string_lossy().replace('\\', "/"))
            .collect();
        paths.sort();
        assert_eq!(
            paths,
            vec!["folder/alpha.txt", "folder/beta.txt", "gamma.txt"]
        );
    }
}

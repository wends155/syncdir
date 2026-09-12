//! SQLite implementation of `HashStore`.
//!
//! Provides the `SqliteHashStore` engine, managing SQLite connections in WAL mode,
//! statement caching, schema verification, single-transaction atomic deletions,
//! and strongly-typed metadata/signature persistence.

use super::traits::{BlockHash, FileRecord, HashStore, StoreConfig, path_to_sqlite_key};
use crate::error::SyncError;
use crate::path_util::RelativePath;
use rusqlite::{Connection, params};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// SQLite implementation of `HashStore`.
pub struct SqliteHashStore {
    conn: Mutex<Connection>,
}

impl SqliteHashStore {
    /// Generate deterministic cache database path for a target destination directory.
    ///
    /// Naming format is `sigcache_<blake3_hex>.db` within `app_dir`.
    ///
    /// # Panics
    ///
    /// Does not panic.
    #[must_use]
    pub fn cache_db_path(app_dir: &Path, target_dest: &Path) -> PathBuf {
        let dest_str = target_dest.to_string_lossy();
        let hash = blake3::hash(dest_str.as_bytes());
        app_dir.join(format!("sigcache_{}.db", hash.to_hex()))
    }

    /// Open (or create) the SQLite database and initialize the schema.
    ///
    /// Enforces foreign keys, creates tables if missing, and validates
    /// that cached configuration parameters match the active config.
    /// If they differ, all cached file data is purged.
    ///
    /// # Errors
    /// Returns `SyncError::Db` on any SQLite failure.
    pub fn new(db_path: &Path, config: StoreConfig) -> Result<Self, SyncError> {
        let store_cfg = config;
        let conn = Connection::open(db_path)?;
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA foreign_keys = ON;
             PRAGMA temp_store = MEMORY;",
        )?;
        let store = SqliteHashStore {
            conn: Mutex::new(conn),
        };
        store.init_schema()?;
        store.enforce_metadata(store_cfg)?;
        Ok(store)
    }

    #[cfg(test)]
    pub(crate) fn conn(&self) -> Result<std::sync::MutexGuard<'_, Connection>, SyncError> {
        self.conn
            .lock()
            .map_err(|_| SyncError::lock_poison("DB lock poisoned"))
    }

    #[cfg(not(test))]
    fn conn(&self) -> Result<std::sync::MutexGuard<'_, Connection>, SyncError> {
        self.conn
            .lock()
            .map_err(|_| SyncError::lock_poison("DB lock poisoned"))
    }

    fn init_schema(&self) -> Result<(), SyncError> {
        let conn = self.conn()?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS file_metadata (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                relative_path TEXT NOT NULL UNIQUE COLLATE NOCASE,
                file_size INTEGER NOT NULL,
                last_modified INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS block_hashes (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                file_id INTEGER NOT NULL,
                block_index INTEGER NOT NULL,
                hash BLOB NOT NULL,
                FOREIGN KEY(file_id) REFERENCES file_metadata(id) ON DELETE CASCADE
            );
            CREATE UNIQUE INDEX IF NOT EXISTS idx_block_hashes_file_block
                ON block_hashes (file_id, block_index);
            CREATE TABLE IF NOT EXISTS db_metadata (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );",
        )?;
        Ok(())
    }

    fn enforce_metadata(&self, config: StoreConfig) -> Result<(), SyncError> {
        let cached_block_size = self.get_meta_value("block_size_bytes")?;
        let cached_threshold = self.get_meta_value("block_sync_threshold_bytes")?;
        let cached_version = self.get_meta_value("db_version")?;

        let current_block_size = config.block_size_bytes().to_string();
        let current_threshold = config.block_sync_threshold_bytes().to_string();
        let current_version = "5";

        // Treat any missing key or mismatch as requiring a full purge
        let needs_purge = match (cached_block_size, cached_threshold, cached_version) {
            (Some(b), Some(t), Some(v)) => {
                b != current_block_size || t != current_threshold || v != current_version
            }
            (None, None, None) => false, // Fresh database, no purge needed
            _ => true, // Partial metadata/old schema version = corrupted/migration needed
        };

        if needs_purge {
            let conn = self.conn()?;
            conn.execute_batch(
                "DROP TABLE IF EXISTS block_hashes;
                 DROP TABLE IF EXISTS file_metadata;",
            )?;
            drop(conn);
            self.init_schema()?;
        }

        self.set_meta_value("block_size_bytes", &current_block_size)?;
        self.set_meta_value("block_sync_threshold_bytes", &current_threshold)?;
        self.set_meta_value("db_version", current_version)?;
        Ok(())
    }

    fn get_meta_value(&self, key: &str) -> Result<Option<String>, SyncError> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare_cached("SELECT value FROM db_metadata WHERE key = ?")?;
        let mut rows = stmt.query(params![key])?;
        if let Some(row) = rows.next()? {
            let val: String = row.get(0)?;
            Ok(Some(val))
        } else {
            Ok(None)
        }
    }

    fn set_meta_value(&self, key: &str, value: &str) -> Result<(), SyncError> {
        let conn = self.conn()?;
        let mut stmt =
            conn.prepare_cached("INSERT OR REPLACE INTO db_metadata (key, value) VALUES (?, ?)")?;
        stmt.execute(params![key, value])?;
        Ok(())
    }
}

impl HashStore for SqliteHashStore {
    #[tracing::instrument(
        skip(self),
        fields(path = %path.display()),
        level = "debug"
    )]
    fn get_file(&self, path: &Path) -> Result<Option<FileRecord>, SyncError> {
        let key = path_to_sqlite_key(path)?;
        let conn = self.conn()?;
        let mut stmt = conn.prepare_cached(
            "SELECT id, relative_path, file_size, last_modified \
             FROM file_metadata WHERE relative_path = ?",
        )?;
        let mut rows = stmt.query(params![key])?;
        if let Some(row) = rows.next()? {
            let id: i64 = row.get(0)?;
            let path_str: String = row.get(1)?;
            let size_i64: i64 = row.get(2)?;
            let last_modified: i64 = row.get(3)?;
            let rel = RelativePath::from_sanitized_unchecked(PathBuf::from(path_str));
            Ok(Some(
                FileRecord::new(rel, size_i64.max(0) as u64, last_modified).with_id(id),
            ))
        } else {
            Ok(None)
        }
    }

    #[tracing::instrument(
        skip(self, hashes),
        fields(path = %record.relative_path().as_path().display()),
        level = "debug"
    )]
    fn save_file(&self, record: &FileRecord, hashes: &[BlockHash]) -> Result<(), SyncError> {
        let key = record.relative_path().as_forward_slash_str();
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;

        // UPSERT preserves the rowid on conflict, keeping FK references stable.
        // RETURNING id retrieves the rowid in a single round-trip.
        let file_id: i64 = tx.query_row(
            "INSERT INTO file_metadata (relative_path, file_size, last_modified) \
             VALUES (?1, ?2, ?3) \
             ON CONFLICT(relative_path) DO UPDATE SET \
               relative_path = excluded.relative_path, \
               file_size = excluded.file_size, \
               last_modified = excluded.last_modified \
             RETURNING id",
            params![key, record.file_size() as i64, record.last_modified()],
            |row| row.get(0),
        )?;

        // Upsert block hashes (preserve unchanged rows, update modified)
        {
            let mut stmt = tx.prepare_cached(
                "INSERT INTO block_hashes (file_id, block_index, hash) \
                 VALUES (?1, ?2, ?3) \
                 ON CONFLICT(file_id, block_index) DO UPDATE SET hash = excluded.hash \
                 WHERE block_hashes.hash != excluded.hash",
            )?;
            for (idx, hash) in hashes.iter().enumerate() {
                stmt.execute(params![file_id, idx as i64, hash.as_slice()])?;
            }
        }
        // Prune trailing block hashes if file shrank
        tx.execute(
            "DELETE FROM block_hashes WHERE file_id = ?1 AND block_index >= ?2",
            params![file_id, hashes.len() as i64],
        )?;
        tx.commit()?;
        Ok(())
    }

    fn save_files_batch(&self, records: &[(&FileRecord, &[BlockHash])]) -> Result<(), SyncError> {
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        {
            let mut meta_stmt = tx.prepare_cached(
                "INSERT INTO file_metadata (relative_path, file_size, last_modified) \
                 VALUES (?1, ?2, ?3) \
                 ON CONFLICT(relative_path) DO UPDATE SET \
                   relative_path = excluded.relative_path, \
                   file_size = excluded.file_size, \
                   last_modified = excluded.last_modified \
                 RETURNING id",
            )?;
            let mut hash_stmt = tx.prepare_cached(
                "INSERT INTO block_hashes (file_id, block_index, hash) \
                 VALUES (?1, ?2, ?3) \
                 ON CONFLICT(file_id, block_index) DO UPDATE SET hash = excluded.hash \
                 WHERE block_hashes.hash != excluded.hash",
            )?;
            let mut prune_stmt = tx.prepare_cached(
                "DELETE FROM block_hashes WHERE file_id = ?1 AND block_index >= ?2",
            )?;

            for (record, hashes) in records {
                let key = record.relative_path().as_forward_slash_str();
                let file_id: i64 = meta_stmt.query_row(
                    params![key, record.file_size() as i64, record.last_modified()],
                    |row| row.get(0),
                )?;
                for (idx, hash) in hashes.iter().enumerate() {
                    hash_stmt.execute(params![file_id, idx as i64, hash.as_slice()])?;
                }
                prune_stmt.execute(params![file_id, hashes.len() as i64])?;
            }
            drop(meta_stmt);
            drop(hash_stmt);
            drop(prune_stmt);
        }
        tx.commit()?;
        Ok(())
    }

    fn get_block_hashes(&self, path: &Path) -> Result<Vec<BlockHash>, SyncError> {
        let key = path_to_sqlite_key(path)?;
        let conn = self.conn()?;
        let mut stmt = conn.prepare_cached(
            "SELECT b.hash FROM block_hashes b \
             JOIN file_metadata f ON b.file_id = f.id \
             WHERE f.relative_path = ? \
             ORDER BY b.block_index ASC",
        )?;
        let mut rows = stmt.query(params![key])?;
        let mut hashes = Vec::with_capacity(64);
        while let Some(row) = rows.next()? {
            let val_ref = row.get_ref(0)?;
            let hash_blob = val_ref
                .as_blob()
                .map_err(|e| SyncError::db_with_source("Failed to read block hash blob", e))?;
            let hash: BlockHash = hash_blob.try_into().map_err(|e| {
                SyncError::db_with_source(
                    format!(
                        "Invalid block hash length: {} bytes (expected 32)",
                        hash_blob.len()
                    ),
                    e,
                )
            })?;
            hashes.push(hash);
        }
        Ok(hashes)
    }

    #[tracing::instrument(
        skip(self),
        fields(path = %path.display()),
        level = "debug"
    )]
    fn delete_file(&self, path: &Path) -> Result<(), SyncError> {
        self.delete_files_batch(&[path])
    }

    fn delete_files_batch(&self, paths: &[&Path]) -> Result<(), SyncError> {
        if paths.is_empty() {
            return Ok(());
        }
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        {
            let mut stmt_exact =
                tx.prepare_cached("DELETE FROM file_metadata WHERE relative_path = ?1")?;
            let mut stmt_prefix = tx.prepare_cached(
                "DELETE FROM file_metadata WHERE relative_path >= ?1 AND relative_path < ?2",
            )?;
            for path in paths {
                let key = path_to_sqlite_key(path)?;
                stmt_exact.execute(params![key])?;
                let prefix_start = format!("{}/", key);
                let prefix_end = format!("{}0", key);
                stmt_prefix.execute(params![prefix_start, prefix_end])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    fn list_files(&self) -> Result<Vec<RelativePath>, SyncError> {
        let conn = self.conn()?;
        let mut stmt = conn
            .prepare_cached("SELECT relative_path FROM file_metadata ORDER BY relative_path ASC")?;
        let mut rows = stmt.query([])?;
        let mut paths = Vec::new();
        while let Some(row) = rows.next()? {
            let key: String = row.get(0)?;
            paths.push(RelativePath::try_new(key)?);
        }
        Ok(paths)
    }

    fn list_all_records(&self) -> Result<Vec<FileRecord>, SyncError> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare_cached(
            "SELECT id, relative_path, file_size, last_modified FROM file_metadata",
        )?;
        let mut rows = stmt.query([])?;
        let mut records = Vec::new();
        while let Some(row) = rows.next()? {
            let id: i64 = row.get(0)?;
            let rel_str: String = row.get(1)?;
            let file_size: i64 = row.get(2)?;
            let last_modified: i64 = row.get(3)?;
            let rel_path = PathBuf::from(rel_str);
            let rel = RelativePath::from_sanitized_unchecked(rel_path);
            records.push(FileRecord::new(rel, file_size.max(0) as u64, last_modified).with_id(id));
        }
        Ok(records)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    fn dummy_store_config(block_size: u64) -> StoreConfig {
        StoreConfig::new(block_size, block_size * 2).expect("test block_size must be > 0")
    }

    #[test]
    fn test_save_get_delete_with_cascade() {
        let temp = NamedTempFile::new().unwrap();
        let store = SqliteHashStore::new(temp.path(), dummy_store_config(1024)).unwrap();

        let record = FileRecord::from_raw("docs/spec.txt", 2048, 1234567890).unwrap();
        let hashes = vec![[1u8; 32], [2u8; 32]];

        store.save_file(&record, &hashes).unwrap();

        let fetched = store.get_file(Path::new("docs/spec.txt")).unwrap().unwrap();
        let file_id = fetched.id().unwrap();
        assert_eq!(
            fetched.relative_path().as_path(),
            Path::new("docs/spec.txt")
        );
        assert_eq!(fetched.file_size(), 2048);
        assert_eq!(fetched.last_modified(), 1234567890);

        let fetched_hashes = store.get_block_hashes(Path::new("docs/spec.txt")).unwrap();
        assert_eq!(fetched_hashes.len(), 2);
        assert_eq!(fetched_hashes[0], [1u8; 32]);
        assert_eq!(fetched_hashes[1], [2u8; 32]);

        // Verify foreign key cascade delete
        store.delete_file(Path::new("docs/spec.txt")).unwrap();
        assert!(
            store
                .get_file(Path::new("docs/spec.txt"))
                .unwrap()
                .is_none()
        );

        let count: i64 = store
            .conn()
            .unwrap()
            .query_row(
                "SELECT count(*) FROM block_hashes WHERE file_id = ?",
                params![file_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn test_upsert_preserves_rowid() {
        let temp = NamedTempFile::new().unwrap();
        let store = SqliteHashStore::new(temp.path(), dummy_store_config(1024)).unwrap();

        let record = FileRecord::from_raw("test.bin", 100, 1000).unwrap();
        store.save_file(&record, &[[1u8; 32]]).unwrap();
        let id1 = store
            .get_file(Path::new("test.bin"))
            .unwrap()
            .unwrap()
            .id()
            .unwrap();

        // Update same file — rowid should be preserved
        let updated = FileRecord::from_raw("test.bin", 200, 2000).unwrap();
        store.save_file(&updated, &[[2u8; 32], [3u8; 32]]).unwrap();
        let fetched = store.get_file(Path::new("test.bin")).unwrap().unwrap();
        assert_eq!(fetched.id().unwrap(), id1); // Same rowid
        assert_eq!(fetched.file_size(), 200);

        let hashes = store.get_block_hashes(Path::new("test.bin")).unwrap();
        assert_eq!(hashes.len(), 2);
    }

    #[test]
    fn test_db_config_invalidation() {
        let temp = NamedTempFile::new().unwrap();

        // Open with config A and save a file
        {
            let store = SqliteHashStore::new(temp.path(), dummy_store_config(1024)).unwrap();
            let record = FileRecord::from_raw("test.bin", 100, 9999).unwrap();
            store.save_file(&record, &[[7u8; 32]]).unwrap();
            assert!(store.get_file(Path::new("test.bin")).unwrap().is_some());
        }

        // Open with config B (different block size) — cache should be purged
        {
            let store = SqliteHashStore::new(temp.path(), dummy_store_config(512)).unwrap();
            assert!(store.get_file(Path::new("test.bin")).unwrap().is_none());
        }
    }

    #[test]
    fn test_list_files() {
        let temp = NamedTempFile::new().unwrap();
        let store = SqliteHashStore::new(temp.path(), dummy_store_config(1024)).unwrap();

        // Empty database
        assert!(store.list_files().unwrap().is_empty());

        // Insert two files
        let r1 = FileRecord::from_raw("b_second.txt", 100, 1000).unwrap();
        let r2 = FileRecord::from_raw("a_first.txt", 200, 2000).unwrap();
        store.save_file(&r1, &[[1u8; 32]]).unwrap();
        store.save_file(&r2, &[[2u8; 32]]).unwrap();

        let files = store.list_files().unwrap();
        assert_eq!(
            files,
            vec![
                RelativePath::try_new("a_first.txt").unwrap(),
                RelativePath::try_new("b_second.txt").unwrap(),
            ]
        );

        // After delete, removed file is gone
        store.delete_file(Path::new("a_first.txt")).unwrap();
        let files = store.list_files().unwrap();
        assert_eq!(files, vec![RelativePath::try_new("b_second.txt").unwrap()]);
    }

    #[test]
    fn test_get_block_hashes_by_path_known_and_unknown() {
        let temp = NamedTempFile::new().unwrap();
        let store = SqliteHashStore::new(temp.path(), dummy_store_config(1024)).unwrap();
        let rec = FileRecord::from_raw("data/sample.bin", 2048, 5000).unwrap();
        let hashes = vec![[0x11u8; 32], [0x22u8; 32]];
        store.save_file(&rec, &hashes).unwrap();

        let fetched = store
            .get_block_hashes(Path::new("data/sample.bin"))
            .unwrap();
        assert_eq!(fetched, hashes);

        let unknown = store.get_block_hashes(Path::new("unknown.bin")).unwrap();
        assert!(unknown.is_empty());
    }

    #[test]
    fn test_save_file_upsert_single_block_update() {
        let temp = NamedTempFile::new().unwrap();
        let store = SqliteHashStore::new(temp.path(), dummy_store_config(1024)).unwrap();
        let rec = FileRecord::from_raw("delta.bin", 3072, 1000).unwrap();
        let initial = vec![[0x11u8; 32], [0x22u8; 32], [0x33u8; 32]];
        store.save_file(&rec, &initial).unwrap();
        let initial_id = store
            .get_file(Path::new("delta.bin"))
            .unwrap()
            .unwrap()
            .id()
            .unwrap();
        let updated = vec![[0x11u8; 32], [0xFAu8; 32], [0x33u8; 32]];
        let updated_rec = FileRecord::from_raw("delta.bin", 3072, 2000).unwrap();
        store.save_file(&updated_rec, &updated).unwrap();
        let after_id = store
            .get_file(Path::new("delta.bin"))
            .unwrap()
            .unwrap()
            .id()
            .unwrap();
        assert_eq!(initial_id, after_id, "UPSERT must preserve stable row ID");
        let hashes = store.get_block_hashes(Path::new("delta.bin")).unwrap();
        assert_eq!(hashes[1], [0xFAu8; 32], "Middle block must be updated");
        assert_eq!(hashes[0], [0x11u8; 32], "First block must be unchanged");
    }

    #[test]
    fn test_delete_directory_cascades_child_records() {
        let temp = NamedTempFile::new().unwrap();
        let store = SqliteHashStore::new(temp.path(), dummy_store_config(1024)).unwrap();
        let r1 = FileRecord::from_raw("dir/sub/file1.txt", 100, 1000).unwrap();
        let r2 = FileRecord::from_raw("dir/file2.txt", 200, 2000).unwrap();
        let r3 = FileRecord::from_raw("other/file3.txt", 300, 3000).unwrap();
        store.save_file(&r1, &[[1u8; 32]]).unwrap();
        store.save_file(&r2, &[[2u8; 32]]).unwrap();
        store.save_file(&r3, &[[3u8; 32]]).unwrap();
        store.delete_file(Path::new("dir")).unwrap();
        assert!(
            store
                .get_file(Path::new("dir/sub/file1.txt"))
                .unwrap()
                .is_none()
        );
        assert!(
            store
                .get_file(Path::new("dir/file2.txt"))
                .unwrap()
                .is_none()
        );
        assert!(
            store
                .get_file(Path::new("other/file3.txt"))
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn test_delete_file_escapes_like_wildcards() {
        let temp = NamedTempFile::new().unwrap();
        let store = SqliteHashStore::new(temp.path(), dummy_store_config(1024)).unwrap();

        let r1 = FileRecord::from_raw("test_1/file.txt", 100, 1000).unwrap();
        let r2 = FileRecord::from_raw("test-1/file.txt", 200, 2000).unwrap();
        let r3 = FileRecord::from_raw("test%1/file.txt", 300, 3000).unwrap();
        let r4 = FileRecord::from_raw("test_1_extra/file.txt", 400, 4000).unwrap();

        store.save_file(&r1, &[[1u8; 32]]).unwrap();
        store.save_file(&r2, &[[2u8; 32]]).unwrap();
        store.save_file(&r3, &[[3u8; 32]]).unwrap();
        store.save_file(&r4, &[[4u8; 32]]).unwrap();

        // Delete "test_1" directory: should delete only "test_1/file.txt", NOT "test-1/file.txt", "test%1/file.txt", or "test_1_extra/file.txt"
        store.delete_file(Path::new("test_1")).unwrap();

        assert!(
            store
                .get_file(Path::new("test_1/file.txt"))
                .unwrap()
                .is_none(),
            "test_1/file.txt should have been deleted"
        );
        assert!(
            store
                .get_file(Path::new("test-1/file.txt"))
                .unwrap()
                .is_some(),
            "test-1/file.txt must NOT be deleted by test_1 delete"
        );
        assert!(
            store
                .get_file(Path::new("test%1/file.txt"))
                .unwrap()
                .is_some(),
            "test%1/file.txt must NOT be deleted by test_1 delete"
        );
        assert!(
            store
                .get_file(Path::new("test_1_extra/file.txt"))
                .unwrap()
                .is_some(),
            "test_1_extra/file.txt must NOT be deleted by test_1 delete"
        );
    }

    #[test]
    fn test_delete_file_sargable_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let store = SqliteHashStore::new(
            &dir.path().join("test.db"),
            StoreConfig::new(1_048_576, 10_485_760).unwrap(),
        )
        .unwrap();
        // Insert a file and its "child" directory file
        let parent = FileRecord::from_raw("docs", 100, 1000).unwrap();
        let child = FileRecord::from_raw("docs/readme.md", 200, 2000).unwrap();
        let other = FileRecord::from_raw("src/main.rs", 50, 500).unwrap();
        store.save_file(&parent, &[]).unwrap();
        store.save_file(&child, &[]).unwrap();
        store.save_file(&other, &[]).unwrap();
        // Delete the "docs" directory
        store.delete_file(Path::new("docs")).unwrap();
        // Both docs and docs/readme.md should be gone
        let remaining = store.list_files().unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0], PathBuf::from("src/main.rs"));
    }

    #[test]
    fn test_batch_save_transaction() {
        let dir = tempfile::tempdir().unwrap();
        let store = SqliteHashStore::new(
            &dir.path().join("test.db"),
            StoreConfig::new(1_048_576, 10_485_760).unwrap(),
        )
        .unwrap();
        let records: Vec<(FileRecord, Vec<BlockHash>)> = (0..100)
            .map(|i| {
                (
                    FileRecord::from_raw(
                        format!("file_{}.txt", i),
                        (i as u64) * 100,
                        (i as i64) * 1000,
                    )
                    .unwrap(),
                    vec![],
                )
            })
            .collect();
        let batch: Vec<(&FileRecord, &[BlockHash])> =
            records.iter().map(|(r, h)| (r, h.as_slice())).collect();
        store.save_files_batch(&batch).unwrap();
        let all = store.list_all_records().unwrap();
        assert_eq!(all.len(), 100);
    }

    #[test]
    fn test_delete_files_batch_transaction() {
        let dir = tempfile::tempdir().unwrap();
        let store = SqliteHashStore::new(
            &dir.path().join("test.db"),
            StoreConfig::new(1_048_576, 10_485_760).unwrap(),
        )
        .unwrap();
        for i in 0..10 {
            let record = FileRecord::from_raw(
                format!("file_{}.txt", i),
                (i as u64) * 100,
                (i as i64) * 1000,
            )
            .unwrap();
            store.save_file(&record, &[]).unwrap();
        }
        assert_eq!(store.list_files().unwrap().len(), 10);

        let to_delete = [
            Path::new("file_1.txt"),
            Path::new("file_3.txt"),
            Path::new("file_5.txt"),
        ];
        store.delete_files_batch(&to_delete).unwrap();

        let remaining = store.list_files().unwrap();
        assert_eq!(remaining.len(), 7);
        assert!(store.get_file(Path::new("file_1.txt")).unwrap().is_none());
        assert!(store.get_file(Path::new("file_2.txt")).unwrap().is_some());
    }

    #[test]
    fn test_collate_nocase_lookup_and_delete() {
        let dir = tempfile::tempdir().unwrap();
        let store = SqliteHashStore::new(
            &dir.path().join("test.db"),
            StoreConfig::new(1_048_576, 10_485_760).unwrap(),
        )
        .unwrap();
        let record = FileRecord::from_raw("MyFile.TXT", 500, 1000).unwrap();
        store.save_file(&record, &[]).unwrap();

        // Lookup with different case should find it
        let found = store.get_file(Path::new("myfile.txt")).unwrap();
        assert!(found.is_some());
        assert!(found.unwrap().id().is_some());

        // Delete with different case should remove it
        store.delete_file(Path::new("MYFILE.TXT")).unwrap();
        assert!(store.get_file(Path::new("MyFile.TXT")).unwrap().is_none());
    }

    #[test]
    fn test_sqlite_cache_db_path() {
        let app_dir = Path::new(r"C:\AppData\syncdir");
        let target = Path::new(r"\\server\share\folder");
        let expected_hash = blake3::hash(target.to_string_lossy().as_bytes());
        let expected_path = app_dir.join(format!("sigcache_{}.db", expected_hash.to_hex()));
        assert_eq!(
            SqliteHashStore::cache_db_path(app_dir, target),
            expected_path
        );
    }

    #[test]
    fn test_save_files_batch_statement_caching_and_casing_update() {
        use std::path::Path;
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let db_path = dir.path().join("batch_test.db");
        let store_cfg = StoreConfig::new(1024 * 1024, 10 * 1024 * 1024).unwrap();
        let store = SqliteHashStore::new(&db_path, store_cfg).unwrap();

        let mut records = Vec::with_capacity(500);
        let mut hashes_pool = Vec::with_capacity(500);
        for i in 0..500 {
            let rec =
                FileRecord::from_raw(format!("data/file_{:03}.bin", i), 2048, 1000 + i as i64)
                    .unwrap();
            let h1: BlockHash = [i as u8; 32];
            let mut h2: BlockHash = [0u8; 32];
            h2[0] = (i % 256) as u8;
            h2[31] = 0xFF;
            records.push(rec);
            hashes_pool.push(vec![h1, h2]);
        }

        let batch_slices: Vec<(&FileRecord, &[BlockHash])> = records
            .iter()
            .zip(hashes_pool.iter())
            .map(|(r, h)| (r, h.as_slice()))
            .collect();

        store
            .save_files_batch(&batch_slices)
            .expect("Batch insert of 500 records with cached prepared statements must succeed");

        assert_eq!(store.list_all_records().unwrap().len(), 500);

        let initial_h0 = store
            .get_block_hashes(Path::new("data/file_000.bin"))
            .unwrap();
        assert_eq!(initial_h0.len(), 2);
        assert_eq!(initial_h0[0], [0u8; 32]);

        let cased_rec_0 = FileRecord::from_raw("DATA/FILE_000.BIN", 4096, 5000).unwrap();
        let cased_rec_250 = FileRecord::from_raw("data/File_250.Bin", 8192, 6000).unwrap();
        let new_h0 = vec![[0xAA; 32]];
        let new_h250 = vec![[0xBB; 32], [0xCC; 32], [0xDD; 32]];

        let update_batch = vec![
            (&cased_rec_0, new_h0.as_slice()),
            (&cased_rec_250, new_h250.as_slice()),
        ];

        store
            .save_files_batch(&update_batch)
            .expect("Batch update must succeed");

        let queried_rec_0 = store
            .get_file(Path::new("DATA/FILE_000.BIN"))
            .unwrap()
            .unwrap();
        assert_eq!(
            queried_rec_0.relative_path().as_path(),
            Path::new("DATA/FILE_000.BIN")
        );
        assert_eq!(queried_rec_0.file_size(), 4096);

        let queried_rec_250 = store
            .get_file(Path::new("data/File_250.Bin"))
            .unwrap()
            .unwrap();
        assert_eq!(
            queried_rec_250.relative_path().as_path(),
            Path::new("data/File_250.Bin")
        );
        assert_eq!(queried_rec_250.file_size(), 8192);

        let updated_h250 = store
            .get_block_hashes(Path::new("data/File_250.Bin"))
            .unwrap();
        assert_eq!(updated_h250.len(), 3);
        assert_eq!(updated_h250[0], [0xBB; 32]);
    }

    #[test]
    fn test_get_block_hashes_multi_block_preallocation() {
        use std::path::Path;
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let db_path = dir.path().join("prealloc_test.db");
        let store_cfg = StoreConfig::new(1024 * 1024, 10 * 1024 * 1024).unwrap();
        let store = SqliteHashStore::new(&db_path, store_cfg).unwrap();

        let block_count = 16usize;
        let record =
            FileRecord::from_raw("large_file.dat", (block_count * 1024 * 1024) as u64, 12345)
                .unwrap();
        let expected_hashes: Vec<BlockHash> = (0..block_count)
            .map(|i| {
                let mut h = [0u8; 32];
                h[0] = i as u8;
                h[15] = 0xAA;
                h[31] = (255 - i) as u8;
                h
            })
            .collect();

        store.save_file(&record, &expected_hashes).unwrap();

        let retrieved = store
            .get_block_hashes(Path::new("large_file.dat"))
            .expect("Querying block hashes for multi-block file must succeed");

        assert_eq!(retrieved.len(), block_count);
        assert!(
            retrieved.capacity() >= 64,
            "Vector capacity must be pre-allocated to at least 64"
        );

        for (idx, (actual, expected)) in retrieved.iter().zip(expected_hashes.iter()).enumerate() {
            assert_eq!(actual, expected, "Block hash at index {} must match", idx);
        }

        let missing = store
            .get_block_hashes(Path::new("nonexistent.dat"))
            .unwrap();
        assert!(missing.is_empty());
    }

    #[test]
    fn test_hash_store_list_files_returns_relative_paths() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let store = SqliteHashStore::new(&db_path, StoreConfig::new(1024, 4096).unwrap()).unwrap();
        let rel = RelativePath::try_new("sub/file.txt").unwrap();
        let record = FileRecord::new(rel.clone(), 100, 1000);
        store.save_file(&record, &[]).unwrap();

        let files: Vec<RelativePath> = store.list_files().unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0], rel);
    }

    #[test]
    fn test_unicode_superscript_path_and_crud_fidelity() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let store = SqliteHashStore::new(&db_path, StoreConfig::new(1024, 4096).unwrap()).unwrap();

        // Exact Unicode filenames: doc¹.txt (superscript 1: U+00B9) vs doc1.txt (ASCII 1)
        let r_super = FileRecord::from_raw("notes/doc¹.txt", 1024, 100).unwrap();
        let r_ascii = FileRecord::from_raw("notes/doc1.txt", 2048, 200).unwrap();

        store.save_file(&r_super, &[]).unwrap();
        store.save_file(&r_ascii, &[]).unwrap();

        // Both records exist independently without collision
        let fetched_super = store.get_file(Path::new("notes/doc¹.txt")).unwrap();
        let fetched_ascii = store.get_file(Path::new("notes/doc1.txt")).unwrap();

        assert!(fetched_super.is_some());
        assert!(fetched_ascii.is_some());

        let fs = fetched_super.unwrap();
        let fa = fetched_ascii.unwrap();

        assert_eq!(fs.file_size(), 1024);
        assert_eq!(fa.file_size(), 2048);
        assert_ne!(fs.relative_path(), fa.relative_path());

        // Deleting the superscript file leaves the ASCII file intact
        store.delete_file(Path::new("notes/doc¹.txt")).unwrap();
        assert!(
            store
                .get_file(Path::new("notes/doc¹.txt"))
                .unwrap()
                .is_none()
        );
        assert!(
            store
                .get_file(Path::new("notes/doc1.txt"))
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn test_delete_file_atomic_single_transaction() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let store = SqliteHashStore::new(&db_path, StoreConfig::new(1024, 4096).unwrap()).unwrap();

        let rec = FileRecord::from_raw("atomic_fail.txt", 512, 1000).unwrap();
        let hash = [0x5Au8; 32];
        store.save_file(&rec, &[hash]).unwrap();

        // Verify initial state
        assert!(
            store
                .get_file(Path::new("atomic_fail.txt"))
                .unwrap()
                .is_some()
        );
        assert_eq!(
            store
                .get_block_hashes(Path::new("atomic_fail.txt"))
                .unwrap()
                .len(),
            1
        );

        // Install a trigger that aborts deletion of "atomic_fail.txt"
        {
            let conn = rusqlite::Connection::open(&db_path).unwrap();
            conn.execute_batch(
                "CREATE TRIGGER abort_delete_fail
                 BEFORE DELETE ON file_metadata
                 FOR EACH ROW
                 WHEN OLD.relative_path = 'atomic_fail.txt'
                 BEGIN
                     SELECT RAISE(ABORT, 'forced delete failure');
                 END;",
            )
            .unwrap();
        }

        // Attempt delete_file; must fail due to trigger
        let res = store.delete_file(Path::new("atomic_fail.txt"));
        assert!(res.is_err(), "delete_file must fail due to trigger");

        // Transaction must have rolled back atomically: record and hashes still present
        let rechecked = store.get_file(Path::new("atomic_fail.txt")).unwrap();
        assert!(
            rechecked.is_some(),
            "Record must still exist after rollback"
        );
        let rechecked_hashes = store
            .get_block_hashes(Path::new("atomic_fail.txt"))
            .unwrap();
        assert_eq!(
            rechecked_hashes.len(),
            1,
            "Block hashes must still exist after rollback"
        );
    }

    #[test]
    fn test_delete_files_batch_mixed_exact_and_hierarchies() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let store = SqliteHashStore::new(&db_path, StoreConfig::new(1024, 4096).unwrap()).unwrap();

        let r1 = FileRecord::from_raw("dir_a/file1.txt", 100, 100).unwrap();
        let r2 = FileRecord::from_raw("dir_a/sub/file2.txt", 200, 200).unwrap();
        let r3 = FileRecord::from_raw("dir_b/file3.txt", 300, 300).unwrap();
        let r4 = FileRecord::from_raw("exact.txt", 400, 400).unwrap();
        let r5 = FileRecord::from_raw("keep.txt", 500, 500).unwrap();

        store.save_file(&r1, &[]).unwrap();
        store.save_file(&r2, &[]).unwrap();
        store.save_file(&r3, &[]).unwrap();
        store.save_file(&r4, &[]).unwrap();
        store.save_file(&r5, &[]).unwrap();

        // Delete "dir_a" (hierarchy), "exact.txt" (exact file)
        let targets = [Path::new("dir_a"), Path::new("exact.txt")];
        store.delete_files_batch(&targets).unwrap();

        let remaining = store.list_files().unwrap();
        let mut rem_str: Vec<String> = remaining
            .iter()
            .map(|r| r.to_string_lossy().replace('\\', "/"))
            .collect();
        rem_str.sort();

        assert_eq!(rem_str, vec!["dir_b/file3.txt", "keep.txt"]);
    }

    #[test]
    fn test_sqlite_hash_store_list_all_records_flat_vec() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let store = SqliteHashStore::new(&db_path, StoreConfig::new(1024, 4096).unwrap()).unwrap();

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

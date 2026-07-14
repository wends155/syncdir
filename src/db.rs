//! SQLite-backed storage for file metadata and block hash signatures.
//!
//! Provides the `HashStore` trait and its `SqliteHashStore` implementation.
//! Enforces foreign key cascades and validates configuration consistency.

use crate::config::Config;
use crate::error::SyncError;
use rusqlite::{Connection, params};
use std::path::Path;
use std::sync::Mutex;

/// Metadata record for a tracked file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileRecord {
    pub id: Option<i64>,
    pub relative_path: String,
    /// File size in bytes. Uses `i64` to match SQLite INTEGER column type.
    pub file_size: i64,
    pub last_modified: i64,
}

/// Interface for persisting and querying file block signatures.
pub trait HashStore {
    fn get_file(&self, path: &str) -> Result<Option<FileRecord>, SyncError>;
    fn save_file(&self, record: &FileRecord, hashes: &[Vec<u8>]) -> Result<(), SyncError>;
    fn get_block_hashes(&self, file_id: i64) -> Result<Vec<Vec<u8>>, SyncError>;
    fn delete_file(&self, path: &str) -> Result<(), SyncError>;
    fn list_files(&self) -> Result<Vec<String>, SyncError>;
}

/// SQLite implementation of `HashStore`.
pub struct SqliteHashStore {
    conn: Mutex<Connection>,
}

impl SqliteHashStore {
    /// Open (or create) the SQLite database and initialize the schema.
    ///
    /// Enforces foreign keys, creates tables if missing, and validates
    /// that cached configuration parameters match the active config.
    /// If they differ, all cached file data is purged.
    ///
    /// # Errors
    /// Returns `SyncError::Db` on any SQLite failure.
    pub fn new(db_path: &Path, config: &Config) -> Result<Self, SyncError> {
        let conn = Connection::open(db_path)?;
        conn.execute_batch("PRAGMA foreign_keys = ON;")?;
        let store = SqliteHashStore {
            conn: Mutex::new(conn),
        };
        store.init_schema()?;
        store.enforce_metadata(config)?;
        Ok(store)
    }

    fn conn(&self) -> Result<std::sync::MutexGuard<'_, Connection>, SyncError> {
        self.conn
            .lock()
            .map_err(|e| SyncError::LockPoison(format!("DB lock poisoned: {e}")))
    }

    fn init_schema(&self) -> Result<(), SyncError> {
        let conn = self.conn()?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS file_metadata (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                relative_path TEXT NOT NULL UNIQUE,
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
            CREATE TABLE IF NOT EXISTS db_metadata (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );",
        )?;
        Ok(())
    }

    fn enforce_metadata(&self, config: &Config) -> Result<(), SyncError> {
        let cached_block_size = self.get_meta_value("block_size_bytes")?;
        let cached_threshold = self.get_meta_value("block_sync_threshold_bytes")?;
        let cached_version = self.get_meta_value("db_version")?;

        let current_block_size = config.block_size_bytes.to_string();
        let current_threshold = config.block_sync_threshold_bytes.to_string();
        let current_version = "2";

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
            conn.execute("DELETE FROM file_metadata", [])?;
            // block_hashes cleaned by CASCADE
        }

        self.set_meta_value("block_size_bytes", &current_block_size)?;
        self.set_meta_value("block_sync_threshold_bytes", &current_threshold)?;
        self.set_meta_value("db_version", current_version)?;
        Ok(())
    }

    fn get_meta_value(&self, key: &str) -> Result<Option<String>, SyncError> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare("SELECT value FROM db_metadata WHERE key = ?")?;
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
        conn.execute(
            "INSERT OR REPLACE INTO db_metadata (key, value) VALUES (?, ?)",
            params![key, value],
        )?;
        Ok(())
    }
}

impl HashStore for SqliteHashStore {
    fn get_file(&self, path: &str) -> Result<Option<FileRecord>, SyncError> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, relative_path, file_size, last_modified \
             FROM file_metadata WHERE relative_path = ?",
        )?;
        let mut rows = stmt.query(params![path])?;
        if let Some(row) = rows.next()? {
            Ok(Some(FileRecord {
                id: Some(row.get(0)?),
                relative_path: row.get(1)?,
                file_size: row.get(2)?,
                last_modified: row.get(3)?,
            }))
        } else {
            Ok(None)
        }
    }

    fn save_file(&self, record: &FileRecord, hashes: &[Vec<u8>]) -> Result<(), SyncError> {
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;

        // UPSERT preserves the rowid on conflict, keeping FK references stable.
        tx.execute(
            "INSERT INTO file_metadata (relative_path, file_size, last_modified) \
             VALUES (?1, ?2, ?3) \
             ON CONFLICT(relative_path) DO UPDATE SET \
               file_size = excluded.file_size, \
               last_modified = excluded.last_modified",
            params![record.relative_path, record.file_size, record.last_modified],
        )?;

        // Retrieve the stable rowid (works for both insert and update)
        let file_id: i64 = tx.query_row(
            "SELECT id FROM file_metadata WHERE relative_path = ?",
            params![record.relative_path],
            |row| row.get(0),
        )?;

        // Replace all block hashes for this file
        tx.execute(
            "DELETE FROM block_hashes WHERE file_id = ?",
            params![file_id],
        )?;
        for (idx, hash) in hashes.iter().enumerate() {
            tx.execute(
                "INSERT INTO block_hashes (file_id, block_index, hash) \
                 VALUES (?, ?, ?)",
                params![file_id, idx as i64, hash],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    fn get_block_hashes(&self, file_id: i64) -> Result<Vec<Vec<u8>>, SyncError> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT hash FROM block_hashes WHERE file_id = ? \
             ORDER BY block_index ASC",
        )?;
        let mut rows = stmt.query(params![file_id])?;
        let mut hashes = Vec::new();
        while let Some(row) = rows.next()? {
            hashes.push(row.get(0)?);
        }
        Ok(hashes)
    }

    fn delete_file(&self, path: &str) -> Result<(), SyncError> {
        let conn = self.conn()?;
        conn.execute(
            "DELETE FROM file_metadata WHERE relative_path = ?",
            params![path],
        )?;
        Ok(())
    }

    fn list_files(&self) -> Result<Vec<String>, SyncError> {
        let conn = self.conn()?;
        let mut stmt =
            conn.prepare("SELECT relative_path FROM file_metadata ORDER BY relative_path ASC")?;
        let mut rows = stmt.query([])?;
        let mut paths = Vec::new();
        while let Some(row) = rows.next()? {
            paths.push(row.get(0)?);
        }
        Ok(paths)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use std::path::PathBuf;
    use tempfile::NamedTempFile;

    fn dummy_config(block_size: u64) -> Config {
        Config {
            source_dir: PathBuf::from("."),
            dest_dir: PathBuf::from("."),
            debounce_seconds: 3,
            propagate_deletions: true,
            block_sync_threshold_bytes: block_size * 2,
            block_size_bytes: block_size,
            verify_writes: true,
            retry_interval_seconds: 10,
        }
    }

    #[test]
    fn test_save_get_delete_with_cascade() {
        let temp = NamedTempFile::new().unwrap();
        let config = dummy_config(1024);
        let store = SqliteHashStore::new(temp.path(), &config).unwrap();

        let record = FileRecord {
            id: None,
            relative_path: "docs/spec.txt".to_string(),
            file_size: 2048,
            last_modified: 1234567890,
        };
        let hashes = vec![vec![1; 32], vec![2; 32]];

        store.save_file(&record, &hashes).unwrap();

        let fetched = store.get_file("docs/spec.txt").unwrap().unwrap();
        let file_id = fetched.id.unwrap();
        assert_eq!(fetched.file_size, 2048);
        assert_eq!(fetched.last_modified, 1234567890);

        let fetched_hashes = store.get_block_hashes(file_id).unwrap();
        assert_eq!(fetched_hashes.len(), 2);
        assert_eq!(fetched_hashes[0], vec![1; 32]);
        assert_eq!(fetched_hashes[1], vec![2; 32]);

        // Verify foreign key cascade delete
        store.delete_file("docs/spec.txt").unwrap();
        assert!(store.get_file("docs/spec.txt").unwrap().is_none());

        let count: i64 = store
            .conn
            .lock()
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
        let config = dummy_config(1024);
        let store = SqliteHashStore::new(temp.path(), &config).unwrap();

        let record = FileRecord {
            id: None,
            relative_path: "test.bin".to_string(),
            file_size: 100,
            last_modified: 1000,
        };
        store.save_file(&record, &[vec![1; 32]]).unwrap();
        let id1 = store.get_file("test.bin").unwrap().unwrap().id.unwrap();

        // Update same file — rowid should be preserved
        let updated = FileRecord {
            id: None,
            relative_path: "test.bin".to_string(),
            file_size: 200,
            last_modified: 2000,
        };
        store
            .save_file(&updated, &[vec![2; 32], vec![3; 32]])
            .unwrap();
        let fetched = store.get_file("test.bin").unwrap().unwrap();
        assert_eq!(fetched.id.unwrap(), id1); // Same rowid
        assert_eq!(fetched.file_size, 200);

        let hashes = store.get_block_hashes(id1).unwrap();
        assert_eq!(hashes.len(), 2);
    }

    #[test]
    fn test_db_config_invalidation() {
        let temp = NamedTempFile::new().unwrap();

        // Open with config A and save a file
        let config_a = dummy_config(1024);
        {
            let store = SqliteHashStore::new(temp.path(), &config_a).unwrap();
            let record = FileRecord {
                id: None,
                relative_path: "test.bin".to_string(),
                file_size: 100,
                last_modified: 9999,
            };
            store.save_file(&record, &[vec![7; 32]]).unwrap();
            assert!(store.get_file("test.bin").unwrap().is_some());
        }

        // Open with config B (different block size) — cache should be purged
        let config_b = dummy_config(512);
        {
            let store = SqliteHashStore::new(temp.path(), &config_b).unwrap();
            assert!(store.get_file("test.bin").unwrap().is_none());
        }
    }

    #[test]
    fn test_list_files() {
        let temp = NamedTempFile::new().unwrap();
        let config = dummy_config(1024);
        let store = SqliteHashStore::new(temp.path(), &config).unwrap();

        // Empty database
        assert!(store.list_files().unwrap().is_empty());

        // Insert two files
        let r1 = FileRecord {
            id: None,
            relative_path: "b_second.txt".to_string(),
            file_size: 100,
            last_modified: 1000,
        };
        let r2 = FileRecord {
            id: None,
            relative_path: "a_first.txt".to_string(),
            file_size: 200,
            last_modified: 2000,
        };
        store.save_file(&r1, &[vec![1; 32]]).unwrap();
        store.save_file(&r2, &[vec![2; 32]]).unwrap();

        let files = store.list_files().unwrap();
        assert_eq!(files, vec!["a_first.txt", "b_second.txt"]);

        // After delete, removed file is gone
        store.delete_file("a_first.txt").unwrap();
        let files = store.list_files().unwrap();
        assert_eq!(files, vec!["b_second.txt"]);
    }
}

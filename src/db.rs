//! SQLite-backed storage for file metadata and block hash signatures.
//!
//! Provides the `HashStore` trait and its `SqliteHashStore` implementation.
//! Enforces foreign key cascades and validates configuration consistency.

use crate::error::SyncError;
use crate::path_util::RelativePath;
use rusqlite::{Connection, params};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Metadata record for a tracked file.
#[derive(Clone, PartialEq, Eq)]
pub struct FileRecord {
    id: Option<i64>,
    relative_path: RelativePath,
    /// File size in bytes.
    file_size: u64,
    last_modified: i64,
}

impl std::fmt::Debug for FileRecord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileRecord")
            .field("id", &self.id)
            .field("relative_path", &self.relative_path.as_path())
            .field("file_size", &self.file_size)
            .field("last_modified", &self.last_modified)
            .finish()
    }
}

impl FileRecord {
    /// Create a new file record without a database surrogate ID.
    pub fn new(relative_path: RelativePath, file_size: u64, last_modified: i64) -> Self {
        Self {
            id: None,
            relative_path,
            file_size,
            last_modified,
        }
    }

    /// Construct a `FileRecord` validating raw path into a `RelativePath`.
    ///
    /// # Errors
    /// Returns `SyncError::Validation` if `path` is not a valid relative path.
    pub fn from_raw(
        path: impl AsRef<Path>,
        file_size: u64,
        last_modified: i64,
    ) -> Result<Self, SyncError> {
        let rel = RelativePath::new(path)?;
        Ok(Self::new(rel, file_size, last_modified))
    }

    /// Attach a surrogate database ID to the record.
    #[must_use]
    pub fn with_id(mut self, id: i64) -> Self {
        self.id = Some(id);
        self
    }

    /// Attach an optional surrogate database ID to the record.
    #[must_use]
    pub fn with_optional_id(mut self, id: Option<i64>) -> Self {
        self.id = id;
        self
    }

    /// Returns the database surrogate ID if persisted.
    #[must_use]
    pub fn id(&self) -> Option<i64> {
        self.id
    }

    /// Returns `true` if this file record has been persisted to the database.
    #[must_use]
    pub fn is_tracked(&self) -> bool {
        self.id.is_some()
    }

    /// Returns the relative path of the file.
    #[must_use]
    pub fn relative_path(&self) -> &RelativePath {
        &self.relative_path
    }

    /// Returns the size of the file in bytes.
    #[must_use]
    pub fn file_size(&self) -> u64 {
        self.file_size
    }

    /// Returns the last modified timestamp in milliseconds since UNIX epoch.
    #[must_use]
    pub fn last_modified(&self) -> i64 {
        self.last_modified
    }
}

/// Convert relative path to canonical forward-slash SQLite storage key.
pub(crate) fn path_to_sqlite_key(path: &Path) -> Result<String, SyncError> {
    let s = path.to_string_lossy();
    if s.is_empty() {
        return Err(SyncError::validation("Path cannot be empty"));
    }
    let normalized = crate::path_util::normalize_superscripts_cow(&s);
    // Normalize any backslashes to forward slashes for cross-platform SQLite storage
    let mut key = normalized.replace('\\', "/");
    let trim_count = key.chars().take_while(|&c| c == '/').count();
    if trim_count == key.len() {
        return Err(SyncError::validation(
            "Path cannot resolve to empty SQLite key",
        ));
    }
    if trim_count > 0 {
        key.drain(..trim_count);
    }
    Ok(key)
}

/// Minimal configuration parameters required by `SqliteHashStore`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StoreConfig {
    block_size_bytes: u64,
    block_sync_threshold_bytes: u64,
}

impl StoreConfig {
    /// Create a new store configuration with specified block size and threshold.
    ///
    /// # Errors
    /// Returns `SyncError::Validation` if `block_size_bytes` is 0.
    pub fn new(block_size_bytes: u64, block_sync_threshold_bytes: u64) -> Result<Self, SyncError> {
        if block_size_bytes == 0 {
            return Err(SyncError::validation(
                "block_size_bytes must be greater than zero",
            ));
        }
        Ok(Self {
            block_size_bytes,
            block_sync_threshold_bytes,
        })
    }

    /// Return configured block size in bytes.
    #[must_use]
    pub fn block_size_bytes(&self) -> u64 {
        self.block_size_bytes
    }

    /// Return configured block sync threshold in bytes.
    #[must_use]
    pub fn block_sync_threshold_bytes(&self) -> u64 {
        self.block_sync_threshold_bytes
    }
}

/// A Blake3 block hash: fixed 32-byte digest.
pub type BlockHash = [u8; 32];

/// Interface for persisting and querying file block signatures.
pub trait HashStore: Send + Sync {
    /// Retrieve stored metadata and signatures for a file by relative path.
    fn get_file(&self, path: &Path) -> Result<Option<FileRecord>, SyncError>;

    /// Persist file metadata and its associated block hashes.
    fn save_file(&self, record: &FileRecord, hashes: &[BlockHash]) -> Result<(), SyncError>;

    /// Retrieve all stored block hashes for a given relative path.
    fn get_block_hashes(&self, path: &Path) -> Result<Vec<BlockHash>, SyncError>;

    /// Delete a file record and cascade removal of all its block hashes.
    fn delete_file(&self, path: &Path) -> Result<(), SyncError>;

    /// List relative paths of all currently tracked files in the database.
    fn list_files(&self) -> Result<Vec<RelativePath>, SyncError>;

    /// Bulk retrieve all stored file metadata records mapped by relative path.
    fn list_all_records(&self) -> Result<HashMap<PathBuf, FileRecord>, SyncError>;

    /// Persist multiple file records and their block hashes in a single transaction.
    ///
    /// This dramatically reduces SQLite commit overhead during full scans.
    ///
    /// # Errors
    ///
    /// Returns `SyncError::Db` if any database operation fails.
    fn save_files_batch(&self, records: &[(&FileRecord, &[BlockHash])]) -> Result<(), SyncError>;

    /// Delete multiple file records and cascade removal of all their block hashes in a single transaction.
    ///
    /// Default implementation calls `delete_file` sequentially.
    ///
    /// # Errors
    ///
    /// Returns `SyncError::Db` if any database operation fails.
    fn delete_files_batch(&self, paths: &[&Path]) -> Result<(), SyncError> {
        for path in paths {
            self.delete_file(path)?;
        }
        Ok(())
    }
}

impl<S: HashStore + ?Sized> HashStore for std::sync::Arc<S> {
    fn get_file(&self, path: &Path) -> Result<Option<FileRecord>, SyncError> {
        (**self).get_file(path)
    }

    fn save_file(&self, record: &FileRecord, hashes: &[BlockHash]) -> Result<(), SyncError> {
        (**self).save_file(record, hashes)
    }

    fn get_block_hashes(&self, path: &Path) -> Result<Vec<BlockHash>, SyncError> {
        (**self).get_block_hashes(path)
    }

    fn delete_file(&self, path: &Path) -> Result<(), SyncError> {
        (**self).delete_file(path)
    }

    fn list_files(&self) -> Result<Vec<RelativePath>, SyncError> {
        (**self).list_files()
    }

    fn list_all_records(&self) -> Result<HashMap<PathBuf, FileRecord>, SyncError> {
        (**self).list_all_records()
    }

    fn save_files_batch(&self, records: &[(&FileRecord, &[BlockHash])]) -> Result<(), SyncError> {
        (**self).save_files_batch(records)
    }

    fn delete_files_batch(&self, paths: &[&Path]) -> Result<(), SyncError> {
        (**self).delete_files_batch(paths)
    }
}

impl<S: HashStore + ?Sized> HashStore for &S {
    fn get_file(&self, path: &Path) -> Result<Option<FileRecord>, SyncError> {
        (**self).get_file(path)
    }

    fn save_file(&self, record: &FileRecord, hashes: &[BlockHash]) -> Result<(), SyncError> {
        (**self).save_file(record, hashes)
    }

    fn get_block_hashes(&self, path: &Path) -> Result<Vec<BlockHash>, SyncError> {
        (**self).get_block_hashes(path)
    }

    fn delete_file(&self, path: &Path) -> Result<(), SyncError> {
        (**self).delete_file(path)
    }

    fn list_files(&self) -> Result<Vec<RelativePath>, SyncError> {
        (**self).list_files()
    }

    fn list_all_records(&self) -> Result<HashMap<PathBuf, FileRecord>, SyncError> {
        (**self).list_all_records()
    }

    fn save_files_batch(&self, records: &[(&FileRecord, &[BlockHash])]) -> Result<(), SyncError> {
        (**self).save_files_batch(records)
    }

    fn delete_files_batch(&self, paths: &[&Path]) -> Result<(), SyncError> {
        (**self).delete_files_batch(paths)
    }
}

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
            let path_str: String = row.get(1)?;
            let size_i64: i64 = row.get(2)?;
            Ok(Some(FileRecord {
                id: Some(row.get(0)?),
                relative_path: RelativePath::from_sanitized_unchecked(PathBuf::from(path_str)),
                file_size: size_i64.max(0) as u64,
                last_modified: row.get(3)?,
            }))
        } else {
            Ok(None)
        }
    }

    #[tracing::instrument(
        skip(self, hashes),
        fields(path = %record.relative_path.as_path().display()),
        level = "debug"
    )]
    fn save_file(&self, record: &FileRecord, hashes: &[BlockHash]) -> Result<(), SyncError> {
        let key = record.relative_path.to_sqlite_key();
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
            params![key, record.file_size as i64, record.last_modified],
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
                let key = record.relative_path.to_sqlite_key();
                let file_id: i64 = meta_stmt.query_row(
                    params![key, record.file_size as i64, record.last_modified],
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
        let key = path_to_sqlite_key(path)?;
        let conn = self.conn()?;
        // 1. Exact match (uses UNIQUE index directly)
        conn.prepare_cached("DELETE FROM file_metadata WHERE relative_path = ?1")?
            .execute(params![key])?;
        // 2. Sargable prefix range for directory children (uses index range scan)
        let prefix_start = format!("{}/", key);
        let prefix_end = format!("{}0", key); // '0' is next ASCII char after '/'
        conn.prepare_cached(
            "DELETE FROM file_metadata WHERE relative_path >= ?1 AND relative_path < ?2",
        )?
        .execute(params![prefix_start, prefix_end])?;
        Ok(())
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

    fn list_all_records(&self) -> Result<HashMap<PathBuf, FileRecord>, SyncError> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare_cached(
            "SELECT id, relative_path, file_size, last_modified FROM file_metadata",
        )?;
        let mut rows = stmt.query([])?;
        let mut map = HashMap::new();
        while let Some(row) = rows.next()? {
            let id: i64 = row.get(0)?;
            let rel_str: String = row.get(1)?;
            let file_size: i64 = row.get(2)?;
            let last_modified: i64 = row.get(3)?;
            let rel_path = PathBuf::from(rel_str);
            let rel = RelativePath::from_sanitized_unchecked(rel_path.clone());
            map.insert(
                rel_path,
                FileRecord {
                    id: Some(id),
                    relative_path: rel,
                    file_size: file_size.max(0) as u64,
                    last_modified,
                },
            );
        }
        Ok(map)
    }
}

/// Type alias for error hook injected into `MockHashStore`.
pub type MockStoreErrorHook = Box<dyn Fn(&str) -> Option<SyncError> + Send + Sync>;

#[derive(Default)]
struct MockStoreInner {
    records: std::collections::HashMap<String, FileRecord>,
    hashes: std::collections::HashMap<i64, Vec<BlockHash>>,
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
    inner: std::sync::Arc<std::sync::RwLock<MockStoreInner>>,
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
        let key = record.relative_path.to_sqlite_key().to_lowercase();
        let mut inner = self
            .inner
            .write()
            .map_err(|_| SyncError::lock_poison("Mock hash store lock poisoned"))?;
        if let Some(err) = inner.error_hook.as_ref().and_then(|h| h("save_file")) {
            return Err(err);
        }
        inner.save_file_calls += 1;

        let id = if let Some(existing) = inner.records.get(&key) {
            existing.id.unwrap_or(1)
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
            && let Some(id) = record.id
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
                && let Some(id) = removed.id
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

    fn list_all_records(&self) -> Result<HashMap<PathBuf, FileRecord>, SyncError> {
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
        let map = inner
            .records
            .values()
            .map(|r| (r.relative_path.to_path_buf(), r.clone()))
            .collect();
        Ok(map)
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
            let key = record.relative_path.to_sqlite_key().to_lowercase();
            let id = if let Some(existing) = inner.records.get(&key) {
                existing.id.unwrap_or(1)
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
                    && let Some(id) = removed.id
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
    use pretty_assertions::assert_eq;
    use std::path::PathBuf;
    use tempfile::NamedTempFile;

    fn dummy_store_config(block_size: u64) -> StoreConfig {
        StoreConfig::new(block_size, block_size * 2).expect("test block_size must be > 0")
    }

    #[test]
    fn test_file_record_encapsulation_and_getters() {
        use crate::db::FileRecord;
        let rec = FileRecord::from_raw("sub/doc.txt", 4096u64, 1690000000i64)
            .unwrap()
            .with_id(99);
        assert_eq!(rec.relative_path(), Path::new("sub/doc.txt"));
        assert_eq!(rec.file_size(), 4096u64);
        assert_eq!(rec.last_modified(), 1690000000i64);
        assert_eq!(rec.id(), Some(99));

        let rec2 = FileRecord::from_raw("test.bin", 0u64, 100i64)
            .unwrap()
            .with_optional_id(None);
        assert_eq!(rec2.id(), None);
    }

    #[test]
    fn test_file_record_relative_path_encapsulation() {
        use crate::path_util::RelativePath;
        let rel = RelativePath::new("nested/doc.txt").unwrap();
        let rec = FileRecord::new(rel.clone(), 1024, 1700000000);
        assert_eq!(rec.relative_path(), &rel);
        assert_eq!(rec.relative_path().as_path(), Path::new("nested/doc.txt"));

        // Test from_raw constructor
        let rec_raw = FileRecord::from_raw("raw/path.bin", 2048, 1700000001).unwrap();
        assert_eq!(rec_raw.relative_path().as_path(), Path::new("raw/path.bin"));

        // Test from_raw rejects invalid relative path
        let err_raw = FileRecord::from_raw("../escape.txt", 100, 100);
        assert!(err_raw.is_err());
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
        assert_eq!(fetched.relative_path(), Path::new("docs/spec.txt"));
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
    fn test_path_to_sqlite_key() {
        assert_eq!(
            path_to_sqlite_key(Path::new(r"foo\bar\baz.txt")).unwrap(),
            "foo/bar/baz.txt"
        );
        assert_eq!(
            path_to_sqlite_key(Path::new("foo/bar/baz.txt")).unwrap(),
            "foo/bar/baz.txt"
        );
        assert!(path_to_sqlite_key(Path::new("")).is_err());
    }

    #[test]
    fn test_path_to_sqlite_key_root_slashes_return_err() {
        assert!(path_to_sqlite_key(Path::new("/")).is_err());
        assert!(path_to_sqlite_key(Path::new(r"\")).is_err());
        assert!(path_to_sqlite_key(Path::new("///")).is_err());
        assert_eq!(
            path_to_sqlite_key(Path::new("/valid/path.txt")).unwrap(),
            "valid/path.txt"
        );
    }

    #[test]
    fn test_store_config_conversions() {
        let sc = StoreConfig::new(4096, 8192).unwrap();
        assert_eq!(sc.block_size_bytes(), 4096);
        assert_eq!(sc.block_sync_threshold_bytes(), 8192);

        let sc_zero = StoreConfig::new(0, 8192);
        assert!(
            sc_zero.is_err(),
            "StoreConfig::new must return Err on block_size_bytes == 0"
        );
    }

    #[test]
    fn test_get_block_hashes_by_path_known_and_unknown() {
        let temp = NamedTempFile::new().unwrap();
        let store = SqliteHashStore::new(temp.path(), dummy_store_config(1024)).unwrap();
        let rec = FileRecord::from_raw("data/sample.bin", 2048, 5000).unwrap();
        let hashes = vec![[0xAAu8; 32], [0xBBu8; 32]];
        store.save_file(&rec, &hashes).unwrap();
        let fetched = store
            .get_block_hashes(Path::new("data/sample.bin"))
            .unwrap();
        assert_eq!(fetched, hashes);
        let unknown = store.get_block_hashes(Path::new("missing.bin")).unwrap();
        assert!(
            unknown.is_empty(),
            "Unknown path must return empty vec, not error"
        );
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
    fn test_store_config_validation_rejects_zero() {
        let res_zero = StoreConfig::new(0, 10_485_760);
        assert!(
            res_zero.is_err(),
            "StoreConfig::new must reject block_size_bytes == 0"
        );
        let valid = StoreConfig::new(1_048_576, 10_485_760);
        assert!(
            valid.is_ok(),
            "StoreConfig::new must accept positive block_size_bytes"
        );
        let cfg = valid.unwrap();
        assert_eq!(cfg.block_size_bytes(), 1_048_576);
        assert_eq!(cfg.block_sync_threshold_bytes(), 10_485_760);
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
        assert_eq!(found.unwrap().id().is_some(), true);

        // Delete with different case should remove it
        store.delete_file(Path::new("MYFILE.TXT")).unwrap();
        assert!(store.get_file(Path::new("MyFile.TXT")).unwrap().is_none());
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
        use crate::db::{BlockHash, FileRecord, HashStore, SqliteHashStore, StoreConfig};
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
        use crate::db::{BlockHash, FileRecord, HashStore, SqliteHashStore, StoreConfig};
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
}

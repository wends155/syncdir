//! Pure domain models, storage configuration, key normalization, and the `HashStore` trait.
//!
//! This module defines the storage contracts and domain models for `syncdir`.
//! It contains zero SQLite, C FFI, or platform dependencies.

use crate::error::SyncError;
use crate::path_util::RelativePath;
use std::path::Path;
use std::sync::Arc;

/// A Blake3 block hash: fixed 32-byte digest.
pub type BlockHash = [u8; 32];

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

/// Convert relative path to canonical forward-slash SQLite storage key.
///
/// Preserves exact Unicode UTF-8 character bytes (including superscripts) while
/// normalizing platform path separators (`\` -> `/`) and trimming leading root slashes.
pub(crate) fn path_to_sqlite_key(path: &Path) -> Result<String, SyncError> {
    let s = path.to_string_lossy();
    if s.is_empty() {
        return Err(SyncError::validation("Path cannot be empty"));
    }
    // Normalize any backslashes to forward slashes for cross-platform SQLite storage
    let mut key = s.replace('\\', "/");
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

    /// Bulk retrieve all stored file metadata records as a flat vector.
    fn list_all_records(&self) -> Result<Vec<FileRecord>, SyncError>;

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

impl<S: HashStore + ?Sized> HashStore for Arc<S> {
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

    fn list_all_records(&self) -> Result<Vec<FileRecord>, SyncError> {
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

    fn list_all_records(&self) -> Result<Vec<FileRecord>, SyncError> {
        (**self).list_all_records()
    }

    fn save_files_batch(&self, records: &[(&FileRecord, &[BlockHash])]) -> Result<(), SyncError> {
        (**self).save_files_batch(records)
    }

    fn delete_files_batch(&self, paths: &[&Path]) -> Result<(), SyncError> {
        (**self).delete_files_batch(paths)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_file_record_encapsulation_and_getters() {
        let rec = FileRecord::from_raw("sub/doc.txt", 4096u64, 1690000000i64)
            .unwrap()
            .with_id(99);
        assert_eq!(rec.relative_path().as_path(), Path::new("sub/doc.txt"));
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
}

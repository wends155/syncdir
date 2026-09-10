//! Core sync engine trait, commands, status types, and LocalSyncEngine coordination.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use crate::config::TargetSyncConfig;
use crate::db::{FileRecord, HashStore};
use crate::error::SyncError;

use super::delta::DirtyBlockRange;
use super::path_safety::{
    is_reparse_or_symlink_meta, is_safe_relative_path, verify_destination_not_reparse_cached,
    verify_source_not_reparse,
};
use super::scanner::scan_dir;

/// Extract file modified time as milliseconds since UNIX epoch.
///
/// Pre-1970 timestamps are clamped to 0 (epoch) with a warning log.
///
/// # Errors
///
/// Returns `SyncError::Io` if the file's modified time cannot be read.
pub(crate) fn safe_modified_millis(metadata: &std::fs::Metadata) -> Result<i64, SyncError> {
    let modified = metadata.modified().map_err(SyncError::Io)?;
    match modified.duration_since(UNIX_EPOCH) {
        Ok(dur) => Ok(dur.as_millis() as i64),
        Err(_) => {
            tracing::warn!("File has pre-1970 modified timestamp, clamping to epoch");
            Ok(0)
        }
    }
}

/// Convert a millisecond timestamp to a `Duration`, clamping negative values to zero.
pub(crate) fn safe_epoch_duration_millis(millis: i64) -> std::time::Duration {
    std::time::Duration::from_millis(millis.max(0) as u64)
}

/// Commands sent from the file watcher or tray UI to the sync worker thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncCommand {
    /// A file was created or modified at the given relative path.
    FileModified(PathBuf),
    /// A file was deleted at the given relative path.
    FileDeleted(PathBuf),
    /// Request a full directory scan and sync.
    TriggerFullScan,
}

/// Outcome of a full directory scan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScanOutcome {
    /// All files synced successfully.
    Success { synced: usize },
    /// Some files failed to sync or delete.
    PartialFailure {
        synced: usize,
        failed: usize,
        delete_failed: usize,
    },
    /// Destination is unreachable.
    DestinationUnreachable,
}

/// Network and target connection status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectivityState {
    /// Connection is online and reachable.
    Online,
    /// Connection is offline or unreachable.
    Offline,
}

impl From<bool> for ConnectivityState {
    fn from(b: bool) -> Self {
        if b { Self::Online } else { Self::Offline }
    }
}

impl From<ConnectivityState> for bool {
    fn from(c: ConnectivityState) -> Self {
        matches!(c, ConnectivityState::Online)
    }
}

/// Directory watcher status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatcherState {
    /// Directory watcher is active.
    Active,
    /// Directory watcher is inactive.
    Inactive,
}

impl From<bool> for WatcherState {
    fn from(b: bool) -> Self {
        if b { Self::Active } else { Self::Inactive }
    }
}

impl From<WatcherState> for bool {
    fn from(w: WatcherState) -> Self {
        matches!(w, WatcherState::Active)
    }
}

/// Observer for sync worker status changes. Decouples sync from UI.
///
/// Implementors can register to receive asynchronous notifications about target connectivity,
/// source watcher status, and permanent write verification failures.
///
/// # Examples
///
/// ```
/// use syncdir::sync::engine::{SyncStatusObserver, ConnectivityState, WatcherState};
/// use std::path::Path;
///
/// struct MyObserver;
/// impl SyncStatusObserver for MyObserver {
///     fn on_target_status_change(&self, target_index: usize, state: ConnectivityState) {
///         println!("Target {} is {:?}", target_index, state);
///     }
/// }
/// ```
pub trait SyncStatusObserver: Send + Sync + 'static {
    /// Notification when target connectivity changes.
    fn on_target_status_change(&self, target_index: usize, state: ConnectivityState);
    /// Forward source directory connectivity and watcher active status to observers.
    fn on_watcher_status_change(&self, _source: ConnectivityState, _watcher: WatcherState) {}
    /// Notification when a file's write verification permanently fails after retries.
    fn on_write_verification_failed(&self, _path: &Path) {}
}

/// Core sync execution contract. Implemented by the delta sync engine.
///
/// # Examples
///
/// ```rust,no_run
/// use syncdir::sync::engine::SyncEngine;
/// use syncdir::sync::mock::MockSyncEngine;
/// use std::path::Path;
///
/// let engine = MockSyncEngine::new();
/// let _ = engine.sync_file(Path::new("document.txt"));
/// ```
pub trait SyncEngine: Send + Sync {
    /// Synchronize a single file from source to destination.
    ///
    /// # Errors
    /// Returns `SyncError::Io` on filesystem errors, `SyncError::Db` on database persistence failures,
    /// or `SyncError::WriteVerificationFailed` if written content does not match source Blake3 hashes.
    fn sync_file(&self, path: &Path) -> Result<(), SyncError>;

    /// Synchronize a file to a specific destination directory with a reusable scratch buffer.
    ///
    /// Required method: implementors must handle the `dest_dir` parameter.
    ///
    /// # Errors
    /// Returns `SyncError` if reading source, streaming delta, verifying writes, or saving metadata fails.
    fn sync_file_to_dest_buffered(
        &self,
        path: &Path,
        dest_dir: &Path,
        scratch: &mut [u8],
    ) -> Result<(), SyncError>;

    /// Handle deletion of a file (archive on destination).
    ///
    /// # Errors
    /// Returns `SyncError` if moving the deleted file to archive fails.
    fn delete_file(&self, path: &Path) -> Result<(), SyncError>;

    /// Handle deletion of a file on a specific destination directory.
    ///
    /// Required method: implementors must handle the `dest_dir` parameter.
    ///
    /// # Errors
    /// Returns `SyncError` if moving the deleted file to archive fails.
    fn delete_file_from_dest(&self, path: &Path, dest_dir: &Path) -> Result<(), SyncError>;

    /// Prune archive directory on the destination.
    ///
    /// # Errors
    /// Returns `SyncError` if walking or deleting old archive files fails.
    fn prune_archive(&self, _dest_dir: &Path) -> Result<(), SyncError> {
        Ok(())
    }

    /// Run a full scan and sync cycle that can be interrupted by the `cancel` signal.
    ///
    /// # Errors
    /// Returns `SyncError::Cancelled` if cancelled, or `SyncError` on scanning/sync failures.
    fn run_cancellable_full_scan(
        &self,
        dest_dir: &Path,
        _cancel: &std::sync::atomic::AtomicBool,
    ) -> Result<ScanOutcome, SyncError> {
        let _ = dest_dir;
        Ok(ScanOutcome::Success { synced: 0 })
    }

    /// Perform a full directory scan on `dest_dir` and sync all changed files.
    ///
    /// # Errors
    /// Returns `SyncError` on directory traversal or synchronization failure.
    fn run_full_scan(&self, dest_dir: &Path) -> Result<ScanOutcome, SyncError> {
        static NEVER_CANCELLED: std::sync::atomic::AtomicBool =
            std::sync::atomic::AtomicBool::new(false);
        self.run_cancellable_full_scan(dest_dir, &NEVER_CANCELLED)
    }
}

/// Snapshot of a file's size and modification timestamp for drift detection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileMetadataSnapshot {
    pub size: i64,
    pub modified_epoch_millis: i64,
}

impl FileMetadataSnapshot {
    /// Create a new metadata snapshot.
    pub fn new(size: i64, modified_epoch_millis: i64) -> Self {
        Self {
            size,
            modified_epoch_millis,
        }
    }

    /// Compute snapshot from `std::fs::Metadata`
    pub fn from_metadata(meta: &std::fs::Metadata) -> Result<Self, SyncError> {
        Ok(Self {
            size: meta.len() as i64,
            modified_epoch_millis: safe_modified_millis(meta)?,
        })
    }

    /// Check if destination metadata matches source snapshot and optional DB record.
    #[must_use]
    pub fn is_up_to_date(
        &self,
        dest: &FileMetadataSnapshot,
        record: Option<&crate::db::FileRecord>,
    ) -> bool {
        if let Some(record) = record
            && record.file_size == self.size
            && record.last_modified == self.modified_epoch_millis
            && dest.size == self.size
            && dest
                .modified_epoch_millis
                .abs_diff(self.modified_epoch_millis)
                <= 2000
        {
            return true;
        }
        false
    }
}

/// Raw metadata evaluation for testing and backward compatibility.
#[doc(hidden)]
pub fn is_metadata_up_to_date_raw(
    dest: &FileMetadataSnapshot,
    src: &FileMetadataSnapshot,
    record: Option<&crate::db::FileRecord>,
) -> bool {
    src.is_up_to_date(dest, record)
}

/// Delta sync engine backed by a `HashStore` for signature caching.
pub struct LocalSyncEngine<S: HashStore> {
    pub(crate) db: S,
    pub(crate) config: TargetSyncConfig,
    pub(crate) resolved_dest: Option<PathBuf>,
    pub(crate) dirty_range: std::sync::Mutex<DirtyBlockRange>,
    pub(crate) verified_dirs: std::sync::Mutex<HashSet<PathBuf>>,
}

#[derive(Debug)]
pub(crate) struct FileSyncTask<'a> {
    pub rel_path: &'a Path,
    pub src_path: &'a Path,
    pub dest_path: &'a Path,
    pub dest_dir: &'a Path,
    pub src_size: i64,
    pub src_mod: i64,
    pub cached_id: Option<i64>,
}

impl<S: HashStore> LocalSyncEngine<S> {
    /// Create a new sync engine with the given database and config.
    pub fn new(db: S, config: impl Into<TargetSyncConfig>) -> Self {
        let config = config.into();
        let block_size = config.block_size_bytes();
        Self {
            db,
            config,
            resolved_dest: None,
            dirty_range: std::sync::Mutex::new(DirtyBlockRange::new(block_size)),
            verified_dirs: std::sync::Mutex::new(HashSet::new()),
        }
    }

    /// Set a pre-resolved destination path (e.g. from ReachabilityMonitor or worker context).
    pub fn with_resolved_dest(mut self, dest: impl Into<PathBuf>) -> Self {
        self.resolved_dest = Some(dest.into());
        self
    }

    /// Get the pre-resolved destination path if configured.
    pub fn resolved_dest(&self) -> Option<&Path> {
        self.resolved_dest.as_deref()
    }

    /// Perform a full directory scan on default or pre-resolved destination.
    pub fn run_full_scan(&self) -> Result<ScanOutcome, SyncError> {
        static NEVER_CANCELLED: std::sync::atomic::AtomicBool =
            std::sync::atomic::AtomicBool::new(false);
        let dest = self
            .resolved_dest
            .as_deref()
            .unwrap_or_else(|| self.config.dest_dir());
        self.run_cancellable_full_scan_impl(dest, &NEVER_CANCELLED)
    }

    /// Synchronize a file or directory tree to a specific destination directory (primary or alternate).
    pub fn sync_file_to_dest(&self, rel_path: &Path, dest_dir: &Path) -> Result<(), SyncError> {
        let mut scratch = vec![0u8; self.config.block_size_bytes() as usize];
        self.sync_file_to_dest_buffered(rel_path, dest_dir, &mut scratch)
    }

    fn is_metadata_up_to_date(
        dest_meta: Option<&std::fs::Metadata>,
        src_size: i64,
        src_mod: i64,
        rec: Option<&FileRecord>,
    ) -> bool {
        if let Some(dest_meta) = dest_meta {
            let dest = FileMetadataSnapshot {
                size: dest_meta.len() as i64,
                modified_epoch_millis: safe_modified_millis(dest_meta).unwrap_or(0),
            };
            let src = FileMetadataSnapshot {
                size: src_size,
                modified_epoch_millis: src_mod,
            };
            return is_metadata_up_to_date_raw(&dest, &src, rec);
        }
        false
    }

    pub(crate) fn sync_file_core(
        &self,
        task: &FileSyncTask<'_>,
        scratch: &mut [u8],
    ) -> Result<(FileRecord, Vec<crate::db::BlockHash>), SyncError> {
        if (task.src_size as u64) < self.config.block_sync_threshold_bytes() {
            self.sync_small_file_core(task, scratch)
        } else {
            self.sync_delta_large_file_core(task, scratch)
        }
    }

    pub(crate) fn sync_file_to_dest_core(
        &self,
        rel_path: &Path,
        dest_dir: &Path,
        scratch: &mut [u8],
        file_record: Option<&FileRecord>,
    ) -> Result<Option<(FileRecord, Vec<crate::db::BlockHash>)>, SyncError> {
        if !is_safe_relative_path(rel_path) {
            return Err(SyncError::validation(format!(
                "Unsafe path traversal detected: {}",
                rel_path.display()
            )));
        }
        let src_path = self.config.source_dir().join(rel_path);
        let dest_path = dest_dir.join(rel_path);

        let sym_meta = fs::symlink_metadata(&src_path).map_err(SyncError::Io)?;
        if is_reparse_or_symlink_meta(&sym_meta) {
            tracing::debug!(path = %src_path.display(), "Skipping symlink or reparse point");
            return Ok(None);
        }
        verify_source_not_reparse(self.config.source_dir(), rel_path)?;
        if sym_meta.is_dir() {
            let _ = {
                let mut cache = self
                    .verified_dirs
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                verify_destination_not_reparse_cached(dest_dir, rel_path, &mut cache)?
            };
            fs::create_dir_all(&dest_path)?;
            let mut dir_files = HashSet::new();
            let mut scan_complete = true;
            scan_dir(
                &src_path,
                self.config.source_dir(),
                &mut dir_files,
                &mut scan_complete,
                0,
            )?;
            for child_rel in &dir_files {
                self.sync_file_to_dest_buffered(child_rel, dest_dir, scratch)?;
            }
            return Ok(None);
        }

        let dest_meta = {
            let mut cache = self
                .verified_dirs
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            verify_destination_not_reparse_cached(dest_dir, rel_path, &mut cache)?
        };

        let src_size = sym_meta.len() as i64;
        let src_mod = safe_modified_millis(&sym_meta)?;

        if Self::is_metadata_up_to_date(dest_meta.as_ref(), src_size, src_mod, file_record) {
            tracing::debug!(path = %rel_path.display(), "Metadata unchanged, skipping sync");
            return Ok(None);
        }

        let cached_id = file_record.and_then(|r| r.id);
        let task = FileSyncTask {
            rel_path,
            src_path: &src_path,
            dest_path: &dest_path,
            dest_dir,
            src_size,
            src_mod,
            cached_id,
        };

        let (record, hashes) = self.sync_file_core(&task, scratch)?;
        Ok(Some((record, hashes)))
    }

    fn sync_file_to_dest_buffered_with_record(
        &self,
        rel_path: &Path,
        dest_dir: &Path,
        scratch: &mut [u8],
        file_record: Option<&FileRecord>,
    ) -> Result<(), SyncError> {
        if let Some((record, hashes)) =
            self.sync_file_to_dest_core(rel_path, dest_dir, scratch, file_record)?
        {
            self.db.save_file(&record, &hashes)?;
        }
        Ok(())
    }

    /// Synchronize a file or directory tree using a reusable scratch buffer.
    pub fn sync_file_to_dest_buffered(
        &self,
        rel_path: &Path,
        dest_dir: &Path,
        scratch: &mut [u8],
    ) -> Result<(), SyncError> {
        let file_record = self.db.get_file(rel_path)?;
        self.sync_file_to_dest_buffered_with_record(
            rel_path,
            dest_dir,
            scratch,
            file_record.as_ref(),
        )
    }
}

impl<S: HashStore> SyncEngine for LocalSyncEngine<S> {
    fn sync_file(&self, path: &Path) -> Result<(), SyncError> {
        let dest = self
            .resolved_dest
            .as_deref()
            .unwrap_or_else(|| self.config.dest_dir());
        self.sync_file_to_dest(path, dest)
    }

    fn sync_file_to_dest_buffered(
        &self,
        path: &Path,
        dest_dir: &Path,
        scratch: &mut [u8],
    ) -> Result<(), SyncError> {
        self.sync_file_to_dest_buffered(path, dest_dir, scratch)
    }

    fn delete_file(&self, path: &Path) -> Result<(), SyncError> {
        let dest = self
            .resolved_dest
            .as_deref()
            .unwrap_or_else(|| self.config.dest_dir());
        self.delete_file_from_dest(path, dest)
    }

    fn delete_file_from_dest(&self, path: &Path, dest_dir: &Path) -> Result<(), SyncError> {
        self.delete_file_from_dest(path, dest_dir)
    }

    fn prune_archive(&self, dest_dir: &Path) -> Result<(), SyncError> {
        self.prune_destination_archive(dest_dir)
    }

    fn run_cancellable_full_scan(
        &self,
        dest_dir: &Path,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> Result<ScanOutcome, SyncError> {
        self.run_cancellable_full_scan_impl(dest_dir, cancel)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, TargetSyncConfig};
    use crate::db::{MockHashStore, SqliteHashStore};
    use pretty_assertions::assert_eq;
    use std::fs::OpenOptions;
    use std::time::SystemTime;
    use tempfile::tempdir;

    #[cfg(windows)]
    fn create_test_junction(target: &Path, link: &Path) -> std::io::Result<()> {
        if std::os::windows::fs::symlink_dir(target, link).is_err() {
            let status = std::process::Command::new("powershell")
                .args([
                    "-Command",
                    &format!(
                        "New-Item -ItemType Junction -Path '{}' -Target '{}' -Force",
                        link.display(),
                        target.display()
                    ),
                ])
                .status()?;
            if !status.success() {
                return Err(std::io::Error::other("Failed to create test junction"));
            }
        }
        Ok(())
    }

    #[test]
    fn test_safe_epoch_duration_millis_positive() {
        assert_eq!(
            safe_epoch_duration_millis(1000),
            std::time::Duration::from_millis(1000)
        );
    }

    #[test]
    fn test_safe_epoch_duration_millis_negative() {
        assert_eq!(
            safe_epoch_duration_millis(-500),
            std::time::Duration::from_millis(0)
        );
    }

    #[test]
    fn test_is_metadata_up_to_date_raw() {
        let record = crate::db::FileRecord::new(PathBuf::from("file.txt"), 100, 10_000).with_id(1);
        let snap = |size, millis| FileMetadataSnapshot::new(size, millis);

        // Exact match
        assert!(is_metadata_up_to_date_raw(
            &snap(100, 10_000),
            &snap(100, 10_000),
            Some(&record)
        ));

        // Within 2000ms SMB tolerance
        assert!(is_metadata_up_to_date_raw(
            &snap(100, 11_500),
            &snap(100, 10_000),
            Some(&record)
        ));
        assert!(is_metadata_up_to_date_raw(
            &snap(100, 8_500),
            &snap(100, 10_000),
            Some(&record)
        ));

        // Beyond 2000ms tolerance
        assert!(!is_metadata_up_to_date_raw(
            &snap(100, 12_500),
            &snap(100, 10_000),
            Some(&record)
        ));
    }

    #[test]
    fn test_smb_timestamp_tolerance_fast_path() {
        let dir = tempdir().unwrap();
        let source = dir.path().join("src");
        let dest = dir.path().join("dst");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&dest).unwrap();

        let file_name = "test.txt";
        fs::write(source.join(file_name), b"test content").unwrap();

        let config = Config::test_default(source.clone(), dest.clone());
        let target_cfg = TargetSyncConfig::try_from_config(&config).unwrap();
        let store = MockHashStore::new();
        let engine = LocalSyncEngine::new(store, target_cfg);

        // First sync
        engine.sync_file(Path::new(file_name)).unwrap();
        assert!(dest.join(file_name).exists());

        // Modify destination file timestamp slightly (1500 ms off) to simulate SMB rounding
        let dest_file = OpenOptions::new()
            .write(true)
            .open(dest.join(file_name))
            .unwrap();
        let src_meta = fs::metadata(source.join(file_name)).unwrap();
        let src_mtime = src_meta.modified().unwrap();
        let rounded_mtime = src_mtime - std::time::Duration::from_millis(1500);
        dest_file
            .set_times(fs::FileTimes::new().set_modified(rounded_mtime))
            .unwrap();

        // Second sync: should fast-path return Ok(()) due to ±2000 ms tolerance
        engine.sync_file(Path::new(file_name)).unwrap();
    }

    #[test]
    fn test_sync_file_directory_creation() {
        let dir = tempdir().unwrap();
        let source = dir.path().join("src");
        let dest = dir.path().join("dst");
        fs::create_dir_all(source.join("new_folder")).unwrap();
        fs::create_dir_all(&dest).unwrap();

        let config = Config::test_default(source.clone(), dest.clone());
        let target_cfg = TargetSyncConfig::try_from_config(&config).unwrap();
        let store = MockHashStore::new();
        let engine = LocalSyncEngine::new(store, target_cfg);

        // sync_file on a directory path should create the folder on dest and return Ok(())
        engine.sync_file(Path::new("new_folder")).unwrap();
        assert!(dest.join("new_folder").is_dir());
    }

    #[test]
    fn test_sync_file_truncated_dest_recovers_missing_blocks() {
        let dir = tempdir().unwrap();
        let source = dir.path().join("src");
        let dest = dir.path().join("dst");
        let db_path = dir.path().join("sig.db");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&dest).unwrap();

        let config = Config::builder(source.clone())
            .dest_dir(dest.clone())
            .block_sync_threshold_bytes(4)
            .block_size_bytes(4)
            .build();
        let store =
            SqliteHashStore::new(&db_path, crate::db::StoreConfig::try_from(&config).unwrap())
                .unwrap();
        let target_cfg = TargetSyncConfig::try_from_config(&config).unwrap();
        let engine = LocalSyncEngine::new(store, target_cfg);

        // 12 bytes = 3 blocks of 4 bytes
        fs::write(source.join("file.bin"), b"AAAABBBBCCCC").unwrap();
        engine.sync_file(Path::new("file.bin")).unwrap();
        assert_eq!(fs::read(dest.join("file.bin")).unwrap(), b"AAAABBBBCCCC");

        // Truncate dest file to 4 bytes simulating interrupted transfer
        let dest_file = OpenOptions::new()
            .write(true)
            .open(dest.join("file.bin"))
            .unwrap();
        dest_file.set_len(4).unwrap();
        drop(dest_file);

        // Advance source mtime to bypass mtime fast path
        let src_file = OpenOptions::new()
            .write(true)
            .open(source.join("file.bin"))
            .unwrap();
        src_file
            .set_times(
                fs::FileTimes::new()
                    .set_modified(SystemTime::now() + std::time::Duration::from_secs(5)),
            )
            .unwrap();
        drop(src_file);

        // Sync again: Even though DB has matching hashes for blocks 1 and 2,
        // dest_len < expected means blocks 1 and 2 must be written and NOT zero-filled!
        engine.sync_file(Path::new("file.bin")).unwrap();
        assert_eq!(fs::read(dest.join("file.bin")).unwrap(), b"AAAABBBBCCCC");
    }

    #[test]
    fn test_sync_file_toctou_set_len_uses_total_bytes_read() {
        let temp = tempdir().unwrap();
        let src = temp.path().join("source");
        let dst = temp.path().join("dest");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&dst).unwrap();
        let config = Config::builder(src.clone())
            .dest_dir(dst.clone())
            .block_size_bytes(512)
            .block_sync_threshold_bytes(1024)
            .build();
        let target_cfg = TargetSyncConfig::try_from_config(&config).unwrap();
        let engine = LocalSyncEngine::new(MockHashStore::new(), target_cfg);

        std::fs::write(src.join("file.bin"), vec![0xEEu8; 2048]).unwrap();
        let mut scratch = vec![0u8; 512];
        engine
            .sync_file_to_dest_buffered(Path::new("file.bin"), &dst, &mut scratch)
            .unwrap();
        assert_eq!(std::fs::metadata(dst.join("file.bin")).unwrap().len(), 2048);

        // Actual file shrank to 1024 bytes, but stale src_size of 2048 is passed
        std::fs::write(src.join("file.bin"), vec![0x55u8; 1024]).unwrap();
        let src_file = src.join("file.bin");
        let dst_file = dst.join("file.bin");
        let src_mod = safe_modified_millis(&std::fs::metadata(&src_file).unwrap()).unwrap();
        let task = FileSyncTask {
            rel_path: Path::new("file.bin"),
            src_path: &src_file,
            dest_path: &dst_file,
            dest_dir: &dst,
            src_size: 2048,
            src_mod,
            cached_id: None,
        };
        engine.sync_delta_large_file(&task, &mut scratch).unwrap();
        let dest_len = std::fs::metadata(dst.join("file.bin")).unwrap().len();
        assert_eq!(
            dest_len, 1024,
            "set_len must use total_bytes_read (1024), not stale src_size (2048)"
        );
    }

    #[cfg(windows)]
    #[test]
    fn test_sync_refuses_dest_symlink_overwrite() {
        let temp = tempdir().unwrap();
        let src = temp.path().join("source");
        let dst = temp.path().join("dest");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&dst).unwrap();
        let config = Config::test_default(src.clone(), dst.clone());
        let target_cfg = TargetSyncConfig::try_from_config(&config).unwrap();
        let engine = LocalSyncEngine::new(MockHashStore::new(), target_cfg);
        std::fs::write(src.join("target.txt"), b"content").unwrap();
        let link_target = temp.path().join("link_target");
        std::fs::create_dir_all(&link_target).unwrap();
        create_test_junction(&link_target, &dst.join("target.txt")).unwrap();

        let mut scratch = vec![0u8; 4096];
        let res = engine.sync_file_to_dest_buffered(Path::new("target.txt"), &dst, &mut scratch);
        assert!(
            res.is_err(),
            "Engine must return Err when dest is a symlink/reparse point"
        );
    }

    #[test]
    fn test_run_full_scan_uses_pre_resolved_dest() {
        let temp = tempdir().unwrap();
        let src = temp.path().join("src");
        let unreachable_dst = temp.path().join("unreachable_dst");
        let alt_dst = temp.path().join("alt_dst");
        fs::create_dir_all(&src).unwrap();
        fs::create_dir_all(&alt_dst).unwrap();
        fs::write(src.join("hello.txt"), b"test pre-resolved dest").unwrap();

        let config = Config::test_default(src, unreachable_dst);
        let target_cfg = TargetSyncConfig::try_from_config(&config).unwrap();
        let engine =
            LocalSyncEngine::new(MockHashStore::new(), target_cfg).with_resolved_dest(&alt_dst);
        assert_eq!(engine.resolved_dest(), Some(alt_dst.as_path()));

        let outcome = engine.run_full_scan().unwrap();
        assert!(matches!(outcome, ScanOutcome::Success { synced: 1 }));
        assert!(alt_dst.join("hello.txt").exists());
    }
}

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
/// use syncdir::sync::{ConnectivityState, SyncStatusObserver, WatcherState};
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
/// use syncdir::sync::{MockSyncEngine, SyncEngine};
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
    /// Perform a full directory scan on `dest_dir` and sync all changed files.
    ///
    /// # Errors
    /// Returns `SyncError` on directory traversal or synchronization failure.
    fn run_full_scan(&self, dest_dir: &Path) -> Result<ScanOutcome, SyncError> {
        static NEVER_CANCELLED: std::sync::atomic::AtomicBool =
            std::sync::atomic::AtomicBool::new(false);
        self.run_cancellable_full_scan(dest_dir, &NEVER_CANCELLED)
    }

    /// Invalidate any cached directory metadata (e.g. reparse point and ancestor junction checks).
    ///
    /// Clears internal directory verification caches to ensure subsequent operations re-inspect
    /// filesystem components. Called upon destination reconnection, full scan initiation, or error recovery.
    /// Default implementation is a no-op for mock or non-caching engines.
    fn invalidate_verified_dirs(&self) {}
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

impl From<&crate::db::FileRecord> for FileMetadataSnapshot {
    fn from(record: &crate::db::FileRecord) -> Self {
        Self {
            size: record.file_size.max(0),
            modified_epoch_millis: record.last_modified.max(0),
        }
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

/// RAII lease for a `DirtyBlockRange` buffer checked out from `LocalSyncEngine`.
///
/// On drop, resets the buffer and returns it to the pool, retaining whichever has
/// greater allocation capacity if the pool is already populated.
pub struct DirtyRangeLease<'a> {
    pool: &'a std::sync::Mutex<Option<DirtyBlockRange>>,
    range: Option<DirtyBlockRange>,
}

impl<'a> DirtyRangeLease<'a> {
    pub(crate) fn new(
        pool: &'a std::sync::Mutex<Option<DirtyBlockRange>>,
        range: DirtyBlockRange,
    ) -> Self {
        Self {
            pool,
            range: Some(range),
        }
    }
}

impl<'a> std::ops::Deref for DirtyRangeLease<'a> {
    type Target = DirtyBlockRange;

    fn deref(&self) -> &Self::Target {
        self.range
            .as_ref()
            .expect("DirtyRangeLease invariant violated: range is None")
    }
}

impl<'a> std::ops::DerefMut for DirtyRangeLease<'a> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.range
            .as_mut()
            .expect("DirtyRangeLease invariant violated: range is None")
    }
}

impl<'a> Drop for DirtyRangeLease<'a> {
    fn drop(&mut self) {
        if let Some(mut range) = self.range.take() {
            range.reset();
            let mut pool = self
                .pool
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            match pool.as_mut() {
                Some(existing) => {
                    if range.capacity() > existing.capacity() {
                        *existing = range;
                    }
                }
                None => {
                    *pool = Some(range);
                }
            }
        }
    }
}

/// Delta sync engine backed by a `HashStore` for signature caching, composed of collaborating transfer, archive, and scan engines.
pub struct LocalSyncEngine<S: HashStore> {
    pub(crate) db: std::sync::Arc<S>,
    pub(crate) config: TargetSyncConfig,
    pub(crate) resolved_dest: Option<PathBuf>,
    pub(crate) verified_dirs: std::sync::Mutex<HashSet<PathBuf>>,
    pub(crate) small_file_engine: crate::sync::small_file::SmallFileTransferEngine,
    pub(crate) delta_engine: crate::sync::delta::DeltaTransferEngine<std::sync::Arc<S>>,
    pub(crate) archive_manager: crate::sync::archive::ArchiveManager,
    pub(crate) scanner: crate::sync::scanner::DirectoryScanner,
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
        let db = std::sync::Arc::new(db);
        let small_file_engine =
            crate::sync::small_file::SmallFileTransferEngine::new(config.clone());
        let delta_engine = crate::sync::delta::DeltaTransferEngine::new(
            std::sync::Arc::clone(&db),
            config.clone(),
        );
        let archive_manager = crate::sync::archive::ArchiveManager::new(config.clone());
        let scanner = crate::sync::scanner::DirectoryScanner::new(config.clone());
        Self {
            db,
            config,
            resolved_dest: None,
            verified_dirs: std::sync::Mutex::new(HashSet::new()),
            small_file_engine,
            delta_engine,
            archive_manager,
            scanner,
        }
    }

    /// Invalidate any cached directory metadata (e.g. reparse point and ancestor junction checks).
    ///
    /// Clears the internal `verified_dirs` cache so that subsequent synchronizations re-verify
    /// the entire path tree against reparse point and symlink substitution attacks.
    pub fn invalidate_verified_dirs(&self) {
        let mut cache = self
            .verified_dirs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        cache.clear();
    }

    /// Evict a specific directory and its descendants from the verified directory cache.
    ///
    /// Removes `dir` and any path starting with `dir` from the cache. Called when files or
    /// directories are deleted to close the time-of-check to time-of-use (TOCTOU) substitution window.
    ///
    /// # Arguments
    ///
    /// * `dir` - The directory path whose cache entries should be purged.
    pub fn evict_verified_dir(&self, dir: &Path) {
        let mut cache = self
            .verified_dirs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        cache.retain(|p| !p.starts_with(dir));
    }

    /// Acquire an exclusive RAII lease on a `DirtyBlockRange` buffer.
    ///
    /// The buffer is checked out without holding locks during file I/O and returned
    /// to the pool automatically on lease drop.
    ///
    /// # Returns
    ///
    /// A [`DirtyRangeLease`] handle providing mutable access to a pooled or newly allocated buffer.
    #[allow(dead_code)]
    pub(crate) fn acquire_dirty_range_lease(&self) -> DirtyRangeLease<'_> {
        self.delta_engine.acquire_dirty_range_lease()
    }

    /// Return the capacity of the pooled buffer if present, or 0 if checked out or empty.
    #[cfg(test)]
    pub(crate) fn dirty_range_capacity(&self) -> usize {
        self.delta_engine.dirty_range_capacity()
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
            self.small_file_engine.sync_small_file_core(task, scratch)
        } else {
            self.delta_engine.sync_delta_large_file_core(task, scratch)
        }
    }

    pub(crate) fn verify_destination_cached(
        &self,
        dest_dir: &Path,
        rel_path: &Path,
    ) -> Result<Option<std::fs::Metadata>, SyncError> {
        let (root_verified, unverified_ancestors) = {
            let cache = self
                .verified_dirs
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let root_ok = cache.contains(dest_dir);
            let mut unverified = Vec::new();
            let mut curr = dest_dir.to_path_buf();
            let components: Vec<_> = rel_path.components().collect();
            let total = components.len();
            for (i, c) in components.into_iter().enumerate() {
                curr.push(c);
                if i + 1 < total && !cache.contains(&curr) {
                    unverified.push(curr.clone());
                }
            }
            (root_ok, unverified)
        };

        if !root_verified || !unverified_ancestors.is_empty() {
            let mut cache = self
                .verified_dirs
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            verify_destination_not_reparse_cached(dest_dir, rel_path, &mut cache)
        } else {
            let leaf_path = dest_dir.join(rel_path);
            let meta = match fs::symlink_metadata(&leaf_path) {
                Ok(m) => {
                    if is_reparse_or_symlink_meta(&m) {
                        return Err(SyncError::validation(format!(
                            "Destination component '{}' is a symlink or reparse point; refusing to write",
                            leaf_path.display()
                        )));
                    }
                    Some(m)
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
                Err(e) => return Err(SyncError::Io(e)),
            };
            Ok(meta)
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
            let _ = self.verify_destination_cached(dest_dir, rel_path)?;
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

        let dest_meta = self.verify_destination_cached(dest_dir, rel_path)?;

        let src_size = sym_meta.len() as i64;
        let src_mod = safe_modified_millis(&sym_meta)?;

        if let Some(dest_meta_ref) = dest_meta.as_ref() {
            let dest_size = dest_meta_ref.len() as i64;
            let dest_mod = safe_modified_millis(dest_meta_ref).unwrap_or(0);
            if dest_size == src_size && dest_mod.abs_diff(src_mod) <= 2000 {
                if let Some(record) = file_record
                    && record.is_tracked()
                    && record.file_size == src_size
                    && record.last_modified == src_mod
                {
                    tracing::debug!(path = %rel_path.display(), "Local signature cache hit and destination matches, skipping sync");
                    return Ok(None);
                }

                if Self::is_metadata_up_to_date(Some(dest_meta_ref), src_size, src_mod, file_record)
                {
                    tracing::debug!(path = %rel_path.display(), "Metadata unchanged, skipping sync");
                    return Ok(None);
                }
            } else {
                tracing::debug!(
                    path = %rel_path.display(),
                    src_size,
                    dest_size,
                    "Destination file size or timestamp mismatch; re-synchronizing"
                );
            }
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

    fn invalidate_verified_dirs(&self) {
        self.invalidate_verified_dirs();
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
            .build()
            .unwrap();
        let store = SqliteHashStore::new(
            &db_path,
            crate::db::StoreConfig::new(
                config.block_size_bytes(),
                config.block_sync_threshold_bytes(),
            )
            .unwrap(),
        )
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
            .build()
            .unwrap();
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

    #[test]
    fn test_sync_file_to_dest_core_cache_hit() {
        let temp = tempdir().unwrap();
        let src = temp.path().join("src");
        let dst = temp.path().join("dst");
        fs::create_dir_all(&src).unwrap();
        fs::create_dir_all(&dst).unwrap();

        let file_rel = Path::new("cached_file.txt");
        let src_file = src.join(file_rel);
        fs::write(&src_file, b"cache hit payload").unwrap();
        let meta = fs::symlink_metadata(&src_file).unwrap();
        let src_size = meta.len() as i64;
        let src_mod = safe_modified_millis(&meta).unwrap();

        let record = FileRecord::new(file_rel.to_path_buf(), src_size, src_mod).with_id(42);

        let config = Config::test_default(src, dst.clone());
        let target_cfg = TargetSyncConfig::try_from_config(&config).unwrap();
        let engine = LocalSyncEngine::new(MockHashStore::new(), target_cfg);

        let dst_file = dst.join(file_rel);
        fs::write(&dst_file, b"cache hit payload").unwrap();

        let mut scratch = vec![0u8; 4096];
        let res = engine.sync_file_to_dest_core(file_rel, &dst, &mut scratch, Some(&record));
        assert!(res.is_ok());
        assert_eq!(res.unwrap(), None);
        assert_eq!(
            fs::read(&dst_file).unwrap(),
            b"cache hit payload",
            "Fast path should not overwrite destination when cache hits"
        );
    }

    #[test]
    fn test_sync_file_to_dest_core_repairs_truncated_dest_file() {
        let temp = tempdir().unwrap();
        let src = temp.path().join("src");
        let dst = temp.path().join("dst");
        fs::create_dir_all(&src).unwrap();
        fs::create_dir_all(&dst).unwrap();

        let file_rel = Path::new("truncated.txt");
        let src_file = src.join(file_rel);
        let payload = b"complete source payload of substantial length";
        fs::write(&src_file, payload).unwrap();
        let meta = fs::symlink_metadata(&src_file).unwrap();
        let src_size = meta.len() as i64;
        let src_mod = safe_modified_millis(&meta).unwrap();

        let record = FileRecord::new(file_rel.to_path_buf(), src_size, src_mod).with_id(99);
        let config = Config::test_default(src, dst.clone());
        let target_cfg = TargetSyncConfig::try_from_config(&config).unwrap();
        let engine = LocalSyncEngine::new(MockHashStore::new(), target_cfg);

        let dst_file = dst.join(file_rel);
        fs::write(&dst_file, b"truncated").unwrap(); // 9 bytes vs 45 bytes

        let mut scratch = vec![0u8; 4096];
        let res = engine.sync_file_to_dest_core(file_rel, &dst, &mut scratch, Some(&record));
        assert!(res.is_ok());
        assert!(
            res.unwrap().is_some(),
            "Truncated destination must trigger synchronization"
        );
        assert_eq!(
            fs::read(&dst_file).unwrap(),
            payload,
            "Destination file must be repaired to match source"
        );
    }

    #[test]
    fn test_dirty_range_lease_pool() {
        let temp = tempdir().unwrap();
        let src = temp.path().join("src");
        let dst = temp.path().join("dst");
        let config = Config::test_default(src, dst);
        let target_cfg = TargetSyncConfig::try_from_config(&config).unwrap();
        let engine = LocalSyncEngine::new(MockHashStore::new(), target_cfg);

        // Initially pool is empty, capacity is 0
        assert_eq!(engine.dirty_range_capacity(), 0);

        {
            let mut lease = engine.acquire_dirty_range_lease();
            let mut sink = std::io::Cursor::new(Vec::new());
            lease.add_block(0, &[1, 2, 3, 4], &mut sink).unwrap();
            assert_eq!(lease.block_count(), 1);
            // While checked out, pool is empty
            assert_eq!(engine.dirty_range_capacity(), 0);
        }

        // After drop, pool has the buffer reset (block_count 0) and capacity preserved
        assert!(engine.dirty_range_capacity() > 0);

        {
            let lease2 = engine.acquire_dirty_range_lease();
            assert_eq!(lease2.block_count(), 0);
            assert_eq!(engine.dirty_range_capacity(), 0);
        }
        assert!(engine.dirty_range_capacity() > 0);
    }

    #[test]
    fn test_verified_dirs_junction_substitution_detection_and_invalidation() {
        let temp = tempdir().unwrap();
        let src = temp.path().join("src");
        let dst = temp.path().join("dst");
        let sub = dst.join("sub");
        let nested = sub.join("nested");
        fs::create_dir_all(&src).unwrap();
        fs::create_dir_all(&nested).unwrap();

        let config = Config::test_default(src, dst.clone());
        let target_cfg = TargetSyncConfig::try_from_config(&config).unwrap();
        let engine = LocalSyncEngine::new(MockHashStore::new(), target_cfg);

        // Populate verified_dirs cache via verify_destination_cached
        let _ = engine.verify_destination_cached(&dst, Path::new("sub/nested/file.txt"));
        {
            let cache = engine.verified_dirs.lock().unwrap();
            assert!(cache.contains(&dst));
            assert!(cache.contains(&sub));
            assert!(cache.contains(&nested));
        }

        // Test prefix eviction: evicting sub must remove sub and sub/nested, keeping dst
        engine.evict_verified_dir(&sub);
        {
            let cache = engine.verified_dirs.lock().unwrap();
            assert!(cache.contains(&dst), "Root destination must remain cached");
            assert!(!cache.contains(&sub), "Evicted dir must be removed");
            assert!(
                !cache.contains(&nested),
                "Child of evicted dir must be removed"
            );
        }

        // Test full cache invalidation via SyncEngine trait
        let engine_trait: &dyn SyncEngine = &engine;
        engine_trait.invalidate_verified_dirs();
        {
            let cache = engine.verified_dirs.lock().unwrap();
            assert!(cache.is_empty(), "Full invalidation must clear all entries");
        }
    }

    #[test]
    fn test_composed_local_sync_engine_end_to_end_regression() {
        let temp = tempdir().unwrap();
        let src = temp.path().join("src");
        let dst = temp.path().join("dst");
        let db_path = temp.path().join("sig.db");
        fs::create_dir_all(&src).unwrap();
        fs::create_dir_all(&dst).unwrap();

        // 1. Verify From<&FileRecord> for FileMetadataSnapshot
        let rec = crate::db::FileRecord {
            id: Some(42),
            relative_path: PathBuf::from("rec.txt"),
            file_size: 1024,
            last_modified: 1_700_000_000_000,
        };
        let snap = FileMetadataSnapshot::from(&rec);
        assert_eq!(snap.size, 1024);
        assert_eq!(snap.modified_epoch_millis, 1_700_000_000_000);

        // 2. End-to-end test composed LocalSyncEngine via SyncEngine trait
        let config = Config::builder(src.clone())
            .dest_dir(dst.clone())
            .debounce_seconds(1)
            .retry_interval_seconds(1)
            .propagate_deletions(true)
            .block_sync_threshold_bytes(64)
            .block_size_bytes(16)
            .build()
            .unwrap();

        let store_cfg = crate::db::StoreConfig::new(
            config.block_size_bytes(),
            config.block_sync_threshold_bytes(),
        )
        .unwrap();
        let store = SqliteHashStore::new(&db_path, store_cfg).unwrap();
        let target_cfg = TargetSyncConfig::from_config(&config, dst.clone()).unwrap();
        let engine = LocalSyncEngine::new(store, target_cfg);

        // Small file sync (< 64 bytes threshold)
        fs::write(src.join("small.txt"), b"small content").unwrap();
        engine.sync_file(Path::new("small.txt")).unwrap();
        assert_eq!(fs::read(dst.join("small.txt")).unwrap(), b"small content");

        // Large file sync (>= 64 bytes threshold)
        let large_payload = vec![0xEEu8; 128];
        fs::write(src.join("large.bin"), &large_payload).unwrap();
        engine.sync_file(Path::new("large.bin")).unwrap();
        assert_eq!(fs::read(dst.join("large.bin")).unwrap(), large_payload);

        // Deletion and archive verification
        fs::remove_file(src.join("small.txt")).unwrap();
        engine
            .delete_file_from_dest(Path::new("small.txt"), &dst)
            .unwrap();
        assert!(!dst.join("small.txt").exists());
        assert!(dst.join(".syncdir_archive").exists());

        // Full scan verification
        let scan_res = engine.run_full_scan().unwrap();
        assert!(matches!(scan_res, ScanOutcome::Success { .. }));
    }
}

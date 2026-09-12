//! Core sync engine trait, commands, status types, and LocalSyncEngine coordination.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use crate::config::TargetSyncConfig;
use crate::db::{FileRecord, HashStore};
use crate::error::SyncError;

use super::delta::DirtyBlockRange;
use super::path_safety::{is_reparse_or_symlink_meta, is_safe_relative_path};
use super::scanner::scan_dir;

#[allow(unused_imports)]
pub use super::types::{
    FileSyncTask, RelativePath, safe_epoch_duration_millis, safe_modified_millis,
};

/// Commands sent from the file watcher or tray UI to the sync worker thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncCommand {
    /// A file was created or modified at the given relative path.
    FileModified(RelativePath),
    /// A file was deleted at the given relative path.
    FileDeleted(RelativePath),
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

/// Core sync execution contract for synchronizing files and directory trees.
///
/// `SyncEngine` serves as the primary behavioral abstraction decoupling sync workers
/// and daemon orchestration from low-level filesystem I/O, hash database caching, and delta transfers.
///
/// Production implementations (like [`LocalSyncEngine`]) coordinate atomic small-file copies,
/// block-level delta transfers, path traversal safety checks, and destination archiving. Test
/// suites utilize [`MockSyncEngine`](crate::sync::MockSyncEngine) to verify worker state machines without live disk access.
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
/// Role trait for synchronizing files.
pub trait FileSynchronizer: Send + Sync {
    /// Synchronize a single file from source to the default configured destination.
    fn sync_file(&self, path: &Path) -> Result<(), SyncError>;

    /// Synchronize a single file to a specific destination directory with a caller-provided scratch buffer.
    fn sync_file_to_dest_buffered(
        &self,
        path: &Path,
        dest_dir: &Path,
        scratch: &mut [u8],
    ) -> Result<(), SyncError>;

    /// Synchronize a single file to a destination directory, staging its database metadata update.
    fn sync_file_to_dest_staged(
        &self,
        path: &Path,
        dest_dir: &Path,
        scratch: &mut [u8],
    ) -> Result<(), SyncError> {
        self.sync_file_to_dest_buffered(path, dest_dir, scratch)
    }
}

/// Role trait for file deletions and destination cleanup.
pub trait FileDeleter: Send + Sync {
    /// Handle deletion of a file by archiving it on the default configured destination.
    fn delete_file(&self, path: &Path) -> Result<(), SyncError>;

    /// Handle deletion of a file on a specific destination directory.
    fn delete_file_from_dest(&self, path: &Path, dest_dir: &Path) -> Result<(), SyncError>;
}

/// Role trait for flushing staged file metadata and hash records.
pub trait BatchFlusher: Send + Sync {
    /// Flush all staged file records and hash metadata updates to the database in a single transaction.
    fn flush_staged_syncs(&self) -> Result<(), SyncError> {
        Ok(())
    }
}

/// Role trait for full directory scanning and synchronization.
pub trait ScanEngine: Send + Sync {
    /// Run a full scan on `dest_dir` that can be cancelled via an atomic token.
    fn run_cancellable_full_scan(
        &self,
        dest_dir: &Path,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> Result<ScanOutcome, SyncError>;

    /// Perform a full directory scan on `dest_dir` without cancellation.
    fn run_full_scan(&self, dest_dir: &Path) -> Result<ScanOutcome, SyncError> {
        static NEVER_CANCELLED: std::sync::atomic::AtomicBool =
            std::sync::atomic::AtomicBool::new(false);
        self.run_cancellable_full_scan(dest_dir, &NEVER_CANCELLED)
    }
}

/// Role trait for destination archive pruning and lifecycle maintenance.
pub trait ArchiveEngine: Send + Sync {
    /// Prune old and excess files in the destination's archive directory.
    fn prune_archive(&self, _dest_dir: &Path) -> Result<(), SyncError> {
        Ok(())
    }
}

/// Core sync execution contract composing the segregated role traits.
///
/// `SyncEngine` serves as the composite behavioral abstraction decoupling sync workers
/// and daemon orchestration from low-level filesystem I/O, hash database caching, and delta transfers.
///
/// Production implementations (like [`LocalSyncEngine`]) coordinate atomic small-file copies,
/// block-level delta transfers, path traversal safety checks, and destination archiving. Test
/// suites utilize [`MockSyncEngine`](crate::sync::MockSyncEngine) to verify worker state machines without live disk access.
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
pub trait SyncEngine:
    FileSynchronizer + FileDeleter + BatchFlusher + ScanEngine + ArchiveEngine
{
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
            && record.file_size() as i64 == self.size
            && record.last_modified() == self.modified_epoch_millis
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
            size: record.file_size() as i64,
            modified_epoch_millis: record.last_modified().max(0),
        }
    }
}

/// Raw metadata evaluation for testing and backward compatibility.
#[deprecated(
    since = "0.2.0",
    note = "use FileMetadataSnapshot::is_up_to_date instead"
)]
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
    range: DirtyBlockRange,
}

impl<'a> DirtyRangeLease<'a> {
    pub(crate) fn new(
        pool: &'a std::sync::Mutex<Option<DirtyBlockRange>>,
        range: DirtyBlockRange,
    ) -> Self {
        Self { pool, range }
    }
}

impl<'a> std::ops::Deref for DirtyRangeLease<'a> {
    type Target = DirtyBlockRange;

    fn deref(&self) -> &Self::Target {
        &self.range
    }
}

impl<'a> std::ops::DerefMut for DirtyRangeLease<'a> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.range
    }
}

impl<'a> Drop for DirtyRangeLease<'a> {
    fn drop(&mut self) {
        let mut range = std::mem::take(&mut self.range);
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

/// Delta sync engine backed by a `HashStore` for signature caching, composed of collaborating transfer, archive, and scan engines.
pub struct LocalSyncEngine<S: HashStore> {
    db: std::sync::Arc<S>,
    config: TargetSyncConfig,
    resolved_dest: Option<PathBuf>,
    reparse_cache: std::sync::Arc<crate::sync::path_safety::ReparseCache>,
    small_file_engine: crate::sync::small_file::SmallFileTransferEngine,
    delta_engine: crate::sync::delta::DeltaTransferEngine<std::sync::Arc<S>>,
    archive_manager: crate::sync::archive::ArchiveManager,
    scanner: crate::sync::scanner::DirectoryScanner,
    staged_records: std::sync::Mutex<Vec<(FileRecord, Vec<crate::db::BlockHash>)>>,
}

impl<S: HashStore> LocalSyncEngine<S> {
    /// Create a new sync engine with the given database and config.
    pub fn new(db: S, config: TargetSyncConfig) -> Self {
        let db = std::sync::Arc::new(db);
        let reparse_cache =
            std::sync::Arc::new(crate::sync::path_safety::ReparseCache::new(50_000, 10_000));
        let small_file_engine =
            crate::sync::small_file::SmallFileTransferEngine::new(config.clone());
        let delta_engine = crate::sync::delta::DeltaTransferEngine::new(
            std::sync::Arc::clone(&db),
            config.clone(),
        );
        let archive_manager = crate::sync::archive::ArchiveManager::new(
            config.clone(),
            std::sync::Arc::clone(&reparse_cache),
        );
        let scanner = crate::sync::scanner::DirectoryScanner::new(config.clone());
        Self {
            db,
            config,
            resolved_dest: None,
            reparse_cache,
            small_file_engine,
            delta_engine,
            archive_manager,
            scanner,
            staged_records: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// Return a shared handle to the engine's `ReparseCache`.
    pub fn reparse_cache(&self) -> std::sync::Arc<crate::sync::path_safety::ReparseCache> {
        std::sync::Arc::clone(&self.reparse_cache)
    }

    /// Invalidate any cached directory metadata (e.g. reparse point and ancestor junction checks).
    ///
    /// Clears the internal `reparse_cache` so that subsequent synchronizations re-verify
    /// the entire path tree against reparse point and symlink substitution attacks.
    pub fn invalidate_verified_dirs(&self) {
        self.reparse_cache.clear();
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
        self.reparse_cache.evict_dir(dir);
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

    pub(crate) fn db(&self) -> &S {
        &self.db
    }

    pub(crate) fn config(&self) -> &TargetSyncConfig {
        &self.config
    }

    pub(crate) fn scanner(&self) -> &crate::sync::scanner::DirectoryScanner {
        &self.scanner
    }

    /// Perform a full directory scan on default or pre-resolved destination.
    pub fn run_configured_full_scan(&self) -> Result<ScanOutcome, SyncError> {
        static NEVER_CANCELLED: std::sync::atomic::AtomicBool =
            std::sync::atomic::AtomicBool::new(false);
        let dest = self
            .resolved_dest
            .as_deref()
            .unwrap_or_else(|| self.config.dest_dir());
        self.run_cancellable_full_scan_impl(dest, &NEVER_CANCELLED)
    }

    /// Deprecated: use [`run_configured_full_scan`] instead.
    #[deprecated(note = "use run_configured_full_scan instead")]
    pub fn run_full_scan(&self) -> Result<ScanOutcome, SyncError> {
        self.run_configured_full_scan()
    }

    /// Synchronize a file or directory tree to a specific destination directory (primary or alternate).
    pub fn sync_file_to_dest(&self, rel_path: &Path, dest_dir: &Path) -> Result<(), SyncError> {
        let mut scratch = vec![0u8; self.config.block_size_bytes() as usize];
        self.sync_file_to_dest_buffered(rel_path, dest_dir, &mut scratch)
    }

    /// Synchronize a single file from source to the default configured destination.
    pub fn sync_file(&self, path: &Path) -> Result<(), SyncError> {
        let dest = self
            .resolved_dest
            .as_deref()
            .unwrap_or_else(|| self.config.dest_dir());
        self.sync_file_to_dest(path, dest)
    }

    /// Handle deletion of a file by archiving it on the default configured destination.
    pub fn delete_file(&self, path: &Path) -> Result<(), SyncError> {
        let dest = self
            .resolved_dest
            .as_deref()
            .unwrap_or_else(|| self.config.dest_dir());
        self.delete_file_from_dest(path, dest)
    }

    /// Run a full scan and synchronization cycle on `dest_dir` that can be cancelled via an atomic token.
    pub fn run_cancellable_full_scan(
        &self,
        dest_dir: &Path,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> Result<ScanOutcome, SyncError> {
        self.run_cancellable_full_scan_impl(dest_dir, cancel)
    }

    pub(crate) fn sync_file_core(
        &self,
        task: &FileSyncTask<'_>,
        scratch: &mut [u8],
    ) -> Result<(FileRecord, Vec<crate::db::BlockHash>), SyncError> {
        if task.src_size < self.config.block_sync_threshold_bytes() {
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
        crate::sync::path_safety::verify_destination_not_reparse_cached(
            dest_dir,
            rel_path,
            &self.reparse_cache,
        )
    }

    pub(crate) fn sync_file_to_dest_core(
        &self,
        rel_path: &Path,
        dest_dir: &Path,
        scratch: &mut [u8],
        file_record: Option<&FileRecord>,
    ) -> Result<Option<(FileRecord, Vec<crate::db::BlockHash>)>, SyncError> {
        let safe_rel = RelativePath::try_new(rel_path)?;

        let _span = tracing::info_span!(
            "sync_file",
            rel_path = ?rel_path,
            source = %self.config.source_dir().display(),
            destination = %dest_dir.display(),
        );
        let _guard = _span.enter();
        let src_path = self.config.source_dir().join(rel_path);
        let dest_path = dest_dir.join(rel_path);

        let sym_meta = fs::symlink_metadata(&src_path).map_err(SyncError::Io)?;
        if is_reparse_or_symlink_meta(&sym_meta) {
            tracing::debug!(path = %src_path.display(), "Skipping symlink or reparse point");
            return Ok(None);
        }
        crate::sync::path_safety::verify_source_not_reparse_cached(
            self.config.source_dir(),
            rel_path,
            &self.reparse_cache,
        )?;
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
                self.align_dest_file_casing_if_needed(dest_dir, rel_path)?;
                if let Some(record) = file_record
                    && record.is_tracked()
                    && record.file_size() == src_size as u64
                    && record.last_modified() == src_mod
                {
                    if record.relative_path() != &safe_rel {
                        let hashes = self.db.get_block_hashes(rel_path)?;
                        let updated_rec = FileRecord::from_raw(rel_path, src_size as u64, src_mod)?;
                        self.db.save_file(&updated_rec, &hashes)?;
                    }
                    tracing::debug!(path = %rel_path.display(), "Local signature cache hit and destination matches, skipping sync");
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

        let cached_id = file_record.and_then(|r| r.id());
        let task = FileSyncTask {
            rel_path: &safe_rel,
            src_path: &src_path,
            dest_path: &dest_path,
            dest_dir,
            src_size: sym_meta.len(),
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

    /// Synchronize a file using a reusable scratch buffer and stage its metadata update for batch persistence.
    pub fn sync_file_to_dest_staged(
        &self,
        rel_path: &Path,
        dest_dir: &Path,
        scratch: &mut [u8],
    ) -> Result<(), SyncError> {
        let file_record = self.db.get_file(rel_path)?;
        if let Some((record, hashes)) =
            self.sync_file_to_dest_core(rel_path, dest_dir, scratch, file_record.as_ref())?
        {
            let mut staged = self
                .staged_records
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            staged.push((record, hashes));
        }
        Ok(())
    }

    /// Flush all staged file records and hash metadata updates to the database in a single batch.
    pub fn flush_staged_syncs(&self) -> Result<(), SyncError> {
        let mut staged = self
            .staged_records
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.flush_record_batch(&mut staged)
    }

    /// Flush accumulated file records and hashes to the database in a single batch.
    pub(crate) fn flush_record_batch(
        &self,
        batch: &mut Vec<(FileRecord, Vec<crate::db::BlockHash>)>,
    ) -> Result<(), SyncError> {
        if batch.is_empty() {
            return Ok(());
        }
        let refs: Vec<(&FileRecord, &[crate::db::BlockHash])> = batch
            .iter()
            .map(|(rec, hashes)| (rec, hashes.as_slice()))
            .collect();
        if let Err(e) = self.db.save_files_batch(&refs) {
            tracing::warn!(error = %e, "Batch save failed; falling back to individual record saves");
            for (rec, hashes) in batch.iter() {
                self.db.save_file(rec, hashes)?;
            }
        }
        batch.clear();
        Ok(())
    }

    /// Full scan implementation with cooperative cancellation support.
    #[tracing::instrument(
        name = "full_scan",
        skip(self, cancel),
        fields(
            source = %self.config.source_dir().display(),
            destination = %dest_dir.display()
        ),
        level = "info"
    )]
    pub(crate) fn run_cancellable_full_scan_impl(
        &self,
        dest_dir: &Path,
        cancel: &AtomicBool,
    ) -> Result<ScanOutcome, SyncError> {
        crate::sync::FullScanCoordinator::new(self, dest_dir, cancel).run()
    }

    /// Archive or remove a file on destination filesystem without updating the database.
    pub(crate) fn archive_dest_file_only(
        &self,
        rel_path: &Path,
        dest_dir: &Path,
    ) -> Result<(), SyncError> {
        if !is_safe_relative_path(rel_path) {
            return Err(SyncError::validation_security(format!(
                "Unsafe path traversal detected: {:?}",
                rel_path
            )));
        }
        let _span = tracing::info_span!(
            "archive_file",
            rel_path = ?rel_path,
            destination = %dest_dir.display(),
        );
        let _guard = _span.enter();
        self.archive_manager
            .archive_dest_file_only(rel_path, dest_dir)
    }

    /// Handle deletion of a file on a specific destination directory.
    pub fn delete_file_from_dest(&self, rel_path: &Path, dest_dir: &Path) -> Result<(), SyncError> {
        self.archive_dest_file_only(rel_path, dest_dir)?;
        let dest_path = dest_dir.join(rel_path);
        if let Some(parent) = dest_path.parent() {
            self.evict_verified_dir(parent);
        }
        let source_path = self.config.source_dir().join(rel_path);
        if let Some(parent) = source_path.parent() {
            self.evict_verified_dir(parent);
        }
        if self.config.propagate_deletions() || !dest_path.exists() {
            self.db.delete_file(rel_path)?;
        }
        Ok(())
    }

    /// Prune old and excess files in the destination archive.
    pub fn prune_destination_archive(&self, dest_dir: &Path) -> Result<(), SyncError> {
        self.archive_manager.prune_destination_archive(dest_dir)
    }

    #[cfg(windows)]
    pub(crate) fn align_dest_file_casing_if_needed(
        &self,
        dest_dir: &Path,
        rel_path: &Path,
    ) -> Result<(), SyncError> {
        let dest_path = dest_dir.join(rel_path);
        let Some(expected_name) = rel_path.file_name() else {
            return Ok(());
        };
        let Some(parent) = dest_path.parent() else {
            return Ok(());
        };
        if !parent.exists() {
            return Ok(());
        }

        let mut needs_rename = false;
        if let Ok(entries) = std::fs::read_dir(parent) {
            let expected_str = expected_name.to_string_lossy();
            for entry in entries.flatten() {
                let name_str = entry.file_name().to_string_lossy().into_owned();
                if name_str.eq_ignore_ascii_case(&expected_str) {
                    if name_str != expected_str {
                        needs_rename = true;
                    }
                    break;
                }
            }
        }

        if needs_rename {
            let temp_name = format!(
                "{}.syncdir_casetmp_{}",
                dest_path.display(),
                std::process::id()
            );
            let temp_path = std::path::PathBuf::from(temp_name);
            if temp_path.exists() {
                let _ = std::fs::remove_file(&temp_path);
            }
            std::fs::rename(&dest_path, &temp_path)?;
            std::fs::rename(&temp_path, &dest_path)?;
        }
        Ok(())
    }

    #[cfg(not(windows))]
    #[inline]
    pub(crate) fn align_dest_file_casing_if_needed(
        &self,
        _dest_dir: &Path,
        _rel_path: &Path,
    ) -> Result<(), SyncError> {
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn sync_delta_large_file_core(
        &self,
        task: &FileSyncTask<'_>,
        scratch: &mut [u8],
    ) -> Result<(FileRecord, Vec<crate::db::BlockHash>), SyncError> {
        self.delta_engine.sync_delta_large_file_core(task, scratch)
    }

    #[cfg(test)]
    pub(crate) fn sync_delta_large_file(
        &self,
        task: &FileSyncTask<'_>,
        scratch: &mut [u8],
    ) -> Result<(), SyncError> {
        let (record, hashes) = self.sync_delta_large_file_core(task, scratch)?;
        self.db.save_file(&record, &hashes)?;
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn sync_small_file_core(
        &self,
        task: &FileSyncTask<'_>,
        scratch: &mut [u8],
    ) -> Result<(FileRecord, Vec<crate::db::BlockHash>), SyncError> {
        self.small_file_engine.sync_small_file_core(task, scratch)
    }

    #[cfg(test)]
    pub(crate) fn sync_small_file(&self, task: &FileSyncTask<'_>) -> Result<(), SyncError> {
        let mut stack_scratch = [0u8; 64 * 1024];
        let (record, _) = self.sync_small_file_core(task, &mut stack_scratch)?;
        self.db.save_file(&record, &[])?;
        Ok(())
    }
}

#[inline]
pub(crate) fn calculate_remaining_files(total: usize, synced: usize, failed: usize) -> usize {
    total.saturating_sub(synced.saturating_add(failed))
}

impl<S: HashStore> FileSynchronizer for LocalSyncEngine<S> {
    fn sync_file(&self, path: &Path) -> Result<(), SyncError> {
        self.sync_file(path)
    }

    fn sync_file_to_dest_buffered(
        &self,
        path: &Path,
        dest_dir: &Path,
        scratch: &mut [u8],
    ) -> Result<(), SyncError> {
        self.sync_file_to_dest_buffered(path, dest_dir, scratch)
    }

    fn sync_file_to_dest_staged(
        &self,
        path: &Path,
        dest_dir: &Path,
        scratch: &mut [u8],
    ) -> Result<(), SyncError> {
        self.sync_file_to_dest_staged(path, dest_dir, scratch)
    }
}

impl<S: HashStore> FileDeleter for LocalSyncEngine<S> {
    fn delete_file(&self, path: &Path) -> Result<(), SyncError> {
        self.delete_file(path)
    }

    fn delete_file_from_dest(&self, path: &Path, dest_dir: &Path) -> Result<(), SyncError> {
        self.delete_file_from_dest(path, dest_dir)
    }
}

impl<S: HashStore> BatchFlusher for LocalSyncEngine<S> {
    fn flush_staged_syncs(&self) -> Result<(), SyncError> {
        self.flush_staged_syncs()
    }
}

impl<S: HashStore> ScanEngine for LocalSyncEngine<S> {
    fn run_cancellable_full_scan(
        &self,
        dest_dir: &Path,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> Result<ScanOutcome, SyncError> {
        self.run_cancellable_full_scan_impl(dest_dir, cancel)
    }

    fn run_full_scan(&self, dest_dir: &Path) -> Result<ScanOutcome, SyncError> {
        static NEVER_CANCELLED: std::sync::atomic::AtomicBool =
            std::sync::atomic::AtomicBool::new(false);
        self.run_cancellable_full_scan(dest_dir, &NEVER_CANCELLED)
    }
}

impl<S: HashStore> ArchiveEngine for LocalSyncEngine<S> {
    fn prune_archive(&self, dest_dir: &Path) -> Result<(), SyncError> {
        self.prune_destination_archive(dest_dir)
    }
}

impl<S: HashStore> SyncEngine for LocalSyncEngine<S> {
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

    fn test_config(source: PathBuf, dest: PathBuf) -> Config {
        Config::builder(source)
            .dest_dir(dest)
            .debounce_seconds(1)
            .retry_interval_seconds(1)
            .propagate_deletions(true)
            .block_sync_threshold_bytes(64)
            .block_size_bytes(16)
            .build()
            .unwrap()
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
    #[allow(deprecated)]
    fn test_is_metadata_up_to_date_raw() {
        let record = crate::db::FileRecord::from_raw("file.txt", 100, 10_000)
            .unwrap()
            .with_id(1);
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
        let rel = RelativePath::try_new("file.bin").unwrap();
        let task = FileSyncTask {
            rel_path: &rel,
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

        let outcome = engine.run_configured_full_scan().unwrap();
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

        let record = FileRecord::from_raw(file_rel, src_size as u64, src_mod)
            .unwrap()
            .with_id(42);

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

        let record = FileRecord::from_raw(file_rel, src_size as u64, src_mod)
            .unwrap()
            .with_id(99);
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
            let cache = engine.reparse_cache();
            assert!(cache.contains(&dst));
            assert!(cache.contains(&sub));
            assert!(cache.contains(&nested));
        }

        // Test prefix eviction: evicting sub must remove sub and sub/nested, keeping dst
        engine.evict_verified_dir(&sub);
        {
            let cache = engine.reparse_cache();
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
            let cache = engine.reparse_cache();
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
        let rec = crate::db::FileRecord::from_raw("rec.txt", 1024, 1_700_000_000_000)
            .unwrap()
            .with_id(42);
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
        let scan_res = engine.run_configured_full_scan().unwrap();
        assert!(matches!(scan_res, ScanOutcome::Success { .. }));
    }

    // =========================================================================
    // Relocated Small File Tests (5 tests)
    // =========================================================================

    #[test]
    fn test_small_file_sync() {
        let dir = tempdir().unwrap();
        let source = dir.path().join("src");
        let dest = dir.path().join("dst");
        let db_path = dir.path().join("sig.db");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&dest).unwrap();

        let config = test_config(source.clone(), dest.clone());
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

        fs::write(source.join("small.txt"), b"hello world").unwrap();
        engine.sync_file(Path::new("small.txt")).unwrap();

        assert_eq!(fs::read(dest.join("small.txt")).unwrap(), b"hello world");
    }

    #[test]
    fn test_zero_byte_file_sync() {
        let dir = tempdir().unwrap();
        let source = dir.path().join("src");
        let dest = dir.path().join("dst");
        let db_path = dir.path().join("sig.db");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&dest).unwrap();

        let config = test_config(source.clone(), dest.clone());
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

        fs::write(source.join("empty.txt"), b"").unwrap();
        engine.sync_file(Path::new("empty.txt")).unwrap();

        assert!(dest.join("empty.txt").exists());
        assert_eq!(fs::read(dest.join("empty.txt")).unwrap(), b"");
    }

    #[test]
    fn test_sync_small_file_skips_block_hashes() {
        let dir = tempdir().unwrap();
        let source = dir.path().join("src");
        let dest = dir.path().join("dst");
        let db_path = dir.path().join("sig.db");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&dest).unwrap();

        let config = Config::builder(source.clone())
            .dest_dir(dest.clone())
            .block_sync_threshold_bytes(1024)
            .block_size_bytes(256)
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

        fs::write(source.join("small.txt"), b"under threshold").unwrap();
        engine.sync_file(Path::new("small.txt")).unwrap();

        assert_eq!(
            fs::read(dest.join("small.txt")).unwrap(),
            b"under threshold"
        );
        let block_hashes = engine.db.get_block_hashes(Path::new("small.txt")).unwrap();
        assert!(
            block_hashes.is_empty(),
            "Small files should not record block hashes"
        );
    }

    #[test]
    fn test_sync_file_small_file_verify_writes() {
        let temp = tempdir().unwrap();
        let src = temp.path().join("source");
        let dst = temp.path().join("dest");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&dst).unwrap();
        let config = Config::test_default(src.clone(), dst.clone());
        let target_cfg = TargetSyncConfig::from_config(&config, dst.clone()).unwrap();
        let engine =
            LocalSyncEngine::new(MockHashStore::new(), target_cfg.with_verify_writes(true));
        std::fs::write(src.join("small.txt"), b"payload").unwrap();
        let mut scratch = vec![0u8; 4096];
        assert!(
            engine
                .sync_file_to_dest_buffered(Path::new("small.txt"), &dst, &mut scratch)
                .is_ok()
        );
        assert_eq!(std::fs::read(dst.join("small.txt")).unwrap(), b"payload");
    }

    #[test]
    fn test_sync_small_file_records_actual_bytes_copied() {
        let temp = tempdir().unwrap();
        let src = temp.path().join("source");
        let dst = temp.path().join("dest");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&dst).unwrap();
        let config = Config::builder(src.clone())
            .dest_dir(dst.clone())
            .block_size_bytes(512)
            .verify_writes(false)
            .build()
            .unwrap();
        let target_cfg = TargetSyncConfig::from_config(&config, dst.clone()).unwrap();
        let store = MockHashStore::new();
        let engine = LocalSyncEngine::new(store.clone(), target_cfg);

        let src_file = src.join("small.txt");
        let dst_file = dst.join("small.txt");
        std::fs::write(&src_file, vec![0x42; 500]).unwrap();

        let src_mod = safe_modified_millis(&std::fs::metadata(&src_file).unwrap()).unwrap();
        let rel = RelativePath::try_new("small.txt").unwrap();
        let task = FileSyncTask {
            rel_path: &rel,
            src_path: &src_file,
            dest_path: &dst_file,
            dest_dir: &dst,
            src_size: 1000,
            src_mod,
            cached_id: None,
        };

        engine.sync_small_file(&task).unwrap();
        let record = store.get_file(Path::new("small.txt")).unwrap().unwrap();
        assert_eq!(
            record.file_size(),
            500,
            "Saved record must use actual bytes copied (500), not stale task.src_size (1000)"
        );
    }

    // =========================================================================
    // Relocated Delta Tests (3 tests)
    // =========================================================================

    #[test]
    fn test_exact_block_multiple_sync() {
        let dir = tempdir().unwrap();
        let source = dir.path().join("src");
        let dest = dir.path().join("dst");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&dest).unwrap();

        let config = Config::builder(source.clone())
            .dest_dir(dest.clone())
            .block_sync_threshold_bytes(4)
            .block_size_bytes(4)
            .build()
            .unwrap();
        let target_cfg = TargetSyncConfig::try_from_config(&config).unwrap();
        let store = MockHashStore::new();
        let engine = LocalSyncEngine::new(store, target_cfg);

        // 8 bytes payload = exactly 2 blocks of 4 bytes
        fs::write(source.join("exact.bin"), b"12345678").unwrap();
        engine.sync_file(Path::new("exact.bin")).unwrap();

        assert_eq!(fs::read(dest.join("exact.bin")).unwrap(), b"12345678");
        let hashes = engine.db.get_block_hashes(Path::new("exact.bin")).unwrap();
        assert_eq!(hashes.len(), 2);
    }

    #[test]
    fn test_delta_sync_large_file() {
        let dir = tempdir().unwrap();
        let source = dir.path().join("src");
        let dest = dir.path().join("dst");
        let db_path = dir.path().join("sig.db");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&dest).unwrap();

        let config = Config::builder(source.clone())
            .dest_dir(dest.clone())
            .block_sync_threshold_bytes(10)
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

        // 12 bytes > 10 byte threshold -> delta sync path (3 blocks of 4)
        fs::write(source.join("big.bin"), b"AAAABBBBcccc").unwrap();
        engine.sync_file(Path::new("big.bin")).unwrap();

        let synced = fs::read(dest.join("big.bin")).unwrap();
        assert_eq!(synced, b"AAAABBBBcccc");

        // Modify only block 1 (bytes 4-7)
        let big_bin_path = source.join("big.bin");
        fs::write(&big_bin_path, b"AAAAZZZZCCCC").unwrap();
        let f = OpenOptions::new().write(true).open(&big_bin_path).unwrap();
        f.set_times(
            fs::FileTimes::new()
                .set_modified(SystemTime::now() + std::time::Duration::from_secs(5)),
        )
        .unwrap();

        engine.sync_file(Path::new("big.bin")).unwrap();

        let synced = fs::read(dest.join("big.bin")).unwrap();
        assert_eq!(synced, b"AAAAZZZZCCCC");

        let hashes = engine.db.get_block_hashes(Path::new("big.bin")).unwrap();
        assert_eq!(hashes.len(), 3);
    }

    #[test]
    fn test_local_sync_engine_dirty_range_buffer_reuse() {
        let temp = tempdir().unwrap();
        let src = temp.path().join("source");
        let dst = temp.path().join("dest");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&dst).unwrap();
        let config = Config::builder(src.clone())
            .dest_dir(dst.clone())
            .block_size_bytes(512)
            .build()
            .unwrap();
        let target_cfg = TargetSyncConfig::from_config(&config, dst.clone()).unwrap();
        let engine = LocalSyncEngine::new(MockHashStore::new(), target_cfg);

        // Sync file 1
        let f1 = "file1.bin";
        std::fs::write(src.join(f1), vec![0x11; 1024]).unwrap();
        let src_meta1 = std::fs::metadata(src.join(f1)).unwrap();
        let rel1 = RelativePath::try_new(f1).unwrap();
        let task1 = FileSyncTask {
            rel_path: &rel1,
            src_path: &src.join(f1),
            dest_path: &dst.join(f1),
            dest_dir: &dst,
            src_size: src_meta1.len(),
            src_mod: safe_modified_millis(&src_meta1).unwrap(),
            cached_id: None,
        };
        let mut scratch = vec![0u8; 512];
        engine.sync_delta_large_file(&task1, &mut scratch).unwrap();

        let cap1 = engine.dirty_range_capacity();
        assert!(
            cap1 >= 512,
            "dirty_range buffer must have allocated capacity"
        );

        // Sync file 2
        let f2 = "file2.bin";
        std::fs::write(src.join(f2), vec![0x22; 1024]).unwrap();
        let src_meta2 = std::fs::metadata(src.join(f2)).unwrap();
        let rel2 = RelativePath::try_new(f2).unwrap();
        let task2 = FileSyncTask {
            rel_path: &rel2,
            src_path: &src.join(f2),
            dest_path: &dst.join(f2),
            dest_dir: &dst,
            src_size: src_meta2.len(),
            src_mod: safe_modified_millis(&src_meta2).unwrap(),
            cached_id: None,
        };
        engine.sync_delta_large_file(&task2, &mut scratch).unwrap();

        let cap2 = engine.dirty_range_capacity();
        assert!(
            cap2 >= cap1,
            "dirty_range capacity should be retained or grown across files"
        );
    }

    // =========================================================================
    // Relocated Archive Tests (3 tests)
    // =========================================================================

    #[test]
    fn test_deletion_archive() {
        let dir = tempdir().unwrap();
        let source = dir.path().join("src");
        let dest = dir.path().join("dst");
        let db_path = dir.path().join("sig.db");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&dest).unwrap();

        let config = test_config(source.clone(), dest.clone());
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

        fs::write(source.join("doomed.txt"), b"bye").unwrap();
        engine.sync_file(Path::new("doomed.txt")).unwrap();
        assert!(dest.join("doomed.txt").exists());

        engine.delete_file(Path::new("doomed.txt")).unwrap();
        assert!(!dest.join("doomed.txt").exists());

        let archive = dest.join(".syncdir_archive");
        assert!(archive.exists());
        let entries: Vec<_> = fs::read_dir(&archive)
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();
        assert_eq!(entries.len(), 1);
        let archived_name = entries[0].file_name().to_string_lossy().to_string();
        assert!(archived_name.ends_with("_doomed.txt"));

        assert!(
            engine
                .db
                .get_file(Path::new("doomed.txt"))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn test_nested_directory_deletion_archive() {
        let dir = tempdir().unwrap();
        let source = dir.path().join("src");
        let dest = dir.path().join("dst");
        let db_path = dir.path().join("sig.db");
        fs::create_dir_all(source.join("subdir")).unwrap();
        fs::create_dir_all(&dest).unwrap();

        let config = Config::test_default(source.clone(), dest.clone());
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

        fs::write(source.join("subdir").join("deep.txt"), b"nested content").unwrap();
        engine.sync_file(Path::new("subdir/deep.txt")).unwrap();
        assert!(dest.join("subdir").join("deep.txt").exists());

        engine.delete_file(Path::new("subdir/deep.txt")).unwrap();
        assert!(!dest.join("subdir").join("deep.txt").exists());

        let archive = dest.join(".syncdir_archive");
        assert!(archive.exists());
        let entries: Vec<_> = fs::read_dir(&archive)
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();
        assert_eq!(entries.len(), 1);
        let archived_entry = &entries[0];
        let nested = archived_entry.path().join("deep.txt");
        assert!(
            nested.exists(),
            "Archived nested file should preserve directory structure"
        );
    }

    #[test]
    fn test_conditional_db_deletion_on_dest_state() {
        let temp = tempdir().unwrap();
        let src = temp.path().join("source");
        let dst = temp.path().join("dest");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&dst).unwrap();
        let config = Config::builder(src.clone())
            .dest_dir(dst.clone())
            .propagate_deletions(true)
            .build()
            .unwrap();
        let target_cfg = TargetSyncConfig::from_config(&config, dst.clone()).unwrap();
        let store = MockHashStore::new();
        let engine = LocalSyncEngine::new(store.clone(), target_cfg);

        // Case 1: Destination file exists but has an exclusive lock
        let locked_file = dst.join("locked.txt");
        std::fs::write(&locked_file, "secret").unwrap();
        let rec1 = FileRecord::from_raw("locked.txt", 6, 100).unwrap();
        store.save_file(&rec1, &[]).unwrap();

        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            let _exclusive_handle = std::fs::OpenOptions::new()
                .read(true)
                .share_mode(0)
                .open(&locked_file)
                .unwrap();

            let res = engine.delete_file_from_dest(Path::new("locked.txt"), &dst);
            assert!(
                res.is_err(),
                "delete_file_from_dest must fail on locked/inaccessible destination"
            );
            assert!(
                store.get_file(Path::new("locked.txt")).unwrap().is_some(),
                "DB record must be retained when destination is locked/inaccessible"
            );
        }

        // Case 2: Destination file is genuinely absent (NotFound)
        let rec2 = FileRecord::from_raw("absent.txt", 10, 200).unwrap();
        store.save_file(&rec2, &[]).unwrap();
        assert!(store.get_file(Path::new("absent.txt")).unwrap().is_some());

        let res = engine.delete_file_from_dest(Path::new("absent.txt"), &dst);
        assert!(
            res.is_ok(),
            "delete_file_from_dest must succeed when file is NotFound"
        );
        assert!(
            store.get_file(Path::new("absent.txt")).unwrap().is_none(),
            "DB record must be deleted when destination file is confirmed NotFound"
        );

        // Case 3: Destination file exists with propagate_deletions = false
        let unprop_file = dst.join("unprop.txt");
        std::fs::write(&unprop_file, "data").unwrap();
        let config_no_prop = Config::builder(src)
            .dest_dir(dst.clone())
            .propagate_deletions(false)
            .build()
            .unwrap();
        let target_cfg_no_prop =
            TargetSyncConfig::from_config(&config_no_prop, dst.clone()).unwrap();
        let engine_no_prop = LocalSyncEngine::new(store.clone(), target_cfg_no_prop);
        let rec3 = FileRecord::from_raw("unprop.txt", 4, 300).unwrap();
        store.save_file(&rec3, &[]).unwrap();
        assert!(store.get_file(Path::new("unprop.txt")).unwrap().is_some());

        let res = engine_no_prop.delete_file_from_dest(Path::new("unprop.txt"), &dst);
        assert!(res.is_ok());
        assert!(
            store.get_file(Path::new("unprop.txt")).unwrap().is_some(),
            "DB record must be retained when propagate_deletions = false and destination file still exists"
        );
        assert!(unprop_file.exists());
    }

    // =========================================================================
    // Relocated Scanner Tests (9 tests)
    // =========================================================================

    #[test]
    fn test_full_scan_continues_past_file_errors() {
        let dir = tempdir().unwrap();
        let source = dir.path().join("src");
        let dest = dir.path().join("dst");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&dest).unwrap();

        fs::write(source.join("good.txt"), b"good content").unwrap();

        fs::create_dir_all(source.join("bad")).unwrap();
        fs::write(source.join("bad").join("nested.txt"), b"bad content").unwrap();

        fs::write(dest.join("bad"), b"blocking file").unwrap();

        let config = Config::test_default(source.clone(), dest.clone());
        let target_cfg = TargetSyncConfig::try_from_config(&config).unwrap();
        let store = MockHashStore::new();
        let engine = LocalSyncEngine::new(store, target_cfg);

        assert!(matches!(
            engine.run_configured_full_scan().unwrap(),
            ScanOutcome::PartialFailure {
                synced: 1,
                failed: 1,
                delete_failed: 0,
            }
        ));

        assert!(dest.join("good.txt").exists());
        assert_eq!(
            fs::read_to_string(dest.join("good.txt")).unwrap(),
            "good content"
        );
        assert!(!dest.join("bad").join("nested.txt").exists());
    }

    #[test]
    fn test_full_scan_all_skipped_returns_false() {
        let dir = tempdir().unwrap();
        let source = dir.path().join("src");
        let dest = dir.path().join("dst");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&dest).unwrap();

        fs::create_dir_all(source.join("bad")).unwrap();
        fs::write(source.join("bad").join("nested.txt"), b"bad content").unwrap();

        fs::write(dest.join("bad"), b"blocking file").unwrap();

        let config = Config::test_default(source.clone(), dest.clone());
        let target_cfg = TargetSyncConfig::try_from_config(&config).unwrap();
        let store = MockHashStore::new();
        let engine = LocalSyncEngine::new(store, target_cfg);

        assert_eq!(
            engine.run_configured_full_scan().unwrap(),
            ScanOutcome::DestinationUnreachable
        );
    }

    #[test]
    fn test_full_scan_dest_missing_skips_early() {
        let dir = tempdir().unwrap();
        let source = dir.path().join("src");
        let dest = dir.path().join("nonexistent_dest_dir");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("file1.txt"), b"content").unwrap();

        let config = Config::test_default(source.clone(), dest.clone());
        let target_cfg = TargetSyncConfig::try_from_config(&config).unwrap();
        let store = MockHashStore::new();
        let engine = LocalSyncEngine::new(store, target_cfg);

        assert_eq!(
            engine.run_configured_full_scan().unwrap(),
            ScanOutcome::DestinationUnreachable
        );
    }

    #[test]
    fn test_run_cancellable_full_scan_interruption() {
        let temp = tempdir().unwrap();
        let src = temp.path().join("src");
        let dst = temp.path().join("dst");
        fs::create_dir_all(&src).unwrap();
        fs::create_dir_all(&dst).unwrap();

        for i in 0..20 {
            fs::write(
                src.join(format!("file_{}.txt", i)),
                format!("content {}", i),
            )
            .unwrap();
        }

        let config = Config::test_default(src, dst.clone());
        let target_cfg = TargetSyncConfig::try_from_config(&config).unwrap();
        let engine = LocalSyncEngine::new(MockHashStore::new(), target_cfg);

        let cancel_token = AtomicBool::new(true);
        let result = engine.run_cancellable_full_scan(&dst, &cancel_token);

        assert!(
            result.is_err(),
            "Full scan should abort when cancel token is set"
        );
        match result.unwrap_err() {
            SyncError::Cancelled => {}
            other => panic!("Expected SyncError::Cancelled, got: {:?}", other),
        }
    }

    #[test]
    fn test_empty_source_safety_threshold() {
        let dir = tempdir().unwrap();
        let source = dir.path().join("src");
        let dest = dir.path().join("dst");
        let db_path = dir.path().join("sig.db");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&dest).unwrap();

        let config = Config::builder(source.clone())
            .dest_dir(dest.clone())
            .debounce_seconds(1)
            .retry_interval_seconds(1)
            .propagate_deletions(true)
            .block_sync_threshold_bytes(10)
            .block_size_bytes(4)
            .build()
            .unwrap();
        let store = crate::db::SqliteHashStore::new(
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

        fs::write(source.join("important.txt"), b"save me").unwrap();
        engine.run_configured_full_scan().unwrap();
        assert!(dest.join("important.txt").exists());

        fs::remove_file(source.join("important.txt")).unwrap();
        engine.run_configured_full_scan().unwrap();

        assert!(dest.join("important.txt").exists());
    }

    #[test]
    fn test_run_full_scan_uses_save_files_batch() {
        let temp = tempdir().unwrap();
        let src = temp.path().join("source");
        let dst = temp.path().join("dest");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&dst).unwrap();

        for i in 0..5 {
            std::fs::write(src.join(format!("file_{i}.txt")), format!("content {i}")).unwrap();
        }

        let store = MockHashStore::new();
        let config = Config::builder(src).dest_dir(dst).build().unwrap();
        let target_cfg = TargetSyncConfig::try_from_config(&config).unwrap();
        let engine = LocalSyncEngine::new(store.clone(), target_cfg);

        let outcome = engine.run_configured_full_scan().unwrap();
        assert!(matches!(outcome, ScanOutcome::Success { synced: 5 }));

        assert_eq!(
            store.save_file_count(),
            0,
            "Full scan must not call save_file individually"
        );
        assert!(
            store.batch_save_count() >= 1,
            "Full scan must call save_files_batch"
        );
    }

    #[test]
    fn test_run_full_scan_case_insensitive_deletions() {
        let temp = tempdir().unwrap();
        let src = temp.path().join("source");
        let dst = temp.path().join("dest");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&dst).unwrap();

        std::fs::write(src.join("readme.txt"), b"hello").unwrap();
        std::fs::write(dst.join("README.TXT"), b"hello").unwrap();

        let db = MockHashStore::new();
        let old_record = FileRecord::from_raw("README.TXT", 5, 1000)
            .unwrap()
            .with_id(1);
        db.save_file(&old_record, &[]).unwrap();

        let config = Config::builder(src.clone())
            .dest_dir(dst.clone())
            .propagate_deletions(true)
            .build()
            .unwrap();
        let target_cfg = TargetSyncConfig::from_config(&config, dst.clone()).unwrap();
        let engine = LocalSyncEngine::new(db, target_cfg);

        let outcome = engine.run_configured_full_scan().unwrap();
        assert_eq!(outcome, ScanOutcome::Success { synced: 1 });
        assert!(!dst.join(".syncdir_archive").exists());
    }

    #[test]
    fn test_full_scan_path_separator_normalization() {
        let temp = tempdir().unwrap();
        let src = temp.path().join("src");
        let dst = temp.path().join("dst");
        let nested_dir = src.join("nested");
        fs::create_dir_all(&nested_dir).unwrap();
        fs::create_dir_all(&dst).unwrap();
        let test_file = nested_dir.join("file.txt");
        fs::write(&test_file, b"content").unwrap();

        let config = Config::test_default(src.clone(), dst.clone());
        let store = MockHashStore::new();
        let rec = FileRecord::from_raw(
            "nested/file.txt",
            7,
            safe_modified_millis(&fs::metadata(&test_file).unwrap()).unwrap(),
        )
        .unwrap()
        .with_id(1);
        store.save_file(&rec, &[]).unwrap();

        let target_cfg = TargetSyncConfig::try_from_config(&config).unwrap();
        let engine = LocalSyncEngine::new(store, target_cfg);
        let outcome = engine.run_configured_full_scan().unwrap();
        assert_eq!(outcome, ScanOutcome::Success { synced: 1 });
        assert!(dst.join("nested").join("file.txt").exists());
    }

    #[test]
    fn test_full_scan_db_error_propagated() {
        let temp = tempdir().unwrap();
        let src = temp.path().join("src");
        let dst = temp.path().join("dst");
        fs::create_dir_all(&src).unwrap();
        fs::create_dir_all(&dst).unwrap();
        let config = Config::test_default(src, dst);
        let store = MockHashStore::new();
        store.set_error_hook(Some(Box::new(|op| {
            if op == "list_all_records" {
                Some(SyncError::db("Forced list_all_records failure"))
            } else {
                None
            }
        })));
        let target_cfg = TargetSyncConfig::try_from_config(&config).unwrap();
        let engine = LocalSyncEngine::new(store, target_cfg);
        let result = engine.run_configured_full_scan();
        assert!(matches!(result, Err(SyncError::Db(..))));
    }

    #[test]
    fn test_run_cancellable_full_scan_arithmetic_underflow_protection() {
        assert_eq!(calculate_remaining_files(0, 0, 1), 0);
        assert_eq!(calculate_remaining_files(1, 1, 1), 0);
        assert_eq!(calculate_remaining_files(5, 2, 1), 2);
        assert_eq!(calculate_remaining_files(usize::MAX, usize::MAX, 1), 0);
    }

    #[test]
    fn test_dirty_range_lease_panic_free_deref_and_deref_mut() {
        let pool = std::sync::Mutex::new(None);
        let range = DirtyBlockRange::new(std::num::NonZeroU64::new(1024).unwrap());
        let mut lease = DirtyRangeLease::new(&pool, range);
        assert_eq!(lease.capacity(), 0);
        assert!(lease.is_empty());
        assert_eq!(lease.block_size(), 1024);
        let mut cursor = std::io::Cursor::new(Vec::new());
        lease.add_block(0, &[1u8; 10], &mut cursor).unwrap();
        assert_eq!(lease.byte_len(), 10);
        assert!(!lease.is_empty());
        drop(lease);
        let pooled = pool.lock().unwrap();
        assert!(pooled.is_some());
    }

    #[test]
    fn test_local_sync_engine_reparse_cache_shared_across_components() {
        let dir = tempdir().unwrap();
        let src = dir.path().join("src");
        let dst = dir.path().join("dst");
        fs::create_dir_all(&src).unwrap();
        fs::create_dir_all(&dst).unwrap();
        let config = Config::builder(src)
            .dest_dir(dst.clone())
            .build_unvalidated();
        let target_cfg = TargetSyncConfig::try_from_config(&config).unwrap();
        let engine = LocalSyncEngine::new(MockHashStore::new(), target_cfg);

        let cache = engine.reparse_cache();
        assert_eq!(cache.len(), 0);
        let _ = engine.verify_destination_cached(&dst, Path::new("sub/file.txt"));
        assert!(cache.contains(&dst));
    }

    #[test]
    fn test_sync_engine_staged_sync_and_flush_trait_methods() {
        let dir = tempdir().unwrap();
        let src = dir.path().join("src");
        let dst = dir.path().join("dst");
        fs::create_dir_all(&src).unwrap();
        fs::create_dir_all(&dst).unwrap();
        fs::write(src.join("f1.txt"), b"hello 1").unwrap();
        fs::write(src.join("f2.txt"), b"hello 2").unwrap();

        let config = Config::builder(src)
            .dest_dir(dst.clone())
            .build_unvalidated();
        let target_cfg = TargetSyncConfig::try_from_config(&config).unwrap();
        let store = MockHashStore::new();
        let engine = LocalSyncEngine::new(store.clone(), target_cfg);

        let mut scratch = vec![0u8; 64 * 1024];
        engine
            .sync_file_to_dest_staged(Path::new("f1.txt"), &dst, &mut scratch)
            .unwrap();
        engine
            .sync_file_to_dest_staged(Path::new("f2.txt"), &dst, &mut scratch)
            .unwrap();

        // Not yet committed to DB
        assert_eq!(store.save_file_count(), 0);
        assert_eq!(store.batch_save_count(), 0);

        // Flush commits both records in 1 batch save call
        engine.flush_staged_syncs().unwrap();
        assert_eq!(store.batch_save_count(), 1);
        assert_eq!(store.save_file_count(), 0);
    }

    #[test]
    fn test_local_sync_engine_staged_sync_falls_back_to_individual_save_on_batch_error() {
        let dir = tempdir().unwrap();
        let src = dir.path().join("src");
        let dst = dir.path().join("dst");
        fs::create_dir_all(&src).unwrap();
        fs::create_dir_all(&dst).unwrap();
        fs::write(src.join("f1.txt"), b"test 1").unwrap();
        fs::write(src.join("f2.txt"), b"test 2").unwrap();

        let config = Config::builder(src)
            .dest_dir(dst.clone())
            .build_unvalidated();
        let target_cfg = TargetSyncConfig::try_from_config(&config).unwrap();
        let store = MockHashStore::new();

        // Inject error hook on save_files_batch
        store.set_error_hook(Some(Box::new(|op| {
            if op == "save_files_batch" {
                Some(SyncError::db("simulated batch failure"))
            } else {
                None
            }
        })));

        let engine = LocalSyncEngine::new(store.clone(), target_cfg);
        let mut scratch = vec![0u8; 64 * 1024];
        engine
            .sync_file_to_dest_staged(Path::new("f1.txt"), &dst, &mut scratch)
            .unwrap();
        engine
            .sync_file_to_dest_staged(Path::new("f2.txt"), &dst, &mut scratch)
            .unwrap();

        // Flush should fall back to individual save_file calls
        engine.flush_staged_syncs().unwrap();
        assert_eq!(store.batch_save_count(), 0);
        assert_eq!(store.save_file_count(), 2);
    }

    #[test]
    fn test_sync_command_strongly_typed_relative_path() {
        let rel = crate::path_util::RelativePath::new("valid/path.txt").unwrap();
        let cmd_mod = SyncCommand::FileModified(rel.clone());
        let cmd_del = SyncCommand::FileDeleted(rel.clone());

        match &cmd_mod {
            SyncCommand::FileModified(p) => {
                let _: &crate::path_util::RelativePath = p;
                assert_eq!(p.as_path(), Path::new("valid/path.txt"));
            }
            _ => panic!("Expected FileModified"),
        }

        match &cmd_del {
            SyncCommand::FileDeleted(p) => {
                let _: &crate::path_util::RelativePath = p;
                assert_eq!(p.as_path(), Path::new("valid/path.txt"));
            }
            _ => panic!("Expected FileDeleted"),
        }
    }

    #[test]
    fn test_delete_file_from_dest_symmetrical_reparse_eviction() {
        use crate::config::{Config, TargetSyncConfig};
        use crate::db::MockHashStore;
        use crate::sync::engine::LocalSyncEngine;
        use std::fs;
        use std::path::Path;
        use tempfile::tempdir;

        let temp = tempdir().unwrap();
        let src = temp.path().join("src");
        let dst = temp.path().join("dst");
        let src_sub = src.join("nested_dir");
        let dst_sub = dst.join("nested_dir");
        fs::create_dir_all(&src_sub).unwrap();
        fs::create_dir_all(&dst_sub).unwrap();

        let target_file = dst_sub.join("victim.txt");
        fs::write(&target_file, b"to be deleted").unwrap();

        let config = Config::builder(src.clone())
            .dest_dir(dst.clone())
            .propagate_deletions(true)
            .build()
            .unwrap();
        let target_cfg = TargetSyncConfig::from_config(&config, dst.clone()).unwrap();
        let store = MockHashStore::new();
        let engine = LocalSyncEngine::new(store, target_cfg);

        let cache = engine.reparse_cache();
        cache.insert_ancestor(&dst, &dst_sub);
        cache.insert_ancestor(&src, &src_sub);

        assert!(cache.contains(&dst_sub));
        assert!(cache.contains(&src_sub));

        engine
            .delete_file_from_dest(Path::new("nested_dir/victim.txt"), &dst)
            .unwrap();

        assert!(
            !cache.contains(&dst_sub),
            "Destination parent must be evicted from ReparseCache upon file deletion"
        );
        assert!(
            !cache.contains(&src_sub),
            "Source parent must be symmetrically evicted from ReparseCache upon file deletion (CWE-59)"
        );
    }

    #[test]
    fn test_case_only_rename_on_destination_updates_disk_casing_and_db() {
        use crate::config::{Config, TargetSyncConfig};
        use crate::db::{HashStore, SqliteHashStore, StoreConfig};
        use crate::sync::engine::LocalSyncEngine;
        use std::fs;
        use std::path::Path;
        use tempfile::tempdir;

        let temp = tempdir().unwrap();
        let src = temp.path().join("source");
        let dst = temp.path().join("dest");
        fs::create_dir_all(&src).unwrap();
        fs::create_dir_all(&dst).unwrap();

        let initial_content = b"Case Sensitivity In-Place Delta Content";
        let src_file_lower = src.join("test.txt");
        fs::write(&src_file_lower, initial_content).unwrap();

        let db_path = temp.path().join("sigcache.db");
        let store_cfg = StoreConfig::new(64 * 1024, 1024 * 1024).unwrap();
        let db = SqliteHashStore::new(&db_path, store_cfg).unwrap();

        let config = Config::builder(src.clone())
            .dest_dir(dst.clone())
            .build()
            .unwrap();
        let target_cfg = TargetSyncConfig::from_config(&config, dst.clone()).unwrap();
        let engine = LocalSyncEngine::new(db, target_cfg);

        let mut scratch = vec![0u8; 64 * 1024];

        engine
            .sync_file_to_dest_buffered(Path::new("test.txt"), &dst, &mut scratch)
            .unwrap();

        let initial_rec = engine
            .db()
            .get_file(Path::new("test.txt"))
            .unwrap()
            .expect("Record must exist after initial sync");
        assert_eq!(initial_rec.relative_path().as_path(), Path::new("test.txt"));

        let src_file_cased = src.join("Test.txt");
        let temp_stage = src.join("test.txt.syncdir_casetmp");
        fs::rename(&src_file_lower, &temp_stage).unwrap();
        fs::rename(&temp_stage, &src_file_cased).unwrap();

        engine
            .sync_file_to_dest_buffered(Path::new("Test.txt"), &dst, &mut scratch)
            .unwrap();

        let updated_rec = engine
            .db()
            .get_file(Path::new("Test.txt"))
            .unwrap()
            .expect("Record must be queryable via new casing");
        assert_eq!(
            updated_rec.relative_path().as_path(),
            Path::new("Test.txt"),
            "SQLite relative_path must preserve new path casing"
        );

        #[cfg(windows)]
        {
            let entries: Vec<String> = fs::read_dir(&dst)
                .unwrap()
                .filter_map(|e| {
                    e.ok()
                        .map(|de| de.file_name().to_string_lossy().into_owned())
                })
                .collect();
            assert!(
                entries.contains(&"Test.txt".to_string()),
                "Destination directory entry must update to 'Test.txt', found: {:?}",
                entries
            );
            assert!(
                !entries.contains(&"test.txt".to_string()),
                "Old lowercase 'test.txt' directory entry must not linger on destination filesystem"
            );
        }
    }

    #[test]
    fn test_segregated_role_traits_and_reparse_cache_export() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        let dst = dir.path().join("dst");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&dst).unwrap();

        let cfg = TargetSyncConfig::new(
            crate::config::TargetDir::from_validated(src),
            crate::config::TargetDir::from_validated(dst),
        )
        .unwrap();
        let store_cfg =
            crate::db::StoreConfig::new(cfg.block_size_bytes(), cfg.block_sync_threshold_bytes())
                .unwrap();
        let db = std::sync::Arc::new(
            SqliteHashStore::new(&dir.path().join("db.sqlite"), store_cfg).unwrap(),
        );
        let engine = LocalSyncEngine::new(db, cfg);

        let _: &dyn FileSynchronizer = &engine;
        let _: &dyn FileDeleter = &engine;
        let _: &dyn BatchFlusher = &engine;
        let _: &dyn ScanEngine = &engine;
        let _: &dyn ArchiveEngine = &engine;
        let _: &dyn SyncEngine = &engine;
        assert_eq!(engine.reparse_cache().len(), 0);
    }
}

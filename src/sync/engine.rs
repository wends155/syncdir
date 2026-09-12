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

#[allow(unused_imports, deprecated)]
pub use super::types::{
    FileMetadataSnapshot, FileSyncTask, RelativePath, is_metadata_up_to_date_raw,
    safe_epoch_duration_millis, safe_modified_millis,
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
pub use super::traits::*;

#[cfg(test)]
pub(crate) static CASING_ALIGN_READ_DIR_COUNT: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

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

    /// Deprecated: use [`Self::run_configured_full_scan`] instead.
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
                let is_verified_cache_hit = if let Some(record) = file_record {
                    record.is_tracked()
                        && record.file_size() == src_size as u64
                        && record.last_modified() == src_mod
                        && record.relative_path() == &safe_rel
                } else {
                    false
                };

                if is_verified_cache_hit {
                    tracing::debug!(path = %rel_path.display(), "Local signature cache hit and destination matches, skipping sync");
                    return Ok(None);
                }

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
        #[cfg(test)]
        CASING_ALIGN_READ_DIR_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

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
        #[cfg(test)]
        CASING_ALIGN_READ_DIR_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

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

impl<S: HashStore> CasingAligner for LocalSyncEngine<S> {
    fn align_casing_if_needed(&self, dest_dir: &Path, rel_path: &Path) -> Result<(), SyncError> {
        self.align_dest_file_casing_if_needed(dest_dir, rel_path)
    }
}

impl<S: HashStore> ArchiveManager for LocalSyncEngine<S> {
    fn archive_file(&self, dest_dir: &Path, rel_path: &Path) -> Result<(), SyncError> {
        self.delete_file_from_dest(rel_path, dest_dir)
    }

    fn prune(&self, dest_dir: &Path) -> Result<(), SyncError> {
        self.prune_destination_archive(dest_dir)
    }
}

#[cfg(test)]
#[path = "engine_tests.rs"]
mod tests;

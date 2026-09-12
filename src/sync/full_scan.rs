//! Full directory scan coordinator.
//!
//! Decomposes `LocalSyncEngine::run_cancellable_full_scan_impl` into discrete,
//! low-complexity lifecycle stages with zero-allocation deletion reconciliation.

use std::collections::{HashMap, HashSet};
use std::hash::Hash;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::db::{FileRecord, HashStore};
use crate::error::SyncError;
use crate::sync::engine::{LocalSyncEngine, ScanOutcome, calculate_remaining_files};

/// Borrowed path wrapper that hashes and compares paths case-insensitively
/// and slash-insensitively (normalizing `\\` to `/`) without allocating.
#[derive(Debug, Copy, Clone)]
pub(crate) struct NormalizedCaseFoldedPath<'a>(pub &'a Path);

impl Hash for NormalizedCaseFoldedPath<'_> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        if let Some(s) = self.0.to_str() {
            for b in s.bytes() {
                let norm_b = if b == b'\\' {
                    b'/'
                } else {
                    b.to_ascii_lowercase()
                };
                state.write_u8(norm_b);
            }
        }
    }
}

impl PartialEq for NormalizedCaseFoldedPath<'_> {
    fn eq(&self, other: &Self) -> bool {
        match (self.0.to_str(), other.0.to_str()) {
            (Some(s1), Some(s2)) => {
                if s1.len() != s2.len() {
                    return false;
                }
                s1.bytes().zip(s2.bytes()).all(|(b1, b2)| {
                    let n1 = if b1 == b'\\' {
                        b'/'
                    } else {
                        b1.to_ascii_lowercase()
                    };
                    let n2 = if b2 == b'\\' {
                        b'/'
                    } else {
                        b2.to_ascii_lowercase()
                    };
                    n1 == n2
                })
            }
            _ => self.0 == other.0,
        }
    }
}

impl Eq for NormalizedCaseFoldedPath<'_> {}

/// Aggregate synchronization statistics produced during file synchronization.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SyncStats {
    pub synced: usize,
    pub failed: usize,
    pub skipped: usize,
    pub network_offline: bool,
}

/// Abstraction driving the individual operations of a full scan cycle.
///
/// Decouples [`FullScanCoordinator`] from concrete engine implementations, allowing
/// lightweight, pure in-memory testing of scanning, synchronization, and deletion lifecycles.
pub trait FullScanDriver: Send + Sync {
    /// Validate source existence and resolve the active destination directory.
    fn resolve_active_destination(&self, dest_dir: &Path) -> Result<Option<PathBuf>, SyncError>;

    /// Scan source directory for files, returning discovered relative paths and whether the scan completed fully.
    fn scan_source_files(&self, cancel: &AtomicBool)
    -> Result<(HashSet<PathBuf>, bool), SyncError>;

    /// Load existing database records for comparison.
    fn load_cached_records(&self) -> Result<Vec<FileRecord>, SyncError>;

    /// Synchronize a single file to the destination directory.
    fn sync_file_core(
        &self,
        rel_path: &Path,
        dest_dir: &Path,
        scratch: &mut [u8],
        cached: Option<&FileRecord>,
    ) -> Result<Option<(FileRecord, Vec<crate::db::BlockHash>)>, SyncError>;

    /// Archive or remove a file on destination filesystem without updating database.
    fn archive_dest_file_only(&self, rel_path: &Path, dest_dir: &Path) -> Result<(), SyncError>;

    /// Delete records in batch from the database.
    fn delete_files_batch(&self, paths: &[&Path]) -> Result<(), SyncError>;

    /// Flush a batch of synchronized records and block hashes to the database.
    fn flush_record_batch(
        &self,
        batch: &mut Vec<(FileRecord, Vec<crate::db::BlockHash>)>,
    ) -> Result<(), SyncError>;

    /// Prune old destination archive versions.
    fn prune_destination_archive(&self, dest_dir: &Path) -> Result<(), SyncError>;

    /// Check if deletions should be propagated to destination.
    fn propagate_deletions(&self) -> bool;

    /// Get block size in bytes for buffer allocation.
    fn block_size_bytes(&self) -> u64;
}

impl<S: HashStore> FullScanDriver for LocalSyncEngine<S> {
    fn resolve_active_destination(&self, dest_dir: &Path) -> Result<Option<PathBuf>, SyncError> {
        let resolved_source = self.config().source_dir();
        if !resolved_source.exists() {
            return Err(SyncError::validation("Source directory does not exist"));
        }

        let active_dest = if let Some(pre_resolved) = self.resolved_dest() {
            if pre_resolved.exists() && pre_resolved.is_dir() {
                pre_resolved.to_path_buf()
            } else {
                dest_dir.to_path_buf()
            }
        } else {
            dest_dir.to_path_buf()
        };

        if !active_dest.exists() || !active_dest.is_dir() {
            tracing::warn!(
                target = %active_dest.display(),
                "Target destination directory does not exist or is unreachable. Skipping full scan."
            );
            return Ok(None);
        }

        Ok(Some(active_dest))
    }

    fn scan_source_files(
        &self,
        cancel: &AtomicBool,
    ) -> Result<(HashSet<PathBuf>, bool), SyncError> {
        let mut source_files = HashSet::new();
        let mut scan_complete = true;
        self.scanner().scan_dir_cancellable(
            self.config().source_dir(),
            &mut source_files,
            &mut scan_complete,
            cancel,
        )?;
        Ok((source_files, scan_complete))
    }

    fn load_cached_records(&self) -> Result<Vec<FileRecord>, SyncError> {
        self.db().list_all_records()
    }

    fn sync_file_core(
        &self,
        rel_path: &Path,
        dest_dir: &Path,
        scratch: &mut [u8],
        cached: Option<&FileRecord>,
    ) -> Result<Option<(FileRecord, Vec<crate::db::BlockHash>)>, SyncError> {
        self.sync_file_to_dest_core(rel_path, dest_dir, scratch, cached)
    }

    fn archive_dest_file_only(&self, rel_path: &Path, dest_dir: &Path) -> Result<(), SyncError> {
        self.archive_dest_file_only(rel_path, dest_dir)
    }

    fn delete_files_batch(&self, paths: &[&Path]) -> Result<(), SyncError> {
        self.db().delete_files_batch(paths)
    }

    fn flush_record_batch(
        &self,
        batch: &mut Vec<(FileRecord, Vec<crate::db::BlockHash>)>,
    ) -> Result<(), SyncError> {
        self.flush_record_batch(batch)
    }

    fn prune_destination_archive(&self, dest_dir: &Path) -> Result<(), SyncError> {
        self.prune_destination_archive(dest_dir)
    }

    fn propagate_deletions(&self) -> bool {
        self.config().propagate_deletions()
    }

    fn block_size_bytes(&self) -> u64 {
        self.config().block_size_bytes()
    }
}

/// Orchestrates full directory synchronization across modular, testable stages.
pub struct FullScanCoordinator<'a, D: FullScanDriver + ?Sized> {
    driver: &'a D,
    dest_dir: &'a Path,
    cancel: &'a AtomicBool,
}

impl<'a, D: FullScanDriver + ?Sized> FullScanCoordinator<'a, D> {
    /// Create a new full scan coordinator.
    pub fn new(driver: &'a D, dest_dir: &'a Path, cancel: &'a AtomicBool) -> Self {
        Self {
            driver,
            dest_dir,
            cancel,
        }
    }

    /// Stage 1: Validate source existence and resolve the active destination directory.
    pub(crate) fn resolve_active_destination(&self) -> Result<Option<PathBuf>, SyncError> {
        if self.cancel.load(Ordering::Relaxed) {
            return Err(SyncError::Cancelled);
        }
        self.driver.resolve_active_destination(self.dest_dir)
    }

    /// Stage 2: Scan source directory for files, supporting cancellation.
    pub(crate) fn collect_source_files(&self) -> Result<(HashSet<PathBuf>, bool), SyncError> {
        self.driver.scan_source_files(self.cancel)
    }

    /// Stage 3: Load existing database records and construct normalized lookup map.
    pub(crate) fn load_cached_records(&self) -> Result<Vec<FileRecord>, SyncError> {
        self.driver.load_cached_records()
    }

    /// Build normalized lookup mapping relative paths to database records.
    pub(crate) fn build_cache_lookup<'b>(
        &self,
        cached_records: &'b [FileRecord],
    ) -> HashMap<NormalizedCaseFoldedPath<'b>, &'b FileRecord> {
        cached_records
            .iter()
            .map(|rec| (NormalizedCaseFoldedPath(rec.relative_path().as_path()), rec))
            .collect()
    }

    /// Stage 4: Synchronize source files to active destination in batches.
    pub(crate) fn synchronize_files(
        &self,
        active_dest: &Path,
        source_files: &HashSet<PathBuf>,
        cached_lookup: &HashMap<NormalizedCaseFoldedPath<'_>, &FileRecord>,
    ) -> Result<SyncStats, SyncError> {
        let mut stats = SyncStats::default();
        let mut scratch = vec![0u8; self.driver.block_size_bytes() as usize];
        let mut batch: Vec<(FileRecord, Vec<crate::db::BlockHash>)> = Vec::with_capacity(500);

        for rel_path in source_files {
            if self.cancel.load(Ordering::Relaxed) {
                self.driver.flush_record_batch(&mut batch)?;
                return Err(SyncError::Cancelled);
            }

            match self.driver.sync_file_core(
                rel_path,
                active_dest,
                &mut scratch,
                cached_lookup
                    .get(&NormalizedCaseFoldedPath(rel_path))
                    .copied(),
            ) {
                Ok(Some((record, hashes))) => {
                    stats.synced += 1;
                    batch.push((record, hashes));
                    if batch.len() >= 500 {
                        self.driver.flush_record_batch(&mut batch)?;
                    }
                }
                Ok(None) => {
                    stats.synced += 1;
                }
                Err(e) => {
                    stats.failed += 1;
                    let os_code = match &e {
                        SyncError::Io(io_err) => io_err.raw_os_error(),
                        _ => None,
                    };
                    if e.is_network_offline() {
                        stats.network_offline = true;
                        tracing::warn!(
                            path = %rel_path.display(),
                            target = %active_dest.display(),
                            error = %e,
                            os_error = ?os_code,
                            remaining = calculate_remaining_files(
                                source_files.len(),
                                stats.synced,
                                stats.failed,
                            ),
                            "Target unreachable during full scan, skipping remaining files"
                        );
                        stats.skipped = source_files.len();
                        break;
                    }
                    tracing::warn!(
                        path = %rel_path.display(),
                        target = %active_dest.display(),
                        error = %e,
                        os_error = ?os_code,
                        "Skipped file during full scan"
                    );
                    stats.skipped += 1;
                }
            }
        }

        self.driver.flush_record_batch(&mut batch)?;

        if stats.skipped > 0 {
            tracing::warn!(
                skipped = stats.skipped,
                total = source_files.len(),
                target = %active_dest.display(),
                "Full scan completed with sync errors"
            );
        }

        Ok(stats)
    }

    /// Stage 5: Reconcile deleted files using zero-allocation case-insensitive lookup.
    pub(crate) fn reconcile_deletions(
        &self,
        active_dest: &Path,
        source_files: &HashSet<PathBuf>,
        cached_records: &[FileRecord],
        scan_complete: bool,
    ) -> Result<usize, SyncError> {
        if !self.driver.propagate_deletions() {
            return Ok(0);
        }

        if !scan_complete {
            tracing::warn!(
                "Full scan was incomplete due to inaccessible directories or errors; skipping deletion propagation to prevent data loss"
            );
            return Ok(0);
        }

        if source_files.is_empty() && !cached_records.is_empty() {
            tracing::warn!(
                tracked_count = cached_records.len(),
                "Source directory is empty but cache contains tracked files. Skipping deletion propagation to prevent accidental target wipe."
            );
            return Ok(0);
        }

        #[cfg(windows)]
        let source_lookup: HashSet<NormalizedCaseFoldedPath<'_>> = source_files
            .iter()
            .map(|p| NormalizedCaseFoldedPath(p.as_path()))
            .collect();

        let mut delete_skip_count = 0usize;
        let mut missing_to_delete: Vec<&Path> = Vec::new();

        for record in cached_records {
            if self.cancel.load(Ordering::Relaxed) {
                return Err(SyncError::Cancelled);
            }
            let tracked_path = record.relative_path().as_path();

            #[cfg(windows)]
            let is_present = source_lookup.contains(&NormalizedCaseFoldedPath(tracked_path));
            #[cfg(not(windows))]
            let is_present = source_files.contains(tracked_path);

            if !is_present {
                if let Err(e) = self
                    .driver
                    .archive_dest_file_only(tracked_path, active_dest)
                {
                    let os_code = match &e {
                        SyncError::Io(io_err) => io_err.raw_os_error(),
                        _ => None,
                    };
                    if e.is_network_offline() {
                        tracing::warn!(
                            path = %tracked_path.display(),
                            target = %active_dest.display(),
                            error = %e,
                            os_error = ?os_code,
                            "Target unreachable during deletion phase of full scan, aborting deletion pass"
                        );
                        delete_skip_count += 1;
                        break;
                    }
                    tracing::warn!(
                        path = %tracked_path.display(),
                        target = %active_dest.display(),
                        error = %e,
                        os_error = ?os_code,
                        "Skipped deletion during full scan"
                    );
                    delete_skip_count += 1;
                } else {
                    missing_to_delete.push(tracked_path);
                }
            }
        }

        if !missing_to_delete.is_empty() {
            self.driver.delete_files_batch(&missing_to_delete)?;
        }

        if delete_skip_count > 0 {
            tracing::warn!(
                skipped = delete_skip_count,
                target = %active_dest.display(),
                "Full scan completed with deletion errors"
            );
        }

        Ok(delete_skip_count)
    }

    /// Stage 6: Prune old destination archive versions.
    pub(crate) fn prune_archive(&self, active_dest: &Path) {
        if let Err(e) = self.driver.prune_destination_archive(active_dest) {
            tracing::warn!(
                target = %active_dest.display(),
                error = %e,
                "Failed to prune destination archive after full scan"
            );
        }
    }

    /// Execute the full directory scan across all stages.
    pub fn run(&self) -> Result<ScanOutcome, SyncError> {
        let active_dest = match self.resolve_active_destination()? {
            Some(dest) => dest,
            None => return Ok(ScanOutcome::DestinationUnreachable),
        };

        let (source_files, scan_complete) = self.collect_source_files()?;
        let cached_records = self.load_cached_records()?;
        let cached_lookup = self.build_cache_lookup(&cached_records);

        let stats = self.synchronize_files(&active_dest, &source_files, &cached_lookup)?;

        if !source_files.is_empty() && stats.skipped == source_files.len() {
            return Ok(ScanOutcome::DestinationUnreachable);
        }

        if source_files.is_empty()
            && !cached_records.is_empty()
            && self.driver.propagate_deletions()
        {
            tracing::warn!(
                tracked_count = cached_records.len(),
                "Source directory is empty but cache contains tracked files. Skipping deletion propagation to prevent accidental target wipe."
            );
            self.prune_archive(&active_dest);
            return Ok(ScanOutcome::Success { synced: 0 });
        }

        let delete_skip_count = if scan_complete {
            self.reconcile_deletions(&active_dest, &source_files, &cached_records, scan_complete)?
        } else {
            0
        };

        self.prune_archive(&active_dest);

        if stats.failed > 0 || delete_skip_count > 0 {
            Ok(ScanOutcome::PartialFailure {
                synced: stats.synced,
                failed: stats.failed,
                delete_failed: delete_skip_count,
            })
        } else {
            Ok(ScanOutcome::Success {
                synced: stats.synced,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, TargetDir, TargetSyncConfig};
    use crate::db::MockHashStore;
    use std::collections::hash_map::DefaultHasher;
    use std::fs;
    use std::hash::Hasher;
    use tempfile::tempdir;

    #[test]
    fn test_full_scan_coordinator_stage1_unreachable_destination() {
        let temp = tempdir().unwrap();
        let src = temp.path().join("src");
        let dst = temp.path().join("nonexistent_dst");
        fs::create_dir_all(&src).unwrap();

        let config = Config::test_default(src, dst.clone());
        let target_cfg = TargetSyncConfig::try_from_config(&config).unwrap();
        let store = MockHashStore::new();
        let engine = LocalSyncEngine::new(store.clone(), target_cfg);
        let cancel = AtomicBool::new(false);

        let coordinator = FullScanCoordinator::new(&engine, &dst, &cancel);
        let outcome = coordinator.run().unwrap();
        assert_eq!(outcome, ScanOutcome::DestinationUnreachable);
        assert_eq!(store.list_all_records().unwrap().len(), 0);
    }

    #[test]
    fn test_full_scan_coordinator_stages2_3_case_insensitive_matching() {
        let p1 = Path::new(r"nested\FILE.TXT");
        let p2 = Path::new("nested/file.txt");
        let p3 = Path::new("other/file.txt");

        let n1 = NormalizedCaseFoldedPath(p1);
        let n2 = NormalizedCaseFoldedPath(p2);
        let n3 = NormalizedCaseFoldedPath(p3);

        assert_eq!(n1, n2);
        assert_ne!(n1, n3);

        let mut h1 = DefaultHasher::new();
        let mut h2 = DefaultHasher::new();
        n1.hash(&mut h1);
        n2.hash(&mut h2);
        assert_eq!(h1.finish(), h2.finish());

        let mut set = HashSet::new();
        set.insert(n1);
        assert!(set.contains(&n2));
        assert!(!set.contains(&n3));
    }

    #[test]
    fn test_full_scan_coordinator_stage4_cancellation() {
        let temp = tempdir().unwrap();
        let src = temp.path().join("src");
        let dst = temp.path().join("dst");
        fs::create_dir_all(&src).unwrap();
        fs::create_dir_all(&dst).unwrap();
        fs::write(src.join("file.txt"), b"hello").unwrap();

        let config = Config::test_default(src, dst.clone());
        let target_cfg = TargetSyncConfig::try_from_config(&config).unwrap();
        let store = MockHashStore::new();
        let engine = LocalSyncEngine::new(store, target_cfg);
        let cancel = AtomicBool::new(true);

        let coordinator = FullScanCoordinator::new(&engine, &dst, &cancel);
        let res = coordinator.run();
        assert!(matches!(res, Err(SyncError::Cancelled)));
    }

    #[test]
    fn test_full_scan_coordinator_stage5_zero_allocation_deletion_reconciliation() {
        let temp = tempdir().unwrap();
        let src = temp.path().join("src");
        let dst = temp.path().join("dst");
        fs::create_dir_all(&src).unwrap();
        fs::create_dir_all(&dst).unwrap();

        fs::write(src.join("keep.txt"), b"kept content").unwrap();
        fs::write(dst.join("keep.txt"), b"kept content").unwrap();
        fs::write(dst.join("remove.txt"), b"to be removed").unwrap();

        let store = MockHashStore::new();
        let rec_keep = FileRecord::from_raw("keep.txt", 12, 1000)
            .unwrap()
            .with_id(1);
        let rec_remove = FileRecord::from_raw("remove.txt", 13, 2000)
            .unwrap()
            .with_id(2);
        store.save_file(&rec_keep, &[]).unwrap();
        store.save_file(&rec_remove, &[]).unwrap();

        let config = Config::builder(src)
            .dest_dir(dst.clone())
            .propagate_deletions(true)
            .build()
            .unwrap();
        let target_cfg =
            TargetSyncConfig::from_config(&config, TargetDir::from_validated(dst.clone())).unwrap();
        let engine = LocalSyncEngine::new(store.clone(), target_cfg);
        let cancel = AtomicBool::new(false);

        let coordinator = FullScanCoordinator::new(&engine, &dst, &cancel);
        let outcome = coordinator.run().unwrap();
        assert_eq!(outcome, ScanOutcome::Success { synced: 1 });

        // remove.txt must be pruned/archived and deleted from db
        assert!(store.get_file(Path::new("remove.txt")).unwrap().is_none());
        assert!(!dst.join("remove.txt").exists());
        assert!(dst.join(".syncdir_archive").exists());

        // keep.txt must remain
        assert!(store.get_file(Path::new("keep.txt")).unwrap().is_some());
        assert!(dst.join("keep.txt").exists());
    }

    #[test]
    fn test_case_folded_zero_allocation_cache_lookup() {
        use crate::config::{Config, TargetSyncConfig};
        use crate::db::{FileRecord, MockHashStore};
        use crate::sync::engine::LocalSyncEngine;
        use std::path::Path;
        use std::sync::atomic::AtomicBool;
        use tempfile::tempdir;

        let temp = tempdir().unwrap();
        let src = temp.path().join("src");
        let dst = temp.path().join("dst");
        let config = Config::test_default(src, dst.clone());
        let target_cfg = TargetSyncConfig::try_from_config(&config).unwrap();
        let engine = LocalSyncEngine::new(MockHashStore::new(), target_cfg);
        let cancel = AtomicBool::new(false);
        let coordinator = FullScanCoordinator::new(&engine, &dst, &cancel);

        let rec1 = FileRecord::from_raw("Docs/Architecture.md", 2048, 1000)
            .unwrap()
            .with_id(1);
        let rec2 = FileRecord::from_raw(r"Assets\Icons\Logo.PNG", 4096, 2000)
            .unwrap()
            .with_id(2);
        let cached_records = vec![rec1, rec2];

        let lookup = coordinator.build_cache_lookup(&cached_records);

        let hit1 = lookup.get(&NormalizedCaseFoldedPath(Path::new("docs/architecture.md")));
        assert!(hit1.is_some());
        assert_eq!(hit1.unwrap().id(), Some(1));

        let hit2 = lookup.get(&NormalizedCaseFoldedPath(Path::new(
            "assets/icons/logo.png",
        )));
        assert!(hit2.is_some());
        assert_eq!(hit2.unwrap().id(), Some(2));

        let hit3 = lookup.get(&NormalizedCaseFoldedPath(Path::new("DOCS/ARCHITECTURE.MD")));
        assert!(hit3.is_some());

        assert!(!lookup.contains_key(&NormalizedCaseFoldedPath(Path::new(
            "docs/specification.md"
        ))));
    }

    /// Pure in-memory driver for [`FullScanCoordinator`].
    #[derive(Clone, Debug)]
    pub(crate) struct MockFullScanDriver {
        source_files: std::sync::Arc<std::sync::Mutex<std::collections::HashSet<PathBuf>>>,
        scan_complete: std::sync::Arc<std::sync::atomic::AtomicBool>,
        cached_records: std::sync::Arc<std::sync::Mutex<Vec<crate::db::FileRecord>>>,
        synced_files: std::sync::Arc<std::sync::Mutex<Vec<(PathBuf, PathBuf)>>>,
        deleted_files: std::sync::Arc<std::sync::Mutex<Vec<PathBuf>>>,
        archived_files: std::sync::Arc<std::sync::Mutex<Vec<PathBuf>>>,
        pruned_archives: std::sync::Arc<std::sync::Mutex<Vec<PathBuf>>>,
        propagate_deletions: bool,
        block_size: u64,
        dest_unreachable: bool,
    }

    impl Default for MockFullScanDriver {
        fn default() -> Self {
            Self {
                source_files: std::sync::Arc::new(std::sync::Mutex::new(
                    std::collections::HashSet::new(),
                )),
                scan_complete: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
                cached_records: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
                synced_files: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
                deleted_files: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
                archived_files: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
                pruned_archives: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
                propagate_deletions: true,
                block_size: 4096,
                dest_unreachable: false,
            }
        }
    }

    #[allow(dead_code)]
    impl MockFullScanDriver {
        pub fn new() -> Self {
            Self::default()
        }

        pub fn with_source_files(self, files: impl IntoIterator<Item = PathBuf>) -> Self {
            let mut sf = self.source_files.lock().unwrap_or_else(|p| p.into_inner());
            sf.extend(files);
            drop(sf);
            self
        }

        pub fn with_scan_complete(self, complete: bool) -> Self {
            self.scan_complete
                .store(complete, std::sync::atomic::Ordering::Relaxed);
            self
        }

        pub fn with_cached_records(
            self,
            records: impl IntoIterator<Item = (PathBuf, u64, u64)>,
        ) -> Self {
            let mut cr = self
                .cached_records
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            for (i, (path, size, mtime)) in records.into_iter().enumerate() {
                let rec = crate::db::FileRecord::from_raw(&path, size, mtime as i64)
                    .unwrap_or_else(|_| {
                        let rel = crate::path_util::RelativePath::try_new("fallback.txt").unwrap();
                        crate::db::FileRecord::new(rel, size, mtime as i64)
                    })
                    .with_id(i as i64 + 1);
                cr.push(rec);
            }
            drop(cr);
            self
        }

        pub fn with_propagate_deletions(mut self, propagate: bool) -> Self {
            self.propagate_deletions = propagate;
            self
        }

        pub fn with_dest_unreachable(mut self, unreachable: bool) -> Self {
            self.dest_unreachable = unreachable;
            self
        }

        pub fn synced_files(&self) -> Vec<(PathBuf, PathBuf)> {
            self.synced_files
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .clone()
        }

        pub fn deleted_files(&self) -> Vec<PathBuf> {
            self.deleted_files
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .clone()
        }
    }

    impl FullScanDriver for MockFullScanDriver {
        fn resolve_active_destination(
            &self,
            dest_dir: &Path,
        ) -> Result<Option<PathBuf>, SyncError> {
            if self.dest_unreachable {
                Ok(None)
            } else {
                Ok(Some(dest_dir.to_path_buf()))
            }
        }

        fn scan_source_files(
            &self,
            cancel: &std::sync::atomic::AtomicBool,
        ) -> Result<(std::collections::HashSet<PathBuf>, bool), SyncError> {
            if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                return Err(SyncError::Cancelled);
            }
            let files = self
                .source_files
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .clone();
            let complete = self
                .scan_complete
                .load(std::sync::atomic::Ordering::Relaxed);
            Ok((files, complete))
        }

        fn load_cached_records(&self) -> Result<Vec<crate::db::FileRecord>, SyncError> {
            Ok(self
                .cached_records
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .clone())
        }

        fn sync_file_core(
            &self,
            rel_path: &Path,
            dest_dir: &Path,
            _scratch: &mut [u8],
            _cached: Option<&crate::db::FileRecord>,
        ) -> Result<Option<(crate::db::FileRecord, Vec<crate::db::BlockHash>)>, SyncError> {
            self.synced_files
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .push((rel_path.to_path_buf(), dest_dir.to_path_buf()));
            let rec = crate::db::FileRecord::from_raw(rel_path, 100, 1000).unwrap_or_else(|_| {
                let rel = crate::path_util::RelativePath::try_new("fallback.txt").unwrap();
                crate::db::FileRecord::new(rel, 100, 1000)
            });
            Ok(Some((rec, vec![])))
        }

        fn archive_dest_file_only(
            &self,
            rel_path: &Path,
            _dest_dir: &Path,
        ) -> Result<(), SyncError> {
            self.archived_files
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .push(rel_path.to_path_buf());
            Ok(())
        }

        fn delete_files_batch(&self, paths: &[&Path]) -> Result<(), SyncError> {
            let mut deleted = self.deleted_files.lock().unwrap_or_else(|p| p.into_inner());
            let mut cached = self
                .cached_records
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            for p in paths {
                deleted.push(p.to_path_buf());
            }
            cached.retain(|r| !paths.contains(&r.relative_path().as_path()));
            Ok(())
        }

        fn flush_record_batch(
            &self,
            batch: &mut Vec<(crate::db::FileRecord, Vec<crate::db::BlockHash>)>,
        ) -> Result<(), SyncError> {
            batch.clear();
            Ok(())
        }

        fn prune_destination_archive(&self, dest_dir: &Path) -> Result<(), SyncError> {
            self.pruned_archives
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .push(dest_dir.to_path_buf());
            Ok(())
        }

        fn propagate_deletions(&self) -> bool {
            self.propagate_deletions
        }

        fn block_size_bytes(&self) -> u64 {
            self.block_size
        }
    }

    #[test]
    fn test_mock_full_scan_driver_in_memory() {
        let driver = MockFullScanDriver::new()
            .with_source_files(vec![PathBuf::from("a.txt")])
            .with_cached_records(vec![(PathBuf::from("b.txt"), 50, 1000)])
            .with_scan_complete(true);
        let cancel = std::sync::atomic::AtomicBool::new(false);
        let (files, complete) = driver.scan_source_files(&cancel).unwrap();
        assert!(complete);
        assert_eq!(files.len(), 1);
        assert!(files.contains(&PathBuf::from("a.txt")));
        let records = driver.load_cached_records().unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].relative_path().as_path(), Path::new("b.txt"));
    }

    #[test]
    fn test_full_scan_driver_cancellation_guards_deletions() {
        let dst = Path::new("dst");
        let cancel = AtomicBool::new(false);
        let driver = MockFullScanDriver::new()
            .with_source_files(vec![PathBuf::from("file1.txt")])
            .with_scan_complete(false)
            .with_propagate_deletions(true)
            .with_cached_records(vec![
                (PathBuf::from("file1.txt"), 10, 100),
                (PathBuf::from("file2.txt"), 20, 200),
            ]);
        let coordinator = FullScanCoordinator::new(&driver, dst, &cancel);
        let outcome = coordinator.run().unwrap();
        assert_eq!(outcome, ScanOutcome::Success { synced: 1 });
        // Since scan_complete was false, file2.txt MUST NOT have been deleted
        assert_eq!(driver.deleted_files().len(), 0);
        assert_eq!(driver.synced_files().len(), 1);

        // Now test when scan_complete is true
        let driver_complete = MockFullScanDriver::new()
            .with_source_files(vec![PathBuf::from("file1.txt")])
            .with_scan_complete(true)
            .with_propagate_deletions(true)
            .with_cached_records(vec![
                (PathBuf::from("file1.txt"), 10, 100),
                (PathBuf::from("file2.txt"), 20, 200),
            ]);
        let coordinator_complete = FullScanCoordinator::new(&driver_complete, dst, &cancel);
        let outcome_complete = coordinator_complete.run().unwrap();
        assert_eq!(outcome_complete, ScanOutcome::Success { synced: 1 });
        // Since scan_complete was true, file2.txt MUST have been deleted
        assert_eq!(driver_complete.deleted_files().len(), 1);
        assert_eq!(
            driver_complete.deleted_files()[0],
            PathBuf::from("file2.txt")
        );
    }

    #[test]
    fn test_full_scan_local_io_error_not_destination_unreachable() {
        let mut stats = SyncStats::default();
        let io_err = SyncError::Io(std::io::Error::from(std::io::ErrorKind::PermissionDenied));

        if io_err.is_network_offline() {
            stats.network_offline = true;
        }

        assert!(
            !stats.network_offline,
            "Local permission denied must NOT mark network offline"
        );
    }
}

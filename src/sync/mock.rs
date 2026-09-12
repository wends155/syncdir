use crate::error::SyncError;
use crate::sync::engine::{
    ArchiveEngine, BatchFlusher, FileDeleter, FileSynchronizer, ScanEngine, ScanOutcome,
    SyncEngine, SyncStatusObserver,
};
use crate::sync::full_scan::FullScanDriver;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

type SyncErrorFactory = std::sync::Arc<dyn Fn() -> SyncError + Send + Sync>;
type SyncHandler = std::sync::Arc<dyn Fn(&Path) -> Result<(), SyncError> + Send + Sync>;

/// Thread-safe mock implementation of `SyncEngine` for isolated unit testing.
#[derive(Clone, Default)]
pub struct MockSyncEngine {
    synced_calls: std::sync::Arc<std::sync::Mutex<Vec<(PathBuf, PathBuf)>>>,
    deleted_calls: std::sync::Arc<std::sync::Mutex<Vec<(PathBuf, PathBuf)>>>,
    failed_calls: std::sync::Arc<std::sync::Mutex<Vec<(PathBuf, String)>>>,
    prune_calls: std::sync::Arc<std::sync::Mutex<Vec<PathBuf>>>,
    full_scans: std::sync::Arc<std::sync::Mutex<usize>>,
    sync_error_fn: std::sync::Arc<std::sync::Mutex<Option<SyncErrorFactory>>>,
    sync_handler: std::sync::Arc<std::sync::Mutex<Option<SyncHandler>>>,
    delete_handler: std::sync::Arc<std::sync::Mutex<Option<SyncHandler>>>,
    scan_outcome: std::sync::Arc<std::sync::Mutex<Option<ScanOutcome>>>,
}

impl std::fmt::Debug for MockSyncEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MockSyncEngine")
            .field("synced_calls", &self.synced_calls)
            .field("deleted_calls", &self.deleted_calls)
            .field("failed_calls", &self.failed_calls)
            .field("prune_calls", &self.prune_calls)
            .field("full_scans", &self.full_scans)
            .field("scan_outcome", &self.scan_outcome)
            .finish()
    }
}

impl MockSyncEngine {
    /// Create a new empty mock sync engine.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set an error factory to be returned by future sync and delete operations.
    pub fn set_sync_error(&self, error_fn: impl Fn() -> SyncError + Send + Sync + 'static) {
        let mut err = self.sync_error_fn.lock().unwrap_or_else(|p| p.into_inner());
        *err = Some(std::sync::Arc::new(error_fn));
    }

    /// Clear configured sync error factory.
    pub fn clear_sync_error(&self) {
        let mut err = self.sync_error_fn.lock().unwrap_or_else(|p| p.into_inner());
        *err = None;
    }

    /// Set a dynamic handler returning Result<(), SyncError> on sync calls.
    pub fn set_sync_handler(
        &self,
        handler: impl Fn(&Path) -> Result<(), SyncError> + Send + Sync + 'static,
    ) {
        let mut h = self.sync_handler.lock().unwrap_or_else(|p| p.into_inner());
        *h = Some(std::sync::Arc::new(handler));
    }

    /// Clear configured sync handler.
    pub fn clear_sync_handler(&self) {
        let mut h = self.sync_handler.lock().unwrap_or_else(|p| p.into_inner());
        *h = None;
    }

    /// Set a dynamic handler returning Result<(), SyncError> on delete calls.
    pub fn set_delete_handler(
        &self,
        handler: impl Fn(&Path) -> Result<(), SyncError> + Send + Sync + 'static,
    ) {
        let mut h = self
            .delete_handler
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        *h = Some(std::sync::Arc::new(handler));
    }

    /// Clear configured delete handler.
    pub fn clear_delete_handler(&self) {
        let mut h = self
            .delete_handler
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        *h = None;
    }

    /// Set the scan outcome to be returned by `run_full_scan`.
    pub fn set_scan_outcome(&self, outcome: Option<ScanOutcome>) {
        let mut sc = self.scan_outcome.lock().unwrap_or_else(|p| p.into_inner());
        *sc = outcome;
    }

    /// Return recorded (rel_path, dest_dir) tuples for `sync_file` calls.
    pub fn synced_calls(&self) -> Vec<(PathBuf, PathBuf)> {
        self.synced_calls
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    /// Return recorded (rel_path, dest_dir) tuples for `delete_file` calls.
    pub fn deleted_calls(&self) -> Vec<(PathBuf, PathBuf)> {
        self.deleted_calls
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    /// Return recorded failed calls with path and error description.
    pub fn failed_calls(&self) -> Vec<(PathBuf, String)> {
        self.failed_calls
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    /// Return recorded `prune_archive` destination directories.
    pub fn prune_archive_calls(&self) -> Vec<PathBuf> {
        self.prune_calls
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    /// Return recorded `prune_archive` destination directories (alias).
    pub fn prune_calls(&self) -> Vec<PathBuf> {
        self.prune_archive_calls()
    }

    /// Return count of `run_full_scan` calls.
    pub fn full_scans_count(&self) -> usize {
        *self.full_scans.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Synchronize a file using MockSyncEngine.
    pub fn sync_file(&self, path: &Path) -> Result<(), SyncError> {
        FileSynchronizer::sync_file(self, path)
    }

    /// Synchronize a file to destination using MockSyncEngine.
    pub fn sync_file_to_dest_buffered(
        &self,
        path: &Path,
        dest_dir: &Path,
        scratch: &mut [u8],
    ) -> Result<(), SyncError> {
        FileSynchronizer::sync_file_to_dest_buffered(self, path, dest_dir, scratch)
    }

    /// Delete a file using MockSyncEngine.
    pub fn delete_file(&self, path: &Path) -> Result<(), SyncError> {
        FileDeleter::delete_file(self, path)
    }

    /// Delete a file from destination using MockSyncEngine.
    pub fn delete_file_from_dest(&self, path: &Path, dest_dir: &Path) -> Result<(), SyncError> {
        FileDeleter::delete_file_from_dest(self, path, dest_dir)
    }

    /// Prune archive using MockSyncEngine.
    pub fn prune_archive(&self, dest_dir: &Path) -> Result<(), SyncError> {
        ArchiveEngine::prune_archive(self, dest_dir)
    }

    /// Run full scan using MockSyncEngine.
    pub fn run_full_scan(&self, dest_dir: &Path) -> Result<ScanOutcome, SyncError> {
        ScanEngine::run_full_scan(self, dest_dir)
    }

    /// Run cancellable full scan using MockSyncEngine.
    pub fn run_cancellable_full_scan(
        &self,
        dest_dir: &Path,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> Result<ScanOutcome, SyncError> {
        ScanEngine::run_cancellable_full_scan(self, dest_dir, cancel)
    }
}

impl FileSynchronizer for MockSyncEngine {
    fn sync_file(&self, path: &Path) -> Result<(), SyncError> {
        self.sync_file_to_dest_buffered(path, Path::new(""), &mut [])
    }

    fn sync_file_to_dest_buffered(
        &self,
        path: &Path,
        dest_dir: &Path,
        _scratch: &mut [u8],
    ) -> Result<(), SyncError> {
        let res: Result<(), SyncError> = (|| {
            let handler = self
                .sync_handler
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .clone();
            if let Some(h) = handler {
                h(path)?;
            } else {
                let err_fn = self
                    .sync_error_fn
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .clone();
                if let Some(f) = err_fn {
                    return Err(f());
                }
            }
            Ok(())
        })();

        match res {
            Ok(()) => {
                self.synced_calls
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .push((path.to_path_buf(), dest_dir.to_path_buf()));
                Ok(())
            }
            Err(e) => {
                self.failed_calls
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .push((path.to_path_buf(), e.to_string()));
                Err(e)
            }
        }
    }

    fn sync_file_to_dest_staged(
        &self,
        path: &Path,
        dest_dir: &Path,
        scratch: &mut [u8],
    ) -> Result<(), SyncError> {
        self.sync_file_to_dest_buffered(path, dest_dir, scratch)
    }
}

impl FileDeleter for MockSyncEngine {
    fn delete_file(&self, path: &Path) -> Result<(), SyncError> {
        self.delete_file_from_dest(path, Path::new(""))
    }

    fn delete_file_from_dest(&self, path: &Path, dest_dir: &Path) -> Result<(), SyncError> {
        let res: Result<(), SyncError> = (|| {
            let handler = self
                .delete_handler
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .clone();
            if let Some(h) = handler {
                h(path)?;
            } else {
                let err_fn = self
                    .sync_error_fn
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .clone();
                if let Some(f) = err_fn {
                    return Err(f());
                }
            }
            Ok(())
        })();

        match res {
            Ok(()) => {
                self.deleted_calls
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .push((path.to_path_buf(), dest_dir.to_path_buf()));
                Ok(())
            }
            Err(e) => {
                self.failed_calls
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .push((path.to_path_buf(), e.to_string()));
                Err(e)
            }
        }
    }
}

impl BatchFlusher for MockSyncEngine {
    fn flush_staged_syncs(&self) -> Result<(), SyncError> {
        Ok(())
    }
}

impl ScanEngine for MockSyncEngine {
    fn run_cancellable_full_scan(
        &self,
        _dest_dir: &Path,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> Result<ScanOutcome, SyncError> {
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(SyncError::Cancelled);
        }
        *self.full_scans.lock().unwrap_or_else(|p| p.into_inner()) += 1;
        let err_fn = self
            .sync_error_fn
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        if let Some(f) = err_fn {
            return Err(f());
        }
        if let Some(outcome) = self
            .scan_outcome
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
        {
            return Ok(outcome);
        }
        Ok(ScanOutcome::Success { synced: 0 })
    }

    fn run_full_scan(&self, dest_dir: &Path) -> Result<ScanOutcome, SyncError> {
        static NEVER_CANCELLED: std::sync::atomic::AtomicBool =
            std::sync::atomic::AtomicBool::new(false);
        self.run_cancellable_full_scan(dest_dir, &NEVER_CANCELLED)
    }
}

impl ArchiveEngine for MockSyncEngine {
    fn prune_archive(&self, dest_dir: &Path) -> Result<(), SyncError> {
        let err_fn = self
            .sync_error_fn
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        if let Some(f) = err_fn {
            return Err(f());
        }
        self.prune_calls
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(dest_dir.to_path_buf());
        Ok(())
    }
}

impl SyncEngine for MockSyncEngine {}

/// Thread-safe mock implementation of `SyncStatusObserver` for worker state machine testing.
#[derive(Default, Clone)]
pub struct MockSyncStatusObserver {
    target_statuses: std::sync::Arc<std::sync::Mutex<Vec<(usize, crate::sync::ConnectivityState)>>>,
    watcher_statuses: std::sync::Arc<
        std::sync::Mutex<Vec<(crate::sync::ConnectivityState, crate::sync::WatcherState)>>,
    >,
    write_verification_failures: std::sync::Arc<std::sync::Mutex<Vec<std::path::PathBuf>>>,
}

impl MockSyncStatusObserver {
    /// Create a new empty `MockSyncStatusObserver`.
    pub fn new() -> Self {
        Self::default()
    }

    /// Return recorded target connectivity status transitions.
    pub fn target_statuses(&self) -> Vec<(usize, crate::sync::ConnectivityState)> {
        self.target_statuses
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    /// Return recorded watcher status transitions.
    pub fn watcher_statuses(
        &self,
    ) -> Vec<(crate::sync::ConnectivityState, crate::sync::WatcherState)> {
        self.watcher_statuses
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    /// Return recorded write verification failure paths.
    pub fn write_verification_failures(&self) -> Vec<std::path::PathBuf> {
        self.write_verification_failures
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }
}

impl SyncStatusObserver for MockSyncStatusObserver {
    fn on_target_status_change(&self, target_index: usize, state: crate::sync::ConnectivityState) {
        self.target_statuses
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push((target_index, state));
    }

    fn on_watcher_status_change(
        &self,
        source: crate::sync::ConnectivityState,
        watcher: crate::sync::WatcherState,
    ) {
        self.watcher_statuses
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push((source, watcher));
    }

    fn on_write_verification_failed(&self, path: &std::path::Path) {
        self.write_verification_failures
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(path.to_path_buf());
    }
}

/// Pure in-memory driver for [`crate::sync::full_scan::FullScanCoordinator`].
#[derive(Clone, Debug)]
pub struct MockFullScanDriver {
    source_files: std::sync::Arc<std::sync::Mutex<HashSet<PathBuf>>>,
    scan_complete: std::sync::Arc<std::sync::atomic::AtomicBool>,
    cached_records: std::sync::Arc<std::sync::Mutex<HashMap<PathBuf, crate::db::FileRecord>>>,
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
            source_files: std::sync::Arc::new(std::sync::Mutex::new(HashSet::new())),
            scan_complete: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
            cached_records: std::sync::Arc::new(std::sync::Mutex::new(HashMap::new())),
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

impl MockFullScanDriver {
    /// Create a new mock full scan driver with default settings.
    pub fn new() -> Self {
        Self::default()
    }

    /// Populate the mock source files discovered during scanning.
    pub fn with_source_files(self, files: impl IntoIterator<Item = PathBuf>) -> Self {
        let mut sf = self.source_files.lock().unwrap_or_else(|p| p.into_inner());
        sf.extend(files);
        drop(sf);
        self
    }

    /// Set whether the source directory scan completes fully.
    pub fn with_scan_complete(self, complete: bool) -> Self {
        self.scan_complete.store(complete, Ordering::Relaxed);
        self
    }

    /// Populate cached database records.
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
            cr.insert(path, rec);
        }
        drop(cr);
        self
    }

    /// Configure whether deletions are propagated.
    pub fn with_propagate_deletions(mut self, propagate: bool) -> Self {
        self.propagate_deletions = propagate;
        self
    }

    /// Configure whether active destination is unreachable.
    pub fn with_dest_unreachable(mut self, unreachable: bool) -> Self {
        self.dest_unreachable = unreachable;
        self
    }

    /// Get recorded synced (rel_path, dest_dir) calls.
    pub fn synced_files(&self) -> Vec<(PathBuf, PathBuf)> {
        self.synced_files
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    /// Get recorded deleted paths.
    pub fn deleted_files(&self) -> Vec<PathBuf> {
        self.deleted_files
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    /// Get recorded archived paths.
    pub fn archived_files(&self) -> Vec<PathBuf> {
        self.archived_files
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    /// Get recorded pruned destination directories.
    pub fn pruned_archives(&self) -> Vec<PathBuf> {
        self.pruned_archives
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }
}

impl FullScanDriver for MockFullScanDriver {
    fn resolve_active_destination(&self, dest_dir: &Path) -> Result<Option<PathBuf>, SyncError> {
        if self.dest_unreachable {
            Ok(None)
        } else {
            Ok(Some(dest_dir.to_path_buf()))
        }
    }

    fn scan_source_files(
        &self,
        cancel: &AtomicBool,
    ) -> Result<(HashSet<PathBuf>, bool), SyncError> {
        if cancel.load(Ordering::Relaxed) {
            return Err(SyncError::Cancelled);
        }
        let files = self
            .source_files
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        let complete = self.scan_complete.load(Ordering::Relaxed);
        Ok((files, complete))
    }

    fn load_cached_records(&self) -> Result<HashMap<PathBuf, crate::db::FileRecord>, SyncError> {
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

    fn archive_dest_file_only(&self, rel_path: &Path, _dest_dir: &Path) -> Result<(), SyncError> {
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
            cached.remove(*p);
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn test_mock_sync_engine_recording_and_errors() {
        let mock = MockSyncEngine::new();
        let mut scratch = [0u8; 64];
        let p1 = Path::new("a.txt");
        let d1 = Path::new("C:\\dest1");

        assert!(
            mock.sync_file_to_dest_buffered(p1, d1, &mut scratch)
                .is_ok()
        );
        assert_eq!(
            mock.synced_calls(),
            vec![(p1.to_path_buf(), d1.to_path_buf())]
        );

        let p2 = Path::new("b.txt");
        let d2 = Path::new("C:\\dest2");
        assert!(mock.delete_file_from_dest(p2, d2).is_ok());
        assert_eq!(
            mock.deleted_calls(),
            vec![(p2.to_path_buf(), d2.to_path_buf())]
        );

        mock.set_sync_error(|| SyncError::validation("simulated failure"));
        assert!(
            mock.sync_file_to_dest_buffered(p1, d1, &mut scratch)
                .is_err()
        );
        assert!(mock.delete_file_from_dest(p2, d2).is_err());
        assert_eq!(
            mock.failed_calls(),
            vec![
                (
                    p1.to_path_buf(),
                    "Validation error: simulated failure".to_string()
                ),
                (
                    p2.to_path_buf(),
                    "Validation error: simulated failure".to_string()
                ),
            ]
        );

        mock.clear_sync_error();
        assert!(
            mock.sync_file_to_dest_buffered(p1, d1, &mut scratch)
                .is_ok()
        );

        assert_eq!(mock.full_scans_count(), 0);
        let outcome = mock.run_full_scan(d1).unwrap();
        assert_eq!(outcome, ScanOutcome::Success { synced: 0 });
        assert_eq!(mock.full_scans_count(), 1);

        assert!(mock.prune_archive(d1).is_ok());
        assert_eq!(mock.prune_archive_calls(), vec![d1.to_path_buf()]);
        assert_eq!(mock.prune_calls(), vec![d1.to_path_buf()]);
    }

    #[test]
    fn test_mock_sync_status_observer_recording() {
        let observer = MockSyncStatusObserver::new();
        observer.on_target_status_change(0, crate::sync::ConnectivityState::Online);
        observer.on_write_verification_failed(std::path::Path::new("file.txt"));

        assert_eq!(
            observer.target_statuses(),
            vec![(0, crate::sync::ConnectivityState::Online)]
        );
        assert_eq!(
            observer.write_verification_failures(),
            vec![PathBuf::from("file.txt")]
        );
    }

    #[test]
    fn test_mock_sync_engine_role_trait_implementations() {
        use crate::sync::engine::{
            ArchiveEngine, BatchFlusher, FileDeleter, FileSynchronizer, ScanEngine, SyncEngine,
        };
        let engine = MockSyncEngine::new();
        let _: &dyn FileSynchronizer = &engine;
        let _: &dyn FileDeleter = &engine;
        let _: &dyn BatchFlusher = &engine;
        let _: &dyn ScanEngine = &engine;
        let _: &dyn ArchiveEngine = &engine;
        let _: &dyn SyncEngine = &engine;
    }

    #[test]
    fn test_mock_full_scan_driver_in_memory() {
        let driver = MockFullScanDriver::new()
            .with_source_files(vec![PathBuf::from("a.txt")])
            .with_cached_records(vec![(PathBuf::from("b.txt"), 50, 1000)])
            .with_scan_complete(true);
        let cancel = AtomicBool::new(false);
        let (files, complete) = driver.scan_source_files(&cancel).unwrap();
        assert!(complete);
        assert_eq!(files.len(), 1);
        assert!(files.contains(&PathBuf::from("a.txt")));
        let records = driver.load_cached_records().unwrap();
        assert_eq!(records.len(), 1);
        assert!(records.contains_key(&PathBuf::from("b.txt")));
    }
}

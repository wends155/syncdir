use crate::error::SyncError;
use crate::sync::engine::{ScanOutcome, SyncEngine};
use std::path::{Path, PathBuf};

type SyncErrorFactory = std::sync::Arc<dyn Fn() -> SyncError + Send + Sync>;
type SyncHandler = std::sync::Arc<dyn Fn(&Path) -> Result<(), SyncError> + Send + Sync>;

/// Thread-safe mock implementation of `SyncEngine` for isolated unit testing.
#[derive(Clone, Default)]
pub struct MockSyncEngine {
    synced_calls: std::sync::Arc<std::sync::Mutex<Vec<(PathBuf, PathBuf)>>>,
    deleted_calls: std::sync::Arc<std::sync::Mutex<Vec<(PathBuf, PathBuf)>>>,
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
        let mut err = self.sync_error_fn.lock().unwrap();
        *err = Some(std::sync::Arc::new(error_fn));
    }

    /// Clear configured sync error factory.
    pub fn clear_sync_error(&self) {
        let mut err = self.sync_error_fn.lock().unwrap();
        *err = None;
    }

    /// Set a dynamic handler returning Result<(), SyncError> on sync calls.
    pub fn set_sync_handler(
        &self,
        handler: impl Fn(&Path) -> Result<(), SyncError> + Send + Sync + 'static,
    ) {
        let mut h = self.sync_handler.lock().unwrap();
        *h = Some(std::sync::Arc::new(handler));
    }

    /// Clear configured sync handler.
    pub fn clear_sync_handler(&self) {
        let mut h = self.sync_handler.lock().unwrap();
        *h = None;
    }

    /// Set a dynamic handler returning Result<(), SyncError> on delete calls.
    pub fn set_delete_handler(
        &self,
        handler: impl Fn(&Path) -> Result<(), SyncError> + Send + Sync + 'static,
    ) {
        let mut h = self.delete_handler.lock().unwrap();
        *h = Some(std::sync::Arc::new(handler));
    }

    /// Clear configured delete handler.
    pub fn clear_delete_handler(&self) {
        let mut h = self.delete_handler.lock().unwrap();
        *h = None;
    }

    /// Set the scan outcome to be returned by `run_full_scan`.
    pub fn set_scan_outcome(&self, outcome: Option<ScanOutcome>) {
        let mut sc = self.scan_outcome.lock().unwrap();
        *sc = outcome;
    }

    /// Return recorded (rel_path, dest_dir) tuples for `sync_file` calls.
    pub fn synced_calls(&self) -> Vec<(PathBuf, PathBuf)> {
        self.synced_calls.lock().unwrap().clone()
    }

    /// Return recorded (rel_path, dest_dir) tuples for `delete_file` calls.
    pub fn deleted_calls(&self) -> Vec<(PathBuf, PathBuf)> {
        self.deleted_calls.lock().unwrap().clone()
    }

    /// Return count of `run_full_scan` calls.
    pub fn full_scans_count(&self) -> usize {
        *self.full_scans.lock().unwrap()
    }
}

impl SyncEngine for MockSyncEngine {
    fn sync_file(&self, path: &Path) -> Result<(), SyncError> {
        self.sync_file_to_dest_buffered(path, Path::new(""), &mut [])
    }

    fn sync_file_buffered(&self, path: &Path, scratch: &mut [u8]) -> Result<(), SyncError> {
        self.sync_file_to_dest_buffered(path, Path::new(""), scratch)
    }

    fn sync_file_to_dest_buffered(
        &self,
        path: &Path,
        dest_dir: &Path,
        _scratch: &mut [u8],
    ) -> Result<(), SyncError> {
        let handler = self.sync_handler.lock().unwrap().clone();
        if let Some(h) = handler {
            h(path)?;
        } else {
            let err_fn = self.sync_error_fn.lock().unwrap().clone();
            if let Some(f) = err_fn {
                return Err(f());
            }
        }
        self.synced_calls
            .lock()
            .unwrap()
            .push((path.to_path_buf(), dest_dir.to_path_buf()));
        Ok(())
    }

    fn delete_file(&self, path: &Path) -> Result<(), SyncError> {
        self.delete_file_from_dest(path, Path::new(""))
    }

    fn delete_file_from_dest(&self, path: &Path, dest_dir: &Path) -> Result<(), SyncError> {
        let handler = self.delete_handler.lock().unwrap().clone();
        if let Some(h) = handler {
            h(path)?;
        } else {
            let err_fn = self.sync_error_fn.lock().unwrap().clone();
            if let Some(f) = err_fn {
                return Err(f());
            }
        }
        self.deleted_calls
            .lock()
            .unwrap()
            .push((path.to_path_buf(), dest_dir.to_path_buf()));
        Ok(())
    }

    fn prune_archive(&self, _dest_dir: &Path) -> Result<(), SyncError> {
        Ok(())
    }

    fn run_cancellable_full_scan(
        &self,
        _dest_dir: &Path,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> Result<ScanOutcome, SyncError> {
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(SyncError::Cancelled);
        }
        *self.full_scans.lock().unwrap() += 1;
        let err_fn = self.sync_error_fn.lock().unwrap().clone();
        if let Some(f) = err_fn {
            return Err(f());
        }
        if let Some(outcome) = self.scan_outcome.lock().unwrap().clone() {
            return Ok(outcome);
        }
        Ok(ScanOutcome::Success { synced: 0 })
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

        mock.clear_sync_error();
        assert!(
            mock.sync_file_to_dest_buffered(p1, d1, &mut scratch)
                .is_ok()
        );

        assert_eq!(mock.full_scans_count(), 0);
        let outcome = mock.run_full_scan(d1).unwrap();
        assert_eq!(outcome, ScanOutcome::Success { synced: 0 });
        assert_eq!(mock.full_scans_count(), 1);
    }
}

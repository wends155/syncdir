//! Synchronization traits, role interfaces, and observer contracts.

use crate::error::SyncError;
use std::path::Path;

pub use super::types::{FileMetadataSnapshot, FileSyncTask, RelativePath};

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
pub trait SyncStatusObserver: Send + Sync + 'static {
    /// Notification when target connectivity changes.
    fn on_target_status_change(&self, target_index: usize, state: ConnectivityState);
    /// Forward source directory connectivity and watcher active status to observers.
    fn on_watcher_status_change(&self, _source: ConnectivityState, _watcher: WatcherState) {}
    /// Notification when a file's write verification permanently fails after retries.
    fn on_write_verification_failed(&self, _path: &Path) {}
}

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
pub trait SyncEngine:
    FileSynchronizer + FileDeleter + BatchFlusher + ScanEngine + ArchiveEngine
{
    /// Invalidate any cached directory metadata (e.g. reparse point and ancestor junction checks).
    fn invalidate_verified_dirs(&self) {}
}

/// Interface for handling file and directory casing alignment on case-insensitive filesystems.
pub trait CasingAligner: Send + Sync {
    fn align_casing_if_needed(&self, dest_dir: &Path, rel_path: &Path) -> Result<(), SyncError>;
}

/// Interface for managing file versioning and deletion archiving.
pub trait ArchiveManager: Send + Sync {
    fn archive_file(&self, dest_dir: &Path, rel_path: &Path) -> Result<(), SyncError>;
    fn prune(&self, dest_dir: &Path) -> Result<(), SyncError>;
}

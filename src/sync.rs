//! Sync command types and engine trait for syncdir.
//!
//! Defines the message types that the monitor and tray threads
//! send to the sync worker, and the trait contract for the sync engine.

use std::path::PathBuf;

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

// STUB(Phase 2): Replace with actual delta sync calculator.
// Contract: trait SyncEngine — sync_file(&self, path: &str) -> Result<(), SyncError>
/// Core sync execution contract. Implemented by the delta sync engine.
pub trait SyncEngine {
    /// Synchronize a single file from source to destination.
    fn sync_file(&self, path: &str) -> Result<(), crate::error::SyncError>;
    /// Handle deletion of a file (archive on destination).
    fn delete_file(&self, path: &str) -> Result<(), crate::error::SyncError>;
    /// Perform a full directory scan and sync all changed files.
    fn run_full_scan(&self) -> Result<(), crate::error::SyncError>;
}

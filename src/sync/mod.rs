//! Synchronization subsystem for syncdir.
//!
//! Provides the core traits, file scanning, small-file copy, block-level delta
//! transfer, archive management, path traversal safety checks, and background worker loop.

pub mod archive;
pub mod delta;
pub mod engine;
pub mod mock;
pub mod path_safety;
pub mod scanner;
pub mod small_file;
pub mod worker;

pub use delta::DirtyBlockRange;
pub use engine::{
    ConnectivityState, FileMetadataSnapshot, LocalSyncEngine, ScanOutcome, SyncCommand, SyncEngine,
    SyncStatusObserver, WatcherState, is_metadata_up_to_date_raw,
};
pub use mock::MockSyncEngine;
pub use path_safety::is_safe_relative_path;
#[doc(hidden)]
pub use path_safety::verify_destination_not_reparse_cached;
pub use worker::{
    DebounceQueue, ReachabilityMonitor, SourceConnectivityTracker, SyncWorkerContext,
    SyncWorkerState, calculate_exponential_backoff, start_sync_worker,
};

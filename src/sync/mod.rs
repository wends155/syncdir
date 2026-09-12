//! Synchronization subsystem for syncdir.
//!
//! Provides the core traits, file scanning, small-file copy, block-level delta
//! transfer, archive management, path traversal safety checks, and background worker loop.

pub(crate) mod archive;
pub(crate) mod delta;
pub(crate) mod engine;
pub(crate) mod full_scan;
pub(crate) mod mock;
pub(crate) mod path_safety;
pub(crate) mod scanner;
pub(crate) mod small_file;
pub mod types;
pub(crate) mod worker;

pub use full_scan::{FullScanCoordinator, FullScanDriver};

pub use delta::DirtyBlockRange;
pub use engine::{
    ArchiveEngine, BatchFlusher, ConnectivityState, FileDeleter, FileMetadataSnapshot,
    FileSynchronizer, LocalSyncEngine, ScanEngine, ScanOutcome, SyncCommand, SyncEngine,
    SyncStatusObserver, WatcherState,
};
pub use mock::{MockSyncEngine, MockSyncStatusObserver};
pub use path_safety::{ReparseCache, is_safe_relative_path};
#[allow(deprecated)]
pub use types::is_metadata_up_to_date_raw;
pub use types::{
    FileSyncTask, FileSyncTaskBuilder, safe_epoch_duration_millis, safe_modified_millis,
};
pub use worker::{
    SourceConnectivityTracker, SyncWorkerContext, SyncWorkerContextBuilder, start_sync_worker,
};

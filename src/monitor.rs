//! Directory monitoring for real-time file change detection.
//!
//! Uses the `notify` crate (wrapping Windows `ReadDirectoryChangesW`)
//! to watch the source directory and feed `SyncCommand`s to the sync worker.

use crate::config::Config;
use crate::error::SyncError;
use crate::sync::SyncCommand;
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::sync::mpsc::Sender;

/// Watches a source directory for file changes and deletions.
pub struct DirectoryWatcher {
    _watcher: RecommendedWatcher,
}

impl DirectoryWatcher {
    /// Start watching the configured source directory.
    ///
    /// File events are debounced by the sync worker, not here.
    ///
    /// # Errors
    /// Returns `SyncError::Watcher` if the OS watcher cannot be created
    /// or the directory cannot be watched.
    pub fn start(config: &Config, tx: Sender<SyncCommand>) -> Result<Self, SyncError> {
        let source = config.source_dir.clone();

        let mut watcher =
            notify::recommended_watcher(move |res: Result<Event, notify::Error>| match res {
                Ok(event) => {
                    for path in event.paths {
                        if let Ok(rel_path) = path.strip_prefix(&source) {
                            let rel = rel_path.to_path_buf();
                            match event.kind {
                                EventKind::Create(_) | EventKind::Modify(_) => {
                                    let _ = tx.send(SyncCommand::FileModified(rel));
                                }
                                EventKind::Remove(_) => {
                                    let _ = tx.send(SyncCommand::FileDeleted(rel));
                                }
                                _ => {}
                            }
                        }
                    }
                }
                Err(e) => {
                    tracing::error!(error = %e, "Watcher error");
                }
            })?;

        watcher.watch(&config.source_dir, RecursiveMode::Recursive)?;
        Ok(DirectoryWatcher { _watcher: watcher })
    }
}

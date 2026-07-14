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
                Ok(event) => match event.kind {
                    EventKind::Create(_)
                    | EventKind::Modify(notify::event::ModifyKind::Data(_))
                    | EventKind::Modify(notify::event::ModifyKind::Metadata(_))
                    | EventKind::Modify(notify::event::ModifyKind::Any) => {
                        for path in event.paths {
                            if let Ok(rel_path) = path.strip_prefix(&source) {
                                let _ = tx.send(SyncCommand::FileModified(rel_path.to_path_buf()));
                            }
                        }
                    }
                    EventKind::Remove(_) => {
                        for path in event.paths {
                            if let Ok(rel_path) = path.strip_prefix(&source) {
                                let _ = tx.send(SyncCommand::FileDeleted(rel_path.to_path_buf()));
                            }
                        }
                    }
                    EventKind::Modify(notify::event::ModifyKind::Name(rename_mode)) => {
                        match rename_mode {
                            notify::event::RenameMode::Both => {
                                if event.paths.len() == 2 {
                                    if let Ok(from_rel) = event.paths[0].strip_prefix(&source) {
                                        let _ = tx
                                            .send(SyncCommand::FileDeleted(from_rel.to_path_buf()));
                                    }
                                    if let Ok(to_rel) = event.paths[1].strip_prefix(&source) {
                                        let _ = tx
                                            .send(SyncCommand::FileModified(to_rel.to_path_buf()));
                                    }
                                } else {
                                    for path in event.paths {
                                        if let Ok(rel_path) = path.strip_prefix(&source) {
                                            let _ = tx.send(SyncCommand::FileModified(
                                                rel_path.to_path_buf(),
                                            ));
                                        }
                                    }
                                }
                            }
                            notify::event::RenameMode::From => {
                                for path in event.paths {
                                    if let Ok(rel_path) = path.strip_prefix(&source) {
                                        let _ = tx
                                            .send(SyncCommand::FileDeleted(rel_path.to_path_buf()));
                                    }
                                }
                            }
                            notify::event::RenameMode::To => {
                                for path in event.paths {
                                    if let Ok(rel_path) = path.strip_prefix(&source) {
                                        let _ = tx.send(SyncCommand::FileModified(
                                            rel_path.to_path_buf(),
                                        ));
                                    }
                                }
                            }
                            _ => {
                                if event.paths.len() == 2 {
                                    if let Ok(from_rel) = event.paths[0].strip_prefix(&source) {
                                        let _ = tx
                                            .send(SyncCommand::FileDeleted(from_rel.to_path_buf()));
                                    }
                                    if let Ok(to_rel) = event.paths[1].strip_prefix(&source) {
                                        let _ = tx
                                            .send(SyncCommand::FileModified(to_rel.to_path_buf()));
                                    }
                                } else {
                                    for path in event.paths {
                                        if let Ok(rel_path) = path.strip_prefix(&source) {
                                            let _ = tx.send(SyncCommand::FileModified(
                                                rel_path.to_path_buf(),
                                            ));
                                        }
                                    }
                                }
                            }
                        }
                    }
                    _ => {}
                },
                Err(e) => {
                    tracing::error!(error = %e, "Watcher error");
                }
            })?;

        watcher.watch(&config.source_dir, RecursiveMode::Recursive)?;
        Ok(DirectoryWatcher { _watcher: watcher })
    }
}

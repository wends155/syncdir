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
#[must_use = "dropping DirectoryWatcher immediately unregisters OS directory notifications"]
pub struct DirectoryWatcher {
    _watcher: RecommendedWatcher,
}

impl DirectoryWatcher {
    /// Starts watching the configured source directory.
    ///
    /// Hooks into the OS filesystem event notifications via `notify` to capture
    /// creation, modification, removal, and rename events. Converts these OS events
    /// to [`SyncCommand`] instances and forwards them onto the sync worker channel.
    ///
    /// # Arguments
    ///
    /// * `config` - Runtime configuration specifying the source directory to monitor.
    /// * `tx` - Sender channel handle to transmit [`SyncCommand`] messages to the background worker.
    ///
    /// # Returns
    ///
    /// Returns a new [`DirectoryWatcher`] instance holding the active OS file system hook handle.
    ///
    /// # Errors
    ///
    /// Returns [`SyncError::Watcher`] if the underlying watcher hook fails to initialize
    /// or if it fails to bind to the source directory path.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use std::sync::mpsc::channel;
    /// # use syncdir::config::Config;
    /// # use syncdir::monitor::DirectoryWatcher;
    /// # use std::path::PathBuf;
    /// # let config = Config::test_default(PathBuf::from("C:/source"), PathBuf::from("D:/dest"));
    /// let (tx, rx) = channel();
    /// let watcher = DirectoryWatcher::start(&config, tx)?;
    /// # Ok::<(), syncdir::error::SyncError>(())
    /// ```
    pub fn start(config: &Config, tx: Sender<SyncCommand>) -> Result<Self, SyncError> {
        let source = config.resolved_source_dir();
        let source_root = source.clone();

        let mut watcher =
            notify::recommended_watcher(move |res: Result<Event, notify::Error>| match res {
                Ok(event) => {
                    let send = |cmd: SyncCommand| -> bool {
                        if let Err(e) = tx.send(cmd) {
                            tracing::error!(
                                error = %e,
                                "Sync worker channel disconnected; watcher event dropped"
                            );
                            false
                        } else {
                            true
                        }
                    };
                    match event.kind {
                        EventKind::Create(_)
                        | EventKind::Modify(notify::event::ModifyKind::Data(_))
                        | EventKind::Modify(notify::event::ModifyKind::Metadata(_))
                        | EventKind::Modify(notify::event::ModifyKind::Any) => {
                            for path in event.paths {
                                if let Ok(rel_path) = path.strip_prefix(&source_root)
                                    && !send(SyncCommand::FileModified(rel_path.to_path_buf()))
                                {
                                    return;
                                }
                            }
                        }
                        EventKind::Remove(_) => {
                            for path in event.paths {
                                if let Ok(rel_path) = path.strip_prefix(&source_root)
                                    && !send(SyncCommand::FileDeleted(rel_path.to_path_buf()))
                                {
                                    return;
                                }
                            }
                        }
                        EventKind::Modify(notify::event::ModifyKind::Name(rename_mode)) => {
                            match rename_mode {
                                notify::event::RenameMode::Both => {
                                    if event.paths.len() == 2 {
                                        if let Ok(from_rel) =
                                            event.paths[0].strip_prefix(&source_root)
                                            && !send(SyncCommand::FileDeleted(
                                                from_rel.to_path_buf(),
                                            ))
                                        {
                                            return;
                                        }
                                        if let Ok(to_rel) =
                                            event.paths[1].strip_prefix(&source_root)
                                        {
                                            send(SyncCommand::FileModified(to_rel.to_path_buf()));
                                        }
                                    } else {
                                        for path in event.paths {
                                            if let Ok(rel_path) = path.strip_prefix(&source_root)
                                                && !send(SyncCommand::FileModified(
                                                    rel_path.to_path_buf(),
                                                ))
                                            {
                                                return;
                                            }
                                        }
                                    }
                                }
                                notify::event::RenameMode::From => {
                                    for path in event.paths {
                                        if let Ok(rel_path) = path.strip_prefix(&source_root)
                                            && !send(SyncCommand::FileDeleted(
                                                rel_path.to_path_buf(),
                                            ))
                                        {
                                            return;
                                        }
                                    }
                                }
                                notify::event::RenameMode::To => {
                                    for path in event.paths {
                                        if let Ok(rel_path) = path.strip_prefix(&source_root)
                                            && !send(SyncCommand::FileModified(
                                                rel_path.to_path_buf(),
                                            ))
                                        {
                                            return;
                                        }
                                    }
                                }
                                _ => {
                                    if event.paths.len() == 2 {
                                        if let Ok(from_rel) =
                                            event.paths[0].strip_prefix(&source_root)
                                            && !send(SyncCommand::FileDeleted(
                                                from_rel.to_path_buf(),
                                            ))
                                        {
                                            return;
                                        }
                                        if let Ok(to_rel) =
                                            event.paths[1].strip_prefix(&source_root)
                                        {
                                            send(SyncCommand::FileModified(to_rel.to_path_buf()));
                                        }
                                    } else {
                                        for path in event.paths {
                                            if let Ok(rel_path) = path.strip_prefix(&source_root)
                                                && !send(SyncCommand::FileModified(
                                                    rel_path.to_path_buf(),
                                                ))
                                            {
                                                return;
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                }
                Err(e) => {
                    tracing::error!(error = %e, "Watcher error");
                }
            })?;

        watcher.watch(&source, RecursiveMode::Recursive)?;
        Ok(DirectoryWatcher { _watcher: watcher })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::channel;
    use tempfile::tempdir;

    #[test]
    fn test_channel_disconnect() {
        let temp = tempdir().unwrap();
        let src = temp.path().join("src");
        let dst = temp.path().join("dst");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&dst).unwrap();

        let config = Config::test_default(src.clone(), dst);
        let (tx, rx) = channel();
        drop(rx); // Force channel disconnect

        let watcher = DirectoryWatcher::start(&config, tx).expect("watcher should start");

        // Trigger a file change event in the monitored directory
        std::fs::write(src.join("test.txt"), b"hello").unwrap();

        // Brief sleep to give the background notification thread time to run the callback
        std::thread::sleep(std::time::Duration::from_millis(200));

        drop(watcher);
    }
}

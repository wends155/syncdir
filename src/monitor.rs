//! Directory monitoring for real-time file change detection.
//!
//! Uses the `notify` crate (wrapping Windows `ReadDirectoryChangesW`)
//! to watch the source directory and feed `SyncCommand`s to the sync worker.

use crate::error::SyncError;
use crate::sync::SyncCommand;
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::path::Path;
use std::sync::mpsc::Sender;

/// Watches a source directory for file changes and deletions.
#[must_use = "dropping DirectoryWatcher immediately unregisters OS directory notifications"]
pub struct DirectoryWatcher {
    _watcher: RecommendedWatcher,
}

impl DirectoryWatcher {
    /// Starts watching the specified source directory.
    ///
    /// Hooks into the OS filesystem event notifications via `notify` to capture
    /// creation, modification, removal, and rename events. Converts these OS events
    /// to [`SyncCommand`] instances and forwards them onto the sync worker channel.
    ///
    /// # Arguments
    ///
    /// * `source_dir` - Path to the source directory to monitor.
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
    /// # use std::path::Path;
    /// # use syncdir::monitor::DirectoryWatcher;
    /// let (tx, rx) = channel();
    /// let watcher = DirectoryWatcher::start(Path::new("C:/source"), tx)?;
    /// # Ok::<(), syncdir::error::SyncError>(())
    /// ```
    pub fn start(source_dir: impl AsRef<Path>, tx: Sender<SyncCommand>) -> Result<Self, SyncError> {
        let source_root = source_dir.as_ref().to_path_buf();
        let source_clone = source_root.clone();

        let mut watcher =
            notify::recommended_watcher(move |res: Result<Event, notify::Error>| match res {
                Ok(event) => Self::dispatch_event(event, &source_clone, &tx),
                Err(e) => {
                    tracing::error!(error = %e, "Watcher error");
                }
            })?;

        watcher.watch(&source_root, RecursiveMode::Recursive)?;
        Ok(DirectoryWatcher { _watcher: watcher })
    }

    /// Dispatches a filesystem notification event, translating paths relative to `source_root`.
    pub(crate) fn dispatch_event(event: Event, source_root: &Path, tx: &Sender<SyncCommand>) {
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
                    if let Ok(rel_path) = path.strip_prefix(source_root) {
                        if rel_path.as_os_str().is_empty() {
                            continue;
                        }
                        if !send(SyncCommand::FileModified(rel_path.to_path_buf())) {
                            return;
                        }
                    }
                }
            }
            EventKind::Remove(_) => {
                for path in event.paths {
                    if let Ok(rel_path) = path.strip_prefix(source_root) {
                        if rel_path.as_os_str().is_empty() {
                            continue;
                        }
                        if !send(SyncCommand::FileDeleted(rel_path.to_path_buf())) {
                            return;
                        }
                    }
                }
            }
            EventKind::Modify(notify::event::ModifyKind::Name(rename_mode)) => match rename_mode {
                notify::event::RenameMode::Both => {
                    if event.paths.len() == 2 {
                        if let Ok(from_rel) = event.paths[0].strip_prefix(source_root)
                            && !from_rel.as_os_str().is_empty()
                            && !send(SyncCommand::FileDeleted(from_rel.to_path_buf()))
                        {
                            return;
                        }
                        if let Ok(to_rel) = event.paths[1].strip_prefix(source_root)
                            && !to_rel.as_os_str().is_empty()
                        {
                            send(SyncCommand::FileModified(to_rel.to_path_buf()));
                        }
                    } else {
                        for path in event.paths {
                            if let Ok(rel_path) = path.strip_prefix(source_root) {
                                if rel_path.as_os_str().is_empty() {
                                    continue;
                                }
                                if !send(SyncCommand::FileModified(rel_path.to_path_buf())) {
                                    return;
                                }
                            }
                        }
                    }
                }
                notify::event::RenameMode::From => {
                    for path in event.paths {
                        if let Ok(rel_path) = path.strip_prefix(source_root) {
                            if rel_path.as_os_str().is_empty() {
                                continue;
                            }
                            if !send(SyncCommand::FileDeleted(rel_path.to_path_buf())) {
                                return;
                            }
                        }
                    }
                }
                notify::event::RenameMode::To => {
                    for path in event.paths {
                        if let Ok(rel_path) = path.strip_prefix(source_root) {
                            if rel_path.as_os_str().is_empty() {
                                continue;
                            }
                            if !send(SyncCommand::FileModified(rel_path.to_path_buf())) {
                                return;
                            }
                        }
                    }
                }
                _ => {
                    if event.paths.len() == 2 {
                        if let Ok(from_rel) = event.paths[0].strip_prefix(source_root)
                            && !from_rel.as_os_str().is_empty()
                            && !send(SyncCommand::FileDeleted(from_rel.to_path_buf()))
                        {
                            return;
                        }
                        if let Ok(to_rel) = event.paths[1].strip_prefix(source_root)
                            && !to_rel.as_os_str().is_empty()
                        {
                            send(SyncCommand::FileModified(to_rel.to_path_buf()));
                        }
                    } else {
                        for path in event.paths {
                            if let Ok(rel_path) = path.strip_prefix(source_root) {
                                if rel_path.as_os_str().is_empty() {
                                    continue;
                                }
                                if !send(SyncCommand::FileModified(rel_path.to_path_buf())) {
                                    return;
                                }
                            }
                        }
                    }
                }
            },
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::mpsc::channel;
    use tempfile::tempdir;

    #[test]
    fn test_channel_disconnect() {
        let temp = tempdir().unwrap();
        let src = temp.path().join("src");
        let dst = temp.path().join("dst");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&dst).unwrap();

        let (tx, rx) = channel();
        drop(rx); // Force channel disconnect

        let watcher = DirectoryWatcher::start(&src, tx).expect("watcher should start");

        // Trigger a file change event in the monitored directory
        std::fs::write(src.join("test.txt"), b"hello").unwrap();

        // Brief sleep to give the background notification thread time to run the callback
        std::thread::sleep(std::time::Duration::from_millis(200));

        drop(watcher);
    }

    #[test]
    fn test_watcher_does_not_forward_empty_relative_path() {
        let (tx, rx) = std::sync::mpsc::channel();
        let root = PathBuf::from(r"C:\test\source");

        let event = notify::Event {
            kind: notify::EventKind::Modify(notify::event::ModifyKind::Any),
            paths: vec![root.clone()],
            attrs: notify::event::EventAttributes::default(),
        };

        DirectoryWatcher::dispatch_event(event, &root, &tx);

        assert!(
            rx.try_recv().is_err(),
            "Events targeting the source root itself must not forward empty relative path to sync worker"
        );
    }
}

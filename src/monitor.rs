//! Directory monitoring for real-time file change detection.
//!
//! Uses the `notify` crate (wrapping Windows `ReadDirectoryChangesW`)
//! to watch the source directory and feed `SyncCommand`s to the sync worker.

use crate::error::SyncError;
pub use crate::error::WatcherError;
use crate::path_util::RelativePath;
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

        let mut watcher = notify::recommended_watcher(move |res: Result<Event, notify::Error>| {
            Self::handle_watcher_result(res, &source_clone, &tx);
        })?;

        watcher.watch(&source_root, RecursiveMode::Recursive)?;
        Ok(DirectoryWatcher { _watcher: watcher })
    }

    fn handle_watcher_result(
        res: Result<Event, notify::Error>,
        source_root: &Path,
        tx: &Sender<SyncCommand>,
    ) {
        match res {
            Ok(event) => Self::dispatch_event(event, source_root, tx),
            Err(e) => {
                tracing::error!(
                    error = %e,
                    "Directory watcher error; triggering full scan to recover potentially missed events"
                );
                if let Err(send_err) = tx.send(SyncCommand::TriggerFullScan) {
                    tracing::error!(
                        error = %send_err,
                        "Sync worker channel disconnected; full scan recovery command dropped"
                    );
                }
            }
        }
    }

    fn handle_rename_pair(
        from_path: &Path,
        to_path: &Path,
        source_root: &Path,
        send: &mut impl FnMut(SyncCommand) -> bool,
    ) -> bool {
        let safe_from = from_path
            .strip_prefix(source_root)
            .ok()
            .and_then(|r| RelativePath::new(r).ok());
        let safe_to = to_path
            .strip_prefix(source_root)
            .ok()
            .and_then(|r| RelativePath::new(r).ok());

        #[cfg(windows)]
        let is_case_only_rename = match (&safe_from, &safe_to) {
            (Some(from), Some(to)) => {
                from.as_path() != to.as_path()
                    && from
                        .as_path()
                        .to_string_lossy()
                        .eq_ignore_ascii_case(&to.as_path().to_string_lossy())
            }
            _ => false,
        };
        #[cfg(not(windows))]
        let is_case_only_rename = false;

        if is_case_only_rename {
            if let Some(safe_to) = safe_to {
                return send(SyncCommand::FileModified(safe_to));
            }
            return true;
        }

        if let Some(safe_from) = safe_from
            && !send(SyncCommand::FileDeleted(safe_from))
        {
            return false;
        }
        if let Some(safe_to) = safe_to
            && !send(SyncCommand::FileModified(safe_to))
        {
            return false;
        }
        true
    }

    /// Dispatches a filesystem notification event, translating paths relative to `source_root`.
    fn dispatch_event(event: Event, source_root: &Path, tx: &Sender<SyncCommand>) {
        let mut send = |cmd: SyncCommand| -> bool {
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
        let send_modified = |rel_path: &Path| -> bool {
            match RelativePath::new(rel_path) {
                Ok(safe_rel) => send(SyncCommand::FileModified(safe_rel)),
                Err(e) => {
                    tracing::warn!(
                        path = ?rel_path,
                        error = %e,
                        "Watcher observed unsafe or invalid relative path; skipping"
                    );
                    true
                }
            }
        };
        let send_deleted = |rel_path: &Path| -> bool {
            match RelativePath::new(rel_path) {
                Ok(safe_rel) => send(SyncCommand::FileDeleted(safe_rel)),
                Err(e) => {
                    tracing::warn!(
                        path = ?rel_path,
                        error = %e,
                        "Watcher observed unsafe or invalid relative path; skipping"
                    );
                    true
                }
            }
        };
        match event.kind {
            EventKind::Create(_)
            | EventKind::Modify(notify::event::ModifyKind::Data(_))
            | EventKind::Modify(notify::event::ModifyKind::Metadata(_))
            | EventKind::Modify(notify::event::ModifyKind::Any) => {
                for path in event.paths {
                    if let Ok(rel_path) = path.strip_prefix(source_root)
                        && !send_modified(rel_path)
                    {
                        return;
                    }
                }
            }
            EventKind::Remove(_) => {
                for path in event.paths {
                    if let Ok(rel_path) = path.strip_prefix(source_root)
                        && !send_deleted(rel_path)
                    {
                        return;
                    }
                }
            }
            EventKind::Modify(notify::event::ModifyKind::Name(rename_mode)) => match rename_mode {
                notify::event::RenameMode::Both => {
                    if event.paths.len() == 2 {
                        let _ = Self::handle_rename_pair(
                            &event.paths[0],
                            &event.paths[1],
                            source_root,
                            &mut send,
                        );
                    } else {
                        for path in event.paths {
                            if let Ok(rel_path) = path.strip_prefix(source_root)
                                && !send_modified(rel_path)
                            {
                                return;
                            }
                        }
                    }
                }
                notify::event::RenameMode::From => {
                    for path in event.paths {
                        if let Ok(rel_path) = path.strip_prefix(source_root)
                            && !send_deleted(rel_path)
                        {
                            return;
                        }
                    }
                }
                notify::event::RenameMode::To => {
                    for path in event.paths {
                        if let Ok(rel_path) = path.strip_prefix(source_root)
                            && !send_modified(rel_path)
                        {
                            return;
                        }
                    }
                }
                _ => {
                    if event.paths.len() == 2 {
                        let _ = Self::handle_rename_pair(
                            &event.paths[0],
                            &event.paths[1],
                            source_root,
                            &mut send,
                        );
                    } else {
                        for path in event.paths {
                            if let Ok(rel_path) = path.strip_prefix(source_root)
                                && !send_modified(rel_path)
                            {
                                return;
                            }
                        }
                    }
                }
            },
            _ => {}
        }
    }
}

/// Abstraction for filesystem watchers that observe directory changes.
pub trait FileWatcher: Send + 'static {
    /// Returns true if the watcher is actively monitoring filesystem events.
    fn is_watching(&self) -> bool;
}

impl FileWatcher for DirectoryWatcher {
    fn is_watching(&self) -> bool {
        true
    }
}

/// Factory trait for creating filesystem watchers.
pub trait WatcherFactory: Send + Sync + 'static {
    /// Creates a new filesystem watcher monitoring `source_dir` and sending commands to `tx`.
    fn create_watcher(
        &self,
        source_dir: &Path,
        tx: Sender<SyncCommand>,
    ) -> Result<Box<dyn FileWatcher>, SyncError>;
}

/// Default factory creating a `DirectoryWatcher` backed by notify.
#[derive(Debug, Clone, Copy, Default)]
pub struct RecommendedWatcherFactory;

impl WatcherFactory for RecommendedWatcherFactory {
    fn create_watcher(
        &self,
        source_dir: &Path,
        tx: Sender<SyncCommand>,
    ) -> Result<Box<dyn FileWatcher>, SyncError> {
        let watcher = DirectoryWatcher::start(source_dir, tx)?;
        Ok(Box::new(watcher))
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

    #[test]
    fn test_case_only_rename_windows_dispatches_file_modified() {
        let (tx, rx) = std::sync::mpsc::channel();
        let root = PathBuf::from(r"C:\test\source");

        let event = notify::Event {
            kind: notify::EventKind::Modify(notify::event::ModifyKind::Name(
                notify::event::RenameMode::Both,
            )),
            paths: vec![root.join("file.txt"), root.join("File.txt")],
            attrs: notify::event::EventAttributes::default(),
        };

        DirectoryWatcher::dispatch_event(event, &root, &tx);

        #[cfg(windows)]
        {
            let cmd = rx.try_recv().expect("Should have received a command");
            assert_eq!(
                cmd,
                SyncCommand::FileModified(RelativePath::new("File.txt").unwrap())
            );
            assert!(
                rx.try_recv().is_err(),
                "Should only dispatch one FileModified command"
            );
        }
        #[cfg(not(windows))]
        {
            let cmd1 = rx.try_recv().expect("Should have received first command");
            assert_eq!(
                cmd1,
                SyncCommand::FileDeleted(RelativePath::new("file.txt").unwrap())
            );
            let cmd2 = rx.try_recv().expect("Should have received second command");
            assert_eq!(
                cmd2,
                SyncCommand::FileModified(RelativePath::new("File.txt").unwrap())
            );
        }
    }

    #[test]
    fn test_directory_watcher_dispatches_full_scan_on_buffer_overflow() {
        let (tx, rx) = std::sync::mpsc::channel();
        let root = PathBuf::from(r"C:\test\source");

        let notify_err = notify::Error::generic("OS buffer overflow in ReadDirectoryChangesW");
        DirectoryWatcher::handle_watcher_result(Err(notify_err), &root, &tx);

        let cmd = rx.try_recv().expect("Should have received a SyncCommand");
        assert_eq!(cmd, SyncCommand::TriggerFullScan);
    }

    #[test]
    fn test_file_watcher_trait_mockability() {
        use crate::error::SyncError;
        use crate::sync::SyncCommand;
        use std::path::Path;
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::mpsc::Sender;

        struct MockWatcher {
            active: Arc<AtomicBool>,
        }
        impl FileWatcher for MockWatcher {
            fn is_watching(&self) -> bool {
                self.active.load(Ordering::SeqCst)
            }
        }

        struct MockWatcherFactory {
            active: Arc<AtomicBool>,
        }
        impl WatcherFactory for MockWatcherFactory {
            fn create_watcher(
                &self,
                _source_dir: &Path,
                _tx: Sender<SyncCommand>,
            ) -> Result<Box<dyn FileWatcher>, SyncError> {
                Ok(Box::new(MockWatcher {
                    active: self.active.clone(),
                }))
            }
        }

        let active = Arc::new(AtomicBool::new(true));
        let factory = MockWatcherFactory {
            active: active.clone(),
        };
        let (tx, _rx) = std::sync::mpsc::channel();
        let watcher = factory
            .create_watcher(Path::new("C:\\test"), tx)
            .expect("mock watcher");
        assert!(watcher.is_watching());
    }

    #[test]
    fn test_watcher_dispatch_malicious_crlf_path_prevents_log_injection() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let root = PathBuf::from(r"C:\test\src");
        let malicious_rel = Path::new("legit\r\n[CRITICAL] FORGED LOG ENTRY\r\nsub.txt");
        let event = notify::Event {
            kind: notify::EventKind::Create(notify::event::CreateKind::File),
            paths: vec![root.join(malicious_rel)],
            attrs: Default::default(),
        };
        let (_, log_output) = crate::test_support::with_captured_tracing(|| {
            DirectoryWatcher::dispatch_event(event, &root, &tx);
        });
        assert!(
            !log_output.contains("\r\n[CRITICAL] FORGED LOG ENTRY"),
            "Log output contained unescaped CRLF forged line: {log_output}"
        );
    }

    #[test]
    fn test_handle_rename_pair_case_only_dispatches_file_modified() {
        use crate::path_util::RelativePath;
        use crate::sync::engine::SyncCommand;
        use std::path::PathBuf;

        let (tx, rx) = std::sync::mpsc::channel();
        let root = PathBuf::from(r"C:\syncdir\source");
        let from_path = root.join("document.pdf");
        let to_path = root.join("Document.pdf");

        let event = notify::Event {
            kind: notify::EventKind::Modify(notify::event::ModifyKind::Name(
                notify::event::RenameMode::Both,
            )),
            paths: vec![from_path, to_path],
            attrs: notify::event::EventAttributes::default(),
        };

        DirectoryWatcher::dispatch_event(event, &root, &tx);

        #[cfg(windows)]
        {
            let cmd = rx
                .try_recv()
                .expect("Watcher must emit command for case-only rename");
            assert_eq!(
                cmd,
                SyncCommand::FileModified(RelativePath::new("Document.pdf").unwrap())
            );
            assert!(rx.try_recv().is_err());
        }
        #[cfg(not(windows))]
        {
            let cmd1 = rx.try_recv().expect("Unix must dispatch FileDeleted first");
            assert_eq!(
                cmd1,
                SyncCommand::FileDeleted(RelativePath::new("document.pdf").unwrap())
            );
            let cmd2 = rx
                .try_recv()
                .expect("Unix must dispatch FileModified second");
            assert_eq!(
                cmd2,
                SyncCommand::FileModified(RelativePath::new("Document.pdf").unwrap())
            );
        }
    }
}

//! Background sync daemon and tray action handling.
//!
//! Owns background daemon lifecycle, worker thread spawning, watcher event loops,
//! reconnect scan triggers, and RAII shutdown.

use crate::config::Config;
use crate::db::SqliteHashStore;
use crate::error::SyncError;
use crate::net::try_resolve_alternate_path;
use crate::startup::RegistryBackend;
use crate::sync::{
    LocalSyncEngine, SyncCommand, SyncStatusObserver, SyncWorkerContext, start_sync_worker,
};
use crate::tray::TrayActionHandler;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Sender, channel};
use std::thread::JoinHandle;

/// Open a file or directory in the system default application.
pub(crate) fn open_path(path: &Path) -> Result<(), SyncError> {
    if !path.exists() {
        return Err(SyncError::validation(format!(
            "Path does not exist: {}",
            path.display()
        )));
    }
    #[cfg(target_os = "windows")]
    {
        let explorer = crate::config::system_root().join("explorer.exe");
        let mut cmd = if explorer.exists() {
            std::process::Command::new(explorer)
        } else {
            std::process::Command::new("explorer")
        };
        cmd.arg(path).spawn().map_err(SyncError::Io)?;
    }
    #[cfg(not(target_os = "windows"))]
    {
        let opener = if cfg!(target_os = "macos") {
            "open"
        } else {
            "xdg-open"
        };
        std::process::Command::new(opener)
            .arg(path)
            .spawn()
            .map_err(SyncError::Io)?;
    }
    Ok(())
}

/// Handle for dispatching asynchronous control commands to a running `SyncDaemon`.
#[derive(Clone)]
pub struct DaemonHandle {
    command_tx: Sender<SyncCommand>,
}

impl DaemonHandle {
    /// Create a new daemon handle wrapping the given command sender.
    pub fn new(command_tx: Sender<SyncCommand>) -> Self {
        Self { command_tx }
    }

    /// Trigger an immediate full synchronization scan across all targets.
    pub fn trigger_full_scan(&self) -> Result<(), SyncError> {
        self.command_tx
            .send(SyncCommand::TriggerFullScan)
            .map_err(|e| SyncError::tray_with_source("Sync worker channel disconnected", e))
    }
}

/// Tray action handler connecting UI context menu callbacks to daemon and registry operations.
pub struct DaemonTrayHandler<R: RegistryBackend> {
    config_path: PathBuf,
    log_dir: PathBuf,
    handle: DaemonHandle,
    registry: R,
}

impl<R: RegistryBackend> DaemonTrayHandler<R> {
    /// Create a new tray handler with target config path, log directory, daemon handle, and registry backend.
    pub fn new(config_path: PathBuf, log_dir: PathBuf, handle: DaemonHandle, registry: R) -> Self {
        Self {
            config_path,
            log_dir,
            handle,
            registry,
        }
    }
}

impl<R: RegistryBackend + Send + Sync + 'static> TrayActionHandler for DaemonTrayHandler<R> {
    fn on_sync_now(&self) -> Result<(), SyncError> {
        self.handle.trigger_full_scan()
    }

    fn on_reload_config(&self) -> Result<(), SyncError> {
        let new_config = Config::load(&self.config_path)?;
        new_config.validate()?;
        Ok(())
    }

    fn on_toggle_startup(&self, enable: bool) -> Result<bool, SyncError> {
        if enable {
            self.registry.register()?;
            Ok(true)
        } else {
            self.registry.unregister()?;
            Ok(false)
        }
    }

    fn is_startup_enabled(&self) -> Result<bool, SyncError> {
        self.registry.is_registered()
    }

    fn on_open_config(&self) -> Result<(), SyncError> {
        open_path(&self.config_path)
    }

    fn on_view_logs(&self) -> Result<(), SyncError> {
        open_path(&self.log_dir)
    }
}

/// Orchestrator for syncdir background sync workers, file watcher, and central command broadcaster.
#[must_use = "dropping SyncDaemon immediately terminates all background sync workers"]
pub struct SyncDaemon {
    config: Config,
    watcher_handle: Option<JoinHandle<()>>,
    broadcaster_handle: Option<JoinHandle<()>>,
    worker_handles: Vec<JoinHandle<()>>,
    shutdown_flag: Arc<AtomicBool>,
    command_tx: Sender<SyncCommand>,
}

impl SyncDaemon {
    /// Starts all sync workers, the central directory watcher, and the central command broadcaster.
    ///
    /// Initializes isolated signature cache SQLite databases for each configured target directory,
    /// launches independent worker threads, wires the filesystem directory watcher on the source folder,
    /// and triggers an initial full synchronization scan.
    ///
    /// # Arguments
    ///
    /// * `config` - Validated [`Config`] specifying source and destination targets.
    /// * `app_dir` - Application data directory where SQLite database files reside.
    /// * `observer` - Optional status observer callback implementing [`SyncStatusObserver`].
    ///
    /// # Returns
    ///
    /// A running [`SyncDaemon`] handle containing background thread join handles and shutdown flags.
    ///
    /// # Errors
    ///
    /// Returns [`SyncError::Db`] if SQLite database initialization fails for any target.
    #[must_use = "dropping SyncDaemon immediately terminates all background sync workers"]
    pub fn start(
        config: Config,
        app_dir: &Path,
        observer: Option<Arc<dyn SyncStatusObserver>>,
    ) -> Result<Self, SyncError> {
        let mut config = config;
        config.resolve_network_paths();

        let shutdown_flag = Arc::new(AtomicBool::new(false));
        let mut worker_handles = Vec::new();

        let resolved_source = config.resolved_source_dir();
        let initial_source_online = resolved_source.exists() && resolved_source.is_dir();
        let source_online = Arc::new(AtomicBool::new(initial_source_online));

        // 1. Initialize target databases and workers
        let mut worker_txs = Vec::new();
        for (idx, target_config) in config.target_configs().into_iter().enumerate() {
            let dest = target_config.dest_dir.clone();

            // Calculate isolated SQLite database filename using Blake3 hash of the target path
            let dest_str = dest.to_string_lossy();
            let hash = blake3::hash(dest_str.as_bytes());
            let db_filename = format!("sigcache_{}.db", hash.to_hex());
            let db_path = app_dir.join(db_filename);

            tracing::info!(
                target_index = idx + 1,
                target_path = %dest.display(),
                db_path = %db_path.display(),
                "Opening signature cache database for target",
            );
            let store = SqliteHashStore::new(&db_path, &config)?;

            match std::fs::metadata(&dest) {
                Ok(meta) if meta.is_dir() => {
                    tracing::info!(
                        target_index = idx + 1,
                        target_path = %dest.display(),
                        "Target destination is online and reachable."
                    );
                }
                Ok(_) => {
                    tracing::warn!(
                        target_index = idx + 1,
                        target_path = %dest.display(),
                        "Target destination exists but is not a directory."
                    );
                }
                Err(e) => {
                    let alt_path = try_resolve_alternate_path(&dest);
                    if alt_path != dest
                        && matches!(std::fs::metadata(&alt_path), Ok(m) if m.is_dir())
                    {
                        tracing::info!(
                            target_index = idx + 1,
                            target_path = %dest.display(),
                            resolved_path = %alt_path.display(),
                            "Target destination resolved alternate mapped drive/UNC SMB path."
                        );
                    } else {
                        tracing::warn!(
                            target_index = idx + 1,
                            target_path = %dest.display(),
                            resolved_path = %alt_path.display(),
                            error = %e,
                            os_error = ?e.raw_os_error(),
                            "Target destination is currently offline or unreachable."
                        );
                    }
                }
            }

            // Wire per-worker channel
            let (w_tx, w_rx) = channel();
            worker_txs.push(w_tx);

            tracing::info!(
                target_index = idx + 1,
                target_path = %dest.display(),
                "Starting sync worker thread for target..."
            );
            let engine = LocalSyncEngine::new(store, target_config.clone());
            let worker_ctx = SyncWorkerContext::new(
                idx,
                target_config,
                engine,
                w_rx,
                observer.clone(),
                source_online.clone(),
            );
            let worker_handle = start_sync_worker(worker_ctx);
            worker_handles.push(worker_handle);
        }

        // 2. Central coordination channels and threads
        let (tx, rx) = channel();

        // Spawn central watcher coordinator thread
        let watcher_config = config.clone();
        let watcher_tx = tx.clone();
        let watcher_source_online = source_online.clone();
        let watcher_shutdown = shutdown_flag.clone();
        let watcher_observer = observer.clone();
        let watcher_handle = std::thread::spawn(move || {
            let mut watcher: Option<crate::monitor::DirectoryWatcher> = None;
            let retry_interval =
                std::time::Duration::from_secs(watcher_config.retry_interval_seconds());
            let mut last_status_check: Option<std::time::Instant> = None;

            let mut last_sent_online = None;
            let mut last_sent_active = None;

            while !watcher_shutdown.load(Ordering::Relaxed) {
                let now = std::time::Instant::now();
                let should_check = match last_status_check {
                    None => true,
                    Some(last) => now.duration_since(last) >= retry_interval,
                };

                if should_check {
                    last_status_check = Some(now);
                    let current_source = watcher_config.resolved_source_dir();
                    let is_online = current_source.exists() && current_source.is_dir();
                    watcher_source_online.store(is_online, Ordering::Relaxed);

                    let mut watcher_active = false;
                    if is_online {
                        if watcher.is_none() {
                            tracing::info!(
                                "Source directory online. Starting directory watcher..."
                            );
                            match crate::monitor::DirectoryWatcher::start(
                                watcher_config.resolved_source_dir(),
                                watcher_tx.clone(),
                            ) {
                                Ok(w) => {
                                    watcher = Some(w);
                                    watcher_active = true;
                                    // Trigger catch-up full scan on source reconnection/startup
                                    tracing::info!(
                                        "Triggering full scan after source directory came online."
                                    );
                                    let _ = watcher_tx.send(SyncCommand::TriggerFullScan);
                                }
                                Err(e) => {
                                    tracing::error!("Failed to start directory watcher: {e}");
                                    watcher_active = false;
                                }
                            }
                        } else {
                            watcher_active = true;
                        }
                    } else if watcher.is_some() {
                        tracing::warn!(
                            "Source directory went offline. Dropping directory watcher."
                        );
                        watcher = None;
                    }

                    if last_sent_online != Some(is_online)
                        || last_sent_active != Some(watcher_active)
                    {
                        last_sent_online = Some(is_online);
                        last_sent_active = Some(watcher_active);
                        if let Some(ref obs) = watcher_observer {
                            obs.on_watcher_status_change(is_online.into(), watcher_active.into());
                        }
                    }
                }

                for _ in 0..10 {
                    if watcher_shutdown.load(Ordering::Relaxed) {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
            }
        });

        // Spawn central broadcaster thread
        let broadcaster_shutdown = shutdown_flag.clone();
        let broadcaster_rx = rx;
        let mut worker_senders = worker_txs;
        let broadcaster_handle = std::thread::spawn(move || {
            while !broadcaster_shutdown.load(Ordering::Relaxed) {
                match broadcaster_rx.recv_timeout(std::time::Duration::from_millis(200)) {
                    Ok(mut cmd) => {
                        let count = worker_senders.len();
                        let mut failed = Vec::new();
                        for (i, tx) in worker_senders.iter().enumerate() {
                            let to_send = if i + 1 == count {
                                std::mem::replace(&mut cmd, SyncCommand::TriggerFullScan)
                            } else {
                                cmd.clone()
                            };
                            if tx.send(to_send).is_err() {
                                tracing::warn!(
                                    "Sync worker channel disconnected. Removing sender."
                                );
                                failed.push(i);
                            }
                        }
                        for &i in failed.iter().rev() {
                            worker_senders.swap_remove(i);
                        }
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                }
            }
        });

        Ok(Self {
            config,
            watcher_handle: Some(watcher_handle),
            broadcaster_handle: Some(broadcaster_handle),
            worker_handles,
            shutdown_flag,
            command_tx: tx,
        })
    }

    /// Create a handle for dispatching commands to this daemon.
    pub fn handle(&self) -> DaemonHandle {
        DaemonHandle::new(self.command_tx.clone())
    }

    /// Trigger an immediate full synchronization scan across all targets.
    pub fn trigger_full_scan(&self) -> Result<(), SyncError> {
        self.handle().trigger_full_scan()
    }

    /// Access the command sender for broadcasting commands into the daemon.
    pub fn command_tx(&self) -> Sender<SyncCommand> {
        self.command_tx.clone()
    }

    /// Access the underlying daemon configuration.
    pub fn config(&self) -> &Config {
        &self.config
    }

    /// Explicit shutdown joining all worker threads cleanly.
    pub fn shutdown(mut self) {
        self.perform_shutdown();
    }

    fn perform_shutdown(&mut self) {
        if !self.shutdown_flag.swap(true, Ordering::Relaxed) {
            tracing::info!("Shutting down SyncDaemon and all worker threads...");
            // Step 1: Join watcher thread first so no new events are generated
            if let Some(handle) = self.watcher_handle.take() {
                let _ = handle.join();
            }
            // Step 2: Join broadcaster thread so in-flight commands are distributed
            if let Some(handle) = self.broadcaster_handle.take() {
                let _ = handle.join();
            }
            // Step 3: Join all worker threads
            for handle in self.worker_handles.drain(..) {
                let _ = handle.join();
            }
            tracing::info!("SyncDaemon shutdown complete.");
        }
    }
}

impl Drop for SyncDaemon {
    fn drop(&mut self) {
        self.perform_shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::startup::MockStartupRegistry;
    use tempfile::tempdir;

    #[test]
    fn test_sync_daemon_start_and_shutdown() {
        let dir = tempdir().unwrap();
        let src = dir.path().join("source");
        let dst = dir.path().join("dest");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&dst).unwrap();

        let config = Config::builder(src).dest_dir(dst).build();
        let daemon = SyncDaemon::start(config, dir.path(), None).unwrap();
        assert_eq!(daemon.worker_handles.len(), 1);
        daemon.shutdown();
    }

    #[test]
    fn test_daemon_tray_handler_actions() {
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        std::fs::write(
            &config_path,
            r#"
source_dir = "C:\\dummy_source"
dest_dir = "C:\\dummy_dest"
"#,
        )
        .unwrap();

        let (tx, rx) = channel();
        let mock_registry = MockStartupRegistry::new(false);
        let handle = DaemonHandle::new(tx);
        let handler =
            DaemonTrayHandler::new(config_path, dir.path().join("logs"), handle, mock_registry);

        assert!(!handler.is_startup_enabled().unwrap());
        assert!(handler.on_toggle_startup(true).unwrap());
        assert!(handler.is_startup_enabled().unwrap());
        assert!(!handler.on_toggle_startup(false).unwrap());
        assert!(!handler.is_startup_enabled().unwrap());

        handler.on_sync_now().unwrap();
        let cmd = rx.try_recv().unwrap();
        assert_eq!(cmd, SyncCommand::TriggerFullScan);
    }

    #[test]
    fn test_sync_daemon_reconnect_triggers_full_scan() {
        let dir = tempdir().unwrap();
        let src = dir.path().join("source");
        let dst = dir.path().join("dest");
        std::fs::create_dir_all(&dst).unwrap();
        // Source initially missing
        let config = Config::builder(src.clone())
            .dest_dir(dst)
            .retry_interval_seconds(1)
            .build();
        let daemon = SyncDaemon::start(config, dir.path(), None).unwrap();

        // Now bring source online
        std::fs::create_dir_all(&src).unwrap();

        // Sleep past retry interval to allow coordinator loop to discover source
        std::thread::sleep(std::time::Duration::from_millis(1500));

        daemon.shutdown();
    }
}

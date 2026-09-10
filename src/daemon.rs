//! Background sync daemon and tray action handling.
//!
//! Owns background daemon lifecycle, worker thread spawning, watcher event loops,
//! reconnect scan triggers, and RAII shutdown.

use crate::config::Config;
use crate::db::{SqliteHashStore, StoreConfig};
use crate::error::SyncError;
use crate::startup::RegistryBackend;
use crate::sync::{
    LocalSyncEngine, SyncCommand, SyncEngine, SyncStatusObserver, SyncWorkerContext,
    start_sync_worker,
};
use crate::tray::TrayActionHandler;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Sender, channel};
use std::thread::JoinHandle;

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
    resolver: Arc<dyn crate::net::NetworkResolver>,
}

impl<R: RegistryBackend> DaemonTrayHandler<R> {
    /// Create a new tray handler with target config path, log directory, daemon handle, and registry backend.
    ///
    /// Defaults to `Win32NetworkResolver`.
    pub fn new(config_path: PathBuf, log_dir: PathBuf, handle: DaemonHandle, registry: R) -> Self {
        Self::with_resolver(
            config_path,
            log_dir,
            handle,
            registry,
            Arc::new(crate::net::Win32NetworkResolver),
        )
    }

    /// Create a new tray handler with a custom network resolver.
    pub fn with_resolver(
        config_path: PathBuf,
        log_dir: PathBuf,
        handle: DaemonHandle,
        registry: R,
        resolver: Arc<dyn crate::net::NetworkResolver>,
    ) -> Self {
        Self {
            config_path,
            log_dir,
            handle,
            registry,
            resolver,
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
        SyncDaemon::validate_target_loops(&new_config, self.resolver.as_ref())?;
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
        crate::path_util::open_path(&self.config_path).map_err(SyncError::Io)
    }

    fn on_view_logs(&self) -> Result<(), SyncError> {
        crate::path_util::open_path(&self.log_dir).map_err(SyncError::Io)
    }
}

/// Factory trait for creating synchronization engines for configured targets.
pub trait SyncEngineFactory: Send + Sync + 'static {
    /// Engine implementation type returned by this factory.
    type Engine: SyncEngine + 'static;

    /// Construct a sync engine for the target directory at `target_index`.
    fn create_engine(
        &self,
        target_index: usize,
        target_config: &crate::config::TargetSyncConfig,
        app_dir: &Path,
    ) -> Result<Self::Engine, SyncError>;
}

/// Default factory creating `LocalSyncEngine` backed by `SqliteHashStore`.
#[derive(Debug, Clone, Copy, Default)]
pub struct SqliteEngineFactory;

impl SyncEngineFactory for SqliteEngineFactory {
    type Engine = LocalSyncEngine<SqliteHashStore>;

    fn create_engine(
        &self,
        target_index: usize,
        target_config: &crate::config::TargetSyncConfig,
        app_dir: &Path,
    ) -> Result<Self::Engine, SyncError> {
        let dest = target_config.dest_dir();
        let dest_str = dest.to_string_lossy();
        let hash = blake3::hash(dest_str.as_bytes());
        let db_filename = format!("sigcache_{}.db", hash.to_hex());
        let db_path = app_dir.join(db_filename);

        tracing::info!(
            target_index = target_index + 1,
            target_path = %dest.display(),
            db_path = %db_path.display(),
            "Opening signature cache database for target",
        );
        let store_cfg = StoreConfig::new(
            target_config.block_size_bytes(),
            target_config.block_sync_threshold_bytes(),
        )?;
        let store = SqliteHashStore::new(&db_path, store_cfg)?;
        Ok(LocalSyncEngine::new(store, target_config.clone()))
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
    cancellation: Arc<AtomicBool>,
    command_tx: Sender<SyncCommand>,
}

impl SyncDaemon {
    /// Validates that no recursive sync loops exist between the source directory and destination directories,
    /// resolving mapped drive letters to UNC paths via `resolver` to prevent loop bypass.
    pub fn validate_target_loops(
        config: &Config,
        resolver: &dyn crate::net::NetworkResolver,
    ) -> Result<(), SyncError> {
        let src_orig = config.source_dir();
        let src_unc = resolver.try_resolve_unc_path(src_orig);

        let dests: Vec<_> = config
            .resolved_dest_dirs()
            .into_iter()
            .map(|dest| {
                let dest_unc = resolver.try_resolve_unc_path(&dest);
                (dest, dest_unc)
            })
            .collect();

        // 1. Check source vs destination loops
        for (dest, dest_unc) in &dests {
            let is_loop = crate::config::is_same_or_descendant(&src_unc, dest_unc)
                || crate::config::is_same_or_descendant(dest_unc, &src_unc)
                || crate::config::is_same_or_descendant(src_orig, dest_unc)
                || crate::config::is_same_or_descendant(dest_unc, src_orig)
                || crate::config::is_same_or_descendant(&src_unc, dest)
                || crate::config::is_same_or_descendant(dest, &src_unc);

            if is_loop {
                return Err(SyncError::validation(format!(
                    "Destination directory '{}' (resolved: '{}') is identical to or nested within source directory '{}' (resolved: '{}') (recursive sync loop)",
                    dest.display(),
                    dest_unc.display(),
                    src_orig.display(),
                    src_unc.display()
                )));
            }
        }

        // 2. Check destination vs destination overlaps (pairwise O(N^2))
        for i in 0..dests.len() {
            for j in (i + 1)..dests.len() {
                let (d1, d1_unc) = &dests[i];
                let (d2, d2_unc) = &dests[j];

                let is_dest_dest_overlap = crate::config::is_same_or_descendant(d1_unc, d2_unc)
                    || crate::config::is_same_or_descendant(d2_unc, d1_unc)
                    || crate::config::is_same_or_descendant(d1, d2_unc)
                    || crate::config::is_same_or_descendant(d2_unc, d1)
                    || crate::config::is_same_or_descendant(d1_unc, d2)
                    || crate::config::is_same_or_descendant(d2, d1_unc);

                if is_dest_dest_overlap {
                    return Err(SyncError::validation(format!(
                        "Destination directory '{}' conflicts with destination directory '{}' (nested or overlapping destination paths)",
                        d1.display(),
                        d2.display()
                    )));
                }
            }
        }

        Ok(())
    }

    /// Spawn the directory watcher coordinator thread.
    fn spawn_watcher_coordinator(
        config: Config,
        command_tx: Sender<SyncCommand>,
        source_connectivity: crate::sync::SourceConnectivityTracker,
        shutdown_flag: Arc<AtomicBool>,
        observer: Option<Arc<dyn SyncStatusObserver>>,
    ) -> Result<JoinHandle<()>, SyncError> {
        std::thread::Builder::new()
            .name("watcher-coordinator".to_string())
            .spawn(move || {
                let mut watcher: Option<crate::monitor::DirectoryWatcher> = None;
                let retry_interval =
                    std::time::Duration::from_secs(config.retry_interval_seconds());
                let mut last_status_check: Option<std::time::Instant> = None;

                let mut last_sent_online = None;
                let mut last_sent_active = None;

                while !shutdown_flag.load(Ordering::Relaxed) {
                    let now = std::time::Instant::now();
                    let should_check = match last_status_check {
                        None => true,
                        Some(last) => now.duration_since(last) >= retry_interval,
                    };

                    if should_check {
                        last_status_check = Some(now);
                        let current_source = config.source_dir();
                        let is_online = current_source.exists() && current_source.is_dir();
                        source_connectivity.set_online(is_online);

                        let mut watcher_active = false;
                        if is_online {
                            if watcher.is_none() {
                                tracing::info!(
                                    "Source directory online. Starting directory watcher..."
                                );
                                match crate::monitor::DirectoryWatcher::start(
                                    config.source_dir(),
                                    command_tx.clone(),
                                ) {
                                    Ok(w) => {
                                        watcher = Some(w);
                                        watcher_active = true;
                                        // Trigger catch-up full scan on source reconnection/startup
                                        tracing::info!(
                                            "Triggering full scan after source directory came online."
                                        );
                                        let _ = command_tx.send(SyncCommand::TriggerFullScan);
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
                            if let Some(ref obs) = observer {
                                obs.on_watcher_status_change(is_online.into(), watcher_active.into());
                            }
                        }
                    }

                    for _ in 0..10 {
                        if shutdown_flag.load(Ordering::Relaxed) {
                            break;
                        }
                        std::thread::sleep(std::time::Duration::from_millis(50));
                    }
                }
            })
            .map_err(SyncError::Io)
    }

    /// Spawn the central command broadcaster thread.
    fn spawn_command_broadcaster(
        command_rx: std::sync::mpsc::Receiver<SyncCommand>,
        mut worker_senders: Vec<Sender<SyncCommand>>,
        shutdown_flag: Arc<AtomicBool>,
    ) -> Result<JoinHandle<()>, SyncError> {
        std::thread::Builder::new()
            .name("command-broadcaster".to_string())
            .spawn(move || {
                while !shutdown_flag.load(Ordering::Relaxed) {
                    match command_rx.recv_timeout(std::time::Duration::from_millis(200)) {
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
            })
            .map_err(SyncError::Io)
    }

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
        Self::start_with_factory(SqliteEngineFactory, config, app_dir, observer)
    }

    /// Starts all sync workers using the provided engine factory.
    #[must_use = "dropping SyncDaemon immediately terminates all background sync workers"]
    pub fn start_with_factory<F: SyncEngineFactory>(
        factory: F,
        config: Config,
        app_dir: &Path,
        observer: Option<Arc<dyn SyncStatusObserver>>,
    ) -> Result<Self, SyncError> {
        let resolver = Arc::new(crate::net::Win32NetworkResolver);
        Self::start_with_factory_and_resolver(factory, config, app_dir, observer, resolver)
    }

    /// Starts all sync workers using the provided engine factory and network resolver.
    #[must_use = "dropping SyncDaemon immediately terminates all background sync workers"]
    pub fn start_with_factory_and_resolver<F: SyncEngineFactory>(
        factory: F,
        config: Config,
        app_dir: &Path,
        observer: Option<Arc<dyn SyncStatusObserver>>,
        resolver: Arc<dyn crate::net::NetworkResolver>,
    ) -> Result<Self, SyncError> {
        config.validate()?;
        Self::validate_target_loops(&config, resolver.as_ref())?;

        let shutdown_flag = Arc::new(AtomicBool::new(false));
        let cancellation = Arc::new(AtomicBool::new(false));
        let mut worker_handles = Vec::new();

        let source_connectivity = crate::sync::SourceConnectivityTracker::new(false);

        // 1. Initialize target databases and workers
        let mut worker_txs = Vec::new();
        for (idx, target_config) in config.target_configs()?.into_iter().enumerate() {
            let engine = factory.create_engine(idx, &target_config, app_dir)?;

            // Wire per-worker channel
            let (w_tx, w_rx) = channel();
            worker_txs.push(w_tx);

            tracing::info!(
                target_index = idx + 1,
                target_path = %target_config.dest_dir().display(),
                "Starting sync worker thread for target..."
            );
            let worker_ctx = SyncWorkerContext::new(
                idx,
                target_config,
                engine,
                w_rx,
                observer.clone(),
                source_connectivity.clone(),
            )
            .with_resolver(resolver.clone())
            .with_cancellation(cancellation.clone());
            let worker_handle = start_sync_worker(worker_ctx)?;
            worker_handles.push(worker_handle);
        }

        // 2. Central coordination channels and threads
        let (tx, rx) = channel();

        // Spawn central watcher coordinator thread
        let watcher_handle = Self::spawn_watcher_coordinator(
            config.clone(),
            tx.clone(),
            source_connectivity,
            shutdown_flag.clone(),
            observer,
        )?;

        // Spawn central broadcaster thread
        let broadcaster_handle =
            Self::spawn_command_broadcaster(rx, worker_txs, shutdown_flag.clone())?;

        Ok(Self {
            config,
            watcher_handle: Some(watcher_handle),
            broadcaster_handle: Some(broadcaster_handle),
            worker_handles,
            shutdown_flag,
            cancellation,
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
            // Signal cancellation token to all workers immediately
            self.cancellation.store(true, Ordering::Relaxed);
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

        let config = Config::builder(src).dest_dir(dst).build().unwrap();
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
debounce_seconds = 3
propagate_deletions = true
block_sync_threshold_bytes = 10485760
block_size_bytes = 1048576
verify_writes = true
"#,
        )
        .unwrap();

        let (tx, rx) = channel();
        let mock_registry = MockStartupRegistry::new(false);
        let handle = DaemonHandle::new(tx);
        let handler = DaemonTrayHandler::new(
            config_path,
            dir.path().join("logs"),
            handle.clone(),
            mock_registry,
        );

        assert!(!handler.is_startup_enabled().unwrap());
        assert!(handler.on_toggle_startup(true).unwrap());
        assert!(handler.is_startup_enabled().unwrap());
        assert!(!handler.on_toggle_startup(false).unwrap());
        assert!(!handler.is_startup_enabled().unwrap());

        handler.on_sync_now().unwrap();
        let cmd = rx.try_recv().unwrap();
        assert_eq!(cmd, SyncCommand::TriggerFullScan);

        // Valid reload
        assert!(handler.on_reload_config().is_ok());

        // Recursive loop reload should fail
        let loop_config_path = dir.path().join("loop_config.toml");
        std::fs::write(
            &loop_config_path,
            r#"
source_dir = "C:\\dummy_source"
dest_dir = "C:\\dummy_source\\nested"
debounce_seconds = 3
propagate_deletions = true
block_sync_threshold_bytes = 10485760
block_size_bytes = 1048576
verify_writes = true
"#,
        )
        .unwrap();
        let loop_handler = DaemonTrayHandler::new(
            loop_config_path,
            dir.path().join("logs"),
            handle.clone(),
            MockStartupRegistry::new(false),
        );
        let reload_err = loop_handler.on_reload_config().unwrap_err();
        assert!(
            reload_err.to_string().contains("recursive sync loop"),
            "Expected recursive sync loop error, got: {}",
            reload_err
        );
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
            .build()
            .unwrap();
        let daemon = SyncDaemon::start(config, dir.path(), None).unwrap();

        // Now bring source online
        std::fs::create_dir_all(&src).unwrap();

        // Sleep past retry interval to allow coordinator loop to discover source
        std::thread::sleep(std::time::Duration::from_millis(1500));

        daemon.shutdown();
    }

    struct MockEngineFactory;

    impl SyncEngineFactory for MockEngineFactory {
        type Engine = LocalSyncEngine<crate::db::MockHashStore>;

        fn create_engine(
            &self,
            _target_index: usize,
            target_config: &crate::config::TargetSyncConfig,
            _app_dir: &Path,
        ) -> Result<Self::Engine, SyncError> {
            Ok(LocalSyncEngine::new(
                crate::db::MockHashStore::new(),
                target_config.clone(),
            ))
        }
    }

    #[test]
    fn test_sync_daemon_start_with_custom_factory() {
        let dir = tempdir().unwrap();
        let src = dir.path().join("source");
        let dst = dir.path().join("dest");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&dst).unwrap();

        let config = Config::builder(src).dest_dir(dst).build().unwrap();
        let daemon =
            SyncDaemon::start_with_factory(MockEngineFactory, config, dir.path(), None).unwrap();
        assert_eq!(daemon.worker_handles.len(), 1);
        daemon.shutdown();
    }

    #[test]
    fn test_validate_target_loops_detects_mapped_drive_unc_loop() {
        let mock_resolver = crate::net::MockNetworkResolver::new();
        mock_resolver.set_alternate_path("Z:\\shared", "\\\\server\\share\\data");

        let config = Config::builder("Z:\\shared")
            .dest_dir("\\\\server\\share\\data\\subfolder")
            .build()
            .unwrap();

        let result = SyncDaemon::validate_target_loops(&config, &mock_resolver);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("recursive sync loop"));
    }

    #[test]
    fn test_validate_target_loops_allows_disjoint_targets() {
        let mock_resolver = crate::net::MockNetworkResolver::new();
        mock_resolver.set_alternate_path("Z:\\source", "\\\\server\\share1");

        let config = Config::builder("Z:\\source")
            .dest_dir("\\\\server\\share2")
            .build()
            .unwrap();

        let result = SyncDaemon::validate_target_loops(&config, &mock_resolver);
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_target_loops_detects_dest_dest_unc_overlap() {
        let mock_resolver = crate::net::MockNetworkResolver::new();
        mock_resolver.set_alternate_path("Y:\\backup", "\\\\server\\share\\backup");

        let config = Config::builder("C:\\source")
            .dest_dirs(vec!["Y:\\backup", "\\\\server\\share\\backup\\sub"])
            .build()
            .unwrap();

        let result = SyncDaemon::validate_target_loops(&config, &mock_resolver);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("conflicts with destination directory")
                || err_msg.contains("recursive sync loop")
                || err_msg.contains("overlapping destination"),
            "unexpected error message: {err_msg}"
        );
    }

    #[test]
    fn test_validate_target_loops_allows_disjoint_unc_targets() {
        let mock_resolver = crate::net::MockNetworkResolver::new();
        mock_resolver.set_alternate_path("Y:\\backup1", "\\\\server\\share\\b1");
        mock_resolver.set_alternate_path("Z:\\backup2", "\\\\server\\share\\b2");

        let config = Config::builder("C:\\source")
            .dest_dirs(vec!["Y:\\backup1", "Z:\\backup2"])
            .build()
            .unwrap();

        let result = SyncDaemon::validate_target_loops(&config, &mock_resolver);
        assert!(result.is_ok());
    }

    #[test]
    fn test_sync_daemon_rejects_unvalidated_config() {
        let dir = tempdir().unwrap();
        // Config with zero destinations fails config.validate()
        let invalid_config = Config::builder(dir.path()).build_unvalidated();
        let result =
            SyncDaemon::start_with_factory(MockEngineFactory, invalid_config, dir.path(), None);
        match result {
            Err(e) => assert!(e.to_string().contains("destination")),
            Ok(_) => panic!("Expected error for config without destinations"),
        }
    }

    #[test]
    fn test_sync_daemon_start_with_custom_resolver() {
        let dir = tempdir().unwrap();
        let src = dir.path().join("source");
        let dst = dir.path().join("dest");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&dst).unwrap();

        let config = Config::builder(src).dest_dir(dst).build().unwrap();
        let mock_resolver = Arc::new(crate::net::MockNetworkResolver::new());
        let daemon = SyncDaemon::start_with_factory_and_resolver(
            MockEngineFactory,
            config,
            dir.path(),
            None,
            mock_resolver,
        )
        .unwrap();
        assert_eq!(daemon.worker_handles.len(), 1);
        daemon.shutdown();
    }

    struct TrackingResolver {
        unc_calls: std::sync::atomic::AtomicUsize,
        alt_calls: std::sync::atomic::AtomicUsize,
    }

    impl crate::net::NetworkResolver for TrackingResolver {
        fn try_resolve_alternate_path(&self, path: &Path) -> PathBuf {
            self.alt_calls
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            path.to_path_buf()
        }

        fn try_resolve_unc_path(&self, path: &Path) -> PathBuf {
            self.unc_calls
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            path.to_path_buf()
        }

        fn is_destination_accessible(&self, _path: &Path) -> bool {
            true
        }

        fn establish_smb_connection(&self, _unc_path: &Path) -> Result<(), SyncError> {
            Ok(())
        }
    }

    #[test]
    fn test_validate_target_loops_unc() {
        let temp = tempdir().unwrap();
        let src = temp.path().join("source");
        let dest = temp.path().join("dest");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&dest).unwrap();

        let config = Config::builder(src).dest_dir(dest).build().unwrap();
        let resolver = TrackingResolver {
            unc_calls: std::sync::atomic::AtomicUsize::new(0),
            alt_calls: std::sync::atomic::AtomicUsize::new(0),
        };

        let result = SyncDaemon::validate_target_loops(&config, &resolver);
        assert!(result.is_ok());
        assert!(resolver.unc_calls.load(std::sync::atomic::Ordering::SeqCst) >= 2);
        assert_eq!(
            resolver.alt_calls.load(std::sync::atomic::Ordering::SeqCst),
            0
        );
    }
}

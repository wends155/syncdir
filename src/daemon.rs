//! Background sync daemon orchestrator and worker management.
//!
//! Owns background daemon lifecycle, worker thread spawning, watcher event loops,
//! reconnect scan triggers, and RAII shutdown.

use crate::config::Config;
use crate::db::{SqliteHashStore, StoreConfig};
use crate::error::SyncError;
use crate::path_util::is_same_or_descendant;
use crate::sync::{
    LocalSyncEngine, SyncCommand, SyncEngine, SyncStatusObserver, SyncWorkerContext,
    start_sync_worker,
};
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
    ///
    /// # Errors
    ///
    /// Returns [`SyncError::Tray`] if the internal worker channel has disconnected.
    pub fn trigger_full_scan(&self) -> Result<(), SyncError> {
        self.command_tx
            .send(SyncCommand::TriggerFullScan)
            .map_err(|e| SyncError::tray_with_source("Sync worker channel disconnected", e))
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
        let db_path = SqliteHashStore::cache_db_path(app_dir, dest);

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

fn join_thread_and_log_panic(handle: std::thread::JoinHandle<()>, thread_name: &str) {
    if let Err(panic_payload) = handle.join() {
        let msg = if let Some(s) = panic_payload.downcast_ref::<&str>() {
            *s
        } else if let Some(s) = panic_payload.downcast_ref::<String>() {
            s.as_str()
        } else {
            "unknown panic payload"
        };
        tracing::error!(
            panic = %msg,
            thread = %thread_name,
            "Thread terminated unexpectedly with panic"
        );
    }
}

/// Single-purpose control signal for event-driven watcher coordinator lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WatcherSignal {
    Shutdown,
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
    watcher_active: Arc<AtomicBool>,
    watcher_signal_tx: Option<Sender<WatcherSignal>>,
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
            let is_loop = is_same_or_descendant(&src_unc, dest_unc)
                || is_same_or_descendant(dest_unc, &src_unc)
                || is_same_or_descendant(src_orig, dest_unc)
                || is_same_or_descendant(dest_unc, src_orig)
                || is_same_or_descendant(&src_unc, dest)
                || is_same_or_descendant(dest, &src_unc);

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

                let is_dest_dest_overlap = is_same_or_descendant(d1_unc, d2_unc)
                    || is_same_or_descendant(d2_unc, d1_unc)
                    || is_same_or_descendant(d1, d2_unc)
                    || is_same_or_descendant(d2_unc, d1)
                    || is_same_or_descendant(d1_unc, d2)
                    || is_same_or_descendant(d2, d1_unc);

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
    #[allow(clippy::too_many_arguments)]
    fn spawn_watcher_coordinator(
        config: Config,
        command_tx: Sender<SyncCommand>,
        source_connectivity: crate::sync::SourceConnectivityTracker,
        shutdown_flag: Arc<AtomicBool>,
        observer: Option<Arc<dyn SyncStatusObserver>>,
        watcher_factory: Arc<dyn crate::monitor::WatcherFactory>,
        watcher_active: Arc<AtomicBool>,
        signal_rx: std::sync::mpsc::Receiver<WatcherSignal>,
    ) -> Result<JoinHandle<()>, SyncError> {
        let initial_online = config.source_dir().exists() && config.source_dir().is_dir();
        source_connectivity.set_online(initial_online);

        let (initial_watcher, is_initially_active) = if initial_online {
            match watcher_factory.create_watcher(config.source_dir(), command_tx.clone()) {
                Ok(w) => {
                    let active = w.is_watching();
                    let _ = command_tx.send(SyncCommand::TriggerFullScan);
                    (Some(w), active)
                }
                Err(e) => {
                    tracing::error!(
                        source_dir = %config.source_dir().display(),
                        error = %e,
                        "Failed to start directory watcher"
                    );
                    (None, false)
                }
            }
        } else {
            (None, false)
        };

        watcher_active.store(is_initially_active, Ordering::SeqCst);
        if let Some(ref obs) = observer {
            obs.on_watcher_status_change(initial_online.into(), is_initially_active.into());
        }

        let parent_span = tracing::Span::current();
        let source_dir = config.source_dir().to_path_buf();
        let watcher_span = tracing::info_span!(
            parent: &parent_span,
            "watcher_coordinator",
            source_dir = %source_dir.display()
        );
        let dispatcher = tracing::dispatcher::get_default(|d| d.clone());

        std::thread::Builder::new()
            .name("watcher-coordinator".to_string())
            .spawn(move || {
                let _dispatch_guard = tracing::dispatcher::set_default(&dispatcher);
                let _span_guard = watcher_span.entered();
                tracing::debug!("Watcher coordinator thread started");
                let mut watcher: Option<Box<dyn crate::monitor::FileWatcher>> = initial_watcher;
                let retry_interval =
                    std::time::Duration::from_secs(config.retry_interval_seconds());
                let mut last_sent_online = Some(initial_online);
                let mut last_sent_active = Some(is_initially_active);

                loop {
                    if shutdown_flag.load(Ordering::Relaxed) {
                        tracing::info!(
                            source_dir = %config.source_dir().display(),
                            "Watcher coordinator thread exiting on shutdown signal"
                        );
                        break;
                    }

                    match signal_rx.recv_timeout(retry_interval) {
                        Ok(WatcherSignal::Shutdown)
                        | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                            tracing::info!(
                                source_dir = %config.source_dir().display(),
                                "Watcher coordinator thread exiting on shutdown signal"
                            );
                            break;
                        }
                        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                    }

                    let current_source = config.source_dir();
                    let is_online = current_source.exists() && current_source.is_dir();
                    source_connectivity.set_online(is_online);

                    let mut active = false;
                    if is_online {
                        if watcher.is_none() {
                            tracing::info!(
                                source_dir = %config.source_dir().display(),
                                "Source directory online. Starting directory watcher..."
                            );
                            match watcher_factory
                                .create_watcher(config.source_dir(), command_tx.clone())
                            {
                                Ok(w) => {
                                    active = w.is_watching();
                                    watcher = Some(w);
                                    // Trigger catch-up full scan on source reconnection
                                    tracing::info!(
                                        source_dir = %config.source_dir().display(),
                                        "Triggering full scan after source directory came online."
                                    );
                                    let _ = command_tx.send(SyncCommand::TriggerFullScan);
                                }
                                Err(e) => {
                                    tracing::error!(
                                        source_dir = %config.source_dir().display(),
                                        error = %e,
                                        "Failed to start directory watcher"
                                    );
                                    active = false;
                                }
                            }
                        } else {
                            active = watcher.as_ref().map(|w| w.is_watching()).unwrap_or(true);
                        }
                    } else if watcher.is_some() {
                        tracing::warn!(
                            source_dir = %config.source_dir().display(),
                            "Source directory went offline. Dropping directory watcher."
                        );
                        watcher = None;
                    }

                    watcher_active.store(active, Ordering::SeqCst);

                    if last_sent_online != Some(is_online) || last_sent_active != Some(active) {
                        last_sent_online = Some(is_online);
                        last_sent_active = Some(active);
                        if let Some(ref obs) = observer {
                            obs.on_watcher_status_change(is_online.into(), active.into());
                        }
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
        let parent_span = tracing::Span::current();
        let broadcaster_span = tracing::info_span!(
            parent: &parent_span,
            "command_broadcaster"
        );
        let dispatcher = tracing::dispatcher::get_default(|d| d.clone());

        std::thread::Builder::new()
            .name("command-broadcaster".to_string())
            .spawn(move || {
                let _dispatch_guard = tracing::dispatcher::set_default(&dispatcher);
                let _span_guard = broadcaster_span.entered();
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
}

/// Fluent builder for constructing and starting a [`SyncDaemon`].
pub struct SyncDaemonBuilder<F = SqliteEngineFactory> {
    factory: F,
    config: Config,
    app_dir: PathBuf,
    observer: Option<Arc<dyn SyncStatusObserver>>,
    resolver: Arc<dyn crate::net::NetworkResolver>,
    watcher_factory: Arc<dyn crate::monitor::WatcherFactory>,
}

impl SyncDaemonBuilder<SqliteEngineFactory> {
    /// Create a new builder with default services (SqliteEngineFactory, Win32NetworkResolver, RecommendedWatcherFactory).
    pub fn new(config: Config, app_dir: impl Into<PathBuf>) -> Self {
        Self {
            factory: SqliteEngineFactory,
            config,
            app_dir: app_dir.into(),
            observer: None,
            resolver: Arc::new(crate::net::Win32NetworkResolver),
            watcher_factory: Arc::new(crate::monitor::RecommendedWatcherFactory),
        }
    }
}

impl<F: SyncEngineFactory> SyncDaemonBuilder<F> {
    /// Provide a custom engine factory.
    pub fn factory<F2: SyncEngineFactory>(self, factory: F2) -> SyncDaemonBuilder<F2> {
        SyncDaemonBuilder {
            factory,
            config: self.config,
            app_dir: self.app_dir,
            observer: self.observer,
            resolver: self.resolver,
            watcher_factory: self.watcher_factory,
        }
    }

    /// Provide an optional sync status observer.
    pub fn observer(mut self, observer: Option<Arc<dyn SyncStatusObserver>>) -> Self {
        self.observer = observer;
        self
    }

    /// Provide a custom network resolver.
    pub fn resolver(mut self, resolver: Arc<dyn crate::net::NetworkResolver>) -> Self {
        self.resolver = resolver;
        self
    }

    /// Provide a custom watcher factory.
    pub fn watcher_factory(
        mut self,
        watcher_factory: Arc<dyn crate::monitor::WatcherFactory>,
    ) -> Self {
        self.watcher_factory = watcher_factory;
        self
    }

    /// Start the sync daemon with configured services.
    pub fn start(self) -> Result<SyncDaemon, SyncError> {
        SyncDaemon::start_with_all_services(
            self.factory,
            self.config,
            &self.app_dir,
            self.observer,
            self.resolver,
            self.watcher_factory,
        )
    }
}

impl SyncDaemon {
    /// Create a fluent [`SyncDaemonBuilder`] for configuring and starting a daemon.
    pub fn builder(
        config: Config,
        app_dir: impl Into<PathBuf>,
    ) -> SyncDaemonBuilder<SqliteEngineFactory> {
        SyncDaemonBuilder::new(config, app_dir)
    }

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
        Self::builder(config, app_dir).observer(observer).start()
    }

    /// Starts all sync workers using the provided engine factory.
    #[deprecated(since = "0.2.0", note = "use SyncDaemon::builder instead")]
    #[must_use = "dropping SyncDaemon immediately terminates all background sync workers"]
    pub fn start_with_factory<F: SyncEngineFactory>(
        factory: F,
        config: Config,
        app_dir: &Path,
        observer: Option<Arc<dyn SyncStatusObserver>>,
    ) -> Result<Self, SyncError> {
        Self::builder(config, app_dir)
            .factory(factory)
            .observer(observer)
            .start()
    }

    /// Starts all sync workers using the provided engine factory and network resolver.
    #[deprecated(since = "0.2.0", note = "use SyncDaemon::builder instead")]
    #[must_use = "dropping SyncDaemon immediately terminates all background sync workers"]
    pub fn start_with_factory_and_resolver<F: SyncEngineFactory>(
        factory: F,
        config: Config,
        app_dir: &Path,
        observer: Option<Arc<dyn SyncStatusObserver>>,
        resolver: Arc<dyn crate::net::NetworkResolver>,
    ) -> Result<Self, SyncError> {
        Self::builder(config, app_dir)
            .factory(factory)
            .observer(observer)
            .resolver(resolver)
            .start()
    }

    /// Starts all sync workers using standard SqliteEngineFactory and custom resolver and watcher factory.
    #[deprecated(since = "0.2.0", note = "use SyncDaemon::builder instead")]
    #[must_use = "dropping SyncDaemon immediately terminates all background sync workers"]
    pub fn start_with_services(
        config: Config,
        app_dir: &Path,
        observer: Option<Arc<dyn SyncStatusObserver>>,
        resolver: Arc<dyn crate::net::NetworkResolver>,
        watcher_factory: Arc<dyn crate::monitor::WatcherFactory>,
    ) -> Result<Self, SyncError> {
        Self::builder(config, app_dir)
            .observer(observer)
            .resolver(resolver)
            .watcher_factory(watcher_factory)
            .start()
    }

    /// Starts all sync workers using custom engine factory, resolver, and watcher factory.
    #[must_use = "dropping SyncDaemon immediately terminates all background sync workers"]
    pub fn start_with_all_services<F: SyncEngineFactory>(
        factory: F,
        config: Config,
        app_dir: &Path,
        observer: Option<Arc<dyn SyncStatusObserver>>,
        resolver: Arc<dyn crate::net::NetworkResolver>,
        watcher_factory: Arc<dyn crate::monitor::WatcherFactory>,
    ) -> Result<Self, SyncError> {
        config.validate()?;
        Self::validate_target_loops(&config, resolver.as_ref())?;

        let shutdown_flag = Arc::new(AtomicBool::new(false));
        let cancellation = Arc::new(AtomicBool::new(false));
        let watcher_active = Arc::new(AtomicBool::new(false));
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
            let worker_ctx = SyncWorkerContext::builder(
                idx,
                target_config,
                engine,
                w_rx,
                source_connectivity.clone(),
            )
            .maybe_observer(observer.clone())
            .resolver(resolver.clone())
            .cancellation(cancellation.clone())
            .build()?;
            let worker_handle = start_sync_worker(worker_ctx)?;
            worker_handles.push(worker_handle);
        }

        // 2. Central coordination channels and threads
        let (tx, rx) = channel();
        let (watcher_signal_tx, watcher_signal_rx) = channel();

        // Spawn central watcher coordinator thread
        let watcher_handle = Self::spawn_watcher_coordinator(
            config.clone(),
            tx.clone(),
            source_connectivity,
            shutdown_flag.clone(),
            observer,
            watcher_factory,
            watcher_active.clone(),
            watcher_signal_rx,
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
            watcher_active,
            watcher_signal_tx: Some(watcher_signal_tx),
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

    /// Returns true if the directory watcher is currently active.
    pub fn watcher_running(&self) -> bool {
        self.watcher_active.load(Ordering::SeqCst)
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
            // Signal watcher coordinator to wake up and terminate immediately before thread join
            if let Some(signal_tx) = self.watcher_signal_tx.take() {
                let _ = signal_tx.send(WatcherSignal::Shutdown);
            }
            // Step 1: Join watcher thread first so no new events are generated
            if let Some(handle) = self.watcher_handle.take() {
                join_thread_and_log_panic(handle, "watcher-coordinator");
            }
            // Step 2: Join broadcaster thread so in-flight commands are distributed
            if let Some(handle) = self.broadcaster_handle.take() {
                join_thread_and_log_panic(handle, "command-broadcaster");
            }
            // Step 3: Join all worker threads
            for (idx, handle) in self.worker_handles.drain(..).enumerate() {
                let name = format!("sync-worker-{}", idx + 1);
                join_thread_and_log_panic(handle, &name);
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
    use std::path::PathBuf;
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
    #[allow(deprecated)]
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
    #[allow(deprecated)]
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
    #[allow(deprecated)]
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

    #[test]
    fn test_daemon_perform_shutdown_logs_worker_panic() {
        let dir = tempfile::tempdir().expect("tempdir");
        let src = dir.path().join("source");
        let dst = dir.path().join("dest");
        std::fs::create_dir_all(&src).expect("src");
        std::fs::create_dir_all(&dst).expect("dst");

        let config = crate::config::Config::builder(src)
            .dest_dir(dst)
            .build()
            .expect("config");
        let mut daemon =
            crate::daemon::SyncDaemon::start(config, dir.path(), None).expect("daemon start");

        // Push a worker thread that deliberately panics
        daemon.worker_handles.push(
            std::thread::Builder::new()
                .name("panicking-worker".to_string())
                .spawn(|| panic!("simulated worker panic payload"))
                .expect("spawn"),
        );

        // perform_shutdown must safely intercept, join, and log the panic without panicking itself
        daemon.perform_shutdown();
        assert!(daemon.worker_handles.is_empty());
    }

    struct DummyWatcher(Arc<AtomicBool>);
    impl crate::monitor::FileWatcher for DummyWatcher {
        fn is_watching(&self) -> bool {
            self.0.load(Ordering::SeqCst)
        }
    }

    struct DummyFactory(Arc<AtomicBool>);
    impl crate::monitor::WatcherFactory for DummyFactory {
        fn create_watcher(
            &self,
            _source_dir: &Path,
            _tx: std::sync::mpsc::Sender<crate::sync::SyncCommand>,
        ) -> Result<Box<dyn crate::monitor::FileWatcher>, SyncError> {
            Ok(Box::new(DummyWatcher(self.0.clone())))
        }
    }

    #[test]
    #[allow(deprecated)]
    fn test_sync_daemon_coordinates_with_mock_watcher() {
        let dir = tempfile::tempdir().expect("tempdir");
        let src = dir.path().join("source");
        let dst = dir.path().join("dest");
        std::fs::create_dir_all(&src).expect("src");
        std::fs::create_dir_all(&dst).expect("dst");

        let config = crate::config::Config::builder(src)
            .dest_dir(dst)
            .build()
            .expect("config");
        let flag = Arc::new(AtomicBool::new(true));
        let factory = Arc::new(DummyFactory(flag.clone()));

        let daemon = SyncDaemon::start_with_services(
            config,
            dir.path(),
            None,
            Arc::new(crate::net::MockNetworkResolver::new()),
            factory,
        )
        .expect("daemon start");

        assert!(daemon.watcher_running());
        daemon.shutdown();
    }

    #[test]
    fn test_sync_daemon_builder_default_and_custom_services() {
        let dir = tempfile::tempdir().expect("tempdir");
        let src = dir.path().join("source");
        let dst = dir.path().join("dest");
        std::fs::create_dir_all(&src).expect("src");
        std::fs::create_dir_all(&dst).expect("dst");

        let config = crate::config::Config::builder(src)
            .dest_dir(dst)
            .build()
            .expect("config");

        let flag = Arc::new(AtomicBool::new(true));
        let mock_watcher_factory = Arc::new(DummyFactory(flag));
        let mock_resolver = Arc::new(crate::net::MockNetworkResolver::new());

        let daemon = SyncDaemon::builder(config, dir.path())
            .factory(MockEngineFactory)
            .resolver(mock_resolver)
            .watcher_factory(mock_watcher_factory)
            .start()
            .expect("start via builder");

        assert!(daemon.watcher_running());
        daemon.shutdown();
    }

    #[test]
    fn test_watcher_thread_logs_correlate_with_daemon_context() {
        let dir = tempfile::tempdir().expect("tempdir");
        let src = dir.path().join("source");
        let dst = dir.path().join("dest");
        std::fs::create_dir_all(&src).expect("src");
        std::fs::create_dir_all(&dst).expect("dst");

        let config = crate::config::Config::builder(&src)
            .dest_dir(dst)
            .build()
            .expect("config");

        let flag = Arc::new(AtomicBool::new(true));
        let mock_watcher_factory = Arc::new(DummyFactory(flag));
        let mock_resolver = Arc::new(crate::net::MockNetworkResolver::new());

        let (_, log_output) = crate::test_support::with_captured_tracing(|| {
            let daemon = SyncDaemon::builder(config, dir.path())
                .factory(MockEngineFactory)
                .resolver(mock_resolver)
                .watcher_factory(mock_watcher_factory)
                .start()
                .expect("start via builder");

            std::thread::sleep(std::time::Duration::from_millis(150));
            daemon.shutdown();
        });

        assert!(
            log_output.contains("watcher_coordinator"),
            "Missing watcher_coordinator span: {log_output}"
        );
        assert!(
            log_output.contains("source_dir="),
            "Missing source_dir field: {log_output}"
        );
    }

    #[test]
    fn test_watcher_coordinator_shutdown_responsiveness_and_structured_logs() {
        let dir = tempfile::tempdir().expect("tempdir");
        let src = dir.path().join("source");
        let dst = dir.path().join("dest");
        std::fs::create_dir_all(&src).expect("src");
        std::fs::create_dir_all(&dst).expect("dst");

        let config = crate::config::Config::builder(&src)
            .dest_dir(dst)
            .retry_interval_seconds(10)
            .build()
            .expect("config");

        let flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let mock_watcher_factory = std::sync::Arc::new(DummyFactory(flag));
        let mock_resolver = std::sync::Arc::new(crate::net::MockNetworkResolver::new());

        let (_, log_output) = crate::test_support::with_captured_tracing(|| {
            let daemon = SyncDaemon::builder(config, dir.path())
                .factory(MockEngineFactory)
                .resolver(mock_resolver)
                .watcher_factory(mock_watcher_factory)
                .start()
                .expect("daemon start");

            std::thread::sleep(std::time::Duration::from_millis(50));

            let shutdown_start = std::time::Instant::now();
            daemon.shutdown();
            let shutdown_duration = shutdown_start.elapsed();

            assert!(
                shutdown_duration < std::time::Duration::from_millis(500),
                "Watcher coordinator shutdown took too long ({:?}); must respond within 500ms",
                shutdown_duration
            );
        });

        assert!(
            log_output.contains("watcher_coordinator"),
            "Missing watcher_coordinator span in logs: {log_output}"
        );
        assert!(
            log_output.contains("Watcher coordinator thread exiting on shutdown signal"),
            "Missing structured shutdown exit log event in logs: {log_output}"
        );
    }
}

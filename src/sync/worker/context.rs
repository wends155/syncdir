use crate::config::TargetSyncConfig;
use crate::error::SyncError;
use crate::net::NetworkResolver;
use crate::sync::engine::{SyncCommand, SyncEngine, SyncStatusObserver};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;

/// Thread-safe tracker for source directory connectivity.
#[derive(Clone, Debug)]
pub struct SourceConnectivityTracker(Arc<AtomicBool>);

impl SourceConnectivityTracker {
    /// Create a new tracker with initial online state.
    pub fn new(initial: bool) -> Self {
        Self(Arc::new(AtomicBool::new(initial)))
    }

    /// Return true if the source is currently marked online.
    pub fn is_online(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }

    /// Set the source online status.
    pub fn set_online(&self, online: bool) {
        self.0.store(online, Ordering::Relaxed);
    }

    /// Access the underlying `Arc<AtomicBool>` for low-level compatibility.
    pub fn raw_arc(&self) -> Arc<AtomicBool> {
        self.0.clone()
    }
}

impl From<Arc<AtomicBool>> for SourceConnectivityTracker {
    fn from(arc: Arc<AtomicBool>) -> Self {
        Self(arc)
    }
}

impl From<bool> for SourceConnectivityTracker {
    fn from(b: bool) -> Self {
        Self::new(b)
    }
}

/// Execution context for a target synchronization worker thread.
pub struct SyncWorkerContext<E: SyncEngine> {
    pub(crate) target_index: usize,
    pub(crate) config: TargetSyncConfig,
    pub(crate) engine: E,
    pub(crate) rx: Receiver<SyncCommand>,
    pub(crate) observer: Option<Arc<dyn SyncStatusObserver>>,
    pub(crate) source_connectivity: SourceConnectivityTracker,
    pub(crate) resolver: Arc<dyn NetworkResolver>,
    pub(crate) cancellation: Arc<AtomicBool>,
    pub(crate) max_pending_queue: usize,
}

/// Builder for constructing a [`SyncWorkerContext`] with validated configuration invariants.
#[must_use = "builders do nothing unless .build() is called"]
pub struct SyncWorkerContextBuilder<E: SyncEngine> {
    target_index: usize,
    config: TargetSyncConfig,
    engine: E,
    rx: Receiver<SyncCommand>,
    observer: Option<Arc<dyn SyncStatusObserver>>,
    source_connectivity: SourceConnectivityTracker,
    resolver: Option<Arc<dyn NetworkResolver>>,
    cancellation: Option<Arc<AtomicBool>>,
    max_pending_queue: usize,
}

impl<E: SyncEngine> SyncWorkerContextBuilder<E> {
    /// Create a new builder with required parameters and default options.
    pub fn new(
        target_index: usize,
        config: impl Into<TargetSyncConfig>,
        engine: E,
        rx: Receiver<SyncCommand>,
        source_connectivity: impl Into<SourceConnectivityTracker>,
    ) -> Self {
        Self {
            target_index,
            config: config.into(),
            engine,
            rx,
            observer: None,
            source_connectivity: source_connectivity.into(),
            resolver: None,
            cancellation: None,
            max_pending_queue: 50_000,
        }
    }

    /// Attach a status observer.
    pub fn observer(mut self, observer: Arc<dyn SyncStatusObserver>) -> Self {
        self.observer = Some(observer);
        self
    }

    /// Optionally attach a status observer.
    pub fn maybe_observer(mut self, observer: Option<Arc<dyn SyncStatusObserver>>) -> Self {
        self.observer = observer;
        self
    }

    /// Attach a custom network resolver.
    pub fn resolver(mut self, resolver: Arc<dyn NetworkResolver>) -> Self {
        self.resolver = Some(resolver);
        self
    }

    /// Attach a cancellation flag.
    pub fn cancellation(mut self, cancellation: Arc<AtomicBool>) -> Self {
        self.cancellation = Some(cancellation);
        self
    }

    /// Set the maximum capacity of the pending debounce queue.
    pub fn max_pending_queue(mut self, max: usize) -> Self {
        self.max_pending_queue = max;
        self
    }

    /// Build the `SyncWorkerContext`, validating invariants.
    ///
    /// # Errors
    ///
    /// Returns `SyncError::Validation` if `max_pending_queue == 0`.
    pub fn build(self) -> Result<SyncWorkerContext<E>, SyncError> {
        if self.max_pending_queue == 0 {
            return Err(SyncError::validation_invariant(
                "max_pending_queue must be greater than zero",
            ));
        }
        Ok(SyncWorkerContext {
            target_index: self.target_index,
            config: self.config,
            engine: self.engine,
            rx: self.rx,
            observer: self.observer,
            source_connectivity: self.source_connectivity,
            resolver: self
                .resolver
                .unwrap_or_else(|| Arc::new(crate::net::Win32NetworkResolver)),
            cancellation: self
                .cancellation
                .unwrap_or_else(|| Arc::new(AtomicBool::new(false))),
            max_pending_queue: self.max_pending_queue,
        })
    }
}

impl<E: SyncEngine> SyncWorkerContext<E> {
    /// Return a builder for `SyncWorkerContext`.
    pub fn builder(
        target_index: usize,
        config: impl Into<TargetSyncConfig>,
        engine: E,
        rx: Receiver<SyncCommand>,
        source_connectivity: impl Into<SourceConnectivityTracker>,
    ) -> SyncWorkerContextBuilder<E> {
        SyncWorkerContextBuilder::new(target_index, config, engine, rx, source_connectivity)
    }

    /// Create a new sync worker context.
    pub fn new(
        target_index: usize,
        config: impl Into<TargetSyncConfig>,
        engine: E,
        rx: Receiver<SyncCommand>,
        observer: Option<Arc<dyn SyncStatusObserver>>,
        source_connectivity: impl Into<SourceConnectivityTracker>,
    ) -> Self {
        Self {
            target_index,
            config: config.into(),
            engine,
            rx,
            observer,
            source_connectivity: source_connectivity.into(),
            resolver: Arc::new(crate::net::Win32NetworkResolver),
            cancellation: Arc::new(AtomicBool::new(false)),
            max_pending_queue: 50_000,
        }
    }

    /// Read-only target index accessor.
    pub fn target_index(&self) -> usize {
        self.target_index
    }

    /// Read-only target config accessor.
    pub fn config(&self) -> &TargetSyncConfig {
        &self.config
    }

    /// Read-only engine reference.
    pub fn engine(&self) -> &E {
        &self.engine
    }

    /// Mutable engine reference.
    pub fn engine_mut(&mut self) -> &mut E {
        &mut self.engine
    }

    /// Read-only observer reference.
    pub fn observer(&self) -> Option<&Arc<dyn SyncStatusObserver>> {
        self.observer.as_ref()
    }

    /// Read-only source connectivity tracker reference.
    pub fn source_connectivity(&self) -> &SourceConnectivityTracker {
        &self.source_connectivity
    }

    /// Read-only network resolver reference.
    pub fn resolver(&self) -> &Arc<dyn NetworkResolver> {
        &self.resolver
    }

    /// Read-only cancellation token reference.
    pub fn cancellation(&self) -> &Arc<AtomicBool> {
        &self.cancellation
    }

    /// Read-only max pending queue capacity.
    pub fn max_pending_queue(&self) -> usize {
        self.max_pending_queue
    }

    /// Set a custom maximum pending queue capacity.
    #[must_use]
    pub fn with_max_pending_queue(mut self, max_pending_queue: usize) -> Self {
        self.max_pending_queue = max_pending_queue;
        self
    }

    /// Set a custom network resolver.
    #[must_use]
    pub fn with_resolver(mut self, resolver: Arc<dyn NetworkResolver>) -> Self {
        self.resolver = resolver;
        self
    }

    /// Set a custom cancellation token.
    #[must_use]
    pub fn with_cancellation(mut self, cancellation: Arc<AtomicBool>) -> Self {
        self.cancellation = cancellation;
        self
    }

    /// Backwards-compatible accessor for raw atomic bool.
    pub fn source_online_atomic(&self) -> Arc<AtomicBool> {
        self.source_connectivity.raw_arc()
    }
}

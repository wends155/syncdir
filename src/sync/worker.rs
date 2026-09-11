use crate::config::TargetSyncConfig;
use crate::error::{SyncError, is_network_offline_io};
use crate::sync::engine::{
    ConnectivityState, ScanOutcome, SyncCommand, SyncEngine, SyncStatusObserver,
};
use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Thread-safe tracker for source directory connectivity.
#[derive(Clone, Debug)]
pub struct SourceConnectivityTracker(std::sync::Arc<std::sync::atomic::AtomicBool>);

impl SourceConnectivityTracker {
    /// Create a new tracker with initial online state.
    pub fn new(initial: bool) -> Self {
        Self(std::sync::Arc::new(std::sync::atomic::AtomicBool::new(
            initial,
        )))
    }

    /// Return true if the source is currently marked online.
    pub fn is_online(&self) -> bool {
        self.0.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Set the source online status.
    pub fn set_online(&self, online: bool) {
        self.0.store(online, std::sync::atomic::Ordering::Relaxed);
    }

    /// Access the underlying `Arc<AtomicBool>` for low-level compatibility.
    pub fn raw_arc(&self) -> std::sync::Arc<std::sync::atomic::AtomicBool> {
        self.0.clone()
    }
}

impl From<std::sync::Arc<std::sync::atomic::AtomicBool>> for SourceConnectivityTracker {
    fn from(arc: std::sync::Arc<std::sync::atomic::AtomicBool>) -> Self {
        Self(arc)
    }
}

impl From<bool> for SourceConnectivityTracker {
    fn from(b: bool) -> Self {
        Self::new(b)
    }
}

/// Manages pending sync and delete paths with per-path debounce deadlines and capacity limits.
#[derive(Debug)]
pub struct DebounceQueue {
    pending_syncs: HashMap<PathBuf, Instant>,
    pending_deletes: HashMap<PathBuf, Instant>,
    sync_heap: std::collections::BinaryHeap<std::cmp::Reverse<(Instant, PathBuf)>>,
    delete_heap: std::collections::BinaryHeap<std::cmp::Reverse<(Instant, PathBuf)>>,
    max_capacity: usize,
}

impl DebounceQueue {
    /// Create a new debounce queue with given capacity.
    pub fn new(max_capacity: usize) -> Self {
        Self {
            pending_syncs: HashMap::new(),
            pending_deletes: HashMap::new(),
            sync_heap: std::collections::BinaryHeap::new(),
            delete_heap: std::collections::BinaryHeap::new(),
            max_capacity,
        }
    }

    /// Enqueue a path for sync with a debounce duration.
    /// If the path is already pending, its deadline is extended.
    /// Returns false if at capacity and the path was not already pending.
    pub fn enqueue_sync(&mut self, path: PathBuf, debounce: Duration) -> bool {
        let is_tracked =
            self.pending_syncs.contains_key(&path) || self.pending_deletes.contains_key(&path);
        if !is_tracked && self.pending_syncs.len() + self.pending_deletes.len() >= self.max_capacity
        {
            return false;
        }
        self.pending_deletes.remove(&path);
        let dl = Instant::now() + debounce;
        self.pending_syncs.insert(path.clone(), dl);
        self.sync_heap.push(Reverse((dl, path)));
        self.compact_heaps();
        true
    }

    /// Enqueue a path for deletion with a debounce duration.
    /// If the path is already pending, its deadline is extended.
    /// Returns false if at capacity and the path was not already pending.
    pub fn enqueue_delete(&mut self, path: PathBuf, debounce: Duration) -> bool {
        let is_tracked =
            self.pending_syncs.contains_key(&path) || self.pending_deletes.contains_key(&path);
        if !is_tracked && self.pending_syncs.len() + self.pending_deletes.len() >= self.max_capacity
        {
            return false;
        }
        self.pending_syncs.remove(&path);
        let dl = Instant::now() + debounce;
        self.pending_deletes.insert(path.clone(), dl);
        self.delete_heap.push(Reverse((dl, path)));
        self.compact_heaps();
        true
    }

    fn compact_single_heap(
        heap: &mut BinaryHeap<Reverse<(Instant, PathBuf)>>,
        active_items: &HashMap<PathBuf, Instant>,
    ) {
        if heap.len() > 64 && heap.len() > active_items.len() * 2 {
            let valid_entries: Vec<Reverse<(Instant, PathBuf)>> = heap
                .drain()
                .filter(|Reverse((deadline, path))| active_items.get(path) == Some(deadline))
                .collect();
            *heap = BinaryHeap::from(valid_entries);
        }
    }

    fn compact_heaps(&mut self) {
        Self::compact_single_heap(&mut self.sync_heap, &self.pending_syncs);
        Self::compact_single_heap(&mut self.delete_heap, &self.pending_deletes);
    }

    /// Drain and return all sync paths whose debounce deadlines are <= `now`.
    pub fn drain_ready_syncs(&mut self, now: Instant) -> Vec<PathBuf> {
        let mut ready = Vec::new();
        while let Some(std::cmp::Reverse((deadline, _path))) = self.sync_heap.peek() {
            if *deadline > now {
                break;
            }
            if let Some(std::cmp::Reverse((deadline, path))) = self.sync_heap.pop()
                && self.pending_syncs.get(&path) == Some(&deadline)
            {
                self.pending_syncs.remove(&path);
                ready.push(path);
            }
        }
        ready
    }

    /// Drain and return all delete paths whose debounce deadlines are <= `now`.
    pub fn drain_ready_deletes(&mut self, now: Instant) -> Vec<PathBuf> {
        let mut ready = Vec::new();
        while let Some(std::cmp::Reverse((deadline, _path))) = self.delete_heap.peek() {
            if *deadline > now {
                break;
            }
            if let Some(std::cmp::Reverse((deadline, path))) = self.delete_heap.pop()
                && self.pending_deletes.get(&path) == Some(&deadline)
            {
                self.pending_deletes.remove(&path);
                ready.push(path);
            }
        }
        ready
    }

    /// Re-enqueue a failed sync path for retry with a backoff delay.
    pub fn requeue_sync_retry(&mut self, path: PathBuf, delay: std::time::Duration) {
        let dl = Instant::now() + delay;
        self.pending_syncs.insert(path.clone(), dl);
        self.sync_heap.push(std::cmp::Reverse((dl, path)));
    }

    /// Re-enqueue a failed delete path for retry with a backoff delay.
    pub fn requeue_delete_retry(&mut self, path: PathBuf, delay: std::time::Duration) {
        let dl = Instant::now() + delay;
        self.pending_deletes.insert(path.clone(), dl);
        self.delete_heap.push(std::cmp::Reverse((dl, path)));
    }

    /// Calculate earliest deadline across all pending syncs and deletes.
    ///
    /// Uses min-heap top with lazy eviction of stale entries (whose deadline
    /// was updated or path was drained).
    pub fn earliest_deadline(&mut self) -> Option<Instant> {
        let sync_earliest = loop {
            match self.sync_heap.peek() {
                Some(std::cmp::Reverse((deadline, path))) => {
                    if self.pending_syncs.get(path) == Some(deadline) {
                        break Some(*deadline);
                    } else {
                        self.sync_heap.pop();
                    }
                }
                None => break None,
            }
        };
        let delete_earliest = loop {
            match self.delete_heap.peek() {
                Some(std::cmp::Reverse((deadline, path))) => {
                    if self.pending_deletes.get(path) == Some(deadline) {
                        break Some(*deadline);
                    } else {
                        self.delete_heap.pop();
                    }
                }
                None => break None,
            }
        };
        match (sync_earliest, delete_earliest) {
            (Some(s), Some(d)) => Some(std::cmp::min(s, d)),
            (Some(s), None) => Some(s),
            (None, Some(d)) => Some(d),
            (None, None) => None,
        }
    }

    /// Return true if both pending sync and delete queues are empty.
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.pending_syncs.is_empty() && self.pending_deletes.is_empty()
    }

    /// Return count of pending syncs.
    #[allow(dead_code)]
    pub fn pending_sync_count(&self) -> usize {
        self.pending_syncs.len()
    }

    /// Return count of pending deletes.
    #[allow(dead_code)]
    pub fn pending_delete_count(&self) -> usize {
        self.pending_deletes.len()
    }

    /// Return total count of pending syncs and deletes.
    pub fn pending_count(&self) -> usize {
        self.pending_syncs.len() + self.pending_deletes.len()
    }

    /// Returns the total number of pending operations (syncs + deletes).
    #[inline]
    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.pending_count()
    }
}

/// Manages reachability checks and alternate network path resolution for a target worker.
pub struct ReachabilityMonitor {
    target_index: usize,
    configured_dest: PathBuf,
    active_dest: PathBuf,
    dest_online: bool,
    last_sent_status: Option<ConnectivityState>,
    last_status_check: Option<Instant>,
    retry_dur: std::time::Duration,
    resolver: std::sync::Arc<dyn crate::net::NetworkResolver>,
}

impl ReachabilityMonitor {
    /// Create a new reachability monitor for a target directory.
    pub fn new(
        target_index: usize,
        configured_dest: PathBuf,
        retry_interval_seconds: u64,
        resolver: std::sync::Arc<dyn crate::net::NetworkResolver>,
    ) -> Self {
        let active_dest = configured_dest.clone();
        Self {
            target_index,
            configured_dest,
            active_dest,
            dest_online: false,
            last_sent_status: None,
            last_status_check: None,
            retry_dur: std::time::Duration::from_secs(retry_interval_seconds),
            resolver,
        }
    }

    /// Return the active resolved destination directory path.
    pub fn active_dest(&self) -> &Path {
        &self.active_dest
    }

    /// Return true if the destination is currently determined to be online.
    pub fn is_dest_online(&self) -> bool {
        self.dest_online
    }

    /// Check if enough time has elapsed to warrant another reachability check.
    pub fn should_check_reachability(&self, now: Instant) -> bool {
        match self.last_status_check {
            None => true,
            Some(last) => now.duration_since(last) >= self.retry_dur,
        }
    }

    /// Probe destination reachability and resolve alternate UNC paths if necessary.
    pub fn check_reachability(
        &mut self,
        now: Instant,
        observer: Option<&std::sync::Arc<dyn SyncStatusObserver>>,
    ) {
        self.last_status_check = Some(now);

        let resolved = self
            .resolver
            .try_resolve_alternate_path(&self.configured_dest);
        let resolved_accessible = self.resolver.is_destination_accessible(&resolved);
        let configured_accessible = if !resolved_accessible && resolved != self.configured_dest {
            self.resolver
                .is_destination_accessible(&self.configured_dest)
        } else {
            false
        };

        if resolved_accessible {
            self.active_dest = resolved;
            self.dest_online = true;
        } else if configured_accessible {
            self.active_dest = self.configured_dest.clone();
            self.dest_online = true;
        } else {
            self.active_dest = self.configured_dest.clone();
            self.dest_online = false;
        }

        let new_state = if self.dest_online {
            ConnectivityState::Online
        } else {
            ConnectivityState::Offline
        };

        if self.last_sent_status != Some(new_state) {
            if let Some(obs) = observer {
                obs.on_target_status_change(self.target_index, new_state);
            }
            self.last_sent_status = Some(new_state);
        }
    }

    /// Mark the target destination offline immediately and notify observers.
    pub fn mark_offline(&mut self, observer: Option<&std::sync::Arc<dyn SyncStatusObserver>>) {
        self.dest_online = false;
        if self.last_sent_status != Some(ConnectivityState::Offline) {
            if let Some(obs) = observer {
                obs.on_target_status_change(self.target_index, ConnectivityState::Offline);
            }
            self.last_sent_status = Some(ConnectivityState::Offline);
        }
    }

    /// Mark the target destination online immediately and notify observers.
    pub fn mark_online(&mut self, observer: Option<&std::sync::Arc<dyn SyncStatusObserver>>) {
        self.dest_online = true;
        if self.last_sent_status != Some(ConnectivityState::Online) {
            if let Some(obs) = observer {
                obs.on_target_status_change(self.target_index, ConnectivityState::Online);
            }
            self.last_sent_status = Some(ConnectivityState::Online);
        }
    }
}

/// State container for the sync worker execution loop.
pub(crate) struct SyncWorkerState {
    pub(crate) scratch: Vec<u8>,
    pub(crate) failure_tracker: HashMap<PathBuf, u32>,
    pub(crate) hourly_prune_interval: std::time::Duration,
    pub(crate) last_archive_prune: Instant,
    pub(crate) needs_catchup_scan: bool,
}

impl SyncWorkerState {
    /// Initialize worker scratch buffer and archive prune timers.
    pub fn new(block_size_bytes: u64) -> Self {
        Self {
            scratch: vec![0u8; block_size_bytes as usize],
            failure_tracker: HashMap::new(),
            hourly_prune_interval: std::time::Duration::from_secs(3600),
            last_archive_prune: Instant::now(),
            needs_catchup_scan: false,
        }
    }

    /// Check if enough time has passed to trigger the hourly archive prune.
    pub fn should_prune_archive(&self, now: Instant) -> bool {
        now.duration_since(self.last_archive_prune) >= self.hourly_prune_interval
    }

    /// Record timestamp of the most recent archive pruning.
    pub fn record_prune(&mut self, now: Instant) {
        self.last_archive_prune = now;
    }

    /// Increment and return consecutive failure count for a path.
    pub fn record_failure(&mut self, path: &Path) -> u32 {
        let entry = self.failure_tracker.entry(path.to_path_buf()).or_insert(0);
        *entry += 1;
        *entry
    }

    /// Reset failure count upon successful synchronization.
    pub fn reset_failure(&mut self, path: &Path) {
        self.failure_tracker.remove(path);
    }

    /// Marks that a catch-up scan is required (e.g. on queue overflow or permanent eviction).
    pub fn mark_needs_catchup_scan(&mut self) {
        self.needs_catchup_scan = true;
    }

    /// Clears the catch-up scan requirement flag.
    pub fn clear_needs_catchup_scan(&mut self) {
        self.needs_catchup_scan = false;
    }

    /// Whether a catch-up full scan is currently needed.
    #[must_use]
    pub fn needs_catchup_scan(&self) -> bool {
        self.needs_catchup_scan
    }

    /// Determines if a catch-up scan should execute based on queue depth and connectivity.
    pub fn should_trigger_catchup_scan(
        &self,
        queue_pending: usize,
        drain_threshold: usize,
        source_online: bool,
        dest_online: bool,
    ) -> bool {
        self.needs_catchup_scan()
            && queue_pending <= drain_threshold
            && source_online
            && dest_online
    }
}

/// Execution context for a target synchronization worker thread.
pub struct SyncWorkerContext<E: SyncEngine> {
    pub(crate) target_index: usize,
    pub(crate) config: TargetSyncConfig,
    pub(crate) engine: E,
    pub(crate) rx: std::sync::mpsc::Receiver<SyncCommand>,
    pub(crate) observer: Option<std::sync::Arc<dyn SyncStatusObserver>>,
    pub(crate) source_connectivity: SourceConnectivityTracker,
    pub(crate) resolver: std::sync::Arc<dyn crate::net::NetworkResolver>,
    pub(crate) cancellation: std::sync::Arc<std::sync::atomic::AtomicBool>,
    pub(crate) max_pending_queue: usize,
}

/// Builder for constructing a [`SyncWorkerContext`] with validated configuration invariants.
pub struct SyncWorkerContextBuilder<E: SyncEngine> {
    target_index: usize,
    config: TargetSyncConfig,
    engine: E,
    rx: std::sync::mpsc::Receiver<SyncCommand>,
    observer: Option<std::sync::Arc<dyn SyncStatusObserver>>,
    source_connectivity: SourceConnectivityTracker,
    resolver: Option<std::sync::Arc<dyn crate::net::NetworkResolver>>,
    cancellation: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    max_pending_queue: usize,
}

impl<E: SyncEngine> SyncWorkerContextBuilder<E> {
    /// Create a new builder with required parameters and default options.
    pub fn new(
        target_index: usize,
        config: impl Into<TargetSyncConfig>,
        engine: E,
        rx: std::sync::mpsc::Receiver<SyncCommand>,
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
    pub fn observer(mut self, observer: std::sync::Arc<dyn SyncStatusObserver>) -> Self {
        self.observer = Some(observer);
        self
    }

    /// Optionally attach a status observer.
    pub fn maybe_observer(
        mut self,
        observer: Option<std::sync::Arc<dyn SyncStatusObserver>>,
    ) -> Self {
        self.observer = observer;
        self
    }

    /// Attach a custom network resolver.
    pub fn resolver(mut self, resolver: std::sync::Arc<dyn crate::net::NetworkResolver>) -> Self {
        self.resolver = Some(resolver);
        self
    }

    /// Attach a cancellation flag.
    pub fn cancellation(
        mut self,
        cancellation: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) -> Self {
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
                .unwrap_or_else(|| std::sync::Arc::new(crate::net::Win32NetworkResolver)),
            cancellation: self
                .cancellation
                .unwrap_or_else(|| std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false))),
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
        rx: std::sync::mpsc::Receiver<SyncCommand>,
        source_connectivity: impl Into<SourceConnectivityTracker>,
    ) -> SyncWorkerContextBuilder<E> {
        SyncWorkerContextBuilder::new(target_index, config, engine, rx, source_connectivity)
    }

    /// Create a new sync worker context.
    pub fn new(
        target_index: usize,
        config: impl Into<TargetSyncConfig>,
        engine: E,
        rx: std::sync::mpsc::Receiver<SyncCommand>,
        observer: Option<std::sync::Arc<dyn SyncStatusObserver>>,
        source_connectivity: impl Into<SourceConnectivityTracker>,
    ) -> Self {
        Self {
            target_index,
            config: config.into(),
            engine,
            rx,
            observer,
            source_connectivity: source_connectivity.into(),
            resolver: std::sync::Arc::new(crate::net::Win32NetworkResolver),
            cancellation: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
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
    pub fn observer(&self) -> Option<&std::sync::Arc<dyn SyncStatusObserver>> {
        self.observer.as_ref()
    }

    /// Read-only source connectivity tracker reference.
    pub fn source_connectivity(&self) -> &SourceConnectivityTracker {
        &self.source_connectivity
    }

    /// Read-only network resolver reference.
    pub fn resolver(&self) -> &std::sync::Arc<dyn crate::net::NetworkResolver> {
        &self.resolver
    }

    /// Read-only cancellation token reference.
    pub fn cancellation(&self) -> &std::sync::Arc<std::sync::atomic::AtomicBool> {
        &self.cancellation
    }

    /// Read-only max pending queue capacity.
    pub fn max_pending_queue(&self) -> usize {
        self.max_pending_queue
    }

    /// Set a custom maximum pending queue capacity.
    pub fn with_max_pending_queue(mut self, max_pending_queue: usize) -> Self {
        self.max_pending_queue = max_pending_queue;
        self
    }

    /// Set a custom network resolver.
    pub fn with_resolver(
        mut self,
        resolver: std::sync::Arc<dyn crate::net::NetworkResolver>,
    ) -> Self {
        self.resolver = resolver;
        self
    }

    /// Set a custom cancellation token.
    pub fn with_cancellation(
        mut self,
        cancellation: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) -> Self {
        self.cancellation = cancellation;
        self
    }

    /// Backwards-compatible accessor for raw atomic bool.
    pub fn source_online_atomic(&self) -> std::sync::Arc<std::sync::atomic::AtomicBool> {
        self.source_connectivity.raw_arc()
    }
}

/// Calculates exponential backoff duration based on the number of attempts.
pub fn calculate_exponential_backoff(
    attempts: u32,
    base_interval: std::time::Duration,
) -> std::time::Duration {
    let factor = 2u64.saturating_pow(attempts.saturating_sub(1));
    let max_delay = std::time::Duration::from_secs(300);
    base_interval
        .checked_mul(factor.min(u32::MAX as u64) as u32)
        .unwrap_or(max_delay)
        .min(max_delay)
}

/// Spawns a background synchronization worker thread.
/// Compute the channel receive timeout for the background worker event loop.
///
/// If there are pending expired sync items in the debounce queue, `Duration::ZERO`
/// is returned only if both destination and source are currently reachable (`can_drain`).
/// If either is offline, the timeout is clamped to 1 second to prevent CPU busy-spinning.
pub(crate) fn calculate_worker_poll_timeout(
    queue_earliest: Option<std::time::Instant>,
    now: std::time::Instant,
    can_drain: bool,
) -> std::time::Duration {
    match queue_earliest {
        Some(dl) if dl > now => (dl - now).min(std::time::Duration::from_secs(1)),
        Some(_) if can_drain => std::time::Duration::ZERO,
        Some(_) => std::time::Duration::from_secs(1),
        None => std::time::Duration::from_secs(1),
    }
}

/// The outcome of evaluating a discrete execution tick of the [`SyncWorkerRunner`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkerTickOutcome {
    /// The worker should continue processing events and polling commands.
    Continue,
    /// The worker received a shutdown command or cancellation signal and should terminate.
    ShutdownRequested,
}

/// Testable sync worker state machine orchestrating debouncing, reachability, and execution.
///
/// Encapsulates worker context, debounce priority queues, reachability monitors, and execution
/// state, allowing deterministic, zero-sleep stepping through time via [`SyncWorkerRunner::tick`].
pub struct SyncWorkerRunner<E: SyncEngine> {
    pub(crate) context: SyncWorkerContext<E>,
    pub(crate) queue: DebounceQueue,
    pub(crate) reachability: ReachabilityMonitor,
    pub(crate) state: SyncWorkerState,
    pub(crate) drain_threshold: usize,
}

impl<E: SyncEngine> SyncWorkerRunner<E> {
    /// Create a new `SyncWorkerRunner` instance initialized with the given worker context.
    ///
    /// # Arguments
    ///
    /// * `context` - The worker configuration and shared state context.
    ///
    /// # Returns
    ///
    /// A configured [`SyncWorkerRunner`] ready to process commands and tick cycles.
    pub fn new(context: SyncWorkerContext<E>) -> Self {
        let max_pending_queue = context.max_pending_queue;
        let queue = DebounceQueue::new(max_pending_queue);
        let reachability = ReachabilityMonitor::new(
            context.target_index,
            context.config.dest_dir().to_path_buf(),
            context.config.retry_interval_seconds(),
            context.resolver.clone(),
        );
        let state = SyncWorkerState::new(context.config.block_size_bytes());
        let drain_threshold = 1_000.min(max_pending_queue / 2);
        Self {
            context,
            queue,
            reachability,
            state,
            drain_threshold,
        }
    }

    /// Process an incoming synchronization command into the worker state machine.
    ///
    /// Routes filesystem change notifications to debounced priority queues, full scans
    /// to immediate execution, and cancellation signals.
    ///
    /// # Arguments
    ///
    /// * `cmd` - The [`SyncCommand`] to process.
    ///
    /// # Returns
    ///
    /// Returns `true` if the runner should continue processing, or `false` if shutdown was requested.
    pub fn handle_command(&mut self, cmd: SyncCommand) -> bool {
        let debounce_dur = std::time::Duration::from_secs(self.context.config.debounce_seconds());
        let target_index = self.context.target_index;
        match cmd {
            SyncCommand::FileModified(path) => {
                self.state.reset_failure(&path);
                if !self.queue.enqueue_sync(path.to_path_buf(), debounce_dur) {
                    tracing::error!(
                        target_index = target_index + 1,
                        "Debounce queue overflow on file modify; scheduling catchup scan"
                    );
                    self.state.mark_needs_catchup_scan();
                }
                true
            }
            SyncCommand::FileDeleted(path) => {
                self.state.reset_failure(&path);
                if !self.queue.enqueue_delete(path.to_path_buf(), debounce_dur) {
                    tracing::error!(
                        target_index = target_index + 1,
                        "Debounce queue overflow on file delete; scheduling catchup scan"
                    );
                    self.state.mark_needs_catchup_scan();
                }
                true
            }
            SyncCommand::TriggerFullScan => {
                if self.context.source_connectivity.is_online() {
                    self.context.engine.invalidate_verified_dirs();
                    match self.context.engine.run_cancellable_full_scan(
                        self.reachability.active_dest(),
                        &self.context.cancellation,
                    ) {
                        Ok(ScanOutcome::Success { synced }) => {
                            tracing::info!(synced, "Full scan completed successfully");
                            self.reachability
                                .mark_online(self.context.observer.as_ref());
                        }
                        Ok(ScanOutcome::PartialFailure {
                            synced,
                            failed,
                            delete_failed,
                        }) => {
                            tracing::warn!(
                                synced,
                                failed,
                                delete_failed,
                                "Full scan completed with failures"
                            );
                        }
                        Ok(ScanOutcome::DestinationUnreachable) => {
                            tracing::warn!("Full scan determined destination is unreachable");
                            self.reachability
                                .mark_offline(self.context.observer.as_ref());
                        }
                        Err(SyncError::Cancelled) => {
                            tracing::info!("Full scan cancelled");
                            return false;
                        }
                        Err(e) => {
                            tracing::error!(error = %e, "Full scan failed");
                        }
                    }
                }
                true
            }
        }
    }

    fn flush_staged_paths(&mut self, staged_paths: &mut Vec<PathBuf>, retry_dur: Duration) {
        if staged_paths.is_empty() {
            return;
        }
        match self.context.engine.flush_staged_syncs() {
            Ok(()) => {
                for p in staged_paths.drain(..) {
                    self.state.reset_failure(&p);
                }
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    count = staged_paths.len(),
                    "Failed to flush staged sync batch; requeuing paths for retry"
                );
                for p in staged_paths.drain(..) {
                    let attempts = self.state.record_failure(&p);
                    let backoff = calculate_exponential_backoff(attempts, retry_dur);
                    self.queue.requeue_sync_retry(p, backoff);
                }
            }
        }
    }

    /// Execute a discrete tick of the worker state machine at simulated timestamp `now`.
    ///
    /// Evaluates target reachability, prunes archives, drains expired debounce queues,
    /// triggers file transfers with exponential backoff on retryable errors, and evicts
    /// permanent validation failures. Runs deterministically without thread sleeps.
    ///
    /// # Arguments
    ///
    /// * `now` - The current synthetic or physical [`Instant`].
    ///
    /// # Returns
    ///
    /// Returns [`WorkerTickOutcome::Continue`] to keep running, or [`WorkerTickOutcome::ShutdownRequested`]
    /// if cancellation or shutdown was signaled.
    ///
    /// # Errors
    ///
    /// Returns a [`SyncError`] if a fatal operational error occurs during tick execution.
    pub fn tick(&mut self, now: Instant) -> Result<WorkerTickOutcome, SyncError> {
        if self
            .context
            .cancellation
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            return Ok(WorkerTickOutcome::ShutdownRequested);
        }

        let mut network_offline_detected = false;
        let was_offline = !self.reachability.is_dest_online();
        if self.reachability.should_check_reachability(now) {
            self.reachability
                .check_reachability(now, self.context.observer.as_ref());
            if was_offline && self.reachability.is_dest_online() {
                tracing::info!(
                    target_index = self.context.target_index + 1,
                    target_path = %self.context.config.dest_dir().display(),
                    "Target destination is back online."
                );
                self.context.engine.invalidate_verified_dirs();
                if self.context.source_connectivity.is_online() {
                    tracing::info!(
                        target_index = self.context.target_index + 1,
                        "Triggering catch-up full scan following destination reconnect."
                    );
                    match self.context.engine.run_cancellable_full_scan(
                        self.reachability.active_dest(),
                        &self.context.cancellation,
                    ) {
                        Ok(ScanOutcome::DestinationUnreachable) => {
                            tracing::warn!("Catch-up scan determined destination is unreachable");
                            self.reachability
                                .mark_offline(self.context.observer.as_ref());
                        }
                        Ok(_) => {}
                        Err(SyncError::Cancelled) => {
                            tracing::info!("Catch-up scan cancelled");
                            return Ok(WorkerTickOutcome::ShutdownRequested);
                        }
                        Err(e) => {
                            tracing::error!(error = %e, "Catch-up full scan on reconnect failed");
                        }
                    }
                }
            }
        }

        let retry_dur =
            std::time::Duration::from_secs(self.context.config.retry_interval_seconds());

        if self.reachability.is_dest_online() && self.state.should_prune_archive(now) {
            self.state.record_prune(now);
            if let Err(e) = self
                .context
                .engine
                .prune_archive(self.reachability.active_dest())
            {
                tracing::warn!(
                    target_index = self.context.target_index + 1,
                    target = %self.reachability.active_dest().display(),
                    error = %e,
                    "Periodic archive prune failed"
                );
            }
        }

        let can_drain = self.reachability.is_dest_online()
            && self.context.source_connectivity.is_online()
            && !network_offline_detected;

        if can_drain {
            let ready_syncs = self.queue.drain_ready_syncs(now);
            let mut staged_paths: Vec<PathBuf> = Vec::new();
            for path in ready_syncs {
                if self
                    .context
                    .cancellation
                    .load(std::sync::atomic::Ordering::Relaxed)
                {
                    self.flush_staged_paths(&mut staged_paths, retry_dur);
                    return Ok(WorkerTickOutcome::ShutdownRequested);
                }
                if network_offline_detected
                    || !self.context.source_connectivity.is_online()
                    || !self.reachability.is_dest_online()
                {
                    self.queue.requeue_sync_retry(path, retry_dur);
                    continue;
                }

                match self.context.engine.sync_file_to_dest_staged(
                    &path,
                    self.reachability.active_dest(),
                    &mut self.state.scratch,
                ) {
                    Ok(()) => {
                        staged_paths.push(path);
                        if staged_paths.len() >= 500 {
                            self.flush_staged_paths(&mut staged_paths, retry_dur);
                        }
                    }
                    Err(SyncError::WriteVerificationFailed { .. }) => {
                        let attempts = self.state.record_failure(&path);
                        if attempts <= 10 {
                            let backoff = calculate_exponential_backoff(attempts, retry_dur);
                            tracing::warn!(
                                path = %path.display(),
                                attempt = attempts,
                                ?backoff,
                                "Write verification failed; rescheduling retry"
                            );
                            self.queue.requeue_sync_retry(path, backoff);
                        } else {
                            tracing::error!(
                                path = %path.display(),
                                "Write verification permanently failed after 10 retries"
                            );
                            self.state.reset_failure(&path);
                            if let Some(ref obs) = self.context.observer {
                                obs.on_write_verification_failed(&path);
                            }
                        }
                    }
                    Err(err) if err.is_permanent_validation_failure() => {
                        tracing::error!(
                            path = %path.display(),
                            target = %self.reachability.active_dest().display(),
                            error = %err,
                            "Permanent validation failure; evicting from sync queue without retry"
                        );
                        self.state.reset_failure(&path);
                        if let Some(ref obs) = self.context.observer {
                            obs.on_write_verification_failed(&path);
                        }
                    }
                    Err(SyncError::Validation { ref message, .. }) => {
                        let attempts = self.state.record_failure(&path);
                        if attempts <= 10 {
                            let backoff = calculate_exponential_backoff(attempts, retry_dur);
                            tracing::warn!(
                                path = %path.display(),
                                attempt = attempts,
                                ?backoff,
                                error = %message,
                                "Validation failure; rescheduling retry"
                            );
                            self.queue.requeue_sync_retry(path, backoff);
                        } else {
                            tracing::error!(
                                path = %path.display(),
                                error = %message,
                                "Validation permanently failed after 10 retries"
                            );
                            self.state.reset_failure(&path);
                            if let Some(ref obs) = self.context.observer {
                                obs.on_write_verification_failed(&path);
                            }
                        }
                    }
                    Err(e) if e.is_network_offline() => {
                        tracing::warn!(
                            target_index = self.context.target_index + 1,
                            path = %path.display(),
                            error = %e,
                            "Network offline detected during file sync; rescheduling retry"
                        );
                        network_offline_detected = true;
                        self.reachability
                            .mark_offline(self.context.observer.as_ref());
                        self.queue.requeue_sync_retry(path, retry_dur);
                    }
                    Err(SyncError::Io(e)) => {
                        if is_network_offline_io(&e) {
                            tracing::warn!(
                                target_index = self.context.target_index + 1,
                                path = %path.display(),
                                error = %e,
                                "Network offline I/O error detected during file sync; rescheduling retry"
                            );
                            network_offline_detected = true;
                            self.reachability
                                .mark_offline(self.context.observer.as_ref());
                        }
                        let attempts = self.state.record_failure(&path);
                        let backoff = calculate_exponential_backoff(attempts, retry_dur);
                        tracing::warn!(
                            path = %path.display(),
                            attempt = attempts,
                            ?backoff,
                            error = %e,
                            "File sync failed with IO error; rescheduling retry"
                        );
                        self.queue.requeue_sync_retry(path, backoff);
                    }
                    Err(e) => {
                        let attempts = self.state.record_failure(&path);
                        let backoff = calculate_exponential_backoff(attempts, retry_dur);
                        tracing::warn!(
                            path = %path.display(),
                            attempt = attempts,
                            ?backoff,
                            error = %e,
                            "File sync failed; rescheduling retry"
                        );
                        self.queue.requeue_sync_retry(path, backoff);
                    }
                }
            }
            self.flush_staged_paths(&mut staged_paths, retry_dur);

            let ready_deletes = self.queue.drain_ready_deletes(now);
            for path in ready_deletes {
                if self
                    .context
                    .cancellation
                    .load(std::sync::atomic::Ordering::Relaxed)
                {
                    return Ok(WorkerTickOutcome::ShutdownRequested);
                }
                if network_offline_detected
                    || !self.context.source_connectivity.is_online()
                    || !self.reachability.is_dest_online()
                {
                    self.queue.requeue_delete_retry(path, retry_dur);
                    continue;
                }

                if self.context.config.propagate_deletions() {
                    match self
                        .context
                        .engine
                        .delete_file_from_dest(&path, self.reachability.active_dest())
                    {
                        Ok(()) => {
                            self.state.reset_failure(&path);
                        }
                        Err(err) if err.is_permanent_validation_failure() => {
                            tracing::error!(
                                path = %path.display(),
                                error = %err,
                                "Permanent validation failure on deletion; evicting without retry"
                            );
                            self.state.reset_failure(&path);
                        }
                        Err(SyncError::Validation { ref message, .. }) => {
                            let attempts = self.state.record_failure(&path);
                            if attempts <= 10 {
                                let backoff = calculate_exponential_backoff(attempts, retry_dur);
                                tracing::warn!(
                                    path = %path.display(),
                                    attempt = attempts,
                                    ?backoff,
                                    error = %message,
                                    "Validation error on deletion; rescheduling retry"
                                );
                                self.queue.requeue_delete_retry(path, backoff);
                            } else {
                                tracing::error!(
                                    path = %path.display(),
                                    error = %message,
                                    "Deletion permanently failed after 10 retries"
                                );
                                self.state.reset_failure(&path);
                            }
                        }
                        Err(e) if e.is_network_offline() => {
                            network_offline_detected = true;
                            self.reachability
                                .mark_offline(self.context.observer.as_ref());
                            self.queue.requeue_delete_retry(path, retry_dur);
                        }
                        Err(SyncError::Io(e)) => {
                            if is_network_offline_io(&e) {
                                network_offline_detected = true;
                                self.reachability
                                    .mark_offline(self.context.observer.as_ref());
                            }
                            let attempts = self.state.record_failure(&path);
                            let backoff = calculate_exponential_backoff(attempts, retry_dur);
                            self.queue.requeue_delete_retry(path, backoff);
                        }
                        Err(e) => {
                            let attempts = self.state.record_failure(&path);
                            let backoff = calculate_exponential_backoff(attempts, retry_dur);
                            tracing::warn!(
                                path = %path.display(),
                                attempt = attempts,
                                ?backoff,
                                error = %e,
                                "File delete failed; rescheduling retry"
                            );
                            self.queue.requeue_delete_retry(path, backoff);
                        }
                    }
                }
            }
        }

        if self.state.should_trigger_catchup_scan(
            self.queue.pending_count(),
            self.drain_threshold,
            self.context.source_connectivity.is_online(),
            self.reachability.is_dest_online(),
        ) {
            tracing::info!(
                target_index = self.context.target_index + 1,
                "Triggering catch-up full scan following queue overflow recovery or eviction"
            );
            match self.context.engine.run_cancellable_full_scan(
                self.reachability.active_dest(),
                &self.context.cancellation,
            ) {
                Ok(ScanOutcome::Success { .. }) | Ok(ScanOutcome::PartialFailure { .. }) => {
                    self.state.clear_needs_catchup_scan();
                }
                Ok(ScanOutcome::DestinationUnreachable) => {
                    self.reachability
                        .mark_offline(self.context.observer.as_ref());
                }
                Err(SyncError::Cancelled) => {
                    return Ok(WorkerTickOutcome::ShutdownRequested);
                }
                Err(e) => {
                    tracing::error!(error = %e, "Catch-up scan after queue overflow failed");
                }
            }
        }

        Ok(WorkerTickOutcome::Continue)
    }
}

/// Start a dedicated background worker thread for a specific target destination.
///
/// The worker listens for filesystem events (file changes/deletions) on its channel
/// and triggers block-level sync operations to its specific destination directory.
///
/// # Arguments
///
/// * `context` - Worker execution context containing the engine, configuration, channels, and observers.
///
/// # Returns
///
/// Returns the join handle for the spawned background worker thread, or a `SyncError` if spawning fails.
#[must_use = "dropping the JoinHandle detaches the sync worker thread"]
pub fn start_sync_worker<E: SyncEngine + 'static>(
    context: SyncWorkerContext<E>,
) -> Result<std::thread::JoinHandle<()>, SyncError> {
    let target_index = context.target_index;
    let parent_span = tracing::Span::current();
    let dest = context.config.dest_dir().to_path_buf();
    let target_idx = target_index + 1;
    let worker_span = tracing::info_span!(
        parent: &parent_span,
        "sync_worker",
        target_index = target_idx,
        dest = %dest.display()
    );
    let dispatcher = tracing::dispatcher::get_default(|d| d.clone());

    std::thread::Builder::new()
        .name(format!("sync-worker-{}", target_index))
        .spawn(move || {
            let _dispatch_guard = tracing::dispatcher::set_default(&dispatcher);
            let _span_guard = worker_span.entered();
            let mut runner = SyncWorkerRunner::new(context);
            'worker: loop {
                if runner
                    .context
                    .cancellation
                    .load(std::sync::atomic::Ordering::Relaxed)
                {
                    tracing::info!(
                        target_index = target_index + 1,
                        "Sync worker shutting down via cancellation signal."
                    );
                    break 'worker;
                }

                let now = Instant::now();
                match runner.tick(now) {
                    Ok(WorkerTickOutcome::Continue) => {}
                    Ok(WorkerTickOutcome::ShutdownRequested) => break 'worker,
                    Err(e) => {
                        tracing::error!(error = %e, "Sync worker tick error");
                    }
                }

                let can_drain = runner.reachability.is_dest_online()
                    && runner.context.source_connectivity.is_online();
                let timeout =
                    calculate_worker_poll_timeout(runner.queue.earliest_deadline(), now, can_drain);

                match runner.context.rx.recv_timeout(timeout) {
                    Ok(cmd) => {
                        if !runner.handle_command(cmd) {
                            tracing::info!(
                                target_index = target_index + 1,
                                "Sync worker shutting down via command."
                            );
                            break 'worker;
                        }
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break 'worker,
                }

                // Drain any additional queued commands non-blocking
                while let Ok(cmd) = runner.context.rx.try_recv() {
                    if !runner.handle_command(cmd) {
                        tracing::info!(
                            target_index = target_index + 1,
                            "Sync worker shutting down via drained command."
                        );
                        break 'worker;
                    }
                }
            }
        })
        .map_err(SyncError::Io)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, TargetSyncConfig};
    use crate::db::MockHashStore;
    use crate::path_util::RelativePath;
    use crate::sync::engine::LocalSyncEngine;
    use crate::sync::mock::MockSyncEngine;
    use pretty_assertions::assert_eq;
    use std::fs;
    use std::time::{Duration, Instant};
    use tempfile::tempdir;

    #[test]
    fn test_debounce_queue_action_replacement_at_capacity() {
        let mut q = DebounceQueue::new(2);
        let path1 = PathBuf::from("file1.txt");
        let path2 = PathBuf::from("file2.txt");
        let path3 = PathBuf::from("file3.txt");

        // Fill queue to max_capacity (2) with pending deletes
        assert!(q.enqueue_delete(path1.clone(), Duration::from_secs(10)));
        assert!(q.enqueue_delete(path2.clone(), Duration::from_secs(10)));
        assert_eq!(q.len(), 2);

        // New untracked path must be rejected due to capacity limit
        assert!(!q.enqueue_sync(path3.clone(), Duration::from_secs(10)));
        assert_eq!(q.len(), 2);

        // Tracked path in pending_deletes must be accepted for action replacement (sync replacing delete)
        assert!(q.enqueue_sync(path1.clone(), Duration::from_secs(5)));
        assert_eq!(q.len(), 2);

        // Vice-versa: tracked path in pending_syncs must be accepted for delete replacement
        assert!(q.enqueue_delete(path1.clone(), Duration::from_secs(5)));
        assert_eq!(q.len(), 2);
    }

    #[test]
    fn test_debounce_queue_heap_compaction() {
        let mut q = DebounceQueue::new(100);
        let path = PathBuf::from("frequently_edited.txt");

        // Enqueue the same path 1000 times with updated deadlines
        for i in 1..=1000 {
            q.enqueue_sync(path.clone(), Duration::from_millis(i * 10));
        }

        // Only 1 item is logically pending
        assert_eq!(q.len(), 1);
        // Compaction must have run and kept heap size bounded well below 1000
        assert!(
            q.sync_heap.len() <= 64,
            "Heap size not bounded: {}",
            q.sync_heap.len()
        );
    }

    #[test]
    fn test_debounce_queue_drain_safety() {
        let mut q = DebounceQueue::new(10);
        let now = Instant::now();

        // Empty queue drain returns empty vec without panic
        assert!(q.drain_ready_syncs(now).is_empty());
        assert!(q.drain_ready_deletes(now).is_empty());

        // Enqueue items
        let p1 = PathBuf::from("ready_sync.txt");
        let p2 = PathBuf::from("future_sync.txt");
        let d1 = PathBuf::from("ready_del.txt");
        let d2 = PathBuf::from("future_del.txt");

        q.enqueue_sync(p1.clone(), Duration::from_millis(0));
        q.enqueue_sync(p2.clone(), Duration::from_secs(60));
        q.enqueue_delete(d1.clone(), Duration::from_millis(0));
        q.enqueue_delete(d2.clone(), Duration::from_secs(60));

        // Sleep 1ms to ensure deadline is passed
        std::thread::sleep(Duration::from_millis(1));
        let check_now = Instant::now();

        let syncs = q.drain_ready_syncs(check_now);
        assert_eq!(syncs, vec![p1]);

        let deletes = q.drain_ready_deletes(check_now);
        assert_eq!(deletes, vec![d1]);

        // Future items remain pending
        assert_eq!(q.len(), 2);
    }

    #[test]
    fn test_calculate_worker_poll_timeout() {
        let now = Instant::now();

        // Case 1: Empty queue (None) -> 1 second
        assert_eq!(
            calculate_worker_poll_timeout(None, now, true),
            Duration::from_secs(1)
        );
        assert_eq!(
            calculate_worker_poll_timeout(None, now, false),
            Duration::from_secs(1)
        );

        // Case 2: Future deadline -> min(deadline - now, 1s)
        let future_500ms = now + Duration::from_millis(500);
        let timeout_future = calculate_worker_poll_timeout(Some(future_500ms), now, true);
        assert!(timeout_future <= Duration::from_millis(500));
        assert!(timeout_future >= Duration::from_millis(400));

        let future_2s = now + Duration::from_secs(2);
        assert_eq!(
            calculate_worker_poll_timeout(Some(future_2s), now, true),
            Duration::from_secs(1)
        );

        // Case 3: Expired deadline (deadline <= now)
        let past = now - Duration::from_millis(100);

        // When can_drain is true -> Duration::ZERO
        assert_eq!(
            calculate_worker_poll_timeout(Some(past), now, true),
            Duration::ZERO
        );

        // When can_drain is false (destination or source offline) -> clamped to 1 second
        assert_eq!(
            calculate_worker_poll_timeout(Some(past), now, false),
            Duration::from_secs(1)
        );
    }

    #[test]
    fn test_worker_queue_debouncing_storm() {
        let dir = tempdir().unwrap();
        let source = dir.path().join("src");
        let dest = dir.path().join("dst");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&dest).unwrap();

        let config = Config::test_default(source.clone(), dest.clone());
        let store = MockHashStore::new();
        let (tx, rx) = std::sync::mpsc::channel();
        let source_online = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        // Write initial source file
        fs::write(source.join("storm.txt"), b"initial").unwrap();

        let target_config = TargetSyncConfig::try_from_config(&config).unwrap();
        let engine = LocalSyncEngine::new(store, target_config.clone());
        let context = SyncWorkerContext::new(0, target_config, engine, rx, None, source_online);
        let _handle = start_sync_worker(context).unwrap();

        // Allow initial scan to complete
        std::thread::sleep(std::time::Duration::from_millis(200));

        // Update source file and send rapid burst of interleaved modified/deleted events
        fs::write(source.join("storm.txt"), b"storm data").unwrap();

        tx.send(SyncCommand::FileModified(
            RelativePath::new("storm.txt").unwrap(),
        ))
        .unwrap();
        tx.send(SyncCommand::FileDeleted(
            RelativePath::new("storm.txt").unwrap(),
        ))
        .unwrap();
        tx.send(SyncCommand::FileModified(
            RelativePath::new("storm.txt").unwrap(),
        ))
        .unwrap();

        // Wait for debounce and sync to complete (debounce is 1s, allow up to 5s under load)
        let start = std::time::Instant::now();
        let mut synced = false;
        while start.elapsed() < std::time::Duration::from_secs(5) {
            if let Ok(content) = fs::read(dest.join("storm.txt"))
                && content == b"storm data"
            {
                synced = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(
            synced,
            "Timed out waiting for debounced storm sync to complete"
        );
    }

    #[test]
    fn test_trigger_full_scan_skipped_offline() {
        let dir = tempdir().unwrap();
        let source = dir.path().join("nonexistent_source");
        let dest = dir.path().join("dst");
        fs::create_dir_all(&dest).unwrap();

        let config = Config::test_default(source, dest.clone());
        let target_config = TargetSyncConfig::try_from_config(&config).unwrap();
        let store = MockHashStore::new();
        let (tx, rx) = std::sync::mpsc::channel();
        let source_online = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let engine = LocalSyncEngine::new(store, target_config.clone());
        let context = SyncWorkerContext::new(0, target_config, engine, rx, None, source_online);
        let _handle = start_sync_worker(context).unwrap();

        tx.send(SyncCommand::TriggerFullScan).unwrap();

        std::thread::sleep(std::time::Duration::from_millis(500));

        let entries: Vec<_> = fs::read_dir(&dest)
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();
        assert!(
            entries.is_empty(),
            "Destination should be empty when source is offline"
        );
    }

    #[test]
    fn test_source_offline_guards_deletions() {
        let dir = tempdir().unwrap();
        let source = dir.path().join("src");
        let dest = dir.path().join("dst");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&dest).unwrap();

        fs::write(dest.join("keep_me.txt"), b"pre-existing").unwrap();

        let config = Config::builder(source)
            .dest_dir(dest.clone())
            .debounce_seconds(1)
            .retry_interval_seconds(1)
            .propagate_deletions(true)
            .build_unvalidated();
        let target_config = TargetSyncConfig::try_from_config(&config).unwrap();
        let store = MockHashStore::new();
        let (tx, rx) = std::sync::mpsc::channel();
        let source_online = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let engine = LocalSyncEngine::new(store, target_config.clone());
        let context = SyncWorkerContext::new(0, target_config, engine, rx, None, source_online);
        let _handle = start_sync_worker(context).unwrap();

        tx.send(SyncCommand::FileDeleted(
            RelativePath::new("keep_me.txt").unwrap(),
        ))
        .unwrap();

        std::thread::sleep(std::time::Duration::from_millis(300));

        assert!(dest.join("keep_me.txt").exists());
    }

    #[test]
    fn test_worker_network_offline_bailout() {
        let dir = tempdir().unwrap();
        let source = dir.path().join("src");
        let dest = dir.path().join("dst_offline");
        fs::create_dir_all(&source).unwrap();

        let config = Config::builder(source.clone())
            .dest_dir(dest.clone())
            .debounce_seconds(1)
            .retry_interval_seconds(1)
            .build_unvalidated();
        let target_config = TargetSyncConfig::try_from_config(&config).unwrap();
        let store = MockHashStore::new();
        let (tx, rx) = std::sync::mpsc::channel();
        let source_online = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let engine = LocalSyncEngine::new(store, target_config.clone());
        let context = SyncWorkerContext::new(0, target_config, engine, rx, None, source_online);
        let _handle = start_sync_worker(context).unwrap();

        fs::write(source.join("file1.txt"), b"hello").unwrap();
        tx.send(SyncCommand::FileModified(
            RelativePath::new("file1.txt").unwrap(),
        ))
        .unwrap();

        std::thread::sleep(std::time::Duration::from_millis(300));
        assert!(!dest.exists());
    }

    #[test]
    fn test_sync_worker_evicts_validation_errors_without_retry() {
        let engine = MockSyncEngine::new();
        engine.set_sync_error(|| SyncError::validation_security("Permanent validation failure"));
        let (tx, rx) = std::sync::mpsc::channel();
        let dir = tempdir().unwrap();
        let src = dir.path().join("src");
        let dst = dir.path().join("dst");
        fs::create_dir_all(&src).unwrap();
        fs::create_dir_all(&dst).unwrap();
        let config = Config::builder(src)
            .dest_dir(dst)
            .debounce_seconds(1)
            .retry_interval_seconds(1)
            .build_unvalidated();
        let target_config = TargetSyncConfig::try_from_config(&config).unwrap();
        let source_online = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let ctx = SyncWorkerContext::new(0, target_config, engine.clone(), rx, None, source_online);
        let handle = start_sync_worker(ctx).unwrap();
        tx.send(SyncCommand::FileModified(
            RelativePath::new("file.txt").unwrap(),
        ))
        .unwrap();
        let start = Instant::now();
        while engine.failed_calls().is_empty()
            && start.elapsed() < std::time::Duration::from_secs(3)
        {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        drop(tx);
        handle.join().unwrap();
        assert_eq!(
            engine.failed_calls().len(),
            1,
            "Validation error must be evicted after 1 attempt, not retried"
        );
    }

    #[test]
    fn test_source_connectivity_tracker() {
        let tracker = SourceConnectivityTracker::new(true);
        assert!(tracker.is_online());
        tracker.set_online(false);
        assert!(!tracker.is_online());
        let raw = tracker.raw_arc();
        assert!(!raw.load(std::sync::atomic::Ordering::Relaxed));
    }

    #[test]
    fn test_debounce_queue_operations() {
        let mut queue = DebounceQueue::new(2);
        let p1 = PathBuf::from("a.txt");
        let p2 = PathBuf::from("b.txt");
        let p3 = PathBuf::from("c.txt");

        assert!(queue.enqueue_sync(p1.clone(), std::time::Duration::from_millis(10)));
        assert!(queue.enqueue_delete(p2.clone(), std::time::Duration::from_millis(20)));
        // At capacity (2 items):
        assert!(!queue.enqueue_sync(p3, std::time::Duration::from_millis(10)));
        // Existing path can be refreshed even at capacity:
        assert!(queue.enqueue_sync(p1.clone(), std::time::Duration::from_millis(50)));

        assert_eq!(queue.pending_sync_count(), 1);
        assert_eq!(queue.pending_delete_count(), 1);

        // Before deadline, draining returns empty:
        let drained = queue.drain_ready_syncs(Instant::now());
        assert!(drained.is_empty());

        // Drain after deadline:
        let future = Instant::now() + std::time::Duration::from_secs(1);
        let ready_syncs = queue.drain_ready_syncs(future);
        assert_eq!(ready_syncs, vec![p1.clone()]);
        assert_eq!(queue.pending_sync_count(), 0);

        let ready_deletes = queue.drain_ready_deletes(future);
        assert_eq!(ready_deletes, vec![p2]);
        assert_eq!(queue.pending_delete_count(), 0);

        // Requeue retry
        queue.requeue_sync_retry(p1.clone(), std::time::Duration::from_millis(50));
        assert_eq!(queue.pending_sync_count(), 1);
    }

    #[test]
    fn test_debounce_queue_min_heap_correctness() {
        let mut queue = DebounceQueue::new(10);
        let pa = PathBuf::from("a.txt");
        let pb = PathBuf::from("b.txt");
        let pc = PathBuf::from("c.txt");
        let pd = PathBuf::from("d.txt");

        let t0 = Instant::now();
        queue.enqueue_sync(pa.clone(), std::time::Duration::from_millis(50));
        queue.enqueue_sync(pb.clone(), std::time::Duration::from_millis(10));
        queue.enqueue_delete(pc.clone(), std::time::Duration::from_millis(30));

        // pb should be earliest (~10ms)
        let dl1 = queue.earliest_deadline().unwrap();
        assert!(dl1 <= t0 + std::time::Duration::from_millis(20));

        // Overwrite pb with later deadline (~100ms) -> pc should now be earliest (~30ms)
        queue.enqueue_sync(pb.clone(), std::time::Duration::from_millis(100));
        let dl2 = queue.earliest_deadline().unwrap();
        assert!(dl2 <= t0 + std::time::Duration::from_millis(40));

        // Requeue retry for pd (~5ms) -> pd should now be earliest
        queue.requeue_sync_retry(pd.clone(), std::time::Duration::from_millis(5));
        let dl3 = queue.earliest_deadline().unwrap();
        assert!(dl3 <= t0 + std::time::Duration::from_millis(15));

        // Drain up to 35ms -> pd (5ms) and pc (30ms) drained
        let drained_sync = queue.drain_ready_syncs(t0 + std::time::Duration::from_millis(35));
        assert_eq!(drained_sync, vec![pd]);
        let drained_del = queue.drain_ready_deletes(t0 + std::time::Duration::from_millis(35));
        assert_eq!(drained_del, vec![pc]);

        // pa (~50ms) should now be earliest
        let dl4 = queue.earliest_deadline().unwrap();
        assert!(dl4 <= t0 + std::time::Duration::from_millis(60));
    }

    #[test]
    fn test_debounce_queue_stress_10k_entries() {
        let mut queue = DebounceQueue::new(20_000);
        for i in 0..10_000 {
            let path = PathBuf::from(format!("dir/file_{}.txt", i));
            let delay = std::time::Duration::from_millis((i % 500 + 1) as u64);
            queue.enqueue_sync(path, delay);
        }

        let start = Instant::now();
        for _ in 0..1000 {
            let dl = queue.earliest_deadline();
            assert!(dl.is_some());
        }
        let elapsed = start.elapsed();
        assert!(
            elapsed < std::time::Duration::from_millis(50),
            "1000 earliest_deadline queries on 10k items took {:?}, expected < 50ms (O(1) peek)",
            elapsed
        );
    }

    #[test]
    fn test_sync_worker_state_failure_tracking() {
        let mut state = SyncWorkerState::new(1024);
        assert_eq!(state.scratch.len(), 1024);
        let p = Path::new("failed.txt");
        assert_eq!(state.record_failure(p), 1);
        assert_eq!(state.record_failure(p), 2);
        state.reset_failure(p);
        assert_eq!(state.record_failure(p), 1);

        assert!(!state.needs_catchup_scan());
        state.mark_needs_catchup_scan();
        assert!(state.needs_catchup_scan());
        assert!(!state.should_trigger_catchup_scan(100, 50, true, true));
        assert!(!state.should_trigger_catchup_scan(50, 50, false, true));
        assert!(!state.should_trigger_catchup_scan(50, 50, true, false));
        assert!(state.should_trigger_catchup_scan(50, 50, true, true));
        state.clear_needs_catchup_scan();
        assert!(!state.needs_catchup_scan());
    }

    #[test]
    fn test_reachability_monitor() {
        let temp = tempdir().unwrap();
        let target = temp.path().to_path_buf();
        let mock_resolver = std::sync::Arc::new(crate::net::MockNetworkResolver::new());
        let mut monitor = ReachabilityMonitor::new(0, target.clone(), 5, mock_resolver);

        assert_eq!(monitor.active_dest(), target.as_path());
        assert!(!monitor.is_dest_online());

        let now = Instant::now();
        monitor.check_reachability(now, None);
        assert!(monitor.is_dest_online());
    }

    #[test]
    fn test_calculate_exponential_backoff() {
        let base = std::time::Duration::from_secs(5);
        assert_eq!(
            calculate_exponential_backoff(1, base),
            std::time::Duration::from_secs(5)
        );
        assert_eq!(
            calculate_exponential_backoff(2, base),
            std::time::Duration::from_secs(10)
        );
        assert_eq!(
            calculate_exponential_backoff(3, base),
            std::time::Duration::from_secs(20)
        );
        assert_eq!(
            calculate_exponential_backoff(4, base),
            std::time::Duration::from_secs(40)
        );
        assert_eq!(
            calculate_exponential_backoff(10, base),
            std::time::Duration::from_secs(300)
        );
    }

    #[test]
    fn test_write_verification_retained_in_pending_syncs() {
        let dir = tempdir().unwrap();
        let source = dir.path().join("src");
        let dest = dir.path().join("dst");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&dest).unwrap();

        let config = Config::builder(source.clone())
            .dest_dir(dest.clone())
            .debounce_seconds(1)
            .retry_interval_seconds(1)
            .build_unvalidated();

        let engine = MockSyncEngine::new();
        let call_count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let call_count_clone = call_count.clone();

        engine.set_sync_handler(move |path| {
            let count = call_count_clone.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if count == 0 {
                Err(SyncError::write_verification_failed(path.to_path_buf()))
            } else {
                Ok(())
            }
        });

        let (tx, rx) = std::sync::mpsc::channel();
        let source_online = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let target_config = TargetSyncConfig::try_from_config(&config).unwrap();
        let ctx = SyncWorkerContext::new(0, target_config, engine.clone(), rx, None, source_online);
        let handle = start_sync_worker(ctx).unwrap();

        tx.send(SyncCommand::FileModified(
            RelativePath::new("data.txt").unwrap(),
        ))
        .unwrap();

        let start = Instant::now();
        while engine.synced_calls().is_empty()
            && start.elapsed() < std::time::Duration::from_secs(3)
        {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }

        drop(tx);
        handle.join().unwrap();

        assert_eq!(
            call_count.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "WriteVerificationFailed must be retried and not evicted after first failure"
        );
        assert_eq!(engine.synced_calls().len(), 1);
        assert_eq!(engine.synced_calls()[0].0, PathBuf::from("data.txt"));
    }

    #[test]
    fn test_queue_overflow_triggers_catchup_scan() {
        let dir = tempdir().unwrap();
        let source = dir.path().join("source");
        let dest = dir.path().join("dest");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&dest).unwrap();

        let config = Config::builder(source)
            .dest_dir(dest)
            .debounce_seconds(1)
            .retry_interval_seconds(1)
            .build_unvalidated();

        let target_config = TargetSyncConfig::try_from_config(&config).unwrap();
        let engine = MockSyncEngine::new();
        let (tx, rx) = std::sync::mpsc::channel();
        let source_online = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let ctx = SyncWorkerContext::new(0, target_config, engine.clone(), rx, None, source_online)
            .with_max_pending_queue(5);
        let handle = start_sync_worker(ctx).unwrap();

        // Wait for initial catch-up full scan on destination startup
        let start = Instant::now();
        while engine.full_scans_count() == 0 && start.elapsed() < std::time::Duration::from_secs(2)
        {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let base_scans = engine.full_scans_count();
        assert!(
            base_scans >= 1,
            "Initial scan on destination reconnect should complete"
        );

        // Enqueue 10 distinct file paths to trigger queue overflow (capacity is 5)
        for i in 0..10 {
            let _ = tx.send(SyncCommand::FileModified(
                RelativePath::new(format!("file_{}.txt", i)).unwrap(),
            ));
        }

        // Wait for queue to drain and catchup scan to trigger
        let start = Instant::now();
        while engine.full_scans_count() <= base_scans
            && start.elapsed() < std::time::Duration::from_secs(2)
        {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        drop(tx);
        handle.join().unwrap();

        assert!(
            engine.full_scans_count() > base_scans,
            "Queue overflow must trigger an additional catchup full scan after queue drains"
        );
    }

    #[test]
    fn test_worker_sync_and_delete_uses_resolved_unc_path() {
        let dir = tempdir().unwrap();
        let source = dir.path().join("src");
        let fake_dest = PathBuf::from(r"Z:\mapped_share");
        let real_unc_dest = dir.path().join("unc_share");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&real_unc_dest).unwrap();

        let config = Config::builder(source)
            .dest_dir(fake_dest.clone())
            .debounce_seconds(1)
            .retry_interval_seconds(1)
            .build_unvalidated();

        let resolver = std::sync::Arc::new(crate::net::MockNetworkResolver::new());
        resolver.set_alternate_path(fake_dest, real_unc_dest.clone());

        let target_config = TargetSyncConfig::try_from_config(&config).unwrap();
        let engine = MockSyncEngine::new();
        let (tx, rx) = std::sync::mpsc::channel();
        let source_online = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));

        let ctx = SyncWorkerContext::new(0, target_config, engine.clone(), rx, None, source_online)
            .with_resolver(resolver);
        let handle = start_sync_worker(ctx).unwrap();

        tx.send(SyncCommand::FileModified(
            RelativePath::new("doc.txt").unwrap(),
        ))
        .unwrap();
        tx.send(SyncCommand::FileDeleted(
            RelativePath::new("old.txt").unwrap(),
        ))
        .unwrap();

        let start = Instant::now();
        while (engine.synced_calls().is_empty() || engine.deleted_calls().is_empty())
            && start.elapsed() < std::time::Duration::from_secs(3)
        {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }

        drop(tx);
        handle.join().unwrap();

        assert_eq!(engine.synced_calls().len(), 1);
        assert_eq!(engine.synced_calls()[0].0, PathBuf::from("doc.txt"));
        assert_eq!(engine.synced_calls()[0].1, real_unc_dest);

        assert_eq!(engine.deleted_calls().len(), 1);
        assert_eq!(engine.deleted_calls()[0].0, PathBuf::from("old.txt"));
        assert_eq!(engine.deleted_calls()[0].1, real_unc_dest);
    }

    #[test]
    fn test_worker_io_backoff() {
        let base = std::time::Duration::from_secs(2);
        let b1 = calculate_exponential_backoff(1, base);
        let b2 = calculate_exponential_backoff(2, base);
        let b3 = calculate_exponential_backoff(3, base);
        assert!(b1 <= b2);
        assert!(b2 <= b3);
    }

    #[test]
    fn test_sync_worker_discrete_tick_processes_queue_without_sleep() {
        let dir = tempdir().unwrap();
        let source = dir.path().join("src");
        let dest = dir.path().join("dst");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&dest).unwrap();

        let config = Config::builder(source)
            .dest_dir(dest)
            .debounce_seconds(2)
            .retry_interval_seconds(1)
            .build_unvalidated();

        let target_config = TargetSyncConfig::try_from_config(&config).unwrap();
        let engine = MockSyncEngine::new();
        let (_tx, rx) = std::sync::mpsc::channel();
        let source_online = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));

        let ctx = SyncWorkerContext::new(0, target_config, engine.clone(), rx, None, source_online);
        let mut runner = SyncWorkerRunner::new(ctx);

        let t0 = Instant::now();
        runner.handle_command(SyncCommand::FileModified(
            RelativePath::new("test.txt").unwrap(),
        ));

        // T0: Debounce has not elapsed (debounce_seconds = 2), so tick does not drain
        let outcome = runner.tick(t0).unwrap();
        assert_eq!(outcome, WorkerTickOutcome::Continue);
        assert_eq!(engine.synced_calls().len(), 0);
        assert_eq!(runner.queue.pending_count(), 1);

        // T0 + 3s: Debounce has elapsed, tick should process the file
        let t1 = t0 + Duration::from_secs(3);
        let outcome = runner.tick(t1).unwrap();
        assert_eq!(outcome, WorkerTickOutcome::Continue);
        assert_eq!(engine.synced_calls().len(), 1);
        assert_eq!(engine.synced_calls()[0].0, PathBuf::from("test.txt"));
        assert_eq!(runner.queue.pending_count(), 0);
    }

    #[test]
    fn test_sync_worker_preserves_retry_on_non_permanent_validation_error() {
        let dir = tempdir().unwrap();
        let source = dir.path().join("src");
        let dest = dir.path().join("dst");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&dest).unwrap();

        let config = Config::builder(source)
            .dest_dir(dest)
            .debounce_seconds(1)
            .retry_interval_seconds(5)
            .build_unvalidated();

        let target_config = TargetSyncConfig::try_from_config(&config).unwrap();
        let engine = MockSyncEngine::new();
        let (_tx, rx) = std::sync::mpsc::channel();
        let source_online = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));

        let ctx = SyncWorkerContext::new(0, target_config, engine.clone(), rx, None, source_online);
        let mut runner = SyncWorkerRunner::new(ctx);

        // Configure mock engine to return a transient validation error
        engine.set_sync_error(|| SyncError::validation("temporary lock conflict"));

        runner.handle_command(SyncCommand::FileModified(
            RelativePath::new("transient.txt").unwrap(),
        ));
        let t0 = Instant::now() + Duration::from_secs(2);
        let _ = runner.tick(t0).unwrap();

        // Non-permanent validation error must be requeued for retry (with 5s backoff)
        assert_eq!(runner.queue.pending_count(), 1);

        // Now test permanent validation error
        engine.set_sync_error(|| {
            SyncError::validation_security("Unsafe path traversal detected: ../secret")
        });

        runner.handle_command(SyncCommand::FileModified(
            RelativePath::new("traversal.txt").unwrap(),
        ));
        assert_eq!(runner.queue.pending_count(), 2);

        let t1 = t0 + Duration::from_secs(2);
        let _ = runner.tick(t1).unwrap();

        // Permanent validation failure must be evicted without retry
        // Only transient.txt remains in the queue (waiting for its 5s backoff)
        assert_eq!(runner.queue.pending_count(), 1);

        // Advance simulated time past the 5s backoff; clear error so transient.txt succeeds
        engine.clear_sync_error();
        let t2 = t1 + Duration::from_secs(6);
        let _ = runner.tick(t2).unwrap();
        assert_eq!(runner.queue.pending_count(), 0);
        assert_eq!(engine.synced_calls().len(), 1);
        assert_eq!(engine.synced_calls()[0].0, PathBuf::from("transient.txt"));
    }

    #[test]
    fn test_sync_worker_context_builder_invariants() {
        let temp = tempdir().unwrap();
        let src = temp.path().join("source");
        let dst = temp.path().join("dest");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&dst).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let _tx = tx;
        let target_cfg = TargetSyncConfig::builder(src, dst).build().unwrap();
        let engine = MockSyncEngine::new();

        // 1. Zero max_pending_queue must fail validation
        let err = SyncWorkerContext::builder(0, target_cfg.clone(), engine.clone(), rx, true)
            .max_pending_queue(0)
            .build();
        assert!(err.is_err(), "max_pending_queue = 0 must fail validation");
        let err = err.err().unwrap();
        assert!(matches!(err, SyncError::Validation { .. }));
        assert!(err.to_string().contains("greater than zero"));

        // 2. Successful build with defaults
        let (_tx2, rx2) = std::sync::mpsc::channel();
        let ctx = SyncWorkerContext::builder(1, target_cfg.clone(), engine.clone(), rx2, true)
            .build()
            .unwrap();
        assert_eq!(ctx.target_index(), 1);
        assert_eq!(ctx.max_pending_queue(), 50_000);
        assert!(ctx.observer().is_none());
        assert!(!ctx.cancellation().load(std::sync::atomic::Ordering::SeqCst));

        // 3. Custom options
        let (_tx3, rx3) = std::sync::mpsc::channel();
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let ctx3 = SyncWorkerContext::builder(2, target_cfg, engine, rx3, true)
            .max_pending_queue(100)
            .cancellation(cancel)
            .build()
            .unwrap();
        assert_eq!(ctx3.target_index(), 2);
        assert_eq!(ctx3.max_pending_queue(), 100);
        assert!(
            ctx3.cancellation()
                .load(std::sync::atomic::Ordering::SeqCst)
        );
    }

    #[test]
    fn test_worker_shutdown_drained_from_try_recv() {
        let temp = tempdir().unwrap();
        let src = temp.path().join("source");
        let dst = temp.path().join("dest");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&dst).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let target_cfg = TargetSyncConfig::builder(src, dst).build().unwrap();
        let engine = MockSyncEngine::new();
        let scan_count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let sc = scan_count.clone();
        engine.set_sync_error(move || {
            let count = sc.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if count >= 1 {
                SyncError::Cancelled
            } else {
                SyncError::Io(std::io::Error::other("transient"))
            }
        });
        let source_online = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));

        let ctx = SyncWorkerContext::new(0, target_cfg, engine, rx, None, source_online);
        tx.send(SyncCommand::FileModified(
            RelativePath::new("first.txt").unwrap(),
        ))
        .unwrap();
        tx.send(SyncCommand::TriggerFullScan).unwrap();

        let handle = start_sync_worker(ctx).unwrap();

        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let join_thread = std::thread::spawn(move || {
            let _ = handle.join();
            let _ = done_tx.send(());
        });

        // The worker should terminate immediately upon processing TriggerFullScan in try_recv.
        // If the drain loop break only exits `while let` instead of the outer worker loop,
        // it hangs waiting for the next recv_timeout / debounce interval.
        let res = done_rx.recv_timeout(std::time::Duration::from_millis(200));
        assert!(
            res.is_ok(),
            "Worker thread failed to terminate after draining Shutdown/Cancelled command"
        );
        let _ = join_thread.join();
    }

    #[derive(Clone, Default)]
    struct SharedBuffer(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl std::io::Write for SharedBuffer {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn test_sync_worker_runner_periodic_archive_prune_failure_emits_warn_log() {
        let buffer = SharedBuffer::default();
        let writer_buffer = buffer.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(move || writer_buffer.clone())
            .with_ansi(false)
            .finish();

        let dir = tempdir().unwrap();
        let source = dir.path().join("src");
        let dest = dir.path().join("dst");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&dest).unwrap();

        let config = Config::builder(source).dest_dir(dest).build_unvalidated();

        let target_config = TargetSyncConfig::try_from_config(&config).unwrap();
        let engine = MockSyncEngine::new();
        engine
            .set_sync_error(|| SyncError::Io(std::io::Error::other("disk full on archive volume")));

        let (_tx, rx) = std::sync::mpsc::channel();
        let source_online = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let ctx = SyncWorkerContext::new(0, target_config, engine, rx, None, source_online);
        let mut runner = SyncWorkerRunner::new(ctx);

        // Advance simulated time past 3600s interval to trigger prune
        let now = Instant::now() + Duration::from_secs(3605);
        let outcome = tracing::subscriber::with_default(subscriber, || runner.tick(now)).unwrap();
        assert_eq!(outcome, WorkerTickOutcome::Continue);

        let bytes = buffer.0.lock().unwrap().clone();
        let log_str = String::from_utf8_lossy(&bytes);
        assert!(
            log_str.contains("Periodic archive prune failed"),
            "Expected 'Periodic archive prune failed' in logs, got: {}",
            log_str
        );
    }

    #[test]
    fn test_sync_worker_thread_span_propagation() {
        let dir = tempdir().unwrap();
        let source = dir.path().join("src");
        let dest = dir.path().join("dst");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&dest).unwrap();

        let config = Config::builder(source).dest_dir(dest).build_unvalidated();
        let target_config = TargetSyncConfig::try_from_config(&config).unwrap();
        let engine = MockSyncEngine::new();
        let (tx, rx) = std::sync::mpsc::channel();
        let source_online = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let ctx = SyncWorkerContext::new(0, target_config, engine, rx, None, source_online);

        let (_, log_output) = crate::test_support::with_captured_tracing(|| {
            let handle = start_sync_worker(ctx).unwrap();
            drop(tx);
            let _ = handle.join();
        });

        assert!(
            log_output.contains("sync_worker"),
            "Log output missing sync_worker span: {log_output}"
        );
        assert!(
            log_output.contains("target_index=1"),
            "Log output missing target_index field: {log_output}"
        );
    }
}

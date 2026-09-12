use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Tracks per-path sync failure counts with bounded capacity.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub(crate) struct FailureTracker {
    capacity: usize,
    counts: HashMap<PathBuf, u32>,
    order: VecDeque<PathBuf>,
}

#[allow(dead_code)]
impl FailureTracker {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            counts: HashMap::new(),
            order: VecDeque::new(),
        }
    }

    pub fn record_failure(&mut self, path: &Path) -> u32 {
        let entry = self.counts.entry(path.to_path_buf()).or_insert(0);
        *entry += 1;
        *entry
    }

    pub fn reset_failure(&mut self, path: &Path) {
        self.counts.remove(path);
    }

    pub fn clear(&mut self) {
        self.counts.clear();
        self.order.clear();
    }

    pub fn get(&self, path: &Path) -> Option<u32> {
        self.counts.get(path).copied()
    }

    pub fn contains_key(&self, path: &Path) -> bool {
        self.counts.contains_key(path)
    }

    pub fn len(&self) -> usize {
        self.counts.len()
    }

    pub fn is_empty(&self) -> bool {
        self.counts.is_empty()
    }
}

/// State container for the sync worker execution loop.
pub(crate) struct SyncWorkerState {
    pub(crate) scratch: Vec<u8>,
    pub(crate) failure_tracker: FailureTracker,
    pub(crate) hourly_prune_interval: Duration,
    pub(crate) last_archive_prune: Instant,
    pub(crate) needs_catchup_scan: bool,
    pub(crate) catchup_scan_failures: u32,
    pub(crate) next_catchup_scan_attempt: Option<Instant>,
}

impl SyncWorkerState {
    /// Initialize worker scratch buffer and archive prune timers.
    pub fn new(block_size_bytes: u64) -> Self {
        Self {
            scratch: vec![0u8; block_size_bytes as usize],
            failure_tracker: FailureTracker::new(5000),
            hourly_prune_interval: Duration::from_secs(3600),
            last_archive_prune: Instant::now(),
            needs_catchup_scan: false,
            catchup_scan_failures: 0,
            next_catchup_scan_attempt: None,
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
        self.failure_tracker.record_failure(path)
    }

    /// Reset failure count upon successful synchronization.
    pub fn reset_failure(&mut self, path: &Path) {
        self.failure_tracker.reset_failure(path);
    }

    /// Clears all tracked failures.
    #[allow(dead_code)]
    pub fn clear_failures(&mut self) {
        self.failure_tracker.clear();
    }

    /// Marks that a catch-up scan is required (e.g. on queue overflow or permanent eviction).
    pub fn mark_needs_catchup_scan(&mut self) {
        self.needs_catchup_scan = true;
    }

    /// Record a catch-up scan failure, incrementing failure count and setting the exponential backoff deadline.
    pub fn record_catchup_scan_failure(&mut self, now: Instant, base_interval: Duration) {
        self.catchup_scan_failures = self.catchup_scan_failures.saturating_add(1);
        let backoff = calculate_exponential_backoff(self.catchup_scan_failures, base_interval);
        self.next_catchup_scan_attempt = Some(now + backoff);
        tracing::warn!(
            attempts = self.catchup_scan_failures,
            ?backoff,
            "Catch-up scan failed; backoff scheduled"
        );
    }

    /// Record a successful catch-up scan, clearing the pending flag and resetting backoff failure state.
    pub fn record_catchup_scan_success(&mut self) {
        self.needs_catchup_scan = false;
        self.catchup_scan_failures = 0;
        self.next_catchup_scan_attempt = None;
    }

    /// Clears the catch-up scan requirement flag and resets failure backoff.
    #[allow(dead_code)]
    pub fn clear_needs_catchup_scan(&mut self) {
        self.record_catchup_scan_success();
    }

    /// Whether a catch-up full scan is currently needed.
    #[must_use]
    pub fn needs_catchup_scan(&self) -> bool {
        self.needs_catchup_scan
    }

    /// Determines if a catch-up scan should execute based on queue depth, connectivity, and failure backoff timer.
    pub fn should_trigger_catchup_scan(
        &self,
        now: Instant,
        queue_pending: usize,
        drain_threshold: usize,
        source_online: bool,
        dest_online: bool,
    ) -> bool {
        self.needs_catchup_scan()
            && queue_pending <= drain_threshold
            && source_online
            && dest_online
            && self
                .next_catchup_scan_attempt
                .is_none_or(|earliest| now >= earliest)
    }
}

/// Calculates exponential backoff duration based on the number of attempts.
pub fn calculate_exponential_backoff(attempts: u32, base_interval: Duration) -> Duration {
    let factor = 2u64.saturating_pow(attempts.saturating_sub(1));
    let max_delay = Duration::from_secs(300);
    base_interval
        .checked_mul(factor.min(u32::MAX as u64) as u32)
        .unwrap_or(max_delay)
        .min(max_delay)
}

use crate::error::SyncError;
use crate::sync::engine::{ScanOutcome, SyncCommand, SyncEngine};
use crate::sync::worker::context::SyncWorkerContext;
use crate::sync::worker::queue::DebounceQueue;
use crate::sync::worker::reachability::ReachabilityMonitor;
use crate::sync::worker::state::{SyncWorkerState, calculate_exponential_backoff};
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// Compute the channel receive timeout for the background worker event loop.
///
/// If there are pending expired sync items in the debounce queue, `Duration::ZERO`
/// is returned only if both destination and source are currently reachable (`can_drain`).
/// If either is offline, the timeout is clamped to 1 second to prevent CPU busy-spinning.
pub(crate) fn calculate_worker_poll_timeout(
    queue_earliest: Option<Instant>,
    now: Instant,
    can_drain: bool,
) -> Duration {
    match queue_earliest {
        Some(dl) if dl > now => (dl - now).min(Duration::from_secs(1)),
        Some(_) if can_drain => Duration::ZERO,
        Some(_) => Duration::from_secs(1),
        None => Duration::from_secs(1),
    }
}

/// The outcome of evaluating a discrete execution tick of the [`SyncWorkerRunner`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WorkerTickOutcome {
    /// The worker should continue processing events and polling commands.
    Continue,
    /// The worker received a shutdown command or cancellation signal and should terminate.
    ShutdownRequested,
}

/// Testable sync worker state machine orchestrating debouncing, reachability, and execution.
///
/// Encapsulates worker context, debounce priority queues, reachability monitors, and execution
/// state, allowing deterministic, zero-sleep stepping through time via [`SyncWorkerRunner::tick`].
pub(crate) struct SyncWorkerRunner<E: SyncEngine> {
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
        let debounce_dur = Duration::from_secs(self.context.config.debounce_seconds());
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
                            self.state.record_catchup_scan_success();
                            self.state.clear_failures();
                        }
                        Ok(ScanOutcome::PartialFailure {
                            synced,
                            failed,
                            delete_failed,
                        }) => {
                            self.reachability
                                .mark_online(self.context.observer.as_ref());
                            self.state.record_catchup_scan_success();
                            self.state.clear_failures();
                            tracing::warn!(
                                target_index = self.context.target_index + 1,
                                synced,
                                failed,
                                delete_failed,
                                "Full scan completed with partial failure (destination reachable)"
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
                        Ok(ScanOutcome::Success { .. } | ScanOutcome::PartialFailure { .. }) => {
                            self.reachability
                                .mark_online(self.context.observer.as_ref());
                            self.state.record_catchup_scan_success();
                            self.state.clear_failures();
                        }
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

        let retry_dur = Duration::from_secs(self.context.config.retry_interval_seconds());

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
                    Err(SyncError::Io(ref e))
                        if e.kind() == std::io::ErrorKind::NotFound
                            && !self.context.config.source_dir().join(&path).exists() =>
                    {
                        tracing::info!(
                            path = %path.display(),
                            "Source file no longer exists; discarding retry"
                        );
                        self.state.reset_failure(&path);
                    }
                    Err(SyncError::Io(e)) => {
                        let attempts = self.state.record_failure(&path);
                        if attempts <= 10 {
                            let backoff = calculate_exponential_backoff(attempts, retry_dur);
                            tracing::warn!(
                                path = %path.display(),
                                attempt = attempts,
                                ?backoff,
                                error = %e,
                                "File sync failed with IO error; rescheduling retry"
                            );
                            self.queue.requeue_sync_retry(path, backoff);
                        } else {
                            tracing::error!(
                                path = %path.display(),
                                error = %e,
                                "File sync permanently failed after 10 retries"
                            );
                            self.state.reset_failure(&path);
                            if let Some(ref obs) = self.context.observer {
                                obs.on_write_verification_failed(&path);
                            }
                        }
                    }
                    Err(e) => {
                        let attempts = self.state.record_failure(&path);
                        if attempts <= 10 {
                            let backoff = calculate_exponential_backoff(attempts, retry_dur);
                            tracing::warn!(
                                path = %path.display(),
                                attempt = attempts,
                                ?backoff,
                                error = %e,
                                "File sync failed; rescheduling retry"
                            );
                            self.queue.requeue_sync_retry(path, backoff);
                        } else {
                            tracing::error!(
                                path = %path.display(),
                                error = %e,
                                "File sync permanently failed after 10 retries"
                            );
                            self.state.reset_failure(&path);
                            if let Some(ref obs) = self.context.observer {
                                obs.on_write_verification_failed(&path);
                            }
                        }
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
                            let attempts = self.state.record_failure(&path);
                            if attempts <= 10 {
                                let backoff = calculate_exponential_backoff(attempts, retry_dur);
                                tracing::warn!(
                                    path = %path.display(),
                                    attempt = attempts,
                                    ?backoff,
                                    error = %e,
                                    "File delete failed with IO error; rescheduling retry"
                                );
                                self.queue.requeue_delete_retry(path, backoff);
                            } else {
                                tracing::error!(
                                    path = %path.display(),
                                    error = %e,
                                    "Deletion permanently failed after 10 retries"
                                );
                                self.state.reset_failure(&path);
                            }
                        }
                        Err(e) => {
                            let attempts = self.state.record_failure(&path);
                            if attempts <= 10 {
                                let backoff = calculate_exponential_backoff(attempts, retry_dur);
                                tracing::warn!(
                                    path = %path.display(),
                                    attempt = attempts,
                                    ?backoff,
                                    error = %e,
                                    "File delete failed; rescheduling retry"
                                );
                                self.queue.requeue_delete_retry(path, backoff);
                            } else {
                                tracing::error!(
                                    path = %path.display(),
                                    error = %e,
                                    "File delete permanently failed after 10 retries"
                                );
                                self.state.reset_failure(&path);
                            }
                        }
                    }
                }
            }
        }

        if self.state.should_trigger_catchup_scan(
            now,
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
                Ok(ScanOutcome::Success { .. } | ScanOutcome::PartialFailure { .. }) => {
                    self.state.record_catchup_scan_success();
                    self.state.clear_failures();
                }
                Ok(ScanOutcome::DestinationUnreachable) => {
                    self.reachability
                        .mark_offline(self.context.observer.as_ref());
                    self.state.record_catchup_scan_failure(now, retry_dur);
                }
                Err(SyncError::Cancelled) => {
                    return Ok(WorkerTickOutcome::ShutdownRequested);
                }
                Err(e) => {
                    tracing::error!(error = %e, "Catch-up scan after queue overflow failed");
                    self.state.record_catchup_scan_failure(now, retry_dur);
                }
            }
        }

        Ok(WorkerTickOutcome::Continue)
    }
}

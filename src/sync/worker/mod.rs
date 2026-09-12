use crate::error::SyncError;
use crate::sync::engine::SyncEngine;
use std::time::Instant;

pub(crate) mod context;
pub(crate) mod queue;
pub(crate) mod reachability;
pub(crate) mod runner;
pub(crate) mod state;

pub use context::{SourceConnectivityTracker, SyncWorkerContext, SyncWorkerContextBuilder};
#[allow(unused_imports)]
pub(crate) use queue::DebounceQueue;
#[allow(unused_imports)]
pub(crate) use reachability::ReachabilityMonitor;
#[allow(unused_imports)]
pub(crate) use runner::{SyncWorkerRunner, WorkerTickOutcome, calculate_worker_poll_timeout};
#[allow(unused_imports)]
pub(crate) use state::{FailureTracker, SyncWorkerState, calculate_exponential_backoff};

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
mod tests;

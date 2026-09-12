use crate::net::NetworkResolver;
use crate::sync::engine::{ConnectivityState, SyncStatusObserver};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Manages reachability checks and alternate network path resolution for a target worker.
pub(crate) struct ReachabilityMonitor {
    pub(crate) target_index: usize,
    pub(crate) configured_dest: PathBuf,
    pub(crate) active_dest: PathBuf,
    pub(crate) dest_online: bool,
    pub(crate) last_sent_status: Option<ConnectivityState>,
    pub(crate) last_status_check: Option<Instant>,
    pub(crate) retry_dur: Duration,
    pub(crate) resolver: Arc<dyn NetworkResolver>,
}

impl ReachabilityMonitor {
    /// Create a new reachability monitor for a target directory.
    pub fn new(
        target_index: usize,
        configured_dest: PathBuf,
        retry_interval_seconds: u64,
        resolver: Arc<dyn NetworkResolver>,
    ) -> Self {
        let active_dest = configured_dest.clone();
        Self {
            target_index,
            configured_dest,
            active_dest,
            dest_online: false,
            last_sent_status: None,
            last_status_check: None,
            retry_dur: Duration::from_secs(retry_interval_seconds),
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
        observer: Option<&Arc<dyn SyncStatusObserver>>,
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
    pub fn mark_offline(&mut self, observer: Option<&Arc<dyn SyncStatusObserver>>) {
        self.dest_online = false;
        if self.last_sent_status != Some(ConnectivityState::Offline) {
            if let Some(obs) = observer {
                obs.on_target_status_change(self.target_index, ConnectivityState::Offline);
            }
            self.last_sent_status = Some(ConnectivityState::Offline);
        }
    }

    /// Mark the target destination online immediately and notify observers.
    pub fn mark_online(&mut self, observer: Option<&Arc<dyn SyncStatusObserver>>) {
        self.dest_online = true;
        if self.last_sent_status != Some(ConnectivityState::Online) {
            if let Some(obs) = observer {
                obs.on_target_status_change(self.target_index, ConnectivityState::Online);
            }
            self.last_sent_status = Some(ConnectivityState::Online);
        }
    }
}

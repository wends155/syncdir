//! Pure UI state domain models and connectivity tracking for the system tray interface.

use crate::sync::{ConnectivityState, WatcherState};
use std::path::{Path, PathBuf};

/// Reason the tray event loop exited.
///
/// Returned by [`TrayEventLoop::run`](super::TrayEventLoop::run) so the caller can decide
/// whether to re-launch the process after the tray icon has been cleanly dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayExitReason {
    /// User selected "Exit" from the tray menu.
    UserExit,
    /// User selected "Reload Config" — caller should re-spawn the process.
    Restart,
}

/// Status of the background sync engine.
///
/// Communicates the connectivity state of the source and destination directories
/// to the tray interface for visual tray signaling and tooltips.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
#[repr(usize)]
pub enum EngineStatus {
    /// Both source and destination directories are online and accessible.
    Healthy = 0,
    /// Some (but not all) destination directories are offline.
    Degraded = 1,
    /// The source directory is missing or unmounted.
    SourceOffline = 2,
    /// The destination directory is missing or unmounted.
    DestinationOffline = 3,
    /// Both directories are missing or unmounted.
    BothOffline = 4,
}

impl EngineStatus {
    pub const COUNT: usize = 5;
    pub const ALL: [Self; Self::COUNT] = [
        Self::Healthy,
        Self::Degraded,
        Self::SourceOffline,
        Self::DestinationOffline,
        Self::BothOffline,
    ];

    #[inline]
    #[must_use]
    pub const fn index(self) -> usize {
        self as usize
    }
}

/// Initial state of a destination target for the system tray interface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DestinationState {
    path: PathBuf,
    is_online: ConnectivityState,
    resolved_unc: Option<PathBuf>,
    display_label: String,
}

impl DestinationState {
    /// Create a new DestinationState with path and online reachability.
    #[must_use]
    pub fn new(path: impl Into<PathBuf>, is_online: impl Into<ConnectivityState>) -> Self {
        let path = path.into();
        let display_label = path.display().to_string();
        Self {
            path,
            is_online: is_online.into(),
            resolved_unc: None,
            display_label,
        }
    }

    /// Builder method to attach a resolved alternate UNC path.
    #[must_use]
    pub fn with_resolved_unc(mut self, resolved_unc: impl Into<Option<PathBuf>>) -> Self {
        self.resolved_unc = resolved_unc.into();
        self.display_label = match &self.resolved_unc {
            Some(unc) => format!("{} -> {}", self.path.display(), unc.display()),
            None => self.path.display().to_string(),
        };
        self
    }

    /// Retrieve the destination path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Retrieve the online connectivity status.
    #[must_use]
    pub fn is_online(&self) -> ConnectivityState {
        self.is_online
    }

    /// Retrieve the resolved alternate UNC path, if any.
    #[must_use]
    pub fn resolved_unc(&self) -> Option<&Path> {
        self.resolved_unc.as_deref()
    }

    /// Retrieve the precomputed user-facing display label for the destination.
    #[must_use]
    pub fn display_label(&self) -> &str {
        &self.display_label
    }

    /// Update online connectivity status.
    pub fn set_online(&mut self, is_online: impl Into<ConnectivityState>) {
        self.is_online = is_online.into();
    }
}

/// Encapsulates visual and connectivity state tracking for the system tray interface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrayState {
    source_online: ConnectivityState,
    watcher_active: WatcherState,
    dest_online: Vec<ConnectivityState>,
    scan_notice: Option<String>,
}

impl TrayState {
    /// Create a new TrayState with initial destination reachability states.
    #[must_use]
    pub fn new(
        initial_dest_online: impl IntoIterator<Item = impl Into<ConnectivityState>>,
    ) -> Self {
        Self {
            source_online: ConnectivityState::Offline,
            watcher_active: WatcherState::Inactive,
            dest_online: initial_dest_online.into_iter().map(Into::into).collect(),
            scan_notice: None,
        }
    }

    /// Create an empty TrayState with no destinations.
    #[must_use]
    pub fn empty() -> Self {
        Self::new(Vec::<ConnectivityState>::new())
    }

    /// Access whether the source directory is currently online.
    #[must_use]
    pub fn source_online(&self) -> bool {
        self.source_online == ConnectivityState::Online
    }

    /// Access strongly-typed source connectivity state.
    #[must_use]
    pub fn source_connectivity(&self) -> ConnectivityState {
        self.source_online
    }

    /// Access whether the directory watcher is currently active.
    #[must_use]
    pub fn watcher_active(&self) -> bool {
        self.watcher_active == WatcherState::Active
    }

    /// Access strongly-typed directory watcher state.
    #[must_use]
    pub fn watcher_state(&self) -> WatcherState {
        self.watcher_active
    }

    /// Access the per-destination online reachability slice.
    #[must_use]
    pub fn dest_online(&self) -> &[ConnectivityState] {
        &self.dest_online
    }

    /// Update target destination reachability by index.
    pub fn update_target_status(
        &mut self,
        target_index: usize,
        state: impl Into<ConnectivityState>,
    ) -> bool {
        let conn_state = state.into();
        if target_index < self.dest_online.len() {
            let changed = self.dest_online[target_index] != conn_state;
            self.dest_online[target_index] = conn_state;
            changed
        } else {
            false
        }
    }

    /// Update source directory connectivity and watcher active status using domain enums.
    pub fn update_watcher_status(
        &mut self,
        source_online: ConnectivityState,
        watcher_active: WatcherState,
    ) -> bool {
        let changed = self.source_online != source_online || self.watcher_active != watcher_active;
        self.source_online = source_online;
        self.watcher_active = watcher_active;
        changed
    }

    /// Set an optional scan notice (e.g. "Partial Scan (N skipped)").
    pub fn set_scan_notice(&mut self, notice: Option<String>) -> bool {
        let changed = self.scan_notice != notice;
        self.scan_notice = notice;
        changed
    }

    /// Access the current scan notice if any.
    #[must_use]
    pub fn scan_notice(&self) -> Option<&str> {
        self.scan_notice.as_deref()
    }

    /// Calculate the overall engine health status based on current state.
    #[must_use]
    pub fn overall_status(&self) -> EngineStatus {
        let all_dest_online = !self.dest_online.is_empty()
            && self
                .dest_online
                .iter()
                .all(|&online| online == ConnectivityState::Online);
        let any_dest_online = self.dest_online.contains(&ConnectivityState::Online);

        if self.source_online != ConnectivityState::Online
            || self.watcher_active != WatcherState::Active
        {
            if !any_dest_online && !self.dest_online.is_empty() {
                EngineStatus::BothOffline
            } else {
                EngineStatus::SourceOffline
            }
        } else if all_dest_online || self.dest_online.is_empty() {
            EngineStatus::Healthy
        } else if any_dest_online {
            EngineStatus::Degraded
        } else {
            EngineStatus::DestinationOffline
        }
    }

    /// Count how many destination targets are currently online.
    #[must_use]
    pub fn online_dest_count(&self) -> usize {
        self.dest_online
            .iter()
            .filter(|&&online| online == ConnectivityState::Online)
            .count()
    }

    /// Generate the formatted tooltip text for the system tray icon.
    #[must_use]
    pub fn tooltip_text(&self) -> String {
        let src_status_str = match (self.source_online, self.watcher_active) {
            (ConnectivityState::Offline, _) => "Offline",
            (ConnectivityState::Online, WatcherState::Inactive) => "Degraded",
            (ConnectivityState::Online, WatcherState::Active) => "Online",
        };
        let mut text = format!(
            "syncdir — Src: {} | Dests: {}/{} Online",
            src_status_str,
            self.online_dest_count(),
            self.dest_online.len()
        );
        if let Some(notice) = &self.scan_notice {
            text.push_str(" | ");
            text.push_str(notice);
        }
        text
    }
}

#[cfg(test)]
#[path = "state_tests.rs"]
mod tests;

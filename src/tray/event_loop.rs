//! UI event loop, winit event integration, and tray controller for the system tray.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tray_icon::TrayIconBuilder;
use tray_icon::menu::{CheckMenuItem, MenuEvent, MenuItem};
use winit::event::Event;
use winit::event_loop::{
    ControlFlow, EventLoop, EventLoopBuilder, EventLoopProxy, EventLoopWindowTarget,
};

use crate::error::SyncError;
use crate::sync::{ConnectivityState, SyncStatusObserver, WatcherState};
use crate::tray::assets::{generate_default_icon, get_cached_icon};
use crate::tray::dialog::{show_about_dialog, show_error_dialog};
use crate::tray::menu::{TrayActionHandler, TrayMenuIds, build_tray_menu};
use crate::tray::state::{DestinationState, EngineStatus, TrayExitReason, TrayState};

/// Per-target status report sent from worker threads to the tray event loop.
#[derive(Debug, Clone)]
pub(crate) struct TargetStatusUpdate {
    pub target_index: usize,
    pub dest_online: ConnectivityState,
}

/// Custom winit user event to wake up the loop on tray interactions and status updates.
///
/// This enum allows background worker threads and OS menu clicks to safely signal
/// the main thread UI event loop.
#[derive(Debug)]
pub(crate) enum UserEvent {
    /// A menu item click event forwarded from the tray menu callback.
    Menu(MenuEvent),
    /// A directory status change signal sent by the sync worker thread.
    StatusUpdate(TargetStatusUpdate),
    /// Watcher status update sent by the coordinator thread.
    WatcherStatus {
        source_online: ConnectivityState,
        watcher_active: WatcherState,
    },
    /// Result of an asynchronous configuration reload validation.
    ConfigReloadResult(Result<(), SyncError>),
}

/// Encapsulates tray icon menus, event dispatching, and UI state synchronization.
pub(crate) struct TrayController<H: TrayActionHandler + ?Sized> {
    tray_icon: tray_icon::TrayIcon,
    menu_ids: TrayMenuIds,
    startup_toggle: CheckMenuItem,
    dest_menu_items: Vec<MenuItem>,
    destinations: Vec<DestinationState>,
    state: TrayState,
    handler: Arc<H>,
    reload_proxy: EventLoopProxy<UserEvent>,
    is_reloading: Arc<AtomicBool>,
    exit_reason: Rc<Cell<TrayExitReason>>,
    pub(crate) last_icon_status: Option<EngineStatus>,
}

impl<H: TrayActionHandler + ?Sized> TrayController<H> {
    /// Initialize the tray controller with menus, icons, and event proxies.
    pub fn new(
        event_loop: &EventLoop<UserEvent>,
        destinations: Vec<DestinationState>,
        handler: Arc<H>,
        exit_reason: Rc<Cell<TrayExitReason>>,
    ) -> Result<Self, SyncError> {
        let components = build_tray_menu(&destinations, &handler)?;

        let icon = generate_default_icon()?;
        let tray_icon = TrayIconBuilder::new()
            .with_menu(Box::new(components.menu))
            .with_tooltip("syncdir — Folder Sync")
            .with_icon(icon)
            .build()
            .map_err(|e| SyncError::tray_with_source("Failed to create tray icon", e))?;

        let proxy = event_loop.create_proxy();
        let reload_proxy = proxy.clone();
        MenuEvent::set_event_handler(Some(move |event| {
            let _ = proxy.send_event(UserEvent::Menu(event));
        }));

        let state = TrayState::new(destinations.iter().map(|d| d.is_online()));
        let is_reloading = Arc::new(AtomicBool::new(false));

        Ok(Self {
            tray_icon,
            menu_ids: components.menu_ids,
            startup_toggle: components.startup_toggle,
            dest_menu_items: components.dest_menu_items,
            destinations,
            state,
            handler,
            reload_proxy,
            is_reloading,
            exit_reason,
            last_icon_status: None,
        })
    }

    fn trigger_config_reload(&self) {
        if self
            .is_reloading
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            tracing::warn!("Configuration reload already in progress; ignoring duplicate request.");
            return;
        }
        tracing::info!("Reload Config requested via tray menu; offloading to background thread.");
        let handler = self.handler.clone();
        let proxy = self.reload_proxy.clone();
        let reloading = self.is_reloading.clone();
        if let Err(e) = std::thread::Builder::new()
            .name("config-reload".to_string())
            .spawn(move || {
                let res = handler.on_reload_config();
                reloading.store(false, Ordering::SeqCst);
                let _ = proxy.send_event(UserEvent::ConfigReloadResult(res));
            })
        {
            self.is_reloading.store(false, Ordering::SeqCst);
            tracing::error!(error = %e, "Failed to spawn config-reload background thread");
            show_error_dialog(
                "Reload Error",
                &format!("Failed to spawn reload thread:\n\n{e}"),
            );
        }
    }

    /// Dispatch context menu selection events.
    pub fn handle_menu_event(
        &mut self,
        menu_event: MenuEvent,
        elwt: &EventLoopWindowTarget<UserEvent>,
    ) {
        let id = menu_event.id;
        if id == self.menu_ids.exit_id {
            MenuEvent::set_event_handler::<fn(MenuEvent)>(None);
            elwt.exit();
        } else if id == self.menu_ids.sync_now_id {
            match self.handler.on_sync_now() {
                Ok(()) => tracing::info!("Manual sync triggered from tray menu"),
                Err(e) => tracing::error!(error = %e, "Manual sync failed"),
            }
        } else if id == self.menu_ids.open_config_id {
            if let Err(e) = self.handler.on_open_config() {
                tracing::error!(error = %e, "Failed to open config file");
                show_error_dialog(
                    "Open Config Error",
                    &format!("Failed to open config:\n\n{e}"),
                );
            }
        } else if id == self.menu_ids.reload_config_id {
            self.trigger_config_reload();
        } else if id == self.menu_ids.view_logs_id {
            if let Err(e) = self.handler.on_view_logs() {
                tracing::error!(error = %e, "Failed to open log directory");
                show_error_dialog(
                    "View Logs Error",
                    &format!("Failed to open log directory:\n\n{e}"),
                );
            }
        } else if id == self.menu_ids.startup_toggle_id {
            let is_checked = self.startup_toggle.is_checked();
            match self.handler.on_toggle_startup(is_checked) {
                Ok(actual_state) => {
                    self.startup_toggle.set_checked(actual_state);
                    tracing::info!(
                        enabled = actual_state,
                        "Startup auto-run toggled from tray menu"
                    );
                }
                Err(e) => {
                    tracing::error!(error = %e, "Failed to toggle startup registration from tray");
                    self.startup_toggle.set_checked(!is_checked);
                }
            }
        } else if id == self.menu_ids.about_id {
            show_about_dialog();
        }
    }

    /// Handle the completion of an asynchronous configuration reload attempt.
    pub fn handle_config_reload_result(
        &mut self,
        res: Result<(), SyncError>,
        elwt: &EventLoopWindowTarget<UserEvent>,
    ) {
        match res {
            Ok(()) => {
                tracing::info!("Configuration validated successfully. Restarting daemon...");
                MenuEvent::set_event_handler::<fn(MenuEvent)>(None);
                self.exit_reason.set(TrayExitReason::Restart);
                elwt.exit();
            }
            Err(e) => {
                tracing::error!(error = %e, "Configuration reload failed");
                show_error_dialog(
                    "Config Reload Error",
                    &format!("Configuration reload error:\n\n{e}"),
                );
            }
        }
    }

    /// Update per-destination connectivity and repaint tray icon and menu item text.
    pub fn handle_status_update(&mut self, update: TargetStatusUpdate) {
        if self
            .state
            .update_target_status(update.target_index, update.dest_online)
        {
            if let Some(d) = self.destinations.get(update.target_index) {
                let is_online = update.dest_online == ConnectivityState::Online;
                let status_str = if is_online { "Online" } else { "Offline" };
                let indicator = if is_online { "●" } else { "○" };
                self.dest_menu_items[update.target_index].set_text(format!(
                    "{} {} ({})",
                    indicator,
                    d.display_label(),
                    status_str
                ));
            }
            self.repaint();
        }
    }

    /// Update source connectivity and directory watcher active status.
    pub fn handle_watcher_status(
        &mut self,
        source_online: ConnectivityState,
        watcher_active: WatcherState,
    ) {
        if self
            .state
            .update_watcher_status(source_online, watcher_active)
        {
            self.repaint();
        }
    }

    /// Re-render the tray icon and update its hover tooltip.
    pub fn repaint(&mut self) {
        let status = self.state.overall_status();
        let new_tooltip = self.state.tooltip_text();
        if let Err(e) = self.tray_icon.set_tooltip(Some(&new_tooltip)) {
            tracing::warn!(error = %e, "Failed to update tray tooltip");
        }

        let icon_changed = self.last_icon_status != Some(status);
        if icon_changed {
            match get_cached_icon(status) {
                Ok(new_icon) => match self.tray_icon.set_icon(Some(new_icon)) {
                    Ok(()) => self.last_icon_status = Some(status),
                    Err(e) => tracing::warn!(error = %e, "Failed to set tray icon"),
                },
                Err(e) => {
                    tracing::warn!(error = %e, status = ?status, "Failed to retrieve cached tray icon");
                }
            }
            tracing::info!(status = ?status, online_count = self.state.online_dest_count(), "Tray status updated");
        } else {
            tracing::trace!(status = ?status, online_count = self.state.online_dest_count(), "Tray status unchanged");
        }
    }

    /// Retrieve the current exit reason.
    #[allow(dead_code)]
    pub fn exit_reason(&self) -> TrayExitReason {
        self.exit_reason.get()
    }

    /// Read-only access to inner TrayState for inspection.
    #[allow(dead_code)]
    pub fn state(&self) -> &TrayState {
        &self.state
    }
}

/// Launch the system tray event loop (blocking).
///
/// Creates a tray icon in the Windows notification area with a checkable
/// context menu and listens for user mouse interactions and directory status updates.
///
/// # Arguments
///
/// * `event_loop` - The winit event loop initialized on the main UI thread.
/// * `destinations` - Initial destination states with reachability status.
/// * `handler` - Handler dispatching user actions to the sync daemon and startup backend.
///
/// # Returns
///
/// Returns [`TrayExitReason`] specifying whether the user requested normal shutdown or process restart.
///
/// # Errors
///
/// Returns [`SyncError::Tray`] if the tray menu, icon, or event loop builder fails.
pub(crate) fn run_tray<H: TrayActionHandler + ?Sized>(
    event_loop: EventLoop<UserEvent>,
    destinations: Vec<DestinationState>,
    handler: Arc<H>,
) -> Result<TrayExitReason, SyncError> {
    let exit_reason = Rc::new(Cell::new(TrayExitReason::UserExit));
    let mut controller =
        TrayController::new(&event_loop, destinations, handler, exit_reason.clone())?;

    controller.repaint();

    event_loop
        .run(move |event, elwt| {
            elwt.set_control_flow(ControlFlow::Wait);

            match event {
                Event::UserEvent(UserEvent::Menu(ev)) => controller.handle_menu_event(ev, elwt),
                Event::UserEvent(UserEvent::ConfigReloadResult(res)) => {
                    controller.handle_config_reload_result(res, elwt);
                }
                Event::UserEvent(UserEvent::StatusUpdate(upd)) => {
                    controller.handle_status_update(upd)
                }
                Event::UserEvent(UserEvent::WatcherStatus {
                    source_online,
                    watcher_active,
                }) => {
                    controller.handle_watcher_status(source_online, watcher_active);
                }
                _ => {}
            }
        })
        .map_err(|e| SyncError::tray_with_source("Event loop error", e))?;

    Ok(exit_reason.get())
}

struct WinitStatusObserver {
    proxy: EventLoopProxy<UserEvent>,
}

impl SyncStatusObserver for WinitStatusObserver {
    fn on_target_status_change(&self, target_index: usize, state: ConnectivityState) {
        let _ = self
            .proxy
            .send_event(UserEvent::StatusUpdate(TargetStatusUpdate {
                target_index,
                dest_online: state,
            }));
    }

    fn on_watcher_status_change(&self, source: ConnectivityState, watcher: WatcherState) {
        let _ = self.proxy.send_event(UserEvent::WatcherStatus {
            source_online: source,
            watcher_active: watcher,
        });
    }
}

/// Wrapper around the native UI event loop to encapsulate windowing dependencies.
pub struct TrayEventLoop {
    inner: EventLoop<UserEvent>,
}

impl TrayEventLoop {
    /// Create a new tray event loop.
    ///
    /// # Errors
    ///
    /// Returns [`SyncError::Tray`] if the underlying windowing event loop fails to initialize.
    pub fn new() -> Result<Self, SyncError> {
        let inner = EventLoopBuilder::<UserEvent>::with_user_event()
            .build()
            .map_err(|e| SyncError::tray_with_source("Failed to create event loop", e))?;
        Ok(Self { inner })
    }

    /// Create an event proxy for dispatching events from background threads.
    pub(crate) fn create_proxy(&self) -> EventLoopProxy<UserEvent> {
        self.inner.create_proxy()
    }

    /// Create a status observer handle that dispatches status updates to the tray event loop.
    #[must_use]
    pub fn status_observer(&self) -> Arc<dyn SyncStatusObserver> {
        Arc::new(WinitStatusObserver {
            proxy: self.create_proxy(),
        })
    }

    /// Run the tray event loop, blocking the main thread until exit is requested.
    ///
    /// Initializes tray icon, context menu, and background event handling loop.
    ///
    /// # Arguments
    ///
    /// * `destinations` - Configured target destination display states for menu indicators.
    /// * `handler` - Action handler dispatching context menu actions to the daemon and registry.
    ///
    /// # Returns
    ///
    /// Returns [`TrayExitReason`] specifying whether the user requested normal shutdown or process restart.
    ///
    /// # Errors
    ///
    /// Returns [`SyncError::Tray`] if the tray menu, icon, or event loop builder fails.
    pub fn run<H: TrayActionHandler + ?Sized>(
        self,
        destinations: Vec<DestinationState>,
        handler: Arc<H>,
    ) -> Result<TrayExitReason, SyncError> {
        run_tray(self.inner, destinations, handler)
    }
}

#[cfg(test)]
#[path = "event_loop_tests.rs"]
mod tests;
